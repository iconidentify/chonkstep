//! Scripted `hyprland-focus-grab-v1` client for lifecycle E2E tests.
//!
//! Quickshell creates many inert grab objects and activates one only
//! while a popup is open. This probe deliberately has that shape: the
//! live object sits after a batch of inert objects, half that prefix is
//! destroyed, a successor supersedes it, and the successor's surface
//! is destroyed. The events printed at the end prove both cached-index
//! maintenance and the protocol's implicit `remove_surface` rule over
//! the real Wayland wire.

use std::io::Write;

use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

mod focus_grab_bindings {
    // The scanner's expansion names `super::wayland_client` and core
    // protocol types directly; keep these imports beside the XML that
    // needs them, as the other generated probes do.
    #[allow(clippy::single_component_path_imports)]
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../wm-wayland/protocols/hyprland-focus-grab-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("../wm-wayland/protocols/hyprland-focus-grab-v1.xml");
}

use focus_grab_bindings::hyprland_focus_grab_manager_v1::HyprlandFocusGrabManagerV1;
use focus_grab_bindings::hyprland_focus_grab_v1::{self, HyprlandFocusGrabV1};

const FIRST_ACTIVE: u32 = 1_000;
const SUCCESSOR: u32 = 1_001;
const INERT_GRABS: u32 = 128;

#[derive(Default)]
struct Probe {
    compositor: Option<wl_compositor::WlCompositor>,
    manager: Option<HyprlandFocusGrabManagerV1>,
    cleared: Vec<u32>,
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
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => probe.compositor = Some(registry.bind(name, version.min(6), qh, ())),
            "hyprland_focus_grab_manager_v1" => {
                probe.manager = Some(registry.bind(name, version.min(1), qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: <wl_compositor::WlCompositor as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: <wl_surface::WlSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<HyprlandFocusGrabManagerV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &HyprlandFocusGrabManagerV1,
        _: <HyprlandFocusGrabManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<HyprlandFocusGrabV1, u32> for Probe {
    fn event(
        probe: &mut Self,
        _: &HyprlandFocusGrabV1,
        event: hyprland_focus_grab_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, hyprland_focus_grab_v1::Event::Cleared) {
            probe.cleared.push(*id);
        }
    }
}

fn main() {
    let connection = Connection::connect_to_env().expect("connect to compositor");
    let display = connection.display();
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    display.get_registry(&qh, ());
    let mut probe = Probe::default();
    queue.roundtrip(&mut probe).expect("registry roundtrip");

    let manager = probe.manager.clone().expect("focus-grab global");
    let compositor = probe.compositor.clone().expect("wl_compositor global");

    // Put the first active object after a sizeable inert prefix, then
    // erase alternating prefix entries. The server's cached index must
    // follow every `Vec::remove`, or the supersession below clears the
    // wrong object (or none).
    let mut inert: Vec<_> = (0..INERT_GRABS)
        .map(|id| manager.create_grab(&qh, id))
        .collect();
    let first_surface = compositor.create_surface(&qh, ());
    let first = manager.create_grab(&qh, FIRST_ACTIVE);
    first.add_surface(&first_surface);
    first.commit();
    queue.roundtrip(&mut probe).expect("first grab commit");

    for grab in inert.drain(..INERT_GRABS as usize / 2) {
        grab.destroy();
    }
    queue.roundtrip(&mut probe).expect("inert grab destruction");

    let successor_surface = compositor.create_surface(&qh, ());
    let successor = manager.create_grab(&qh, SUCCESSOR);
    successor.add_surface(&successor_surface);
    successor.commit();
    queue.roundtrip(&mut probe).expect("successor grab commit");

    successor_surface.destroy();
    queue
        .roundtrip(&mut probe)
        .expect("whitelisted surface destruction");
    // The destruction callback clears staged and committed state. A
    // later commit must not resurrect the grab or emit a second event.
    successor.commit();
    queue
        .roundtrip(&mut probe)
        .expect("post-destruction commit");

    let first_count = probe
        .cleared
        .iter()
        .filter(|id| **id == FIRST_ACTIVE)
        .count();
    let successor_count = probe.cleared.iter().filter(|id| **id == SUCCESSOR).count();
    println!("**first cleared {first_count}; successor cleared {successor_count}**");
    let _ = std::io::stdout().flush();
}
