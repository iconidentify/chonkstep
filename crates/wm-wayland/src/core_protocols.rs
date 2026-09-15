//! Small, standard Wayland globals ordinary desktop clients expect.
//!
//! Smithay owns the wire implementations. This module keeps their
//! lifecycle and the handful of compositor policy callbacks together,
//! rather than scattering one-field protocol states through `state.rs`.

use std::time::{Duration, Instant};

use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::{
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::ZwpInputMethodV2,
    zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{backend::ClientId, Client, DataInit, Dispatch, DisplayHandle, Resource, Weak};
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::input_method::{
    InputMethodHandler, InputMethodKeyboardUserData, InputMethodManagerGlobalData, InputMethodManagerState,
    InputMethodPopupSurfaceUserData, InputMethodUserData, PopupSurface,
};
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
    KeyboardShortcutsInhibitorSeat,
};
use smithay::wayland::pointer_constraints::PointerConstraintsState;
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

use crate::state::{Compositor, WaylandBackend};

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
pub(crate) const ACTIVATION_TOKEN_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// How long after its minting a token may still move the keyboard.
/// A second, shorter clock than the TTL: the TTL says when an abandoned
/// token is forgotten, this says when a redeemed one has gone stale.
/// The two things a token legitimately does happen quickly — a running
/// single-instance application raises its window the moment its second
/// process hands the token over, and a terminal's link opens in a
/// browser that is already up — while a process that sits on its token
/// and redeems it minutes later is the shape of a focus steal. Thirty
/// seconds covers a loaded machine's D-Bus round trip with room to
/// spare; a cold start that maps a *new* window gets focus from the
/// map-time policy and never needed the token for it.
const ACTIVATION_FOCUS_WINDOW: Duration = Duration::from_secs(30);
const MAX_ACTIVATION_TOKENS_PER_CLIENT: usize = 256;
/// The per-client ceiling prevents one connection from growing the pool;
/// this second ceiling also covers an attacker cycling connections.
const MAX_ACTIVATION_TOKENS_GLOBAL: usize = 4_096;
/// IME popup surfaces participate in every render and pointer hit-test.
/// One input method normally owns one popup; these ceilings preserve
/// headroom for hand-offs while bounding hostile protocol-object churn.
const MAX_IME_POPUPS_PER_CLIENT: usize = 16;
const MAX_IME_POPUPS_GLOBAL: usize = 256;

impl smithay::wayland::tablet_manager::TabletSeatHandler for Compositor {
    /// A tool's cursor surface, hide or named shape. The tablet handlers
    /// never move the pointer, so without this a pen hovering the desk
    /// drew no cursor at all. See `input::set_tablet_cursor_image`.
    fn tablet_tool_image(
        &mut self,
        tool: &smithay::backend::input::TabletToolDescriptor,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        crate::input::set_tablet_cursor_image(self, tool, image);
    }
}

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
        self.wm.backend().window_for_surface(surface).map(|_| crate::input::keyboard::KeyboardFocus::new(self, surface.clone()))
    }
}

smithay::delegate_xwayland_keyboard_grab!(Compositor);

