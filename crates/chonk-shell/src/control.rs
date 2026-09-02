//! The control socket: the shell's state, narrated to whoever asks.
//!
//! A third-party bar — the Quickshell one Omarchy ships, a Waybar, a
//! `socat` in a terminal — connects to
//! `$XDG_RUNTIME_DIR/chonkstep/control-<display>.sock`, is handed the
//! whole state of the desktop as a handful of JSON lines, and is then
//! told each time one of those lines would read differently. It may
//! send two requests back: "say it all again" and "switch to that
//! workspace". That is the entire protocol, and `docs/control-socket.md`
//! is its normative text: the wire format lives there, not here, and a
//! disagreement between this file and that document is a bug in this
//! file (the document's own preamble points back here for the
//! implementation, not for the contract). A bar author should read the
//! document and never need this module; it is written so the examples
//! in the document are exactly what a client sees.
//!
//! # Two invariants
//!
//! **The shell never blocks on a client.** Every socket the shell holds
//! is `O_NONBLOCK` from the syscall that created it (`accept4`, see
//! `chonk_dock_proto::transport`), every read and write is a single
//! non-blocking pass, and a client that stops reading is disconnected
//! when the shell's buffer for it crosses [`OUTBOUND_CAP`] rather than
//! being waited for. This is the same rule the dockapp host lives by,
//! for the same reason: a bar that hangs must cost the user that bar,
//! never the desktop.
//!
//! A client that shuts down only its writing side has said its last
//! request, not goodbye. `printf '{"request":"snapshot"}' | socat -
//! UNIX-CONNECT:…` does exactly that the instant the pipe drains, and
//! still expects to read the answer; so does a watcher run with its
//! stdin on `/dev/null`. Such a client keeps receiving — its answers,
//! and every event after — until the peer has closed both directions,
//! which the kernel reports as `POLLHUP` and the shell checks for
//! rather than inferring from a zero-length read.
//!
//! **A client is never told anything twice for no reason.** Each facet
//! of the state — workspaces, outputs, focus, theme — is serialised once
//! per change and sent only when it differs from what was last sent
//! (the `ThemeBroadcast::refresh` shape from the dockapp host). A bar
//! that redraws on every event is therefore cheap by construction, and
//! the socket is silent on a quiet desktop. The one deliberate
//! exception is a `focus-workspace` request naming the workspace the
//! desktop is already on: the spec promises every request an answer,
//! so that client — and only that client — gets the `workspaces` line
//! again as its acknowledgement.
//!
//! # Deliberately absent
//!
//! No window list (`wlr-foreign-toplevel-management` already is one),
//! no keyboard layout, no subscribe verb, no authentication token. The
//! socket is reachable only by the session's own user — a 0700
//! directory, a 0600 socket, and an `SO_PEERCRED` check on accept that
//! restates the same fact — and everything it offers, that user's
//! keyboard already does. There is no configuration key for it either:
//! like the dock socket, it is always on, because a bar that has to be
//! told the socket exists cannot tell the user how to turn it on.
//!
//! # Where it sits in the shell
//!
//! Bound once in `Shell::new`, right after the dockapp host and before
//! the first process meant to see it, so `CHONKSTEP_CONTROL_SOCKET` is
//! in the environment of every autostart entry, `[commands]` launch and
//! menu launch (see `spawn::declare_control_socket` for the route;
//! dockapp tiles are spawned earlier and kept from it on purpose,
//! `spawn::DOCKAPP_WITHHELD_ENV`).
//! Serviced once per `Shell::tick`: accept, read, answer, publish. A
//! bind failure is a warning and a session without a control socket,
//! never a session that failed to start.

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};

use chonk_dock_proto::transport::{self, Stream, StreamListener};
use serde::{Deserialize, Serialize};
use wm_core::{Backend, Lifecycle, WindowManager};
use wm_theme::{Appearance, Theme};
use wm_theme_api::Point;

use crate::spawn;

/// The integer version of `docs/control-socket.md` this build speaks.
pub(crate) const PROTOCOL: u32 = 1;

/// The longest line a client may send, newline included (spec §1). A
/// client whose pending bytes cross this without a newline is
/// disconnected: the biggest legitimate request is under fifty bytes,
/// so anything near the cap is a client that has lost its framing.
pub(crate) const LINE_CAP: usize = 65_536;

/// Bytes the shell will hold for one client that has stopped reading
/// before it gives up on it (spec §1.2). A full snapshot is a few
/// hundred bytes, so this is hundreds of missed snapshots — a client
/// that is merely slow never sees it, a client that is wedged does.
pub(crate) const OUTBOUND_CAP: usize = 262_144;

/// How much one client may hand the shell in one servicing pass.
/// Requests are drained per line as they arrive, so this is not a
/// framing limit; it is a bound on how long the read loop can run
/// against a client writing faster than the shell parses, so a tick
/// stays a tick.
const READ_BUDGET: usize = 2 * LINE_CAP;

// ---------------------------------------------------------------------
// The wire, as types
// ---------------------------------------------------------------------
//
// One struct per message, named after the wire and holding exactly the
// keys the document lists. Serialisation is through the tagged enums so
// the `event`/`request` discriminator is written by serde, not by hand,
// and so an unknown request verb becomes a deserialisation error on one
// path rather than a fall-through somewhere.

/// Shell → client, spec §3. The tag is the `event` key.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub(crate) enum Event {
    Hello(Hello),
    Workspaces(WorkspacesEvent),
    Outputs(OutputsEvent),
    Focus(FocusEvent),
    Theme(ThemeEvent),
    Error(ErrorEvent),
}

/// §3.1. Always the first line a client reads.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct Hello {
    pub protocol: u32,
    /// `"wayland"` or `"x11"`.
    pub session: String,
    pub pid: u32,
}

/// §3.2. The facet a workspace strip is drawn from.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct WorkspacesEvent {
    pub active: usize,
    pub workspaces: Vec<WorkspaceEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct WorkspaceEntry {
    pub index: usize,
    pub windows: usize,
}

/// §3.3.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct OutputsEvent {
    pub focused: Option<usize>,
    pub outputs: Vec<OutputEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct OutputEntry {
    pub index: usize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

/// §3.4.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct FocusEvent {
    pub window: Option<FocusedWindow>,
    pub count: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct FocusedWindow {
    /// Opaque, stable for the window's lifetime: the core's own
    /// `ClientId`, re-encoded (`ClientId::as_u64`).
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub workspace: usize,
}

/// §3.5.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct ThemeEvent {
    pub id: String,
    pub name: String,
    /// `"dark"` or `"light"`.
    pub appearance: String,
    /// `"omarchy"` when the session follows Omarchy's palette
    /// (`SessionState::following`, see `docs/appearance.md`), else
    /// `null`. It reports the choice rather than the outcome: a follow
    /// whose palette is missing wears the flagship but still says
    /// `"omarchy"`, because that is what the desk will wear the moment
    /// Omarchy sets a theme. Reaches here through
    /// [`Surroundings::following`].
    pub following: Option<String>,
}

/// §3.6.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct ErrorEvent {
    /// The `request` value of the message that failed, or `null` when
    /// there was no parseable `request` string to quote back.
    pub request: Option<String>,
    /// For a human; not stable.
    pub message: String,
}

/// Client → shell, spec §4. The tag is the `request` key; a verb this
/// build does not know fails to deserialise, which is what turns it
/// into an `error` event rather than silence.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "request", rename_all = "kebab-case")]
pub(crate) enum Request {
    Snapshot,
    FocusWorkspace { index: usize },
}

