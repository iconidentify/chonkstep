
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};

use chonk_ipc::{self as transport, Stream, StreamListener};
use serde::{Deserialize, Serialize};
use wm_core::{Backend, Lifecycle, WindowManager};
use wm_theme::{Appearance, Theme};
use wm_theme_api::{DecorationStyle, Point};

use crate::spawn;

/// The integer version of `docs/control-socket.md` this build speaks.
pub(crate) const PROTOCOL: u32 = 2;

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

/// Most simultaneous control subscribers retained by the shell.
pub(crate) const MAX_CLIENTS: usize = 64;

/// Limit connection churn as well as requests: a process that keeps
/// refilling the listener must not trap the shell in accept(), even
/// when every connection is refused because the client list is full.
const MAX_ACCEPTS_PER_PASS: usize = 8;

/// How much all clients together may hand the shell in one servicing
/// pass. Requests are drained per line as they arrive, so this is not
/// a framing limit; it bounds the whole read phase even when every
/// retained client is writing faster than the shell parses.
const READ_BUDGET: usize = 2 * LINE_CAP;

/// Most requests one client may have acted on in one servicing pass.
/// Every request is real work for the compositor thread — a
/// `focus-workspace` is a workspace switch, a `debug` is a walk of the
/// whole scene — and the byte budget alone lets a client pack a few
/// thousand of either into one read. Complete lines past this cap stay
/// parked in the client's inbound buffer and are handled, in order, on
/// the passes that follow; none is ever dropped, so the spec's one
/// answer per request holds, only later. A bar sends a handful of
/// requests a minute and never notices.
pub(crate) const MAX_REQUESTS_PER_PASS: usize = 16;

/// Most requests all clients together may have acted on in one pass —
/// the population-wide ceiling that [`READ_BUDGET`] is for bytes, so
/// sixty-four flooding clients cannot multiply the per-client cap into
/// a thousand switches. The rotating first reader keeps a client behind
/// the spent budget from starving.
pub(crate) const REQUEST_BUDGET: usize = 4 * MAX_REQUESTS_PER_PASS;

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
    Debug(DebugEvent),
    Error(ErrorEvent),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct DebugEvent {
    pub topic: String,
    pub data: String,
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
    /// Additive protocol field; old snapshots imply the existing frame recipe.
    #[serde(default)]
    pub decoration_style: DecorationStyle,
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
    Debug { topic: String },
}

/// Everything the shell publishes, as the four facet events it is
/// published as. Built fresh from the live `WindowManager` when its
/// cheap semantic stamp changes or a client has a request to answer;
/// compared facet-by-facet against the last one sent so an irrelevant
/// invalidation remains a silent socket.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snapshot {
    pub workspaces: WorkspacesEvent,
    pub outputs: OutputsEvent,
    pub focus: FocusEvent,
    pub theme: ThemeEvent,
}

/// Cheap identity of every input that can change a control snapshot.
///
/// The full [`Snapshot`] is deliberately not used as its own dirty
/// check: constructing it is the work this stamp exists to avoid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotStamp {
    /// `WindowManager` client/focus/workspace semantic revision.
    pub wm: u64,
    /// Workarea publication revision, which also catches output changes.
    pub workareas: u64,
    /// Monitor currently under the pointer.
    pub focused_output: usize,
    /// Shell-owned theme, appearance, following, and scale revision.
    pub theme: u64,
}

impl Snapshot {
    /// One facet as its event, by its position in the §3 order.
    fn event(&self, index: usize) -> Event {
        match index {
            0 => Event::Workspaces(self.workspaces.clone()),
            1 => Event::Outputs(self.outputs.clone()),
            2 => Event::Focus(self.focus.clone()),
            _ => Event::Theme(self.theme.clone()),
        }
    }

    /// The facets as events, in the order §3 lists them — the order a
    /// client is promised on accept and on `snapshot`. The wire path
    /// goes through [`Facets`] instead, which serialises each once.
    #[cfg(test)]
    fn events(&self) -> [Event; 4] {
        std::array::from_fn(|index| self.event(index))
    }
}

/// A snapshot's facets as wire lines, each serialised at most once per
/// servicing pass however many clients are owed it. Lazy, so a pass in
/// which nothing changed and nobody asked serialises nothing at all;
/// before this every client cloned and re-encoded every facet for
/// itself.
struct Facets<'a> {
    snapshot: &'a Snapshot,
    lines: [std::cell::OnceCell<Vec<u8>>; 4],
}

impl<'a> Facets<'a> {
    fn new(snapshot: &'a Snapshot) -> Self {
        Self { snapshot, lines: std::array::from_fn(|_| std::cell::OnceCell::new()) }
    }

    fn line(&self, index: usize) -> &[u8] {
        self.lines[index].get_or_init(|| line(&self.snapshot.event(index)))
    }
}

