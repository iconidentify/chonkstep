//! Seat input translation: raw backend events (winit today, libinput
//! under the `session` feature later) into exactly the `BackendEvent`
//! stream `wm-x11` produces, plus direct wl_seat delivery for the input
//! that belongs to clients rather than the window manager.
//!
//! The routing authority here is the same top-down hit-test the
//! renderer paints by: unmanaged override-redirect X11 windows, `above`
//! shell surfaces, frames (each with its client's xdg popups floating
//! over it), `below` shell surfaces (see `backend_impl.rs`'s module doc
//! on stacking bands). On X11 this routing was the server's job —
//! event windows, passive grabs, replay — and `wm-x11` merely
//! translated what the server had already decided. A compositor IS the
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
//! Presses that hit nothing at all queue under [`ROOT_SHELL`] — the
//! sentinel `Compositor::dispatch_pending` splits off into
//! `Shell::on_root_press`, standing in for the root-window id the X11
//! loop compares against.
//!
//! The cross-event routing state ([`InputState`]) lives in the seat's
//! user-data map rather than as a `Compositor` field: the seat is the
//! thing whose events the state describes, the map ties the lifetime to
//! it for free, and the handlers here stay the only code that can see
//! it.

use std::cell::RefCell;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
    KeyState, KeyboardKeyEvent, MouseButton as InputMouseButton, PointerAxisEvent,
    PointerButtonEvent, PointerMotionEvent,
};
use smithay::desktop::utils::under_from_surface_tree;
use smithay::desktop::{PopupManager, WindowSurfaceType};
use smithay::input::keyboard::{FilterResult, Keycode, ModifiersState};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::input::Seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point as LogicalPoint, SERIAL_COUNTER};

use wm_core::{BackendEvent, KeyCombo, Modifiers, MouseButton, SurfaceRef};
use wm_theme_api::Point;

use crate::state::{
    Compositor, ManagedSurface, StackEntry, WaylandBackend, WlFrameId, WlShellId, WlWindowId,
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
    /// The frame the pointer was inside (chrome or content) at the last
    /// motion — crossing INTO a frame emits `BackendEvent::PointerEnter`
    /// exactly once, which is what focus-follows-mouse keys off. X11
    /// gave us EnterNotify for free; here the crossing is detected by
    /// comparing consecutive hit-tests.
    hovered_frame: Option<WlFrameId>,
    /// X11-style implicit pointer grab: set on the first button press,
    /// held until the last button release, and every pointer event in
    /// between routes to the press's target rather than whatever is
    /// under the pointer now.
    implicit_grab: Option<ImplicitGrab>,
    /// Keycodes whose press was intercepted (a grabbed combo, or any
    /// press during the modal keyboard grab) — their releases are
    /// swallowed too, so a client never sees a release for a press it
    /// never got. This is what an X11 passive grab did server-side; a
    /// stray release confuses stateful clients (games, VMs) even though
    /// most toolkits shrug it off.
    suppressed_keys: Vec<Keycode>,
}

struct ImplicitGrab {
    target: PressTarget,
    /// Number of buttons currently held. X11 keeps the original grab
    /// window when further buttons press mid-grab, and releases only
    /// end the grab when the last button lifts — mirrored exactly.
    buttons: u32,
}

