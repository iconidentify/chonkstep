//! The clipboard-manager protocols: `wlr-data-control-unstable-v1` and
//! `ext-data-control-v1`.
//!
//! `wl_data_device` hands the clipboard to whoever has keyboard focus,
//! which is exactly right for an application pasting into itself and
//! useless for a clipboard *manager* — a program whose entire job is to
//! watch every copy that happens while it is not focused, and to hand
//! an old one back later. Data control is the protocol that lets a
//! client read and write the selections without focus, and it is the
//! only one; without it `wl-paste --watch` refuses to start at all:
//!
//! ```text
//! Watch mode requires a compositor that supports the data-control protocol
//! ```
//!
//! That single refusal is what stops clipboard history working on this
//! desktop. `cliphist` and `clipman` are built on it end to end,
//! `wl-clip-persist` uses it to keep a copied selection alive after the
//! application that made it exits, and the Omarchy scripts that put a
//! screenshot, a scanned QR code, or an emoji *on the clipboard* all go
//! through `wl-copy`, which reaches for data control the moment it is
//! asked to survive its own exit.
//!
//! # Why both interfaces, and not just one
//!
//! They are the same protocol twice: `ext-data-control-v1` is the
//! standardised rewrite of wlroots' original, near-identical down to
//! the request names. Which one a client asks for is purely a question
//! of when it was written.
//!
//! - `wl-clipboard` (2.2 and later) prefers `ext_data_control_manager_v1`
//!   and falls back to the wlr name, so it is happy with either.
//! - `cliphist`, `clipman` and `wl-clip-persist` bind the wlr name and
//!   nothing else. To them a compositor serving only `ext-` has no
//!   clipboard history at all.
//!
//! So serving one of the two serves about half the ecosystem, and
//! choosing which half is not an interesting decision to make. Both
//! globals are advertised, they share the seat's selection state, and a
//! manager on either interface sees the same clipboard. Sway, Hyprland
//! and KWin all do the same.
//!
//! # Both clipboards, which is a wiring decision and not a default
//!
//! A clipboard manager watches two selections: the ordinary clipboard
//! and PRIMARY, the middle-click one. Smithay routes both through the
//! seat's shared `SeatData`, and a data-control device is registered
//! there beside the `wl_data_device` and the
//! `zwp_primary_selection_device_v1` — so interoperation with the two
//! selection protocols `state.rs` already advertises is automatic once
//! the devices are in the same list.
//!
//! With one exception, which is the reason [`init`] takes the primary
//! selection state by reference rather than leaving it out: smithay's
//! constructors accept `Option<&PrimarySelectionState>`, and passing
//! `None` compiles, runs, and silently produces a compositor where
//! `zwlr_data_control_device_v1.set_primary_selection` is *ignored* and
//! no primary offer is ever advertised. That failure looks like a
//! working clipboard manager with a middle-click selection that never
//! appears in its history. Requiring the argument here is what makes
//! that mistake unrepresentable.
//!
//! # XWayland comes along for free
//!
//! A data-control client setting the selection goes through the same
//! `SelectionHandler::new_selection` callback in `xdg.rs` that a
//! `wl_data_device` client does, so restoring an entry from clipboard
//! history claims CLIPBOARD on the X server exactly as an ordinary copy
//! would, and pasting it into xterm works. Nothing in this module has
//! to know that; it is a property of smithay dispatching every
//! selection provider through one handler.
//!
//! # Shared security-context gate
//!
//! Data control is a read-anything-on-the-clipboard capability, so both
//! globals consult `state::privileged_global_visible`. Security-context
//! clients are now tagged and can be distinguished there. The current
//! single-user policy remains permissive, where a clipboard manager, a
//! terminal and a browser are all equally that user's own programs.

use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::selection::ext_data_control::{
    DataControlHandler as ExtDataControlHandler, DataControlState as ExtDataControlState,
};
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::selection::wlr_data_control::{
    DataControlHandler as WlrDataControlHandler, DataControlState as WlrDataControlState,
};
use smithay::{delegate_data_control, delegate_ext_data_control};

use crate::state::Compositor;

/// The two data-control globals on [`Compositor`].
///
/// Both fields are live state rather than parked `GlobalId`s: smithay's
/// handler traits ask for the `DataControlState` back on every request
/// (that is how a device finds the primary-selection filter it was
/// created with), so neither can be dropped after registration the way
/// `protocols.rs` drops its ids.
pub(crate) struct DataControl {
    /// `zwlr_data_control_manager_v1`, version 2 — the version that has
    /// the primary-selection half of the protocol. wlroots-era clients
    /// bind this and only this.
    pub wlr: WlrDataControlState,
    /// `ext_data_control_manager_v1`, version 1 — the standardised
    /// spelling `wl-clipboard` looks for first.
    pub ext: ExtDataControlState,
}

