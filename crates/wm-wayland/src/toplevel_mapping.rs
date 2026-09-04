//! `hyprland-toplevel-mapping-v1`: joins a foreign-toplevel handle to
//! the same opaque address exposed by `hyprctl clients -j`.

use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New};

use crate::state::Compositor;

pub(crate) mod bindings {
    use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1;
    use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1;
    use smithay::reexports::wayland_server;

    pub mod __interfaces {
        use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::__interfaces::*;
        use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::__interfaces::*;
        use smithay::reexports::wayland_server::backend as wayland_backend;
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-mapping-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/hyprland-toplevel-mapping-v1.xml");
}

use bindings::hyprland_toplevel_mapping_manager_v1::{self, HyprlandToplevelMappingManagerV1};
use bindings::hyprland_toplevel_window_mapping_handle_v1::{self, HyprlandToplevelWindowMappingHandleV1};

pub(crate) struct ToplevelMapping {
    _global: GlobalId,
}

pub(crate) fn init(display: &DisplayHandle) -> ToplevelMapping {
    let global = display.create_global::<Compositor, HyprlandToplevelMappingManagerV1, ()>(1, ());
    tracing::info!(version = 1, "hyprland-toplevel-mapping advertised");
    ToplevelMapping { _global: global }
}

impl GlobalDispatch<HyprlandToplevelMappingManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<HyprlandToplevelMappingManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<HyprlandToplevelMappingManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &HyprlandToplevelMappingManagerV1,
        request: hyprland_toplevel_mapping_manager_v1::Request,
        _data: &(),
        _display_handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            hyprland_toplevel_mapping_manager_v1::Request::GetWindowForToplevelWlr { handle, toplevel } => {
                let response = data_init.init(handle, ());
                let address = crate::protocols::window_for_wlr_toplevel(&state.protocols, &toplevel)
                    .and_then(|window| state.wm.client_for_window(window))
                    .map(wm_core::ClientId::as_u64);
                send_address(&response, address);
            }
            // Chonkstep currently advertises the stateful wlr manager,
            // not ext-foreign-toplevel-list. The request remains in the
            // upstream protocol and is answered honestly if a client
            // imports such an object through another future global.
            hyprland_toplevel_mapping_manager_v1::Request::GetWindowForToplevel { handle, .. } => {
                let response = data_init.init(handle, ());
                response.failed();
            }
            hyprland_toplevel_mapping_manager_v1::Request::Destroy => {}
        }
    }
}

fn send_address(handle: &HyprlandToplevelWindowMappingHandleV1, address: Option<u64>) {
    if let Some(address) = address {
        handle.window_address((address >> 32) as u32, address as u32);
    } else {
        handle.failed();
    }
}

impl Dispatch<HyprlandToplevelWindowMappingHandleV1, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &HyprlandToplevelWindowMappingHandleV1,
        request: hyprland_toplevel_window_mapping_handle_v1::Request,
        _data: &(),
        _display_handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            hyprland_toplevel_window_mapping_handle_v1::Request::Destroy => {}
        }
    }
}
