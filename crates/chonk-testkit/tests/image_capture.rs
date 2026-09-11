//! Native ext-image-copy-capture lifetime and locked-output behavior.
#![allow(clippy::disallowed_methods)]

use chonk_testkit::{poll_until, profile_binary, Screenshot, Session, SessionOptions};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::{fs::FileExt, net::UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_output, wl_registry, wl_shm, wl_shm_pool,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1, ext_output_image_capture_source_manager_v1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1, ext_image_copy_capture_manager_v1,
    ext_image_copy_capture_session_v1,
};

const EVENT: Duration = Duration::from_secs(5);
type CopySession = ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1;
type CopyFrame = ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1;

#[derive(Default)]
struct Probe {
    output: Option<wl_output::WlOutput>,
    shm: Option<wl_shm::WlShm>,
    sources:
        Option<ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1>,
    copies: Option<ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1>,
    sizes: HashMap<usize, (u32, u32)>,
    constraints_done: HashSet<usize>,
    stopped: HashSet<usize>,
    ready: HashSet<usize>,
    ready_order: Vec<usize>,
    failed: HashSet<usize>,
    syncs: HashSet<usize>,
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
                "wl_output" if state.output.is_none() => {
                    state.output = Some(registry.bind(name, version.min(3), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "ext_output_image_capture_source_manager_v1" => {
                    state.sources = Some(registry.bind(name, 1, qh, ()))
                }
                "ext_image_copy_capture_manager_v1" => {
                    state.copies = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<wl_callback::WlCallback, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        id: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.syncs.insert(*id);
    }
}
impl Dispatch<CopySession, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &CopySession,
        event: ext_image_copy_capture_session_v1::Event,
        id: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                state.sizes.insert(*id, (width, height));
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                state.constraints_done.insert(*id);
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                state.stopped.insert(*id);
            }
            _ => {}
        }
    }
}
impl Dispatch<CopyFrame, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &CopyFrame,
        event: ext_image_copy_capture_frame_v1::Event,
        id: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => {
                state.ready.insert(*id);
                state.ready_order.push(*id);
            }
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => {
                state.failed.insert(*id);
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
    wl_output::WlOutput,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1
);

