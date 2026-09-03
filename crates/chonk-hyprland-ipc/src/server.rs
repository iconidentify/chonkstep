//! The two sockets, and the table that answers requests.
//!
//! # Transport
//!
//! Hyprland's IPC is two Unix sockets in one directory:
//!
//! ```text
//! $XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock   requests
//! $XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock  events
//! ```
//!
//! They behave differently and it is worth being precise about how,
//! because the difference drives the whole design:
//!
//! - `.socket.sock` is **one connection, one request, one response,
//!   close**. There is no framing, no length header, no newline. The
//!   client writes its request, the server writes the answer and hangs
//!   up, and the client reads to EOF. A server that keeps the
//!   connection open leaves `hyprctl` blocked in `read()` forever.
//! - `.socket2.sock` is **write-only, line-oriented, and unbounded in
//!   time**. The client connects and never writes; the server streams
//!   `EVENT>>DATA\n` until one side goes away.
//!
//! # The compositor never blocks on a client
//!
//! This is the invariant `docs/control-socket.md` inherits from the
//! dockapp protocol, and it is inherited again here unchanged, for the
//! same reason: this codebase has already shipped the bug. The
//! `clippy.toml` at the workspace root exists because a wifi tile ran a
//! blocking `nmcli` on the repaint thread and froze the desktop for 3.6
//! seconds at a time, and the failure was reported as a display-driver
//! stall. A socket that a hostile or merely wedged client can stall is
//! the same bug with a better disguise.
//!
//! So, exactly as in `chonk-shell/src/control.rs`:
//!
//! - Every fd is non-blocking **by construction** — `SOCK_NONBLOCK` at
//!   `socket(2)` and `accept4(2)`, never an `fcntl` afterwards, so
//!   there is no window in which a blocking fd exists.
//! - Reads are budgeted per pass, so a flooding client costs a bounded
//!   number of bytes per tick rather than a tick.
//! - Writes are attempted once and judged afterwards: a client whose
//!   unsent backlog exceeds [`OUTBOUND_CAP`] has stopped reading, and is
//!   disconnected rather than waited for.
//! - Nothing here ever calls `poll` with a timeout, sleeps, or joins.
//!
//! # Security posture
//!
//! **This socket accepts commands, and it is not authenticated.**
//! That is the same choice `docs/control-socket.md` §1.2 makes, and it
//! rests on the same argument: everything this socket offers — switch
//! workspace, focus a window, close a window, run a command —
//! *the user's own keyboard already offers*. A token would not withhold
//! any capability from an attacker who can reach the socket, because
//! anything that can reach it is already running as this user inside
//! this session, and can simply synthesise the keystroke instead. What
//! a token would reliably do is stop the real `hyprctl` from working,
//! which is the entire point of the exercise.
//!
//! The access control is therefore positional, and layered three deep:
//!
//! 1. `$XDG_RUNTIME_DIR` is per-user and 0700, and the `hypr/`
//!    directory beneath it is created 0700 or verified to be. There is
//!    **no `/tmp` fallback** — Quickshell will look in `/tmp/hypr/` if
//!    the runtime dir is missing, and we deliberately decline to put a
//!    command-accepting socket in a world-writable directory where any
//!    local process can win a create race for the name.
//! 2. The socket itself is `chmod` 0600 after `bind`, because `bind`
//!    applies the umask and the umask is not ours to trust.
//! 3. `SO_PEERCRED` on accept, which restates rather than enforces:
//!    a socket that answers only to its own user should check, not
//!    assume.
//!
//! It is gated by one environment variable, `CHONKSTEP_HYPRLAND_IPC`
//! — see [`Server::enabled`] — and that gate is the honest answer to
//! "how do I turn it off", because impersonating another compositor is
//! a bigger claim than serving one's own control socket and a user is
//! entitled to decline it.

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use crate::dispatch::{self, Action, Outcome};
use crate::event::{Differ, Event};
use crate::request::{self, Request};
use crate::state::Snapshot;

use chonk_dock_proto::transport::{Stream, StreamListener};

