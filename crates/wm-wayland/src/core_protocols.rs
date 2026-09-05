//! Small, standard Wayland globals ordinary desktop clients expect.
//!
//! Smithay owns the wire implementations. This module keeps their
//! lifecycle and the handful of compositor policy callbacks together,
//! rather than scattering one-field protocol states through `state.rs`.

use std::time::{Duration, Instant};

use smithay::input::pointer::PointerHandle;
use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::{
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::ZwpInputMethodV2,
    zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{backend::ClientId, Client, DataInit, Dispatch, DisplayHandle, Resource};
use smithay::utils::{Logical, Point as LogicalPoint, Rectangle};
use smithay::wayland::input_method::{
    InputMethodHandler, InputMethodKeyboardUserData, InputMethodManagerGlobalData, InputMethodManagerState,
    InputMethodPopupSurfaceUserData, InputMethodUserData, PopupSurface,
};
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
};
use smithay::wayland::pointer_constraints::{
    with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState,
};
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState};
use smithay::{
    delegate_commit_timing, delegate_cursor_shape, delegate_fifo, delegate_keyboard_shortcuts_inhibit,
    delegate_pointer_constraints, delegate_pointer_gestures, delegate_presentation, delegate_relative_pointer,
    delegate_single_pixel_buffer, delegate_text_input_manager, delegate_xdg_activation, delegate_xdg_dialog,
    delegate_security_context, delegate_tablet_manager, delegate_xdg_foreign, delegate_xdg_system_bell, delegate_xdg_toplevel_tag,
};

use wm_core::BackendEvent;

use crate::state::Compositor;

impl smithay::wayland::security_context::SecurityContextHandler for Compositor {
    fn context_created(
        &mut self,
        source: smithay::wayland::security_context::SecurityContextListenerSource,
        context: smithay::wayland::security_context::SecurityContext,
    ) {
        let mut display = self.display_handle.clone();
        if let Err(error) = self.loop_handle.insert_source(source, move |stream, _, _comp| {
            if let Err(error) = display.insert_client(
                stream,
                std::sync::Arc::new(crate::state::ClientState::confined(context.clone())),
            ) {
                tracing::warn!(?error, "failed to admit a security-context client");
            }
        }) {
            tracing::warn!(?error, "failed to register a security-context listener");
        }
    }
}

/// Activation tokens are launch hand-offs, not session-long capabilities.
/// Five minutes leaves ample room for a cold application start without
/// retaining a client that requested tokens and then disappeared forever.
const ACTIVATION_TOKEN_TTL: Duration = Duration::from_secs(5 * 60);
const ACTIVATION_TOKEN_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const MAX_ACTIVATION_TOKENS_PER_CLIENT: usize = 256;
/// The per-client ceiling prevents one connection from growing the pool;
/// this second ceiling also covers an attacker cycling connections.
const MAX_ACTIVATION_TOKENS_GLOBAL: usize = 4_096;
/// IME popup surfaces participate in every render and pointer hit-test.
/// One input method normally owns one popup; these ceilings preserve
/// headroom for hand-offs while bounding hostile protocol-object churn.
const MAX_IME_POPUPS_PER_CLIENT: usize = 16;
const MAX_IME_POPUPS_GLOBAL: usize = 256;

impl smithay::wayland::tablet_manager::TabletSeatHandler for Compositor {}

/// `zwp_xwayland_keyboard_grab_v1`: an XWayland client asking to keep
/// every key, because the X client behind it called `XGrabKeyboard`.
///
/// The default `grab` body is what we want — install smithay's grab on
/// the seat — but it is overridden here for one reason: the compositor
/// needs to *know* an XWayland grab is live, and there is no way to ask
/// the seat which kind of grab it is holding. `keyboard_grab_active` is
/// that answer, and the binding gate in `input.rs` reads it.
///
/// Deliberately not `KeyboardHandle::is_grabbed`, which is the obvious
/// spelling and is wrong: smithay's own input-method installs a
/// keyboard grab (`input_method_handle.rs`'s `set_grab`), so gating on
/// `is_grabbed` would silently disable every compositor keybinding for
/// as long as an IME popup was up.
impl smithay::wayland::xwayland_keyboard_grab::XWaylandKeyboardGrabHandler for Compositor {
    fn grab(
        &mut self,
        surface: WlSurface,
        seat: smithay::input::Seat<Self>,
        grab: smithay::wayland::xwayland_keyboard_grab::XWaylandKeyboardGrab<Self>,
    ) {
        let Some(keyboard) = seat.get_keyboard() else {
            return;
        };
        let resource = grab.grab().clone();
        keyboard.set_grab(self, grab, smithay::utils::SERIAL_COUNTER.next_serial());
        self.wm.backend_mut().xwayland_keyboard_grab = Some(resource);
        tracing::info!(surface = ?surface.id(), "an XWayland client took the keyboard grab; its combos stop reaching the desktop");
    }

