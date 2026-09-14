//! `zwlr_output_power_management_v1`: DPMS without removing outputs.
//!
//! A power control is exclusive per output as the protocol requires.
//! Turning a connector off clears its DRM surface, but its Wayland
//! global, geometry, workspaces and shell placement remain intact; the
//! first frame after power-on restores scanout.
//!
//! A control names its output by identity, never by its position in
//! `Compositor::outputs`, and finds that position again for each
//! request through `Compositor::output_index_of`. A hotplug that shifts
//! the outputs therefore cannot point a DPMS client at another monitor,
//! and the controller of an output that leaves is sent `failed`.

use smithay::output::WeakOutput;
use smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_manager_v1::{
    self, ZwlrOutputPowerManagerV1,
};
use smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_v1::{
    self, Mode, ZwlrOutputPowerV1,
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

use crate::state::{Compositor, Graphics};

const VERSION: u32 = 1;

pub(crate) struct OutputPower {
    _global: Option<GlobalId>,
    owners: Owners<WeakOutput, ZwlrOutputPowerV1>,
}

/// Per-`zwlr_output_power_v1` data: the output it was created for, by
/// identity, or `None` for a `wl_output` that was never ours. Whether
/// it controls that output is [`OutputPower::owners`]'s answer, so
/// exclusivity is decided in one place.
struct PowerData {
    output: Option<WeakOutput>,
}

/// The protocol's exclusivity ledger: at most one controller per
/// output, keyed by the output's identity and never by its position.
///
/// Generic over the identity and the controller so the tests at the
/// bottom of this file can drive it without a Wayland connection, as
/// `focus_grab::Whitelist` is. In production the identity is a
/// `WeakOutput` and the controller its `zwlr_output_power_v1`.
#[derive(Debug)]
struct Owners<O, R> {
    entries: Vec<(O, R)>,
}

impl<O: PartialEq, R: PartialEq> Owners<O, R> {
    fn new() -> Self {
        Owners { entries: Vec::new() }
    }

    /// Grants `output` to `controller`, or refuses while another
    /// controller holds it.
    fn claim(&mut self, output: O, controller: R) -> bool {
        if self.owner(&output).is_some() {
            return false;
        }
        self.entries.push((output, controller));
        true
    }

    /// The controller holding `output`, if any.
    fn owner(&self, output: &O) -> Option<&R> {
        self.entries.iter().find_map(|(held, controller)| (held == output).then_some(controller))
    }

    /// Forgets `controller`'s claim. A controller that was refused
    /// holds nothing, so its destruction frees nothing.
    fn release(&mut self, controller: &R) {
        self.entries.retain(|(_, held)| held != controller);
    }

    /// Drops every claim on an output `present` no longer finds, and
    /// returns those controllers so each can be told.
    fn retain_present(&mut self, present: impl Fn(&O) -> bool) -> Vec<R> {
        let mut gone = Vec::new();
        for (output, controller) in std::mem::take(&mut self.entries) {
            if present(&output) {
                self.entries.push((output, controller));
            } else {
                gone.push(controller);
            }
        }
        gone
    }
}

pub(crate) fn init(display: &DisplayHandle, graphics: &Graphics) -> OutputPower {
    if !crate::session::has_physical_outputs(graphics) {
        tracing::info!("no physical outputs; wlr-output-power-management is not advertised");
        return OutputPower { _global: None, owners: Owners::new() };
    }
    let global = display.create_global::<Compositor, ZwlrOutputPowerManagerV1, ()>(VERSION, ());
    tracing::info!(version = VERSION, "wlr-output-power-management advertised");
    OutputPower { _global: Some(global), owners: Owners::new() }
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

/// Tells the controller of every output a connector hotplug removed
/// that its control failed, which is the protocol's event for an output
/// that disappeared, and frees the claim. Called from
/// `apply_connector_hotplug` once `Compositor::outputs` holds the new
/// set. The controller of an output that stayed keeps its claim,
/// wherever the hotplug moved that output.
pub(crate) fn outputs_changed(comp: &mut Compositor) {
    let outputs = &comp.outputs;
    let gone = comp.output_power.owners.retain_present(|output| {
        crate::state::output_index_in(outputs.iter().map(|entry| &entry.output), output).is_some()
    });
    for controller in gone {
        tracing::info!("output power control failed: its output was unplugged");
        controller.failed();
    }
}

/// `index` is resolved by the caller for this one request.
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
                comp.surface_outputs.suspend_output(&comp.outputs[index].output);
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

/// Sends `mode` to the controller of the output at `index`, found by
/// that output's identity, so an IPC `dpms` reaches the client that
/// holds the monitor it named.
fn notify_owner(comp: &Compositor, index: usize, powered: bool) {
    let Some(entry) = comp.outputs.get(index) else {
        return;
    };
    if let Some(owner) = comp.output_power.owners.owner(&entry.output.downgrade()) {
        owner.mode(if powered { Mode::On } else { Mode::Off });
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
            let output = state.output_identity(&output);
            let resource = data_init.init(id, PowerData { output: output.clone() });
            let Some((output, index)) =
                output.and_then(|output| state.output_index_of(&output).map(|index| (output, index)))
            else {
                resource.failed();
                return;
            };
            if !state.output_power.owners.claim(output, resource.clone()) {
                resource.failed();
                return;
            }
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
            // Only the controller holding its output may switch it, and
            // it switches that output wherever it now sits. A control
            // that was refused, or whose output was unplugged, is
            // answered `failed` and never acts on another monitor.
            let index = data
                .output
                .as_ref()
                .filter(|output| state.output_power.owners.owner(output) == Some(resource))
                .and_then(|output| state.output_index_of(output));
            if !index.is_some_and(|index| set(state, index, powered)) {
                resource.failed();
            }
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &ZwlrOutputPowerV1, _data: &PowerData) {
        state.output_power.owners.release(resource);
    }
}

#[cfg(test)]
mod tests {
    use smithay::output::{Output, PhysicalProperties, Subpixel};

    use super::Owners;
    use crate::state::output_index_in;

    fn output(name: &str) -> Output {
        Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        )
    }

    /// The ledger as `outputs_changed` prunes it against `outputs`.
    fn prune(owners: &mut Owners<smithay::output::WeakOutput, u32>, outputs: &[Output]) -> Vec<u32> {
        owners.retain_present(|held| output_index_in(outputs, held).is_some())
    }

    #[test]
    fn a_claim_follows_its_output_when_an_earlier_one_is_unplugged() {
        // Controller 7 holds HDMI-A-1, the third output.
        let mut outputs = vec![output("eDP-1"), output("DP-1"), output("HDMI-A-1")];
        let hdmi = outputs[2].downgrade();
        let mut owners = Owners::new();
        assert!(owners.claim(hdmi.clone(), 7));

        // DP-1 is unplugged and DP-2 plugged in at the end, so DP-2
        // takes position 2 and HDMI-A-1 moves to 1.
        outputs.remove(1);
        outputs.push(output("DP-2"));
        assert_eq!(prune(&mut owners, &outputs), Vec::<u32>::new(), "nothing anyone controlled left");

        // The claim still names HDMI-A-1, now at position 1. A
        // positional ledger would have handed controller 7 DP-2.
        assert_eq!(owners.owner(&hdmi), Some(&7));
        assert_eq!(output_index_in(&outputs, &hdmi), Some(1));
        assert_eq!(owners.owner(&outputs[2].downgrade()), None);

        // The new output can be claimed, and HDMI-A-1 stays exclusive.
        assert!(owners.claim(outputs[2].downgrade(), 8));
        assert!(!owners.claim(outputs[1].downgrade(), 9));
    }

    #[test]
    fn an_unplugged_output_fails_only_its_own_controller() {
        let mut outputs = vec![output("eDP-1"), output("DP-1"), output("HDMI-A-1")];
        let mut owners = Owners::new();
        assert!(owners.claim(outputs[1].downgrade(), 7));
        assert!(owners.claim(outputs[2].downgrade(), 8));

        outputs.remove(1);
        assert_eq!(prune(&mut owners, &outputs), vec![7], "DP-1's controller is owed `failed`");
        // HDMI-A-1 inherited DP-1's position, not its controller.
        assert_eq!(owners.owner(&outputs[1].downgrade()), Some(&8));

        // DP-1 plugged back in is a new output, free to claim.
        outputs.push(output("DP-1"));
        assert!(owners.claim(outputs[2].downgrade(), 9));
    }

    #[test]
    fn only_the_holder_releasing_frees_an_output() {
        let outputs = [output("eDP-1")];
        let panel = outputs[0].downgrade();
        let mut owners = Owners::new();
        assert!(owners.claim(panel.clone(), 7));
        assert!(!owners.claim(panel.clone(), 8), "a second controller is refused");
        // The refused controller being destroyed frees nothing.
        owners.release(&8);
        assert_eq!(owners.owner(&panel), Some(&7));
        owners.release(&7);
        assert!(owners.claim(panel, 8));
    }
}
