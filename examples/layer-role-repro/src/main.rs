//! Minimal reproducer for the smithay 0.7 wlr-layer-shell stale pre-commit
//! hook bug.
//!
//! Mimics what Qt/Quickshell does when it hides a layer-shell popup: it
//! destroys the `zwlr_layer_surface_v1` role object but KEEPS the underlying
//! `wl_surface` alive, unmapping it with a nil buffer instead.
//!
//!   1. bind wl_compositor, wl_shm, zwlr_layer_shell_v1
//!   2. wl_surface + get_layer_surface
//!   3. set_size(200, 100) + set_anchor(TOP)   <- NOT anchored left+right
//!   4. commit, wait for configure, ack
//!   5. attach a real buffer, commit  (surface is now mapped)
//!   6. zwlr_layer_surface_v1.destroy()
//!   7. wl_surface.attach(nil, 0, 0) + wl_surface.commit()
//!   8. roundtrip -> on smithay 0.7 the client is killed with
//!      zwlr_layer_surface_v1 error 1 (invalid_size),
//!      "width 0 requested without setting left and right anchors"
//!
//! Exit codes: 0 = survived (no bug), 2 = killed by the invalid_size protocol
//! error (bug reproduced), 3 = killed by some other protocol error, 1 = setup
//! failure.

use std::os::fd::AsFd;

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_shm, wl_shm_pool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