    /// Which surface the grab focuses. Only a surface this compositor is
    /// actually managing as an X11 window qualifies; returning `None`
    /// for anything else means no grab is created, which is the honest
    /// answer for a surface that has no X window behind it.
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<Self::KeyboardFocus> {
        self.wm.backend().window_for_surface(surface).map(|_| surface.clone())
    }
}

smithay::delegate_xwayland_keyboard_grab!(Compositor);

/// State retained for the globals whose helpers need a getter or whose
/// `GlobalId` lifetime is tied to the state value.
pub(crate) struct CoreProtocols {
    pub activation: XdgActivationState,
    next_activation_token_sweep: Instant,
    rejected_activation_tokens: u64,
    rejected_ime_popups: u64,
    pub xdg_foreign: XdgForeignState,
    pub shortcuts: KeyboardShortcutsInhibitState,
    pub active_shortcut_inhibitor: Option<KeyboardShortcutsInhibitor>,
    pub _cursor_shape: smithay::wayland::cursor_shape::CursorShapeManagerState,
    pub _single_pixel: smithay::wayland::single_pixel_buffer::SinglePixelBufferState,
    pub _presentation: smithay::wayland::presentation::PresentationState,
    pub _fifo: smithay::wayland::fifo::FifoManagerState,
    pub _commit_timing: smithay::wayland::commit_timing::CommitTimingManagerState,
    pub _security_context: smithay::wayland::security_context::SecurityContextState,
    pub _relative_pointer: smithay::wayland::relative_pointer::RelativePointerManagerState,
    pub _pointer_constraints: PointerConstraintsState,
    pub _pointer_gestures: smithay::wayland::pointer_gestures::PointerGesturesState,
    pub _tablet: smithay::wayland::tablet_manager::TabletManagerState,
    pub _text_input: smithay::wayland::text_input::TextInputManagerState,
    pub _input_method: InputMethodManagerState,
    pub _xdg_dialog: smithay::wayland::shell::xdg::dialog::XdgDialogState,
    pub _system_bell: smithay::wayland::xdg_system_bell::XdgSystemBellState,
    pub _toplevel_tag: smithay::wayland::xdg_toplevel_tag::XdgToplevelTagManager,
    pub _xwayland_keyboard_grab: smithay::wayland::xwayland_keyboard_grab::XWaylandKeyboardGrabState,
}