/// Longest backlog held for a client that has stopped reading.
///
/// Same number as the control socket's, for the same reason: it is
/// several hundred events, which is far more than a bar can fall behind
/// by while still being a bar.
pub const OUTBOUND_CAP: usize = 262_144;

/// Bytes read from one client in one pass.
const READ_BUDGET: usize = 2 * request::MAX_REQUEST;

/// The environment variable that turns this server off.
pub const ENABLE_ENV: &str = "CHONKSTEP_HYPRLAND_IPC";

/// The variable both clients use to find the sockets.
pub const SIGNATURE_ENV: &str = "HYPRLAND_INSTANCE_SIGNATURE";

/// The version string served by `version` and `j/version`.
///
/// Hyprland's own `version` reports a tag like `v0.56.2`. Reporting a
/// Hyprland version we do not implement would be a lie of exactly the
/// kind this module forbids, and reporting nothing breaks callers that
/// parse it. So: a real chonkstep version, in the shape a parser
/// expects, with the truth in the fields a human reads.
fn version_json() -> serde_json::Value {
    serde_json::json!({
        "branch": "chonkstep",
        "commit": env!("CARGO_PKG_VERSION"),
        "dirty": false,
        "commit_message": "chonkstep serving Hyprland's IPC",
        "commit_date": "",
        "tag": concat!("v", env!("CARGO_PKG_VERSION")),
        "commits": "",
        "buildAquamarine": "",
        "buildHyprlang": "",
        "buildHyprutils": "",
        "buildHyprcursor": "",
        "buildHyprgraphics": "",
        "flags": ["chonkstep"],
    })
}

/// Answer one request against a snapshot.
///
/// Pure: no I/O, no state. Returns the response bytes and, when the
/// request asked for something to happen, the action to perform. This
/// split is what lets the whole request table be unit-tested without a
/// compositor, and it is why the caller — not this function — owns the
/// `&mut WindowManager`.
pub fn answer(request: &Request, snapshot: &Snapshot) -> (String, Option<Action>) {
    let json = request.wants_json();
    match request.command.as_str() {
        // `j/status` is requested FIRST by Quickshell, before it even
        // connects to the event socket, and the event socket is where
        // the bar's liveness comes from. Answering this wrong does not
        // degrade the bar; it disconnects it.
        "status" => (
            serde_json::json!({
                // Omarchy 4 configures Hyprland in Lua and Quickshell
                // reads this to decide which dispatch dialect to speak.
                // chonkstep is not configured in Lua, and saying "lua"
                // would make the bar send only Lua dispatch — which we
                // would then have to guess at. Say what is true and get
                // the classic dialect, which is the one we implement.
                "configProvider": "chonkstep",
            })
            .to_string(),
            None,
        ),
        "monitors" => (encode(json, &snapshot.monitors_json(), plain_monitors(snapshot)), None),
        "workspaces" => {
            (encode(json, &snapshot.workspaces_json(), plain_workspaces(snapshot)), None)
        }
        "clients" => (encode(json, &snapshot.clients_json(), plain_clients(snapshot)), None),
        "activewindow" => {
            let value = snapshot.active_window_json();
            if json {
                (value.to_string(), None)
            } else {
                (
                    snapshot
                        .focused_window()
                        .map(|window| format!("Window {} -> {}", window.address(), window.title))
                        .unwrap_or_default(),
                    None,
                )
            }
        }
        "activeworkspace" => {
            let value = snapshot.active_workspace_json();
            if json {
                (value.to_string(), None)
            } else {
                (plain_workspaces(snapshot), None)
            }
        }
        "version" => {
            let value = version_json();
            if json {
                (value.to_string(), None)
            } else {
                (format!("chonkstep {}\n", env!("CARGO_PKG_VERSION")), None)
            }
        }
        "cursorpos" => {
            // Plain text even under `-j`, matching Hyprland, and read by
            // `omarchy-capture-region` as `${pos%,*}` / `${pos#*, }` —
            // a comma AND a space, which is why the format string has
            // both.
            let (x, y) = snapshot
                .focused_monitor()
                .map(|monitor| (monitor.x + monitor.width / 2, monitor.y + monitor.height / 2))
                .unwrap_or((0, 0));
            (format!("{x}, {y}"), None)
        }
        "dispatch" => {
            let outcome = dispatch::parse(&request.args, snapshot);
            let response = outcome.response();
            match outcome {
                Outcome::Run(action) => (response, Some(action)),
                _ => (response, None),
            }
        }
        // Deliberately refused rather than faked. Each of these is a
        // real Omarchy caller, and each would take a wrong branch on a
        // plausible answer:
        //
        //  - `getoption` feeds `Style.qml`'s corner radius and gap. A
        //    made-up number would restyle the user's bar to match a
        //    compositor they are not running. Answering with the
        //    documented "does not exist" shape leaves Style.qml's
        //    `catch` to keep its previous value, which is right.
        //  - `keyword` and `reload` would claim to have changed a
        //    Hyprland config chonkstep does not read.
        //  - `binds` feeds the keybindings menu; ours are not Hyprland's.
        "getoption" => (
            if json {
                serde_json::json!({ "option": request.args, "set": false }).to_string()
            } else {
                "no such option".to_string()
            },
            None,
        ),
        "keyword" => (
            "Invalid dispatcher: chonkstep does not read a Hyprland config; keyword changes nothing"
                .to_string(),
            None,
        ),
        "reload" => (
            "Invalid dispatcher: chonkstep has no Hyprland config to reload".to_string(),
            None,
        ),
        "binds" => (if json { "[]".to_string() } else { String::new() }, None),
        "devices" => (
            // Shape matters more than content: `KeyboardLayout.qml`
            // checks `Array.isArray(parsed.keyboards)` and refuses to
            // speak for the seat unless it is one. An empty array is the
            // honest "chonkstep does not switch layouts" and keeps the
            // widget on its previous value instead of blanking it.
            serde_json::json!({
                "mice": [], "keyboards": [], "tablets": [], "touch": [], "switches": []
            })
            .to_string(),
            None,
        ),
        "configerrors" => (if json { "[]".to_string() } else { String::new() }, None),
        "splash" => ("chonkstep".to_string(), None),
        // Hyprland's literal reply for a verb it does not have, and the
        // exact string Quickshell tests for when probing `j/status`.
        other => (format!("unknown request: {other}"), None),
    }
}

