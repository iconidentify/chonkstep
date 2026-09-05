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
//! | `key CODE press\|release` | keyboard key by *evdev* keycode (`KEY_*` from input-event-codes.h; the xkb +8 offset is applied here) |
//! | `repeat` | replies with the held compositor-binding repeat count and interval, or `repeat none` |
//! | `activation-tokens` | replies with the number of retained xdg-activation tokens |
//! | `protocol-ledgers` | replies with retained input-method popup, idle-inhibitor object, and lock-surface counts |
//! | `protocol-publishes` | replies with native-control and Hyprland event-snapshot, foreign full-sync and foreign dragged-window-sync counters |
//! | `hyprland-sources` | replies with desired and registered Hyprland IPC calloop-source counts |
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
//! window id=3 x=100 y=80 w=400 h=300 offset_x=12 offset_y=12 mapped=true app="org.gnome.zenity" title="Question"
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
    AbsolutePositionEvent, ButtonState, Device, DeviceCapability, Event, InputBackend, InputEvent,
    KeyState, KeyboardKeyEvent, PointerButtonEvent, PointerMotionAbsoluteEvent, UnusedEvent,
};
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
        matches!(capability, DeviceCapability::Keyboard | DeviceCapability::Pointer)
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

impl InputBackend for TestInput {
    type Device = TestDevice;
    type KeyboardKeyEvent = TestKeyEvent;
    type PointerAxisEvent = UnusedEvent;
    type PointerButtonEvent = TestButtonEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = TestMotionEvent;
    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;
    type TouchDownEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
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
        Some("frame-stats") => {
            let stats = std::mem::take(&mut comp.frame_stats);
            let micros = |duration: std::time::Duration| duration.as_micros();
            let buckets = |values: &[u64; 16]| {
                values.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
            };
            let _ = stream.write_all(
                format!(
                    "frame-stats dispatch_calls={} dispatch_us={} dispatch_max_us={} input_us={} shell_us={} protocol_us={} layout_us={} render_calls={} render_us={} render_max_us={} flush_us={} ipc_us={} dispatch_hist={} render_hist={}\n",
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
            reply.push_str(&format!("scale {}\n", comp.ui_scale));
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
                reply.push_str(&format!(
                    "window id={} x={} y={} w={} h={} offset_x={} offset_y={} mapped={} app={:?} title={:?}\n",
                    id.0,
                    record.content.pos.x,
                    record.content.pos.y,
                    record.content.size.w,
                    record.content.size.h,
                    record.content_offset.x,
                    record.content_offset.y,
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