/// Everything the shell publishes, as the four facet events it is
/// published as. Built fresh from the live `WindowManager` each tick
/// there is a client to tell; compared facet-by-facet against the last
/// one sent so a quiet desktop is a silent socket.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snapshot {
    pub workspaces: WorkspacesEvent,
    pub outputs: OutputsEvent,
    pub focus: FocusEvent,
    pub theme: ThemeEvent,
}

impl Snapshot {
    /// The facets as events, in the order §3 lists them — the order a
    /// client is promised on accept and on `snapshot`.
    fn events(&self) -> [Event; 4] {
        [
            Event::Workspaces(self.workspaces.clone()),
            Event::Outputs(self.outputs.clone()),
            Event::Focus(self.focus.clone()),
            Event::Theme(self.theme.clone()),
        ]
    }
}

/// What the shell knows that the window manager does not, handed in
/// alongside the `WindowManager` when a [`Snapshot`] is taken.
pub(crate) struct Surroundings<'a> {
    pub theme: &'a Theme,
    pub appearance: Appearance,
    /// The shell's UI scale — one number for every output today.
    pub scale: f32,
    /// The pointer's last known root position, for `outputs.focused`
    /// when the backend cannot report the pointer itself.
    pub pointer_root: Point,
    /// See [`ThemeEvent::following`].
    pub following: Option<String>,
}

/// Reads the four facets out of the live desktop.
///
/// `windows` counts every client the core manages, miniaturised ones
/// included — a window in the Dock is still on its workspace — and
/// withdrawn ones excluded, because a withdrawn client is a table entry
/// for a window that is no longer on any screen. Dock and shell
/// surfaces are not clients at all, so they need no excluding.
pub(crate) fn snapshot<B: Backend>(wm: &WindowManager<B>, surroundings: &Surroundings<'_>) -> Snapshot {
    let mut workspaces: Vec<WorkspaceEntry> =
        (0..wm.workspace_count()).map(|index| WorkspaceEntry { index, windows: 0 }).collect();
    let mut count = 0;
    for (_, client) in wm.iter_clients() {
        if client.lifecycle == Lifecycle::Withdrawn {
            continue;
        }
        count += 1;
        // A client can sit on a workspace index at or past the count
        // only transiently (the core grows the count as it moves the
        // window); a snapshot taken in that instant must not drop the
        // window on the floor, so the list grows to fit it.
        while workspaces.len() <= client.workspace {
            workspaces.push(WorkspaceEntry { index: workspaces.len(), windows: 0 });
        }
        workspaces[client.workspace].windows += 1;
    }

    let window = wm.focused_client().and_then(|id| wm.client(id).map(|client| (id, client))).map(|(id, client)| FocusedWindow {
        id: id.as_u64(),
        title: client.title.clone(),
        app_id: client.class.clone(),
        workspace: client.workspace,
    });

    let monitors = wm.monitors();
    let outputs = if monitors.is_empty() {
        // A backend that names no outputs still has a screen; §3.3
        // says to describe it as one output called "screen".
        let size = wm.backend().screen_size();
        OutputsEvent {
            focused: Some(0),
            outputs: vec![OutputEntry {
                index: 0,
                name: "screen".to_string(),
                x: 0,
                y: 0,
                width: size.w,
                height: size.h,
                scale: surroundings.scale,
            }],
        }
    } else {
        let pointer = wm.backend().pointer_position().unwrap_or(surroundings.pointer_root);
        OutputsEvent {
            focused: Some(wm.monitor_index_at(pointer)),
            outputs: monitors
                .iter()
                .enumerate()
                .map(|(index, monitor)| OutputEntry {
                    index,
                    name: monitor.name.clone(),
                    x: monitor.geometry.pos.x,
                    y: monitor.geometry.pos.y,
                    width: monitor.geometry.size.w,
                    height: monitor.geometry.size.h,
                    scale: surroundings.scale,
                })
                .collect(),
        }
    };

    Snapshot {
        workspaces: WorkspacesEvent { active: wm.current_workspace(), workspaces },
        outputs,
        focus: FocusEvent { window, count },
        theme: ThemeEvent {
            id: surroundings.theme.id.clone(),
            name: surroundings.theme.name.clone(),
            appearance: surroundings.appearance.name().to_string(),
            following: surroundings.following.clone(),
        },
    }
}

/// One JSON line: the object and exactly one `\n`.
fn line(event: &Event) -> Vec<u8> {
    // Every type here serialises infallibly (no maps with non-string
    // keys, no floats that are NaN), so a failure would be a bug in
    // the derive, not in client bytes; there is nothing sensible to do
    // with one but say so loudly.
    let mut bytes = serde_json::to_vec(event).expect("control events serialise infallibly");
    bytes.push(b'\n');
    bytes
}

/// Reads one client line into a [`Request`], or into the `error` event
/// that answers it.
///
/// Two passes on purpose. The first reads the line as a bare JSON
/// value so the `request` string can be quoted back in the error even
/// when the rest of the message is wrong (`"index": "two"`, say); the
/// second is the typed parse. A line that is not a JSON object at all
/// has no verb to quote, so its error carries `"request": null` — the
/// distinction §3.6 draws.
pub(crate) fn parse_request(bytes: &[u8]) -> Result<Request, ErrorEvent> {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(error) => return Err(ErrorEvent { request: None, message: format!("not a JSON message: {error}") }),
    };
    let Some(object) = value.as_object() else {
        return Err(ErrorEvent { request: None, message: "a request must be a JSON object".to_string() });
    };
    let verb = object.get("request").and_then(serde_json::Value::as_str).map(str::to_string);
    if verb.is_none() {
        return Err(ErrorEvent { request: None, message: "a request must carry a \"request\" string".to_string() });
    }
    serde_json::from_value::<Request>(value).map_err(|error| ErrorEvent { request: verb, message: error.to_string() })
}

/// Something a client asked for that only the shell, holding the
/// `WindowManager`, can do. Handed back from [`ControlSocket::service`]
/// rather than done inside it because this module never holds a
/// mutable window manager — it reads a snapshot and writes lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    /// `focus-workspace`, already validated against the workspace list
    /// in the snapshot the request was serviced with. The shell applies
    /// it with `WindowManager::switch_workspace`, which cannot create a
    /// workspace at an index that already exists — so the validation
    /// here is what keeps the spec's "a switch, never a create".
    FocusWorkspace(usize),
}

// ---------------------------------------------------------------------
// One connected client
// ---------------------------------------------------------------------

struct ControlClient {
    stream: Stream,
    /// Bytes received and not yet terminated by a newline.
    inbound: Vec<u8>,
    /// Lines queued and not yet accepted by the kernel.
    outbound: Vec<u8>,
    /// Owed the whole snapshot on the next publish: freshly accepted,
    /// or asked for one. Receives all four facets in §3 order and no
    /// separate delta that pass.
    wants_snapshot: bool,
    /// Owed the `workspaces` facet on the next publish whether or not
    /// it changed — the acknowledgement of a `focus-workspace` that
    /// named the workspace already active. See the module doc.
    owed_workspaces: bool,
    /// The peer has shut down its writing side: no request will ever
    /// arrive again, so reading stops, but writing goes on until the
    /// peer closes the reading side too (see the module doc).
    peer_finished: bool,
    /// Let go this pass; the fd closes when the pass ends. Marked
    /// rather than removed inline because the marking happens inside
    /// loops over the client list.
    doomed: bool,
}