fn encode<T: serde::Serialize>(json: bool, value: &T, plain: String) -> String {
    if json {
        serde_json::to_string(value).unwrap_or_else(|_| "[]".to_string())
    } else {
        plain
    }
}

fn plain_monitors(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    for monitor in &snapshot.monitors {
        out.push_str(&format!(
            "Monitor {} (ID {}):\n\t{}x{} at {}x{}\n\tscale: {:.2}\n\tfocused: {}\n\n",
            monitor.name,
            monitor.id,
            monitor.width,
            monitor.height,
            monitor.x,
            monitor.y,
            monitor.scale,
            if monitor.focused { "yes" } else { "no" },
        ));
    }
    out
}

fn plain_workspaces(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    for workspace in &snapshot.workspaces {
        out.push_str(&format!(
            "workspace ID {} ({}) on monitor {}:\n\twindows: {}\n\n",
            workspace.hypr_id(),
            workspace.hypr_name(),
            workspace.monitor,
            workspace.windows,
        ));
    }
    out
}

fn plain_clients(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    for window in &snapshot.windows {
        out.push_str(&format!(
            "Window {} -> {}:\n\tclass: {}\n\tat: {},{}\n\tsize: {},{}\n\tworkspace: {}\n\n",
            window.address(),
            window.title,
            window.class,
            window.x,
            window.y,
            window.width,
            window.height,
            window.workspace + 1,
        ));
    }
    out
}