#[derive(Default)]
struct App {
    compositor: Option<WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    configured: bool,
    cfg_size: (u32, u32),
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        st: &mut Self,
        registry: &wl_registry::WlRegistry,
        ev: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = ev
        {
            match &interface[..] {
                "wl_compositor" => {
                    st.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" => st.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => {
                    st.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        st: &mut Self,
        ls: &ZwlrLayerSurfaceV1,
        ev: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                eprintln!("[repro] configure serial={serial} {width}x{height}");
                ls.ack_configure(serial);
                st.cfg_size = (width, height);
                st.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => eprintln!("[repro] closed"),
            _ => {}
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(),
                     _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(
    WlCompositor,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    WlBuffer,
    WlSurface,
    ZwlrLayerShellV1
);

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "destroy-first".into());
    let conn = Connection::connect_to_env().expect("no WAYLAND_DISPLAY");
    let mut q = conn.new_event_queue();
    let qh = q.handle();
    let mut st = App::default();
    conn.display().get_registry(&qh, ());
    q.roundtrip(&mut st).unwrap();

    let (comp, shm, shell) = match (&st.compositor, &st.shm, &st.layer_shell) {
        (Some(c), Some(s), Some(l)) => (c.clone(), s.clone(), l.clone()),
        _ => {
            eprintln!("[repro] FAIL: compositor lacks wl_compositor/wl_shm/zwlr_layer_shell_v1");
            std::process::exit(1);
        }
    };

    // 2/3. Layer surface: explicit size, anchored TOP only -- like a centered
    // menu. Deliberately NOT anchored left+right, so the default (0-width,
    // no-anchor) state is a protocol error for this surface.
    let surface = comp.create_surface(&qh, ());
    let layer = shell.get_layer_surface(
        &surface,
        None,
        zwlr_layer_shell_v1::Layer::Top,
        "layer-repro".into(),
        &qh,
        (),
    );
    layer.set_size(200, 100);
    layer.set_anchor(zwlr_layer_surface_v1::Anchor::Top);
    surface.commit();

    // 4. Wait for the initial configure.
    for _ in 0..50 {
        q.blocking_dispatch(&mut st).unwrap();
        if st.configured {
            break;
        }
    }
    if !st.configured {
        eprintln!("[repro] FAIL: never got a configure");
        std::process::exit(1);
    }

    // 5. Map it with a real buffer.
    let (w, h) = (
        if st.cfg_size.0 == 0 { 200 } else { st.cfg_size.0 } as i32,
        if st.cfg_size.1 == 0 { 100 } else { st.cfg_size.1 } as i32,
    );
    let len = (w * h * 4) as usize;
    let file = tmpfile(len);
    let pool = shm.create_pool(file.as_fd(), len as i32, &qh, ());
    let buf = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &qh, ());
    surface.attach(Some(&buf), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    surface.commit();
    q.roundtrip(&mut st).unwrap();
    eprintln!("[repro] mapped {w}x{h}; surface is live");

    // 6/7. Teardown. Three orderings, selected by argv[1]:
    //   destroy-first (default) -- what Qt/Quickshell and gtk4-layer-shell do
    //   unmap-first             -- the ordering upstream suggests clients adopt
    //   bare-commit             -- destroy, then commit with no attach at all
    match mode.as_str() {
        "unmap-first" => {
            eprintln!("[repro] mode=unmap-first: wl_surface.attach(nil) + commit()");
            surface.attach(None, 0, 0);
            surface.commit();
            q.roundtrip(&mut st).ok();
            eprintln!("[repro] zwlr_layer_surface_v1.destroy()");
            layer.destroy();
            surface.commit();
        }
        "bare-commit" => {
            eprintln!("[repro] mode=bare-commit: zwlr_layer_surface_v1.destroy()");
            layer.destroy();
            eprintln!("[repro] wl_surface.commit() with no attach");
            surface.commit();
        }
        _ => {
            eprintln!("[repro] mode=destroy-first: zwlr_layer_surface_v1.destroy()");
            layer.destroy();
            eprintln!("[repro] wl_surface.attach(nil) + wl_surface.commit()");
            surface.attach(None, 0, 0);
            surface.commit();
        }
    }

    // 8. Verdict.
    //
    // Two separate reads of the outcome, because smithay posts this error on
    // an object the client has already destroyed. libwayland drops such an
    // error silently; wayland-rs may or may not decode it -- so the *decoded
    // error* is a nice-to-have and the *liveness of the connection* is the
    // authoritative signal.
    //
    // (a) drain anything the compositor sent, without writing (a write into a
    //     closed socket reports EPIPE and masks the real cause);
    // (b) then probe with a roundtrip: it succeeds iff we are still connected.
    conn.flush().ok();
    let mut decoded = None;
    for _ in 0..10 {
        if let Some(pe) = conn.protocol_error() {
            decoded = Some(pe);
            break;
        }
        let Some(guard) = conn.prepare_read() else {
            q.dispatch_pending(&mut st).ok();
            continue;
        };
        let mut fds = [rustix::event::PollFd::new(
            &conn,
            rustix::event::PollFlags::IN,
        )];
        let timeout = rustix::fs::Timespec {
            tv_sec: 0,
            tv_nsec: 300_000_000,
        };
        if rustix::event::poll(&mut fds, Some(&timeout)).unwrap_or(0) == 0 {
            break; // nothing more to say; fall through to the liveness probe
        }
        if guard.read().is_err() {
            decoded = conn.protocol_error();
            break;
        }
        q.dispatch_pending(&mut st).ok();
    }

    // Liveness probe. A surviving client can still round-trip.
    let alive = {
        let probe = comp.create_surface(&qh, ());
        let ok = q.roundtrip(&mut st).is_ok();
        if ok {
            probe.destroy();
            conn.flush().ok();
        }
        ok
    };

    if let Some(pe) = &decoded {
        eprintln!(
            "[repro] protocol error decoded: object={} id={} code={} message={:?}",
            pe.object_interface, pe.object_id, pe.code, pe.message
        );
    } else {
        eprintln!("[repro] no wl_display.error decoded on the wire");
    }

    if alive {
        eprintln!(
            "[repro] RESULT: SURVIVED -- connection still live after \
             destroy()+attach(nil)+commit(). No bug on this compositor."
        );
        std::process::exit(0);
    }

    eprintln!(
        "[repro] RESULT: KILLED -- connection is dead after \
         destroy()+attach(nil)+commit()."
    );
    match &decoded {
        Some(pe) if pe.object_interface == "zwlr_layer_surface_v1" && pe.code == 1 => {
            eprintln!("[repro] ^ invalid_size on an ALREADY-DESTROYED layer surface: the bug.");
            std::process::exit(2);
        }
        Some(_) => std::process::exit(3),
        None => {
            eprintln!(
                "[repro] ^ error was posted on a dead object, so nothing decodable reached \
                 the client. Check the compositor log for the invalid_size error."
            );
            std::process::exit(2);
        }
    }
}
fn tmpfile(len: usize) -> std::fs::File {
    use std::io::Write;
    let path = format!(
        "{}/layer-repro-{}",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/dev/shm".into()),
        std::process::id()
    );
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("shm file");
    let _ = std::fs::remove_file(&path);
    f.write_all(&vec![0xffu8; len]).unwrap();
    f.flush().unwrap();
    f
}

#[allow(dead_code)]
fn unused(_: WEnum<u32>) {}
