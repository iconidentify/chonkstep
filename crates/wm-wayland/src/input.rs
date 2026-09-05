//! Seat input translation: raw backend events (winit today, libinput
//! under the `session` feature later) into exactly the `BackendEvent`
//! stream `wm-x11` produces, plus direct wl_seat delivery for the input
//! that belongs to clients rather than the window manager.
//!
//! The routing authority here is the same top-down hit-test the
//! renderer paints by: unmanaged override-redirect X11 windows, `above`
//! shell surfaces, the frame band (each frame with its client's xdg
//! popups floating over it, and the managed windows whose clients drew
//! their own chrome interleaved among them at their own depth), `below`
//! shell surfaces (see `backend_impl.rs`'s module doc on stacking
//! bands). On X11 this routing was the server's job — event windows,
//! passive grabs, replay — and `wm-x11` merely translated what the
//! server had already decided. A compositor IS the
//! server, so the decisions live here, and the grab verbs on the
//! backend (`grab_pointer_for_drag`, `grab_keyboard`) reduce to flags
//! this module consults instead of round-trips that can fail.
//!
//! X11's implicit grab — after a button press, every pointer event
//! until the last release reports against the window the press landed
//! on — is reproduced literally (`ImplicitGrab` below), because two
//! shell behaviors depend on it: a titlebar drag's release must reach
//! the frame even when the pointer has outrun it mid-drag, and a
//! launcher-strip/menu drag's release must reach the shell surface the
//! press armed, wherever the pointer is by then. For clicks that land
//! on client content, smithay's seat runs its own click grab with the
//! same semantics, so this module only pins the *routing* decision and
//! lets the seat pin the client-side focus.
//!
//! On top of that sits the drag grab ([`DragGrab`]): the same idea,
//! with two differences that only matter for a window whose client drew
//! its own chrome. It is taken by the window manager rather than by a
//! press, and it takes the pointer away from the client under it
//! entirely — the seat's click grab is dropped and the client is sent a
//! leave — because for the length of that drag the gesture belongs to
//! the desktop and not to the application it is happening over.
//!
//! Presses that hit nothing at all queue under [`ROOT_SHELL`] — the
//! sentinel `Compositor::dispatch_pending` splits off into
//! `Shell::on_root_press`, standing in for the root-window id the X11
//! loop compares against.
//!
//! Every coordinate in this module is GLOBAL — the one space spanning
//! all monitors that the ledger stores its rects in — and that is what
//! makes multi-monitor input free here: the hit-test compares a global
//! pointer position against global rects, and the surface-local
//! coordinates it hands the shell, `wm-core`, and the seat are all
//! differences of two global points, so a window on a monitor whose
//! origin is (1920, 0) resolves exactly like one at the origin. The
//! only place output geometry appears at all is confining the pointer
//! ([`confine_to_outputs`]), which is the one question — "where is the
//! pointer allowed to be" — that the physical layout genuinely answers.
//!
//! The cross-event routing state ([`InputState`]) lives in the seat's
//! user-data map rather than as a `Compositor` field: the seat is the
//! thing whose events the state describes, the map ties the lifetime to
//! it for free, and the handlers here stay the only code that can see
//! it.

use std::cell::RefCell;
use std::collections::HashSet;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent, GestureEndEvent,
    GesturePinchUpdateEvent as BackendPinchUpdateEvent, GestureSwipeUpdateEvent as BackendSwipeUpdateEvent,
    Device, DeviceCapability, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, MouseButton as InputMouseButton, PointerAxisEvent,
    PointerButtonEvent, PointerMotionEvent, ProximityState, TabletToolButtonEvent,
    TabletToolDescriptor, TabletToolEvent, TabletToolProximityEvent, TabletToolTipEvent,
    TabletToolTipState,
};
use smithay::desktop::utils::under_from_surface_tree;
use smithay::desktop::WindowSurfaceType;
use smithay::input::keyboard::{keysyms, FilterResult, KeyboardHandle, Keycode, Keysym, ModifiersState};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, MotionEvent,
    RelativeMotionEvent,
};
use smithay::input::{Seat, SeatHandler};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point as LogicalPoint, SERIAL_COUNTER};
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat;
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraint};
use smithay::wayland::shell::wlr_layer;
use smithay::wayland::tablet_manager::{TabletDescriptor, TabletSeatTrait};

use wm_core::{BackendEvent, DragHandle, KeyCombo, Modifiers, MonitorInfo, MouseButton, ScrollDelta, SurfaceRef};
use wm_theme_api::{Point, Rect, ResizeEdge};

use crate::state::{
    Compositor, ManagedSurface, PointerGrabChange, StackEntry, WaylandBackend, WlFrameId, WlShellId, WlWindowId,
    ROOT_SHELL,
};

/// The event shorthand every queue in this module speaks.
type WmEvent = BackendEvent<WlWindowId, WlFrameId>;

/// Input-routing state the event handlers carry between events. All of
/// it is *cross-event* memory — what the last event decided constrains
/// where the next one is allowed to go — parked on the seat's user-data
/// map (see the module doc for why there and not on `Compositor`).
#[derive(Default)]
struct InputState {
    /// The managed window the pointer was inside (chrome or content) at
    /// the last motion — crossing INTO one emits
    /// `BackendEvent::PointerEnter` exactly once, which is what
    /// focus-follows-mouse keys off. X11 gave us EnterNotify for free;
    /// here the crossing is detected by comparing consecutive
    /// hit-tests.
    ///
    /// Named by frame where there is one and by window where there is
    /// not, because a client-decorated window has no frame to name and
    /// still has to be focusable by hovering it — `wm-core`'s
    /// `handle_pointer_enter` resolves either spelling to the same
    /// client. Comparing the whole `SurfaceRef` (rather than an
    /// `Option<WlFrameId>` that collapses every frameless window to
    /// `None`) is what makes moving the pointer from one such window
    /// straight onto another count as a crossing.
    hovered: Option<SurfaceRef<WlWindowId, WlFrameId>>,
    /// X11-style implicit pointer grab: set on the first button press,
    /// held until the last button release, and every pointer event in
    /// between routes to the press's target rather than whatever is
    /// under the pointer now.
    implicit_grab: Option<ImplicitGrab>,
    /// Raw button codes whose press dismissed a focus grab
    /// (`focus_grab.rs`) instead of being routed anywhere. Their
    /// releases are swallowed too, so nothing downstream sees half of a
    /// click it never got the other half of — the same contract
    /// `suppressed_keys` below keeps for the keyboard. Raw codes rather
    /// than [`MouseButton`]s because the dismissing press can be a
    /// button `wm_button` does not map at all, and that one still has
    /// to be matched on the way up.
    grab_dismissals: Vec<u32>,
    /// Keycodes whose press was intercepted (a grabbed combo, or any
    /// press during the modal keyboard grab) — their releases are
    /// swallowed too, so a client never sees a release for a press it
    /// never got. This is what an X11 passive grab did server-side; a
    /// stray release confuses stateful clients (games, VMs) even though
    /// most toolkits shrug it off.
    suppressed_keys: Vec<Keycode>,
    /// One held `binde` binding. XKB sends repeat parameters to clients
    /// but compositors must repeat their own bindings themselves.
    repeating: Option<RepeatingKey>,
    /// Leftover fractions of a wheel notch, for the shell's discrete
    /// scroll channel — see [`ScrollAccumulator`].
    scroll: ScrollAccumulator,
    /// Tablet tools currently in proximity. Smithay keeps the handles
    /// internally but exposes no iterator over them, so this bounded
    /// set is the compositor's way to send every focused tool a
    /// `proximity_out` when the session lock changes the entire input
    /// domain underneath it. Entries leave on the matching physical
    /// proximity-out and the whole set is drained on lock/unlock.
    active_tablet_tools: HashSet<TabletToolDescriptor>,
}

#[derive(Clone, Copy)]
struct RepeatingKey {
    keycode: Keycode,
    combo: KeyCombo,
    next: std::time::Instant,
    interval: std::time::Duration,
    /// Number of compositor-owned repeat presses emitted for this
    /// hold. Besides making the state self-describing in diagnostics,
    /// this gives the end-to-end test door a direct observation of the
    /// scheduler without making repeat correctness depend on child
    /// process startup time.
    emitted: u64,
}

struct ImplicitGrab {
    target: PressTarget,
    /// Which buttons are currently held, by their `wm-core` identity.
    /// X11 keeps the original grab window when further buttons press
    /// mid-grab, and releases only end the grab when the last button
    /// lifts - mirrored exactly.
    ///
    /// A set rather than a count: a count only stays honest while
    /// presses and releases pair up perfectly, and they do not. A
    /// button held across a VT switch releases into the session that
    /// took the seat, and a device unplugged mid-click never reports
    /// the release at all - each of which would leave a count stuck
    /// above zero and route every later click to a stale target for
    /// the rest of the session. A repeated press of a button already
    /// in the set is now idempotent, and `session` clears the grab
    /// outright when the seat comes back (see
    /// [`resynchronise_input_after_resume`]).
    buttons: Vec<MouseButton>,
}

/// Where a button press landed — the routing target its whole implicit
/// grab inherits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressTarget {
    Shell(WlShellId),
    Frame(WlFrameId),
    Content(WlWindowId),
    /// A wlr-layer-shell surface: client territory, like `Content`,
    /// but with no managed window behind it for `wm-core` to hear
    /// about.
    Layer(crate::layers::LayerId),
    /// An input-method candidate popup. It has no wm-core owner, but
    /// the seat must preserve its client-side click grab.
    Ime,
    Root,
}

/// The pointer grab an interactive drag holds — `wm-core`'s
/// `Backend::grab_pointer_for_drag`, and the shell's for its dock and
/// launcher drags — parked on [`WaylandBackend`] because the verb that
/// takes it can reach nothing else.
///
/// A compositor sees every pointer event before any client does, which
/// is the entire condition an X11 pointer grab existed to create, and
/// this verb used to do nothing on that reasoning. It held for a window
/// wearing one of our frames: the titlebar IS our surface, so the
/// press, every motion and the release come back to `wm-core` whether
/// or not anything was grabbed. It is false for a window whose client
/// drew its own titlebar. That window has no frame, so a drag begun
/// with `xdg_toplevel.move` runs entirely over the client's own
/// content: `wm-core` saw the motion as an ordinary hover, never saw
/// the release, and left `active_move` set — the window followed the
/// cursor with no button held, which is exactly what dragging
/// LibreOffice's titlebar did. Seeing an event and being allowed to
/// keep it are different things, and this is what records the
/// difference.
pub(crate) struct DragGrab {
    /// The token `Backend::ungrab_pointer` must present to end this
    /// grab, from the ledger's shared id counter: unique for the life
    /// of the session, so a stale handle — the launcher strip hands one
    /// back for a press whose release never arrived — cannot cancel the
    /// drag that came after it.
    handle: DragHandle,
    /// Where this drag's pointer events go, latched on the first one
    /// rather than passed in. The verb takes no argument (X11 needed
    /// none — the server already knew which window the press was on),
    /// and the honest answer is the target the press that started the
    /// drag already pinned, which is the implicit grab's. Latched once,
    /// so it outlives that grab: the release that ends the drag is the
    /// event that clears the implicit grab and the one that most needs
    /// somewhere to be sent.
    target: Option<PressTarget>,
}

impl DragGrab {
    pub(crate) fn new(handle: DragHandle) -> Self {
        DragGrab { handle, target: None }
    }

    /// Whether `handle` names this grab, and so may end it.
    pub(crate) fn holds(&self, handle: DragHandle) -> bool {
        self.handle == handle
    }

    /// Where this drag's events are going, if a pointer event has
    /// latched it yet — read by [`pointer_subject`] so the renderer
    /// can keep a frame drag's resize cursor up while the drag runs.
    pub(crate) fn target(&self) -> Option<PressTarget> {
        self.target
    }

    /// This drag's routing target, latching it on first use from the
    /// implicit grab the press left behind — or, for a drag that
    /// somehow began with no button down, from whatever is under the
    /// pointer, which is the same answer that press would have given.
    fn anchor(&mut self, implicit: Option<PressTarget>, hit: &Hit) -> PressTarget {
        *self.target.get_or_insert_with(|| implicit.unwrap_or_else(|| press_target(hit)))
    }

    /// Whether the surface this drag anchored on still exists. A client
    /// that dies mid-drag takes its record with it, and a drag with
    /// nothing left to drag is over however firmly the user is still
    /// holding the button.
    fn anchor_alive(&self, backend: &WaylandBackend) -> bool {
        match self.target {
            // Nothing latched: no pointer event has reached this grab
            // yet, so there is nothing that could have died under it.
            None => true,
            Some(PressTarget::Shell(shell)) => backend.shells.contains_key(&shell),
            Some(PressTarget::Frame(frame)) => backend.frames.contains_key(&frame),
            Some(PressTarget::Content(window)) => backend.windows.contains_key(&window),
            Some(PressTarget::Layer(layer)) => backend.layers.iter().any(|record| record.id == layer),
            Some(PressTarget::Ime) => backend.ime_popups.iter().any(|popup| popup.alive()),
            // The desktop background outlives every drag made on it.
            Some(PressTarget::Root) => true,
        }
    }

    /// Whether this grab has outlived the drag it was taken for.
    ///
    /// An interactive drag exists only while a mouse button is held, so
    /// once the last one lifts the drag is over whatever its owner
    /// still believes — see [`reclaim_leaked_grab`], which is the only
    /// caller and explains why the question is asked at all.
    fn expired(&self, buttons_held: bool, anchor_alive: bool) -> bool {
        !buttons_held || !anchor_alive
    }
}

/// Where a pointer event is routed, decided before the hit under the
/// pointer gets a say.
#[derive(Clone, Copy)]
struct Route {
    /// The pinned target: a drag's anchor, else the implicit grab's.
    /// `None` only when neither holds — no drag, no button down — and
    /// the hit under the pointer decides for itself.
    target: Option<PressTarget>,
    /// Whether it was a drag that pinned it. The difference is what
    /// happens to client content under the pointer: an implicit grab on
    /// a client's own window is that client's drag and its events are
    /// the client's, where a drag grab means the window manager is
    /// moving the window and the client must be told nothing at all.
    dragging: bool,
}

/// Drops any implicit pointer grab, because the presses that built it
/// can no longer be trusted to have matching releases.
///
/// Called when the seat is handed back to this session: a button held
/// while the user switched away releases into whoever owns the seat
/// now, so this session would wait forever for an event that is not
/// coming and keep routing every click to the grab's stale target.
pub(crate) fn clear_implicit_grab(seat: &Seat<Compositor>) {
    with_input(seat, |input| {
        input.implicit_grab = None;
        // The swallow list goes with it, and for the same reason: a
        // button held down through a VT switch releases into whoever
        // owns the seat now, so an entry left here would silently eat
        // the next release of that button in this session.
        input.grab_dismissals.clear();
    });
}

/// Reconciles every held-input state after the real session regains its
/// seat.
///
/// Releases that happen while another VT owns the seat are deliberately
/// not replayed by libinput. That leaves both our intercepted-key ledger
/// and Smithay's xkb state believing keys are still down forever. Clear
/// the compositor-owned state, release Smithay's pressed keys without
/// running bindings, and synthesize releases only for presses that were
/// originally forwarded to the focused client. A swallowed shortcut must
/// not turn into a client-visible release merely because a VT switch
/// interrupted it.
pub(crate) fn resynchronise_input_after_resume(state: &mut Compositor) {
    let seat = state.seat.clone();
    let suppressed_keys = with_input(&seat, reset_resume_bookkeeping);

    if let Some(keyboard) = seat.get_keyboard() {
        let time = state.start_time.elapsed().as_millis() as u32;
        release_stale_pressed_keys(state, &keyboard, &suppressed_keys, time);
    }

    // Alt-Tab takes an explicit modal grab and wm-core gives it back on
    // Alt's release. If that release went to the other VT, neither side
    // can otherwise recover: future keys stay swallowed and the cycle
    // panel remains open. Drop exclusivity immediately, then give
    // wm-core the same release its ordinary input path would have queued
    // so it can commit and retire the cycle session on this dispatch.
    reset_modal_keyboard_grab(state.wm.backend_mut());
}

fn reset_modal_keyboard_grab(backend: &mut WaylandBackend) {
    if std::mem::take(&mut backend.keyboard_grabbed) {
        backend.queue(WmEvent::KeyRelease(KeyCombo {
            keysym: keysyms::KEY_Alt_L,
            modifiers: Modifiers::empty(),
        }));
    }
}

fn reset_resume_bookkeeping(input: &mut InputState) -> Vec<Keycode> {
    input.implicit_grab = None;
    input.grab_dismissals.clear();
    input.repeating = None;
    std::mem::take(&mut input.suppressed_keys)
}

/// Clears Smithay's physical-key and xkb state, then balances only the
/// client-visible presses.
///
/// `input_forward` alone is insufficient here: it sends protocol events
/// and updates Smithay's forwarded set, but does not update the physical
/// pressed-key set or xkb modifiers. The intercept pass does that without
/// re-entering production binding logic. The final modifier assignment
/// also drops latched/depressed state while preserving Caps Lock, Num
/// Lock, and the active layout.
fn release_stale_pressed_keys<D: SeatHandler + 'static>(
    data: &mut D,
    keyboard: &KeyboardHandle<D>,
    suppressed_keys: &[Keycode],
    time: u32,
) {
    let mut pressed_keys: Vec<_> = keyboard.pressed_keys().into_iter().collect();
    pressed_keys.sort_unstable();

    let mut modifiers_changed = false;
    for keycode in &pressed_keys {
        let ((), changed) = keyboard.input_intercept(data, *keycode, KeyState::Released, |_, _, _| ());
        modifiers_changed |= changed;
    }

    let old_modifiers = keyboard.modifier_state();
    let clean_modifiers = ModifiersState {
        caps_lock: old_modifiers.caps_lock,
        num_lock: old_modifiers.num_lock,
        ..ModifiersState::default()
    };
    modifiers_changed |= keyboard.set_modifier_state(clean_modifiers) != 0;

    for (index, keycode) in pressed_keys
        .into_iter()
        .filter(|keycode| !suppressed_keys.contains(keycode))
        .enumerate()
    {
        keyboard.input_forward(
            data,
            keycode,
            KeyState::Released,
            SERIAL_COUNTER.next_serial(),
            time,
            modifiers_changed && index == 0,
        );
    }
}