struct Client {
    connection: Connection,
    queue: EventQueue<Probe>,
    probe: Probe,
    serial: usize,
}
struct Frame {
    resource: CopyFrame,
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    pixels: Arc<File>,
    size: (u32, u32),
    id: usize,
}
impl Client {
    fn connect(session: &Session) -> Self {
        Self::connect_display(&session.wayland_display)
    }
    fn connect_display(display: &str) -> Self {
        let socket = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
            .join(display);
        let connection = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut client = Self {
            connection,
            queue,
            probe: Probe::default(),
            serial: 0,
        };
        client.sync();
        client.sync();
        client
    }
    fn until(&mut self, what: &str, condition: impl Fn(&Probe) -> bool) {
        self.until_for(EVENT, what, condition);
    }
    fn until_for(&mut self, timeout: Duration, what: &str, condition: impl Fn(&Probe) -> bool) {
        let deadline = Instant::now() + timeout;
        while !condition(&self.probe) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.queue.dispatch_pending(&mut self.probe).unwrap();
            self.connection.flush().unwrap();
            if let Some(reader) = self.queue.prepare_read() {
                let timeout = Timespec::try_from(Duration::from_millis(10)).unwrap();
                if poll(
                    &mut [PollFd::new(&self.connection, PollFlags::IN)],
                    Some(&timeout),
                )
                .unwrap()
                    > 0
                {
                    match reader.read() {
                        Ok(_) => {}
                        Err(wayland_client::backend::WaylandError::Io(error))
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(error) => panic!("private capture-client read failed: {error}"),
                    }
                }
            }
            self.queue.dispatch_pending(&mut self.probe).unwrap();
        }
    }
    fn sync(&mut self) {
        self.serial += 1;
        let id = self.serial;
        self.connection.display().sync(&self.queue.handle(), id);
        self.until("private capture-client sync", |p| p.syncs.contains(&id));
        self.probe.syncs.remove(&id);
    }
    fn request_session(&mut self) -> (CopySession, usize) {
        self.serial += 1;
        let id = self.serial;
        let source = self.probe.sources.as_ref().unwrap().create_source(
            self.probe.output.as_ref().unwrap(),
            &self.queue.handle(),
            (),
        );
        let session = self.probe.copies.as_ref().unwrap().create_session(
            &source,
            ext_image_copy_capture_manager_v1::Options::empty(),
            &self.queue.handle(),
            id,
        );
        source.destroy(); // The capture session owns its resolved source.
        (session, id)
    }
    fn session(&mut self) -> (CopySession, (u32, u32)) {
        let (session, id) = self.request_session();
        self.until("complete output capture constraints", |p| {
            p.constraints_done.contains(&id) || p.stopped.contains(&id)
        });
        assert!(
            !self.probe.stopped.contains(&id),
            "unexpectedly stopped session {id}"
        );
        (session, self.probe.sizes[&id])
    }
    fn frame(&mut self, session: &CopySession, size: (u32, u32)) -> Frame {
        self.serial += 1;
        let id = self.serial;
        let pixels = Arc::new(tempfile::tempfile().unwrap());
        let length = size.0 as u64 * size.1 as u64 * 4;
        pixels.set_len(length).unwrap();
        let pool = self.probe.shm.as_ref().unwrap().create_pool(
            pixels.as_fd(),
            length as i32,
            &self.queue.handle(),
            (),
        );
        let buffer = pool.create_buffer(
            0,
            size.0 as i32,
            size.1 as i32,
            (size.0 * 4) as i32,
            wl_shm::Format::Xrgb8888,
            &self.queue.handle(),
            (),
        );
        let resource = session.create_frame(&self.queue.handle(), id);
        resource.attach_buffer(&buffer);
        resource.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
        Frame {
            resource,
            pool,
            buffer,
            pixels,
            size,
            id,
        }
    }
    fn capture(&mut self, frame: &Frame) {
        frame.resource.capture();
        self.until("native image copy completes", |p| {
            p.ready.contains(&frame.id) || p.failed.contains(&frame.id)
        });
        assert!(
            !self.probe.failed.contains(&frame.id),
            "capture frame {} failed",
            frame.id
        );
    }

    fn frame_sharing_pixels(&mut self, session: &CopySession, first: &Frame) -> Frame {
        self.serial += 1;
        let id = self.serial;
        // Each outstanding frame owns a distinct protocol buffer. Only its
        // storage is shared; the test does not access these pixels until all
        // compositor writes are complete.
        let buffer = first.pool.create_buffer(
            0,
            first.size.0 as i32,
            first.size.1 as i32,
            (first.size.0 * 4) as i32,
            wl_shm::Format::Xrgb8888,
            &self.queue.handle(),
            (),
        );
        let resource = session.create_frame(&self.queue.handle(), id);
        resource.attach_buffer(&buffer);
        resource.damage_buffer(0, 0, first.size.0 as i32, first.size.1 as i32);
        Frame {
            resource,
            pool: first.pool.clone(),
            buffer,
            pixels: Arc::clone(&first.pixels),
            size: first.size,
            id,
        }
    }
}
impl Frame {
    fn compare(&self, expected: &Screenshot) {
        assert_eq!(self.size, (expected.width, expected.height));
        for y in (3..self.size.1).step_by(47) {
            for x in (3..self.size.0).step_by(43) {
                let mut bgra = [0; 4];
                self.pixels
                    .read_exact_at(&mut bgra, (y as u64 * self.size.0 as u64 + x as u64) * 4)
                    .unwrap();
                assert_eq!(
                    [bgra[2], bgra[1], bgra[0]],
                    expected.pixel(x, y)[..3],
                    "capture pixel at ({x}, {y})"
                );
            }
        }
    }
    fn destroy(self) {
        self.resource.destroy();
        self.buffer.destroy();
        self.pool.destroy();
    }
}

