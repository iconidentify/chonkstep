//! `hyprland-ctm-control-v1`, implemented on the same hardware gamma
//! ramps as wlr-gamma-control.
//!
//! Hyprsunset sends diagonal matrices, so each diagonal element is a
//! per-channel gain over an identity ramp. Non-diagonal matrices need a
//! real KMS CTM property and are refused explicitly; approximating them
//! would produce a different color transform than the client requested.

use std::collections::BTreeMap;
use std::sync::Mutex;

use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use crate::state::Compositor;

pub(crate) mod bindings {
    use smithay::reexports::wayland_server;
    use smithay::reexports::wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-ctm-control-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/hyprland-ctm-control-v1.xml");
}

use bindings::hyprland_ctm_control_manager_v1::{self, HyprlandCtmControlManagerV1};

#[derive(Debug)]
struct ManagerData {
    staged: Mutex<BTreeMap<usize, [f64; 3]>>,
}

pub(crate) struct CtmControl {
    _global: Option<GlobalId>,
}

pub(crate) fn init(display: &DisplayHandle, available: bool) -> CtmControl {
    if !available {
        tracing::info!("hyprland-ctm-control is not advertised: no output has a hardware gamma ramp");
        return CtmControl { _global: None };
    }
    let global = display.create_global::<Compositor, HyprlandCtmControlManagerV1, ()>(2, ());
    tracing::info!(version = 2, "hyprland-ctm-control advertised for hyprsunset");
    CtmControl { _global: Some(global) }
}

impl GlobalDispatch<HyprlandCtmControlManagerV1, ()> for Compositor {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<HyprlandCtmControlManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let resource = data_init.init(resource, ManagerData { staged: Mutex::new(BTreeMap::new()) });
        let blocked = !crate::gamma::claim_ctm_manager(&mut state.gamma, resource.id());
        // Ownership remains authoritative for every later request. The
        // blocked event is only the bind-time notification required by
        // version 2 of the protocol.
        if blocked {
            if resource.version() >= 2 {
                resource.blocked();
            }
            tracing::info!("CTM manager blocked: another color-control client owns the outputs");
        }
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::state::privileged_global_visible(&client)
    }
}

fn owns(state: &Compositor, resource: &HyprlandCtmControlManagerV1) -> bool {
    crate::gamma::ctm_manager_is(&state.gamma, &resource.id())
}

impl Dispatch<HyprlandCtmControlManagerV1, ManagerData> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &HyprlandCtmControlManagerV1,
        request: hyprland_ctm_control_manager_v1::Request,
        data: &ManagerData,
        _display_handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if !owns(state, resource) {
            return;
        }
        match request {
            hyprland_ctm_control_manager_v1::Request::SetCtmForOutput {
                output,
                mat0,
                mat1,
                mat2,
                mat3,
                mat4,
                mat5,
                mat6,
                mat7,
                mat8,
            } => {
                let matrix = [mat0, mat1, mat2, mat3, mat4, mat5, mat6, mat7, mat8];
                let non_negative = matrix.iter().all(|value| value.is_finite() && *value >= 0.0);
                let diagonal = [matrix[0], matrix[4], matrix[8]];
                let off_diagonal = [matrix[1], matrix[2], matrix[3], matrix[5], matrix[6], matrix[7]];
                if !non_negative || off_diagonal.iter().any(|value| value.abs() > f64::EPSILON) {
                    tracing::warn!(?matrix, "CTM refused: non-diagonal or negative matrix needs hardware CTM support");
                    resource.post_error(
                        hyprland_ctm_control_manager_v1::Error::InvalidMatrix,
                        "chonkstep supports diagonal non-negative CTMs; non-diagonal matrices are not approximated",
                    );
                    return;
                }
                let Some(index) = crate::gamma::output_index(state, &output) else {
                    tracing::warn!("CTM ignored for an output that does not belong to this compositor");
                    return;
                };
                data.staged.lock().unwrap().insert(index, diagonal);
            }
            hyprland_ctm_control_manager_v1::Request::Commit => {
                let scales: Vec<(usize, [f64; 3])> =
                    std::mem::take(&mut *data.staged.lock().unwrap()).into_iter().collect();
                crate::gamma::commit_ctm(state, &resource.id(), &scales);
            }
            hyprland_ctm_control_manager_v1::Request::Destroy => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &HyprlandCtmControlManagerV1, _data: &ManagerData) {
        crate::gamma::release_ctm_manager(&mut state.gamma, &resource.id());
        tracing::info!("CTM manager released; restoring original gamma ramps");
    }
}