/// Re-homes client input when the lock boundary changes.
///
/// Smithay's pointer has its own click grab in addition to the routing
/// grab above. A button held as the lock lands would otherwise pin the
/// seat to the pre-lock client even after a motion naming `None`, so
/// that grab is explicitly broken before focus is recomputed. Tablet
/// tools need the equivalent `proximity_out`; Smithay exposes lookup
/// by descriptor but not iteration, hence the active set in
/// [`InputState`]. This is called on both lock and unlock so neither
/// domain inherits focus or a held tip from the other.
pub(crate) fn reset_client_input_focus(state: &mut Compositor) {
    let seat = state.seat.clone();
    let time = state.start_time.elapsed().as_millis() as u32;
    let tools = with_input(&seat, |input| std::mem::take(&mut input.active_tablet_tools));
    let tablet_seat = seat.tablet_seat();
    for descriptor in tools {
        if let Some(tool) = tablet_seat.get_tool(&descriptor) {
            tool.proximity_out(time);
        }
    }
    if let Some(pointer) = seat.get_pointer() {
        pointer.unset_grab(state, SERIAL_COUNTER.next_serial(), time);
    }
    sync_pointer_focus(state);
}

/// Makes the seat's pointer focus agree with the current scene domain
/// without waiting for physical motion. While locked, `hit_at` can
/// return only a lock surface or the root; while unlocked it returns
/// the ordinary scene beneath the saved pointer location.
pub(crate) fn sync_pointer_focus(state: &mut Compositor) {
    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };
    let position = state.pointer_location;
    let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
    let focus = client_focus(&hit_at(state.wm.backend(), at, position));
    let event = MotionEvent {
        location: position,
        serial: SERIAL_COUNTER.next_serial(),
        time: state.start_time.elapsed().as_millis() as u32,
    };
    pointer.motion(state, focus, &event);
    pointer.frame(state);
}

fn remember_tablet_tool(seat: &Seat<Compositor>, descriptor: TabletToolDescriptor) {
    with_input(seat, |input| {
        input.active_tablet_tools.insert(descriptor);
    });
}

fn forget_tablet_tool(seat: &Seat<Compositor>, descriptor: &TabletToolDescriptor) {
    with_input(seat, |input| {
        input.active_tablet_tools.remove(descriptor);
    });
}

/// Runs `f` against the seat's [`InputState`], creating it on first
/// use. Callers keep each access short — never across a seat call that
/// re-enters the `Compositor` handlers — which the closure shape makes
/// structural rather than a discipline.
fn with_input<T>(seat: &Seat<Compositor>, f: impl FnOnce(&mut InputState) -> T) -> T {
    let user_data = seat.user_data();
    user_data.insert_if_missing(|| RefCell::new(InputState::default()));
    let cell = user_data.get::<RefCell<InputState>>().unwrap();
    f(&mut cell.borrow_mut())
}

/// Where this pointer event goes, and who decided: a drag grab outranks
/// the implicit grab, which outranks the hit under the pointer. Latches
/// the drag's anchor on the way past, which is why it takes the ledger
/// mutably.
fn resolve_route(backend: &mut WaylandBackend, seat: &Seat<Compositor>, hit: &Hit) -> Route {
    let implicit = with_input(seat, |input| input.implicit_grab.as_ref().map(|grab| grab.target));
    match backend.pointer_grab.as_mut() {
        Some(drag) => Route { target: Some(drag.anchor(implicit, hit)), dragging: true },
        None => Route { target: implicit, dragging: false },
    }
}

/// Takes back a drag grab whose drag is already over, and tells
/// `wm-core` the drag ended.
///
/// This is the safety net the whole mechanism needs rather than a
/// tidiness pass, because the failure it prevents is worse than the one
/// the grab fixes: a grab nobody gives back routes every pointer event
/// into a drag that finished, so no client can be clicked, no menu
/// opens, and a window follows the cursor forever. There is no way out
/// of that but killing the session.
///
/// So the compositor never depends on the grab's owner remembering. It
/// does not have to guess either: a drag exists only while a mouse
/// button is held (and only while the surface it anchored on exists),
/// both of which this module already knows, so a grab still held on an
/// event where neither is true has leaked. In the ordinary case there
/// is nothing to find — `wm-core` and the shell release the pointer in
/// the same dispatch pass as the release that ended their drag, before
/// the next event arrives — which is what makes this a detector and not
/// part of the flow.
///
/// [`BackendEvent::DragEnded`] goes with it: whatever left the grab
/// behind is, by definition, something that has not noticed its drag is
/// over, and a `wm-core` still holding `active_move` would carry on
/// moving the window on the next motion even with the pointer handed
/// back. Its handler is idempotent, so saying so costs nothing when the
/// owner was the shell and no core drag existed.
fn reclaim_leaked_grab(state: &mut Compositor, seat: &Seat<Compositor>) {
    let buttons_held = with_input(seat, |input| input.implicit_grab.is_some());
    let backend = state.wm.backend_mut();
    let leaked =
        backend.pointer_grab.as_ref().is_some_and(|grab| grab.expired(buttons_held, grab.anchor_alive(backend)));
    if !leaked {
        return;
    }
    tracing::debug!(buttons_held, "reclaiming a pointer grab its drag outlived");
    backend.end_pointer_grab();
    backend.queue(WmEvent::DragEnded);
}

/// Applies a pointer-grab transition the ledger recorded, in the one
/// place that can reach the seat (see
/// [`WaylandBackend::pending_pointer_grab`] for why it is recorded
/// rather than done where it is decided).
///
/// Both halves are about the client under the pointer, which is the
/// half of a drag grab that routing alone cannot deliver.
pub(crate) fn apply_pointer_grab_change(state: &mut Compositor) {
    let Some(change) = state.wm.backend_mut().pending_pointer_grab.take() else {
        return;
    };
    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };
    let serial = SERIAL_COUNTER.next_serial();
    let time = state.start_time.elapsed().as_millis() as u32;
    let location = state.pointer_location;
    match change {
        PointerGrabChange::Taken => {
            // The press that started the drag installed smithay's own
            // click grab, which pins the seat's focus to the surface it
            // landed on until the button comes up — through every
            // motion this drag is made of. Dropping it is what lets the
            // leave below reach the client at all; without it the
            // window manager moves the window while the client goes on
            // receiving the same gesture as its own, which is the
            // "mouse got stuck" half of the report: LibreOffice kept
            // its titlebar drag running under ours.
            pointer.unset_grab(state, serial, time);
            // And one leave, now rather than whenever the pointer next
            // moves, because a drag can be several seconds of a client
            // believing the pointer is inside it.
            pointer.motion(state, None, &MotionEvent { location, serial, time });
            pointer.frame(state);
        }
        PointerGrabChange::Released => {
            // Delivery resumes, and the client under the pointer has to
            // be told: it was sent a leave when the drag began, and
            // nothing else would send it an enter until the user moved
            // the pointer again. A drag that ends with the cursor
            // sitting still over a window — which is every drag that
            // ends where it meant to — would otherwise leave that
            // window unable to report so much as a hover.
            let at = Point::new(location.x.floor() as i32, location.y.floor() as i32);
            let focus = match hit_at(state.wm.backend(), at, location) {
                Hit::Content { surface: Some(surface), origin, .. }
                | Hit::Lock { surface, origin } => Some((surface, origin)),
                _ => None,
            };
            pointer.motion(state, focus, &MotionEvent { location, serial, time });
            pointer.frame(state);
        }
    }
}

/// The policy-relevant family of one Smithay input event.
///
/// [`InputEvent`] is matched exhaustively here rather than through a
/// wildcard at each policy site. When Smithay grows another event
/// variant, this function stops compiling until its idle and lock
/// behavior are stated alongside every family already handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputFamily {
    DeviceLifecycle,
    Keyboard,
    PointerMotion,
    PointerButton,
    PointerAxis,
    Gesture,
    Touch,
    TabletTool,
    Switch,
    Special,
}

/// How a family is kept on the safe side of the session-lock boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LockedInputRoute {
    /// This family is lifecycle data or is not delivered to clients.
    NoClientDelivery,
    /// The keyboard handler filters bindings and uses only its explicit
    /// lock-surface focus.
    KeyboardFilter,
    /// Motion has a dedicated early branch that calls `lock_hit`.
    PointerMotionHandler,
    /// Buttons, axes and gestures use the seat's pointer focus, so it
    /// must be re-asserted against `lock_hit` before delivery.
    PointerFocus,
    /// Tablet motion resolves through the lock-aware scene hit-test;
    /// buttons use the tool focus that motion established or no focus.
    TabletHitTest,
}

impl InputFamily {
    fn of<B: InputBackend>(event: &InputEvent<B>) -> Self {
        match event {
            InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } => {
                Self::DeviceLifecycle
            }
            InputEvent::Keyboard { .. } => Self::Keyboard,
            InputEvent::PointerMotion { .. } | InputEvent::PointerMotionAbsolute { .. } => {
                Self::PointerMotion
            }
            InputEvent::PointerButton { .. } => Self::PointerButton,
            InputEvent::PointerAxis { .. } => Self::PointerAxis,
            InputEvent::GestureSwipeBegin { .. }
            | InputEvent::GestureSwipeUpdate { .. }
            | InputEvent::GestureSwipeEnd { .. }
            | InputEvent::GesturePinchBegin { .. }
            | InputEvent::GesturePinchUpdate { .. }
            | InputEvent::GesturePinchEnd { .. }
            | InputEvent::GestureHoldBegin { .. }
            | InputEvent::GestureHoldEnd { .. } => Self::Gesture,
            InputEvent::TouchDown { .. }
            | InputEvent::TouchMotion { .. }
            | InputEvent::TouchUp { .. }
            | InputEvent::TouchCancel { .. }
            | InputEvent::TouchFrame { .. } => Self::Touch,
            InputEvent::TabletToolAxis { .. }
            | InputEvent::TabletToolProximity { .. }
            | InputEvent::TabletToolTip { .. }
            | InputEvent::TabletToolButton { .. } => Self::TabletTool,
            InputEvent::SwitchToggle { .. } => Self::Switch,
            InputEvent::Special(_) => Self::Special,
        }
    }

    fn resets_idle(self) -> bool {
        matches!(
            self,
            Self::Keyboard
                | Self::PointerMotion
                | Self::PointerButton
                | Self::PointerAxis
                | Self::Gesture
                | Self::TabletTool
        )
    }

    fn locked_route(self) -> LockedInputRoute {
        match self {
            Self::DeviceLifecycle | Self::Touch | Self::Switch | Self::Special => {
                LockedInputRoute::NoClientDelivery
            }
            Self::Keyboard => LockedInputRoute::KeyboardFilter,
            Self::PointerMotion => LockedInputRoute::PointerMotionHandler,
            Self::PointerButton | Self::PointerAxis | Self::Gesture => {
                LockedInputRoute::PointerFocus
            }
            Self::TabletTool => LockedInputRoute::TabletHitTest,
        }
    }
}