#[test]
#[ignore = "needs nested Wayland: scripts/e2e.sh --headless --release --test image_capture"]
fn first_idle_frame_and_frame_after_session_destroy_capture_exact_output() {
    let mut session = Session::boot(
        "image-capture-lifetime",
        SessionOptions {
            scale: Some(1.5),
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let expected = session.screenshot("idle-source").unwrap();
    let mut client = Client::connect(&session);
    let (copy, size) = client.session();
    // No damage or presentation barrier intervenes: the first frame must
    // complete even when its source has been completely idle.
    let first = client.frame(&copy, size);
    client.capture(&first);
    first.compare(&expected);
    first.destroy();
    client.sync();
    let orphan = client.frame(&copy, size);
    copy.destroy(); // Existing frames explicitly survive session destruction.
    client.capture(&orphan);
    orphan.compare(&expected);
    orphan.destroy();
    client.sync();
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs nested Wayland: scripts/e2e.sh --headless --release --test image_capture"]
fn an_existing_capture_session_switches_to_the_locked_scene() {
    let mut session = Session::boot(
        "image-capture-lock",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let unlocked = session.screenshot("unlocked-source").unwrap();
    let mut client = Client::connect(&session);
    let (copy, size) = client.session();
    let before = client.frame(&copy, size);
    client.capture(&before);
    before.compare(&unlocked);
    before.destroy();
    client.sync();
    let locker = profile_binary("chonk-lock-probe").unwrap();
    session
        .launch(locker.to_str().unwrap(), &["--hold"])
        .unwrap();
    poll_until(EVENT, "private lock confirmation", || {
        session
            .client_log("chonk-lock-probe")
            .contains("locked ")
            .then_some(())
    })
    .unwrap();
    let locked = session.screenshot("locked-source").unwrap();
    assert!(
        locked.diff_fraction(&unlocked, 0) > 0.5,
        "lock visibly replaces the desktop"
    );
    let after = client.frame(&copy, size);
    client.capture(&after);
    after.compare(&locked);
    after.destroy();
    copy.destroy();
    client.sync();
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs nested Wayland: scripts/e2e.sh --headless --release --test image_capture"]
fn live_session_overflow_stops_cleanly_and_destroy_releases_admission() {
    let mut session =
        Session::boot("image-capture-session-cap", SessionOptions::default()).unwrap();
    let mut client = Client::connect(&session);
    let sessions: Vec<_> = (0..257).map(|_| client.request_session()).collect();
    client.sync();
    client.until("all session admission replies", |p| {
        p.constraints_done.len() + p.stopped.len() == sessions.len()
    });
    assert_eq!(client.probe.constraints_done.len(), 256);
    assert_eq!(client.probe.stopped, HashSet::from([sessions[256].1]));
    assert!(!client.probe.sizes.contains_key(&sessions[256].1));

    sessions[0].0.destroy();
    client.sync();
    let (replacement, size) = client.session();
    let frame = client.frame(&replacement, size);
    client.capture(&frame);
    frame.destroy();
    replacement.destroy();
    for (session, _) in sessions.into_iter().skip(1) {
        session.destroy();
    }
    client.sync();
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs nested Wayland: scripts/e2e.sh --headless --release --test image_capture"]
fn capture_flood_yields_to_unrelated_clients_and_input_and_bounds_pending_admission() {
    let mut session = Session::boot(
        "image-capture-flood",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let input = profile_binary("chonk-input-probe").unwrap();
    session.launch(input.to_str().unwrap(), &["1"]).unwrap();
    let window = session.wait_for_window("input-probe").unwrap();
    session
        .door()
        .motion((window.x + 30) as f64, (window.y + 50) as f64)
        .unwrap();
    session.door().barrier().unwrap();
    let releases = session
        .client_log("chonk-input-probe")
        .matches(" release ")
        .count();
    let mut client = Client::connect(&session);
    let mut unrelated = Client::connect(&session);
    let (copy, size) = client.session();
    let mut frames = vec![client.frame(&copy, size)];
    copy.destroy();
    // All frames share one SHM allocation. The compositor writes each in
    // sequence; this test never reads a buffer while another frame owns it.
    // Orphan frames are legal and let the pending cap be tested separately
    // from the live-session cap, without allocating a gigabyte of pixels.
    for _ in 1..257 {
        let (copy, next_size) = client.session();
        assert_eq!(next_size, size);
        frames.push(client.frame_sharing_pixels(&copy, &frames[0]));
        copy.destroy();
    }
    client.sync();
    for frame in &frames {
        frame.resource.capture();
    }
    client.sync();
    let started = Instant::now();
    unrelated.sync();
    let roundtrip = started.elapsed();
    session.door().button("left", true).unwrap();
    session.door().button("left", false).unwrap();
    poll_until(
        EVENT,
        "input is delivered while the capture queue drains",
        || {
            (session
                .client_log("chonk-input-probe")
                .matches(" release ")
                .count()
                == releases + 1)
                .then_some(())
        },
    )
    .unwrap();
    client.sync();
    assert_eq!(client.probe.failed, HashSet::from([frames[256].id]));
    assert!(
        !client.probe.ready.is_empty() && client.probe.ready.len() < 256,
        "both unrelated native sync and application input must finish before the capture backlog; ready={}",
        client.probe.ready.len()
    );
    println!(
        "image-capture-flood native_roundtrip_us={} completed_at_input={} accepted=256 rejected=1",
        roundtrip.as_micros(),
        client.probe.ready.len()
    );
    // No new protocol requests or injected input drive this wait: queued
    // frames must make eventual progress using the explicit service deadline.
    client.until_for(
        Duration::from_secs(30),
        "all accepted captures finish without starvation",
        |p| p.ready.len() == 256,
    );
    assert_eq!(
        client.probe.ready_order,
        frames[..256].iter().map(|f| f.id).collect::<Vec<_>>()
    );
    let pool = frames[0].pool.clone();
    for frame in frames {
        frame.resource.destroy();
        frame.buffer.destroy();
    }
    pool.destroy();
    client.sync();
    assert!(session.compositor_alive());
}

#[path = "support/readback.rs"]
mod readback;

#[test]
#[ignore = "needs nested Wayland: scripts/e2e.sh --headless --host-renderer gl --release --test image_capture"]
fn pending_download_does_not_block_input_and_destroyed_buffer_is_never_written() {
    let mut session = Session::boot("image-capture-pending", SessionOptions {
        config_extra: "show_dock = false\n".into(),
        env: vec![("CHONKSTEP_TEST_READBACK_DELAY_MS".into(), "800".into())],
        ..Default::default()
    }).unwrap();
    let expected = session.screenshot("pending-source").unwrap();
    let mut client = Client::connect(&session);
    let (copy, size) = client.session();
    let first = client.frame(&copy, size);
    let before = readback::counter(&session, "queued");
    first.resource.capture();
    client.sync();
    poll_until(EVENT, "GPU capture queued", || {
        (readback::counter(&session, "queued") > before).then_some(())
    }).unwrap();
    let start = Instant::now();
    session.door().barrier().unwrap();
    client.sync();
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(!client.probe.ready.contains(&first.id));
    first.buffer.destroy();
    client.until("destroyed pending buffer fails", |p| p.failed.contains(&first.id));
    let mut untouched = vec![0; (size.0 * size.1 * 4) as usize];
    first.pixels.read_exact_at(&mut untouched, 0).unwrap();
    assert!(untouched.iter().all(|byte| *byte == 0));
    first.resource.destroy(); first.pool.destroy();
    let next = client.frame(&copy, size);
    client.capture(&next);
    next.compare(&expected);
    next.destroy(); copy.destroy(); client.sync();
    assert!(readback::counter(&session, "pending_polls") > 0);
    poll_until(EVENT, "GPU storage released", || {
        (readback::counter(&session, "active_bytes") == 0).then_some(())
    }).unwrap();
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs nested Wayland, grim and a real lock client"]
fn demo_image_capture_keeps_its_policy_and_respects_locking() {
    let mut session = Session::boot("image-capture-demo", SessionOptions {
        scale: Some(1.5),
        config_extra: "show_dock = false\ninteraction_mode = 'mac'\n".into(),
        ..Default::default()
    }).unwrap();
    let clean = session.screenshot("demo-clean-before").unwrap();
    session.door().key(125, true).unwrap();
    session.door().key(42, true).unwrap();
    session.door().tap_key(6).unwrap();
    session.door().key(42, false).unwrap();
    session.door().key(125, false).unwrap();
    session.door().barrier().unwrap();
    let demo_display = format!("chonkstep-capture-{}", session.wayland_display);
    let normal_display = std::mem::replace(&mut session.wayland_display, demo_display.clone());
    let visible = session.screenshot("demo-visible-controls").unwrap();
    session.wayland_display = normal_display;
    assert!(visible.diff_fraction(&clean, 0) > 0.03);
    let mut normal = Client::connect(&session);
    let mut demo = Client::connect_display(&demo_display);
    let (plain_session, size) = normal.session();
    let (demo_session, demo_size) = demo.session();
    assert_eq!(size, demo_size);
    for _ in 0..4 {
        let plain_frame = normal.frame(&plain_session, size);
        let demo_frame = demo.frame(&demo_session, size);
        normal.capture(&plain_frame);
        demo.capture(&demo_frame);
        plain_frame.compare(&clean);
        demo_frame.compare(&visible);
        plain_frame.destroy();
        demo_frame.destroy();
    }
    let locker = profile_binary("chonk-lock-probe").unwrap();
    session.launch(locker.to_str().unwrap(), &["--hold"]).unwrap();
    poll_until(EVENT, "demo lock confirmation", || {
        session.client_log("chonk-lock-probe").contains("locked ").then_some(())
    }).unwrap();
    let locked = session.screenshot("demo-locked").unwrap();
    assert!(locked.diff_fraction(&visible, 0) > 0.5);
    let frame = demo.frame(&demo_session, size);
    demo.capture(&frame);
    frame.compare(&locked);
    frame.destroy();
    demo_session.destroy();
    plain_session.destroy();
    demo.sync();
    normal.sync();
    assert!(session.compositor_alive());
}
