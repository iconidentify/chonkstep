//! The end-to-end test door: synthetic input over a control socket,
//! for driving a real nested compositor from a test harness.
//!
//! # Why this exists
//!
//! Three shipped regressions in a row lived below the unit-test
//! waterline and were found by a human dragging a mouse around:
//!
//! - **the drag that never ended** — the release went to the client
//!   and `wm-core` never heard it, so the window stayed glued to the
//!   cursor (fixed by `DragEnded`; see `input.rs`'s release routing);
//! - **the click that landed offset** — a stale pointer anchor meant
//!   a client-decorated window teleported by the staleness on its
//!   first drag motion (see `WaylandBackend::pointer`);
//! - **the scale-2 collapse** — advertising scale on the outputs
//!   multiplied every chrome element while the wallpaper stayed
//!   anchored, quartering the desktop (see the long comment above
//!   `advertise_scale` in `state.rs`).
//!
//! Every one of them was findable by the same recipe: boot a nested
//! compositor, inject pointer input, screenshot, assert. The fake
//! backend the unit tests drive can never see them, because they live
//! in the real input path (`input.rs`) and the real renderer. This
//! module is the smallest possible injection seam for that recipe —
//! `crates/chonk-testkit` is the harness on the other end of the
//! socket, and its `#[ignore]`d tests are those three bugs spelled as
//! assertions.
//!
//! # Activation
//!
//! Dead unless `CHONKSTEP_TEST_SOCKET` names a path in the
//! compositor's environment at startup. No env var, no listener, no
//! code path: [`init`] returns before touching the filesystem, so a
//! user session carries nothing but the `is_none` test. This is a
//! debugging door with the same trust model as the screenshot marker
//! (`capture.rs`): anything that can set the compositor's environment
//! and reach the socket path already runs as the user.
//!
//! # The seam
//!
//! Injected events enter through [`crate::input::process_input_event`]
//! — the exact function the winit host-window events enter through —
//! via a synthetic [`smithay::backend::input::InputBackend`]
//! implementation ([`TestInput`]). Everything downstream (routing,
//! implicit grabs, drag grabs, hit tests, seat delivery, the shell
//! queues) is the production code with not one branch knowing the
//! event was synthetic. Injecting anywhere shallower — poking the
//! ledger, calling `wm.dispatch` directly — would test a path no real
//! mouse travels, which is precisely how the three regressions above
//! stayed invisible.
//!
//! # Wire protocol
//!
//! Line-oriented UTF-8 over a `SOCK_STREAM` Unix socket; one command
//! per `\n`-terminated line, fields separated by single spaces.
//! Commands are processed in order, in the compositor's own event
//! loop. Malformed lines answer `err <reason>` and are otherwise
//! ignored — a harness bug must not wedge the compositor.
//!
//! | command | meaning |
//! |---|---|
//! | `motion X Y` | absolute pointer motion to (X, Y) in global (output) coordinates; floats accepted |
//! | `button left\|middle\|right press\|release` | pointer button by name |
//! | `touch down\|motion SLOT X Y` | touch position in global output coordinates |
//! | `touch up SLOT` | release a touch slot |
//! | `touch cancel\|frame` | cancel the touch sequence or finish its input frame |
//! | `key CODE press\|release` | keyboard key by *evdev* keycode (`KEY_*` from input-event-codes.h; the xkb +8 offset is applied here) |
//! | `primary-scale FACTOR` | changes the live primary-output scale through the production IPC mutation path |
//! | `repeat` | replies with the held compositor-binding repeat count and interval, or `repeat none` |
//! | `activation-tokens` | replies with the number of retained xdg-activation tokens |
//! | `protocol-ledgers` | replies with retained input-method popup, idle-inhibitor object, and lock-surface counts |
//! | `protocol-publishes` | replies with native-control and Hyprland event-snapshot, foreign full-sync and foreign dragged-window-sync counters |
//! | `hyprland-sources` | replies with desired and registered Hyprland IPC calloop-source counts |
//! | `memory-stats` | opt-in memory-profile builds only: Rust, allocator and glyph-cache counters; no payloads |
//! | `selection-devices` | read-only retained core/primary/wlr/ext device counts, including dead resources |
//! | `hit X Y` | replies with `hit root\|shell\|frame\|content\|layer\|ime\|lock` from the production scene hit-test |
//! | `barrier` | replies `ok` once every command before it has been dispatched **and** a frame has been rendered with no damage left over |
//! | `windows` | replies one line per ledger entry (see below), then `done` |
//!
//! `windows` reply shape, one record per line, `done` terminated:
//!
//! ```text
//! scale 2
//! output 1280 800
//! theme id="nextstep-classic" name="NeXTSTEP Classic" appearance=dark following=""
//! window id=3 x=100 y=80 w=400 h=300 offset_x=12 offset_y=12 presented_w=424 presented_h=324 mapped=true app="org.gnome.zenity" title="Question"
//! frame id=4 window=3 x=96 y=52 w=408 h=332 mapped=true
//! shell id=1 x=1216 y=0 w=64 h=320 mapped=true above=true buffer_bytes=81920
//! done
//! ```
//!
//! The `theme` line is the shell's own account of what it is dressed
//! in — the id and display name of the theme at 1x, the appearance
//! it resolved in, and `following` (`"omarchy"` while the session
//! follows Omarchy's current theme, empty otherwise) — so a harness
//! can assert a theme *took* without inferring it from pixels, and
//! then use pixels for the half a ledger cannot vouch for.
//!
//! Geometry is in the same physical-pixel space the ledger keeps
//! (`WindowRecord::content` / `FrameRecord::geometry`), which is also
//! the space `motion` coordinates are interpreted in — so a harness
//! can read a titlebar's rectangle off one line and press exactly
//! inside it with no coordinate conversion anywhere.
//!
//! # The barrier
//!
//! `barrier` is what makes every harness wait a bounded poll on an
//! observable condition instead of a sleep. Its contract: by the time
//! `ok` arrives, every earlier command on the connection has been
//! routed (they are dispatched synchronously, in order, as their
//! bytes arrive), the resulting `BackendEvent`s have been drained by
//! `dispatch_pending`, and a frame has been rendered leaving the
//! damage flag clear — so a screenshot taken after the ack shows the
//! world those events produced. Implementation: the command marks the
//! scene damaged and parks the connection; [`after_frame`], called at
//! the tail of every `dispatch_pending` pass, acks every parked
//! connection once the ledger reports no damage and no queued events.
//! A frame that fails to render keeps its damage and therefore keeps
//! the barrier parked; the harness's own timeout is the backstop.