/// Registers both globals. Called once from `run`, before the listening
/// socket exists, under the same timing rule as every other global
/// there: a clipboard manager that connects and finds neither interface
/// does not retry, it exits with the message at the top of this module.
///
/// `primary` is not optional on purpose — see the module docs for the
/// silent half-working session that `None` produces.
pub(crate) fn init(display_handle: &DisplayHandle, primary: &PrimarySelectionState) -> DataControl {
    let wlr = WlrDataControlState::new::<Compositor, _>(
        display_handle,
        Some(primary),
        crate::state::privileged_global_visible,
    );
    let ext = ExtDataControlState::new::<Compositor, _>(
        display_handle,
        Some(primary),
        crate::state::privileged_global_visible,
    );
    tracing::info!("data-control advertised on both the wlr and ext interfaces");
    DataControl { wlr, ext }
}

impl WlrDataControlHandler for Compositor {
    fn data_control_state(&self) -> &WlrDataControlState {
        &self.data_control.wlr
    }
}

impl ExtDataControlHandler for Compositor {
    fn data_control_state(&self) -> &ExtDataControlState {
        &self.data_control.ext
    }
}

// The selection callbacks these two need — `SelectionHandler`, and the
// `wl_data_device`/primary-selection state they share a seat with —
// already exist in `xdg.rs`. Data control adds no handler of its own.
delegate_data_control!(Compositor);
delegate_ext_data_control!(Compositor);

#[cfg(test)]
mod tests {
    use super::*;

    use smithay::reexports::wayland_server::Display;

    /// A display with no socket and no client on it, which is all the
    /// registry needs to answer what a client *would* be offered.
    fn globals() -> (Display<Compositor>, DataControl) {
        let display = Display::<Compositor>::new().expect("a display with no socket");
        let handle = display.handle();
        // The real ordering too: data control borrows the primary
        // selection state, so that global exists first in `run` as
        // well.
        let primary = PrimarySelectionState::new::<Compositor>(&handle);
        let data_control = init(&handle, &primary);
        (display, data_control)
    }

    /// The interface names are the whole feature. A clipboard manager
    /// does not negotiate: it looks for one exact string in the
    /// registry and exits if it is missing, so a rename or a dropped
    /// global is indistinguishable from having never implemented this
    /// at all.
    #[test]
    fn both_data_control_interfaces_are_advertised() {
        let (display, data_control) = globals();
        let backend = display.handle().backend_handle();

        let wlr = backend.global_info(data_control.wlr.global()).expect("the wlr global");
        assert_eq!(wlr.interface.name, "zwlr_data_control_manager_v1");
        assert!(!wlr.disabled);

        let ext = backend.global_info(data_control.ext.global()).expect("the ext global");
        assert_eq!(ext.interface.name, "ext_data_control_manager_v1");
        assert!(!ext.disabled);
    }

    /// Version 2 of the wlr interface is where
    /// `zwlr_data_control_device_v1.primary_selection` lives (smithay
    /// spells the guard `EVT_PRIMARY_SELECTION_SINCE`). Advertising
    /// version 1 would leave every middle-click selection out of a
    /// clipboard manager's history while everything else kept working.
    #[test]
    fn the_wlr_interface_is_advertised_at_the_primary_selection_version() {
        let (display, data_control) = globals();
        let info = display
            .handle()
            .backend_handle()
            .global_info(data_control.wlr.global())
            .expect("the wlr global");
        assert!(
            info.version >= 2,
            "version {} predates the primary-selection event",
            info.version
        );
    }

    /// Two distinct globals, not one registered twice. Both
    /// constructors are named `DataControlState::new` and take the same
    /// arguments, which makes a copy-paste that registers the wlr
    /// manager twice both plausible and invisible — the session would
    /// come up, `wl-paste` would work, and `cliphist` would still see
    /// nothing.
    #[test]
    fn the_two_interfaces_are_separate_globals() {
        let (display, data_control) = globals();
        let backend = display.handle().backend_handle();
        let wlr = backend.global_info(data_control.wlr.global()).expect("the wlr global");
        let ext = backend.global_info(data_control.ext.global()).expect("the ext global");
        assert_ne!(wlr.interface.name, ext.interface.name);
    }
}
