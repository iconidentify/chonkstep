//! Hostile Wayland clients, one strategy per name.
//!
//! Every bound on untrusted client input in this compositor was added
//! one incident at a time: an absurd window geometry that panicked the
//! decoration allocator, a subsurface chain deep enough to walk the
//! compositor off its own stack, protocol ledgers with no ceiling, a
//! connection storm. Each was found in the field, fixed, and given a
//! regression test of its own — and nothing swept the surface looking
//! for the next one.
//!
//! This is that sweep's client half. Each strategy is a client that
//! misbehaves in one specific way and then reports what happened to it;
//! `tests/protocol_torture.rs` runs them against a live session and
//! asserts the same contract every time — the compositor survives, the
//! offender is the only casualty, and an innocent client on the same
//! desktop is untouched.
//!
//! Adding a strategy is adding an arm to `main` and a name to the
//! test's table. That is the point: the next bound should arrive with
//! its torture case, not after it.

use std::io::Write;
use std::os::fd::AsFd;

use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_buffer,
    wl_subcompositor::WlSubcompositor, wl_subsurface::WlSubsurface, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

fn fatal(message: &str) -> ! {
    eprintln!("chonk-protocol-torture: {message}");
    std::process::exit(1);
}

fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

#[derive(Default)]
struct Probe {
    compositor: Option<WlCompositor>,
    subcompositor: Option<WlSubcompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<XdgWmBase>,
    configured: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(
        probe: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => probe.compositor = Some(registry.bind(name, version.min(4), qh, ())),
                "wl_subcompositor" => {
                    probe.subcompositor = Some(registry.bind(name, version.min(1), qh, ()))
                }
                "wl_shm" => probe.shm = Some(registry.bind(name, version.min(1), qh, ())),
                "xdg_wm_base" => probe.wm_base = Some(registry.bind(name, version.min(3), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Probe {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
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

impl Dispatch<XdgSurface, ()> for Probe {
    fn event(
        probe: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            probe.configured = true;
        }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for Probe {
            fn event(
                _: &mut Self,
                _: &$proxy,
                _: <$proxy as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore_events!(
    WlCompositor,
    WlSubcompositor,
    WlSurface,
    WlSubsurface,
    XdgToplevel,
    wl_region::WlRegion,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
);

/// One opaque buffer, the honest part of every strategy.
fn buffer(shm: &wl_shm::WlShm, qh: &QueueHandle<Probe>, width: i32, height: i32) -> wl_buffer::WlBuffer {
    let path = format!(
        "{}/chonk-protocol-torture-{}",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/dev/shm".into()),
        std::process::id()
    );
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap_or_else(|error| fatal(&format!("scratch file: {error}")));
    let _ = std::fs::remove_file(&path);
    let bytes: Vec<u8> = std::iter::repeat_n([0x20u8, 0x80, 0x20, 0xFF], (width * height) as usize)
        .flatten()
        .collect();
    let mut writer = &file;
    writer.write_all(&bytes).unwrap_or_else(|error| fatal(&format!("fill: {error}")));
    writer.flush().unwrap_or_else(|error| fatal(&format!("flush: {error}")));
    let pool = shm.create_pool(file.as_fd(), width * height * 4, qh, ());
    let out = pool.create_buffer(0, width, height, width * 4, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let strategy = args.next().unwrap_or_else(|| fatal("usage: chonk-protocol-torture <strategy> [n]"));
    let magnitude: usize = args.next().and_then(|arg| arg.parse().ok()).unwrap_or(4096);

    let connection = Connection::connect_to_env().unwrap_or_else(|e| fatal(&format!("connect: {e}")));
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut probe = Probe::default();
    queue.roundtrip(&mut probe).unwrap_or_else(|e| fatal(&format!("registry roundtrip: {e}")));

    let compositor = probe.compositor.clone().unwrap_or_else(|| fatal("no wl_compositor"));
    let shm = probe.shm.clone().unwrap_or_else(|| fatal("no wl_shm"));
    let wm_base = probe.wm_base.clone().unwrap_or_else(|| fatal("no xdg_wm_base"));
    say(&format!("**strategy {strategy} magnitude {magnitude}**"));

    match strategy.as_str() {
        // Seed one: the declaration that used to reach the decoration
        // allocator as a width and take the session down with it.
        "absurd-geometry" => {
            let surface = compositor.create_surface(&qh, ());
            let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
            let toplevel = xdg.get_toplevel(&qh, ());
            toplevel.set_title("torture-geometry".to_string());
            toplevel.set_app_id("torture-geometry".to_string());
            xdg.set_window_geometry(0, 0, 600_000_000, 10);
            xdg.set_window_geometry(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
            surface.commit();
            let _ = queue.roundtrip(&mut probe);
            let frame = buffer(&shm, &qh, 400, 300);
            surface.attach(Some(&frame), 0, 0);
            surface.damage(0, 0, 400, 300);
            surface.commit();
        }
        // Seed two: the chain deep enough to walk the compositor off
        // its own stack, built leaf-first because that is the cheap way.
        "deep-subsurface" => {
            let subcompositor =
                probe.subcompositor.clone().unwrap_or_else(|| fatal("no wl_subcompositor"));
            let chain: Vec<WlSurface> =
                (0..=magnitude).map(|_| compositor.create_surface(&qh, ())).collect();
            for step in 0..magnitude {
                subcompositor.get_subsurface(&chain[step], &chain[step + 1], &qh, ());
                // Let the compositor answer periodically. Writing the
                // whole chain in one breath fills the client's own send
                // buffer and kills it with `EAGAIN` before a single
                // request has been read — which proves nothing about
                // the compositor. Stopping at the first refusal is the
                // point: a bound that fires is supposed to be visible.
                if step % 64 == 63 && queue.roundtrip(&mut probe).is_err() {
                    break;
                }
            }
            let _ = queue.roundtrip(&mut probe);
            chain[magnitude].commit();
        }
        // The ledgers: one client, an unreasonable number of protocol
        // objects, to find the collection with no ceiling on it.
        "object-flood" => {
            let mut surfaces = Vec::with_capacity(magnitude);
            let mut regions = Vec::with_capacity(magnitude);
            for index in 0..magnitude {
                surfaces.push(compositor.create_surface(&qh, ()));
                let region = compositor.create_region(&qh, ());
                region.add(0, 0, 16, 16);
                regions.push(region);
                // Same reason as the chain above: drained periodically
                // so the compositor is the one deciding, not the
                // client's own socket buffer.
                if index % 256 == 255 && queue.roundtrip(&mut probe).is_err() {
                    break;
                }
            }
            for surface in &surfaces {
                surface.commit();
            }
        }
        // Roles taken, destroyed and taken again on one surface, which
        // is the shape that outlives a protocol hook and kills a
        // connection for an error it did not commit.
        "role-churn" => {
            for round in 0..magnitude.min(512) {
                if round % 64 == 63 && queue.roundtrip(&mut probe).is_err() {
                    break;
                }
                let surface = compositor.create_surface(&qh, ());
                let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
                let toplevel = xdg.get_toplevel(&qh, ());
                surface.commit();
                toplevel.destroy();
                xdg.destroy();
                surface.attach(None, 0, 0);
                surface.commit();
                surface.destroy();
            }
        }
        other => fatal(&format!("unknown strategy {other:?}")),
    }

    match queue.roundtrip(&mut probe) {
        Ok(_) => say("**survived**"),
        Err(error) => say(&format!("**disconnected: {error}**")),
    }
    // Whatever the compositor decided, report what the connection does
    // next: a refusal that does not disconnect leaves the abuse in place.
    match queue.roundtrip(&mut probe) {
        Ok(_) => say("**connection still alive**"),
        Err(error) => say(&format!("**connection closed: {error}**")),
    }
    say("**done**");
    // Hold the connection open so the test can observe the desktop with
    // this client still attached, rather than racing its teardown.
    loop {
        if queue.blocking_dispatch(&mut probe).is_err() {
            return;
        }
    }
}
