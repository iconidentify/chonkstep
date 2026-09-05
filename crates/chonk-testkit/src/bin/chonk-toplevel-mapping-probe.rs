//! Correlates a live foreign-toplevel handle through
//! `hyprland-toplevel-mapping-v1` and prints the returned address —
//! once for the wlr manager's handle and once for the frozen
//! `ext_foreign_toplevel_list_v1` handle naming the same window.
//!
//! The protocol has two requests and both are a *join*: whichever kind
//! of handle a caller holds, the address that comes back has to be the
//! one `hyprctl clients -j` prints. The ext arm answered `failed`
//! unconditionally until the list was served, so the probe labels each
//! answer with the arm that produced it.

use std::io::Write;

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

mod mapping_bindings {
    // The scanner expands references through `super::wayland_client`;
    // this apparently redundant import is part of its generated API.
    #[allow(clippy::single_component_path_imports)]
    use wayland_client;
    use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1;
    use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1;

    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_protocols::ext::foreign_toplevel_list::v1::client::__interfaces::*;
        use wayland_protocols_wlr::foreign_toplevel::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!(
            "../wm-wayland/protocols/hyprland-toplevel-mapping-v1.xml"
        );
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!(
        "../wm-wayland/protocols/hyprland-toplevel-mapping-v1.xml"
    );
}

use mapping_bindings::hyprland_toplevel_mapping_manager_v1::HyprlandToplevelMappingManagerV1;
use mapping_bindings::hyprland_toplevel_window_mapping_handle_v1::{
    self, HyprlandToplevelWindowMappingHandleV1,
};

/// Which of the mapping protocol's two requests a pending answer came
/// from. Carried as the response object's user data, because the two
/// arms are only interesting when compared.
#[derive(Clone, Copy)]
enum Origin {
    Wlr,
    Ext,
}

impl Origin {
    fn name(self) -> &'static str {
        match self {
            Origin::Wlr => "wlr",
            Origin::Ext => "ext",
        }
    }
}

#[derive(Default)]
struct Probe {
    foreign: Option<ZwlrForeignToplevelManagerV1>,
    /// The frozen list beside the wlr manager. Both name the same
    /// windows; the point of the probe is that both resolve to one
    /// address through `hyprland-toplevel-mapping-v1`.
    ext_list: Option<ExtForeignToplevelListV1>,
    mapping: Option<HyprlandToplevelMappingManagerV1>,
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
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwlr_foreign_toplevel_manager_v1" => {
                    probe.foreign = Some(registry.bind(name, version.min(3), qh, ()))
                }
                "ext_foreign_toplevel_list_v1" => {
                    probe.ext_list = Some(registry.bind(name, version.min(1), qh, ()))
                }
                "hyprland_toplevel_mapping_manager_v1" => {
                    probe.mapping = Some(registry.bind(name, version.min(1), qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            if let Some(mapping) = &probe.mapping {
                mapping.get_window_for_toplevel_wlr(&toplevel, qh, Origin::Wlr);
            }
        }
    }

    wayland_client::event_created_child!(Probe, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, zwlr_foreign_toplevel_handle_v1::Event::Done) {
            println!("**foreign done**");
            let _ = std::io::stdout().flush();
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            if let Some(mapping) = &probe.mapping {
                mapping.get_window_for_toplevel(&toplevel, qh, Origin::Ext);
            }
        }
    }

    wayland_client::event_created_child!(Probe, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, ext_foreign_toplevel_handle_v1::Event::Done) {
            println!("**ext done**");
            let _ = std::io::stdout().flush();
        }
    }
}

impl Dispatch<HyprlandToplevelMappingManagerV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &HyprlandToplevelMappingManagerV1,
        _: <HyprlandToplevelMappingManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<HyprlandToplevelWindowMappingHandleV1, Origin> for Probe {
    fn event(
        _: &mut Self,
        _: &HyprlandToplevelWindowMappingHandleV1,
        event: hyprland_toplevel_window_mapping_handle_v1::Event,
        origin: &Origin,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            hyprland_toplevel_window_mapping_handle_v1::Event::WindowAddress {
                address_hi,
                address,
            } => {
                let address = (u64::from(address_hi) << 32) | u64::from(address);
                // The unlabelled line is kept verbatim: an older test
                // matches on it, and the wlr arm is what it watches.
                if matches!(origin, Origin::Wlr) {
                    println!("**mapped address 0x{address:x}**");
                }
                println!("**{} address 0x{address:x}**", origin.name());
            }
            hyprland_toplevel_window_mapping_handle_v1::Event::Failed => {
                println!("**{} mapping failed**", origin.name());
                if matches!(origin, Origin::Wlr) {
                    println!("**mapping failed**");
                }
            }
        }
        let _ = std::io::stdout().flush();
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
    if probe.foreign.is_none() || probe.mapping.is_none() {
        println!("**required global missing**");
        return;
    }
    if probe.ext_list.is_none() {
        // Named separately so "the compositor does not serve the frozen
        // list" and "the probe could not start" are different failures.
        println!("**ext list missing**");
        return;
    }
    println!("**mapping ready**");
    let _ = std::io::stdout().flush();
    loop {
        if queue.blocking_dispatch(&mut probe).is_err() {
            return;
        }
    }
}