pub(crate) fn init(display: &DisplayHandle) -> CoreProtocols {
    CoreProtocols {
        activation: XdgActivationState::new::<Compositor>(display),
        next_activation_token_sweep: Instant::now() + ACTIVATION_TOKEN_SWEEP_INTERVAL,
        rejected_activation_tokens: 0,
        rejected_ime_popups: 0,
        xdg_foreign: XdgForeignState::new::<Compositor>(display),
        shortcuts: KeyboardShortcutsInhibitState::new::<Compositor>(display),
        active_shortcut_inhibitor: None,
        _cursor_shape: smithay::wayland::cursor_shape::CursorShapeManagerState::new::<Compositor>(display),
        _single_pixel: smithay::wayland::single_pixel_buffer::SinglePixelBufferState::new::<Compositor>(display),
        // Linux CLOCK_MONOTONIC. Presentation timestamps emitted by
        // the renderer use the same monotonic time base.
        _presentation: smithay::wayland::presentation::PresentationState::new::<Compositor>(display, 1),
        _fifo: smithay::wayland::fifo::FifoManagerState::new::<Compositor>(display),
        _commit_timing: smithay::wayland::commit_timing::CommitTimingManagerState::new::<Compositor>(display),
        _security_context: smithay::wayland::security_context::SecurityContextState::new::<Compositor, _>(
            display,
            crate::state::security_context_global_visible,
        ),
        _relative_pointer: smithay::wayland::relative_pointer::RelativePointerManagerState::new::<Compositor>(display),
        _pointer_constraints: PointerConstraintsState::new::<Compositor>(display),
        _pointer_gestures: smithay::wayland::pointer_gestures::PointerGesturesState::new::<Compositor>(display),
        _tablet: smithay::wayland::tablet_manager::TabletManagerState::new::<Compositor>(display),
        _text_input: smithay::wayland::text_input::TextInputManagerState::new::<Compositor>(display),
        _input_method: InputMethodManagerState::new::<Compositor, _>(
            display,
            crate::state::privileged_global_visible,
        ),
        _xdg_dialog: smithay::wayland::shell::xdg::dialog::XdgDialogState::new::<Compositor>(display),
        _system_bell: smithay::wayland::xdg_system_bell::XdgSystemBellState::new::<Compositor>(display),
        _toplevel_tag: smithay::wayland::xdg_toplevel_tag::XdgToplevelTagManager::new::<Compositor>(display),
        // The protocol an XWayland client's own `XGrabKeyboard` arrives
        // through. Without it a client that grabbed the keyboard from
        // the X server still lost every bound combo to the compositor —
        // a remote-desktop viewer or a VM console could not send its
        // guest the very combos the host binds.
        _xwayland_keyboard_grab: smithay::wayland::xwayland_keyboard_grab::XWaylandKeyboardGrabState::new::<
            Compositor,
        >(display),
    }
}

impl CoreProtocols {
    /// Discards abandoned activation tokens on a bounded housekeeping
    /// cadence. This is called every compositor dispatch pass, but the
    /// deadline keeps the ordinary no-op path to one timestamp comparison.
    pub(crate) fn sweep_activation_tokens(&mut self, now: Instant) {
        if now < self.next_activation_token_sweep {
            return;
        }
        self.next_activation_token_sweep = now + ACTIVATION_TOKEN_SWEEP_INTERVAL;
        let before = self.activation.tokens().count();
        self.activation
            .retain_tokens(|_, data| activation_token_is_fresh(now, data.timestamp));
        let removed = before - self.activation.tokens().count();
        if removed > 0 {
            tracing::debug!(removed, "expired abandoned xdg-activation tokens");
        }
    }
}

fn activation_token_is_fresh(now: Instant, created: Instant) -> bool {
    now.checked_duration_since(created)
        .is_none_or(|age| age < ACTIVATION_TOKEN_TTL)
}

impl XdgActivationHandler for Compositor {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.core_protocols.activation
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        let mut total = 0;
        let mut for_client = 0;
        for (_, known) in self.core_protocols.activation.tokens() {
            total += 1;
            if known.client_id == data.client_id {
                for_client += 1;
            }
        }
        if total < MAX_ACTIVATION_TOKENS_GLOBAL && for_client < MAX_ACTIVATION_TOKENS_PER_CLIENT {
            return true;
        }

        // A hostile client may keep asking after it hits the ceiling.
        // Powers-of-two logging keeps that visible without turning the
        // defense itself into an unbounded logging attack.
        self.core_protocols.rejected_activation_tokens =
            self.core_protocols.rejected_activation_tokens.saturating_add(1);
        let rejected = self.core_protocols.rejected_activation_tokens;
        if rejected.is_power_of_two() {
            tracing::warn!(
                rejected,
                total,
                for_client,
                per_client_limit = MAX_ACTIVATION_TOKENS_PER_CLIENT,
                global_limit = MAX_ACTIVATION_TOKENS_GLOBAL,
                "refusing excess xdg-activation token"
            );
        }
        false
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        let mut root = surface;
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        if let Some(window) = self.wm.backend().window_for_surface(&root) {
            self.wm.dispatch(BackendEvent::ActivateRequested(window));
        }
        // Tokens are single-use on this desktop. Retaining an already
        // consumed token would let an unrelated later request steal focus.
        self.core_protocols.activation.remove_token(&token);
    }
}

impl XdgForeignHandler for Compositor {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.core_protocols.xdg_foreign
    }
}

