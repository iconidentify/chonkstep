//! Correlates a live wlr foreign-toplevel handle through
//! `hyprland-toplevel-mapping-v1` and prints the returned address.

use std::io::Write;

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};
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

#[derive(Default)]
struct Probe {
    foreign: Option<ZwlrForeignToplevelManagerV1>,
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
                mapping.get_window_for_toplevel_wlr(&toplevel, qh, ());
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
        _: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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

impl Dispatch<HyprlandToplevelWindowMappingHandleV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &HyprlandToplevelWindowMappingHandleV1,
        event: hyprland_toplevel_window_mapping_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            hyprland_toplevel_window_mapping_handle_v1::Event::WindowAddress {
                address_hi,
                address,
            } => {
                println!(
                    "**mapped address 0x{:x}**",
                    (u64::from(address_hi) << 32) | u64::from(address)
                );
            }
            hyprland_toplevel_window_mapping_handle_v1::Event::Failed => {
                println!("**mapping failed**")
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
    println!("**mapping ready**");
    let _ = std::io::stdout().flush();
    loop {
        if queue.blocking_dispatch(&mut probe).is_err() {
            return;
        }
    }
}
