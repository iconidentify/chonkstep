//! Real FIFO/commit-timing ordering and ordinary-client pacing overhead.
//! All clients connect to this test's private compositor, with bounded reads.
#![allow(clippy::disallowed_methods)]

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::commit_timing::v1::client::{
    wp_commit_timer_v1, wp_commit_timing_manager_v1,
};
use wayland_protocols::wp::fifo::v1::client::{wp_fifo_manager_v1, wp_fifo_v1};
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const EVENT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Probe {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<xdg_wm_base::XdgWmBase>,
    fifo: Option<wp_fifo_manager_v1::WpFifoManagerV1>,
    timing: Option<wp_commit_timing_manager_v1::WpCommitTimingManagerV1>,
    presentation: Option<wp_presentation::WpPresentation>,
    clock: Option<u32>,
    configured: bool,
    syncs: HashSet<usize>,
    frames: Vec<usize>,
    presented: HashMap<usize, u64>,
    discarded: HashSet<usize>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => state.shell = Some(registry.bind(name, version.min(3), qh, ())),
                "wp_fifo_manager_v1" => state.fifo = Some(registry.bind(name, 1, qh, ())),
                "wp_commit_timing_manager_v1" => {
                    state.timing = Some(registry.bind(name, 1, qh, ()))
                }
                "wp_presentation" => state.presentation = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Probe {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Probe {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

#[derive(Clone, Copy)]
enum Callback {
    Sync(usize),
    Frame(usize),
}

impl Dispatch<wl_callback::WlCallback, Callback> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        tag: &Callback,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match *tag {
            Callback::Sync(id) => {
                state.syncs.insert(id);
            }
            Callback::Frame(id) => state.frames.push(id),
        }
    }
}

impl Dispatch<wp_presentation::WpPresentation, ()> for Probe {
    fn event(
        state: &mut Self,
        _: &wp_presentation::WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.clock = Some(clk_id);
        }
    }
}

impl Dispatch<wp_presentation_feedback::WpPresentationFeedback, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        tag: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                ..
            } => {
                let nanos = (((tv_sec_hi as u64) << 32) | tv_sec_lo as u64) * 1_000_000_000
                    + tv_nsec as u64;
                state.presented.insert(*tag, nanos);
            }
            wp_presentation_feedback::Event::Discarded => {
                state.discarded.insert(*tag);
            }
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for Probe {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    wl_compositor::WlCompositor,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    wl_surface::WlSurface,
    xdg_toplevel::XdgToplevel,
    wp_fifo_manager_v1::WpFifoManagerV1,
    wp_fifo_v1::WpFifoV1,
    wp_commit_timing_manager_v1::WpCommitTimingManagerV1,
    wp_commit_timer_v1::WpCommitTimerV1
);

struct Native {
    connection: Connection,
    queue: EventQueue<Probe>,
    probe: Probe,
    serial: usize,
}

impl Native {
    fn connect(session: &Session) -> Self {
        let path = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
            .join(&session.wayland_display);
        let connection = Connection::from_socket(UnixStream::connect(path).unwrap()).unwrap();
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut native = Self {
            connection,
            queue,
            probe: Probe::default(),
            serial: 0,
        };
        native.sync();
        native.sync(); // Bound globals' initial events, including the clock.
        native
    }