/// State retained for the globals whose helpers need a getter or whose
/// `GlobalId` lifetime is tied to the state value.
pub(crate) struct CoreProtocols {
    rejected_activation_tokens: u64,
    rejected_ime_popups: u64,
    pub xdg_foreign: XdgForeignState,
    pub shortcuts: KeyboardShortcutsInhibitState,
    /// The one `zwp_keyboard_shortcuts_inhibitor_v1` grant in force:
    /// the focused surface's, when policy let it have one. See the
    /// `Compositor` impl below for the policy.
    pub active_shortcut_inhibitor: Option<KeyboardShortcutsInhibitor>,
    /// The surfaces whose grants the user suspended with the escape
    /// chord. Keyed by surface rather than by inhibitor object
    /// so a client cannot have its grant back by destroying and
    /// recreating the inhibitor, and `Weak` so the surface's own
    /// destruction clears it without a hook of its own. Focus may
    /// leave and return while it is set: the grant stays suspended
    /// until the chord is pressed again with this surface focused.
    suspended_inhibit_surfaces: Vec<Weak<WlSurface>>,
    /// Budget for the info-level inhibit lines. A client creating and
    /// destroying inhibitors in a loop while focused would otherwise
    /// turn one line per decision into an unbounded log.
    inhibit_log: LogBudget,
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
        rejected_activation_tokens: 0,
        rejected_ime_popups: 0,
        xdg_foreign: XdgForeignState::new::<Compositor>(display),
        shortcuts: KeyboardShortcutsInhibitState::new::<Compositor>(display),
        active_shortcut_inhibitor: None,
        suspended_inhibit_surfaces: Vec::new(),
        inhibit_log: LogBudget::default(),
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

/// Marker in a token's user data: the compositor minted this token for
/// a command it launched itself (`Backend::create_activation_token`).
/// Such a token carries no client and no serial — there was no Wayland
/// client behind the request, only a keybinding or a menu pick — and it
/// is the one kind of serial-less token `request_activation` honours.
pub(crate) struct CompositorIssued;

/// Focus generation in which the creator supplied a plausible input
/// serial. Decide this at mint time: a later focus change cannot turn a
/// token created in the background into evidence of user input.
struct FocusedActivationInput(smithay::utils::Serial);

fn activation_serial_in_focus(
    serial: smithay::utils::Serial,
    enter: smithay::utils::Serial,
    next: smithay::utils::Serial,
) -> bool {
    serial.is_no_older_than(&enter) && serial < next
}

impl WaylandBackend {
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