/// Why a client was let go. Logged, never sent: the connection is the
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Farewell {
    ClosedByPeer,
    LineOverflow,
    OutboundOverflow,
    ReadError,
    WriteError,
}

impl ControlClient {
    fn new(stream: Stream) -> Self {
        Self {
            stream,
            inbound: Vec::new(),
            outbound: Vec::new(),
            wants_snapshot: true,
            owed_workspaces: false,
            peer_finished: false,
            doomed: false,
        }
    }

    /// Ends this client. Half of the work is immediate — the socket is
    /// shut down so the peer reads EOF now rather than when the list is
    /// next pruned — and nothing more is queued for it.
    fn doom(&mut self, farewell: Farewell) {
        match farewell {
            Farewell::ClosedByPeer => tracing::debug!("control client disconnected"),
            other => tracing::info!(reason = ?other, "control client dropped"),
        }
        // SAFETY: `shutdown` on a valid fd this struct owns; the result
        // is ignored because the one failure it can report (the peer is
        // already gone) is the state being asked for.
        unsafe {
            libc::shutdown(self.stream.as_raw_fd(), libc::SHUT_RDWR);
        }
        self.inbound.clear();
        self.outbound.clear();
        self.wants_snapshot = false;
        self.owed_workspaces = false;
        self.doomed = true;
    }

    fn queue(&mut self, event: &Event) {
        self.outbound.extend_from_slice(&line(event));
    }

    /// One non-blocking read pass: takes what the kernel has (up to
    /// [`READ_BUDGET`]), splits it into lines, and answers each. Returns
    /// the reason to drop this client, if one arose.
    ///
    /// A zero-length read is the peer's writing side going away, which
    /// is not by itself a reason: it may still be reading. The reason
    /// arrives when [`peer_gone`](Self::peer_gone) says both sides are.
    ///
    /// The queue is drained *before* that verdict: a client that writes
    /// a request and closes at once (`printf ... | nc -U -q0`, a Python
    /// `send(); close()`) shows `POLLHUP` with its bytes still waiting
    /// in the kernel, and those bytes are the whole reason it
    /// connected. Only a peer already known to have finished writing is
    /// judged without a read.
    fn read(&mut self, now: &Snapshot, commands: &mut Vec<Command>) -> Option<Farewell> {
        if self.peer_finished {
            return self.peer_gone().then_some(Farewell::ClosedByPeer);
        }
        let mut budget = READ_BUDGET;
        let mut buffer = [0u8; 4096];
        while budget > 0 {
            let want = buffer.len().min(budget);
            match self.stream.recv(&mut buffer[..want]) {
                Ok(0) => {
                    self.peer_finished = true;
                    // Whatever arrived before the shutdown still counts;
                    // a request without its newline is dropped, as the
                    // spec's framing says it must be.
                    return self.drain_lines(now, commands).or_else(|| self.peer_gone().then_some(Farewell::ClosedByPeer));
                }
                Ok(n) => {
                    budget -= n;
                    self.inbound.extend_from_slice(&buffer[..n]);
                    if let Some(farewell) = self.drain_lines(now, commands) {
                        return Some(farewell);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => break,
                Err(error) => {
                    tracing::debug!(?error, "control client read failed");
                    return Some(Farewell::ReadError);
                }
            }
        }
        None
    }

    /// Whether the peer has closed both directions. `POLLHUP` on a Unix
    /// stream socket is set exactly then — a peer that has only shut
    /// down its writing side shows `POLLIN | POLLRDHUP` instead — so
    /// this is the one question a zero-length `recv` cannot answer.
    /// Zero timeout: a poll, not a wait.
    fn peer_gone(&self) -> bool {
        let mut fds = libc::pollfd { fd: self.stream.as_raw_fd(), events: 0, revents: 0 };
        // SAFETY: one valid pollfd this struct owns, for zero
        // milliseconds. A failed poll (EINTR, for instance) reads as
        // "not gone" and is asked again next tick.
        let ready = unsafe { libc::poll(&mut fds, 1, 0) };
        ready > 0 && fds.revents & (libc::POLLHUP | libc::POLLERR) != 0
    }

    /// Consumes every complete line in `inbound`, then checks what is
    /// left against the cap: a partial line longer than any legal
    /// whole line is a client that has lost its framing.
    fn drain_lines(&mut self, now: &Snapshot, commands: &mut Vec<Command>) -> Option<Farewell> {
        while let Some(end) = self.inbound.iter().position(|&b| b == b'\n') {
            if end + 1 > LINE_CAP {
                return Some(Farewell::LineOverflow);
            }
            let line: Vec<u8> = self.inbound.drain(..=end).collect();
            let body = &line[..end];
            // §1: empty lines are ignored — and "empty" includes the
            // `\r` a telnet-minded client leaves behind.
            if body.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            self.handle(body, now, commands);
        }
        if self.inbound.len() >= LINE_CAP {
            return Some(Farewell::LineOverflow);
        }
        None
    }

    fn handle(&mut self, body: &[u8], now: &Snapshot, commands: &mut Vec<Command>) {
        match parse_request(body) {
            Ok(Request::Snapshot) => self.wants_snapshot = true,
            Ok(Request::FocusWorkspace { index }) => {
                let exist = now.workspaces.workspaces.len();
                if index < exist {
                    commands.push(Command::FocusWorkspace(index));
                    self.owed_workspaces = true;
                } else {
                    self.queue(&Event::Error(ErrorEvent {
                        request: Some("focus-workspace".to_string()),
                        message: format!("no workspace {index} ({exist} exist)"),
                    }));
                }
            }
            Err(error) => self.queue(&Event::Error(error)),
        }
    }

    /// Queues what this client is owed given the facets that changed:
    /// everything if it is owed a snapshot, else the changed facets —
    /// plus `workspaces` if it is owed an acknowledgement. Each is
    /// queued at most once per call, so a request whose switch did
    /// change the facet is answered with one line, not two.
    fn publish(&mut self, now: &Snapshot, changed: &[bool; 4]) {
        let owed = std::mem::take(&mut self.owed_workspaces);
        if std::mem::take(&mut self.wants_snapshot) {
            for event in now.events() {
                self.queue(&event);
            }
            return;
        }
        for (index, event) in now.events().iter().enumerate() {
            if changed[index] || (index == 0 && owed) {
                self.queue(event);
            }
        }
    }

    /// One non-blocking write pass, then the overflow check. The check
    /// comes after the write so a client that is keeping up is judged
    /// on what it has not yet taken, not on what it was about to.
    fn flush(&mut self) -> Option<Farewell> {
        while !self.outbound.is_empty() {
            match self.stream.send(&self.outbound) {
                Ok(0) => break,
                Ok(n) => {
                    self.outbound.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => break,
                Err(error) => {
                    tracing::debug!(?error, "control client write failed");
                    return Some(Farewell::WriteError);
                }
            }
        }
        if self.outbound.len() > OUTBOUND_CAP {
            return Some(Farewell::OutboundOverflow);
        }
        None
    }
}

// ---------------------------------------------------------------------
// The socket
// ---------------------------------------------------------------------

/// The listener, its clients, and the last snapshot they were told.
pub(crate) struct ControlSocket {
    listener: Option<StreamListener>,
    socket_path: PathBuf,
    clients: Vec<ControlClient>,
    hello: Event,
    /// What every connected client has been told, facet by facet.
    /// `None` until the first publish.
    last: Option<Snapshot>,
}

impl ControlSocket {
    /// Binds `control-<display>.sock` beside the dock socket and makes
    /// the path known to every process the shell will launch. Failure
    /// is a warning and an unbound socket, never an error: a desktop
    /// without a bar's socket is still a desktop.
    pub(crate) fn new(display: &str) -> Self {
        let socket_path = match transport::control_socket_path(display) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(?error, "no control socket path; the control socket is unavailable this session");
                return Self::unbound(PathBuf::new());
            }
        };
        let socket = Self::bind_at(socket_path);
        if socket.is_bound() {
            spawn::declare_control_socket(socket.socket_path().to_path_buf());
            tracing::info!(socket = %socket.socket_path().display(), "control socket exported as {}", spawn::CONTROL_SOCKET_ENV);
        }
        socket
    }

    /// Binds a named socket — split from [`new`](Self::new) so a test
    /// can stand a real listener up in a scratch directory without
    /// touching `$XDG_RUNTIME_DIR` or the process environment.
    pub(crate) fn bind_at(socket_path: PathBuf) -> Self {
        match StreamListener::bind(&socket_path) {
            Ok(listener) => {
                tracing::info!(socket = %socket_path.display(), "control socket listening");
                Self { listener: Some(listener), socket_path, clients: Vec::new(), hello: Self::hello(), last: None }
            }
            Err(error) => {
                tracing::warn!(?error, socket = %socket_path.display(), "could not bind the control socket; it is unavailable this session");
                Self::unbound(socket_path)
            }
        }
    }

    fn unbound(socket_path: PathBuf) -> Self {
        Self { listener: None, socket_path, clients: Vec::new(), hello: Self::hello(), last: None }
    }

    fn hello() -> Event {
        let session = match spawn::current_display_stack() {
            spawn::DisplayStack::Wayland => "wayland",
            spawn::DisplayStack::X11 => "x11",
        };
        Event::Hello(Hello { protocol: PROTOCOL, session: session.to_string(), pid: std::process::id() })
    }

    pub(crate) fn is_bound(&self) -> bool {
        self.listener.is_some()
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(crate) fn has_clients(&self) -> bool {
        !self.clients.is_empty()
    }

    /// Admits everything waiting on the listener. A new client owes
    /// nothing yet but `hello`, which is queued here so it is first no
    /// matter what the client sends in the meantime; the snapshot
    /// follows on the next [`publish`](Self::publish) — which the shell
    /// calls in the same tick, with a snapshot taken *after* this, so a
    /// fresh client is never told stale state.
    pub(crate) fn accept(&mut self) {
        let Some(listener) = &self.listener else { return };
        loop {
            match listener.accept() {
                Ok(Some(stream)) => {
                    // Restating what the 0700 directory already
                    // enforces, exactly as the dock host does: a
                    // socket that answers only to its own user should
                    // check, not assume.
                    match stream.peer_is_this_user() {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::warn!("control connection from another user refused");
                            continue;
                        }
                        Err(error) => {
                            tracing::warn!(?error, "control connection's peer could not be identified; refused");
                            continue;
                        }
                    }
                    let mut client = ControlClient::new(stream);
                    client.queue(&self.hello);
                    self.clients.push(client);
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(?error, "control socket accept failed");
                    break;
                }
            }
        }
    }

    #[cfg(test)]
    fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Admits a stream that did not arrive through the listener — a
    /// test's `socketpair`, whose far end can be left unread without a
    /// second thread. Same treatment as an accepted one.
    #[cfg(test)]
    fn admit(&mut self, stream: Stream) {
        let mut client = ControlClient::new(stream);
        client.queue(&self.hello);
        self.clients.push(client);
    }

    /// One full servicing pass against the current state. Per client,
    /// in this order: what it is owed of the state (the snapshot, if
    /// fresh; else the facets that changed), then its pending requests
    /// read and answered, then a `snapshot` it just asked for. The
    /// order is the spec's: a client that speaks before it listens still
    /// reads `hello`, the four facets, and only then the answers its
    /// own lines earned. Returns the commands clients asked for; the
    /// shell applies them and calls [`publish`](Self::publish) once
    /// more in the same tick, which is where a `focus-workspace` gets
    /// its `workspaces` answer — *after* the switch, so the line says
    /// what the switch did.
    pub(crate) fn service(&mut self, now: &Snapshot) -> Vec<Command> {
        let mut commands = Vec::new();
        let changed = self.note(now);
        for client in &mut self.clients {
            client.publish(now, &changed);
            if let Some(farewell) = client.read(now, &mut commands) {
                client.doom(farewell);
                continue;
            }
            if client.wants_snapshot {
                client.publish(now, &[false; 4]);
            }
            if let Some(farewell) = client.flush() {
                client.doom(farewell);
            }
        }
        self.clients.retain(|client| !client.doomed);
        commands
    }

    /// Tells every client what changed since the last publish (or
    /// everything, to a client that is owed a snapshot), writes as much
    /// as the kernel will take, and drops clients that have stopped
    /// reading. The comparison is per facet, so a title change costs
    /// one `focus` line and nothing else.
    pub(crate) fn publish(&mut self, now: &Snapshot) {
        let changed = self.note(now);
        for client in &mut self.clients {
            client.publish(now, &changed);
            if let Some(farewell) = client.flush() {
                client.doom(farewell);
            }
        }
        self.clients.retain(|client| !client.doomed);
    }

    /// Which facets differ from what every client was last told, and
    /// remembers `now` as the new baseline if any do. The one place the
    /// dedup lives.
    fn note(&mut self, now: &Snapshot) -> [bool; 4] {
        let changed = match &self.last {
            None => [true; 4],
            Some(last) => [
                last.workspaces != now.workspaces,
                last.outputs != now.outputs,
                last.focus != now.focus,
                last.theme != now.theme,
            ],
        };
        if changed.iter().any(|&c| c) {
            self.last = Some(now.clone());
        }
        changed
    }

    /// The fds the event loop should wake on, so a request is answered
    /// on arrival rather than on the next 16ms housekeeping bound. Like
    /// the dock's, missing one costs latency, not correctness.
    pub(crate) fn poll_fds(&self) -> impl Iterator<Item = RawFd> + '_ {
        self.listener.iter().map(|l| l.as_raw_fd()).chain(self.clients.iter().map(|c| c.stream.as_raw_fd()))
    }