use std::cell::RefCell;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Device, DeviceCapability, Event, GestureBeginEvent,
    GestureEndEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
    InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent,
    PointerMotionAbsoluteEvent, TouchCancelEvent, TouchDownEvent, TouchEvent, TouchFrameEvent,
    TouchMotionEvent, TouchSlot, TouchUpEvent, UnusedEvent,
};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::input::keyboard::Keycode;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};

use crate::state::Compositor;

// -- the synthetic input backend -----------------------------------------

/// The [`InputBackend`] injected commands claim to come from. Never
/// polled for events — the door *constructs* `InputEvent<TestInput>`
/// values and feeds them straight to `process_input_event`, so the
/// backend is purely a type-level statement that these events carry a
/// position, a button code and a keycode like anyone else's.
#[derive(Debug)]
pub(crate) struct TestInput;

/// The one virtual device every injected event reports. Identity only
/// — nothing in `input.rs` routes by device, but the `Event` trait
/// requires one and honesty in logs is worth the ten lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TestDevice;

impl Device for TestDevice {
    fn id(&self) -> String {
        "chonkstep-test-door".into()
    }
    fn name(&self) -> String {
        "chonkstep test door".into()
    }
    fn has_capability(&self, capability: DeviceCapability) -> bool {
        matches!(capability, DeviceCapability::Keyboard | DeviceCapability::Pointer | DeviceCapability::Touch)
    }
    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }
    fn syspath(&self) -> Option<PathBuf> {
        None
    }
}

/// Injected keyboard key. `code` is already xkb-offset (+8 from the
/// evdev code, applied at parse time) because that is what
/// `KeyboardKeyEvent::key_code` promises and what the seat's xkb state
/// consumes.
#[derive(Debug)]
pub(crate) struct TestKeyEvent {
    code: u32,
    state: KeyState,
    time: u64,
}

impl Event<TestInput> for TestKeyEvent {
    fn time(&self) -> u64 {
        self.time
    }
    fn device(&self) -> TestDevice {
        TestDevice
    }
}

impl KeyboardKeyEvent<TestInput> for TestKeyEvent {
    fn key_code(&self) -> Keycode {
        self.code.into()
    }
    fn state(&self) -> KeyState {
        self.state
    }
    fn count(&self) -> u32 {
        1
    }
}

/// Injected pointer button, carrying the same `BTN_*` codes libinput
/// would so client-side delivery (`pointer.button` forwards the raw
/// code) is indistinguishable from a real mouse.
#[derive(Debug)]
pub(crate) struct TestButtonEvent {
    code: u32,
    state: ButtonState,
    time: u64,
}

impl Event<TestInput> for TestButtonEvent {
    fn time(&self) -> u64 {
        self.time
    }
    fn device(&self) -> TestDevice {
        TestDevice
    }
}

