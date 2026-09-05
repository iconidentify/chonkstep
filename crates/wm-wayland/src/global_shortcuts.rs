//! Hyprland's global-shortcuts protocol, used by
//! xdg-desktop-portal-hyprland to turn portal registrations into
//! compositor-owned shortcut objects.
//!
//! The client deliberately never learns the key chord. Hyprland config
//! binds a chord to `global, app:id`; the shell resolves that action
//! after all ordinary session bindings, and this registry sends the
//! matching object's pressed/released event. Duplicate registrations
//! and a bounded per-client population are rejected at registration.

use smithay::reexports::wayland_server::{
    backend::{ClientId, GlobalId},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::Compositor;

#[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
#[allow(dead_code, unused_imports, unused_variables, unused_unsafe)]
#[allow(missing_docs, clippy::all)]
pub(crate) mod bindings {
    use smithay::reexports::wayland_server;
    use smithay::reexports::wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-global-shortcuts-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocols/hyprland-global-shortcuts-v1.xml");
}

use bindings::hyprland_global_shortcut_v1::{self, HyprlandGlobalShortcutV1};
use bindings::hyprland_global_shortcuts_manager_v1::{self, HyprlandGlobalShortcutsManagerV1};

const VERSION: u32 = 1;
const MAX_SHORTCUTS_PER_CLIENT: usize = 128;

pub(crate) struct GlobalShortcuts {
    _global: GlobalId,
    entries: Vec<Entry>,
}

struct Entry {
    resource: HyprlandGlobalShortcutV1,
    client: ClientId,
    app_id: String,
    id: String,
}

impl GlobalShortcuts {
    /// Fires a configured `app:id` target when it is currently
    /// registered. A miss is intentionally a no-op: configurations may
    /// outlive applications and a portal session may come and go.
    pub(crate) fn trigger(
        &mut self,
        target: &str,
        pressed: bool,
        elapsed: std::time::Duration,
    ) -> bool {
        let Some((app_id, id)) = target.split_once(':') else {
            return false;
        };
        self.entries.retain(|entry| entry.resource.is_alive());
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.app_id == app_id && entry.id == id)
        else {
            return false;
        };
        let seconds = elapsed.as_secs();
        let hi = (seconds >> 32) as u32;
        let lo = seconds as u32;
        if pressed {
            entry.resource.pressed(hi, lo, elapsed.subsec_nanos());
        } else {
            entry.resource.released(hi, lo, elapsed.subsec_nanos());
        }
        true
    }
}

pub(crate) fn init(display_handle: &DisplayHandle) -> GlobalShortcuts {
    let global = display_handle
        .create_global::<Compositor, HyprlandGlobalShortcutsManagerV1, ()>(VERSION, ());
    tracing::info!(version = VERSION, "hyprland-global-shortcuts advertised");
    GlobalShortcuts {
        _global: global,
        entries: Vec::new(),
    }
}

impl GlobalDispatch<HyprlandGlobalShortcutsManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<HyprlandGlobalShortcutsManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::state::privileged_global_visible(&client)
    }
}

impl Dispatch<HyprlandGlobalShortcutsManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &HyprlandGlobalShortcutsManagerV1,
        request: hyprland_global_shortcuts_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use hyprland_global_shortcuts_manager_v1::{Error, Request};
        match request {
            Request::RegisterShortcut {
                shortcut,
                id,
                app_id,
                description: _,
                trigger_description: _,
            } => {
                state
                    .global_shortcuts
                    .entries
                    .retain(|entry| entry.resource.is_alive());
                if state
                    .global_shortcuts
                    .entries
                    .iter()
                    .any(|entry| entry.app_id == app_id && entry.id == id)
                {
                    resource.post_error(
                        Error::AlreadyTaken,
                        "this app_id + id is already registered",
                    );
                    return;
                }
                let client_id = client.id();
                if state
                    .global_shortcuts
                    .entries
                    .iter()
                    .filter(|entry| entry.client == client_id)
                    .count()
                    >= MAX_SHORTCUTS_PER_CLIENT
                {
                    resource.post_error(
                        Error::AlreadyTaken,
                        "global shortcut registration ceiling reached",
                    );
                    return;
                }
                let resource = data_init.init(shortcut, ());
                state.global_shortcuts.entries.push(Entry {
                    resource,
                    client: client_id,
                    app_id,
                    id,
                });
            }
            Request::Destroy => {}
        }
    }
}

impl Dispatch<HyprlandGlobalShortcutV1, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &HyprlandGlobalShortcutV1,
        request: hyprland_global_shortcut_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let _ = request;
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &HyprlandGlobalShortcutV1,
        _data: &(),
    ) {
        state
            .global_shortcuts
            .entries
            .retain(|entry| &entry.resource != resource);
    }
}