/// Answer a whole payload, which may be a `[[BATCH]]`.
pub fn answer_payload(payload: &[u8], snapshot: &Snapshot) -> (String, Vec<Action>) {
    if let Some(segments) = request::split_batch(payload) {
        let mut response = String::new();
        let mut actions = Vec::new();
        for segment in segments {
            let Some(request) = Request::parse(&segment) else {
                continue;
            };
            let (text, action) = answer(&request, snapshot);
            response.push_str(&text);
            response.push('\n');
            actions.extend(action);
        }
        return (response, actions);
    }

    match Request::parse(payload) {
        Some(request) => {
            let (text, action) = answer(&request, snapshot);
            (text, action.into_iter().collect())
        }
        None => ("unknown request".to_string(), Vec::new()),
    }
}

// ---------------------------------------------------------------------
// The sockets.
// ---------------------------------------------------------------------

/// A client of the event socket.
struct EventClient {
    stream: Stream,
    outbound: Vec<u8>,
    doomed: bool,
}

/// A half-read request on the request socket.
struct RequestClient {
    stream: Stream,
    inbound: Vec<u8>,
    outbound: Vec<u8>,
    /// Set once the request has been answered: the connection is
    /// one-shot, so it is closed as soon as the answer is flushed.
    answered: bool,
    doomed: bool,
}

/// The Hyprland IPC server.
pub struct Server {
    directory: PathBuf,
    requests: Option<StreamListener>,
    events: Option<StreamListener>,
    request_clients: Vec<RequestClient>,
    event_clients: Vec<EventClient>,
    differ: Differ,
}

impl Server {
    /// Whether the session asked for this server.
    ///
    /// Off unless `CHONKSTEP_HYPRLAND_IPC=1`. Pretending to be another
    /// compositor is a larger claim than chonkstep's own control socket
    /// makes, and it changes how unrelated software behaves — so unlike
    /// the control socket, which is always on because a bar that must
    /// be told it exists is useless, this one is opted into.
    pub fn enabled() -> bool {
        matches!(
            std::env::var(ENABLE_ENV).as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        )
    }