impl KeyboardShortcutsInhibitHandler for Compositor {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.core_protocols.shortcuts
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        let focused = self.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus());
        if focused.as_ref() == Some(inhibitor.wl_surface()) {
            inhibitor.activate();
            self.core_protocols.active_shortcut_inhibitor = Some(inhibitor);
        }
    }

    fn inhibitor_destroyed(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        if self.core_protocols.active_shortcut_inhibitor.as_ref().is_some_and(|active| active == &inhibitor) {
            self.core_protocols.active_shortcut_inhibitor = None;
        }
    }
}

impl PointerConstraintsHandler for Compositor {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        if pointer.current_focus().as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    /// The client's statement of where the cursor should reappear when
    /// its lock ends — how a game puts the arrow back on the menu item
    /// the player was over, instead of wherever the lock happened to
    /// start.
    ///
    /// Nothing is done *here* on purpose: the protocol says the hint
    /// takes effect when the lock is released, not when it is set, and
    /// smithay keeps the committed value on the constraint for us. The
    /// place it is read is [`crate::input::release_pointer_constraint`].
    /// Before that existed this body was empty under a comment claiming
    /// the hint was consumed on release, which nothing did.
    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        location: LogicalPoint<f64, Logical>,
    ) {
        tracing::debug!(
            surface = ?surface.id(),
            x = location.x,
            y = location.y,
            "client set a cursor-position hint for when its pointer lock ends"
        );
    }
}

impl InputMethodHandler for Compositor {
    fn new_popup(&mut self, surface: PopupSurface) {
        let new_id = surface.wl_surface().id();
        let (total, for_client) = {
            let backend = self.wm.backend_mut();
            // The protocol role's exact destroy callback below is the
            // primary removal path. This also discards an already-dead
            // wl_surface before applying the admission limits.
            backend.ime_popups.retain(PopupSurface::alive);
            let total = backend.ime_popups.len();
            let for_client = backend
                .ime_popups
                .iter()
                .filter(|popup| popup.wl_surface().id().same_client_as(&new_id))
                .count();
            (total, for_client)
        };
        if total < MAX_IME_POPUPS_GLOBAL && for_client < MAX_IME_POPUPS_PER_CLIENT {
            let backend = self.wm.backend_mut();
            backend.ime_popups.push(surface);
            backend.mark_damaged();
            return;
        }

        self.core_protocols.rejected_ime_popups = self.core_protocols.rejected_ime_popups.saturating_add(1);
        let rejected = self.core_protocols.rejected_ime_popups;
        if rejected.is_power_of_two() {
            tracing::warn!(
                rejected,
                total,
                for_client,
                per_client_limit = MAX_IME_POPUPS_PER_CLIENT,
                global_limit = MAX_IME_POPUPS_GLOBAL,
                "refusing excess input-method popup"
            );
        }
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        self.wm.backend_mut().ime_popups.retain(|popup| popup != &surface);
        self.wm.backend_mut().mark_damaged();
    }

    fn popup_repositioned(&mut self, _surface: PopupSurface) {
        self.wm.backend_mut().mark_damaged();
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.wm
            .backend()
            .window_for_surface(parent)
            .and_then(|id| self.wm.backend().windows.get(&id))
            .map(|record| {
                Rectangle::new(
                    (record.content.pos.x, record.content.pos.y).into(),
                    (record.content.size.w as i32, record.content.size.h as i32).into(),
                )
            })
            .unwrap_or_default()
    }
}

impl smithay::wayland::shell::xdg::dialog::XdgDialogHandler for Compositor {
    fn modal_changed(
        &mut self,
        toplevel: smithay::wayland::shell::xdg::ToplevelSurface,
        is_modal: bool,
    ) {
        let backend = self.wm.backend_mut();
        let Some(window) = backend.window_for_surface(toplevel.wl_surface()) else {
            return;
        };
        if let Some(record) = backend.windows.get_mut(&window) {
            record.modal = is_modal;
        }
        backend.queue(BackendEvent::ModalChanged {
            window,
            modal: is_modal,
        });
    }
}

impl smithay::wayland::xdg_toplevel_tag::XdgToplevelTagHandler for Compositor {}

impl smithay::wayland::xdg_system_bell::XdgSystemBellHandler for Compositor {
    fn ring(&mut self, surface: Option<WlSurface>) {
        let id = surface
            .as_ref()
            .and_then(|surface| self.wm.backend().window_for_surface(surface))
            .and_then(|window| self.wm.client_for_window(window));
        if let Some(id) = id {
            self.wm.set_urgent(id, true);
        }
        tracing::info!(?id, "client rang the system bell");
    }
}