    /// A fresh single-use token for a command the desktop launches,
    /// marked [`CompositorIssued`]. Not routed through `token_created`,
    /// so it is not counted against any client's admission quota; it
    /// still expires with every other token at the TTL.
    pub(crate) fn mint_activation_token(&mut self) -> String {
        let data = XdgActivationTokenData::default();
        data.user_data.insert_if_missing(|| CompositorIssued);
        let (token, _) = self.activation.create_external_token(data);
        token.as_str().to_string()
    }
}

fn activation_token_is_fresh(now: Instant, created: Instant) -> bool {
    now.checked_duration_since(created)
        .is_none_or(|age| age < ACTIVATION_TOKEN_TTL)
}

/// What one `xdg_activation_v1.activate` request looks like once the
/// token and the seat have been consulted, reduced to the facts the
/// policy needs so the verdict itself is a pure function.
#[derive(Clone, Copy, Debug)]
struct ActivationRequest {
    /// The token came from [`WaylandBackend::mint_activation_token`].
    compositor_issued: bool,
    /// The token's creator holds the keyboard right now, and the serial
    /// it named is no older than the keyboard's entry into that client:
    /// the token was made while the user was in that client, and the
    /// user still is.
    from_focused_input: bool,
    /// How long ago the token was created.
    age: Duration,
    /// `misc:focus_on_activate`, read live.
    focus_on_activate: bool,
    /// A session lock covers the desktop.
    locked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivationVerdict {
    /// Reveal, raise and focus the window, as a taskbar click would.
    Focus,
    /// Leave the keyboard where it is and mark the window urgent.
    Urgent,
}

/// The policy `misc:focus_on_activate` names. Off, a request moves the
/// keyboard only when the user is known to be behind it: the compositor
/// minted the token for a command the user ran, or the client the user
/// is typing in made the token from one of that user's own input
/// events — and either way it is redeemed while still fresh. A token a
/// background client made for its own window has neither property and
/// earns an urgency hint instead. On, every request is honoured, which
/// is what Omarchy's shipped configuration asks for.
///
/// The lock screen wins over both: keyboard focus is parked until
/// unlock anyway, and revealing the window would switch the workspace
/// under a desk the user cannot see. Urgent is the one answer that
/// stays visible on the bar and costs the user nothing they did not do.
fn activation_verdict(request: &ActivationRequest) -> ActivationVerdict {
    if request.locked {
        return ActivationVerdict::Urgent;
    }
    let fresh = request.age < ACTIVATION_FOCUS_WINDOW;
    if request.focus_on_activate || ((request.compositor_issued || request.from_focused_input) && fresh) {
        ActivationVerdict::Focus
    } else {
        ActivationVerdict::Urgent
    }
}

impl XdgActivationHandler for Compositor {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.wm.backend_mut().activation
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        let mut total = 0;
        let mut for_client = 0;
        for (_, known) in self.wm.backend().activation.tokens() {
            total += 1;
            if known.client_id == data.client_id {
                for_client += 1;
            }
        }
        if total < MAX_ACTIVATION_TOKENS_GLOBAL && for_client < MAX_ACTIVATION_TOKENS_PER_CLIENT {
            if let Some(keyboard) = self.seat.get_keyboard() {
                let focused_client = keyboard.current_focus()
                    .and_then(|focus| focus.surface().client()).map(|client| client.id());
                if focused_client.is_some() && focused_client == data.client_id {
                    if let (Some((serial, seat)), Some(enter)) = (&data.serial, keyboard.last_enter()) {
                        if smithay::input::Seat::<Self>::from_resource(seat).as_ref() == Some(&self.seat)
                            && activation_serial_in_focus(*serial, enter, smithay::utils::SERIAL_COUNTER.next_serial())
                        {
                            data.user_data.insert_if_missing(|| FocusedActivationInput(enter));
                        }
                    }
                }
            }
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

    /// The one activation route that carries a token, and so the one
    /// place the `misc:focus_on_activate` policy lives. The user-driven
    /// routes — a taskbar's foreign-toplevel `activate`, a pager's
    /// `_NET_ACTIVE_WINDOW`, Alt-Tab, IPC `focuswindow` — dispatch
    /// `ActivateRequested` directly and stay unconditional.
    ///
    /// The check is on the token's *creator*: who held the keyboard
    /// when it was made and whether it named one of that client's
    /// input serials. Who redeems it is deliberately not checked — a
    /// terminal handing its token to the browser it just started is
    /// exactly what the protocol is for.
    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Tokens are single-use on this desktop. Retaining an already
        // consumed token would let an unrelated later request steal focus.
        self.wm.backend_mut().activation.remove_token(&token);
        let mut root = surface;
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        let Some(window) = self.wm.backend().window_for_surface(&root) else {
            return;
        };
        let Some(id) = self.wm.client_for_window(window) else {
            return;
        };
        let locked = self.wm.backend().locked;
        if self.wm.focused_client() == Some(id) {
            // Nothing to steal: the window already has the keyboard.
            // Honouring the request keeps today's reveal-and-raise for
            // a client activating itself; behind the lock even that is
            // deferred focus work, so it is dropped rather than turned
            // into an urgency hint on the window the user is in.
            if !locked {
                self.wm.dispatch(BackendEvent::ActivateRequested(window));
            }
            return;
        }
        let compositor_issued = data.user_data.get::<CompositorIssued>().is_some();
        let from_focused_input = self.seat.get_keyboard().is_some_and(|keyboard| {
            let focused_client = keyboard
                .current_focus()
                .and_then(|focus| focus.surface().client())
                .map(|client| client.id());
            let serial_ok = match (data.user_data.get::<FocusedActivationInput>(), keyboard.last_enter()) {
                (Some(FocusedActivationInput(created_enter)), Some(enter)) => *created_enter == enter,
                _ => false,
            };
            serial_ok && focused_client.is_some() && focused_client == data.client_id
        });
        let request = ActivationRequest {
            compositor_issued,
            from_focused_input,
            age: data.timestamp.elapsed(),
            focus_on_activate: self.wm.focus_on_activate(),
            locked,
        };
        match activation_verdict(&request) {
            ActivationVerdict::Focus => self.wm.dispatch(BackendEvent::ActivateRequested(window)),
            ActivationVerdict::Urgent => {
                tracing::info!(
                    ?id,
                    compositor_issued,
                    from_focused_input,
                    has_serial = data.serial.is_some(),
                    age_ms = request.age.as_millis() as u64,
                    locked,
                    "activation request refused the keyboard; window marked urgent"
                );
                self.wm.set_urgent(id, true);
            }
        }
    }
}

impl XdgForeignHandler for Compositor {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.core_protocols.xdg_foreign
    }
}

/// How many inhibit-policy lines a window of [`INHIBIT_LOG_WINDOW`]
/// gets before the rest of the window is counted instead of logged.
const INHIBIT_LOG_BURST: u32 = 8;
const INHIBIT_LOG_WINDOW: Duration = Duration::from_secs(10);

/// A per-window budget for a log line a client can trigger at will.
///
/// The first [`INHIBIT_LOG_BURST`] events of a window are logged one
/// by one; the rest are counted, and the count rides on the first line
/// of the next window. A looping client thus costs the log a burst per
/// window instead of a line per iteration, and the loop itself stays
/// visible as the suppressed count.
#[derive(Debug, Default)]
struct LogBudget {
    window_start: Option<Instant>,
    logged: u32,
    suppressed: u64,
}

impl LogBudget {
    /// `Some(n)` when this event gets its own line, with `n` the events
    /// that went unlogged since the last line; `None` when it does not.
    fn admit(&mut self, now: Instant) -> Option<u64> {
        let fresh_window = self
            .window_start
            .is_none_or(|start| now.saturating_duration_since(start) >= INHIBIT_LOG_WINDOW);
        if fresh_window {
            self.window_start = Some(now);
            self.logged = 1;
            return Some(std::mem::take(&mut self.suppressed));
        }
        if self.logged < INHIBIT_LOG_BURST {
            self.logged += 1;
            Some(0)
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            None
        }
    }
}

/// `zwp_keyboard_shortcuts_inhibit_v1` policy.
///
/// The protocol exists so a VM console or a remote-desktop viewer can
/// receive the chords the host would otherwise take, and it expects
/// the compositor to decide who gets that and to keep "a special key
/// combo … allowing the user to forcibly restore normal keyboard
/// events routing in the case of an unwilling client". Three rules
/// decide a grant, in `shortcut_inhibit_permitted`: the config allows
/// grants at all (`allow_shortcut_inhibit`, Hyprland's
/// `binds:disable_keybind_grabbing` inverted), the client is not
/// sandboxed (a security-context client is hidden from every other
/// desktop-level capability, and an inhibitor plus fullscreen is what
/// a convincing fake lock screen is made of), and the user has not
/// suspended this surface's grant with the escape chord. A refused
/// inhibitor simply never receives `active`, which the protocol reads
/// as "not granted".
///
/// The escape chord (`shortcuts_inhibit_escape`) suspends the grant in
/// force and swallows itself; pressing it again with the same surface
/// focused resumes the grant. Suspension is keyed by surface, so
/// neither a focus round-trip nor destroying and recreating the
/// inhibitor gets the client its grant back — only the user does. The
/// session lock and the VT switch outrank all of this: see the key
/// filter in `input.rs`, where the chord is matched.
impl Compositor {
    /// The name the log and `systeminfo` know a holder by: the app id
    /// of the window the surface belongs to, or the surface id for a
    /// surface with no window (odd, and the id is what identifies it).
    fn inhibit_holder_name(&self, surface: &WlSurface) -> String {
        self.wm
            .backend()
            .window_for_surface(surface)
            .and_then(|id| self.wm.backend().windows.get(&id))
            .and_then(|record| record.app_id.clone())
            .unwrap_or_else(|| format!("{:?}", surface.id()))
    }