    /// Generate an instance signature.
    ///
    /// Real Hyprland's is `<hash>_<unixtime>_<random>`; nothing in
    /// either client parses it, so the shape is free and only
    /// uniqueness matters. Including the pid makes two chonkstep
    /// sessions on one machine disjoint, which is the actual
    /// requirement.
    pub fn signature() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("chonkstep_{now}_{}", std::process::id())
    }

    /// Bind both sockets under `$XDG_RUNTIME_DIR/hypr/<signature>`.
    ///
    /// A failure here is logged by the caller and leaves the session
    /// running without Hyprland compatibility, exactly as a control
    /// socket bind failure does. Impersonation is a feature; it is not
    /// worth a login.
    pub fn bind(signature: &str) -> io::Result<Server> {
        let runtime = std::env::var("XDG_RUNTIME_DIR").map_err(|_| {
            io::Error::new(
                io::ErrorKind::NotFound,
                // Quickshell would fall back to /tmp/hypr here. We do
                // not; see the module doc's security posture.
                "XDG_RUNTIME_DIR is unset; refusing to put a command socket in /tmp",
            )
        })?;
        let hypr = Path::new(&runtime).join("hypr");
        // Sweep the corpses of previous sessions before adding ours.
        //
        // `Server::shut_down` removes this directory on a clean exit,
        // but a compositor that is killed — a crash, a `kill -9`, an
        // e2e harness tearing a nested session down — runs no
        // destructors, and its directory stays. The signature is unique
        // per session, so nothing collides and nothing breaks; the
        // directories simply accumulate in `$XDG_RUNTIME_DIR/hypr/`
        // forever, which is litter in a place the user did not choose
        // to have litter. Real Hyprland has the same wart. We can do
        // better cheaply, so we do.
        sweep_dead_instances(&hypr);

        let directory = hypr.join(signature);
        // 0700 explicitly, not `create_dir_all`'s umask-derived mode.
        // The umask is not ours to trust, and `transport`'s bind
        // refuses to put a socket under a directory that is not private
        // to this user — correctly, since this one accepts commands.
        // Real Hyprland leaves these 0755; we decline to.
        //
        // `hypr/` itself is created the same way rather than inherited,
        // because it may not exist and a parent created 0755 would put
        // the private leaf inside a listable one.
        for level in [directory.parent(), Some(directory.as_path())].into_iter().flatten() {
            match std::fs::DirBuilder::new().mode(0o700).create(level) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }

        let requests = StreamListener::bind(&directory.join(".socket.sock"))?;
        let events = StreamListener::bind(&directory.join(".socket2.sock"))?;

        Ok(Server {
            directory,
            requests: Some(requests),
            events: Some(events),
            request_clients: Vec::new(),
            event_clients: Vec::new(),
            differ: Differ::new(),
        })
    }

    /// Every fd the event loop should wake on.
    pub fn poll_fds(&self) -> Vec<RawFd> {
        let mut fds = Vec::new();
        fds.extend(self.requests.iter().map(AsRawFd::as_raw_fd));
        fds.extend(self.events.iter().map(AsRawFd::as_raw_fd));
        fds.extend(self.request_clients.iter().map(|c| c.stream.as_raw_fd()));
        fds.extend(self.event_clients.iter().map(|c| c.stream.as_raw_fd()));
        fds
    }

    pub fn has_clients(&self) -> bool {
        !self.request_clients.is_empty() || !self.event_clients.is_empty()
    }

    /// Accept everything pending on both listeners.
    pub fn accept(&mut self) {
        if let Some(listener) = &self.requests {
            while let Ok(Some(stream)) = listener.accept() {
                if !accepted_from_this_user(&stream) {
                    continue;
                }
                self.request_clients.push(RequestClient {
                    stream,
                    inbound: Vec::new(),
                    outbound: Vec::new(),
                    answered: false,
                    doomed: false,
                });
            }
        }
        if let Some(listener) = &self.events {
            while let Ok(Some(stream)) = listener.accept() {
                if !accepted_from_this_user(&stream) {
                    continue;
                }
                self.event_clients.push(EventClient {
                    stream,
                    outbound: Vec::new(),
                    doomed: false,
                });
            }
        }
    }

    /// Read requests, answer them, and return the actions they ask for.
    ///
    /// The caller applies the actions and then calls [`Server::publish`]
    /// with a *fresh* snapshot, so that the events a request causes
    /// describe the state it produced — the same two-snapshot discipline
    /// `Shell::service_control` uses.
    pub fn service(&mut self, snapshot: &Snapshot) -> Vec<Action> {
        let mut actions = Vec::new();
        for client in &mut self.request_clients {
            if client.answered {
                client.flush();
                continue;
            }
            let mut budget = READ_BUDGET;
            let mut buffer = [0_u8; 4096];
            loop {
                if budget == 0 {
                    break;
                }
                match client.stream.recv(&mut buffer) {
                    // A zero-length read on this socket means the client
                    // finished writing its request — which is exactly
                    // what `hyprctl` does, and is the cue to answer, not
                    // to hang up. Answering only on EOF would deadlock
                    // Quickshell, which does not shut down its write
                    // side; so the request is also answered as soon as
                    // any bytes have arrived, below.
                    Ok(0) => break,
                    Ok(n) => {
                        client.inbound.extend_from_slice(&buffer[..n]);
                        budget = budget.saturating_sub(n);
                        if client.inbound.len() > request::MAX_REQUEST {
                            client.doomed = true;
                            break;
                        }
                    }
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            || error.kind() == io::ErrorKind::Interrupted =>
                    {
                        break
                    }
                    Err(_) => {
                        client.doomed = true;
                        break;
                    }
                }
            }

            if client.doomed || client.inbound.is_empty() {
                continue;
            }

            let (response, mut requested) = answer_payload(&client.inbound, snapshot);
            actions.append(&mut requested);
            client.outbound.extend_from_slice(response.as_bytes());
            client.answered = true;
            client.flush();
        }
        // One connection, one request, one response, close: a client
        // whose answer has left is done with, and `hyprctl` reads to
        // EOF, so the close IS the framing.
        self.request_clients
            .retain(|client| !(client.doomed || client.answered && client.outbound.is_empty()));
        actions
    }

    /// Derive events from the new state and stream them.
    pub fn publish(&mut self, snapshot: &Snapshot) {
        let events = self.differ.diff(snapshot);
        if !events.is_empty() {
            for event in &events {
                self.broadcast(event);
            }
        }
        for client in &mut self.event_clients {
            client.flush();
        }
        self.event_clients.retain(|client| !client.doomed);
    }

    /// Queue one event to every connected event client.
    pub fn broadcast(&mut self, event: &Event) {
        let line = event.line();
        for client in &mut self.event_clients {
            client.outbound.extend_from_slice(line.as_bytes());
        }
    }

    /// Unlink both sockets and the directory.
    ///
    /// Called before a hot restart's `exec`, which runs no destructors.
    pub fn shut_down(&mut self) {
        self.request_clients.clear();
        self.event_clients.clear();
        self.requests = None;
        self.events = None;
        let _ = std::fs::remove_dir_all(&self.directory);
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

/// Remove instance directories left behind by sessions that are gone.
///
/// "Gone" is decided the way `transport::clear_stale_socket` decides
/// it, and for the same reason: by *probing the socket*, never by
/// trusting a name or a timestamp. A connect that succeeds — or that
/// fails with `WouldBlock`, meaning a live server with a full backlog —
/// means somebody is home, and that directory is left strictly alone.
/// Only a directory whose request socket refuses a connection outright
/// is removed.
///
/// This is deliberately conservative in the one direction that matters.
/// Deleting a live session's socket directory would take that session's
/// bar down, so every ambiguous case is resolved as "leave it": an
/// unreadable directory, a name we do not recognise as ours, a socket
/// that answers, or any error we did not expect. Litter is a much
/// cheaper mistake than eviction.
fn sweep_dead_instances(hypr: &Path) {
    let Ok(entries) = std::fs::read_dir(hypr) else {
        return;
    };
    for entry in entries.flatten() {
        // Only ever our own instances. A real Hyprland running beside
        // us on the same user owns its directory and it is not ours to
        // reap, whatever state it is in.
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("chonkstep_") {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match Stream::connect(&path.join(".socket.sock")) {
            // Someone is listening: a live session, ours or a previous
            // one that is still running. Leave it entirely alone.
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(_) => {
                if let Err(error) = std::fs::remove_dir_all(&path) {
                    tracing_warn(&format!(
                        "could not remove the stale hyprland ipc directory {}: {error}",
                        path.display()
                    ));
                }
            }
        }
    }
}

/// Warn without taking a `tracing` dependency.
///
/// This crate deliberately depends on serde and the socket transport
/// and nothing else (see its `Cargo.toml`), and one housekeeping
/// warning is not worth widening that. The compositor logs everything
/// that matters through its own subscriber; this line is for the case
/// where a directory cannot be removed, which is not actionable and not
/// worth a dependency.
fn tracing_warn(message: &str) {
    eprintln!("chonk-hyprland-ipc: {message}");
}

/// `SO_PEERCRED`, restating what the directory mode already enforces.
fn accepted_from_this_user(stream: &Stream) -> bool {
    stream.peer_is_this_user().unwrap_or(false)
}

impl RequestClient {
    fn flush(&mut self) {
        flush(&self.stream, &mut self.outbound, &mut self.doomed);
    }
}

impl EventClient {
    fn flush(&mut self) {
        flush(&self.stream, &mut self.outbound, &mut self.doomed);
    }
}

/// Write what the kernel will take, then judge what is left.
///
/// The order matters and is copied deliberately from
/// `ControlClient::flush`: a client that is keeping up must be judged on
/// what it has *not yet* taken, not on what it was about to.
fn flush(stream: &Stream, outbound: &mut Vec<u8>, doomed: &mut bool) {
    while !outbound.is_empty() {
        match stream.send(outbound) {
            Ok(0) => break,
            Ok(n) => {
                outbound.drain(..n);
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::Interrupted =>
            {
                break
            }
            Err(_) => {
                *doomed = true;
                return;
            }
        }
    }
    if outbound.len() > OUTBOUND_CAP {
        *doomed = true;
    }
}