/// One entry point for `run()`'s event loop: translate and route a
/// single input event. Generic over the smithay input backend so the
/// winit dev loop and a future libinput session share every line of
/// routing policy — only the raw event types differ.
pub(crate) fn process_input_event<I: InputBackend>(state: &mut Compositor, event: InputEvent<I>) {
    let family = InputFamily::of(&event);
    // Every input event this compositor routes is user activity to the
    // idle timers, decided here at the one funnel both backends share.
    // The exhaustive family classifier and its table-driven test make
    // a newly routed family state that policy before it can compile.
    if family.resets_idle() {
        crate::idle::note_activity(state);
    }
    // Buttons, scroll and gestures are delivered against the seat's
    // existing pointer focus. Re-assert the locked domain before any
    // of them can use it: this covers the instant a lock lands and
    // also a lock surface being replaced without physical motion.
    if state.wm.backend().locked && family.locked_route() == LockedInputRoute::PointerFocus {
        sync_pointer_focus(state);
    }
    match event {
        InputEvent::DeviceAdded { device } => {
            state.mark_hyprland_state_dirty();
            let record = crate::state::InputDeviceRecord {
                id: device.id(),
                name: device.name(),
                keyboard: device.has_capability(DeviceCapability::Keyboard),
                pointer: device.has_capability(DeviceCapability::Pointer),
                touch: device.has_capability(DeviceCapability::Touch),
                tablet: device.has_capability(DeviceCapability::TabletTool)
                    || device.has_capability(DeviceCapability::TabletPad),
                switch: device.has_capability(DeviceCapability::Switch),
            };
            let devices = &mut state.wm.backend_mut().input_devices;
            devices.retain(|held| held.id != record.id);
            devices.push(record);
            if device.has_capability(DeviceCapability::TabletTool) {
                state
                    .seat
                    .tablet_seat()
                    .add_tablet::<Compositor>(&state.display_handle, &TabletDescriptor::from(&device));
            }
        }
        InputEvent::DeviceRemoved { device } => {
            state.mark_hyprland_state_dirty();
            state.wm.backend_mut().input_devices.retain(|held| held.id != device.id());
            if device.has_capability(DeviceCapability::TabletTool) {
                state.seat.tablet_seat().remove_tablet(&TabletDescriptor::from(&device));
            }
        }
        InputEvent::Keyboard { event } => on_keyboard_key::<I>(state, event),
        InputEvent::PointerMotionAbsolute { event } => on_pointer_move_absolute::<I>(state, event),
        InputEvent::PointerMotion { event } => on_pointer_move_relative::<I>(state, event),
        InputEvent::PointerButton { event } => on_pointer_button::<I>(state, event),
        InputEvent::PointerAxis { event } => on_pointer_axis::<I>(state, event),
        InputEvent::GestureSwipeBegin { event } => {
            if let Some(pointer) = state.seat.get_pointer() {
                pointer.gesture_swipe_begin(
                    state,
                    &GestureSwipeBeginEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
            }
        }
        InputEvent::GestureSwipeUpdate { event } => {
            if let Some(pointer) = state.seat.get_pointer() {
                pointer.gesture_swipe_update(
                    state,
                    &GestureSwipeUpdateEvent { time: event.time_msec(), delta: event.delta() },
                );
            }
        }
        InputEvent::GestureSwipeEnd { event } => {
            if let Some(pointer) = state.seat.get_pointer() {
                pointer.gesture_swipe_end(
                    state,
                    &GestureSwipeEndEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
        }
        InputEvent::GesturePinchBegin { event } => {
            if let Some(pointer) = state.seat.get_pointer() {
                pointer.gesture_pinch_begin(
                    state,
                    &GesturePinchBeginEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
            }
        }
        InputEvent::GesturePinchUpdate { event } => {
            if let Some(pointer) = state.seat.get_pointer() {
                pointer.gesture_pinch_update(
                    state,
                    &GesturePinchUpdateEvent {
                        time: event.time_msec(),
                        delta: event.delta(),
                        scale: event.scale(),
                        rotation: event.rotation(),
                    },
                );
            }
        }
        InputEvent::GesturePinchEnd { event } => {
            if let Some(pointer) = state.seat.get_pointer() {
                pointer.gesture_pinch_end(
                    state,
                    &GesturePinchEndEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
        }
        InputEvent::GestureHoldBegin { event } => {
            if let Some(pointer) = state.seat.get_pointer() {
                pointer.gesture_hold_begin(
                    state,
                    &GestureHoldBeginEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
            }
        }
        InputEvent::GestureHoldEnd { event } => {
            if let Some(pointer) = state.seat.get_pointer() {
                pointer.gesture_hold_end(
                    state,
                    &GestureHoldEndEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
        }
        InputEvent::TabletToolAxis { event } => on_tablet_axis::<I>(state, event),
        InputEvent::TabletToolProximity { event } => on_tablet_proximity::<I>(state, event),
        InputEvent::TabletToolTip { event } => on_tablet_tip::<I>(state, event),
        InputEvent::TabletToolButton { event } => on_tablet_button::<I>(state, event),
        // Touch and switch devices remain represented in the device
        // registry above; their protocol/event policy is independent.
        InputEvent::TouchDown { .. }
        | InputEvent::TouchMotion { .. }
        | InputEvent::TouchUp { .. }
        | InputEvent::TouchCancel { .. }
        | InputEvent::TouchFrame { .. }
        | InputEvent::SwitchToggle { .. }
        | InputEvent::Special(_) => {}
    }
}

// -- tablet tools -------------------------------------------------------

fn tablet_handles<I: InputBackend, E: TabletToolEvent<I>>(
    state: &mut Compositor,
    event: &E,
    descriptor: &TabletToolDescriptor,
) -> (
    smithay::wayland::tablet_manager::TabletHandle,
    smithay::wayland::tablet_manager::TabletToolHandle,
) {
    let seat = state.seat.clone();
    let tablet_seat = seat.tablet_seat();
    let device = event.device();
    let tablet_desc = TabletDescriptor::from(&device);
    let display = state.display_handle.clone();
    let tablet = tablet_seat
        .get_tablet(&tablet_desc)
        .unwrap_or_else(|| tablet_seat.add_tablet::<Compositor>(&display, &tablet_desc));
    let tool = tablet_seat.add_tool::<Compositor>(state, &display, descriptor);
    (tablet, tool)
}

fn tablet_position<I: InputBackend, E: TabletToolEvent<I>>(state: &Compositor, event: &E) -> LogicalPoint<f64, Logical> {
    let size = state.wm.backend().output_size;
    event.position_transformed((size.w as i32, size.h as i32).into())
}

fn tablet_focus(
    backend: &WaylandBackend,
    position: LogicalPoint<f64, Logical>,
) -> Option<(WlSurface, LogicalPoint<f64, Logical>)> {
    let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
    client_focus(&hit_at(backend, at, position))
}

/// The seat focus represented by a scene hit. Kept as one adapter so
/// pointer re-synchronisation and tablet motion cannot disagree about
/// which client-owned surface a `Hit` names.
fn client_focus(hit: &Hit) -> Option<(WlSurface, LogicalPoint<f64, Logical>)> {
    match hit {
        Hit::Content { surface: Some(surface), origin, .. }
        | Hit::Layer { surface, origin, .. }
        | Hit::Ime { surface, origin }
        | Hit::Lock { surface, origin } => Some((surface.clone(), *origin)),
        _ => None,
    }
}

fn queue_tablet_axes<I: InputBackend, E: TabletToolEvent<I>>(
    tool: &smithay::wayland::tablet_manager::TabletToolHandle,
    event: &E,
) {
    if event.pressure_has_changed() {
        tool.pressure(event.pressure());
    }
    if event.distance_has_changed() {
        tool.distance(event.distance());
    }
    if event.tilt_has_changed() {
        tool.tilt(event.tilt());
    }
    if event.rotation_has_changed() {
        tool.rotation(event.rotation());
    }
    if event.slider_has_changed() {
        tool.slider_position(event.slider_position());
    }
    if event.wheel_has_changed() {
        tool.wheel(event.wheel_delta(), event.wheel_delta_discrete());
    }
}

fn on_tablet_axis<I: InputBackend>(state: &mut Compositor, event: I::TabletToolAxisEvent) {
    let position = tablet_position::<I, _>(state, &event);
    let descriptor = event.tool();
    let (tablet, tool) = tablet_handles::<I, _>(state, &event, &descriptor);
    remember_tablet_tool(&state.seat, descriptor);
    queue_tablet_axes::<I, _>(&tool, &event);
    let focus = tablet_focus(state.wm.backend(), position);
    tool.motion(position, focus, &tablet, SERIAL_COUNTER.next_serial(), event.time_msec());
    state.wm.backend_mut().mark_damaged();
}

fn on_tablet_proximity<I: InputBackend>(state: &mut Compositor, event: I::TabletToolProximityEvent) {
    let position = tablet_position::<I, _>(state, &event);
    let descriptor = event.tool();
    let (tablet, tool) = tablet_handles::<I, _>(state, &event, &descriptor);
    queue_tablet_axes::<I, _>(&tool, &event);
    match event.state() {
        ProximityState::In => {
            remember_tablet_tool(&state.seat, descriptor);
            let focus = tablet_focus(state.wm.backend(), position);
            tool.motion(position, focus, &tablet, SERIAL_COUNTER.next_serial(), event.time_msec());
        }
        ProximityState::Out => {
            tool.proximity_out(event.time_msec());
            forget_tablet_tool(&state.seat, &descriptor);
        }
    }
    state.wm.backend_mut().mark_damaged();
}

fn on_tablet_tip<I: InputBackend>(state: &mut Compositor, event: I::TabletToolTipEvent) {
    let position = tablet_position::<I, _>(state, &event);
    let descriptor = event.tool();
    let (tablet, tool) = tablet_handles::<I, _>(state, &event, &descriptor);
    remember_tablet_tool(&state.seat, descriptor);
    queue_tablet_axes::<I, _>(&tool, &event);
    let focus = tablet_focus(state.wm.backend(), position);
    let serial = SERIAL_COUNTER.next_serial();
    tool.motion(position, focus, &tablet, serial, event.time_msec());
    match event.tip_state() {
        TabletToolTipState::Down => tool.tip_down(serial, event.time_msec()),
        TabletToolTipState::Up => tool.tip_up(event.time_msec()),
    }
    state.wm.backend_mut().mark_damaged();
}

fn on_tablet_button<I: InputBackend>(state: &mut Compositor, event: I::TabletToolButtonEvent) {
    let descriptor = event.tool();
    let (_, tool) = tablet_handles::<I, _>(state, &event, &descriptor);
    remember_tablet_tool(&state.seat, descriptor);
    tool.button(event.button(), event.button_state(), SERIAL_COUNTER.next_serial(), event.time_msec());
}

// -- keyboard ------------------------------------------------------------

/// Runs every key through the seat keyboard's xkb state (which owns the
/// keymap, modifier tracking, and client delivery) with a filter that
/// implements the WM's grab contract:
///
/// - a pressed combo matching `grabbed_combos` — or ANY press while the
///   modal `keyboard_grabbed` flag is set (Alt-Tab) — becomes
///   `BackendEvent::KeyPress` and never reaches a client;
/// - releases are forwarded to clients normally, except releases of
///   keys whose press was intercepted (see `suppressed_keys`); while
///   `keyboard_grabbed`, every release ADDITIONALLY queues
///   `BackendEvent::KeyRelease` — the Alt release is what commits an
///   Alt-Tab cycle (see `wm-x11`'s KeyRelease comment; same contract).
///
/// Going through `KeyboardHandle::input` rather than writing
/// `wl_keyboard.key` directly is also what puts the session's real
/// keymap back after a virtual keyboard has swapped its own in — see
/// `crate::virtual_keyboard`. Route physical keys around this function
/// and the symptom is the user's own keyboard typing nonsense in the
/// window `wtype` last touched.
fn on_keyboard_key<I: InputBackend>(state: &mut Compositor, event: I::KeyboardKeyEvent) {
    let keycode = event.key_code();
    let key_state = event.state();
    let serial = SERIAL_COUNTER.next_serial();
    let time = event.time_msec();
    let Some(keyboard) = state.seat.get_keyboard() else {
        return;
    };
    let seat = state.seat.clone();
    let shortcuts_inhibited = seat.keyboard_shortcuts_inhibited();
    keyboard.input::<(), _>(state, keycode, key_state, serial, time, |data, mods, handle| {
        // Level-0 (unshifted) keysym, exactly like `wm-x11`'s
        // `keysym_for_keycode` taking the keycode's first sym: a combo
        // bound as Alt+Shift+T must match the T key with SHIFT in the
        // modifier mask, not the keysym 'T' that shift-modified lookup
        // would produce (and Shift+Tab must stay XK_Tab, not
        // ISO_Left_Tab — `wm-core`'s cycle-backwards match depends on
        // it). The latin fallback keeps bindings working on non-latin
        // layouts.
        let keysym = handle.raw_latin_sym_or_raw_current_sym().unwrap_or_else(|| handle.modified_sym());
        let combo = KeyCombo { keysym: keysym.raw(), modifiers: combo_modifiers(mods) };
        match key_state {
            KeyState::Pressed => {
                // VT switching outranks every other binding, including
                // the modal keyboard grab: a user who logged into
                // chonkstep from a TTY has no other way back to one, and
                // a compositor that can wedge the machine is a trap. The
                // seat handle lives on the session backend, reachable
                // only through `&mut Compositor` from in here — see
                // `session::change_vt`, which is also what decides that
                // a nested session should forward these combos instead
                // (it owns no seat to switch).
                if let Some(vt) = vt_switch_target(mods, keysym, handle.modified_sym()) {
                    if crate::session::change_vt(data, vt) {
                        // Suppress the matching release too: the client
                        // never saw the press, and after a successful
                        // switch the release is usually delivered to
                        // whoever owns the VT now anyway.
                        with_input(&seat, |input| input.suppressed_keys.push(keycode));
                        return FilterResult::Intercept(());
                    }
                }
                let backend = data.wm.backend_mut();
                // Under a session lock every key belongs to the lock
                // surface — WM keybindings, the modal grab, all of it
                // stands aside (only the VT switch above outranks the
                // lock, deliberately: it is the user's escape hatch on
                // real hardware and the spec leaves it to the
                // compositor). Intercepting a bound combo here would
                // both swallow a password character and run a desktop
                // action behind the lock.
                let works_locked = backend.locked_combos.contains(&combo);
                if backend.locked && !works_locked {
                    return FilterResult::Forward;
                }
                if shortcuts_inhibited {
                    return FilterResult::Forward;
                }
                if backend.keyboard_grabbed || backend.grabbed_combos.contains(&combo) {
                    if !backend.release_combos.contains(&combo) {
                        backend.queue(WmEvent::KeyPress(combo));
                    }
                    let repeating = backend.repeating_combos.contains(&combo);
                    let delay = backend.repeat_delay;
                    let rate = backend.repeat_rate.max(1);
                    with_input(&seat, |input| {
                        input.suppressed_keys.push(keycode);
                        if repeating {
                            input.repeating = Some(RepeatingKey {
                                keycode,
                                combo,
                                next: std::time::Instant::now() + delay,
                                interval: std::time::Duration::from_secs_f64(1.0 / rate as f64),
                                emitted: 0,
                            });
                        }
                    });
                    FilterResult::Intercept(())
                } else {
                    FilterResult::Forward
                }
            }
            KeyState::Released => {
                let backend = data.wm.backend_mut();
                // The suppressed-release bookkeeping below still runs
                // while locked (a combo pressed before the lock landed
                // owes its release-swallow either way); only the modal
                // grab's KeyRelease stream stops.
                if backend.keyboard_grabbed && !backend.locked {
                    backend.queue(WmEvent::KeyRelease(combo));
                }
                if backend.release_combos.contains(&combo)
                    && (!backend.locked || backend.locked_combos.contains(&combo))
                {
                    backend.queue(WmEvent::KeyRelease(combo));
                }
                let suppressed = with_input(&seat, |input| {
                    if input.repeating.is_some_and(|repeat| repeat.keycode == keycode) {
                        input.repeating = None;
                    }
                    match input.suppressed_keys.iter().position(|k| *k == keycode) {
                        Some(index) => {
                            input.suppressed_keys.swap_remove(index);
                            true
                        }
                        None => false,
                    }
                });
                if suppressed {
                    FilterResult::Intercept(())
                } else {
                    FilterResult::Forward
                }
            }
        }
    });
}

/// Queues due compositor-side repeats. Called once per event-loop
/// dispatch; catches up a bounded number after a stalled frame so a
/// resumed desktop cannot emit an unbounded burst.
pub(crate) fn tick_repeating_binding(state: &mut Compositor) {
    let seat = state.seat.clone();
    let now = std::time::Instant::now();
    let mut due = None;
    with_input(&seat, |input| {
        let Some(repeat) = input.repeating.as_mut() else {
            return;
        };
        let mut count = 0u8;
        for _ in 0..4 {
            if repeat.next > now {
                break;
            }
            count += 1;
            repeat.next += repeat.interval;
        }
        if repeat.next <= now {
            repeat.next = now + repeat.interval;
        }
        if count != 0 {
            repeat.emitted = repeat.emitted.saturating_add(u64::from(count));
            due = Some((repeat.combo, count));
        }
    });
    if let Some((combo, count)) = due {
        for _ in 0..count {
            state.wm.backend_mut().queue(WmEvent::KeyPress(combo));
        }
    }
}

/// Diagnostic state for the currently held compositor-owned binding:
/// emitted repeats and the configured interval. The production loop
/// never calls this; the opt-in test door uses it to verify the live
/// scheduler without timing external programs.
pub(crate) fn repeating_binding_status(state: &Compositor) -> Option<(u64, std::time::Duration)> {
    let seat = state.seat.clone();
    with_input(&seat, |input| input.repeating.map(|repeat| (repeat.emitted, repeat.interval)))
}

/// When compositor-owned key repeat next becomes due. Client key repeat
/// is announced through `wl_keyboard` and belongs to the client; this is
/// only for a held `binde` binding intercepted by the compositor.
pub(crate) fn repeating_binding_deadline(state: &Compositor) -> Option<std::time::Instant> {
    let seat = state.seat.clone();
    with_input(&seat, |input| input.repeating.map(|repeat| repeat.next))
}

/// Which virtual terminal a press asks for, if any: 1-12, or `None`.
///
/// Both spellings of the gesture are accepted because which one arrives
/// depends on the keymap, not on the user:
///
/// - `XF86Switch_VT_n` is what a layout with the standard VT-switch
///   mapping produces for Ctrl+Alt+Fn, and it only appears in the
///   *modified* keysym — the modifiers are already baked into it, so no
///   modifier test applies.
/// - Plain `Fn` with Ctrl+Alt held is the fallback for keymaps that
///   never map the XF86 symbols (a bare `us` layout loaded without the
///   `srvrkeys` rules, which is what a minimal TTY session often gets).
///   This is tested against the *raw* keysym for the same reason
///   bindings are: the level-0 symbol is stable under modifiers.
fn vt_switch_target(mods: &ModifiersState, raw: Keysym, modified: Keysym) -> Option<i32> {
    let modified = modified.raw();
    if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&modified) {
        return Some((modified - keysyms::KEY_XF86Switch_VT_1 + 1) as i32);
    }
    let raw = raw.raw();
    if mods.ctrl && mods.alt && (keysyms::KEY_F1..=keysyms::KEY_F12).contains(&raw) {
        return Some((raw - keysyms::KEY_F1 + 1) as i32);
    }
    None
}

/// xkb modifier state -> the backend-agnostic `Modifiers` `wm-core`
/// reasons about — the exact counterpart of `wm-x11`'s
/// `modifiers_from_state` (whose Mod1=Alt / Mod4=Super conventions xkb
/// simply names directly).
fn combo_modifiers(mods: &ModifiersState) -> Modifiers {
    let mut result = Modifiers::empty();
    if mods.shift {
        result |= Modifiers::SHIFT;
    }
    if mods.ctrl {
        result |= Modifiers::CONTROL;
    }
    if mods.alt {
        result |= Modifiers::ALT;
    }
    if mods.logo {
        result |= Modifiers::SUPER;
    }
    result
}

// -- pointer motion ------------------------------------------------------

/// Winit reports the pointer absolutely against the host window;
/// transform to output space and route.
///
/// The transform is against the *whole* global space (every monitor's
/// union), not one screen: for the nested backend those are the same
/// rectangle, and for an absolute device on a real session — a
/// touchscreen, a tablet — spanning the desktop is what an unconfigured
/// one does everywhere. Mapping a tablet to a single output is a
/// libinput device-configuration feature this session does not read
/// yet.
fn on_pointer_move_absolute<I: InputBackend>(state: &mut Compositor, event: I::PointerMotionAbsoluteEvent) {
    let size = state.wm.backend().output_size;
    let position = event.position_transformed((size.w as i32, size.h as i32).into());
    pointer_moved(state, position, event.time_msec(), None);
}

/// Relative motion (the libinput/session path): accumulate onto the
/// current location and confine the result to the outputs — the
/// compositor equivalent of the X server keeping the pointer on the
/// screen.
fn on_pointer_move_relative<I: InputBackend>(state: &mut Compositor, event: I::PointerMotionEvent) {
    let relative = RelativeMotionEvent {
        delta: event.delta(),
        delta_unaccel: event.delta_unaccel(),
        utime: event.time_msec() as u64 * 1_000,
    };
    let proposed = confine_to_outputs(&state.wm.backend().monitors, state.pointer_location + event.delta());
    let mut position = proposed;
    if let Some((pointer, surface)) = state
        .seat
        .get_pointer()
        .and_then(|pointer| pointer.current_focus().map(|surface| (pointer, surface)))
    {
        let current_origin = surface_focus_at(state.wm.backend(), state.pointer_location, &surface).map(|(_, origin)| origin);
        with_pointer_constraint(&surface, &pointer, |constraint| {
            let Some(constraint) = constraint else { return };
            if !constraint.is_active() {
                constraint.activate();
            }
            match &*constraint {
                PointerConstraint::Locked(_) => position = state.pointer_location,
                PointerConstraint::Confined(confined) => {
                    let inside = match (confined.region(), current_origin) {
                        (Some(region), Some(origin)) => region.contains((
                            (proposed.x - origin.x).floor() as i32,
                            (proposed.y - origin.y).floor() as i32,
                        )),
                        // With no explicit region the complete surface
                        // is the confinement region.
                        _ => surface_focus_at(state.wm.backend(), proposed, &surface).is_some(),
                    };
                    if !inside {
                        position = state.pointer_location;
                    }
                }
            }
        });
    }
    pointer_moved(state, position, event.time_msec(), Some(relative));
}

fn surface_focus_at(
    backend: &WaylandBackend,
    position: LogicalPoint<f64, Logical>,
    wanted: &WlSurface,
) -> Option<(WlSurface, LogicalPoint<f64, Logical>)> {
    let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
    match hit_at(backend, at, position) {
        Hit::Content { surface: Some(surface), origin, .. }
        | Hit::Layer { surface, origin, .. }
        | Hit::Ime { surface, origin }
        | Hit::Lock { surface, origin }
            if &surface == wanted => Some((surface, origin)),
        _ => None,
    }
}

/// Pulls a pointer position onto the nearest output.
///
/// With one monitor this is the plain clamp to its rectangle it has
/// always been. With several it cannot be a clamp to the union bounding
/// box: two monitors of different heights (or a future non-contiguous
/// layout) leave regions inside that box which no output covers, and a
/// pointer parked in one would be invisible — the cursor is composited
/// per output, so a point off every output is drawn nowhere — while
/// still hit-testing against whatever shell surface happens to extend
/// there. Projecting onto the nearest monitor instead keeps the pointer
/// somewhere the user can see it, and costs a distance comparison per
/// monitor on a list that is one or two entries long.
///
/// Note this only confines; it does not stop the pointer at a monitor
/// edge. A drag across the boundary passes straight through, which is
/// the behavior every desktop with a contiguous layout has.
fn confine_to_outputs(monitors: &[MonitorInfo], position: LogicalPoint<f64, Logical>) -> LogicalPoint<f64, Logical> {
    let mut best: Option<(f64, LogicalPoint<f64, Logical>)> = None;
    for monitor in monitors {
        let rect = monitor.geometry;
        // The far edge is the last pixel INSIDE the monitor, matching
        // `Rect::contains`'s half-open convention — a pointer at exactly
        // `pos.x + size.w` belongs to the next monitor, or to nothing.
        let x = position.x.clamp(rect.pos.x as f64, (rect.pos.x + rect.size.w.max(1) as i32 - 1) as f64);
        let y = position.y.clamp(rect.pos.y as f64, (rect.pos.y + rect.size.h.max(1) as i32 - 1) as f64);
        // Zero for a position already on this monitor, which is what
        // makes the common case fall out of the same comparison.
        let distance = (position.x - x).powi(2) + (position.y - y).powi(2);
        if best.as_ref().is_none_or(|(best_distance, _)| distance < *best_distance) {
            best = Some((distance, (x, y).into()));
        }
    }
    // No monitors at all is not a state a running session reaches (see
    // `Compositor::outputs`); leaving the position untouched is still
    // better than snapping it to the origin.
    best.map(|(_, point)| point).unwrap_or(position)
}

/// The shared motion path: hover/crossing bookkeeping, WM/shell queue
/// routing (honoring an implicit grab), then one seat `motion` +
/// `frame` so smithay's location tracking and client enter/leave stay
/// correct no matter where the event was routed.
fn pointer_moved(
    state: &mut Compositor,
    position: LogicalPoint<f64, Logical>,
    time: u32,
    relative: Option<RelativeMotionEvent>,
) {
    let serial = SERIAL_COUNTER.next_serial();
    // Floor, not round: a pointer at x=10.7 is over pixel 10, and
    // rounding at the output's far edge would name a pixel outside
    // every rect (`Rect::contains` is half-open).
    let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };
    let seat = state.seat.clone();
    // The renderer composites the cursor at this location itself (no
    // hardware cursor plane on the nested backend), so pointer motion
    // is scene damage by definition — without this the arrow freezes
    // between window updates.
    state.pointer_location = position;
    // Mirrored onto the ledger so `Backend::pointer_position` can
    // answer without reaching the `Compositor` — see that verb for the
    // stale-anchor bug the mirror exists to prevent.
    state.wm.backend_mut().pointer = Some(at);
    // A locked session's pointer exists only for the lock surfaces:
    // one seat motion against whichever covers the position, and none
    // of the routing below — no hover bookkeeping, no shell queues, no
    // WM events. The branch sits this early so no pre-lock grab state
    // can decide anything while the lock holds.
    if state.wm.backend().locked {
        state.wm.backend_mut().mark_damaged();
        let focus = lock_hit(state.wm.backend(), position);
        if let Some(relative) = &relative {
            pointer.relative_motion(state, focus.clone(), relative);
        }
        pointer.motion(state, focus, &MotionEvent { location: position, serial, time });
        pointer.frame(state);
        return;
    }
    // Before anything routes by it: a grab left behind by a drag that
    // is over must not decide where this event goes.
    reclaim_leaked_grab(state, &seat);
    let hit = hit_at(state.wm.backend(), at, position);
    let route = resolve_route(state.wm.backend_mut(), &seat, &hit);
    // A focus grab holds the pointer to its whitelist: motion outside
    // it moves the cursor and nothing else. No crossing, so
    // focus-follows-mouse cannot pull the keyboard off the popout; no
    // shell or `wm-core` queues, so the dock does not light up under a
    // pointer that is only passing over it; and `None` seat focus, so
    // the client the pointer left gets the `wl_pointer.leave` it needs
    // to drop a hover highlight. It deliberately does NOT clear the
    // grab — see `focus_grab.rs` on why a menu that closed when the
    // pointer left it would be unusable.
    //
    // Gated on nothing holding the pointer already (`route.target`):
    // a press that landed inside the popout and dragged out of it — a
    // slider, a scrollbar — is that client's own gesture and keeps
    // being delivered.
    if route.target.is_none() && grab_excludes(state, &hit) {
        state.wm.backend_mut().mark_damaged();
        pointer.motion(state, None, &MotionEvent { location: position, serial, time });
        pointer.frame(state);
        return;
    }

    // Enter/crossing detection, only while nothing holds the pointer —
    // X11 suppressed crossing events for the duration of a grab too
    // (the drag's grab mask never selected them), and focus-follows-
    // mouse mid-drag would focus every window a fast drag brushes.
    let now_hovered = match &hit {
        Hit::FrameChrome { frame, .. } => Some(SurfaceRef::Frame(*frame)),
        Hit::Content { frame: Some(frame), .. } => Some(SurfaceRef::Frame(*frame)),
        // Content with no frame is either a client-decorated managed
        // window — which must still report a crossing, or hovering it
        // would never focus it — or an override-redirect menu, which
        // has no client entry in `wm-core` for the enter to resolve to
        // and is dropped there harmlessly.
        Hit::Content { window, .. } => Some(SurfaceRef::Client(*window)),
        _ => None,
    };
    let entered = with_input(&seat, |input| {
        if route.dragging || input.implicit_grab.is_some() || now_hovered == input.hovered {
            None
        } else {
            input.hovered = now_hovered;
            now_hovered
        }
    });

    let backend = state.wm.backend_mut();
    backend.mark_damaged();
    if let Some(surface) = entered {
        backend.queue(WmEvent::PointerEnter { surface });
    }
    // The client focus this motion carries into the seat. Only content
    // routes focus a client; every other route clears it (generating
    // the wl_pointer.leave a client under the pointer's previous
    // position expects). During a non-content implicit grab the focus
    // is pinned to None so a WM drag never leaks motion into whatever
    // client windows it crosses — the X11 grab hid those too — and a
    // drag grab pins it to None over content as well, which is the one
    // place the two differ.
    let mut focus: Option<(WlSurface, LogicalPoint<f64, Logical>)> = None;
    match route.target {
        // A drag over client content, which is where a client-decorated
        // window's own titlebar drag spends its entire life: `wm-core`
        // gets the root coordinate every step of the move or resize is
        // made of, and the client gets nothing. Without this arm those
        // motions took the client branch below — the window manager
        // never heard about the drag it had been asked to run, and the
        // window only crept along when the pointer happened to cross
        // the desktop behind it. The frame and shell arms need no such
        // split: what they route to `wm-core` and the shell is already
        // what a drag on them wants.
        Some(PressTarget::Content(_)) if route.dragging => {
            backend.queue(WmEvent::PointerMotion { root: at, surface_local: None });
        }
        Some(PressTarget::Shell(shell)) => {
            if let Some(record) = backend.shells.get(&shell) {
                backend.shell_motions.push_back((shell, local_to(at, record.geometry.pos)));
            }
            backend.queue(WmEvent::PointerMotion { root: at, surface_local: None });
        }
        Some(PressTarget::Frame(frame)) => {
            let surface_local =
                backend.frames.get(&frame).map(|record| (SurfaceRef::Frame(frame), local_to(at, record.geometry.pos)));
            backend.queue(WmEvent::PointerMotion { root: at, surface_local });
        }
        Some(PressTarget::Content(_)) => {
            // The seat's own click grab pins the client-side focus; the
            // hit-test result is passed through untouched (smithay
            // ignores it while the grab holds, and needs it the instant
            // the grab ends).
            if let Hit::Content { surface: Some(surface), origin, .. } = &hit {
                focus = Some((surface.clone(), *origin));
            }
        }
        Some(PressTarget::Layer(_)) => {
            // A drag pinned on a layer surface is that client's own
            // gesture (a slider on an OSD): the seat's click grab is
            // already carrying it, so the hit under the pointer passes
            // through exactly as the `Content` arm's does.
            if let Hit::Layer { surface, origin, .. } = &hit {
                focus = Some((surface.clone(), *origin));
            }
        }
        Some(PressTarget::Ime) => {
            if let Hit::Ime { surface, origin } = &hit {
                focus = Some((surface.clone(), *origin));
            }
        }
        Some(PressTarget::Root) => {
            backend.queue(WmEvent::PointerMotion { root: at, surface_local: None });
        }
        None => match &hit {
            Hit::Shell { shell, local } => {
                // Both queues, from the one event, exactly like
                // `wm-x11`: the shell drains its surface-local motion
                // inside `on_motion`, which the loop only calls when a
                // root-coordinate `PointerMotion` arrives.
                backend.shell_motions.push_back((*shell, *local));
                backend.queue(WmEvent::PointerMotion { root: at, surface_local: None });
            }
            Hit::FrameChrome { frame, local } => {
                backend.queue(WmEvent::PointerMotion {
                    root: at,
                    surface_local: Some((SurfaceRef::Frame(*frame), *local)),
                });
            }
            Hit::Content { surface, origin, .. } => {
                // Client territory: the WM does not see idle motion
                // over client content on X11 (no motion mask there) and
                // it doesn't here either.
                if let Some(surface) = surface {
                    focus = Some((surface.clone(), *origin));
                }
            }
            Hit::Layer { surface, origin, .. } => {
                // Client territory too — a bar's hover highlights ride
                // on these motions; `wm-core` hears nothing, exactly
                // as for content.
                focus = Some((surface.clone(), *origin));
            }
            Hit::Ime { surface, origin } => {
                focus = Some((surface.clone(), *origin));
            }
            Hit::Lock { surface, origin } => {
                focus = Some((surface.clone(), *origin));
            }
            Hit::Root => {
                backend.queue(WmEvent::PointerMotion { root: at, surface_local: None });
            }
        },
    }

    if let Some(relative) = &relative {
        pointer.relative_motion(state, focus.clone(), relative);
    }
    pointer.motion(state, focus, &MotionEvent { location: position, serial, time });
    pointer.frame(state);
}

// -- pointer buttons -----------------------------------------------------

/// Button routing: the press's hit-test establishes (or joins) the
/// implicit grab, and both press and release are dispatched against the
/// grab's target — shell queue, WM event, client seat delivery, or a
/// [`ROOT_SHELL`] click the loop feeds to `shell.on_root_press`. A
/// [`DragGrab`] outranks that target while one is held, and the release
/// that ends the drag is the reason it exists.
fn on_pointer_button<I: InputBackend>(state: &mut Compositor, event: I::PointerButtonEvent) {
    let serial = SERIAL_COUNTER.next_serial();
    let time = event.time_msec();
    let pressed = event.state() == ButtonState::Pressed;
    // L/M/R map onto the WM's vocabulary; anything else (side buttons,
    // wheel tilt) is client-only — `wm-x11` swallowed those outright,
    // here they at least still reach a client under the pointer.
    let button = event.button().and_then(wm_button);
    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };
    let seat = state.seat.clone();
    let position = state.pointer_location;
    let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
    let mods = state
        .seat
        .get_keyboard()
        .map(|keyboard| combo_modifiers(&keyboard.modifier_state()))
        .unwrap_or_else(Modifiers::empty);

    // Locked: the click is the lock surface's (the seat's focus was
    // pinned there by the motion path) and nobody else's — no shell
    // queues, no WM events, no implicit-grab bookkeeping to inherit
    // after unlock.
    if state.wm.backend().locked {
        pointer.button(state, &ButtonEvent { serial, time, button: event.button_code(), state: event.state() });
        pointer.frame(state);
        return;
    }

    reclaim_leaked_grab(state, &seat);
    let hit = hit_at(state.wm.backend(), at, position);
    let route = resolve_route(state.wm.backend_mut(), &seat, &hit);
    // Click-outside-to-dismiss, the whole point of
    // `hyprland-focus-grab-v1`: a press that lands outside every
    // whitelisted surface ends the grab and goes nowhere else. Swallowed
    // rather than delivered because that is what a menu click means
    // everywhere — X11's override-redirect menus, GTK, Qt, and Hyprland,
    // which is the implementation Quickshell was written against: the
    // click that closes the popout must not also press the button it
    // happened to land on.
    //
    // Gated on `route.target` for the same reason the motion path is —
    // a gesture already in flight is not a dismissal — and the release
    // is remembered so it can be swallowed too, since returning here
    // means no implicit grab was recorded to route it by.
    if pressed && route.target.is_none() && grab_excludes(state, &hit) {
        crate::focus_grab::dismiss(state);
        with_input(&seat, |input| input.grab_dismissals.push(event.button_code()));
        return;
    }
    if !pressed {
        let swallowed = with_input(&seat, |input| {
            let code = event.button_code();
            let found = input.grab_dismissals.contains(&code);
            input.grab_dismissals.retain(|held| *held != code);
            found
        });
        if swallowed {
            return;
        }
    }
    // The implicit-grab bookkeeping runs whether or not a drag holds
    // the pointer: it is what tracks which buttons are down, and a
    // drag's own lifetime is measured in exactly those (see
    // `reclaim_leaked_grab`). Its answer is only *used* when no drag
    // outranks it.
    let pressed_target = with_input(&seat, |input| {
        if pressed {
            match input.implicit_grab.as_mut() {
                // A second button mid-grab joins the grab; the original
                // target keeps every event (X11 semantics).
                Some(grab) => {
                    if let Some(button) = button {
                        if !grab.buttons.contains(&button) {
                            grab.buttons.push(button);
                        }
                    }
                    grab.target
                }
                None => {
                    let target = press_target(&hit);
                    input.implicit_grab = Some(ImplicitGrab { target, buttons: button.into_iter().collect() });
                    target
                }
            }
        } else {
            match input.implicit_grab.as_mut() {
                Some(grab) => {
                    let target = grab.target;
                    if let Some(button) = button {
                        grab.buttons.retain(|held| *held != button);
                    }
                    // An unrecognized button (one `wm_button` does not
                    // map) still ends a grab it could not have started.
                    if grab.buttons.is_empty() || button.is_none() {
                        input.implicit_grab = None;
                    }
                    target
                }
                // A release with no tracked press (button already down
                // when the compositor started): best-effort location
                // routing.
                None => press_target(&hit),
            }
        }
    });
    let target = match route.target {
        Some(anchor) if route.dragging => anchor,
        _ => pressed_target,
    };
    // The one line that says where a click went. Buttons are rare
    // enough to afford it at debug level, and "the click landed on the
    // wrong thing" bugs are undebuggable from a live session without
    // it — the whole LibreOffice investigation ran on this line.
    tracing::debug!(?target, ?button, pressed, dragging = route.dragging, "pointer button routed");

    // Read before the ledger is borrowed: this is the window
    // manager's policy, and the routing below needs it while holding
    // `backend`.
    let drag_gesture = state.wm.is_drag_gesture(mods);
    let backend = state.wm.backend_mut();
    let mut deliver_to_client = false;
    // Whether `wm-core` has been told, by this event, that a drag
    // ended: a left-button release reported against a surface it can
    // resolve, which is the shape its `handle_pointer_button` reads as
    // the end of one.
    let mut release_reported = false;
    match target {
        PressTarget::Shell(shell) => {
            if let (Some(button), Some(record)) = (button, backend.shells.get(&shell)) {
                backend.shell_clicks.push_back((shell, local_to(at, record.geometry.pos), button, pressed));
            }
        }
        PressTarget::Frame(frame) => {
            if let (Some(button), Some(record)) = (button, backend.frames.get(&frame)) {
                backend.queue(WmEvent::PointerButton {
                    surface: SurfaceRef::Frame(frame),
                    local: local_to(at, record.geometry.pos),
                    button,
                    pressed,
                    time_ms: time,
                    mods,
                });
                release_reported = !pressed && button == MouseButton::Left;
            }
        }
        PressTarget::Content(window) => {
            // The WM hears about content clicks too — that's the whole
            // click-to-focus path (`wm-core`'s `handle_client_button`
            // focuses and calls `replay_pointer`, a no-op here because
            // the very next lines deliver the click to the client
            // themselves; no passive-grab race exists to replay
            // around).
            //
            // It is also where the modifier-drag starts, and that one
            // is the window manager's click alone: `wm-core` turns it
            // into a move or a resize, and a client that also received
            // it would place a text cursor, start a selection, or fire
            // a button under a window the user is only trying to shove
            // out of the way. Decided here rather than after the fact
            // because delivery is settled before `wm-core` processes
            // the queued event.
            let wm_drag_gesture =
                pressed && drag_gesture && matches!(button, Some(MouseButton::Left) | Some(MouseButton::Right));
            if let Some(button) = button {
                let local = backend.windows.get(&window).map(|record| local_to(at, record.content.pos)).unwrap_or(at);
                backend.queue(WmEvent::PointerButton {
                    surface: SurfaceRef::Client(window),
                    local,
                    button,
                    pressed,
                    time_ms: time,
                    mods,
                });
                release_reported = !pressed && button == MouseButton::Left;
            }
            // Not while a drag holds the pointer: the click is the
            // drag's — its release is what ends the move — and the
            // client was sent a leave when the drag began, so handing
            // it a button now would be a press or release arriving on a
            // surface it believes the pointer is nowhere near. Nor when
            // the press *begins* a modifier-drag, which is ours.
            deliver_to_client = !route.dragging && !wm_drag_gesture;
        }
        PressTarget::Layer(_) => {
            // A layer surface's click is the client's alone — `wm-core`
            // manages no window here, so there is nothing to tell it.
            // The keyboard-interactivity side of the click is handled
            // below, after the borrow of the ledger ends.
            deliver_to_client = !route.dragging;
        }
        PressTarget::Ime => {
            deliver_to_client = !route.dragging;
        }
        PressTarget::Root => {
            // Background clicks travel the shell-click queue under the
            // sentinel id; `dispatch_pending` splits presses off to
            // `on_root_press` and lets releases flow through
            // `on_shell_click`, mirroring the X11 loop's routing
            // asymmetry. Root-local coordinates ARE global ones.
            if let Some(button) = button {
                backend.shell_clicks.push_back((ROOT_SHELL, at, button, pressed));
            }
        }
    }

    // A drag whose release this routing cannot name a surface for still
    // has to end, and that is not an exotic shape: the anchor can be
    // the desktop background, or a frame or window record that has gone
    // since the drag latched onto it, and the button that comes up last
    // can be one `wm_button` does not map at all. `PointerButton`
    // carries none of those — it needs a `SurfaceRef` — and a release
    // that goes unreported leaves the window glued to the cursor, which
    // is the whole failure the grab exists to prevent. `DragEnded` says
    // the one thing that is true without naming a surface. (`wm-x11`
    // arrives at the same event from the other side: a release reported
    // against the root with no child it recognizes underneath.)
    //
    // The shell-facing queues above are left alone, because a dock or
    // launcher-strip drag is driven by exactly this release through
    // exactly those queues: one physical release, two audiences,
    // neither able to end the other's drag.
    if route.dragging && !pressed && !release_reported {
        backend.queue(WmEvent::DragEnded);
    }

    // On-demand keyboard interactivity: a press on a layer surface
    // that asked for keyboard focus takes it; a press anywhere else
    // hands it back to whatever window `wm-core` calls focused. The
    // second half cannot ride the normal click-to-focus path alone,
    // because clicking the window that is *already* focused re-queues
    // nothing — `wm-core`'s early return — and the keyboard would stay
    // on the bar forever.
    //
    // Both halves are suppressed while a focus grab holds the keyboard:
    // this press is, by the check at the top of this function, inside
    // the whitelist, and either half would move the seat off it — the
    // claim to a layer surface the grab may not have whitelisted, the
    // release to whatever window `wm-core` calls focused. The grab's
    // own pass owns the keyboard until it ends (`focus_grab.rs`).
    if pressed && !route.dragging && !state.focus_grab.is_active() {
        match target {
            PressTarget::Layer(layer) => claim_on_demand_focus(state, layer, serial),
            // Clicking a candidate must leave keyboard focus on the
            // text-input surface that owns the IME session.
            PressTarget::Ime => {}
            _ => release_on_demand_focus(state, serial),
        }
    }

    if deliver_to_client {
        pointer.button(state, &ButtonEvent { serial, time, button: event.button_code(), state: event.state() });
        pointer.frame(state);
    }
}

/// Gives the keyboard to a clicked layer surface that declared
/// on-demand (or exclusive) interactivity. A surface that declared
/// `None` refused the keyboard outright, and clicking it changes
/// nothing — not even releasing a previous on-demand holder, since the
/// user aimed at a surface that cannot take what would be released.
fn claim_on_demand_focus(state: &mut Compositor, layer: crate::layers::LayerId, serial: smithay::utils::Serial) {
    if !crate::layers::accepts_focus_on_click(state.wm.backend(), layer) {
        return;
    }
    let surface = state
        .wm
        .backend()
        .layers
        .iter()
        .find(|record| record.id == layer)
        .map(|record| record.surface.wl_surface().clone());
    let Some(surface) = surface else {
        return;
    };
    state.layer_shell.on_demand_focus = Some(layer);
    // An exclusive claimant outranks a click — the protocol's own
    // ordering — so the seat only moves when none holds it.
    if state.layer_shell.exclusive_focus.is_some() {
        return;
    }
    if let Some(keyboard) = state.seat.get_keyboard() {
        keyboard.set_focus(state, Some(surface), serial);
    }
}

/// Returns the keyboard from an on-demand layer surface to the window
/// `wm-core` believes focused, on the first click anywhere else.
fn release_on_demand_focus(state: &mut Compositor, serial: smithay::utils::Serial) {
    if state.layer_shell.on_demand_focus.take().is_none() {
        return;
    }
    if state.layer_shell.exclusive_focus.is_some() {
        return;
    }
    let target = crate::layers::focused_window_surface(state);
    if let Some(keyboard) = state.seat.get_keyboard() {
        keyboard.set_focus(state, target, serial);
    }
}

/// Whether this hit falls outside an active focus grab's whitelist —
/// the one question `focus_grab.rs` asks of the hit-test, asked here
/// because [`Hit`] is this module's type.
///
/// Two surfaces are offered to the whitelist because a client may have
/// named either: the exact `wl_surface` the walk landed on (a
/// subsurface, or a popup of the popup) and the layer surface or
/// toplevel that owns it. Quickshell whitelists the layer surface it
/// opened; a client that whitelists only its popup is served by the
/// same call.
///
/// The two hits with no `wl_surface` behind them at all — this
/// desktop's own shell surfaces (dock, menus) and the background —
/// are outside every whitelist by construction, which is right: they
/// are exactly where a user clicks to dismiss a popup.
fn grab_excludes(state: &Compositor, hit: &Hit) -> bool {
    if !state.focus_grab.is_active() {
        return false;
    }
    let backend = state.wm.backend();
    let (surface, root) = match hit {
        Hit::Layer { layer, surface, .. } => {
            let root = backend
                .layers
                .iter()
                .find(|record| record.id == *layer)
                .filter(|record| record.surface.alive())
                .map(|record| record.surface.wl_surface().clone());
            (Some(surface.clone()), root)
        }
        Hit::Content { window, surface, .. } => {
            let root = backend
                .windows
                .get(window)
                .filter(|record| record.surface.alive())
                .and_then(|record| record.surface.wl_surface());
            (surface.clone(), root)
        }
        Hit::Ime { surface, .. } => (Some(surface.clone()), Some(surface.clone())),
        Hit::Lock { surface, .. } => (Some(surface.clone()), Some(surface.clone())),
        // Our own chrome around a client's window. The client owns no
        // pixel of it, so only the whole window being whitelisted keeps
        // a titlebar click from dismissing.
        Hit::FrameChrome { frame, .. } => {
            let root = backend
                .frames
                .get(frame)
                .and_then(|record| backend.windows.get(&record.window))
                .filter(|record| record.surface.alive())
                .and_then(|record| record.surface.wl_surface());
            (None, root)
        }
        Hit::Shell { .. } | Hit::Root => (None, None),
    };
    state.focus_grab.escapes(surface.as_ref(), root.as_ref())
}

fn wm_button(button: InputMouseButton) -> Option<MouseButton> {
    match button {
        InputMouseButton::Left => Some(MouseButton::Left),
        InputMouseButton::Middle => Some(MouseButton::Middle),
        InputMouseButton::Right => Some(MouseButton::Right),
        _ => None,
    }
}

fn press_target(hit: &Hit) -> PressTarget {
    match hit {
        Hit::Shell { shell, .. } => PressTarget::Shell(*shell),
        Hit::FrameChrome { frame, .. } => PressTarget::Frame(*frame),
        Hit::Content { window, .. } => PressTarget::Content(*window),
        Hit::Layer { layer, .. } => PressTarget::Layer(*layer),
        Hit::Ime { .. } => PressTarget::Ime,
        // Locked pointer handlers return before route/press-target
        // selection. Root is the fail-closed fallback if that contract
        // is ever refactored: no desktop client can receive the event.
        Hit::Lock { .. } => PressTarget::Root,
        Hit::Root => PressTarget::Root,
    }
}

// -- pointer axis --------------------------------------------------------

/// Scroll over a shell surface (or the desktop background) becomes a
/// discrete `Backend::take_shell_scroll` event; everything else goes
/// to clients through the seat, continuous data intact.
///
/// The split is deliberate and only one way round is defensible. A
/// client asked for `wl_pointer.axis` and can use every fraction of
/// it, so quantizing on the way to a client would be destroying data
/// its own protocol promised it. The shell channel is defined in whole
/// notches (`wm_core::ScrollDelta` records why), so it is the side
/// that accumulates.
fn on_pointer_axis<I: InputBackend>(state: &mut Compositor, event: I::PointerAxisEvent) {
    if route_shell_scroll::<I>(state, &event) {
        return;
    }
    let horizontal = event
        .amount(Axis::Horizontal)
        .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0);
    let vertical =
        event.amount(Axis::Vertical).unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0);

    let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
    if horizontal != 0.0 {
        frame = frame.relative_direction(Axis::Horizontal, event.relative_direction(Axis::Horizontal));
        frame = frame.value(Axis::Horizontal, horizontal);
        if let Some(discrete) = event.amount_v120(Axis::Horizontal) {
            frame = frame.v120(Axis::Horizontal, discrete as i32);
        }
    }
    if vertical != 0.0 {
        frame = frame.relative_direction(Axis::Vertical, event.relative_direction(Axis::Vertical));
        frame = frame.value(Axis::Vertical, vertical);
        if let Some(discrete) = event.amount_v120(Axis::Vertical) {
            frame = frame.v120(Axis::Vertical, discrete as i32);
        }
    }
    // A finger lifting off a touchpad ends kinetic scroll with an
    // explicit stop event per axis — clients (GTK especially) use it to
    // start their fling animation.
    if event.source() == AxisSource::Finger {
        if event.amount(Axis::Horizontal) == Some(0.0) {
            frame = frame.stop(Axis::Horizontal);
        }
        if event.amount(Axis::Vertical) == Some(0.0) {
            frame = frame.stop(Axis::Vertical);
        }
    }
    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };
    pointer.axis(state, frame);
    pointer.frame(state);
}

/// One wheel detent, in the units libinput's non-v120 `amount`
/// reports for a wheel: degrees, and every wheel since the IMPS/2 era
/// clicks once per 15°. `on_pointer_axis` above already relies on this
/// number for its v120 fallback, which is where it comes from.
///
/// Reused as the touchpad threshold, where `amount` is logical pixels
/// rather than degrees: 15 px of two-finger travel is one step. That
/// is a chosen number, not a derived one — libinput exposes no notch
/// concept for a device that has no notches — and one constant for
/// both devices is the point. Two would drift apart, and a wheel step
/// and a finger step have to feel like the same step to the user, who
/// owns both.
const UNITS_PER_NOTCH: f64 = 15.0;

/// The notch fraction one axis of one event carries.
///
/// `v120` first when the device reports it: 120 units == one detent is
/// exact by definition (the high-resolution wheel API's whole purpose),
/// so a wheel never accumulates rounding error and a notch is never
/// half-eaten. Only a device with no detents at all — a touchpad,
/// where `v120` is `None` — falls back to the continuous amount.
fn axis_notches(v120: Option<f64>, amount: Option<f64>) -> f64 {
    match v120 {
        Some(high_resolution) => high_resolution / 120.0,
        None => amount.unwrap_or(0.0) / UNITS_PER_NOTCH,
    }
}

/// Sub-notch scroll left over between events, and who it belongs to.
///
/// A touchpad reports a continuous stream that may take a dozen events
/// to add up to one step, so the residual has to survive between
/// events — which is why this lives in [`InputState`] on the seat and
/// not in a local.
#[derive(Default)]
struct ScrollAccumulator {
    /// The surface the residual below was collected over. A scroll
    /// aimed somewhere else discards it: half a notch collected on one
    /// dock tile must not complete into a step on the next one the
    /// pointer happens to cross, which would credit a tile with input
    /// the user never spent there.
    owner: Option<WlShellId>,
    up: f64,
    right: f64,
}

impl ScrollAccumulator {
    /// Folds one event's notch fractions in and returns whatever whole
    /// notches that completed, keeping the remainder for next time.
    /// `None` when nothing completed — the drain's contract is that a
    /// queued delta is never zero.
    fn fold(&mut self, owner: WlShellId, up: f64, right: f64) -> Option<ScrollDelta> {
        if self.owner != Some(owner) {
            *self = ScrollAccumulator { owner: Some(owner), up: 0.0, right: 0.0 };
        }
        self.up = accumulate(self.up, up);
        self.right = accumulate(self.right, right);
        let delta = ScrollDelta { up: take_whole(&mut self.up), right: take_whole(&mut self.right) };
        (!delta.is_zero()).then_some(delta)
    }

    /// Drops the residual — the gesture that was building it is over
    /// (the finger lifted), so the next one starts from zero instead
    /// of inheriting a fraction from a gesture the user considers
    /// finished.
    fn reset(&mut self) {
        *self = ScrollAccumulator::default();
    }
}

/// Adds `amount` to `total`, discarding a residual that points the
/// other way. Without this, a flick that stops 0.9 notches into a
/// scroll down leaves the user needing 1.9 notches of scroll up to get
/// one step back — the classic "the first scroll after a direction
/// change does nothing" bug.
fn accumulate(total: f64, amount: f64) -> f64 {
    if total != 0.0 && amount != 0.0 && total.signum() != amount.signum() {
        amount
    } else {
        total + amount
    }
}

/// Splits the whole notches out of `residual`, leaving the fraction.
fn take_whole(residual: &mut f64) -> i32 {
    let whole = residual.trunc();
    *residual -= whole;
    whole as i32
}

/// Offers a scroll to the shell-surface family, returning whether it
/// was claimed (in which case the seat must not also see it).
///
/// Routed exactly as a button press is, implicit grab included: X11's
/// server-side implicit grab sends wheel buttons to the grab window
/// too, so a scroll during a drag reaches the same place on both
/// backends. Without this, the two would disagree in precisely the
/// situation nobody tests by hand.
fn route_shell_scroll<I: InputBackend>(state: &mut Compositor, event: &I::PointerAxisEvent) -> bool {
    // Locked: nothing here is the shell's — the axis flows to the seat
    // and lands on the lock surface like every other locked input.
    if state.wm.backend().locked {
        return false;
    }
    // Signs. `wl_pointer.axis` defines a positive vertical value as
    // motion toward the BOTTOM of the screen, while `ScrollDelta::up`
    // is named for the gesture, so the vertical axis inverts here and
    // the horizontal one (positive == right in both) does not. This
    // negation is the single line that makes button 4 on X11 and a
    // forward wheel roll here mean the same thing; `wm-x11`'s
    // `the_wheel_buttons_map_to_the_gesture_they_name` is its
    // counterpart.
    //
    // The values are taken as libinput reports them, which already
    // reflects the user's natural-scrolling setting.
    // `relative_direction` is deliberately ignored: it exists so a
    // client can UNDO that inversion for content-following gestures
    // like pinch-zoom, and a dock tile's ±1 step wants the direction
    // the user configured, not the raw hardware one.
    let up = -axis_notches(event.amount_v120(Axis::Vertical), event.amount(Axis::Vertical));
    let right = axis_notches(event.amount_v120(Axis::Horizontal), event.amount(Axis::Horizontal));

    let seat = state.seat.clone();
    let position = state.pointer_location;
    let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
    let hit = hit_at(state.wm.backend(), at, position);
    let route = resolve_route(state.wm.backend_mut(), &seat, &hit);
    let target = route.target.unwrap_or_else(|| press_target(&hit));

    let owner = match target {
        PressTarget::Shell(shell) => shell,
        // Background scrolls travel the same queue under the sentinel
        // id, for the same reason background clicks do: on X11 the
        // root window is just another window the shell recognizes by
        // id, so anything else would make the two backends' scroll
        // streams differ over the desktop.
        PressTarget::Root => ROOT_SHELL,
        // Frame chrome and client content are not the shell's. Chrome
        // binds no scroll gesture (`wm-x11` drops those notches
        // outright), and content is the client's — both fall through
        // to the seat below, which is where they went before this
        // channel existed. A layer surface is a client too (a bar
        // scrolling through workspaces wants the continuous axis).
        PressTarget::Frame(_) | PressTarget::Content(_) | PressTarget::Layer(_) | PressTarget::Ime => {
            with_input(&seat, |input| input.scroll.reset());
            return false;
        }
    };

    // A finger leaving the touchpad ends the gesture; the residual it
    // was building belongs to that gesture and not to the next one.
    // (Checked before the fold so a stop event carrying zeros cannot
    // resurrect an old fraction.)
    if event.source() == AxisSource::Finger
        && event.amount(Axis::Vertical).unwrap_or(0.0) == 0.0
        && event.amount(Axis::Horizontal).unwrap_or(0.0) == 0.0
    {
        with_input(&seat, |input| input.scroll.reset());
        return true;
    }

    let completed = with_input(&seat, |input| input.scroll.fold(owner, up, right));
    // Claimed either way: a fraction that has not yet added up to a
    // step is still the shell's scroll, and handing it to the seat as
    // a consolation prize would deliver one gesture to two places.
    let Some(delta) = completed else {
        return true;
    };

    let backend = state.wm.backend_mut();
    let local = match backend.shells.get(&owner) {
        Some(record) => local_to(at, record.geometry.pos),
        // The background has no record and no origin of its own:
        // root-local coordinates ARE global ones, exactly as
        // `on_pointer_button` treats them.
        None => at,
    };
    backend.shell_scrolls.push_back((owner, local, delta));
    true
}

// -- hit-testing ---------------------------------------------------------

/// The scene's input authority: what sits under a point, walking the
/// SAME z-order the renderer paints — Overlay, `above` shells, Top,
/// unmanaged override-redirect X11 windows, the frame band (each
/// frame's xdg popups floating over its chrome, and client-decorated
/// windows taking their own turn), Bottom, `below` shells, Background.
/// The desktop catches the rest. Any disagreement between this walk
/// and the renderer's makes clicks land on things the user cannot see,
/// so both sides cite `backend_impl.rs`'s stacking-band contract.
fn hit_at(backend: &WaylandBackend, at: Point, position: LogicalPoint<f64, Logical>) -> Hit {
    // The lock is a scene domain, not one more band in the desktop's
    // z-order. Put its boundary on the shared hit-test itself so a new
    // input caller cannot accidentally see a window, layer, shell or
    // IME behind it. Existing pointer handlers keep their cheaper
    // dedicated branch; tablet focus reaches this authority directly.
    if backend.locked {
        return lock_hit(backend, position)
            .map(|(surface, origin)| Hit::Lock { surface, origin })
            .unwrap_or(Hit::Root);
    }

    // Candidate windows are rendered above every layer and therefore
    // get the first chance at pointer input.
    for popup in backend.ime_popups.iter().rev() {
        if !popup.alive() {
            continue;
        }
        let Some(parent) = popup.get_parent() else { continue };
        let location = popup.location();
        let global = Point::new(parent.location.loc.x + location.x, parent.location.loc.y + location.y);
        let parent_rect = Rect::new(
            Point::new(parent.location.loc.x, parent.location.loc.y),
            wm_theme_api::Size::new(parent.location.size.w.max(0) as u32, parent.location.size.h.max(0) as u32),
        );
        let anchor: LogicalPoint<f64, Logical> = (global.x as f64, global.y as f64).into();
        let scale = crate::xdg::effective_surface_scale(
            crate::xdg::committed_surface_scale(popup.wl_surface()),
            backend.scale_at(parent_rect),
        );
        let probe = surface_probe(anchor, position, scale);
        if let Some((surface, found)) =
            under_from_surface_tree(popup.wl_surface(), probe, (global.x, global.y), WindowSurfaceType::ALL)
        {
            return Hit::Ime { surface, origin: seat_origin(position, probe, found.to_f64()) };
        }
    }

    // The `Overlay` layer band beats everything — the renderer draws
    // it in front of even the dock and the shell's menus, so it must
    // win the click there too (every band insertion in this walk
    // mirrors a `push_layer_band` call in `build_scene`; the two lists
    // must stay one list read twice).
    if let Some(hit) = layer_band_hit(backend, wlr_layer::Layer::Overlay, at, position) {
        return hit;
    }

    let desktop_bands_occluded = backend
        .monitors
        .iter()
        .find(|monitor| monitor.geometry.contains(at))
        .is_some_and(|monitor| {
            crate::renderer::fullscreen_occludes_desktop_bands(backend, monitor.geometry)
        });

    if !desktop_bands_occluded {
        // `above` shell band (dock, shell menus), topmost stacking
        // entry first.
        for shell in backend.shell_stacking.iter().rev() {
            if let Some(record) = backend.shells.get(shell) {
                if record.above && record.mapped && record.geometry.contains(at) {
                    return Hit::Shell {
                        shell: *shell,
                        local: local_to(at, record.geometry.pos),
                    };
                }
            }
        }

        // `Top` layer surfaces: over every managed window, under the
        // dock and the shell's menus — where the renderer draws them.
        if let Some(hit) = layer_band_hit(backend, wlr_layer::Layer::Top, at, position) {
            return hit;
        }
    }

    // Unmanaged override-redirect X11 windows self-position above the
    // managed window band, and being frameless live outside `stacking`.
    //
    // Selected by window TYPE, not by "owns no frame". Those were the
    // same test until managed windows could be frameless too, and the
    // cheaper one now catches the wrong windows: a client-decorated
    // browser has no frame either, and answering for it here would give
    // it the always-on-top treatment a menu gets. It is walked with the
    // frames below instead, where its stacking slot decides.
    // `renderer.rs`'s override-redirect pass reads this same field.
    for window in backend.scene_index.unmanaged() {
        let Some(record) = backend.windows.get(&window) else { continue };
        if !record.mapped || !record.content.contains(at) {
            continue;
        }
        if let Some(hit) = content_hit(backend, None, window, position) {
            return hit;
        }
    }

    // Frame band.
    for entry in backend.stacking.iter().rev() {
        // A managed window with no frame is in this band at its own
        // depth (see `StackEntry::Window`). Everything the frame arm
        // below does applies to it but the chrome: popups first, then
        // content. The difference that matters is what happens to a
        // point this window does not cover — a frame swallows it as
        // `FrameChrome` because the frame rect is bigger than the
        // client rect by exactly the titlebar and borders, and here
        // there is no such margin to swallow anything, so the point
        // falls through to whatever is behind. That is right: this
        // window's titlebar is inside its own content rect, and the
        // point was already offered to it.
        if let StackEntry::Window(window) = entry {
            let Some(record) = backend.windows.get(window) else {
                continue;
            };
            if !record.mapped || !backend.scene_index.is_presented(*window) {
                continue;
            }
            if let Some(hit) = popup_hit(backend, None, *window, record, position) {
                return hit;
            }
            if frameless_claims(record.content, record.content_offset, at) {
                if let Some(hit) = content_hit(backend, None, *window, position) {
                    return hit;
                }
            }
            continue;
        }
        let StackEntry::Frame(frame) = entry else {
            continue;
        };
        let Some(record) = backend.frames.get(frame) else {
            continue;
        };
        if !record.mapped {
            continue;
        }
        // The frame's client's xdg popups float above its chrome and
        // may extend beyond the frame rect entirely (a context menu
        // opened near an edge), so they are tested before — and
        // independent of — the frame's own geometry.
        let window = backend.windows.get(&record.window);
        if let Some(hit) = window.and_then(|window| {
            popup_hit(backend, Some(*frame), record.window, window, position)
        }) {
            return hit;
        }
        if !record.geometry.contains(at) {
            continue;
        }
        let over_content = window.is_some_and(|window| window.mapped && window.content.contains(at));
        if over_content {
            if let Some(hit) = content_hit(backend, Some(*frame), record.window, position) {
                return hit;
            }
        }
        return Hit::FrameChrome { frame: *frame, local: local_to(at, record.geometry.pos) };
    }

    // `Bottom` layers under the windows, then the `below` shell band,
    // then `Background` layers over only the wallpaper.
    if let Some(hit) = layer_band_hit(backend, wlr_layer::Layer::Bottom, at, position) {
        return hit;
    }

    // `below` shell band (desktop-level furniture).
    for shell in backend.shell_stacking.iter().rev() {
        if let Some(record) = backend.shells.get(shell) {
            if !record.above && record.mapped && record.geometry.contains(at) {
                return Hit::Shell { shell: *shell, local: local_to(at, record.geometry.pos) };
            }
        }
    }

    if let Some(hit) = layer_band_hit(backend, wlr_layer::Layer::Background, at, position) {
        return hit;
    }

    Hit::Root
}

/// Names the production hit-test result at a global point for the
/// opt-in end-to-end test door. Keeping this adapter beside [`hit_at`]
/// means tests can prove pixels and clicks share the same band policy
/// without exposing protocol object handles on the wire.
pub(crate) fn hit_kind_at(backend: &WaylandBackend, at: Point) -> &'static str {
    let position: LogicalPoint<f64, Logical> = (at.x as f64, at.y as f64).into();
    match hit_at(backend, at, position) {
        Hit::Shell { .. } => "shell",
        Hit::FrameChrome { .. } => "frame",
        Hit::Content { popup: true, .. } => "popup",
        Hit::Content { .. } => "content",
        Hit::Layer { .. } => "layer",
        Hit::Ime { .. } => "ime",
        Hit::Lock { .. } => "lock",
        Hit::Root => "root",
    }
}

/// Tests one layer band, topmost (newest) record first — the same
/// order [`crate::renderer`]'s `push_layer_band` draws. A surface's
/// xdg popups are tested before (and independent of) its own
/// geometry, exactly as a frame's are; a point inside the geometry
/// that the surface tree declines (an input region carved out — a
/// notification daemon shapes its input to the visible bubbles) falls
/// through to whatever is behind, which is what lets the desktop stay
/// clickable around mako's corner.
fn layer_band_hit(
    backend: &WaylandBackend,
    band: wlr_layer::Layer,
    at: Point,
    position: LogicalPoint<f64, Logical>,
) -> Option<Hit> {
    for record in backend.layers.iter().rev() {
        if record.layer != band || !backend.layer_presented(record) {
            continue;
        }
        let root = record.surface.wl_surface();
        let output_scale = backend.scale_at(record.geometry);
        let scale = crate::xdg::effective_surface_scale(crate::xdg::committed_surface_scale(root), output_scale);
        for (popup, offset) in backend.popups_for_surface(root) {
            let popup_surface = popup.wl_surface();
            let popup_origin: LogicalPoint<i32, Logical> = (
                record.geometry.pos.x + crate::xdg::scale_length(offset.x, scale),
                record.geometry.pos.y + crate::xdg::scale_length(offset.y, scale),
            )
                .into();
            let anchor: LogicalPoint<f64, Logical> = (popup_origin.x as f64, popup_origin.y as f64).into();
            let probe = surface_probe(
                anchor,
                position,
                crate::xdg::effective_surface_scale(
                    crate::xdg::committed_surface_scale(popup_surface),
                    output_scale,
                ),
            );
            if let Some((surface, found)) =
                under_from_surface_tree(popup_surface, probe, popup_origin, WindowSurfaceType::ALL)
            {
                return Some(Hit::Layer {
                    layer: record.id,
                    surface,
                    origin: seat_origin(position, probe, found.to_f64()),
                });
            }
        }
        if !record.geometry.contains(at) {
            continue;
        }
        let anchor: LogicalPoint<f64, Logical> = (record.geometry.pos.x as f64, record.geometry.pos.y as f64).into();
        let probe = surface_probe(anchor, position, scale);
        if let Some((surface, found)) =
            under_from_surface_tree(root, probe, (record.geometry.pos.x, record.geometry.pos.y), WindowSurfaceType::ALL)
        {
            return Some(Hit::Layer {
                layer: record.id,
                surface,
                origin: seat_origin(position, probe, found.to_f64()),
            });
        }
    }
    None
}

/// The lock surface (and seat origin) under a position, for the locked
/// input path: each lock surface covers its whole output, so this is a
/// monitor lookup plus the same tree walk every other surface family
/// gets. `None` over an output the locker has not covered — the seat
/// focuses nothing there, which over a blanked screen is the truth.
fn lock_hit(
    backend: &WaylandBackend,
    position: LogicalPoint<f64, Logical>,
) -> Option<(WlSurface, LogicalPoint<f64, Logical>)> {
    let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
    for entry in &backend.lock_surfaces {
        if !entry.surface.alive() {
            continue;
        }
        let Some(monitor) = backend.monitors.get(entry.output) else {
            continue;
        };
        if !monitor.geometry.contains(at) {
            continue;
        }
        let root = entry.surface.wl_surface();
        let anchor: LogicalPoint<f64, Logical> = (monitor.geometry.pos.x as f64, monitor.geometry.pos.y as f64).into();
        let probe = surface_probe(
            anchor,
            position,
            crate::xdg::effective_surface_scale(
                crate::xdg::committed_surface_scale(root),
                backend.scale_at(monitor.geometry),
            ),
        );
        return match under_from_surface_tree(
            root,
            probe,
            (monitor.geometry.pos.x, monitor.geometry.pos.y),
            WindowSurfaceType::ALL,
        ) {
            Some((surface, found)) => Some((surface, seat_origin(position, probe, found.to_f64()))),
            // The walk declining (a buffer briefly smaller than the
            // output mid-resize) still delivers to the root surface —
            // a locker must never find the pointer unreachable.
            None => Some((root.clone(), seat_origin(position, probe, anchor))),
        };
    }
    None
}

/// What the compositor's own pointer should look like, answered from
/// the same ledger the hit-test routes by — the renderer asks this
/// every frame (`push_cursor_elements`) instead of trusting the last
/// `CursorImageStatus` a client happened to set. Trusting it was the
/// bug: a client's cursor surface outlives the pointer's visit (no
/// client un-sets a cursor on leave — leave means it may not), so
/// LibreOffice's pointer kept being drawn over the desktop, the dock,
/// and every frame the pointer crossed afterwards.
pub(crate) enum PointerSubject {
    /// Client content: the client's `wl_pointer.set_cursor` choice
    /// applies, falling back to the arrow when it never made one.
    Client,
    /// One of our frames' chrome, with the resize cursor the frame
    /// last asked for (`Backend::set_frame_cursor`), if any.
    Frame(Option<ResizeEdge>),
    /// Everything else — shell surfaces, the desktop: the arrow.
    Desktop,
}

/// Classifies what the pointer is over for cursor selection.
///
/// A drag grab answers first, from its anchor rather than the hit
/// under the pointer: the client under a WM drag has been sent a leave
/// and is being told nothing, so its cursor must not show — and a
/// frame-edge resize keeps its edge cursor up even as the pointer
/// overshoots the chrome onto content or desktop mid-drag, which every
/// fast resize does.
pub(crate) fn pointer_subject(backend: &WaylandBackend, position: LogicalPoint<f64, Logical>) -> PointerSubject {
    // Locked: the locker's own cursor choice applies over its surface
    // (swaylock hides the pointer, and that statement must hold);
    // everywhere else — a blanked output — the compositor's arrow.
    if backend.locked {
        return if lock_hit(backend, position).is_some() { PointerSubject::Client } else { PointerSubject::Desktop };
    }
    if let Some(grab) = &backend.pointer_grab {
        return match grab.target() {
            Some(PressTarget::Frame(frame)) => PointerSubject::Frame(backend.frame_cursors.get(&frame).copied()),
            _ => PointerSubject::Desktop,
        };
    }
    let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
    match hit_at(backend, at, position) {
        // A layer surface is client territory like any window content:
        // its `set_cursor` choice applies while the pointer is on it.
        Hit::Content { .. } | Hit::Layer { .. } | Hit::Ime { .. } | Hit::Lock { .. } => {
            PointerSubject::Client
        }
        Hit::FrameChrome { frame, .. } => PointerSubject::Frame(backend.frame_cursors.get(&frame).copied()),
        Hit::Shell { .. } | Hit::Root => PointerSubject::Desktop,
    }
}

/// What the top-down hit-test found under a point.
enum Hit {
    /// A mapped shell surface (dock, menu, icon tile), with the
    /// surface-local position the shell's click/motion routing expects.
    Shell { shell: WlShellId, local: Point },
    /// Frame chrome: inside a frame's geometry but outside its client's
    /// content rect (titlebar, borders, resize edges) — `wm-core`'s
    /// territory, reported frame-local like X11 frame events.
    FrameChrome { frame: WlFrameId, local: Point },
    /// Client content (or one of its xdg popups): input goes to the
    /// client through the seat. `frame` is the owning frame when one
    /// exists — `None` both for unmanaged override-redirect X11 windows
    /// and for managed windows whose client drew its own chrome;
    /// `surface` names the exact wl_surface to focus (`None` for an X11
    /// window whose wl_surface has not been associated yet — nothing to
    /// deliver to, but the WM still learns about the click) and
    /// `origin` is the point the seat must subtract the pointer
    /// position from to get the client's own coordinates, which is its
    /// global position for every client drawing at 1x and something
    /// else entirely for one that is not (see [`seat_origin`]). No
    /// local coordinate:
    /// `wm-core` ignores it for `SurfaceRef::Client` events, and the
    /// button handler recomputes a content-local point from the record
    /// for the event shape.
    Content {
        frame: Option<WlFrameId>,
        window: WlWindowId,
        surface: Option<WlSurface>,
        origin: LogicalPoint<f64, Logical>,
        /// Distinguishes an xdg popup from its parent's ordinary
        /// surface tree for test-door diagnostics; routing is identical.
        popup: bool,
    },
    /// A wlr-layer-shell surface (or one of its popups): client
    /// territory with no `wm-core` window behind it. Same seat
    /// delivery contract as `Content` — `surface` is the exact
    /// wl_surface to focus and `origin` the point the seat subtracts
    /// from (see [`seat_origin`]).
    Layer { layer: crate::layers::LayerId, surface: WlSurface, origin: LogicalPoint<f64, Logical> },
    /// An input-method candidate popup, above every normal layer.
    Ime { surface: WlSurface, origin: LogicalPoint<f64, Logical> },
    /// The lock surface under the pointer. This variant can exist only
    /// while `backend.locked`; the early return in `hit_at` makes every
    /// ordinary desktop hit structurally unreachable in that state.
    Lock { surface: WlSurface, origin: LogicalPoint<f64, Logical> },
    /// The desktop background.
    Root,
}

/// How far outside its declared window geometry a client-decorated
/// window still owns the pointer, so its own resize grips are
/// grabbable at the visible edge rather than only strictly inside it.
/// GTK puts the grip band for `xdg_toplevel.resize` in the shadow
/// margin just *outside* the window geometry; a boundary drawn exactly
/// at the geometry would make edge-resize of such a window a
/// pixel-hunt for the sliver of grip that overlaps the window itself.
const RESIZE_MARGIN: i32 = 8;

/// Whether a client-decorated window owns the point `at` — the
/// boundary of ownership for a window whose buffer is larger than the
/// window it declared.
///
/// The declared `xdg_surface.set_window_geometry` rect (`content`) is
/// the boundary, give or take the resize margin above; the rest of the
/// buffer is drop shadow, and a shadow that answered the hit-test
/// would both start drags anchored up to its width away from the
/// visible edge and swallow clicks meant for whatever the shadow is
/// painted over. `shadow` is the window's `content_offset` — how much
/// buffer extends past the geometry — and clamps the margin, so a
/// window with no shadow claims exactly its own rectangle and never a
/// band of its neighbour's pixels: the margin is only ever carved out
/// of space the client is already drawing (translucently) into.
fn frameless_claims(content: Rect, shadow: Point, at: Point) -> bool {
    let margin_x = RESIZE_MARGIN.min(shadow.x.max(0));
    let margin_y = RESIZE_MARGIN.min(shadow.y.max(0));
    at.x >= content.pos.x - margin_x
        && at.y >= content.pos.y - margin_y
        && at.x < content.pos.x + content.size.w as i32 + margin_x
        && at.y < content.pos.y + content.size.h as i32 + margin_y
}

/// Content hit for a window whose content rect contains the point:
/// resolves the exact wl_surface (subsurfaces included) to hand the
/// seat. Falls back to the root surface when the tree walk declines the
/// point (a client buffer briefly smaller than its configured rect
/// mid-resize) — coordinates a little outside the buffer are what X11
/// delivered in that gap too, and clients cope.
fn content_hit(
    backend: &WaylandBackend,
    frame: Option<WlFrameId>,
    window: WlWindowId,
    position: LogicalPoint<f64, Logical>,
) -> Option<Hit> {
    let record = backend.windows.get(&window)?;
    let root_surface = record.surface.wl_surface();
    // The surface tree is anchored where the renderer actually draws
    // it — up and left of the window by the client's own
    // window-geometry offset — so that a click resolves to the same
    // pixel the user is looking at. Anchoring on the window instead
    // would send every client coordinates shifted by its drop shadow.
    let content_origin =
        Point::new(record.content.pos.x - record.content_offset.x, record.content.pos.y - record.content_offset.y);
    let anchor: LogicalPoint<f64, Logical> = (content_origin.x as f64, content_origin.y as f64).into();
    let (surface, origin) = match &root_surface {
        Some(root) => {
            let scale = backend.window_surface_scale(record);
            let probe = surface_probe(anchor, position, scale);
            under_from_surface_tree(root, probe, (content_origin.x, content_origin.y), WindowSurfaceType::ALL)
                .map(|(surface, found)| (Some(surface), seat_origin(position, probe, found.to_f64())))
                .unwrap_or_else(|| (root_surface.clone(), seat_origin(position, probe, anchor)))
        }
        // An X11 window whose wl_surface has not been associated yet:
        // nothing to deliver to, but the click still counts for
        // click-to-focus.
        None => (None, anchor),
    };
    Some(Hit::Content {
        frame,
        window,
        surface,
        origin,
        popup: false,
    })
}

/// Moves a point into the space a client's surface tree is measured in.
///
/// Every rectangle this compositor stores is in device pixels, and so
/// is `position`, but a surface's extent is its buffer divided by the
/// scale that client committed: a window running at 2x covers 600
/// device pixels of screen and reports a 300-pixel surface. Feeding the
/// device-pixel offset to a walk that compares it against the reported
/// extent finds only the top-left quarter of such a window, so a click
/// anywhere else in it falls through to whatever is behind.
///
/// Only the *offset* from the anchor is divided; the anchor itself is
/// passed to the walk untouched. Dividing both would round the window's
/// position onto the client's coarser grid and move the hit rectangle
/// off the drawn one by up to a device pixel per axis, which is exactly
/// the disagreement this is here to remove.
fn surface_probe(
    anchor: LogicalPoint<f64, Logical>,
    position: LogicalPoint<f64, Logical>,
    scale: f64,
) -> LogicalPoint<f64, Logical> {
    let scale = if scale.is_finite() && scale >= 0.125 { scale } else { 1.0 };
    (anchor.x + (position.x - anchor.x) / scale, anchor.y + (position.y - anchor.y) / scale).into()
}

/// The origin to hand the seat for a surface the walk found at `found`
/// when probed at `probe`.
///
/// Not where the surface is on screen: smithay delivers `position -
/// origin` to the client verbatim, and a client wants that difference
/// in its own pixels, so this is wherever the surface would have to sit
/// for the subtraction to come out right. For an unscaled client the
/// probe *is* the position and this is the surface's screen origin,
/// unchanged from before any of this existed.
fn seat_origin(
    position: LogicalPoint<f64, Logical>,
    probe: LogicalPoint<f64, Logical>,
    found: LogicalPoint<f64, Logical>,
) -> LogicalPoint<f64, Logical> {
    (position.x - (probe.x - found.x), position.y - (probe.y - found.y)).into()
}

/// Tests a window's xdg popup tree (context menus, dropdowns of native
/// Wayland clients). `frame` is threaded through to the resulting
/// [`Hit::Content`] rather than used here, so a frameless window's
/// popups report `None` exactly as its content does. Popup offsets from
/// `PopupManager::popups_for_surface` are parent-surface-relative; the
/// renderer resolves them against the content rect (`content.pos +
/// offset` — see `renderer.rs`'s `push_window_content`) and this walk
/// must resolve them identically or popup clicks land beside the menu
/// the user sees.
fn popup_hit(
    backend: &WaylandBackend,
    frame: Option<WlFrameId>,
    window: WlWindowId,
    record: &crate::state::WindowRecord,
    position: LogicalPoint<f64, Logical>,
) -> Option<Hit> {
    if !record.mapped {
        return None;
    }
    let ManagedSurface::Xdg(toplevel) = &record.surface else {
        // X11 menus arrive as override-redirect windows, handled at the
        // top of `hit_at` — no xdg popups to test here.
        return None;
    };
    if !backend.surface_has_popups(toplevel.wl_surface()) {
        return None;
    }
    // Popup offsets are measured from the parent surface's own origin,
    // which is the buffer's, so the same correction as `hit_at` applies.
    let content_origin =
        Point::new(record.content.pos.x - record.content_offset.x, record.content.pos.y - record.content_offset.y);
    // The offset is surface-local to the parent, so it is measured in
    // the parent's pixels and converts by the parent's factor — while
    // the popup's own tree is walked at the popup's, because a menu is
    // a separate surface with a separately committed scale. Both halves
    // are exactly what `renderer.rs`'s popup loop does, which is the
    // agreement this function's doc comment demands: draw and hit-test
    // have to describe one rectangle.
    let parent_scale = backend.window_surface_scale(record);
    for (popup, offset) in backend.popups_for_surface(toplevel.wl_surface()) {
        let popup_surface = popup.wl_surface();
        let popup_origin: LogicalPoint<i32, Logical> = (
            content_origin.x + crate::xdg::scale_length(offset.x, parent_scale),
            content_origin.y + crate::xdg::scale_length(offset.y, parent_scale),
        )
            .into();
        let anchor: LogicalPoint<f64, Logical> = (popup_origin.x as f64, popup_origin.y as f64).into();
        let probe = surface_probe(
            anchor,
            position,
            crate::xdg::effective_surface_scale(
                crate::xdg::committed_surface_scale(popup_surface),
                backend.scale_at(record.content),
            ),
        );
        if let Some((surface, found)) =
            under_from_surface_tree(popup_surface, probe, popup_origin, WindowSurfaceType::ALL)
        {
            return Some(Hit::Content {
                frame,
                window,
                surface: Some(surface),
                origin: seat_origin(position, probe, found.to_f64()),
                popup: true,
            });
        }
    }
    None
}

/// Root-space point -> a rect's local coordinates.
fn local_to(at: Point, origin: Point) -> Point {
    Point::new(at.x - origin.x, at.y - origin.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, XkbConfig};
    use smithay::input::SeatState;
    use smithay::utils::{IsAlive, Serial};
    use wm_theme_api::{Rect, Size};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestInputTarget;

    impl IsAlive for TestInputTarget {
        fn alive(&self) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct TestSeatState {
        seat_state: SeatState<Self>,
        keys: Vec<(Keycode, KeyState)>,
        modifiers: Vec<ModifiersState>,
    }

    impl SeatHandler for TestSeatState {
        type KeyboardFocus = TestInputTarget;
        // This test never adds pointer or touch capabilities. Reusing a
        // Smithay target that already implements both contracts keeps
        // the fixture about the keyboard behavior under test.
        type PointerFocus = smithay::xwayland::X11Surface;
        type TouchFocus = smithay::xwayland::X11Surface;

        fn seat_state(&mut self) -> &mut SeatState<Self> {
            &mut self.seat_state
        }
    }

    impl KeyboardTarget<TestSeatState> for TestInputTarget {
        fn enter(
            &self,
            _seat: &Seat<TestSeatState>,
            _data: &mut TestSeatState,
            _keys: Vec<KeysymHandle<'_>>,
            _serial: Serial,
        ) {
        }

        fn leave(&self, _seat: &Seat<TestSeatState>, _data: &mut TestSeatState, _serial: Serial) {}

        fn key(
            &self,
            _seat: &Seat<TestSeatState>,
            data: &mut TestSeatState,
            key: KeysymHandle<'_>,
            state: KeyState,
            _serial: Serial,
            _time: u32,
        ) {
            data.keys.push((key.raw_code(), state));
        }

        fn modifiers(
            &self,
            _seat: &Seat<TestSeatState>,
            data: &mut TestSeatState,
            modifiers: ModifiersState,
            _serial: Serial,
        ) {
            data.modifiers.push(modifiers);
        }
    }

    fn send_test_key(
        state: &mut TestSeatState,
        keyboard: &KeyboardHandle<TestSeatState>,
        keycode: Keycode,
        key_state: KeyState,
        forward: bool,
    ) {
        keyboard.input::<(), _>(state, keycode, key_state, SERIAL_COUNTER.next_serial(), 1, |_, _, _| {
            if forward {
                FilterResult::Forward
            } else {
                FilterResult::Intercept(())
            }
        });
    }

    #[test]
    fn resume_clears_xkb_state_and_balances_only_forwarded_presses() {
        const TAB: Keycode = Keycode::new(15 + 8);
        const LEFT_ALT: Keycode = Keycode::new(56 + 8);
        const CAPS_LOCK: Keycode = Keycode::new(58 + 8);
        const NUM_LOCK: Keycode = Keycode::new(69 + 8);

        let mut state = TestSeatState::default();
        let mut seat = state.seat_state.new_seat("resume-test");
        let keyboard = seat.add_keyboard(XkbConfig::default(), 200, 25).expect("default keymap");
        keyboard.set_focus(&mut state, Some(TestInputTarget), SERIAL_COUNTER.next_serial());

        for lock in [CAPS_LOCK, NUM_LOCK] {
            send_test_key(&mut state, &keyboard, lock, KeyState::Pressed, true);
            send_test_key(&mut state, &keyboard, lock, KeyState::Released, true);
        }
        assert!(keyboard.modifier_state().caps_lock);
        assert!(keyboard.modifier_state().num_lock);

        send_test_key(&mut state, &keyboard, LEFT_ALT, KeyState::Pressed, true);
        send_test_key(&mut state, &keyboard, TAB, KeyState::Pressed, false);
        assert!(keyboard.modifier_state().alt);
        state.keys.clear();
        state.modifiers.clear();

        release_stale_pressed_keys(&mut state, &keyboard, &[TAB], 2);

        assert!(keyboard.pressed_keys().is_empty());
        assert_eq!(state.keys, vec![(LEFT_ALT, KeyState::Released)]);
        let modifiers = keyboard.modifier_state();
        assert!(!modifiers.alt);
        assert!(!modifiers.ctrl);
        assert!(!modifiers.shift);
        assert!(!modifiers.logo);
        assert!(modifiers.caps_lock);
        assert!(modifiers.num_lock);
        assert_eq!(modifiers.serialized.latched, 0);
        assert_eq!(state.modifiers.last(), Some(&modifiers));
    }

    #[test]
    fn resume_drops_every_compositor_owned_hold() {
        let keycode = Keycode::new(23);
        let combo = KeyCombo { keysym: keysyms::KEY_Tab, modifiers: Modifiers::ALT };
        let mut input = InputState {
            implicit_grab: Some(ImplicitGrab { target: PressTarget::Root, buttons: vec![MouseButton::Left] }),
            grab_dismissals: vec![272],
            suppressed_keys: vec![keycode],
            repeating: Some(RepeatingKey {
                keycode,
                combo,
                next: std::time::Instant::now(),
                interval: std::time::Duration::from_millis(40),
                emitted: 3,
            }),
            ..InputState::default()
        };

        assert_eq!(reset_resume_bookkeeping(&mut input), vec![keycode]);
        assert!(input.implicit_grab.is_none());
        assert!(input.grab_dismissals.is_empty());
        assert!(input.suppressed_keys.is_empty());
        assert!(input.repeating.is_none());
    }

    /// The lock boundary and idle policy are stated once per input
    /// family, including every family the compositor currently drops.
    /// `InputFamily::of` is an exhaustive match over Smithay's event
    /// enum, so a future family first fails to compile there; this
    /// table then makes its required policy visible in one assertion.
    #[test]
    fn every_input_family_has_an_explicit_lock_route_and_idle_policy() {
        use InputFamily::*;
        use LockedInputRoute::*;

        let cases = [
            (DeviceLifecycle, false, NoClientDelivery),
            (Keyboard, true, KeyboardFilter),
            (PointerMotion, true, PointerMotionHandler),
            (PointerButton, true, PointerFocus),
            (PointerAxis, true, PointerFocus),
            (Gesture, true, PointerFocus),
            (Touch, false, NoClientDelivery),
            (TabletTool, true, TabletHitTest),
            (Switch, false, NoClientDelivery),
            (Special, false, NoClientDelivery),
        ];
        for (family, resets_idle, locked_route) in cases {
            assert_eq!(family.resets_idle(), resets_idle, "idle policy for {family:?}");
            assert_eq!(family.locked_route(), locked_route, "lock route for {family:?}");
        }
    }

    // Routing and hit-testing need a live seat, a wayland display and a
    // ledger full of surfaces, so they are exercised by running the
    // session. Pointer confinement is the one piece of this module that
    // is pure geometry — and the one piece whose multi-monitor behavior
    // the single-connector test VM cannot show, which is exactly why it
    // is worth pinning here.

    fn monitor(x: i32, y: i32, w: u32, h: u32) -> MonitorInfo {
        MonitorInfo {
            geometry: Rect { pos: Point::new(x, y), size: Size::new(w, h) },
            name: format!("test-{x}x{y}"),
            primary: x == 0 && y == 0,
        }
    }

    fn confined(monitors: &[MonitorInfo], x: f64, y: f64) -> (f64, f64) {
        let point = confine_to_outputs(monitors, (x, y).into());
        (point.x, point.y)
    }

    #[test]
    fn one_monitor_confines_to_its_own_edges() {
        let monitors = [monitor(0, 0, 800, 600)];
        assert_eq!(confined(&monitors, 400.0, 300.0), (400.0, 300.0));
        // The far edge is the last pixel inside, not the width itself:
        // a pointer at x=800 would hit-test against nothing.
        assert_eq!(confined(&monitors, 1000.0, 900.0), (799.0, 599.0));
        assert_eq!(confined(&monitors, -50.0, -1.0), (0.0, 0.0));
    }

    #[test]
    fn a_second_monitor_extends_the_reachable_area() {
        let monitors = [monitor(0, 0, 800, 600), monitor(800, 0, 640, 480)];
        // The seam is crossable: what used to be past the right edge is
        // now the second monitor's left column.
        assert_eq!(confined(&monitors, 800.0, 100.0), (800.0, 100.0));
        assert_eq!(confined(&monitors, 1439.0, 479.0), (1439.0, 479.0));
        // And the far edge is now the second monitor's.
        assert_eq!(confined(&monitors, 5000.0, 10.0), (1439.0, 10.0));
    }

    #[test]
    fn a_point_no_monitor_covers_snaps_to_the_nearest_one() {
        // Unequal heights leave dead space under the shorter monitor,
        // inside the union bounding box but on no screen. A pointer
        // there would be drawn nowhere, so it lands on the nearest edge
        // instead — here the second monitor's bottom row, not the first
        // monitor's right column, because it is closer.
        let monitors = [monitor(0, 0, 800, 600), monitor(800, 0, 640, 480)];
        assert_eq!(confined(&monitors, 1000.0, 550.0), (1000.0, 479.0));
        // Straddling: nearer to the tall monitor horizontally than to
        // the short one's bottom edge.
        assert_eq!(confined(&monitors, 805.0, 599.0), (799.0, 599.0));
    }

    #[test]
    fn no_monitors_leaves_the_position_alone() {
        // Not a state a running session reaches; it must not panic or
        // teleport the pointer to the origin if it ever does.
        assert_eq!(confined(&[], 12.0, 34.0), (12.0, 34.0));
    }

    // -- discrete scroll ---------------------------------------------
    // The other pure piece of this module: turning libinput's
    // continuous axis values into the whole notches the shell channel
    // is defined in. `route_shell_scroll` itself needs a seat and a
    // populated ledger, but every decision it makes that could differ
    // from `wm-x11` lives in these three functions.

    const DOCK: WlShellId = WlShellId(1);
    const CLIP: WlShellId = WlShellId(2);

    /// One detent of a high-resolution wheel is exactly one step, with
    /// no rounding left behind — the reason `v120` is preferred over
    /// the continuous amount.
    #[test]
    fn a_high_resolution_detent_is_exactly_one_notch() {
        assert_eq!(axis_notches(Some(120.0), Some(15.0)), 1.0);
        assert_eq!(axis_notches(Some(-120.0), Some(-15.0)), -1.0);
        // Half a detent on a free-spinning wheel is half a step, and
        // stays a fraction until its other half arrives.
        assert_eq!(axis_notches(Some(60.0), Some(7.5)), 0.5);
    }

    /// A device with no v120 (a touchpad) falls back to the continuous
    /// amount over the shared threshold.
    #[test]
    fn a_continuous_amount_is_measured_in_notch_fractions() {
        assert_eq!(axis_notches(None, Some(15.0)), 1.0);
        assert_eq!(axis_notches(None, Some(5.0)), 1.0 / 3.0);
        assert_eq!(axis_notches(None, None), 0.0);
    }

    /// The whole point of the accumulator: a touchpad drip-feeds
    /// fractions and the caller must see one step at the moment they
    /// add up, not a step per event and not nothing at all.
    #[test]
    fn touchpad_fractions_add_up_to_exactly_one_step() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.fold(DOCK, 0.4, 0.0), None);
        assert_eq!(scroll.fold(DOCK, 0.4, 0.0), None);
        assert_eq!(scroll.fold(DOCK, 0.4, 0.0), Some(ScrollDelta { up: 1, right: 0 }));
        // 0.2 of a notch is still owed, so the next step needs only
        // 0.8 more — no input is lost to truncation.
        assert_eq!(scroll.fold(DOCK, 0.7, 0.0), None);
        assert_eq!(scroll.fold(DOCK, 0.2, 0.0), Some(ScrollDelta { up: 1, right: 0 }));
    }

    /// A wheel spun hard between two event-loop passes reports several
    /// detents at once; they must arrive as a count, not be flattened
    /// to one step or split into events with no positions of their own.
    #[test]
    fn several_detents_in_one_event_stay_a_count() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.fold(DOCK, -3.0, 0.0), Some(ScrollDelta { up: -3, right: 0 }));
    }

    /// Both axes complete in the same event when a tilt-wheel or a
    /// diagonal two-finger drag says so.
    #[test]
    fn the_two_axes_complete_independently() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.fold(DOCK, 0.5, 1.0), Some(ScrollDelta { up: 0, right: 1 }));
        assert_eq!(scroll.fold(DOCK, 0.5, 0.0), Some(ScrollDelta { up: 1, right: 0 }));
    }

    /// Reversing direction must not cost the user a step: without the
    /// residual reset, 0.9 notches down followed by a deliberate flick
    /// up would need 1.9 notches to produce one step up.
    #[test]
    fn reversing_direction_does_not_eat_the_first_step() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.fold(DOCK, -0.9, 0.0), None);
        assert_eq!(scroll.fold(DOCK, 1.0, 0.0), Some(ScrollDelta { up: 1, right: 0 }));
    }

    /// Residual belongs to the surface it was collected over. A tile
    /// the pointer merely crossed must not inherit most of a step the
    /// user spent on its neighbour.
    #[test]
    fn a_residual_does_not_follow_the_pointer_to_another_surface() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.fold(DOCK, 0.9, 0.0), None);
        assert_eq!(scroll.fold(CLIP, 0.2, 0.0), None, "0.9 + 0.2 would have been a step on the wrong surface");
        assert_eq!(scroll.fold(CLIP, 0.8, 0.0), Some(ScrollDelta { up: 1, right: 0 }));
    }

    /// A finger lifting ends the gesture (`route_shell_scroll` calls
    /// this on libinput's stop event), so the next gesture starts from
    /// zero rather than borrowing a fraction from the last one.
    #[test]
    fn lifting_the_finger_discards_the_partial_step() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.fold(DOCK, 0.9, 0.0), None);
        scroll.reset();
        assert_eq!(scroll.fold(DOCK, 0.9, 0.0), None, "the old 0.9 must not complete this one");
        assert_eq!(scroll.fold(DOCK, 0.1, 0.0), Some(ScrollDelta { up: 1, right: 0 }));
    }

    /// Equivalence with `wm-x11`, stated as an assertion rather than a
    /// comment. There, `wheel_scroll` maps button 4 to `up: 1` and
    /// button 7 to `right: 1`; here the same physical gesture arrives
    /// as a `wl_pointer` axis value whose vertical sign is the
    /// opposite. If either side's sign flips, one of these two tests
    /// fails.
    #[test]
    fn one_detent_here_equals_one_x11_wheel_button_there() {
        let mut scroll = ScrollAccumulator::default();
        // A wheel rolled away from the user: libinput reports negative
        // vertical (positive is toward the screen's bottom), which
        // `route_shell_scroll` negates.
        let up = -axis_notches(Some(-120.0), Some(-15.0));
        assert_eq!(scroll.fold(DOCK, up, 0.0), Some(ScrollDelta { up: 1, right: 0 }), "must equal wm-x11's button 4");

        // A wheel tilted right: positive horizontal on both platforms,
        // so no negation.
        let right = axis_notches(Some(120.0), Some(15.0));
        assert_eq!(
            scroll.fold(DOCK, 0.0, right),
            Some(ScrollDelta { up: 0, right: 1 }),
            "must equal wm-x11's button 7"
        );
    }

    /// A delta the drain would queue is never empty, matching
    /// `Backend::take_shell_scroll`'s stated contract — an event that
    /// completes nothing yields `None` instead of a zero step.
    #[test]
    fn nothing_completing_queues_nothing() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.fold(DOCK, 0.0, 0.0), None);
        assert_eq!(scroll.fold(DOCK, 0.3, -0.3), None);
    }

    // -- the drag grab -----------------------------------------------
    // Routing a live drag needs a seat, a client and a populated
    // ledger, so what is pinned here is the state machine underneath
    // it: which target a drag's events go to, who is allowed to end
    // the grab, and when the compositor takes it back on its own. A
    // wrong answer to any of those is a desktop that stops responding
    // to the mouse, which is the one failure mode worse than the bug
    // the grab fixes.
    //
    // The ledger is real (a `Display` needs no socket and no GPU), so
    // these run against the same `Backend` verbs `wm-core` calls.

    use smithay::reexports::wayland_server::Display;
    use wm_core::Backend;

    fn ledger() -> WaylandBackend {
        let display = Display::<Compositor>::new().expect("a display with no socket");
        // Ledger scale 1 — the value every running session passes today
        // (see the note at `run`'s construction site).
        WaylandBackend::new(display.handle(), Vec::new(), 1.0)
    }

    #[test]
    fn resume_releases_the_modal_keyboard_grab() {
        let mut backend = ledger();
        backend.keyboard_grabbed = true;

        reset_modal_keyboard_grab(&mut backend);

        assert!(!backend.keyboard_grabbed, "fresh input must not remain intercepted");
        assert!(matches!(
            backend.poll_event(),
            Some(BackendEvent::KeyRelease(KeyCombo { keysym: keysyms::KEY_Alt_L, .. }))
        ));
    }

    const FRAME: WlFrameId = WlFrameId(11);
    const WINDOW: WlWindowId = WlWindowId(12);

    /// The anchor is latched once, from the press that started the
    /// drag, and never revised. This is what makes the release
    /// deliverable: the release is the event that clears the implicit
    /// grab, so a drag that re-read it every time would find nothing
    /// there at the one moment it has to name a target.
    #[test]
    fn a_drag_keeps_the_target_its_press_pinned() {
        let mut grab = DragGrab::new(DragHandle(7));
        let pressed_on = PressTarget::Content(WINDOW);
        assert_eq!(grab.anchor(Some(pressed_on), &Hit::Root), pressed_on);
        // The release: no implicit grab left, and the pointer has since
        // wandered off the window onto the desktop.
        assert_eq!(grab.anchor(None, &Hit::Root), pressed_on);
    }

    /// A drag that began with no button down — a client asking for a
    /// move after its own press was already over — still has to route
    /// somewhere, and what is under the pointer is the same answer that
    /// press would have given.
    #[test]
    fn a_drag_with_no_press_behind_it_anchors_on_the_pointer() {
        let mut grab = DragGrab::new(DragHandle(7));
        let hit = Hit::FrameChrome { frame: FRAME, local: Point::new(3, 4) };
        assert_eq!(grab.anchor(None, &hit), PressTarget::Frame(FRAME));
    }

    /// Only the handle that took the grab may end it. The launcher
    /// strip hands a stale handle back when it supersedes a press whose
    /// release never arrived; honoring that would cancel the drag the
    /// user is making now.
    #[test]
    fn a_stale_handle_cannot_end_the_drag_that_replaced_it() {
        let mut backend = ledger();
        let first = backend.grab_pointer_for_drag();
        let second = backend.grab_pointer_for_drag();
        backend.ungrab_pointer(first);
        assert!(backend.pointer_grab.is_some(), "the stale handle named a grab that was already gone");
        backend.ungrab_pointer(second);
        assert!(backend.pointer_grab.is_none());
    }

    /// Handles are unique for the life of the session and never zero —
    /// `DragHandle(0)` is what a backend with no grabs of its own hands
    /// back, and it must never name one of these.
    #[test]
    fn no_handle_is_ever_zero_or_reused() {
        let mut backend = ledger();
        let first = backend.grab_pointer_for_drag();
        let second = backend.grab_pointer_for_drag();
        assert_ne!(first, second);
        assert_ne!(first, DragHandle(0));
        assert_ne!(second, DragHandle(0));
        assert!(!backend.pointer_grab.as_ref().unwrap().holds(DragHandle(0)));
    }

    /// Both ends of the grab ask the seat for the half routing cannot
    /// do: the client under the pointer is sent a leave when a drag
    /// takes it, and an enter when the drag gives it back.
    #[test]
    fn each_end_of_the_grab_is_announced_to_the_seat() {
        let mut backend = ledger();
        let handle = backend.grab_pointer_for_drag();
        assert!(matches!(backend.pending_pointer_grab, Some(PointerGrabChange::Taken)));
        backend.pending_pointer_grab = None;
        backend.ungrab_pointer(handle);
        assert!(matches!(backend.pending_pointer_grab, Some(PointerGrabChange::Released)));
        // An ungrab that ends nothing announces nothing: a repeat would
        // send the client under the pointer a second enter for a leave
        // it never got.
        backend.pending_pointer_grab = None;
        backend.ungrab_pointer(handle);
        assert!(backend.pending_pointer_grab.is_none());
    }

    /// The buttons are the drag's lifetime. A grab still held once they
    /// are all up has been leaked by its owner, and a leaked grab
    /// routes every pointer event into a gesture that finished — no
    /// client clickable, no menu openable, until the session is killed.
    #[test]
    fn a_grab_outlives_neither_the_buttons_nor_its_surface() {
        let grab = DragGrab::new(DragHandle(7));
        assert!(!grab.expired(true, true), "a drag in progress is not expired");
        assert!(grab.expired(false, true), "the last button lifted");
        assert!(grab.expired(true, false), "nothing left to drag");
    }

    /// The surface half of that, against a real ledger: a client that
    /// dies mid-drag takes its record with it, and the grab anchored on
    /// it has nothing left to move however firmly the user is still
    /// holding the button. An unlatched grab has seen no pointer event
    /// yet, so nothing under it can have died.
    #[test]
    fn an_anchor_is_alive_only_while_its_record_is() {
        let backend = ledger();
        let mut grab = DragGrab::new(DragHandle(7));
        assert!(grab.anchor_alive(&backend), "nothing latched yet");
        grab.anchor(Some(PressTarget::Root), &Hit::Root);
        assert!(grab.anchor_alive(&backend), "the desktop outlives every drag on it");

        let mut grab = DragGrab::new(DragHandle(8));
        grab.anchor(Some(PressTarget::Content(WINDOW)), &Hit::Root);
        assert!(!grab.anchor_alive(&backend), "the window is not in the ledger");

        let mut grab = DragGrab::new(DragHandle(9));
        grab.anchor(Some(PressTarget::Frame(FRAME)), &Hit::Root);
        assert!(!grab.anchor_alive(&backend), "the frame is not in the ledger");
    }

    // -- shadow-band ownership ---------------------------------------
    // The boundary rule for a client-decorated window whose buffer is
    // bigger than the window it declared. Pinned with GTK's real
    // numbers (LibreOffice's template dialog declares
    // `set_window_geometry(26, 23, 818, 651)`), because the failure on
    // either side of the line is user-visible: claim the shadow and a
    // press in thin air starts a drag on this window instead of the
    // one visibly under it; claim strictly the geometry and the
    // client's own edge grips are a pixel-hunt.

    #[test]
    fn the_declared_window_geometry_is_owned_and_the_shadow_is_not() {
        let content = Rect { pos: Point::new(200, 100), size: Size::new(818, 651) };
        let shadow = Point::new(26, 23);
        // Inside the window, corners included.
        assert!(frameless_claims(content, shadow, Point::new(200, 100)));
        assert!(frameless_claims(content, shadow, Point::new(1017, 750)));
        // The far reaches of the shadow band fall through to whatever
        // is beneath — this is the click the shadow used to steal.
        assert!(!frameless_claims(content, shadow, Point::new(200 - 26, 100)));
        assert!(!frameless_claims(content, shadow, Point::new(200, 100 - 23)));
        assert!(!frameless_claims(content, shadow, Point::new(1017 + 26, 750)));
    }

    #[test]
    fn a_thin_grip_band_of_the_shadow_still_belongs_to_the_window() {
        let content = Rect { pos: Point::new(200, 100), size: Size::new(818, 651) };
        let shadow = Point::new(26, 23);
        // Just outside the visible edge: the client's resize grip.
        assert!(frameless_claims(content, shadow, Point::new(200 - RESIZE_MARGIN, 100)));
        assert!(frameless_claims(content, shadow, Point::new(200, 100 - RESIZE_MARGIN)));
        assert!(frameless_claims(
            content,
            shadow,
            Point::new(200 + 818 + RESIZE_MARGIN - 1, 100 + 651 + RESIZE_MARGIN - 1),
        ));
        // One past the margin is shadow again.
        assert!(!frameless_claims(content, shadow, Point::new(200 - RESIZE_MARGIN - 1, 100)));
    }

    /// A window with no declared shadow claims exactly its rectangle:
    /// the margin is carved out of the client's own oversized buffer,
    /// never out of a neighbour's pixels.
    #[test]
    fn no_shadow_means_no_margin() {
        let content = Rect { pos: Point::new(50, 50), size: Size::new(100, 100) };
        let none = Point::new(0, 0);
        assert!(frameless_claims(content, none, Point::new(50, 50)));
        assert!(!frameless_claims(content, none, Point::new(49, 50)));
        assert!(!frameless_claims(content, none, Point::new(50, 150)));
        // A shadow thinner than the margin clamps the claim to the
        // shadow — there is no buffer past it to press on.
        let thin = Point::new(3, 3);
        assert!(frameless_claims(content, thin, Point::new(47, 50)));
        assert!(!frameless_claims(content, thin, Point::new(46, 50)));
    }

    // -- pointer position mirror -------------------------------------

    /// `Backend::pointer_position` answers from the ledger mirror the
    /// motion path maintains — the fix for the first CSD titlebar drag
    /// anchoring wherever the pointer last crossed the desktop. `None`
    /// before any motion, so `wm-core` refuses a drag it would have to
    /// anchor on a guess.
    #[test]
    fn pointer_position_reports_the_mirrored_location_only_once_one_exists() {
        let mut backend = ledger();
        assert_eq!(backend.pointer_position(), None);
        backend.pointer = Some(Point::new(123, 456));
        assert_eq!(backend.pointer_position(), Some(Point::new(123, 456)));
    }

    // -- cursor subject ----------------------------------------------

    /// While the window manager drags a frame edge, the resize cursor
    /// stays up even though the pointer is far off the chrome — the
    /// grab's anchor answers, not the hit under the pointer.
    #[test]
    fn a_frame_drag_keeps_its_frames_cursor() {
        let mut backend = ledger();
        backend.frame_cursors.insert(FRAME, ResizeEdge::SouthEast);
        backend.grab_pointer_for_drag();
        backend.pointer_grab.as_mut().unwrap().anchor(Some(PressTarget::Frame(FRAME)), &Hit::Root);
        match pointer_subject(&backend, (5.0, 5.0).into()) {
            PointerSubject::Frame(Some(ResizeEdge::SouthEast)) => {}
            _ => panic!("a drag anchored on a frame must show that frame's cursor"),
        }
    }

    /// A drag anchored on client content is the window manager moving
    /// the window: the client was sent a leave, so its cursor must not
    /// show — the compositor's arrow does.
    #[test]
    fn a_content_drag_shows_the_compositors_own_cursor() {
        let mut backend = ledger();
        backend.grab_pointer_for_drag();
        backend.pointer_grab.as_mut().unwrap().anchor(Some(PressTarget::Content(WINDOW)), &Hit::Root);
        assert!(matches!(pointer_subject(&backend, (5.0, 5.0).into()), PointerSubject::Desktop));
    }

    /// With nothing grabbed and nothing under the pointer, the desktop
    /// answers — which is what un-sticks a client's stale cursor
    /// surface the moment the pointer leaves its window.
    #[test]
    fn an_empty_scene_is_the_desktops_cursor() {
        let backend = ledger();
        assert!(matches!(pointer_subject(&backend, (5.0, 5.0).into()), PointerSubject::Desktop));
    }

    // -- client buffer scale -----------------------------------------
    // The hit walk has to describe the same rectangle the renderer
    // draws, and for a client running at 2x those two used to be
    // different rectangles.

    fn probe_at(origin: (f64, f64), position: (f64, f64), scale: f64) -> (f64, f64) {
        let point = surface_probe(origin.into(), position.into(), scale);
        (point.x, point.y)
    }

    /// An unscaled client is measured exactly as it always was: the
    /// probe is the pointer position itself, and the origin handed to
    /// the seat is the surface's own.
    #[test]
    fn a_one_to_one_client_is_probed_where_the_pointer_is() {
        assert_eq!(probe_at((100.0, 100.0), (460.0, 220.0), 1.0), (460.0, 220.0));
        let origin = seat_origin((460.0, 220.0).into(), (460.0, 220.0).into(), (100.0, 100.0).into());
        assert_eq!((origin.x, origin.y), (100.0, 100.0));
    }

    /// A 2x client covers 600 device pixels of screen and reports a
    /// 300-pixel surface, so the offset into it halves. Without this
    /// only the top-left quarter of such a window hit-tests at all —
    /// every click in the rest of it fell through to whatever was
    /// behind — and the coordinate the client was handed was twice what
    /// it expected.
    #[test]
    fn a_two_x_client_is_probed_in_its_own_pixels() {
        // 300 device pixels into a window at x=100 is 150 of the
        // client's own.
        assert_eq!(probe_at((100.0, 100.0), (400.0, 200.0), 2.0), (250.0, 150.0));
        // The far edge of a 600-device-pixel-wide window still lands
        // inside its 300-pixel surface, which is the half of the
        // rectangle that used to be unreachable.
        assert_eq!(probe_at((100.0, 100.0), (699.0, 100.0), 2.0).0, 399.5);
    }

    /// What the client is told, which is the other half: smithay
    /// delivers `position - origin` verbatim, so the origin has to be
    /// wherever makes that difference come out in the client's pixels.
    #[test]
    fn a_two_x_client_is_told_where_the_pointer_is_in_its_own_pixels() {
        let position: LogicalPoint<f64, Logical> = (400.0, 200.0).into();
        let anchor: LogicalPoint<f64, Logical> = (100.0, 100.0).into();
        let probe = surface_probe(anchor, position, 2.0);
        let origin = seat_origin(position, probe, anchor);
        assert_eq!((position.x - origin.x, position.y - origin.y), (150.0, 50.0));
    }

    /// A scale the protocol forbids must not turn a window into a
    /// division by zero or an inverted rectangle.
    #[test]
    fn an_impossible_scale_degrades_to_one() {
        assert_eq!(probe_at((100.0, 100.0), (400.0, 200.0), 0.0), (400.0, 200.0));
        assert_eq!(probe_at((100.0, 100.0), (400.0, 200.0), -2.0), (400.0, 200.0));
        assert_eq!(probe_at((100.0, 100.0), (400.0, 200.0), f64::NAN), (400.0, 200.0));
    }

    /// A fractional-scale client divides by its exact fraction: 300
    /// device pixels into a 1.5x window is 200 of the client's own.
    #[test]
    fn a_fractional_client_is_probed_by_its_exact_fraction() {
        assert_eq!(probe_at((100.0, 100.0), (400.0, 250.0), 1.5), (300.0, 200.0));
    }
}
