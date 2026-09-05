//! `zwlr_output_power_management_v1`: DPMS without removing outputs.
//!
//! A power control is exclusive per output as the protocol requires.
//! Turning a connector off clears its DRM surface, but its Wayland
//! global, geometry, workspaces and shell placement remain intact; the
//! first frame after power-on restores scanout.

use smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_manager_v1::{
    self, ZwlrOutputPowerManagerV1,
};
use smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_v1::{
    self, Mode, ZwlrOutputPowerV1,
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId, ObjectId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

use crate::state::{Compositor, Graphics};

const VERSION: u32 = 1;

pub(crate) struct OutputPower {
    _global: Option<GlobalId>,
    owners: Vec<Option<ObjectId>>,
}

struct PowerData {
    index: usize,
}

pub(crate) fn init(display: &DisplayHandle, graphics: &Graphics) -> OutputPower {
    if !crate::session::has_physical_outputs(graphics) {
        tracing::info!("no physical outputs; wlr-output-power-management is not advertised");
        return OutputPower { _global: None, owners: Vec::new() };
    }
    let global = display.create_global::<Compositor, ZwlrOutputPowerManagerV1, ()>(VERSION, ());
    tracing::info!(version = VERSION, "wlr-output-power-management advertised");
    OutputPower { _global: Some(global), owners: Vec::new() }
}

pub(crate) fn set_from_ipc(comp: &mut Compositor, name: Option<&str>, powered: bool) -> bool {
    let targets: Vec<usize> = comp
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, entry)| name.is_none_or(|name| entry.output.name() == name))
        .map(|(index, _)| index)
        .collect();
    if targets.is_empty() {
        return false;
    }
    targets.into_iter().all(|index| set(comp, index, powered))
}

/// Wake every powered-down screen on real user activity. Device
/// hotplug events do not call this; keyboard, pointer, touch, tablet
/// and switch input do.
pub(crate) fn wake_all(comp: &mut Compositor) {
    let sleeping = comp.outputs.iter().any(|entry| !entry.powered);
    if sleeping {
        let _ = set_from_ipc(comp, None, true);
    }
}

fn set(comp: &mut Compositor, index: usize, powered: bool) -> bool {
    let Some(entry) = comp.outputs.get(index) else {
        return false;
    };
    if entry.powered == powered {
        notify_owner(comp, index, powered);
        return true;
    }
    let name = entry.output.name();
    match crate::session::set_output_power(&mut comp.graphics, index, powered) {
        Ok(()) => {
            comp.outputs[index].powered = powered;
            if !powered {
                comp.outputs[index].vrr_enabled = false;
            }
            comp.wm.backend_mut().mark_damaged();
            comp.sync_monitor_outputs();
            comp.mark_hyprland_state_dirty();
            notify_owner(comp, index, powered);
            tracing::info!(output = %name, powered, "output power changed");
            true
        }
        Err(error) => {
            tracing::warn!(output = %name, %error, "output power change failed");
            false
        }
    }
}

fn notify_owner(comp: &Compositor, index: usize, powered: bool) {
    let Some(Some(owner)) = comp.output_power.owners.get(index) else {
        return;
    };
    if let Ok(resource) = ZwlrOutputPowerV1::from_id(&comp.display_handle, owner.clone()) {
        resource.mode(if powered { Mode::On } else { Mode::Off });
    }
}

impl GlobalDispatch<ZwlrOutputPowerManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputPowerManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::state::privileged_global_visible(&client)
    }
}

impl Dispatch<ZwlrOutputPowerManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwlrOutputPowerManagerV1,
        request: zwlr_output_power_manager_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_output_power_manager_v1::Request::GetOutputPower { id, output } = request {
            let index = crate::gamma::output_index(state, &output);
            let resource = data_init.init(id, PowerData { index: index.unwrap_or(usize::MAX) });
            let Some(index) = index else {
                resource.failed();
                return;
            };
            state.output_power.owners.resize_with(state.outputs.len(), || None);
            if state.output_power.owners[index].is_some() {
                resource.failed();
                return;
            }
            state.output_power.owners[index] = Some(resource.id());
            resource.mode(if state.outputs[index].powered { Mode::On } else { Mode::Off });
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, PowerData> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputPowerV1,
        request: zwlr_output_power_v1::Request,
        data: &PowerData,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_output_power_v1::Request::SetMode { mode } = request {
            let powered = match mode {
                WEnum::Value(Mode::On) => true,
                WEnum::Value(Mode::Off) => false,
                WEnum::Unknown(_) => {
                    resource.post_error(zwlr_output_power_v1::Error::InvalidMode, "unknown output power mode");
                    return;
                }
                _ => return,
            };
            if !set(state, data.index, powered) {
                resource.failed();
            }
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &ZwlrOutputPowerV1, data: &PowerData) {
        if state.output_power.owners.get(data.index).and_then(Option::as_ref) == Some(&resource.id()) {
            state.output_power.owners[data.index] = None;
        }
    }
}