impl PointerButtonEvent<TestInput> for TestButtonEvent {
    fn button_code(&self) -> u32 {
        self.code
    }
    fn state(&self) -> ButtonState {
        self.state
    }
}

/// Injected absolute motion, already in global logical coordinates.
/// `x_transformed` ignores the target size on purpose: the protocol
/// speaks output space directly, so the transform winit needs (host
/// window space to output space) is the identity here.
#[derive(Debug)]
pub(crate) struct TestMotionEvent {
    x: f64,
    y: f64,
    time: u64,
}

impl Event<TestInput> for TestMotionEvent {
    fn time(&self) -> u64 {
        self.time
    }
    fn device(&self) -> TestDevice {
        TestDevice
    }
}

impl AbsolutePositionEvent<TestInput> for TestMotionEvent {
    fn x(&self) -> f64 {
        self.x
    }
    fn y(&self) -> f64 {
        self.y
    }
    fn x_transformed(&self, _width: i32) -> f64 {
        self.x
    }
    fn y_transformed(&self, _height: i32) -> f64 {
        self.y
    }
}

impl PointerMotionAbsoluteEvent<TestInput> for TestMotionEvent {}

/// Synthetic touch events go through the same backend dispatch as libinput.
/// Slots are explicit so the suite can prove independent multi-touch routes.
#[derive(Debug)]
pub(crate) struct TestTouchEvent {
    position: TestMotionEvent,
    slot: TouchSlot,
}

impl Event<TestInput> for TestTouchEvent {
    fn time(&self) -> u64 { self.position.time }
    fn device(&self) -> TestDevice { TestDevice }
}

impl AbsolutePositionEvent<TestInput> for TestTouchEvent {
    fn x(&self) -> f64 { self.position.x }
    fn y(&self) -> f64 { self.position.y }
    fn x_transformed(&self, _width: i32) -> f64 { self.position.x }
    fn y_transformed(&self, _height: i32) -> f64 { self.position.y }
}

impl TouchEvent<TestInput> for TestTouchEvent {
    fn slot(&self) -> TouchSlot { self.slot }
}

impl TouchDownEvent<TestInput> for TestTouchEvent {}
impl TouchMotionEvent<TestInput> for TestTouchEvent {}
impl TouchUpEvent<TestInput> for TestTouchEvent {}
impl TouchCancelEvent<TestInput> for TestTouchEvent {}
impl TouchFrameEvent<TestInput> for TestTouchEvent {}

/// Values delivered through the physical swipe event route, for nested tests.
#[derive(Debug)]
pub(crate) struct TestSwipeEvent {
    fingers: u32,
    delta: (f64, f64),
    cancelled: bool,
    time: u64,
}

impl Event<TestInput> for TestSwipeEvent {
    fn time(&self) -> u64 {
        self.time
    }
    fn device(&self) -> TestDevice {
        TestDevice
    }
}
impl GestureBeginEvent<TestInput> for TestSwipeEvent {
    fn fingers(&self) -> u32 {
        self.fingers
    }
}
impl GestureEndEvent<TestInput> for TestSwipeEvent {
    fn cancelled(&self) -> bool {
        self.cancelled
    }
}
impl GestureSwipeBeginEvent<TestInput> for TestSwipeEvent {}
impl GestureSwipeEndEvent<TestInput> for TestSwipeEvent {}
impl GestureSwipeUpdateEvent<TestInput> for TestSwipeEvent {
    fn delta_x(&self) -> f64 {
        self.delta.0
    }
    fn delta_y(&self) -> f64 {
        self.delta.1
    }
}

impl InputBackend for TestInput {
    type Device = TestDevice;
    type KeyboardKeyEvent = TestKeyEvent;
    type PointerAxisEvent = UnusedEvent;
    type PointerButtonEvent = TestButtonEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = TestMotionEvent;
    type GestureSwipeBeginEvent = TestSwipeEvent;
    type GestureSwipeUpdateEvent = TestSwipeEvent;
    type GestureSwipeEndEvent = TestSwipeEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;
    type TouchDownEvent = TestTouchEvent;
    type TouchUpEvent = TestTouchEvent;
    type TouchMotionEvent = TestTouchEvent;
    type TouchCancelEvent = TestTouchEvent;
    type TouchFrameEvent = TestTouchEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = ();
}

// -- the socket ----------------------------------------------------------

/// One accepted harness connection: the stream plus the partial line
/// carried between readable wakeups. Lives inside its calloop
/// `Generic` source; `AsFd` delegates to the stream so calloop polls
/// the right fd.
struct Connection {
    stream: UnixStream,
    buffer: Vec<u8>,
}