/// Where a button press landed — the routing target its whole implicit
/// grab inherits.
#[derive(Clone, Copy)]
enum PressTarget {
    Shell(WlShellId),
    Frame(WlFrameId),
    Content(WlWindowId),
    Root,
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

/// One entry point for `run()`'s event loop: translate and route a
/// single input event. Generic over the smithay input backend so the
/// winit dev loop and a future libinput session share every line of
/// routing policy — only the raw event types differ.
pub(crate) fn process_input_event<I: InputBackend>(state: &mut Compositor, event: InputEvent<I>) {
    match event {
        InputEvent::Keyboard { event } => on_keyboard_key::<I>(state, event),
        InputEvent::PointerMotionAbsolute { event } => on_pointer_move_absolute::<I>(state, event),
        InputEvent::PointerMotion { event } => on_pointer_move_relative::<I>(state, event),
        InputEvent::PointerButton { event } => on_pointer_button::<I>(state, event),
        InputEvent::PointerAxis { event } => on_pointer_axis::<I>(state, event),
        // Touch, gestures, device hotplug: nothing chonkstep models yet.
        _ => {}
    }
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
fn on_keyboard_key<I: InputBackend>(state: &mut Compositor, event: I::KeyboardKeyEvent) {
    let keycode = event.key_code();
    let key_state = event.state();
    let serial = SERIAL_COUNTER.next_serial();
    let time = event.time_msec();
    let Some(keyboard) = state.seat.get_keyboard() else {
        return;
    };
    let seat = state.seat.clone();
    keyboard.input::<(), _>(state, keycode, key_state, serial, time, |data, mods, handle| {
        // Level-0 (unshifted) keysym, exactly like `wm-x11`'s
        // `keysym_for_keycode` taking the keycode's first sym: a combo
        // bound as Alt+Shift+T must match the T key with SHIFT in the
        // modifier mask, not the keysym 'T' that shift-modified lookup
        // would produce (and Shift+Tab must stay XK_Tab, not
        // ISO_Left_Tab — `wm-core`'s cycle-backwards match depends on
        // it). The latin fallback keeps bindings working on non-latin
        // layouts.
        let keysym = handle
            .raw_latin_sym_or_raw_current_sym()
            .unwrap_or_else(|| handle.modified_sym());
        let combo = KeyCombo { keysym: keysym.raw(), modifiers: combo_modifiers(mods) };
        match key_state {
            KeyState::Pressed => {
                let backend = data.wm.backend_mut();
                if backend.keyboard_grabbed || backend.grabbed_combos.contains(&combo) {
                    backend.queue(WmEvent::KeyPress(combo));
                    with_input(&seat, |input| input.suppressed_keys.push(keycode));
                    FilterResult::Intercept(())
                } else {
                    FilterResult::Forward
                }
            }
            KeyState::Released => {
                let backend = data.wm.backend_mut();
                if backend.keyboard_grabbed {
                    backend.queue(WmEvent::KeyRelease(combo));
                }
                let suppressed = with_input(&seat, |input| {
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
fn on_pointer_move_absolute<I: InputBackend>(
    state: &mut Compositor,
    event: I::PointerMotionAbsoluteEvent,
) {
    let size = state.wm.backend().output_size;
    let position = event.position_transformed((size.w as i32, size.h as i32).into());
    pointer_moved(state, position, event.time_msec());
}

/// Relative motion (the libinput/session path): accumulate onto the
/// current location and clamp to the output — the compositor equivalent
/// of the X server confining the pointer to the screen.
fn on_pointer_move_relative<I: InputBackend>(
    state: &mut Compositor,
    event: I::PointerMotionEvent,
) {
    let size = state.wm.backend().output_size;
    let mut position = state.pointer_location + event.delta();
    position.x = position.x.clamp(0.0, (size.w.max(1) - 1) as f64);
    position.y = position.y.clamp(0.0, (size.h.max(1) - 1) as f64);
    pointer_moved(state, position, event.time_msec());
}

/// The shared motion path: hover/crossing bookkeeping, WM/shell queue
/// routing (honoring an implicit grab), then one seat `motion` +
/// `frame` so smithay's location tracking and client enter/leave stay
/// correct no matter where the event was routed.
fn pointer_moved(state: &mut Compositor, position: LogicalPoint<f64, Logical>, time: u32) {
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
    let hit = hit_at(state.wm.backend(), at, position);

    // Enter/crossing detection, only while no implicit grab holds —
    // X11 suppressed crossing events for the duration of a grab too
    // (the drag's grab mask never selected them), and focus-follows-
    // mouse mid-drag would focus every window a fast drag brushes.
    let now_hovered = match &hit {
        Hit::FrameChrome { frame, .. } => Some(*frame),
        Hit::Content { frame, .. } => *frame,
        _ => None,
    };
    let entered = with_input(&seat, |input| {
        if input.implicit_grab.is_some() || now_hovered == input.hovered_frame {
            None
        } else {
            input.hovered_frame = now_hovered;
            now_hovered
        }
    });
    let grab_target = with_input(&seat, |input| input.implicit_grab.as_ref().map(|g| g.target));

    let backend = state.wm.backend_mut();
    backend.mark_damaged();
    if let Some(frame) = entered {
        backend.queue(WmEvent::PointerEnter { surface: SurfaceRef::Frame(frame) });
    }
    // The client focus this motion carries into the seat. Only content
    // routes focus a client; every other route clears it (generating
    // the wl_pointer.leave a client under the pointer's previous
    // position expects). During a non-content implicit grab the focus
    // is pinned to None so a WM drag never leaks motion into whatever
    // client windows it crosses — the X11 grab hid those too.
    let mut focus: Option<(WlSurface, LogicalPoint<f64, Logical>)> = None;
    match grab_target {
        Some(PressTarget::Shell(shell)) => {
            if let Some(record) = backend.shells.get(&shell) {
                backend
                    .shell_motions
                    .push_back((shell, local_to(at, record.geometry.pos)));
            }
            backend.queue(WmEvent::PointerMotion { root: at, surface_local: None });
        }
        Some(PressTarget::Frame(frame)) => {
            let surface_local = backend
                .frames
                .get(&frame)
                .map(|record| (SurfaceRef::Frame(frame), local_to(at, record.geometry.pos)));
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
            Hit::Root => {
                backend.queue(WmEvent::PointerMotion { root: at, surface_local: None });
            }
        },
    }

    pointer.motion(state, focus, &MotionEvent { location: position, serial, time });
    pointer.frame(state);
}

// -- pointer buttons -----------------------------------------------------

/// Button routing: the press's hit-test establishes (or joins) the
/// implicit grab, and both press and release are dispatched against the
/// grab's target — shell queue, WM event, client seat delivery, or a
/// [`ROOT_SHELL`] click the loop feeds to `shell.on_root_press`.
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

    let hit = hit_at(state.wm.backend(), at, position);
    let target = with_input(&seat, |input| {
        if pressed {
            match input.implicit_grab.as_mut() {
                // A second button mid-grab joins the grab; the original
                // target keeps every event (X11 semantics).
                Some(grab) => {
                    grab.buttons += 1;
                    grab.target
                }
                None => {
                    let target = press_target(&hit);
                    input.implicit_grab = Some(ImplicitGrab { target, buttons: 1 });
                    target
                }
            }
        } else {
            match input.implicit_grab.as_mut() {
                Some(grab) => {
                    let target = grab.target;
                    grab.buttons = grab.buttons.saturating_sub(1);
                    if grab.buttons == 0 {
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

    let backend = state.wm.backend_mut();
    let mut deliver_to_client = false;
    match target {
        PressTarget::Shell(shell) => {
            if let (Some(button), Some(record)) = (button, backend.shells.get(&shell)) {
                backend.shell_clicks.push_back((
                    shell,
                    local_to(at, record.geometry.pos),
                    button,
                    pressed,
                ));
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
            }
        }
        PressTarget::Content(window) => {
            // The WM hears about content clicks too — that's the whole
            // click-to-focus path (`wm-core`'s `handle_client_button`
            // focuses and calls `replay_pointer`, a no-op here because
            // the very next lines deliver the click to the client
            // themselves; no passive-grab race exists to replay
            // around).
            if let Some(button) = button {
                let local = backend
                    .windows
                    .get(&window)
                    .map(|record| local_to(at, record.content.pos))
                    .unwrap_or(at);
                backend.queue(WmEvent::PointerButton {
                    surface: SurfaceRef::Client(window),
                    local,
                    button,
                    pressed,
                    time_ms: time,
                    mods,
                });
            }
            deliver_to_client = true;
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

    if deliver_to_client {
        pointer.button(
            state,
            &ButtonEvent { serial, time, button: event.button_code(), state: event.state() },
        );
        pointer.frame(state);
    }
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
        Hit::Root => PressTarget::Root,
    }
}

// -- pointer axis --------------------------------------------------------

/// Scroll goes to clients only (the WM has no scroll gestures — same as
/// X11, where wheel events were buttons 4/5 and `wm-x11` dropped them).
/// The seat delivers to the current pointer focus, which the motion
/// routing above only ever points at client content — so a scroll over
/// chrome or a shell surface lands nowhere, exactly as intended.
fn on_pointer_axis<I: InputBackend>(state: &mut Compositor, event: I::PointerAxisEvent) {
    let horizontal = event
        .amount(Axis::Horizontal)
        .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0);
    let vertical = event
        .amount(Axis::Vertical)
        .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0);

    let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
    if horizontal != 0.0 {
        frame = frame
            .relative_direction(Axis::Horizontal, event.relative_direction(Axis::Horizontal));
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

// -- hit-testing ---------------------------------------------------------

/// The scene's input authority: what sits under a point, walking the
/// SAME z-order the renderer paints — unmanaged override-redirect X11
/// windows (menus, tooltips) on top, then `above` shells, frames (with
/// each frame's xdg popups floating over its chrome), and `below`
/// shells; the desktop background catches the rest. Any disagreement
/// between this walk and the renderer's makes clicks land on things the
/// user cannot see, so both sides cite `backend_impl.rs`'s
/// stacking-band contract.
fn hit_at(backend: &WaylandBackend, at: Point, position: LogicalPoint<f64, Logical>) -> Hit {
    // Unmanaged override-redirect X11 windows first: they self-position
    // over everything (an open menu overlapping the dock must win the
    // click), and being frameless they live outside `stacking`.
    for (&window, record) in backend.windows.iter() {
        if !record.mapped || !record.content.contains(at) {
            continue;
        }
        let has_frame = backend.frames.values().any(|frame| frame.window == window);
        if has_frame {
            continue;
        }
        if let Some(hit) = content_hit(backend, None, window, position) {
            return hit;
        }
    }

    // `above` shell band (dock, shell menus), topmost stacking entry
    // first.
    for entry in backend.stacking.iter().rev() {
        let StackEntry::Shell(shell) = entry else {
            continue;
        };
        if let Some(record) = backend.shells.get(shell) {
            if record.above && record.mapped && record.geometry.contains(at) {
                return Hit::Shell { shell: *shell, local: local_to(at, record.geometry.pos) };
            }
        }
    }

    // Frame band.
    for entry in backend.stacking.iter().rev() {
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
        if let Some(hit) = popup_hit(backend, *frame, record.window, position) {
            return hit;
        }
        if !record.geometry.contains(at) {
            continue;
        }
        let over_content = backend
            .windows
            .get(&record.window)
            .is_some_and(|window| window.mapped && window.content.contains(at));
        if over_content {
            if let Some(hit) = content_hit(backend, Some(*frame), record.window, position) {
                return hit;
            }
        }
        return Hit::FrameChrome { frame: *frame, local: local_to(at, record.geometry.pos) };
    }

    // `below` shell band (desktop-level furniture).
    for entry in backend.stacking.iter().rev() {
        let StackEntry::Shell(shell) = entry else {
            continue;
        };
        if let Some(record) = backend.shells.get(shell) {
            if !record.above && record.mapped && record.geometry.contains(at) {
                return Hit::Shell { shell: *shell, local: local_to(at, record.geometry.pos) };
            }
        }
    }

    Hit::Root
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
    /// exists (`None` for unmanaged override-redirect X11 windows);
    /// `surface`/`origin` name the exact wl_surface to focus and its
    /// global position (`None` surface for an X11 window whose
    /// wl_surface has not been associated yet — nothing to deliver to,
    /// but the WM still learns about the click). No local coordinate:
    /// `wm-core` ignores it for `SurfaceRef::Client` events, and the
    /// button handler recomputes a content-local point from the record
    /// for the event shape.
    Content {
        frame: Option<WlFrameId>,
        window: WlWindowId,
        surface: Option<WlSurface>,
        origin: LogicalPoint<f64, Logical>,
    },
    /// The desktop background.
    Root,
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
    let content_origin = record.content.pos;
    let (surface, origin) = match &root_surface {
        Some(root) => under_from_surface_tree(
            root,
            position,
            (content_origin.x, content_origin.y),
            WindowSurfaceType::ALL,
        )
        .map(|(surface, origin)| (Some(surface), origin.to_f64()))
        .unwrap_or_else(|| {
            (root_surface.clone(), (content_origin.x as f64, content_origin.y as f64).into())
        }),
        // An X11 window whose wl_surface has not been associated yet:
        // nothing to deliver to, but the click still counts for
        // click-to-focus.
        None => (None, (content_origin.x as f64, content_origin.y as f64).into()),
    };
    Some(Hit::Content { frame, window, surface, origin })
}

/// Tests a window's xdg popup tree (context menus, dropdowns of native
/// Wayland clients). Popup offsets from
/// `PopupManager::popups_for_surface` are parent-surface-relative; the
/// renderer resolves them against the content rect (`content.pos +
/// offset` — see `renderer.rs`'s `push_window_content`) and this walk
/// must resolve them identically or popup clicks land beside the menu
/// the user sees.
fn popup_hit(
    backend: &WaylandBackend,
    frame: WlFrameId,
    window: WlWindowId,
    position: LogicalPoint<f64, Logical>,
) -> Option<Hit> {
    let record = backend.windows.get(&window)?;
    if !record.mapped {
        return None;
    }
    let ManagedSurface::Xdg(toplevel) = &record.surface else {
        // X11 menus arrive as override-redirect windows, handled at the
        // top of `hit_at` — no xdg popups to test here.
        return None;
    };
    let content_origin = record.content.pos;
    for (popup, offset) in PopupManager::popups_for_surface(toplevel.wl_surface()) {
        let popup_origin: LogicalPoint<i32, Logical> =
            (content_origin.x + offset.x, content_origin.y + offset.y).into();
        if let Some((surface, origin)) = under_from_surface_tree(
            popup.wl_surface(),
            position,
            popup_origin,
            WindowSurfaceType::ALL,
        ) {
            return Some(Hit::Content {
                frame: Some(frame),
                window,
                surface: Some(surface),
                origin: origin.to_f64(),
            });
        }
    }
    None
}

/// Root-space point -> a rect's local coordinates.
fn local_to(at: Point, origin: Point) -> Point {
    Point::new(at.x - origin.x, at.y - origin.y)
}