    fn inhibit_log_slot(&mut self) -> Option<u64> {
        self.core_protocols.inhibit_log.admit(Instant::now())
    }

    fn shortcut_inhibit_suspended(&self, surface: &WlSurface) -> bool {
        self.core_protocols
            .suspended_inhibit_surfaces
            .iter()
            .any(|weak| weak.upgrade().is_ok_and(|suspended| suspended == *surface))
    }

    /// Whether `surface` may hold an active grant right now, or why not.
    fn shortcut_inhibit_permitted(&self, surface: &WlSurface) -> Result<(), &'static str> {
        if !self.wm.backend().shortcut_inhibit_policy.allow {
            return Err("allow_shortcut_inhibit is off");
        }
        if surface.client().is_some_and(|client| crate::state::client_is_confined(&client)) {
            return Err("the client is sandboxed");
        }
        if self.shortcut_inhibit_suspended(surface) {
            return Err("the user suspended this window's grant");
        }
        Ok(())
    }

    /// Activates `inhibitor` if its surface may hold a grant, and logs
    /// the decision either way. The caller has already withdrawn any
    /// grant in force.
    fn grant_shortcut_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        let holder = self.inhibit_holder_name(inhibitor.wl_surface());
        match self.shortcut_inhibit_permitted(inhibitor.wl_surface()) {
            Ok(()) => {
                inhibitor.activate();
                self.core_protocols.active_shortcut_inhibitor = Some(inhibitor);
                if let Some(suppressed) = self.inhibit_log_slot() {
                    tracing::info!(
                        holder,
                        suppressed,
                        "keyboard shortcuts inhibited: the focused client receives every chord until the shortcuts_inhibit_escape chord or a focus change"
                    );
                }
            }
            Err(why) => {
                if let Some(suppressed) = self.inhibit_log_slot() {
                    tracing::info!(holder, why, suppressed, "declined a keyboard-shortcuts inhibitor");
                }
            }
        }
    }

    /// Withdraws the grant in force, if any. The suspension, if any,
    /// is not touched: it belongs to the surface, not to the grant.
    fn withdraw_shortcut_inhibitor(&mut self, why: &'static str) {
        let Some(active) = self.core_protocols.active_shortcut_inhibitor.take() else {
            return;
        };
        active.inactivate();
        let holder = self.inhibit_holder_name(active.wl_surface());
        if let Some(suppressed) = self.inhibit_log_slot() {
            tracing::info!(holder, why, suppressed, "keyboard shortcuts restored");
        }
    }

    /// Moves the one grant with keyboard focus: withdraws the old one
    /// and grants the new focus's inhibitor, if it has one and may
    /// hold it. `focus_changed` calls this on every focus change,
    /// which is what keeps a background VM from retaining raw keys —
    /// and what keeps the session lock ahead of every inhibitor, since
    /// the lock surface takes focus.
    pub(crate) fn sync_shortcut_inhibitor_to_focus(&mut self, target: Option<&WlSurface>) {
        if let (Some(active), Some(surface)) = (self.core_protocols.active_shortcut_inhibitor.as_ref(), target) {
            if active.wl_surface() == surface {
                return;
            }
        }
        self.withdraw_shortcut_inhibitor("keyboard focus left the window");
        if let Some(surface) = target {
            if let Some(inhibitor) = self.seat.keyboard_shortcuts_inhibitor_for_surface(surface) {
                self.grant_shortcut_inhibitor(inhibitor);
            }
        }
    }

    /// The escape chord was pressed (and the session is not locked).
    /// A grant in force is suspended; otherwise a suspended focused
    /// surface has its grant resumed. Returns whether the press meant
    /// either — the caller swallows it then, and treats it as an
    /// ordinary key when it did not.
    pub(crate) fn shortcut_inhibit_escape_pressed(&mut self) -> bool {
        if let Some(active) = self.core_protocols.active_shortcut_inhibitor.take() {
            active.inactivate();
            let surface = active.wl_surface();
            self.core_protocols.suspended_inhibit_surfaces.retain(|weak| weak.upgrade().is_ok());
            if !self.shortcut_inhibit_suspended(surface) {
                self.core_protocols.suspended_inhibit_surfaces.push(surface.downgrade());
            }
            let holder = self.inhibit_holder_name(surface);
            // Not budgeted: this is the user's own key, once per press.
            tracing::info!(
                holder,
                "keyboard shortcuts restored by the user: the window's inhibitor is suspended until the chord is pressed again"
            );
            return true;
        }
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .map(|focus| focus.surface().clone());
        let Some(surface) = focused.filter(|surface| self.shortcut_inhibit_suspended(surface)) else {
            return false;
        };
        self.core_protocols.suspended_inhibit_surfaces.retain(|weak| {
            weak.upgrade().is_ok_and(|suspended| suspended != surface)
        });
        let holder = self.inhibit_holder_name(&surface);
        tracing::info!(holder, "the user resumed the window's keyboard-shortcuts inhibitor");
        if let Some(inhibitor) = self.seat.keyboard_shortcuts_inhibitor_for_surface(&surface) {
            self.grant_shortcut_inhibitor(inhibitor);
        }
        true
    }

    /// Reconciles the grant in force with a policy the shell just
    /// applied (`Backend::set_shortcut_inhibit_policy` stages it):
    /// grants turned off withdraw the active one; grants turned back
    /// on give the focused surface its inhibitor, if it has one.
    pub(crate) fn apply_shortcut_inhibit_policy(&mut self) {
        if !std::mem::take(&mut self.wm.backend_mut().shortcut_inhibit_policy_changed) {
            return;
        }
        let policy = self.wm.backend().shortcut_inhibit_policy.clone();
        if policy.escape.is_none() {
            tracing::warn!(
                "shortcuts_inhibit_escape is unbound: nothing on the keyboard but a VT switch leaves a client that inhibits shortcuts"
            );
        }
        if policy.allow {
            let focused = self
                .seat
                .get_keyboard()
                .and_then(|keyboard| keyboard.current_focus())
                .map(|focus| focus.surface().clone());
            self.sync_shortcut_inhibitor_to_focus(focused.as_ref());
        } else {
            self.withdraw_shortcut_inhibitor("allow_shortcut_inhibit was turned off");
        }
    }

    /// One line for `hyprctl systeminfo`: who holds the shortcuts, or
    /// whose grant is suspended, or that grants are off.
    pub(crate) fn shortcut_inhibit_report(&self) -> String {
        if let Some(active) = &self.core_protocols.active_shortcut_inhibitor {
            return format!("active holder={}", self.inhibit_holder_name(active.wl_surface()));
        }
        if let Some(surface) = self.seat.get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .map(|focus| focus.surface().clone())
            .filter(|surface| self.shortcut_inhibit_suspended(surface))
        {
            return format!("suspended holder={}", self.inhibit_holder_name(&surface));
        }
        if self.wm.backend().shortcut_inhibit_policy.allow { "none".into() } else { "disabled".into() }
    }
}