    /// Closes every client and unlinks the socket. Dropping does the
    /// same; this exists so the shell's shutdown order is explicit and
    /// so a hot restart's re-exec — which runs no destructors — has the
    /// path cleared before the incoming shell probes it. (If it were
    /// not, the probe would find a dead socket and clear it anyway; the
    /// explicit call just makes the common case not lean on that.)
    pub(crate) fn shut_down(&mut self) {
        self.clients.clear();
        self.listener = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use wm_core::fake_backend::{FakeBackend, FakeTheme, FakeWindowId};
    use wm_core::{BackendEvent, MonitorInfo};
    use wm_theme_api::{Rect, Size};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A private scratch directory with a socket path inside it, gone
    /// when the test is — the dockapp host's helper, kept local so the
    /// two modules' tests stay independent.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("chonk-control-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
            Self(dir)
        }

        fn socket(&self) -> PathBuf {
            self.0.join("control-test.sock")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A bar, as the tests see one: a connected stream and the bytes it
    /// has read so far, split into lines on demand.
    struct Bar {
        stream: Stream,
        received: Vec<u8>,
    }

    impl Bar {
        fn connect(socket: &ControlSocket) -> Self {
            Self { stream: Stream::connect(socket.socket_path()).expect("connect to the control socket"), received: Vec::new() }
        }

        fn send(&self, line: &str) {
            let bytes = line.as_bytes();
            let mut sent = 0;
            while sent < bytes.len() {
                match self.stream.send(&bytes[sent..]) {
                    Ok(n) => sent += n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                    Err(e) => panic!("send failed: {e}"),
                }
            }
        }

        /// Reads until `count` whole lines have arrived (or five seconds
        /// pass), and returns them parsed. Bounded so a regression
        /// fails rather than hangs.
        fn lines(&mut self, count: usize) -> Vec<serde_json::Value> {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut buffer = [0u8; 8192];
            while self.received.iter().filter(|&&b| b == b'\n').count() < count {
                match self.stream.recv_until(&mut buffer, deadline).expect("recv") {
                    Some(0) => panic!("the shell closed the connection with {} of {count} lines read", self.line_count()),
                    Some(n) => self.received.extend_from_slice(&buffer[..n]),
                    None => panic!("only {} of {count} lines arrived in time", self.line_count()),
                }
            }
            let mut lines = Vec::new();
            while let Some(end) = self.received.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.received.drain(..=end).collect();
                lines.push(serde_json::from_slice(&line[..end]).expect("every line the shell writes is JSON"));
                if lines.len() == count {
                    break;
                }
            }
            lines
        }

        fn line_count(&self) -> usize {
            self.received.iter().filter(|&&b| b == b'\n').count()
        }

        /// True once the shell has closed its side, within a bound.
        fn is_closed(&mut self) -> bool {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut buffer = [0u8; 8192];
            loop {
                match self.stream.recv_until(&mut buffer, deadline) {
                    Ok(Some(0)) => return true,
                    Ok(Some(n)) => self.received.extend_from_slice(&buffer[..n]),
                    Ok(None) => return false,
                    // ECONNRESET is also "closed", from a peer that had
                    // unread bytes when it shut down.
                    Err(_) => return true,
                }
            }
        }
    }