/// What the shell knows that the window manager does not, handed in
/// alongside the `WindowManager` when a [`Snapshot`] is taken.
pub(crate) struct Surroundings<'a> {
    pub theme: &'a Theme,
    pub appearance: Appearance,
    pub decoration_style: DecorationStyle,
    /// The shell's UI scale — one number for every output today.
    pub scale: f32,
    /// The pointer's last known root position, for `outputs.focused`
    /// when the backend cannot report the pointer itself.
    pub pointer_root: Point,
    /// See [`ThemeEvent::following`].
    pub following: Option<String>,
}

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

    let monitors = wm.monitors_ref();
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
            focused: Some(if wm.separate_spaces() { wm.active_output_index() } else { wm.monitor_index_at(pointer) }),
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
            decoration_style: surroundings.decoration_style,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// `focus-workspace`, already validated against the workspace list
    /// in the snapshot the request was serviced with. The shell applies
    /// it with `WindowManager::switch_workspace`, which cannot create a
    /// workspace at an index that already exists — so the validation
    /// here is what keeps the spec's "a switch, never a create".
    FocusWorkspace(usize),
    /// A live inspection request, tagged with the one connection owed
    /// the answer so diagnostics are never broadcast to every bar.
    Debug { client: u64, topic: String },
}

// ---------------------------------------------------------------------
// One connected client
// ---------------------------------------------------------------------