impl AsFd for Connection {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

thread_local! {
    /// Connections whose `barrier` is waiting for a rendered frame.
    /// Thread-local for the same reason `capture.rs`'s snapshot clock
    /// is: the compositor is single-threaded by construction, and the
    /// alternative is a field on `Compositor` for a module that must
    /// stay out of a user session's way.
    static PENDING_BARRIERS: RefCell<Vec<UnixStream>> = const { RefCell::new(Vec::new()) };
}

/// Opens the door if — and only if — `CHONKSTEP_TEST_SOCKET` is set.
/// Called once from `run` after the event loop exists; in a user
/// session this is one env lookup and out.
pub(crate) fn init(loop_handle: &LoopHandle<'static, Compositor>) {
    let Some(path) = std::env::var_os("CHONKSTEP_TEST_SOCKET") else {
        return;
    };
    let path = PathBuf::from(path);
    // A stale socket file from a crashed previous run would make bind
    // fail; removing it is safe because the path is the harness's own
    // per-test scratch by contract.
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(?error, path = %path.display(), "test door: could not bind the control socket");
            return;
        }
    };
    if let Err(error) = listener.set_nonblocking(true) {
        tracing::error!(?error, "test door: could not make the listener non-blocking");
        return;
    }
    let handle = loop_handle.clone();
    let source = Generic::new(listener, Interest::READ, Mode::Level);
    let inserted = loop_handle.insert_source(source, move |_, listener, _comp| {
        // Accept everything ready; Level mode re-fires if more arrive.
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(error) = stream.set_nonblocking(true) {
                        tracing::warn!(?error, "test door: dropping a connection that cannot be non-blocking");
                        continue;
                    }
                    let connection = Connection { stream, buffer: Vec::new() };
                    let source = Generic::new(connection, Interest::READ, Mode::Level);
                    if let Err(error) = handle.insert_source(source, |_, connection, comp| {
                        // SAFETY: the connection is owned by this
                        // source and never moved out of it; only its
                        // stream is read and its buffer mutated, so
                        // the registered fd cannot be closed or
                        // replaced — the same access pattern `run`
                        // uses for the wayland display source.
                        Ok(on_readable(unsafe { connection.get_mut() }, comp))
                    }) {
                        tracing::warn!(?error, "test door: could not register a connection");
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => {
                    tracing::warn!(?error, "test door: accept failed");
                    break;
                }
            }
        }
        Ok(PostAction::Continue)
    });
    match inserted {
        Ok(_) => tracing::info!(path = %path.display(), "test door listening (CHONKSTEP_TEST_SOCKET)"),
        Err(error) => tracing::error!(?error, "test door: could not register the listener"),
    }
}

/// Drains one connection's readable bytes and executes every complete
/// line. Returns `Remove` on EOF or error so calloop drops the source
/// (and with it the stream).
fn on_readable(connection: &mut Connection, comp: &mut Compositor) -> PostAction {
    let mut chunk = [0u8; 1024];
    loop {
        match connection.stream.read(&mut chunk) {
            Ok(0) => return PostAction::Remove,
            Ok(n) => connection.buffer.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) => {
                tracing::warn!(?error, "test door: read failed; closing the connection");
                return PostAction::Remove;
            }
        }
    }
    // Execute complete lines; keep the trailing partial for next time.
    while let Some(end) = connection.buffer.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = connection.buffer.drain(..=end).collect();
        let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
        handle_command(line.trim(), &mut connection.stream, comp);
    }
    PostAction::Continue
}