    fn theme() -> Theme {
        let mut theme = wm_theme::default_theme::all_themes().into_iter().next().expect("the theme set is never empty");
        theme.id = "nextstep-classic".to_string();
        theme.name = "NeXTSTEP Classic".to_string();
        theme
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            workspaces: WorkspacesEvent {
                active: 0,
                workspaces: vec![
                    WorkspaceEntry { index: 0, windows: 3 },
                    WorkspaceEntry { index: 1, windows: 0 },
                    WorkspaceEntry { index: 2, windows: 1 },
                ],
            },
            outputs: OutputsEvent {
                focused: Some(0),
                outputs: vec![OutputEntry { index: 0, name: "eDP-1".to_string(), x: 0, y: 0, width: 2560, height: 1600, scale: 2.0 }],
            },
            focus: FocusEvent {
                window: Some(FocusedWindow { id: 2147483650, title: "~ — foot".to_string(), app_id: "foot".to_string(), workspace: 0 }),
                count: 4,
            },
            theme: ThemeEvent {
                id: "nextstep-classic".to_string(),
                name: "NeXTSTEP Classic".to_string(),
                appearance: "dark".to_string(),
                following: None,
            },
        }
    }

    fn text(event: &Event) -> String {
        String::from_utf8(line(event)).unwrap()
    }

    /// A bound socket in a scratch directory plus one connected bar
    /// that has been accepted and handed its snapshot.
    fn connected(snapshot: &Snapshot) -> (Scratch, ControlSocket, Bar) {
        let scratch = Scratch::new();
        let mut socket = ControlSocket::bind_at(scratch.socket());
        assert!(socket.is_bound());
        let bar = Bar::connect(&socket);
        wait_for_accept(&mut socket);
        socket.publish(snapshot);
        (scratch, socket, bar)
    }

    fn wait_for_accept(socket: &mut ControlSocket) {
        let before = socket.client_count();
        let deadline = Instant::now() + Duration::from_secs(5);
        while socket.client_count() == before {
            assert!(Instant::now() < deadline, "the connection never arrived");
            socket.accept();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Services until the shell has read `bar`'s request: a request
    /// sent a moment ago may not be in the kernel buffer yet, so this
    /// polls for the first pass that yields a command (or an error
    /// event to the bar, which `service` answers without a command —
    /// callers expecting one of those poll `bar.lines` themselves).
    /// Bounded so a regression fails rather than hangs.
    fn service_after_request(socket: &mut ControlSocket, snapshot: &Snapshot) -> Vec<Command> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let commands = socket.service(snapshot);
            if !commands.is_empty() || Instant::now() >= deadline {
                return commands;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    // -----------------------------------------------------------------
    // The wire, checked against the document's own examples
    // -----------------------------------------------------------------

    #[test]
    fn every_facet_serialises_exactly_as_the_spec_example_reads() {
        // Byte-for-byte, not merely key-for-key: a QML client is being
        // written against these lines in parallel, and the field order
        // is part of what makes `docs/control-socket.md` copy-pasteable.
        let s = sample_snapshot();
        assert_eq!(
            text(&Event::Workspaces(s.workspaces.clone())),
            "{\"event\":\"workspaces\",\"active\":0,\"workspaces\":[{\"index\":0,\"windows\":3},{\"index\":1,\"windows\":0},{\"index\":2,\"windows\":1}]}\n"
        );
        assert_eq!(
            text(&Event::Outputs(s.outputs.clone())),
            "{\"event\":\"outputs\",\"focused\":0,\"outputs\":[{\"index\":0,\"name\":\"eDP-1\",\"x\":0,\"y\":0,\"width\":2560,\"height\":1600,\"scale\":2.0}]}\n"
        );
        assert_eq!(
            text(&Event::Focus(s.focus.clone())),
            "{\"event\":\"focus\",\"window\":{\"id\":2147483650,\"title\":\"~ — foot\",\"app_id\":\"foot\",\"workspace\":0},\"count\":4}\n"
        );
        assert_eq!(
            text(&Event::Theme(s.theme.clone())),
            "{\"event\":\"theme\",\"id\":\"nextstep-classic\",\"name\":\"NeXTSTEP Classic\",\"appearance\":\"dark\",\"following\":null}\n"
        );
        assert_eq!(
            text(&Event::Hello(Hello { protocol: 1, session: "wayland".to_string(), pid: 1441097 })),
            "{\"event\":\"hello\",\"protocol\":1,\"session\":\"wayland\",\"pid\":1441097}\n"
        );
        assert_eq!(
            text(&Event::Error(ErrorEvent { request: Some("focus-workspace".to_string()), message: "no workspace 7 (3 exist)".to_string() })),
            "{\"event\":\"error\",\"request\":\"focus-workspace\",\"message\":\"no workspace 7 (3 exist)\"}\n"
        );
    }

    #[test]
    fn the_nulls_the_spec_names_are_written_as_null_not_omitted() {
        // A client reading `m.window === null` must find the key.
        assert_eq!(text(&Event::Focus(FocusEvent { window: None, count: 0 })), "{\"event\":\"focus\",\"window\":null,\"count\":0}\n");
        assert_eq!(
            text(&Event::Outputs(OutputsEvent { focused: None, outputs: vec![] })),
            "{\"event\":\"outputs\",\"focused\":null,\"outputs\":[]}\n"
        );
        assert_eq!(
            text(&Event::Error(ErrorEvent { request: None, message: "x".to_string() })),
            "{\"event\":\"error\",\"request\":null,\"message\":\"x\"}\n"
        );
    }

    #[test]
    fn every_line_ends_in_exactly_one_newline_and_contains_none_inside() {
        // A title with a newline in it is the realistic way to break
        // framing from the shell's side; JSON escaping is what keeps
        // the invariant, and this pins that it does.
        let mut s = sample_snapshot();
        s.focus.window.as_mut().unwrap().title = "line one\nline two\r\n".to_string();
        for event in s.events() {
            let bytes = line(&event);
            assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
            assert_eq!(bytes.last(), Some(&b'\n'));
        }
    }

    // -----------------------------------------------------------------
    // Requests
    // -----------------------------------------------------------------

    #[test]
    fn the_two_requests_parse_and_extra_keys_are_ignored() {
        assert_eq!(parse_request(br#"{"request":"snapshot"}"#), Ok(Request::Snapshot));
        assert_eq!(parse_request(br#"{"request":"focus-workspace","index":2}"#), Ok(Request::FocusWorkspace { index: 2 }));
        assert_eq!(
            parse_request(br#"{"request":"snapshot","because":"a future client may say why"}"#),
            Ok(Request::Snapshot),
            "the shell adds keys within a version and so must tolerate a client that does"
        );
    }

    #[test]
    fn an_unknown_verb_is_an_error_that_quotes_the_verb_back() {
        let error = parse_request(br#"{"request":"launch-terminal"}"#).unwrap_err();
        assert_eq!(error.request.as_deref(), Some("launch-terminal"));
        assert!(error.message.contains("launch-terminal"), "{}", error.message);
    }

    #[test]
    fn an_unparseable_line_is_an_error_with_a_null_request() {
        for garbage in [&b"this is not json"[..], b"[1,2,3]", b"42", b"{\"index\":2}", b"{\"request\":7}", b"\xff\xfe"] {
            let error = parse_request(garbage).unwrap_err();
            assert_eq!(error.request, None, "{garbage:?} has no verb to quote");
        }
    }

    #[test]
    fn a_known_verb_with_bad_arguments_is_an_error_that_still_names_the_verb() {
        for bad in [&br#"{"request":"focus-workspace"}"#[..], br#"{"request":"focus-workspace","index":"two"}"#, br#"{"request":"focus-workspace","index":-1}"#, br#"{"request":"focus-workspace","index":1.5}"#] {
            let error = parse_request(bad).unwrap_err();
            assert_eq!(error.request.as_deref(), Some("focus-workspace"), "{}", String::from_utf8_lossy(bad));
        }
    }

    // -----------------------------------------------------------------
    // The connection
    // -----------------------------------------------------------------

    #[test]
    fn a_new_client_reads_hello_then_every_facet_in_spec_order() {
        let (_scratch, _socket, mut bar) = connected(&sample_snapshot());
        let lines = bar.lines(5);
        let events: Vec<&str> = lines.iter().map(|l| l["event"].as_str().unwrap()).collect();
        assert_eq!(events, ["hello", "workspaces", "outputs", "focus", "theme"]);
        assert_eq!(lines[0]["protocol"], 1);
        assert_eq!(lines[0]["pid"], std::process::id());
        assert!(matches!(lines[0]["session"].as_str(), Some("wayland" | "x11")));
    }

    #[test]
    fn an_unchanged_snapshot_publishes_nothing_and_a_changed_facet_publishes_only_itself() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        socket.publish(&snapshot);
        socket.publish(&snapshot);
        let mut changed = snapshot.clone();
        changed.focus.window.as_mut().unwrap().title = "another title".to_string();
        socket.publish(&changed);
        let lines = bar.lines(1);
        assert_eq!(lines[0]["event"], "focus");
        assert_eq!(lines[0]["window"]["title"], "another title");
        // And nothing else arrived alongside it.
        std::thread::sleep(Duration::from_millis(10));
        let mut buffer = [0u8; 64];
        assert!(matches!(bar.stream.recv(&mut buffer), Err(e) if e.kind() == io::ErrorKind::WouldBlock), "the dedup let something through");
    }

    #[test]
    fn two_facets_changing_at_once_arrive_in_spec_order() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        let mut changed = snapshot.clone();
        changed.theme.appearance = "light".to_string();
        changed.workspaces.active = 2;
        socket.publish(&changed);
        let lines = bar.lines(2);
        assert_eq!(lines[0]["event"], "workspaces");
        assert_eq!(lines[1]["event"], "theme");
    }

    #[test]
    fn a_snapshot_request_resends_every_facet_in_order() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        bar.send("{\"request\":\"snapshot\"}\n");
        assert!(service_after_request(&mut socket, &snapshot).is_empty());
        let events: Vec<String> = bar.lines(4).iter().map(|l| l["event"].as_str().unwrap().to_string()).collect();
        assert_eq!(events, ["workspaces", "outputs", "focus", "theme"]);
    }

    #[test]
    fn focus_workspace_in_range_becomes_a_command_and_the_answer_follows_the_switch() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        bar.send("{\"request\":\"focus-workspace\",\"index\":2}\n");
        assert_eq!(service_after_request(&mut socket, &snapshot), vec![Command::FocusWorkspace(2)]);
        // The shell applies the command and publishes the result.
        let mut after = snapshot.clone();
        after.workspaces.active = 2;
        socket.publish(&after);
        let lines = bar.lines(1);
        assert_eq!(lines[0]["event"], "workspaces");
        assert_eq!(lines[0]["active"], 2);
    }

    /// The fire-and-forget shape the document's own `printf | socat`
    /// example has: the request and the close arrive in the same
    /// instant, and the request must still be served.
    #[test]
    fn a_request_written_and_closed_at_once_is_still_served() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        bar.send("{\"request\":\"focus-workspace\",\"index\":2}\n");
        drop(bar);
        assert_eq!(service_after_request(&mut socket, &snapshot), vec![Command::FocusWorkspace(2)]);
        // And the departed client is then let go of, not kept.
        let deadline = Instant::now() + Duration::from_secs(5);
        while socket.client_count() > 0 {
            assert!(Instant::now() < deadline, "the closed client should be dropped");
            socket.service(&snapshot);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A request split across two writes — the framing is the newline,
    /// not the write — is assembled across service passes.
    #[test]
    fn a_request_split_across_two_writes_is_assembled() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        bar.send("{\"request\":\"focus-wor");
        // Give the first half every chance to arrive alone: it must
        // yield nothing, and nothing must be discarded.
        std::thread::sleep(Duration::from_millis(5));
        assert!(socket.service(&snapshot).is_empty(), "half a line is not a request");
        bar.send("kspace\",\"index\":1}\n");
        assert_eq!(service_after_request(&mut socket, &snapshot), vec![Command::FocusWorkspace(1)]);
    }

    #[test]
    fn focus_workspace_out_of_range_is_an_error_and_the_connection_stays_open() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        bar.send("{\"request\":\"focus-workspace\",\"index\":7}\n");
        assert!(service_after_request(&mut socket, &snapshot).is_empty(), "an out-of-range index must never reach the window manager");
        let lines = bar.lines(1);
        assert_eq!(lines[0]["event"], "error");
        assert_eq!(lines[0]["request"], "focus-workspace");
        assert_eq!(lines[0]["message"], "no workspace 7 (3 exist)");
        // Still connected: a later request is answered.
        bar.send("{\"request\":\"snapshot\"}\n");
        service_after_request(&mut socket, &snapshot);
        assert_eq!(bar.lines(1)[0]["event"], "workspaces");
    }

    #[test]
    fn focus_workspace_naming_the_current_workspace_is_still_answered() {
        // Spec §1.2: every request gets the events it caused or an
        // error. A switch to where we already are causes nothing, so
        // the dedup would answer with silence — the `workspaces` line
        // is re-sent to that client as the acknowledgement instead.
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        let mut other = Bar::connect(&socket);
        wait_for_accept(&mut socket);
        socket.publish(&snapshot);
        other.lines(5);

        bar.send("{\"request\":\"focus-workspace\",\"index\":0}\n");
        assert_eq!(service_after_request(&mut socket, &snapshot), vec![Command::FocusWorkspace(0)]);
        socket.publish(&snapshot);
        assert_eq!(bar.lines(1)[0]["event"], "workspaces");
        std::thread::sleep(Duration::from_millis(10));
        let mut buffer = [0u8; 64];
        assert!(matches!(other.stream.recv(&mut buffer), Err(e) if e.kind() == io::ErrorKind::WouldBlock), "the acknowledgement is for the asker alone");
    }

    #[test]
    fn unknown_and_malformed_requests_are_answered_in_order_without_disconnecting() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        bar.send("{\"request\":\"dance\"}\n\n   \nnot json at all\n\r\n{\"request\":\"snapshot\"}\n");
        service_after_request(&mut socket, &snapshot);
        let lines = bar.lines(6);
        assert_eq!(lines[0]["event"], "error");
        assert_eq!(lines[0]["request"], "dance");
        assert_eq!(lines[1]["event"], "error");
        assert_eq!(lines[1]["request"], serde_json::Value::Null);
        assert_eq!(lines[2]["event"], "workspaces", "the empty and whitespace lines between were ignored, not answered");
        assert_eq!(lines[5]["event"], "theme");
    }

    #[test]
    fn a_request_sent_before_hello_is_read_is_answered_after_the_snapshot() {
        // §1.2 allows a client to speak first; what it must still see
        // is hello, the four facets, then its answer.
        let scratch = Scratch::new();
        let mut socket = ControlSocket::bind_at(scratch.socket());
        let mut bar = Bar::connect(&socket);
        bar.send("{\"request\":\"focus-workspace\",\"index\":9}\n");
        wait_for_accept(&mut socket);
        std::thread::sleep(Duration::from_millis(5));
        socket.service(&sample_snapshot());
        let events: Vec<String> = bar.lines(6).iter().map(|l| l["event"].as_str().unwrap().to_string()).collect();
        assert_eq!(events, ["hello", "workspaces", "outputs", "focus", "theme", "error"]);
    }

    #[test]
    fn a_line_over_the_cap_disconnects_the_client() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        // One byte over, newline included — and no newline yet, so the
        // shell has to judge the partial line, not a finished one.
        let flood = vec![b'x'; LINE_CAP];
        let mut sent = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while sent < flood.len() && Instant::now() < deadline {
            match bar.stream.send(&flood[sent..]) {
                Ok(n) => sent += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    socket.service(&snapshot);
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => break, // already disconnected — that is the point
            }
        }
        for _ in 0..10 {
            socket.service(&snapshot);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!socket.has_clients());
        assert!(bar.is_closed());
    }

    #[test]
    fn a_line_at_the_cap_with_its_newline_is_still_accepted() {
        // The boundary, from the other side: 65,536 bytes *including*
        // the newline is legal.
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        let mut request = br#"{"request":"snapshot","pad":""#.to_vec();
        let tail = b"\"}\n";
        request.resize(LINE_CAP - tail.len(), b'p');
        request.extend_from_slice(tail);
        assert_eq!(request.len(), LINE_CAP);
        let mut sent = 0;
        while sent < request.len() {
            match bar.stream.send(&request[sent..]) {
                Ok(n) => sent += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    socket.service(&snapshot);
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("send failed: {e}"),
            }
        }
        service_after_request(&mut socket, &snapshot);
        assert!(socket.has_clients());
        assert_eq!(bar.lines(1)[0]["event"], "workspaces");
    }

    #[test]
    fn a_client_that_stops_reading_is_dropped_once_the_cap_is_crossed_and_nobody_waited() {
        // A socketpair whose far end nobody ever reads: the kernel
        // buffer fills, the shell's own buffer fills to the cap, and
        // the shell must let go — on this thread, with no reader to
        // rescue it. If any send here blocked, the test would hang.
        let mut fds = [0 as RawFd; 2];
        // SAFETY: a plain socketpair into a two-element array.
        let made = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
        assert_eq!(made, 0);
        // SAFETY: both fds were just returned by `socketpair` and are
        // owned by nothing else.
        let (shell_end, far_end) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let mut socket = ControlSocket::bind_at(Scratch::new().socket());
        socket.admit(Stream::from_fd(shell_end));

        let mut snapshot = sample_snapshot();
        let started = Instant::now();
        let mut passes = 0;
        while socket.has_clients() {
            // Each pass is a distinct 64 KiB title, so each is a
            // genuine change the dedup lets through.
            passes += 1;
            snapshot.focus.window.as_mut().unwrap().title = format!("{passes}").repeat(LINE_CAP / 4);
            socket.publish(&snapshot);
            assert!(passes < 1_000, "the client was never dropped");
            assert!(started.elapsed() < Duration::from_secs(5), "publishing took far too long for a non-blocking path");
        }
        // The kernel takes a couple of hundred KiB before it stops; the
        // cap is another 256 KiB on top. Under ten passes would mean
        // the cap was not honoured; the actual number depends on the
        // kernel's default buffer, so only the lower bound is pinned.
        assert!(passes >= (OUTBOUND_CAP / LINE_CAP), "dropped after {passes} passes, before the cap could have been crossed");
        drop(far_end);
    }

    #[test]
    fn a_client_that_closes_is_forgotten_on_the_next_pass() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        drop(bar);
        std::thread::sleep(Duration::from_millis(5));
        socket.service(&snapshot);
        assert!(!socket.has_clients());
    }

    #[test]
    fn a_one_shot_client_that_half_closes_still_reads_its_answer_and_later_events() {
        // `printf '{"request":"focus-workspace","index":7}' | socat -
        // UNIX-CONNECT:…`: the request and the write-side shutdown
        // arrive in the same instant, before the shell has serviced the
        // connection once. It must still read hello, the snapshot, and
        // the error — and, since it is still listening, the next change.
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.send("{\"request\":\"focus-workspace\",\"index\":7}\n");
        // SAFETY: shutting down the writing side of a socket the test
        // owns and keeps open for reading.
        unsafe {
            libc::shutdown(bar.stream.as_raw_fd(), libc::SHUT_WR);
        }
        std::thread::sleep(Duration::from_millis(5));
        socket.service(&snapshot);
        let lines = bar.lines(6);
        assert_eq!(lines[0]["event"], "hello");
        assert_eq!(lines[5]["event"], "error");
        assert_eq!(lines[5]["request"], "focus-workspace");
        assert!(socket.has_clients(), "a half-closed peer is a listener, not a departure");

        let mut later = snapshot.clone();
        later.workspaces.active = 1;
        socket.publish(&later);
        assert_eq!(bar.lines(1)[0]["active"], 1, "a listener keeps being told");

        drop(bar);
        std::thread::sleep(Duration::from_millis(5));
        socket.service(&later);
        assert!(!socket.has_clients(), "a full close is still goodbye");
    }

    #[test]
    fn an_unbound_socket_is_inert() {
        let mut socket = ControlSocket::unbound(PathBuf::from("/nonexistent/control.sock"));
        assert!(!socket.is_bound());
        socket.accept();
        assert!(socket.service(&sample_snapshot()).is_empty());
        assert_eq!(socket.poll_fds().count(), 0);
    }

    #[test]
    fn the_listener_and_every_client_are_offered_to_the_event_loop() {
        let (_scratch, socket, _bar) = connected(&sample_snapshot());
        assert_eq!(socket.poll_fds().count(), 2, "one listener, one client");
    }

    #[test]
    fn shutting_down_unlinks_the_socket_and_closes_every_client() {
        let (scratch, mut socket, mut bar) = connected(&sample_snapshot());
        bar.lines(5);
        socket.shut_down();
        assert!(!scratch.socket().exists());
        assert!(bar.is_closed());
        assert!(!socket.is_bound());
    }

    #[test]
    fn a_stale_socket_left_by_a_killed_shell_is_replaced_at_bind() {
        // What a hot restart's re-exec leaves behind, and what the next
        // shell must cope with without help.
        let scratch = Scratch::new();
        std::fs::write(scratch.socket(), b"").unwrap();
        let socket = ControlSocket::bind_at(scratch.socket());
        assert!(socket.is_bound());
        assert!(Stream::connect(socket.socket_path()).is_ok());
    }

    // -----------------------------------------------------------------
    // The snapshot, read from a live window manager
    // -----------------------------------------------------------------

    fn surroundings<'a>(theme: &'a Theme) -> Surroundings<'a> {
        Surroundings { theme, appearance: Appearance::Dark, scale: 2.0, pointer_root: Point::new(10, 10), following: None }
    }

    fn manager(backend: FakeBackend) -> WindowManager<FakeBackend> {
        WindowManager::new(backend, Box::new(FakeTheme))
    }

    fn map(wm: &mut WindowManager<FakeBackend>, window: FakeWindowId) -> wm_core::ClientId {
        wm.dispatch(BackendEvent::MapRequest(window));
        wm.client_for_window(window).expect("mapped")
    }

    #[test]
    fn workspaces_are_zero_based_contiguous_and_count_miniaturised_windows() {
        let mut backend = FakeBackend::new();
        let (w1, w2, w3) = (backend.create_window(), backend.create_window(), backend.create_window());
        let mut wm = manager(backend);
        let (c1, _c2, c3) = (map(&mut wm, w1), map(&mut wm, w2), map(&mut wm, w3));
        wm.move_client_to_workspace(c3, 2);
        wm.miniaturize(c1);
        let theme = theme();
        let s = snapshot(&wm, &surroundings(&theme));
        assert_eq!(s.workspaces.active, 0);
        assert_eq!(
            s.workspaces.workspaces,
            vec![WorkspaceEntry { index: 0, windows: 2 }, WorkspaceEntry { index: 1, windows: 0 }, WorkspaceEntry { index: 2, windows: 1 }],
            "three workspaces exist once a window sits on the third; the miniaturised one still counts on its own"
        );
        assert_eq!(s.focus.count, 3);
    }

    #[test]
    fn focus_names_the_focused_window_by_id_title_class_and_workspace_or_is_null() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        backend.set_title(w1, "~ — foot");
        backend.window_classes.insert(w1, "foot".to_string());
        let mut wm = manager(backend);
        let theme = theme();
        assert_eq!(snapshot(&wm, &surroundings(&theme)).focus, FocusEvent { window: None, count: 0 });

        let id = map(&mut wm, w1);
        let s = snapshot(&wm, &surroundings(&theme));
        let window = s.focus.window.expect("mapping focuses");
        assert_eq!(window.id, id.as_u64());
        assert_ne!(window.id, 0, "a slotmap key is never the null key once it names a live client");
        assert_eq!(window.title, "~ — foot");
        assert_eq!(window.app_id, "foot");
        assert_eq!(window.workspace, 0);
        assert_eq!(s.focus.count, 1);
    }

    #[test]
    fn outputs_come_from_the_monitor_list_with_the_pointers_output_focused() {
        let mut backend = FakeBackend::new();
        backend.set_monitors(vec![
            MonitorInfo { geometry: Rect::new(Point::new(0, 0), Size::new(2560, 1600)), name: "eDP-1".to_string(), primary: true },
            MonitorInfo { geometry: Rect::new(Point::new(2560, 0), Size::new(1920, 1080)), name: "HDMI-A-1".to_string(), primary: false },
        ]);
        let wm = manager(backend);
        let theme = theme();
        let mut surroundings = surroundings(&theme);
        surroundings.pointer_root = Point::new(3000, 100);
        let s = snapshot(&wm, &surroundings);
        assert_eq!(s.outputs.focused, Some(1), "the fake backend reports no pointer, so the shell's last root position decides");
        assert_eq!(s.outputs.outputs.len(), 2);
        assert_eq!(s.outputs.outputs[1], OutputEntry { index: 1, name: "HDMI-A-1".to_string(), x: 2560, y: 0, width: 1920, height: 1080, scale: 2.0 });
    }

    #[test]
    fn a_backend_with_no_monitors_reports_one_output_called_screen() {
        let mut backend = FakeBackend::new();
        backend.set_monitors(vec![]);
        let wm = manager(backend);
        let theme = theme();
        let s = snapshot(&wm, &surroundings(&theme));
        assert_eq!(s.outputs.focused, Some(0));
        assert_eq!(s.outputs.outputs, vec![OutputEntry { index: 0, name: "screen".to_string(), x: 0, y: 0, width: 1600, height: 1200, scale: 2.0 }]);
    }

    #[test]
    fn theme_reports_the_active_theme_and_appearance_and_no_following_yet() {
        let wm = manager(FakeBackend::new());
        let theme = theme();
        let mut surroundings = surroundings(&theme);
        surroundings.appearance = Appearance::Light;
        let s = snapshot(&wm, &surroundings);
        assert_eq!(
            s.theme,
            ThemeEvent { id: "nextstep-classic".to_string(), name: "NeXTSTEP Classic".to_string(), appearance: "light".to_string(), following: None }
        );
    }

    #[test]
    fn a_session_following_omarchy_says_so_in_the_theme_event() {
        let wm = manager(FakeBackend::new());
        let theme = theme();
        let mut surroundings = surroundings(&theme);
        surroundings.following = Some("omarchy".to_string());
        let s = snapshot(&wm, &surroundings);
        assert_eq!(s.theme.following.as_deref(), Some("omarchy"));
        let line = serde_json::to_string(&Event::Theme(s.theme)).unwrap();
        assert!(line.ends_with(",\"following\":\"omarchy\"}"), "{line}");
    }

    #[test]
    fn a_switch_the_shell_applies_changes_only_the_facets_it_touched() {
        // End to end through the real types: request in, command out,
        // switch applied to a real window manager, snapshot republished
        // — the client sees `workspaces` (and `focus`, since the focused
        // window is no longer on the active workspace) and nothing else.
        let mut backend = FakeBackend::new();
        let (w1, w2) = (backend.create_window(), backend.create_window());
        let mut wm = manager(backend);
        let (_c1, c2) = (map(&mut wm, w1), map(&mut wm, w2));
        wm.move_client_to_workspace(c2, 1);
        let theme = theme();
        let first = snapshot(&wm, &surroundings(&theme));
        let (_scratch, mut socket, mut bar) = connected(&first);
        bar.lines(5);

        bar.send("{\"request\":\"focus-workspace\",\"index\":1}\n");
        let commands = service_after_request(&mut socket, &first);
        for command in commands {
            let Command::FocusWorkspace(index) = command;
            wm.switch_workspace(index);
        }
        socket.publish(&snapshot(&wm, &surroundings(&theme)));
        let lines = bar.lines(1);
        assert_eq!(lines[0]["event"], "workspaces");
        assert_eq!(lines[0]["active"], 1);
        assert_eq!(wm.current_workspace(), 1);
        assert_eq!(wm.workspace_count(), 2, "a switch never creates");
    }
}