struct ControlClient {
    id: u64,
    stream: Stream,
    /// Bytes received and not yet handled: complete lines parked by
    /// [`MAX_REQUESTS_PER_PASS`] for a later pass, then at most one
    /// partial line still waiting for its newline.
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
    fn new(id: u64, stream: Stream) -> Self {
        Self {
            id,
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
        self.queue_line(&line(event));
    }

    fn queue_line(&mut self, bytes: &[u8]) {
        self.outbound.extend_from_slice(bytes);
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
    /// judged without a read — and even that one first gets the lines
    /// an earlier pass parked, or a burst followed by a half-close
    /// would lose everything after the first pass's cap.
    ///
    /// `requests` is the pass-wide [`REQUEST_BUDGET`]; this client's
    /// own share is [`MAX_REQUESTS_PER_PASS`]. Reading stops when either
    /// is spent. What is left in the kernel keeps the descriptor
    /// readable, and what is parked here is reported by
    /// [`has_backlog`](Self::has_backlog), so the next pass follows.
    fn read(
        &mut self,
        now: &Snapshot,
        commands: &mut Vec<Command>,
        budget: &mut usize,
        requests: &mut usize,
    ) -> Option<Farewell> {
        let mut handled = 0;
        // Parked lines first: they are older than anything still in
        // the kernel, and answers go out in the order requests came.
        if let Some(farewell) = self.drain_lines(now, commands, &mut handled, requests) {
            return Some(farewell);
        }
        if self.peer_finished {
            return self.peer_gone().then_some(Farewell::ClosedByPeer);
        }
        let mut buffer = [0u8; 4096];
        while *budget > 0 && handled < MAX_REQUESTS_PER_PASS && *requests > 0 {
            let want = buffer.len().min(*budget);
            match self.stream.recv(&mut buffer[..want]) {
                Ok(0) => {
                    self.peer_finished = true;
                    // Whatever arrived before the shutdown still counts;
                    // a request without its newline is dropped, as the
                    // spec's framing says it must be.
                    return self
                        .drain_lines(now, commands, &mut handled, requests)
                        .or_else(|| self.peer_gone().then_some(Farewell::ClosedByPeer));
                }
                Ok(n) => {
                    *budget -= n;
                    self.inbound.extend_from_slice(&buffer[..n]);
                    if let Some(farewell) = self.drain_lines(now, commands, &mut handled, requests) {
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

    /// Whether a read this pass can make progress or discover the
    /// peer's departure. This is only a readiness query: the real read
    /// remains in [`Self::read`], where framing and budgets are enforced.
    ///
    /// `readable` is the event loop's verdict for the whole control
    /// descriptor set — level-triggered, so it is exactly the question
    /// a per-client `poll` would ask — or `None` on a loop that keeps
    /// no such record (X11), which asks the kernel directly. A peer
    /// that has finished writing is the one exception: its descriptor
    /// is readable forever (EOF is readable) and has been taken out of
    /// the wake set, so its full departure is still asked for here.
    fn input_pending(&self, readable: Option<bool>) -> bool {
        if self.peer_finished {
            return self.peer_gone();
        }
        if let Some(readable) = readable {
            return readable;
        }
        let mut fd = libc::pollfd { fd: self.stream.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one live descriptor owned by this client, queried with
        // a zero timeout. Failure means "not known ready" and is retried
        // by the next bounded housekeeping pass.
        let ready = unsafe { libc::poll(&mut fd, 1, 0) };
        ready > 0 && fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
    }

    /// Whether a complete line is parked for a later pass. Such a line
    /// is already out of the kernel, so no descriptor will wake the
    /// loop for it; the shell asks this instead.
    fn has_backlog(&self) -> bool {
        self.inbound.contains(&b'\n')
    }

    /// Whether the event loop should wake for this descriptor. A peer
    /// that has shut down its writing side never sends another byte,
    /// and its EOF would keep a level-triggered source firing on every
    /// pass; its full close is noticed by [`Self::peer_gone`] on the
    /// housekeeping cadence, or by the first write that fails.
    fn wants_wakeups(&self) -> bool {
        !self.peer_finished
    }

    /// Consumes complete lines from `inbound`, oldest first, until this
    /// pass's share of requests — this client's `handled` against
    /// [`MAX_REQUESTS_PER_PASS`], the pass's `requests` against
    /// [`REQUEST_BUDGET`] — is spent, then checks what is left against
    /// the cap. Only a *partial* line is judged: one longer than any
    /// legal whole line is a client that has lost its framing. Complete
    /// lines parked by the caps are legitimate backlog, not a framing
    /// error, and are never disconnected for their bulk — which stays
    /// bounded by the read budget regardless.
    ///
    /// The buffer is shifted once at the end rather than per line so a
    /// flood of blank lines, which cost no request each, is linear in
    /// the bytes read rather than quadratic.
    fn drain_lines(
        &mut self,
        now: &Snapshot,
        commands: &mut Vec<Command>,
        handled: &mut usize,
        requests: &mut usize,
    ) -> Option<Farewell> {
        let mut consumed = 0;
        while *handled < MAX_REQUESTS_PER_PASS && *requests > 0 {
            let Some(len) = self.inbound[consumed..].iter().position(|&b| b == b'\n') else { break };
            if len + 1 > LINE_CAP {
                return Some(Farewell::LineOverflow);
            }
            let start = consumed;
            consumed += len + 1;
            // §1: empty lines are ignored — and "empty" includes the
            // `\r` a telnet-minded client leaves behind.
            if self.inbound[start..start + len].iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let body = self.inbound[start..start + len].to_vec();
            self.handle(&body, now, commands);
            *handled += 1;
            *requests -= 1;
        }
        self.inbound.drain(..consumed);
        let partial = self
            .inbound
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(self.inbound.len(), |newline| self.inbound.len() - newline - 1);
        (partial >= LINE_CAP).then_some(Farewell::LineOverflow)
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
            Ok(Request::Debug { topic }) => {
                if matches!(topic.as_str(), "scene" | "focus" | "clients") {
                    commands.push(Command::Debug { client: self.id, topic });
                } else {
                    self.queue(&Event::Error(ErrorEvent {
                        request: Some("debug".to_string()),
                        message: "topic must be scene, focus, or clients".to_string(),
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
    fn publish(&mut self, now: &Facets<'_>, changed: &[bool; 4]) {
        let owed = std::mem::take(&mut self.owed_workspaces);
        if std::mem::take(&mut self.wants_snapshot) {
            for index in 0..4 {
                self.queue_line(now.line(index));
            }
            return;
        }
        for (index, &differs) in changed.iter().enumerate() {
            if differs || (index == 0 && owed) {
                self.queue_line(now.line(index));
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
    /// Inputs consumed by the most recently constructed snapshot.
    /// Kept separately from `last`: comparing `last` would first have
    /// to construct the allocation-heavy value this gate avoids.
    observed: Option<SnapshotStamp>,
    /// First client offered the shared read budget on the next pass.
    /// Advancing it even when the budget was exhausted prevents one
    /// permanent writer at the front from starving every later peer.
    read_cursor: usize,
    /// Suppresses one warning per refused connection while the socket
    /// remains continuously at its population cap. Reset as soon as a
    /// slot opens so a later leak episode is visible again.
    cap_refusing: bool,
    capacity_refusals: u64,
    next_client_id: u64,
}

impl ControlSocket {
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
                Self {
                    listener: Some(listener),
                    socket_path,
                    clients: Vec::new(),
                    hello: Self::hello(),
                    last: None,
                    observed: None,
                    read_cursor: 0,
                    cap_refusing: false,
                    capacity_refusals: 0,
                    next_client_id: 1,
                }
            }
            Err(error) => {
                tracing::warn!(?error, socket = %socket_path.display(), "could not bind the control socket; it is unavailable this session");
                Self::unbound(socket_path)
            }
        }
    }

    fn unbound(socket_path: PathBuf) -> Self {
        Self {
            listener: None,
            socket_path,
            clients: Vec::new(),
            hello: Self::hello(),
            last: None,
            observed: None,
            read_cursor: 0,
            cap_refusing: false,
            capacity_refusals: 0,
            next_client_id: 1,
        }
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

    /// Whether the shell must construct a snapshot this pass.
    ///
    /// A changed cheap stamp needs a diff. So does client data, whether
    /// still in the kernel or parked here by the per-pass request cap:
    /// requests are interpreted against the current workspace count, and
    /// a freshly accepted client's `wants_snapshot` is its same-tick
    /// publication guarantee. Quiet clients with only buffered outbound
    /// bytes return false; [`Self::flush_pending`] serves those without a
    /// snapshot.
    ///
    /// `readable` is what the event loop recorded for the control
    /// descriptors since the last pass, so a quiet pass costs no syscall
    /// per client; `None` asks the kernel instead, for a loop that keeps
    /// no such record. See [`ControlClient::input_pending`].
    pub(crate) fn snapshot_needed(&self, stamp: SnapshotStamp, readable: Option<bool>) -> bool {
        self.has_clients()
            && (self.observed != Some(stamp)
                || self
                    .clients
                    .iter()
                    .any(|client| client.wants_snapshot || client.has_backlog() || client.input_pending(readable)))
    }

    /// Whether any client has requests parked for a later pass. Those
    /// wake no descriptor, so the shell schedules the next pass itself
    /// while this holds.
    pub(crate) fn has_backlog(&self) -> bool {
        self.clients.iter().any(ControlClient::has_backlog)
    }

    /// Records the cheap inputs represented by a snapshot that was just
    /// constructed and serviced.
    pub(crate) fn note_snapshot(&mut self, stamp: SnapshotStamp) {
        self.observed = Some(stamp);
    }

    /// Retries queued non-blocking writes and forgets dead readers without
    /// requiring a new desktop snapshot.
    pub(crate) fn flush_pending(&mut self) {
        for client in &mut self.clients {
            if let Some(farewell) = client.flush() {
                client.doom(farewell);
            }
        }
        self.clients.retain(|client| !client.doomed);
    }

    /// Admits at most [`MAX_ACCEPTS_PER_PASS`] connections per pass.
    /// The listener stays readable while more are queued, so the event
    /// loop returns for them after other desktop work gets a turn.
    /// A new client owes
    /// nothing yet but `hello`, which is queued here so it is first no
    /// matter what the client sends in the meantime; the snapshot
    /// follows on the next [`publish`](Self::publish) — which the shell
    /// calls in the same tick, with a snapshot taken *after* this, so a
    /// fresh client is never told stale state.
    pub(crate) fn accept(&mut self) {
        let Some(listener) = &self.listener else { return };
        if self.clients.len() < MAX_CLIENTS {
            self.cap_refusing = false;
        }
        for _ in 0..MAX_ACCEPTS_PER_PASS {
            match listener.accept() {
                Ok(Some(stream)) => {
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
                    if self.clients.len() >= MAX_CLIENTS {
                        self.capacity_refusals = self.capacity_refusals.saturating_add(1);
                        if !self.cap_refusing {
                            tracing::warn!(
                                retained = self.clients.len(),
                                maximum = MAX_CLIENTS,
                                refusals = self.capacity_refusals,
                                "control client limit reached; refusing excess connections"
                            );
                            self.cap_refusing = true;
                        }
                        // Dropping the accepted stream closes it now.
                        // Leaving it pending in the listener backlog
                        // would make a well-behaved reconnect hang.
                        continue;
                    }
                    let id = self.next_client_id;
                    self.next_client_id = self.next_client_id.wrapping_add(1).max(1);
                    let mut client = ControlClient::new(id, stream);
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
        let id = self.next_client_id;
        self.next_client_id = self.next_client_id.wrapping_add(1).max(1);
        let mut client = ControlClient::new(id, stream);
        client.queue(&self.hello);
        self.clients.push(client);
    }

    /// Queues one diagnostic answer on the exact connection that
    /// requested it. The shell builds `data` only after seeing this
    /// command, keeping scene and client enumeration off ordinary bar
    /// snapshot paths.
    pub(crate) fn answer_debug(&mut self, client: u64, topic: String, data: &str) {
        if let Some(peer) = self.clients.iter_mut().find(|peer| peer.id == client) {
            peer.queue(&Event::Debug(DebugEvent { topic, data: data.to_string() }));
        }
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
    ///
    /// The commands returned are bounded: at most [`MAX_REQUESTS_PER_PASS`]
    /// from any one client and [`REQUEST_BUDGET`] in all. Requests past
    /// that wait, in order, for the next pass.
    pub(crate) fn service(&mut self, now: &Snapshot) -> Vec<Command> {
        let mut commands = Vec::new();
        let changed = self.note(now);
        let facets = Facets::new(now);
        let client_count = self.clients.len();
        let start = self.read_cursor.min(client_count.saturating_sub(1));
        let mut budget = READ_BUDGET;
        let mut requests = REQUEST_BUDGET;
        for offset in 0..client_count {
            let index = (start + offset) % client_count;
            let client = &mut self.clients[index];
            client.publish(&facets, &changed);
            if let Some(farewell) = client.read(now, &mut commands, &mut budget, &mut requests) {
                client.doom(farewell);
                continue;
            }
            if client.wants_snapshot {
                client.publish(&facets, &[false; 4]);
            }
            if let Some(farewell) = client.flush() {
                client.doom(farewell);
            }
        }
        self.clients.retain(|client| !client.doomed);
        self.read_cursor = if self.clients.is_empty() {
            0
        } else {
            (start + 1) % self.clients.len()
        };
        commands
    }

    /// Tells every client what changed since the last publish (or
    /// everything, to a client that is owed a snapshot), writes as much
    /// as the kernel will take, and drops clients that have stopped
    /// reading. The comparison is per facet, so a title change costs
    /// one `focus` line and nothing else.
    pub(crate) fn publish(&mut self, now: &Snapshot) {
        let changed = self.note(now);
        let facets = Facets::new(now);
        for client in &mut self.clients {
            client.publish(&facets, &changed);
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

    /// The descriptors the event loop should wake for: the listener and
    /// every client that can still send something. See
    /// [`ControlClient::wants_wakeups`] for the ones left out.
    pub(crate) fn poll_fds(&self) -> impl Iterator<Item = RawFd> + '_ {
        self.listener
            .iter()
            .map(|l| l.as_raw_fd())
            .chain(self.clients.iter().filter(|c| c.wants_wakeups()).map(|c| c.stream.as_raw_fd()))
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

/// Stable display identity for the external bar control socket.
pub(crate) fn current_display() -> String {
    std::env::var("WAYLAND_DISPLAY")
        .or_else(|_| std::env::var("DISPLAY"))
        .unwrap_or_else(|_| "default".to_string())
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
                decoration_style: DecorationStyle::WindowMaker,
                following: None,
            },
        }
    }

    fn sample_stamp() -> SnapshotStamp {
        SnapshotStamp { wm: 7, workareas: 3, focused_output: 0, theme: 2 }
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

    #[test]
    fn accepting_past_the_named_cap_never_retains_the_excess() {
        let scratch = Scratch::new();
        let mut socket = ControlSocket::bind_at(scratch.socket());
        let mut bars = Vec::new();

        // Drain the listener after each connect so the transport's
        // pending backlog does not hide an unbounded accepted list.
        for _ in 0..MAX_CLIENTS + 8 {
            bars.push(Bar::connect(&socket));
            socket.accept();
        }

        assert_eq!(socket.client_count(), MAX_CLIENTS);
        assert_eq!(socket.poll_fds().count(), 1 + MAX_CLIENTS);
        assert_eq!(socket.capacity_refusals, 8);
        assert!(socket.cap_refusing);
    }

    #[test]
    fn connection_churn_at_capacity_yields_between_accept_batches() {
        let scratch = Scratch::new();
        let mut socket = ControlSocket::bind_at(scratch.socket());
        let _subscribers: Vec<_> = (0..MAX_CLIENTS).map(|_| paired(&mut socket)).collect();
        let _queued: Vec<_> = (0..2 * MAX_ACCEPTS_PER_PASS).map(|_| Bar::connect(&socket)).collect();

        socket.accept();
        assert_eq!(socket.client_count(), MAX_CLIENTS);
        assert_eq!(socket.capacity_refusals, MAX_ACCEPTS_PER_PASS as u64,
            "refused connections must spend the accept budget too");

        socket.accept();
        assert_eq!(socket.capacity_refusals, (2 * MAX_ACCEPTS_PER_PASS) as u64,
            "the remaining connections are refused on the next pass");
    }

    #[test]
    fn all_clients_share_one_read_budget_per_service_pass() {
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;

        let mut socket = ControlSocket::unbound(PathBuf::new());
        let mut writers = Vec::new();
        for _ in 0..5 {
            let (mut writer, accepted) = UnixStream::pair().unwrap();
            writer.write_all(&vec![b' '; LINE_CAP / 2]).unwrap();
            socket.admit(Stream::from_fd(accepted.into()));
            writers.push(writer);
        }

        let _ = socket.service(&sample_snapshot());
        let read = socket.clients.iter().map(|client| client.inbound.len()).sum::<usize>();
        assert!(read <= READ_BUDGET, "one pass consumed {read} bytes, above the aggregate {READ_BUDGET}-byte budget");
        assert_eq!(socket.clients[4].inbound.len(), 0, "the aggregate budget must stop this pass");

        let _ = socket.service(&sample_snapshot());
        assert_eq!(
            socket.clients[4].inbound.len(),
            LINE_CAP / 2,
            "the rotating first reader must keep a client behind the spent budget from starving"
        );
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

    fn wait_for_snapshot_need(socket: &ControlSocket, stamp: SnapshotStamp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.snapshot_needed(stamp, None) {
            assert!(Instant::now() < deadline, "the client request never became readable");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    // -----------------------------------------------------------------
    // The wire, checked against the document's own examples
    // -----------------------------------------------------------------

    #[test]
    fn a_quiet_connected_client_does_not_require_another_snapshot() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        let stamp = sample_stamp();
        socket.note_snapshot(stamp);

        assert!(!socket.snapshot_needed(stamp, None));
        socket.flush_pending();
        assert!(!socket.snapshot_needed(stamp, None), "socket maintenance is not a desktop invalidation");
    }

    #[test]
    fn a_fresh_client_and_a_readable_snapshot_request_force_one_build() {
        let scratch = Scratch::new();
        let mut socket = ControlSocket::bind_at(scratch.socket());
        let mut bar = Bar::connect(&socket);
        wait_for_accept(&mut socket);
        let stamp = sample_stamp();
        assert!(socket.snapshot_needed(stamp, None), "accept owes the initial snapshot in this tick");
        socket.service(&sample_snapshot());
        socket.note_snapshot(stamp);
        bar.lines(5);
        assert!(!socket.snapshot_needed(stamp, None));

        bar.send("{\"request\":\"snapshot\"}\n");
        wait_for_snapshot_need(&socket, stamp);
        socket.service(&sample_snapshot());
        socket.note_snapshot(stamp);
        assert_eq!(
            bar.lines(4).iter().map(|event| event["event"].as_str().unwrap()).collect::<Vec<_>>(),
            ["workspaces", "outputs", "focus", "theme"]
        );
        assert!(!socket.snapshot_needed(stamp, None));
    }

    #[test]
    fn every_cheap_snapshot_input_invalidates_the_gate() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        let stamp = sample_stamp();
        socket.note_snapshot(stamp);

        assert!(socket.snapshot_needed(SnapshotStamp { wm: stamp.wm + 1, ..stamp }, None));
        assert!(socket.snapshot_needed(SnapshotStamp { workareas: stamp.workareas + 1, ..stamp }, None));
        assert!(socket.snapshot_needed(SnapshotStamp { focused_output: 1, ..stamp }, None));
        assert!(socket.snapshot_needed(SnapshotStamp { theme: stamp.theme + 1, ..stamp }, None));
    }

    #[test]
    fn pointer_and_theme_stamp_changes_publish_their_exact_facets() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        let mut stamp = sample_stamp();
        socket.note_snapshot(stamp);

        let mut moved = snapshot.clone();
        moved.outputs.focused = Some(1);
        moved.outputs.outputs.push(OutputEntry {
            index: 1,
            name: "DP-1".to_string(),
            x: 2560,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
        });
        stamp.focused_output = 1;
        assert!(socket.snapshot_needed(stamp, None));
        socket.service(&moved);
        socket.note_snapshot(stamp);
        let output = bar.lines(1).pop().unwrap();
        assert_eq!(output["event"], "outputs");
        assert_eq!(output["focused"], 1);

        let mut restyled = moved;
        restyled.theme.id = "graphite".to_string();
        restyled.theme.name = "Graphite".to_string();
        stamp.theme += 1;
        assert!(socket.snapshot_needed(stamp, None));
        socket.service(&restyled);
        socket.note_snapshot(stamp);
        let theme = bar.lines(1).pop().unwrap();
        assert_eq!(theme["event"], "theme");
        assert_eq!(theme["id"], "graphite");
    }

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
            "{\"event\":\"theme\",\"id\":\"nextstep-classic\",\"name\":\"NeXTSTEP Classic\",\"appearance\":\"dark\",\"decoration_style\":\"windowmaker\",\"following\":null}\n"
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
    fn every_request_parses_and_extra_keys_are_ignored() {
        assert_eq!(parse_request(br#"{"request":"snapshot"}"#), Ok(Request::Snapshot));
        assert_eq!(parse_request(br#"{"request":"focus-workspace","index":2}"#), Ok(Request::FocusWorkspace { index: 2 }));
        assert_eq!(
            parse_request(br#"{"request":"debug","topic":"scene"}"#),
            Ok(Request::Debug { topic: "scene".to_string() })
        );
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
        assert_eq!(lines[0]["protocol"], 2);
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
    fn a_debug_request_answers_only_the_asking_client() {
        let snapshot = sample_snapshot();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        bar.send("{\"request\":\"debug\",\"topic\":\"scene\"}\n");
        let commands = service_after_request(&mut socket, &snapshot);
        let [Command::Debug { client, topic }] = commands.as_slice() else {
            panic!("debug request was not returned to the shell")
        };
        socket.answer_debug(*client, topic.clone(), "scene=test");
        socket.flush_pending();
        let event = bar.lines(1).remove(0);
        assert_eq!(event["event"], "debug");
        assert_eq!(event["topic"], "scene");
        assert_eq!(event["data"], "scene=test");
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

    // -----------------------------------------------------------------
    // Flooding: the work one pass does is bounded, the answers are not
    // -----------------------------------------------------------------

    /// A shell-side client on a socketpair whose far end the test
    /// writes and reads as it pleases, with no listener in between.
    fn paired(socket: &mut ControlSocket) -> std::os::unix::net::UnixStream {
        let (far, near) = std::os::unix::net::UnixStream::pair().unwrap();
        socket.admit(Stream::from_fd(near.into()));
        far
    }

    /// `count` valid `debug` requests whose topics cycle, so the order
    /// they come back in is checkable.
    fn debug_requests(count: usize) -> Vec<u8> {
        (0..count).flat_map(|i| format!("{{\"request\":\"debug\",\"topic\":\"{}\"}}\n", debug_topic(i)).into_bytes()).collect()
    }

    fn debug_topic(i: usize) -> &'static str {
        ["scene", "focus", "clients"][i % 3]
    }

    #[test]
    fn a_flood_of_requests_is_handled_a_few_per_pass_in_order_and_none_is_dropped() {
        use std::io::Write as _;

        let snapshot = sample_snapshot();
        let stamp = sample_stamp();
        let mut socket = ControlSocket::unbound(PathBuf::new());
        let far = paired(&mut socket);
        (&far).write_all(&debug_requests(1_000)).unwrap();

        let mut seen = Vec::new();
        let mut passes = 0;
        let mut parked_passes = 0;
        while seen.len() < 1_000 {
            assert!(socket.snapshot_needed(stamp, None), "requests were still waiting after {passes} passes");
            let commands = socket.service(&snapshot);
            socket.note_snapshot(stamp);
            passes += 1;
            assert!(commands.len() <= MAX_REQUESTS_PER_PASS, "pass {passes} acted on {} requests", commands.len());
            assert!(passes <= 1_000, "the flood never finished");
            for command in commands {
                let Command::Debug { topic, .. } = command else { panic!("a debug flood produced {command:?}") };
                seen.push(topic);
            }
            if socket.has_backlog() {
                parked_passes += 1;
                // Nothing in the kernel need be readable for a parked
                // line to be owed a pass.
                assert!(socket.snapshot_needed(stamp, Some(false)));
            }
            assert_eq!(socket.client_count(), 1, "a backlog is not a framing error");
        }
        assert_eq!(passes, 1_000usize.div_ceil(MAX_REQUESTS_PER_PASS), "every pass but the last acts on exactly the cap");
        assert!(parked_passes > 0, "the cap must have parked lines for later passes");
        for (i, topic) in seen.iter().enumerate() {
            assert_eq!(topic, debug_topic(i), "request {i} was answered out of order");
        }
        assert!(!socket.snapshot_needed(stamp, None), "nothing is left once the last request was acted on");
        assert!(!socket.snapshot_needed(stamp, Some(false)));
        assert_eq!(socket.client_count(), 1);
        drop(far);
    }

    #[test]
    fn parked_requests_wake_the_next_pass_without_a_readable_descriptor() {
        use std::io::Write as _;

        let snapshot = sample_snapshot();
        let stamp = sample_stamp();
        let mut socket = ControlSocket::unbound(PathBuf::new());
        let far = paired(&mut socket);
        // Small enough for one read to take all of it: after the first
        // pass the kernel holds nothing and the shell holds the rest.
        (&far).write_all(&debug_requests(40)).unwrap();

        assert_eq!(socket.service(&snapshot).len(), MAX_REQUESTS_PER_PASS);
        socket.note_snapshot(stamp);
        assert_eq!(socket.clients[0].inbound.iter().filter(|&&b| b == b'\n').count(), 40 - MAX_REQUESTS_PER_PASS);
        assert!(socket.has_backlog());
        assert!(socket.snapshot_needed(stamp, Some(false)), "the backlog is the shell's to remember, not the kernel's");

        assert_eq!(socket.service(&snapshot).len(), MAX_REQUESTS_PER_PASS);
        assert!(socket.snapshot_needed(stamp, Some(false)));
        assert_eq!(socket.service(&snapshot).len(), 40 - 2 * MAX_REQUESTS_PER_PASS);
        assert!(!socket.has_backlog());
        assert!(!socket.snapshot_needed(stamp, Some(false)), "an empty backlog on a quiet descriptor is a quiet pass");
        drop(far);
    }

    #[test]
    fn a_client_that_floods_and_half_closes_still_gets_every_answer() {
        use std::io::{BufRead as _, Write as _};

        let snapshot = sample_snapshot();
        let mut socket = ControlSocket::unbound(PathBuf::new());
        let far = paired(&mut socket);
        (&far).write_all(&debug_requests(1_000)).unwrap();
        far.shutdown(std::net::Shutdown::Write).unwrap();

        let mut answered = 0;
        let mut passes = 0;
        loop {
            let commands = socket.service(&snapshot);
            passes += 1;
            assert!(passes <= 1_000, "the flood never finished");
            assert!(commands.len() <= MAX_REQUESTS_PER_PASS);
            for command in commands {
                let Command::Debug { client, topic } = command else { panic!("a debug flood produced {command:?}") };
                socket.answer_debug(client, topic, "x");
                answered += 1;
            }
            socket.flush_pending();
            assert_eq!(socket.client_count(), 1, "a half-closed peer with a backlog is a listener, not a departure");
            if !socket.has_backlog() && answered == 1_000 {
                break;
            }
        }

        far.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = std::io::BufReader::new(&far);
        let mut line = String::new();
        let mut debug_lines = 0;
        for i in 0..1_005 {
            line.clear();
            assert!(reader.read_line(&mut line).expect("a line from the shell") > 0, "the shell closed after {i} lines");
            let event: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
            if event["event"] == "debug" {
                assert_eq!(event["topic"], debug_topic(debug_lines), "answer {debug_lines} is out of order");
                debug_lines += 1;
            }
        }
        assert_eq!(debug_lines, 1_000, "hello, four facets, then one answer per request");
    }

    #[test]
    fn the_request_budget_is_shared_and_the_first_reader_rotates() {
        use std::io::Write as _;

        let snapshot = sample_snapshot();
        let mut socket = ControlSocket::unbound(PathBuf::new());
        let fars: Vec<_> = (0..5)
            .map(|_| {
                let far = paired(&mut socket);
                (&far).write_all(&debug_requests(REQUEST_BUDGET)).unwrap();
                far
            })
            .collect();
        let ids: Vec<u64> = socket.clients.iter().map(|client| client.id).collect();
        let served = |commands: &[Command], id: u64| {
            commands.iter().filter(|command| matches!(command, Command::Debug { client, .. } if *client == id)).count()
        };

        let first = socket.service(&snapshot);
        assert_eq!(first.len(), REQUEST_BUDGET, "the pass stops at the shared budget");
        for &id in &ids[..4] {
            assert_eq!(served(&first, id), MAX_REQUESTS_PER_PASS, "each client ahead of the spent budget got its own cap");
        }
        assert_eq!(served(&first, ids[4]), 0, "the shared budget must stop this pass");

        let second = socket.service(&snapshot);
        assert_eq!(second.len(), REQUEST_BUDGET);
        assert_eq!(served(&second, ids[4]), MAX_REQUESTS_PER_PASS, "the rotating first reader keeps the last client from starving");
        assert_eq!(served(&second, ids[0]), 0);
        drop(fars);
    }

    #[test]
    fn an_unterminated_line_over_the_cap_behind_a_backlog_still_disconnects() {
        use std::io::Write as _;

        let snapshot = sample_snapshot();
        let mut socket = ControlSocket::unbound(PathBuf::new());
        let far = paired(&mut socket);
        let mut flood = debug_requests(MAX_REQUESTS_PER_PASS + 1);
        flood.extend(std::iter::repeat_n(b'x', LINE_CAP));
        (&far).write_all(&flood).unwrap();

        // Whether the shell judges the partial line this pass or the
        // next depends only on how the reads chunk; either way the
        // parked seventeenth request is not what disconnects it.
        let mut commands = Vec::new();
        for _ in 0..4 {
            commands.extend(socket.service(&snapshot));
        }
        assert_eq!(commands.len(), MAX_REQUESTS_PER_PASS + 1, "every complete line ahead of the bad one is still acted on");
        assert!(!socket.has_clients(), "a partial line past the cap is a framing violation, backlog or not");
        drop(far);
    }

    #[test]
    fn a_half_closed_client_leaves_the_wake_set_and_is_still_dropped_on_full_close() {
        let snapshot = sample_snapshot();
        let stamp = sample_stamp();
        let (_scratch, mut socket, mut bar) = connected(&snapshot);
        bar.lines(5);
        socket.note_snapshot(stamp);
        assert_eq!(socket.poll_fds().count(), 2);
        // SAFETY: shutting down the writing side of a socket the test
        // owns and keeps open for reading.
        unsafe {
            libc::shutdown(bar.stream.as_raw_fd(), libc::SHUT_WR);
        }
        std::thread::sleep(Duration::from_millis(5));
        socket.service(&snapshot);
        assert!(socket.has_clients());
        assert_eq!(socket.poll_fds().count(), 1, "a peer that will never write again would keep a level-triggered loop spinning");
        assert!(!socket.snapshot_needed(stamp, Some(false)), "and it owes no pass while it merely listens");

        drop(bar);
        std::thread::sleep(Duration::from_millis(5));
        socket.service(&snapshot);
        assert!(!socket.has_clients(), "its full close is still noticed on the housekeeping pass");
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
        Surroundings { theme, appearance: Appearance::Dark, decoration_style: DecorationStyle::WindowMaker, scale: 2.0, pointer_root: Point::new(10, 10), following: None }
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
            MonitorInfo {
                geometry: Rect::new(Point::new(0, 0), Size::new(2560, 1600)),
                name: "eDP-1".to_string(),
                identity: None,
                primary: true,
            },
            MonitorInfo {
                geometry: Rect::new(Point::new(2560, 0), Size::new(1920, 1080)),
                name: "HDMI-A-1".to_string(),
                identity: None,
                primary: false,
            },
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
    fn decoration_style_is_reported_and_old_theme_snapshots_remain_readable() {
        let old: ThemeEvent = serde_json::from_str(
            r#"{"id":"nextstep-classic","name":"Classic","appearance":"dark","following":null}"#
        ).unwrap();
        assert_eq!(old.decoration_style, DecorationStyle::WindowMaker);
        let theme = wm_theme::default_theme::nextstep_classic();
        let mut surroundings = surroundings(&theme);
        surroundings.decoration_style = DecorationStyle::System7;
        let wm = WindowManager::new(FakeBackend::new(), Box::new(FakeTheme));
        let event = snapshot(&wm, &surroundings).theme;
        assert_eq!(event.decoration_style, DecorationStyle::System7);
        assert!(text(&Event::Theme(event)).contains("\"decoration_style\":\"system7\""));
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
            ThemeEvent { id: "nextstep-classic".to_string(), name: "NeXTSTEP Classic".to_string(), appearance: "light".to_string(), decoration_style: DecorationStyle::WindowMaker, following: None }
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
            let Command::FocusWorkspace(index) = command else {
                panic!("workspace request returned a diagnostic command")
            };
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