    fn pump(&mut self, wait: Duration) {
        self.queue.dispatch_pending(&mut self.probe).unwrap();
        self.connection.flush().unwrap();
        if let Some(reader) = self.queue.prepare_read() {
            let timeout = Timespec::try_from(wait).unwrap();
            let ready = poll(
                &mut [PollFd::new(&self.connection, PollFlags::IN)],
                Some(&timeout),
            )
            .unwrap();
            if ready != 0 {
                match reader.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => panic!("private pacing-client read failed: {error}"),
                }
            }
        }
        self.queue.dispatch_pending(&mut self.probe).unwrap();
    }

    fn until(&mut self, what: &str, predicate: impl Fn(&Probe) -> bool) {
        let deadline = Instant::now() + EVENT;
        while !predicate(&self.probe) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pump(Duration::from_millis(10));
        }
    }

    fn sync(&mut self) {
        self.serial += 1;
        let serial = self.serial;
        self.connection
            .display()
            .sync(&self.queue.handle(), Callback::Sync(serial));
        self.until("private display sync", |p| p.syncs.contains(&serial));
        self.probe.syncs.remove(&serial);
    }

    fn surface(&self) -> wl_surface::WlSurface {
        self.probe
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&self.queue.handle(), ())
    }

    fn window(&mut self, title: &str) -> wl_surface::WlSurface {
        let surface = self.surface();
        let xdg =
            self.probe
                .shell
                .as_ref()
                .unwrap()
                .get_xdg_surface(&surface, &self.queue.handle(), ());
        let toplevel = xdg.get_toplevel(&self.queue.handle(), ());
        toplevel.set_title(title.into());
        self.probe.configured = false;
        surface.commit();
        self.until("initial toplevel configure", |p| p.configured);
        self.paint(&surface, [30, 40, 170, 255], 0);
        self.until("initial frame presented", |p| p.presented.contains_key(&0));
        surface
    }

    fn paint(&self, surface: &wl_surface::WlSurface, bgra: [u8; 4], tag: usize) {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&bgra.repeat(160 * 120)).unwrap();
        let pool = self.probe.shm.as_ref().unwrap().create_pool(
            file.as_fd(),
            160 * 120 * 4,
            &self.queue.handle(),
            (),
        );
        let buffer = pool.create_buffer(
            0,
            160,
            120,
            160 * 4,
            wl_shm::Format::Argb8888,
            &self.queue.handle(),
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, 160, 120);
        surface.frame(&self.queue.handle(), Callback::Frame(tag));
        self.probe
            .presentation
            .as_ref()
            .unwrap()
            .feedback(surface, &self.queue.handle(), tag);
        surface.commit();
        buffer.destroy();
        pool.destroy();
    }

    fn fifo_pair(&self, surface: &wl_surface::WlSurface, first: usize) {
        let fifo = self
            .probe
            .fifo
            .as_ref()
            .unwrap()
            .get_fifo(surface, &self.queue.handle(), ());
        fifo.set_barrier();
        self.paint(surface, [190, 40, 30, 255], first);
        fifo.wait_barrier();
        self.paint(surface, [30, 190, 40, 255], first + 1);
        // Destroying the FIFO object must not erase already committed waits.
        fifo.destroy();
        self.connection.flush().unwrap();
    }
}