delegate_xdg_activation!(Compositor);
delegate_cursor_shape!(Compositor);
delegate_single_pixel_buffer!(Compositor);
delegate_presentation!(Compositor);
delegate_fifo!(Compositor);
delegate_commit_timing!(Compositor);
delegate_security_context!(Compositor);

#[cfg(test)]
mod security_context_tests {
    use super::*;
    use smithay::reexports::wayland_server::Display;
    use std::sync::Arc;

    #[test]
    fn confined_clients_cannot_nest_security_contexts_but_keep_current_policy() {
        let display = Display::<Compositor>::new().expect("wayland display");
        let mut handle = display.handle();
        let (creator_socket, _creator_peer) =
            std::os::unix::net::UnixStream::pair().expect("creator socketpair");
        let creator = handle
            .insert_client(creator_socket, Arc::new(crate::state::ClientState::default()))
            .expect("admit creator");
        let context = smithay::wayland::security_context::SecurityContext {
            sandbox_engine: Some("test".into()),
            app_id: Some("org.chonkstep.test".into()),
            instance_id: None,
            creator_client_id: creator.id(),
        };
        let (confined_socket, _confined_peer) =
            std::os::unix::net::UnixStream::pair().expect("confined socketpair");
        let confined = handle
            .insert_client(
                confined_socket,
                Arc::new(crate::state::ClientState::confined(context)),
            )
            .expect("admit confined client");

        assert!(crate::state::security_context_global_visible(&creator));
        assert!(!crate::state::security_context_global_visible(&confined));
        assert!(crate::state::privileged_global_visible(&creator));
        assert!(crate::state::privileged_global_visible(&confined));
    }
}
delegate_relative_pointer!(Compositor);
delegate_pointer_constraints!(Compositor);
delegate_pointer_gestures!(Compositor);
delegate_tablet_manager!(Compositor);
delegate_xdg_foreign!(Compositor);
delegate_keyboard_shortcuts_inhibit!(Compositor);
delegate_text_input_manager!(Compositor);
delegate_xdg_dialog!(Compositor);
delegate_xdg_system_bell!(Compositor);
delegate_xdg_toplevel_tag!(Compositor);

// Smithay's input-method delegation forwards popup-role destruction only
// to its own AliveTracker. Split the macro so the compositor can remove
// the corresponding per-frame ledger entry at the exact object-lifetime
// edge (including client disconnect), then forward to that tracker.
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    ZwpInputMethodManagerV2: InputMethodManagerGlobalData
] => InputMethodManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpInputMethodManagerV2: ()
] => InputMethodManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpInputMethodV2: InputMethodUserData<Compositor>
] => InputMethodManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpInputMethodKeyboardGrabV2: InputMethodKeyboardUserData<Compositor>
] => InputMethodManagerState);

impl Dispatch<ZwpInputPopupSurfaceV2, InputMethodPopupSurfaceUserData> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        object: &ZwpInputPopupSurfaceV2,
        request: <ZwpInputPopupSurfaceV2 as Resource>::Request,
        data: &InputMethodPopupSurfaceUserData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        <InputMethodManagerState as Dispatch<
            ZwpInputPopupSurfaceV2,
            InputMethodPopupSurfaceUserData,
            Compositor,
        >>::request(state, client, object, request, data, dhandle, data_init);
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        object: &ZwpInputPopupSurfaceV2,
        data: &InputMethodPopupSurfaceUserData,
    ) {
        let backend = state.wm.backend_mut();
        let before = backend.ime_popups.len();
        backend.ime_popups.retain(|popup| popup.surface_role != *object);
        if backend.ime_popups.len() != before {
            backend.mark_damaged();
        }
        <InputMethodManagerState as Dispatch<
            ZwpInputPopupSurfaceV2,
            InputMethodPopupSurfaceUserData,
            Compositor,
        >>::destroyed(state, client, object, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_tokens_expire_at_the_ttl_and_future_timestamps_are_safe() {
        let now = Instant::now();
        assert!(activation_token_is_fresh(now, now - ACTIVATION_TOKEN_TTL + Duration::from_nanos(1)));
        assert!(!activation_token_is_fresh(now, now - ACTIVATION_TOKEN_TTL));
        assert!(activation_token_is_fresh(now, now + Duration::from_secs(1)));
    }
}