impl KeyboardShortcutsInhibitHandler for Compositor {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.core_protocols.shortcuts
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        let focused = self.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus());
        if focused.as_ref().map(crate::input::keyboard::KeyboardFocus::surface) != Some(inhibitor.wl_surface()) {
            // Nothing to decide until the surface is focused;
            // `focus_changed` grants it then, policy permitting.
            return;
        }
        // A surface holds at most one inhibitor per seat (smithay
        // refuses a second with `already_inhibited`), so a grant in
        // force here is another surface's stale one; withdraw it so
        // there is ever one grant, and decide this one.
        self.withdraw_shortcut_inhibitor("a newer inhibitor was created");
        self.grant_shortcut_inhibitor(inhibitor);
    }

    fn inhibitor_destroyed(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        if self.core_protocols.active_shortcut_inhibitor.as_ref().is_some_and(|active| active == &inhibitor) {
            self.core_protocols.active_shortcut_inhibitor = None;
            let holder = self.inhibit_holder_name(inhibitor.wl_surface());
            if let Some(suppressed) = self.inhibit_log_slot() {
                tracing::info!(holder, suppressed, "keyboard shortcuts restored: the client destroyed its inhibitor");
            }
        }
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

/// The longest `xdg_toplevel_tag_v1` tag or description kept, in
/// bytes. Both are client-controlled strings that reach the Hyprland
/// IPC reply and the window-rule matcher's input, so a client must not
/// be able to grow either without limit; a tag is a short identifier
/// (`main`, `preferences`) and a description one sentence, so the
/// bound is far above anything honest.
const MAX_TOPLEVEL_TAG_BYTES: usize = 256;

/// `text` cut to at most `max` bytes at a character boundary, so a
/// multi-byte character straddling the bound is dropped whole rather
/// than leaving the string invalid UTF-8.
fn bounded_utf8(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

impl smithay::wayland::xdg_toplevel_tag::XdgToplevelTagHandler for Compositor {
    fn set_tag(&mut self, toplevel: XdgToplevel, tag: String) {
        let tag = bounded_utf8(tag, MAX_TOPLEVEL_TAG_BYTES);
        self.store_toplevel_metadata(&toplevel, |record| {
            let changed = record.xdg_tag.as_deref() != Some(tag.as_str());
            record.xdg_tag = Some(tag);
            changed
        });
    }

    fn set_description(&mut self, toplevel: XdgToplevel, description: String) {
        let description = bounded_utf8(description, MAX_TOPLEVEL_TAG_BYTES);
        self.store_toplevel_metadata(&toplevel, |record| {
            let changed = record.xdg_description.as_deref() != Some(description.as_str());
            record.xdg_description = Some(description);
            changed
        });
    }
}

impl Compositor {
    /// Writes one `xdg_toplevel_tag_v1` value onto the toplevel's
    /// window record. ChonkStep keeps its own copies rather than
    /// reading Smithay's `XdgToplevelTagSurfaceData`, so the handler
    /// arguments are the one source for both the IPC and the rules.
    ///
    /// `write` returns whether the stored value changed; when it did,
    /// the publishers are told through `MetadataChanged` — not
    /// `TitleChanged`, which repaints chrome nothing here affects and
    /// would not bump the revision for an unchanged title anyway.
    /// A window rule is not re-run: it read the tag at map time, and
    /// clients set the tag before their first commit, when the record
    /// already exists (`new_toplevel` registers it) but nothing has
    /// mapped yet.
    fn store_toplevel_metadata(
        &mut self,
        toplevel: &XdgToplevel,
        write: impl FnOnce(&mut crate::state::WindowRecord) -> bool,
    ) {
        let Some(surface) = self.xdg_shell_state.get_toplevel(toplevel) else {
            return;
        };
        let backend = self.wm.backend_mut();
        let Some(window) = backend.window_for_surface(surface.wl_surface()) else {
            return;
        };
        let Some(record) = backend.windows.get_mut(&window) else {
            return;
        };
        if write(record) {
            backend.queue(BackendEvent::MetadataChanged(window));
        }
    }
}

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
    fn confined_clients_cannot_nest_security_contexts_or_use_privileged_globals() {
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
        assert!(!crate::state::privileged_global_visible(&confined));
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
    fn activation_serials_reject_future_and_previous_focus_events_including_wraparound() {
        for enter in [100_u32, u32::MAX - 2] {
            let next = enter.wrapping_add(5);
            assert!(activation_serial_in_focus(enter.into(), enter.into(), next.into()));
            assert!(activation_serial_in_focus(enter.wrapping_add(3).into(), enter.into(), next.into()));
            assert!(!activation_serial_in_focus(enter.wrapping_sub(1).into(), enter.into(), next.into()));
            assert!(!activation_serial_in_focus(next.into(), enter.into(), next.into()));
            assert!(!activation_serial_in_focus(next.wrapping_add(1000).into(), enter.into(), next.into()));
        }
    }

    /// A tag over the bound is cut at a character boundary, never
    /// inside a multi-byte character; one at or under it is untouched.
    #[test]
    fn toplevel_tags_are_bounded_at_a_character_boundary() {
        let short = "a".repeat(MAX_TOPLEVEL_TAG_BYTES);
        assert_eq!(bounded_utf8(short.clone(), MAX_TOPLEVEL_TAG_BYTES), short);
        assert_eq!(
            bounded_utf8("a".repeat(MAX_TOPLEVEL_TAG_BYTES + 1), MAX_TOPLEVEL_TAG_BYTES),
            short
        );
        // 255 ASCII bytes and then a three-byte character: byte 256
        // falls inside it, so the whole character goes.
        let straddling = format!("{}€", "a".repeat(MAX_TOPLEVEL_TAG_BYTES - 1));
        assert_eq!(bounded_utf8(straddling, MAX_TOPLEVEL_TAG_BYTES), "a".repeat(MAX_TOPLEVEL_TAG_BYTES - 1));
        // A run of multi-byte characters is cut to whole ones and stays
        // valid UTF-8 by construction.
        let euros = "€".repeat(100);
        let bounded = bounded_utf8(euros, MAX_TOPLEVEL_TAG_BYTES);
        assert_eq!(bounded, "€".repeat(85), "85 * 3 = 255 bytes, the most that fit");
        assert_eq!(bounded_utf8(String::new(), MAX_TOPLEVEL_TAG_BYTES), "");
    }

    #[test]
    fn activation_tokens_expire_at_the_ttl_and_future_timestamps_are_safe() {
        let now = Instant::now();
        assert!(activation_token_is_fresh(now, now - ACTIVATION_TOKEN_TTL + Duration::from_nanos(1)));
        assert!(!activation_token_is_fresh(now, now - ACTIVATION_TOKEN_TTL));
        assert!(activation_token_is_fresh(now, now + Duration::from_secs(1)));
    }

    /// A client toggling inhibitors in a loop gets a burst of lines per
    /// window, then a count; the count rides on the next window's first
    /// line, so the loop stays visible without the log growing with it.
    #[test]
    fn inhibit_log_budget_bursts_then_counts_then_reports_the_count() {
        let start = Instant::now();
        let mut budget = LogBudget::default();
        for _ in 0..INHIBIT_LOG_BURST {
            assert_eq!(budget.admit(start), Some(0));
        }
        for _ in 0..1000 {
            assert_eq!(budget.admit(start + Duration::from_secs(1)), None);
        }
        // A new window: one line, carrying the thousand it swallowed.
        assert_eq!(budget.admit(start + INHIBIT_LOG_WINDOW), Some(1000));
        assert_eq!(budget.admit(start + INHIBIT_LOG_WINDOW), Some(0));
        // A clock that went backwards is a fresh window, not a panic.
        assert_eq!(budget.admit(start - Duration::from_secs(1)), Some(0));
    }

    fn request() -> ActivationRequest {
        ActivationRequest {
            compositor_issued: false,
            from_focused_input: false,
            age: Duration::from_millis(50),
            focus_on_activate: false,
            locked: false,
        }
    }

    /// A background client minting a token for its own window — no
    /// serial, not the focused client — gets urgency, not the keyboard.
    /// That is the whole point of reading the token at all.
    #[test]
    fn a_self_made_token_from_a_background_client_earns_urgency_not_focus() {
        assert_eq!(activation_verdict(&request()), ActivationVerdict::Urgent);
    }

    /// The two provenances that prove the user is behind the request.
    #[test]
    fn compositor_issued_and_focused_input_tokens_move_focus_while_fresh() {
        assert_eq!(
            activation_verdict(&ActivationRequest { compositor_issued: true, ..request() }),
            ActivationVerdict::Focus
        );
        assert_eq!(
            activation_verdict(&ActivationRequest { from_focused_input: true, ..request() }),
            ActivationVerdict::Focus
        );
    }

    /// The focus window is a second, shorter clock than the token TTL:
    /// a token hoarded past it is still forgotten at the TTL, but no
    /// longer moves the keyboard.
    #[test]
    fn a_stale_token_stops_moving_focus_before_the_ttl_forgets_it() {
        let stale = ACTIVATION_FOCUS_WINDOW;
        assert!(stale < ACTIVATION_TOKEN_TTL);
        assert_eq!(
            activation_verdict(&ActivationRequest { compositor_issued: true, age: stale, ..request() }),
            ActivationVerdict::Urgent
        );
        assert_eq!(
            activation_verdict(&ActivationRequest { from_focused_input: true, age: stale, ..request() }),
            ActivationVerdict::Urgent
        );
        assert_eq!(
            activation_verdict(&ActivationRequest {
                from_focused_input: true,
                age: stale - Duration::from_millis(1),
                ..request()
            }),
            ActivationVerdict::Focus
        );
    }

    /// `misc:focus_on_activate = true` is today's unconditional
    /// behaviour, provenance and age notwithstanding.
    #[test]
    fn focus_on_activate_restores_unconditional_focus() {
        assert_eq!(
            activation_verdict(&ActivationRequest {
                focus_on_activate: true,
                age: ACTIVATION_TOKEN_TTL,
                ..request()
            }),
            ActivationVerdict::Focus
        );
    }

    /// Behind the lock every request is an urgency hint: even the ones
    /// that would otherwise switch workspace or park a focus change.
    #[test]
    fn the_lock_screen_downgrades_every_activation_to_urgency() {
        for allowed in [
            ActivationRequest { compositor_issued: true, ..request() },
            ActivationRequest { from_focused_input: true, ..request() },
            ActivationRequest { focus_on_activate: true, ..request() },
        ] {
            assert_eq!(
                activation_verdict(&ActivationRequest { locked: true, ..allowed }),
                ActivationVerdict::Urgent,
                "{allowed:?}"
            );
        }
    }
}