fn boot(name: &str) -> Session {
    Session::boot(
        name,
        SessionOptions {
            scale: Some(1.0),
            config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
#[ignore = "needs nested Wayland: scripts/e2e.sh --headless --release --test surface_pacing"]
fn fifo_preserves_distinct_presentations_and_commit_timing_never_presents_early() {
    let session = boot("surface-pacing-visible");
    let mut client = Native::connect(&session);
    let surface = client.window("pacing-visible");
    client.fifo_pair(&surface, 10);
    client.until("both FIFO images really presented", |p| {
        p.presented.contains_key(&10) && p.presented.contains_key(&11)
    });
    assert!(
        !client.probe.discarded.contains(&10),
        "the first FIFO image may not be overwritten before presentation"
    );
    assert!(
        client.probe.presented[&11] > client.probe.presented[&10],
        "FIFO images need distinct presentation times"
    );

    assert_eq!(client.probe.clock, Some(libc::CLOCK_MONOTONIC as u32));
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one initialized timespec; the clock is the
    // monotonic clock explicitly advertised by this private compositor.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    assert_eq!(status, 0);
    let target = now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64 + 300_000_000;
    let timer =
        client
            .probe
            .timing
            .as_ref()
            .unwrap()
            .get_timer(&surface, &client.queue.handle(), ());
    timer.set_timestamp(
        ((target / 1_000_000_000) >> 32) as u32,
        (target / 1_000_000_000) as u32,
        (target % 1_000_000_000) as u32,
    );
    client.paint(&surface, [150, 70, 180, 255], 20);
    client.sync();
    // Compare the protocol's presentation timestamp to the requested clock
    // value, rather than assuming this test process was scheduled promptly.
    client.until("timer wakes idle compositor and presents", |p| {
        p.presented.contains_key(&20)
    });
    assert!(
        client.probe.presented[&20] >= target,
        "commit timing must not present before its timestamp"
    );
    timer.destroy();
    client.paint(&surface, [70, 160, 180, 255], 21);
    client.until("ordinary commit after timer teardown", |p| {
        p.presented.contains_key(&21)
    });
}

#[test]
#[ignore = "needs nested Wayland: scripts/e2e.sh --headless --release --test surface_pacing"]
fn hidden_and_locked_fifo_progress_without_presenting_private_pixels() {
    let mut session = boot("surface-pacing-hidden");
    let mut client = Native::connect(&session);
    let surface = client.window("pacing-hidden");
    session.wait_for_window("pacing-hidden").unwrap();
    // Park the application, then issue two commits behind a real FIFO wait.
    session.door().chord(keys::LEFTMETA, keys::TWO).unwrap();
    assert_eq!(session.world().unwrap().current_workspace, 1);
    client.fifo_pair(&surface, 30);
    client.sync();
    // The blocked second commit replacing the first discards its feedback;
    // neither frame may receive presentation feedback while hidden.
    client.until(
        "hidden FIFO advances without waiting for an impossible frame",
        |p| p.discarded.contains(&30),
    );
    assert!(!client.probe.presented.contains_key(&30) && !client.probe.presented.contains_key(&31));
    session.door().chord(keys::LEFTMETA, keys::ONE).unwrap();
    assert_eq!(session.world().unwrap().current_workspace, 0);
    client.until("revealing the window presents its newest FIFO image", |p| {
        p.presented.contains_key(&31)
    });

    let lock = profile_binary("chonk-lock-probe").unwrap();
    session.launch(lock.to_str().unwrap(), &["--hold"]).unwrap();
    poll_until(EVENT, "private locker confirms lock", || {
        session
            .client_log("chonk-lock-probe")
            .contains("locked ")
            .then_some(())
    })
    .unwrap();
    let locked = session.screenshot("locked-before-fifo").unwrap();
    client.fifo_pair(&surface, 40);
    client.sync();
    client.until(
        "FIFO behind lock progresses without exposing its content",
        |p| p.discarded.contains(&40),
    );
    assert!(!client.probe.presented.contains_key(&40) && !client.probe.presented.contains_key(&41));
    let after = session.screenshot("locked-after-fifo").unwrap();
    assert_eq!(
        locked.diff_fraction(&after, 0),
        0.0,
        "locked output must remain unchanged"
    );
    drop(client); // Disconnect while the last hidden image still owns feedback.
    session.door().barrier().unwrap();
    assert!(
        session.compositor_alive(),
        "pacing scratch must release dead client/surface handles"
    );
}

#[test]
#[ignore = "benchmark: scripts/e2e.sh --headless --release --test surface_pacing no_fifo --nocapture"]
fn no_fifo_many_clients_dispatch_profile() {
    let mut session = boot("surface-pacing-profile");
    let mut driver = Native::connect(&session);
    let mut clients = Vec::new();
    let mut surfaces = Vec::new();
    let memory = std::env::var("CHONKSTEP_PACING_MEMORY").as_deref() == Ok("1");
    let passes: usize = std::env::var("CHONKSTEP_PACING_PASSES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256);
    assert!((16..=100_000).contains(&passes));
    for target in [0, 4, 16, 32] {
        while clients.len() < target {
            let mut client = Native::connect(&session);
            for _ in 0..32 {
                let surface = client.surface();
                surface.commit();
                surfaces.push(surface);
            }
            client.sync();
            clients.push(client);
        }
        for _ in 0..16 {
            driver.sync();
        }
        let mut quiet_since = Instant::now();
        poll_until(
            EVENT,
            "the no-FIFO population settles without rendering",
            || {
                if session.door().frame_stats().unwrap().render_calls != 0 {
                    quiet_since = Instant::now();
                }
                (quiet_since.elapsed() >= Duration::from_millis(100)).then_some(())
            },
        )
        .unwrap();
        let before_memory = memory.then(|| session.door().memory_statistics().unwrap());
        session.door().frame_stats().unwrap();
        let started = Instant::now();
        for _ in 0..passes {
            driver.sync();
        }
        let elapsed = started.elapsed();
        let stats = session.door().frame_stats().unwrap();
        let after_memory = memory.then(|| session.door().memory_statistics().unwrap());
        let allocations = before_memory.as_ref().zip(after_memory.as_ref()).map(|(before, after)| {
            serde_json::json!({
                "operations": after.get("rust_operations").unwrap() - before.get("rust_operations").unwrap(),
                "allocated_bytes": after.get("rust_allocated_bytes").unwrap() - before.get("rust_allocated_bytes").unwrap(),
                "live_bytes_before": before.get("rust_live_bytes").unwrap(),
                "live_bytes_after": after.get("rust_live_bytes").unwrap(),
            })
        });
        println!(
            "pacing-profile {}",
            serde_json::json!({
                "clients": target, "surfaces": surfaces.len(), "roundtrips": passes,
                "elapsed_us": elapsed.as_micros(), "dispatch_calls": stats.dispatch_calls,
                "dispatch_us": stats.dispatch_us, "protocol_us": stats.protocol_us,
                "layout_us": stats.layout_us, "renders": stats.render_calls, "allocations": allocations,
            })
        );
        assert_eq!(
            stats.render_calls, 0,
            "ordinary idle surfaces must not cause rendering"
        );
    }
    drop(surfaces);
    drop(clients);
    for _ in 0..16 {
        driver.sync();
    }
    session.door().barrier().unwrap();
    assert!(
        session.compositor_alive(),
        "disconnecting ordinary clients keeps the compositor responsive"
    );
}
