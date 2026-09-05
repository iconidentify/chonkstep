//! A client that locks the pointer and reports what the compositor
//! sends it afterwards.
//!
//! `zwp_locked_pointer_v1` means the pointer does not move. The protocol
//! is explicit that the compositor must stop sending `wl_pointer.motion`
//! for the duration and that the client reads `zwp_relative_pointer_v1`
//! instead — which is the whole point, because a first-person camera or
//! a 3D viewport integrates the relative stream. A compositor that sends
//! both makes such an application drift: it turns by the delta *and* is
//! told an absolute position it never asked to move to.
//!
//! The probe therefore counts the two streams separately and says so.

use std::io::Write;
use std::os::fd::AsFd;

use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_buffer,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{self, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

fn fatal(message: &str) -> ! {
    eprintln!("chonk-pointer-lock-probe: {message}");
    std::process::exit(1);
}

fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

#[derive(Default)]
struct Probe {
    compositor: Option<WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    constraints: Option<ZwpPointerConstraintsV1>,
    relative_manager: Option<ZwpRelativePointerManagerV1>,
    pointer: Option<wl_pointer::WlPointer>,
    configured: bool,
    entered: bool,
    /// `wl_pointer.motion` events received. Must stop at zero new ones
    /// once the lock is in force.
    motions: u32,
    /// `zwp_relative_pointer_v1.relative_motion` events received. Must
    /// keep arriving while locked — that is the stream the client uses.
    relatives: u32,
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
                "wl_shm" => probe.shm = Some(registry.bind(name, version.min(1), qh, ())),
                "xdg_wm_base" => probe.wm_base = Some(registry.bind(name, version.min(3), qh, ())),
                "wl_seat" => probe.seat = Some(registry.bind(name, version.min(5), qh, ())),
                "zwp_pointer_constraints_v1" => {
                    probe.constraints = Some(registry.bind(name, version.min(1), qh, ()))
                }
                "zwp_relative_pointer_manager_v1" => {
                    probe.relative_manager = Some(registry.bind(name, version.min(1), qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Probe {
    fn event(
        probe: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            let has_pointer = capabilities
                .into_result()
                .map(|caps| caps.contains(wl_seat::Capability::Pointer))
                .unwrap_or(false);
            if has_pointer && probe.pointer.is_none() {
                probe.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { .. } => {
                probe.entered = true;
                say("**pointer entered**");
            }
            wl_pointer::Event::Motion { .. } => {
                probe.motions += 1;
                say(&format!("**motion {}**", probe.motions));
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { .. } = event {
            probe.relatives += 1;
            say(&format!("**relative {}**", probe.relatives));
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => say("**locked**"),
            zwp_locked_pointer_v1::Event::Unlocked => say("**unlocked**"),
            _ => {}
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
    WlSurface,
    XdgToplevel,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    ZwpPointerConstraintsV1,
    ZwpRelativePointerManagerV1,
);

fn main() {
    let connection = Connection::connect_to_env().unwrap_or_else(|e| fatal(&format!("connect: {e}")));
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut probe = Probe::default();
    queue.roundtrip(&mut probe).unwrap_or_else(|e| fatal(&format!("registry roundtrip: {e}")));

    let compositor = probe.compositor.clone().unwrap_or_else(|| fatal("no wl_compositor"));
    let shm = probe.shm.clone().unwrap_or_else(|| fatal("no wl_shm"));
    let wm_base = probe.wm_base.clone().unwrap_or_else(|| fatal("no xdg_wm_base"));
    let constraints = probe.constraints.clone().unwrap_or_else(|| fatal("no zwp_pointer_constraints_v1"));
    let relative_manager =
        probe.relative_manager.clone().unwrap_or_else(|| fatal("no zwp_relative_pointer_manager_v1"));
    // A second roundtrip for the seat's capabilities, which arrive
    // after the bind and are what create the pointer.
    queue.roundtrip(&mut probe).unwrap_or_else(|e| fatal(&format!("seat roundtrip: {e}")));
    let pointer = probe.pointer.clone().unwrap_or_else(|| fatal("no wl_pointer"));
    let _relative = relative_manager.get_relative_pointer(&pointer, &qh, ());

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("pointer-lock".to_string());
    toplevel.set_app_id("pointer-lock".to_string());
    surface.commit();
    queue.roundtrip(&mut probe).unwrap_or_else(|e| fatal(&format!("initial configure: {e}")));

    let (width, height) = (400, 300);
    let path = format!(
        "{}/chonk-pointer-lock-{}",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/dev/shm".into()),
        std::process::id()
    );
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap_or_else(|e| fatal(&format!("scratch file: {e}")));
    let _ = std::fs::remove_file(&path);
    let bytes: Vec<u8> = std::iter::repeat_n([0x40u8, 0x40, 0xC0, 0xFF], (width * height) as usize)
        .flatten()
        .collect();
    let mut writer = &file;
    writer.write_all(&bytes).unwrap_or_else(|e| fatal(&format!("fill: {e}")));
    writer.flush().unwrap_or_else(|e| fatal(&format!("flush: {e}")));
    let pool = shm.create_pool(file.as_fd(), width * height * 4, &qh, ());
    let buffer = pool.create_buffer(0, width, height, width * 4, wl_shm::Format::Argb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, width, height);
    surface.commit();
    queue.roundtrip(&mut probe).unwrap_or_else(|e| fatal(&format!("map roundtrip: {e}")));
    say("**mapped**");

    // Wait for the pointer to actually be over this surface: a lock is
    // only granted to the surface that has pointer focus.
    for _ in 0..200 {
        queue.roundtrip(&mut probe).unwrap_or_else(|e| fatal(&format!("focus roundtrip: {e}")));
        if probe.entered {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    if !probe.entered {
        say("**never entered**");
    }

    let _lock = constraints.lock_pointer(
        &surface,
        &pointer,
        None,
        zwp_pointer_constraints_v1::Lifetime::Persistent,
        &qh,
        (),
    );
    surface.commit();
    queue.roundtrip(&mut probe).unwrap_or_else(|e| fatal(&format!("lock roundtrip: {e}")));
    // The count at the moment the lock took effect. Everything after
    // this line is what the test measures.
    say(&format!("**armed motions={} relatives={}**", probe.motions, probe.relatives));

    loop {
        if queue.blocking_dispatch(&mut probe).is_err() {
            return;
        }
    }
}