/// Parses and executes one command line. Injection goes through
/// [`crate::input::process_input_event`] — the seam; see the module
/// docs for why nowhere shallower would do.
fn handle_command(line: &str, stream: &mut UnixStream, comp: &mut Compositor) {
    let mut words = line.split_whitespace();
    let time = comp.start_time.elapsed().as_micros() as u64;
    let reply_err = |stream: &mut UnixStream, reason: &str| {
        let _ = stream.write_all(format!("err {reason}\n").as_bytes());
    };
    match words.next() {
        Some("primary-scale") => {
            let Some(Ok(scale)) = words.next().map(str::parse::<f64>) else {
                reply_err(stream, "primary-scale wants a numeric FACTOR");
                return;
            };
            let Some(name) = comp.outputs.first().map(|entry| entry.output.name()) else {
                reply_err(stream, "primary-scale needs a connected output");
                return;
            };
            if !comp.set_output_scale(&name, scale) {
                reply_err(stream, "primary-scale FACTOR must be between 0.5 and 4");
            }
        }
        Some("touch") => {
            let action = words.next();
            let event = match action {
                Some("down" | "motion" | "up") => {
                    let Some(Ok(slot)) = words.next().map(str::parse::<u32>) else {
                        reply_err(stream, "touch wants a nonnegative SLOT");
                        return;
                    };
                    // TouchSlot's wire ID is signed; avoid a wrapping cast.
                    if slot > i32::MAX as u32 {
                        reply_err(stream, "touch SLOT exceeds protocol range");
                        return;
                    }
                    let (x, y) = if action == Some("up") {
                        (0.0, 0.0)
                    } else {
                        let (Some(Ok(x)), Some(Ok(y))) =
                            (words.next().map(str::parse::<f64>), words.next().map(str::parse::<f64>))
                        else {
                            reply_err(stream, "touch down|motion wants SLOT X Y");
                            return;
                        };
                        if !x.is_finite() || !y.is_finite() {
                            reply_err(stream, "touch coordinates must be finite");
                            return;
                        }
                        (x, y)
                    };
                    TestTouchEvent { position: TestMotionEvent { x, y, time }, slot: Some(slot).into() }
                }
                Some("cancel" | "frame") => TestTouchEvent {
                    position: TestMotionEvent { x: 0.0, y: 0.0, time }, slot: None.into(),
                },
                _ => {
                    reply_err(stream, "touch wants down|motion|up|cancel|frame");
                    return;
                }
            };
            if words.next().is_some() {
                reply_err(stream, "unexpected touch arguments");
                return;
            }
            let input = match action {
                Some("down") => InputEvent::TouchDown { event },
                Some("motion") => InputEvent::TouchMotion { event },
                Some("up") => InputEvent::TouchUp { event },
                Some("cancel") => InputEvent::TouchCancel { event },
                Some("frame") => InputEvent::TouchFrame { event },
                _ => unreachable!("validated touch action"),
            };
            crate::input::process_input_event::<TestInput>(comp, input);
        }
        Some("input-reset") => {
            match words.next() {
                Some("resume") => crate::input::resynchronise_input_after_resume(comp),
                Some("pause") => crate::input::gestures::cancel(comp),
                Some("device") => crate::input::process_input_event::<TestInput>(comp, InputEvent::DeviceRemoved { device: TestDevice }),
                _ => reply_err(stream, "input-reset wants pause|resume|device"),
            }
        }
        Some(command @ ("swipe" | "swipe-at")) => {
            let time = if command == "swipe-at" {
                let Some(time) = words.next().and_then(|s| s.parse::<u32>().ok()) else {
                    reply_err(stream, "swipe-at wants a millisecond timestamp"); return;
                };
                u64::from(time) * 1000
            } else { time };
            let mut event = TestSwipeEvent {
                fingers: 0,
                delta: (0.0, 0.0),
                cancelled: false,
                time,
            };
            let input = match words.next() {
                Some("begin") => {
                    let Some(fingers) = words
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .filter(|n| (1..=10).contains(n))
                    else {
                        reply_err(stream, "swipe begin wants a finger count from 1 to 10");
                        return;
                    };
                    event.fingers = fingers;
                    InputEvent::GestureSwipeBegin { event }
                }
                Some("update") => {
                    let (Some(x), Some(y)) = (
                        words.next().and_then(|s| s.parse::<f64>().ok()),
                        words.next().and_then(|s| s.parse::<f64>().ok()),
                    ) else {
                        reply_err(stream, "swipe update wants DX DY");
                        return;
                    };
                    if !x.is_finite() || !y.is_finite() {
                        reply_err(stream, "swipe deltas must be finite");
                        return;
                    }
                    event.delta = (x, y);
                    InputEvent::GestureSwipeUpdate { event }
                }
                Some("end") => InputEvent::GestureSwipeEnd { event },
                Some("cancel") => {
                    event.cancelled = true;
                    InputEvent::GestureSwipeEnd { event }
                }
                _ => {
                    reply_err(stream, "swipe wants begin N|update DX DY|end|cancel");
                    return;
                }
            };
            if words.next().is_some() {
                reply_err(stream, "unexpected swipe arguments");
                return;
            }
            crate::input::process_input_event::<TestInput>(comp, input);
        }
        Some("motion") => {
            let (Some(Ok(x)), Some(Ok(y))) =
                (words.next().map(str::parse::<f64>), words.next().map(str::parse::<f64>))
            else {
                reply_err(stream, "motion wants: motion X Y");
                return;
            };
            crate::input::process_input_event::<TestInput>(
                comp,
                InputEvent::PointerMotionAbsolute { event: TestMotionEvent { x, y, time } },
            );
        }
        // Relative motion, which no other route on this backend can
        // produce: winit's `PointerMotionEvent` is `UnusedEvent`, so a
        // nested session emits absolute events only. Pointer
        // constraints and the relative-pointer protocol are decided on
        // the relative path, and without this they were unreachable
        // from a test. Routed through the same function the virtual
        // pointer uses, so what a test drives and what a client drives
        // are the same code.
        Some("motion-relative") => {
            let (Some(Ok(dx)), Some(Ok(dy))) =
                (words.next().map(str::parse::<f64>), words.next().map(str::parse::<f64>))
            else {
                reply_err(stream, "motion-relative wants: motion-relative DX DY");
                return;
            };
            crate::input::inject_pointer_motion(comp, dx, dy, time as u32);
        }
        Some("button") => {
            // input-event-codes.h values, the same ones a mouse sends.
            let code = match words.next() {
                Some("left") => 0x110,
                Some("middle") => 0x112,
                Some("right") => 0x111,
                _ => {
                    reply_err(stream, "button wants: button left|middle|right press|release");
                    return;
                }
            };
            let Some(state) = parse_button_state(words.next()) else {
                reply_err(stream, "button wants: button left|middle|right press|release");
                return;
            };
            crate::input::process_input_event::<TestInput>(
                comp,
                InputEvent::PointerButton { event: TestButtonEvent { code, state, time } },
            );
        }
        Some("key") => {
            let Some(Ok(code)) = words.next().map(str::parse::<u32>) else {
                reply_err(stream, "key wants: key EVDEV_CODE press|release");
                return;
            };
            let state = match words.next() {
                Some("press") => KeyState::Pressed,
                Some("release") => KeyState::Released,
                _ => {
                    reply_err(stream, "key wants: key EVDEV_CODE press|release");
                    return;
                }
            };
            crate::input::process_input_event::<TestInput>(
                comp,
                // +8: evdev keycode to xkb keycode, the offset every
                // real backend (winit, libinput) applies before the
                // seam. Taking evdev codes on the wire keeps the
                // protocol in the vocabulary input-event-codes.h
                // documents.
                InputEvent::Keyboard { event: TestKeyEvent { code: code + 8, state, time } },
            );
        }
        Some("barrier") => {
            // Damage guarantees `dispatch_pending` will render a frame
            // this pass, which is what arms `after_frame` to ack. See
            // the module docs for the full contract.
            comp.wm.backend_mut().mark_damaged();
            match stream.try_clone() {
                Ok(clone) => PENDING_BARRIERS.with(|pending| pending.borrow_mut().push(clone)),
                Err(error) => {
                    tracing::warn!(?error, "test door: could not park a barrier");
                    reply_err(stream, "barrier could not be parked");
                }
            }
        }
        Some("repeat") => {
            let reply = match crate::input::repeating_binding_status(comp) {
                Some((emitted, interval)) => {
                    format!("repeat emitted={emitted} interval_us={}\n", interval.as_micros())
                }
                None => "repeat none\n".to_string(),
            };
            let _ = stream.write_all(reply.as_bytes());
        }
        Some("activation-tokens") => {
            let count = comp.core_protocols.activation.tokens().count();
            let _ = stream.write_all(format!("activation-tokens {count}\n").as_bytes());
        }
        Some("protocol-ledgers") => {
            let ime = comp.wm.backend().ime_popups.len();
            let idle = comp.idle.inhibitor_count();
            let lock = comp.wm.backend().lock_surfaces.len();
            let _ = stream.write_all(format!("protocol-ledgers ime={ime} idle={idle} lock={lock}\n").as_bytes());
        }
        Some("protocol-publishes") => {
            let metrics = comp.protocol_publish_metrics;
            let _ = stream.write_all(
                format!(
                    "protocol-publishes control={} hyprland={} foreign_full={} foreign_drag={}\n",
                    comp.shell.control_snapshot_builds(),
                    metrics.hyprland_event_snapshots,
                    metrics.foreign_toplevel_full_syncs,
                    metrics.foreign_toplevel_drag_syncs,
                )
                .as_bytes(),
            );
        }
        Some("selection-devices") => {
            let counts = smithay::wayland::selection::selection_device_statistics(&comp.seat);
            let _ = stream.write_all(format!(
                "selection-devices core={} primary={} wlr={} ext={} dead={}\n",
                counts.core, counts.primary, counts.wlr, counts.ext, counts.dead,
            ).as_bytes());
        }
        #[cfg(feature = "memory-profile")]
        Some("memory-stats") => {
            let rust = crate::memory_profile::rust_allocations();
            let allocator = crate::memory_profile::allocator_statistics();
            let fonts = comp.shell.font_cache_statistics();
            let _ = stream.write_all(format!(
                "memory-stats version=1 rust_live_bytes={} rust_peak_bytes={} rust_operations={} rust_allocated_bytes={} glibc_supported={} glibc_arena_bytes={} glibc_in_use_bytes={} glibc_free_bytes={} glibc_mapped_bytes={} glibc_top_releasable_bytes={} font_faces={} glyph_images={} glyph_image_bytes={} glyph_outlines={} glyph_outline_bytes={}\n",
                rust.live_bytes, rust.peak_bytes, rust.operations, rust.allocated_bytes,
                u8::from(allocator.supported), allocator.arena_bytes, allocator.in_use_bytes,
                allocator.free_bytes, allocator.mapped_bytes, allocator.top_releasable_bytes,
                fonts.available_faces, fonts.image_entries, fonts.image_payload_bytes,
                fonts.outline_entries, fonts.outline_payload_bytes,
            ).as_bytes());
        }
        Some("frame-stats") => {
            let stats = std::mem::take(&mut comp.frame_stats);
            let micros = |duration: std::time::Duration| duration.as_micros();
            let buckets = |values: &[u64; 16]| {
                values.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
            };
            let _ = stream.write_all(
                format!(
                    "frame-stats dispatch_calls={} dispatch_us={} dispatch_max_us={} input_us={} shell_us={} protocol_us={} layout_us={} render_calls={} render_us={} render_max_us={} flush_us={} ipc_us={} dispatch_hist={} render_hist={} render_attempts={} gesture_build_calls={} gesture_build_us={} gesture_build_max_us={}\n",
                    stats.dispatch.calls,
                    micros(stats.dispatch.total),
                    micros(stats.dispatch.max),
                    micros(stats.input.total),
                    micros(stats.shell.total),
                    micros(stats.protocols.total),
                    micros(stats.layout.total),
                    stats.render.calls,
                    micros(stats.render.total),
                    micros(stats.render.max),
                    micros(stats.flush.total),
                    micros(stats.ipc.total),
                    buckets(&stats.dispatch_histogram),
                    buckets(&stats.render_histogram),
                    stats.render_attempts,
                    stats.gesture_build.calls, micros(stats.gesture_build.total), micros(stats.gesture_build.max),
                )
                .as_bytes(),
            );
        }
        Some("hyprland-sources") => {
            let (desired, registered) = comp.hyprland_ipc_source_counts();
            let _ = stream.write_all(
                format!("hyprland-sources desired={desired} registered={registered}\n").as_bytes(),
            );
        }
        Some("hit") => {
            let (Some(Ok(x)), Some(Ok(y))) =
                (words.next().map(str::parse::<i32>), words.next().map(str::parse::<i32>))
            else {
                reply_err(stream, "hit wants: hit X Y");
                return;
            };
            let kind =
                crate::input::hit_kind_at(comp.wm.backend(), wm_theme_api::Point::new(x, y));
            let _ = stream.write_all(format!("hit {kind}\n").as_bytes());
        }
        Some("windows") => {
            let mut reply = String::new();
            let backend = comp.wm.backend();
            let logical_focus = comp.wm.focused_client().and_then(|id| comp.wm.client(id)).map(|c| c.window);
            let seat_focus = comp.seat.get_keyboard().and_then(|k| k.current_focus())
                .and_then(|focus| backend.window_for_surface(focus.surface()));
            reply.push_str(&format!("gesture-focus logical={} seat={}\n",
                logical_focus.map_or(0, |w| w.0), seat_focus.map_or(0, |w| w.0)));
            let (owner, (x, y), motion) = crate::input::gestures::inspect(comp);
            reply.push_str(&format!("swipe-stream owner={owner} x={x} y={y} axis={:?}\n", motion.map(|m| m.axis)));
            reply.push_str(&format!("scale {}\n", comp.ui_scale));
            reply.push_str(&format!("workspaces current={} count={}\n",
                comp.wm.current_workspace(), comp.wm.workspace_count()));
            reply.push_str(&format!(
                "output {} {}\n",
                backend.output_size.w, backend.output_size.h
            ));
            let state = comp.shell.session_state();
            reply.push_str(&format!(
                "theme id={:?} name={:?} appearance={} following={:?}\n",
                state.base_theme.id,
                state.base_theme.name,
                state.appearance.name(),
                comp.shell.following().unwrap_or(""),
            ));
            for (id, record) in &backend.windows {
                // The ledger rectangle changes when the compositor sends a
                // configure. The presented extent changes only after the
                // client commits the corresponding buffer. Keeping both in
                // the same reply gives resize/fullscreen tests an observable
                // client-presentation fence instead of a timing guess.
                let presented = record
                    .surface
                    .wl_surface()
                    .and_then(|surface| {
                        with_renderer_surface_state(&surface, |state| state.surface_size())
                            .flatten()
                    })
                    .map(|size| {
                        let factor = backend.window_surface_scale(record);
                        (
                            crate::xdg::scale_length(size.w, factor).max(0) as u32,
                            crate::xdg::scale_length(size.h, factor).max(0) as u32,
                        )
                    })
                    .unwrap_or_default();
                reply.push_str(&format!(
                    "window id={} x={} y={} w={} h={} offset_x={} offset_y={} presented_w={} presented_h={} mapped={} app={:?} title={:?}\n",
                    id.0,
                    record.content.pos.x,
                    record.content.pos.y,
                    record.content.size.w,
                    record.content.size.h,
                    record.content_offset.x,
                    record.content_offset.y,
                    presented.0,
                    presented.1,
                    record.mapped,
                    record.app_id.as_deref().unwrap_or(""),
                    record.title.as_deref().unwrap_or(""),
                ));
            }
            for (id, record) in &backend.frames {
                reply.push_str(&format!(
                    "frame id={} window={} x={} y={} w={} h={} mapped={}\n",
                    id.0,
                    record.window.0,
                    record.geometry.pos.x,
                    record.geometry.pos.y,
                    record.geometry.size.w,
                    record.geometry.size.h,
                    record.mapped,
                ));
            }
            // Shell surfaces too — the dock, the pager, menus. The
            // scale-2 regression was precisely "the ledger says the
            // dock is at the right edge, the renderer drew it
            // elsewhere", so a test needs the ledger half from here
            // and the pixel half from a screenshot.
            for (id, record) in &backend.shells {
                reply.push_str(&format!(
                    "shell id={} x={} y={} w={} h={} mapped={} above={} buffer_bytes={}\n",
                    id.0,
                    record.geometry.pos.x,
                    record.geometry.pos.y,
                    record.geometry.size.w,
                    record.geometry.size.h,
                    record.mapped,
                    record.above,
                    record.buffer_bytes,
                ));
            }
            if let Some(overview) = &backend.overview {
                reply.push_str(&format!(
                    "overview selected={} label_bytes={} preview_edge={} progress={}\n",
                    overview.selected,
                    overview.label_bytes(),
                    backend.preview_edge.unwrap_or(0), overview.progress
                ));
                for window in &overview.windows {
                    let r = window.destination;
                    reply.push_str(&format!(
                        "overview-window id={} x={} y={} w={} h={} source_w={} source_h={}\n",
                        window.window.0,
                        r.pos.x,
                        r.pos.y,
                        r.size.w,
                        r.size.h,
                        window.source.size.w,
                        window.source.size.h
                    ));
                }
                if let Some(drag) = overview.drag {
                    if let Some(window) = overview.windows.get(drag.index) {
                        let r = drag.destination;
                        reply.push_str(&format!(
                            "overview-drag id={} x={} y={} w={} h={} target={}\n",
                            window.window.0, r.pos.x, r.pos.y, r.size.w, r.size.h,
                            drag.workspace.map_or(-1, |index| index as i64)));
                    }
                }
            }
            if let Some(scene) = &backend.gesture_scene {
                reply.push_str(&format!("gesture kind={} axis={:?} x={} y={} raw_progress={} progress={} velocity={} projected={} target={} settling={} origin={} previous={} next={}\n",
                    if scene.horizontal() { "workspace" } else { "overview" }, scene.motion.axis,
                    scene.motion.x, scene.motion.y, scene.motion.progress, scene.position, scene.velocity,
                    scene.projected, scene.spring.map_or(f64::NAN, |s| s.target), scene.spring.is_some(), scene.origin,
                    scene.previous.map_or(-1, |i| i as i64), scene.next.map_or(-1, |i| i as i64)));
            }
            reply.push_str("done\n");
            let _ = stream.write_all(reply.as_bytes());
        }
        Some(other) => reply_err(stream, &format!("unknown command {other:?}")),
        None => {}
    }
}

fn parse_button_state(word: Option<&str>) -> Option<ButtonState> {
    match word {
        Some("press") => Some(ButtonState::Pressed),
        Some("release") => Some(ButtonState::Released),
        _ => None,
    }
}

/// Acks parked barriers once the pass has left nothing behind: no
/// scene damage (the frame the barrier forced has rendered and
/// cleared it) and no queued backend events. Called at the tail of
/// every `dispatch_pending`; with no door open the thread-local is an
/// empty vec and this is a check and a return.
pub(crate) fn after_frame(comp: &mut Compositor) {
    PENDING_BARRIERS.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending.is_empty() {
            return;
        }
        let backend = comp.wm.backend();
        if backend.damage || !backend.pending.is_empty() {
            return;
        }
        for mut stream in pending.drain(..) {
            // A harness that hung up mid-barrier is its own problem;
            // the write result is deliberately ignored.
            let _ = stream.write_all(b"ok\n");
        }
    });
}
