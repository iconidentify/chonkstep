//! Seat capabilities and focus-dependent protocol grants.
//!
//! Input routing chooses a typed target; this boundary keeps selection access,
//! text-input focus and shortcut inhibition synchronized with that choice.

use smithay::delegate_seat;
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat;
use smithay::wayland::selection::data_device::set_data_device_focus;
use smithay::wayland::selection::primary_selection::set_primary_focus;
use smithay::wayland::text_input::TextInputSeat;

use crate::state::Compositor;

impl SeatHandler for Compositor {
    // Keyboard focus preserves native/X11 delivery semantics. Pointer/touch
    // focus also carries the transform retained by an implicit grab.
    type KeyboardFocus = crate::input::keyboard::KeyboardFocus;
    type PointerFocus = crate::input::surface::SurfaceTarget;
    type TouchFocus = crate::input::surface::SurfaceTarget;

    fn seat_state(&mut self) -> &mut SeatState<Compositor> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, target: Option<&Self::KeyboardFocus>) {
        let target = target.map(crate::input::keyboard::KeyboardFocus::surface);
        // Keyboard focus carries clipboard access with it — without
        // this, paste silently targets whichever client focused first.
        // Both selections follow it, and for the same reason: the
        // protocols gate `set_selection` on the requesting client
        // holding focus, and gate the offers a client is told about on
        // the same thing, so a focus change that only moved one of them
        // leaves the other reading a stale client's clipboard.
        let display_handle = self.display_handle.clone();
        let client = target.and_then(|surface| surface.client());
        set_data_device_focus(&display_handle, seat, client.clone());
        set_primary_focus(&display_handle, seat, client);

        // Keep the seat-level text-input owner aligned too. WlSurface's
        // keyboard target independently handles the wire enter/leave events.
        seat.text_input().set_focus(target.cloned());

        // Only the focused surface may suppress compositor shortcuts.
        // Move the active grant with keyboard focus and explicitly
        // revoke the old one so a background VM cannot retain raw keys.
        if let Some(active) = self.core_protocols.active_shortcut_inhibitor.take() {
            active.inactivate();
        }
        if let Some(surface) = target {
            if let Some(inhibitor) = seat.keyboard_shortcuts_inhibitor_for_surface(surface) {
                inhibitor.activate();
                self.core_protocols.active_shortcut_inhibitor = Some(inhibitor);
            }
        }
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // The renderer composites the pointer itself (there is no
        // hardware cursor plane on the winit backend), so a client
        // changing its cursor is scene damage like any other.
        self.cursor_status = image;
        self.wm.backend_mut().mark_damaged();
    }
}

delegate_seat!(Compositor);
