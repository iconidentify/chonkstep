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
//! - Reads share one aggregate budget per pass, so 64 flooding clients
//!   cost the same bounded number of bytes per tick as one.
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

/// Most simultaneous one-shot request connections retained by the server.
pub const MAX_REQUEST_CLIENTS: usize = 64;

/// Most simultaneous event-stream subscribers retained by the server.
pub const MAX_EVENT_CLIENTS: usize = 64;

/// Server passes a one-shot client may remain completely silent before
/// it is reaped. At the compositor's 100 ms maximum housekeeping wait
/// this gives an idle desktop about 25 seconds; animation-driven passes
/// shorten it, but a real `hyprctl` writes as part of connecting.
pub const REQUEST_IDLE_PASSES: u16 = 256;

/// Bytes read from every request client together in one server pass.
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
        "workspaces" => (encode(json, &snapshot.workspaces_json(), plain_workspaces(snapshot)), None),
        "clients" => (encode(json, &snapshot.clients_json(), plain_clients(snapshot)), None),
        "activewindow" => {
            let value = snapshot.active_window_json();
            if json {
                (value.to_string(), None)
            } else {
                (
                    snapshot
                        .focused_window()
                        .map(plain_client)
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
                (snapshot.active_workspace().map(|workspace| plain_workspace(snapshot, workspace)).unwrap_or_default(), None)
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
        "systeminfo" => (snapshot.system_info.clone(), None),
        "rollinglog" => (
            "ChonkStep writes the current Wayland session log under $XDG_STATE_HOME/chonkstep/wayland-session.log; use `hyprctl log-filter DIRECTIVE` to change live verbosity.\n".to_string(),
            None,
        ),
        "debug-set" => {
            let mut args = request.args.split_whitespace();
            let name = args.next().unwrap_or_default();
            let enabled = match args.next() {
                Some("1" | "true" | "on" | "yes") => Some(true),
                Some("0" | "false" | "off" | "no") => Some(false),
                _ => None,
            };
            match (name.is_empty(), enabled, args.next()) {
                (false, Some(enabled), None) => (
                    "ok".to_string(),
                    Some(Action::SetDiagnostic { name: name.to_string(), enabled }),
                ),
                _ => ("Invalid dispatcher: usage: debug-set KNOB BOOL".to_string(), None),
            }
        }
        "log-filter" => {
            let directive = request.args.trim();
            if directive.is_empty() {
                ("Invalid dispatcher: usage: log-filter DIRECTIVE".to_string(), None)
            } else {
                ("ok".to_string(), Some(Action::SetLogFilter(directive.to_string())))
            }
        }
        "cursorpos" => {
            // Plain text even under `-j`, matching Hyprland, and read by
            // `omarchy-capture-region` as `${pos%,*}` / `${pos#*, }` —
            // a comma AND a space, which is why the format string has
            // both.
            let (x, y) = snapshot.cursor_position.unwrap_or((0, 0));
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
        "eval" => {
            let outcome = dispatch::parse_eval(&request.args, snapshot);
            let response = outcome.response();
            match outcome {
                Outcome::Run(action) => (response, Some(action)),
                _ => (response, None),
            }
        }
        // Deliberately refused rather than faked. Each of these is a
        // real Omarchy caller, and each would consume a wrong value:
        //
        //  - `getoption` feeds `Style.qml`'s corner radius and gap. A
        //    made-up number would restyle the user's bar to match a
        //    compositor they are not running. Answering with the
        //    documented "does not exist" shape leaves Style.qml's
        //    `catch` to keep its previous value, which is right.
        //  - `keyword` would claim to have changed a Hyprland config;
        //    `reload` instead reloads chonkstep's live configuration.
        //  - `binds` and `devices` below report chonkstep's real seat.
        "getoption" => (
            if json {
                // This is the complete Hyprland option shape. The
                // value is explicitly unset rather than a fabricated
                // compositor setting, so callers can use their own
                // fallback without receiving `undefined` fields.
                serde_json::json!({
                    "option": request.args, "int": 0, "float": 0.0,
                    "str": "", "data": "0", "css": "0px", "set": false
                }).to_string()
            } else {
                "no such option".to_string()
            },
            None,
        ),
        // `keyword` stays a refusal for Hyprland's broad configuration
        // namespace. `cursor:invisible` is the one named exception:
        // Omarchy's screensaver ships it as the fallback for the
        // equivalent `hl.config` property, and the compositor can
        // honour it exactly.
        "keyword" => {
            let outcome = dispatch::parse_keyword(&request.args);
            let response = outcome.response();
            match outcome {
                Outcome::Run(action) => (response, Some(action)),
                _ => (response, None),
            }
        }
        "reload" => ("ok".to_string(), Some(Action::ReloadConfig)),
        "binds" => (if json { json_bindings(snapshot) } else { plain_bindings(snapshot) }, None),
        "devices" => (json_devices(snapshot), None),
        "configerrors" => (
            if json {
                serde_json::Value::Array(
                    snapshot.config_errors.iter().map(|error| serde_json::json!({ "error": error })).collect()
                ).to_string()
            } else {
                snapshot.config_errors.join("\n")
            },
            None,
        ),
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
        out.push_str(&plain_workspace(snapshot, workspace));
    }
    out
}

fn plain_workspace(snapshot: &Snapshot, workspace: &crate::state::Workspace) -> String {
    let last = snapshot.focused_window().filter(|window| window.workspace == workspace.index);
    format!(
        "workspace ID {} ({}) on monitor {}:\n\tmonitorID: {}\n\twindows: {}\n\thasfullscreen: {}\n\tlastwindow: {}\n\tlastwindowtitle: {}\n\n",
        workspace.hypr_id(), workspace.hypr_name(), workspace.monitor, workspace.monitor_id,
        workspace.windows, workspace.has_fullscreen,
        last.map(crate::state::Window::address).unwrap_or_else(|| "0x0".to_string()),
        last.map(|window| window.title.as_str()).unwrap_or_default(),
    )
}

fn plain_clients(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    for window in &snapshot.windows {
        out.push_str(&plain_client(window));
        out.push('\n');
    }
    out
}

fn plain_client(window: &crate::state::Window) -> String {
    format!(
        "Window {} -> {}:\n\tmapped: {}\n\thidden: {}\n\tat: {},{}\n\tsize: {},{}\n\tworkspace: {} ({})\n\tfloating: 1\n\tpseudo: 0\n\tmonitor: {}\n\tclass: {}\n\ttitle: {}\n\tinitialClass: {}\n\tinitialTitle: {}\n\tpid: {}\n\txwayland: {}\n\tpinned: {}\n\tfullscreen: {}\n",
        window.address(), window.title, !window.hidden, window.hidden, window.x, window.y,
        window.width, window.height, window.workspace + 1, window.workspace + 1,
        window.monitor, window.class, window.title, window.class, window.title, window.pid,
        window.xwayland, window.pinned, i32::from(window.fullscreen),
    )
}

fn plain_bindings(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    for binding in &snapshot.bindings {
        out.push_str(&format!(
            "bind\n\tlocked: {}\n\tmouse: false\n\trelease: {}\n\trepeat: {}\n\tlongPress: false\n\tnon_consuming: false\n\thas_description: {}\n\tmodmask: {}\n\tsubmap: \n\tkey: {}\n\tkeycode: 0\n\tcatch_all: false\n\tdescription: {}\n\tdispatcher: {}\n\targ: {}\n\n",
            binding.locked, binding.release, binding.repeating, !binding.description.is_empty(),
            binding.modifiers, binding.key, binding.description, binding.dispatcher, binding.argument,
        ));
    }
    out
}

fn json_bindings(snapshot: &Snapshot) -> String {
    serde_json::Value::Array(snapshot.bindings.iter().map(|binding| serde_json::json!({
        "locked": binding.locked, "mouse": false, "release": binding.release,
        "repeat": binding.repeating, "longPress": false, "non_consuming": false,
        "has_description": !binding.description.is_empty(), "modmask": binding.modifiers,
        "submap": "", "key": binding.key, "keycode": 0, "catch_all": false,
        "description": binding.description, "dispatcher": binding.dispatcher, "arg": binding.argument,
    })).collect()).to_string()
}

fn json_devices(snapshot: &Snapshot) -> String {
    let keyboards: Vec<_> = snapshot.devices.keyboards.iter().map(|keyboard| serde_json::json!({
        "address": keyboard.name, "name": keyboard.name, "rules": "", "model": "",
        "layout": keyboard.layout, "variant": "", "options": "",
        "active_keymap": keyboard.active_keymap, "active_layout_index": keyboard.active_layout_index,
        "main": true,
    })).collect();
    let devices = |items: &[crate::state::PointerDevice]| -> Vec<_> {
        items.iter().map(|device| serde_json::json!({ "address": device.name, "name": device.name })).collect()
    };
    serde_json::json!({
        "mice": devices(&snapshot.devices.mice), "keyboards": keyboards,
        "tablets": devices(&snapshot.devices.tablets), "touch": devices(&snapshot.devices.touch),
        "switches": devices(&snapshot.devices.switches),
    }).to_string()
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

/// Answer a payload and run each mutation before claiming success.
/// `hyprctl` exits zero for arbitrary response text, including an
/// `Invalid dispatcher` reply, so the wire text is diagnostic rather
/// than a reliable shell status. This boundary therefore makes the
/// stronger promise we can enforce: `ok` is written only after the
/// compositor reports that the requested action was applied.
pub fn answer_payload_applying<F>(payload: &[u8], snapshot: &Snapshot, mut apply: F) -> String
where
    F: FnMut(Action) -> bool,
{
    fn one<F>(request: &Request, snapshot: &Snapshot, apply: &mut F) -> String
    where
        F: FnMut(Action) -> bool,
    {
        let (response, action) = answer(request, snapshot);
        if let Some(action) = action {
            if apply(action) {
                response
            } else {
                "Invalid dispatcher: action could not be applied to the current desktop state".to_string()
            }
        } else {
            response
        }
    }

    if let Some(segments) = request::split_batch(payload) {
        let mut response = String::new();
        for segment in segments {
            if let Some(request) = Request::parse(&segment) {
                response.push_str(&one(&request, snapshot, &mut apply));
                response.push('\n');
            }
        }
        return response;
    }
    Request::parse(payload)
        .map(|request| one(&request, snapshot, &mut apply))
        .unwrap_or_else(|| "unknown request".to_string())
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
    /// Consecutive servicing/maintenance passes with no request byte.
    idle_passes: u16,
}

/// The Hyprland IPC server.
pub struct Server {
    directory: PathBuf,
    requests: Option<StreamListener>,
    events: Option<StreamListener>,
    request_clients: Vec<RequestClient>,
    event_clients: Vec<EventClient>,
    differ: Differ,
    refusals: u64,
    capacity_refusals: u64,
    request_cap_refusing: bool,
    event_cap_refusing: bool,
    /// First request client offered the aggregate read budget next.
    request_read_cursor: usize,
}

impl Server {
    /// Whether this session should answer as Hyprland.
    ///
    /// **On unless `CHONKSTEP_HYPRLAND_IPC` explicitly says otherwise**
    /// (`0`, `false`, `no`, or the empty string). This began as an
    /// opt-in, on the argument that impersonating another compositor is
    /// a larger claim than chonkstep's own control socket makes and
    /// should be made deliberately. The argument was right about the
    /// claim and wrong about who makes it: answering Hyprland's IPC is
    /// not a side feature of this desktop, it is most of what makes it
    /// usable as a drop-in under Omarchy, and a default that has to be
    /// discovered in a document is a default that is wrong on every
    /// machine nobody read it on. A user who wants the desktop without
    /// the impersonation still has one variable, and the reasons to
    /// want that are still in `docs/hyprland-ipc.md`.
    ///
    /// An unset variable is the common case and means yes. A value
    /// nobody anticipated (`maybe`) also means yes: the failure mode of
    /// a typo should be the feature working, not a silently inert
    /// server whose absence looks like a bug in Omarchy's tooling.
    pub fn enabled() -> bool {
        !matches!(std::env::var(ENABLE_ENV).as_deref(), Ok("0") | Ok("false") | Ok("no") | Ok(""))
    }

    /// Generate an instance signature.
    ///
    /// Real Hyprland's is `<hash>_<unixtime>_<random>`; nothing in
    /// either client parses it, so the shape is free and only
    /// uniqueness matters. Including the pid makes two chonkstep
    /// sessions on one machine disjoint, which is the actual
    /// requirement.
    pub fn signature() -> String {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
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
            refusals: 0,
            capacity_refusals: 0,
            request_cap_refusing: false,
            event_cap_refusing: false,
            request_read_cursor: 0,
        })
    }

    /// Every fd the event loop should wake on.
    pub fn poll_fds(&self) -> Vec<RawFd> {
        let mut fds = Vec::new();
        self.extend_poll_fds(&mut fds);
        fds
    }

    /// Appends every fd the event loop should wake on to reusable
    /// caller-owned storage.
    pub fn extend_poll_fds(&self, fds: &mut Vec<RawFd>) {
        fds.extend(self.requests.iter().map(AsRawFd::as_raw_fd));
        fds.extend(self.events.iter().map(AsRawFd::as_raw_fd));
        fds.extend(self.request_clients.iter().map(|c| c.stream.as_raw_fd()));
        fds.extend(self.event_clients.iter().map(|c| c.stream.as_raw_fd()));
    }

    /// Whether at least one request or event client is connected.
    #[must_use]
    pub fn has_clients(&self) -> bool {
        !self.request_clients.is_empty() || !self.event_clients.is_empty()
    }

    /// Whether at least one one-shot query client is connected.
    #[must_use]
    pub fn has_request_clients(&self) -> bool {
        !self.request_clients.is_empty()
    }

    /// Whether at least one event-stream subscriber is connected.
    #[must_use]
    pub fn has_event_clients(&self) -> bool {
        !self.event_clients.is_empty()
    }

    /// Accept everything pending on both listeners.
    pub fn accept(&mut self) {
        if self.request_clients.len() < MAX_REQUEST_CLIENTS {
            self.request_cap_refusing = false;
        }
        if let Some(listener) = &self.requests {
            while let Ok(Some(stream)) = listener.accept() {
                if !accepted_from_this_user(&stream) {
                    continue;
                }
                if self.request_clients.len() >= MAX_REQUEST_CLIENTS {
                    self.capacity_refusals = self.capacity_refusals.saturating_add(1);
                    if !self.request_cap_refusing {
                        tracing::warn!(
                            socket = "request",
                            retained = self.request_clients.len(),
                            maximum = MAX_REQUEST_CLIENTS,
                            refusals = self.capacity_refusals,
                            "hyprland IPC client limit reached; refusing excess connections"
                        );
                        self.request_cap_refusing = true;
                    }
                    continue;
                }
                self.request_clients.push(RequestClient {
                    stream,
                    inbound: Vec::new(),
                    outbound: Vec::new(),
                    answered: false,
                    doomed: false,
                    idle_passes: 0,
                });
            }
        }
        if self.event_clients.len() < MAX_EVENT_CLIENTS {
            self.event_cap_refusing = false;
        }
        if let Some(listener) = &self.events {
            while let Ok(Some(stream)) = listener.accept() {
                if !accepted_from_this_user(&stream) {
                    continue;
                }
                if self.event_clients.len() >= MAX_EVENT_CLIENTS {
                    self.capacity_refusals = self.capacity_refusals.saturating_add(1);
                    if !self.event_cap_refusing {
                        tracing::warn!(
                            socket = "event",
                            retained = self.event_clients.len(),
                            maximum = MAX_EVENT_CLIENTS,
                            refusals = self.capacity_refusals,
                            "hyprland IPC client limit reached; refusing excess connections"
                        );
                        self.event_cap_refusing = true;
                    }
                    continue;
                }
                self.event_clients.push(EventClient { stream, outbound: Vec::new(), doomed: false });
            }
        }
    }

    /// Read requests, apply mutations, and answer them.
    ///
    /// The caller applies the actions, observes its own semantic state
    /// revision, and calls [`Server::publish`] with a fresh snapshot only
    /// if that revision changed. An accepted no-op remains a successful
    /// request without forcing an empty event diff.
    ///
    /// Returns whether at least one decoded action was applied.
    #[must_use]
    pub fn service<F>(&mut self, snapshot: &Snapshot, mut apply: F) -> bool
    where
        F: FnMut(Action) -> bool,
    {
        let mut applied_any = false;
        let client_count = self.request_clients.len();
        let start = self.request_read_cursor.min(client_count.saturating_sub(1));
        let mut budget = READ_BUDGET;
        for offset in 0..client_count {
            let index = (start + offset) % client_count;
            let client = &mut self.request_clients[index];
            if client.answered {
                client.flush();
                continue;
            }
            // One request has no delimiter, so it must be allowed to
            // drain through its size cap and one byte beyond before a
            // response is parsed. Leaving less would turn a valid
            // fragmented request into a truncated one merely because
            // an earlier peer spent the shared budget.
            if budget <= request::MAX_REQUEST {
                continue;
            }
            let mut buffer = [0_u8; 4096];
            loop {
                if budget == 0 {
                    break;
                }
                let want = buffer.len().min(budget);
                match client.stream.recv(&mut buffer[..want]) {
                    // A zero-length read on this socket means the client
                    // finished writing its request — which is exactly
                    // what `hyprctl` does, and is the cue to answer, not
                    // to hang up. Answering only on EOF would deadlock
                    // Quickshell, which does not shut down its write
                    // side; so the request is also answered as soon as
                    // any bytes have arrived, below.
                    Ok(0) => {
                        // A connection that closes before writing a
                        // request is not an idle client; it is gone.
                        // This is the exact shape of the liveness probe
                        // in `sweep_dead_instances`: connect, learn
                        // that a server owns the path, then drop. The
                        // old code retained that empty EOF forever.
                        // Its fd consequently reported EPOLLHUP on
                        // every calloop wait, turning the compositor's
                        // 16 ms housekeeping loop into an unbounded
                        // busy loop. A live session measured 1,560
                        // passes/second and 99% of one CPU after 23
                        // such probes had accumulated.
                        if client.inbound.is_empty() {
                            client.doomed = true;
                        }
                        break;
                    }
                    Ok(n) => {
                        client.idle_passes = 0;
                        client.inbound.extend_from_slice(&buffer[..n]);
                        budget = budget.saturating_sub(n);
                        if client.inbound.len() > request::MAX_REQUEST {
                            client.doomed = true;
                            break;
                        }
                    }
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock || error.kind() == io::ErrorKind::Interrupted =>
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

            let response = answer_payload_applying(&client.inbound, snapshot, |action| {
                let applied = apply(action);
                applied_any |= applied;
                applied
            });
            for line in response.lines().filter(|line| {
                line.starts_with("Invalid dispatcher:") || line.starts_with("unknown request")
            }) {
                self.refusals = self.refusals.saturating_add(1);
                tracing::warn!(refusals = self.refusals, response = line, "hyprland IPC request refused");
            }
            client.outbound.extend_from_slice(response.as_bytes());
            client.answered = true;
            client.flush();
        }
        // One connection, one request, one response, close: a client
        // whose answer has left is done with, and `hyprctl` reads to
        // EOF, so the close IS the framing.
        self.request_clients.retain(|client| !(client.doomed || client.answered && client.outbound.is_empty()));
        self.request_read_cursor = if self.request_clients.is_empty() {
            0
        } else {
            (start + 1) % self.request_clients.len()
        };
        applied_any
    }

    /// Flush already-produced output and prune disconnected peers
    /// without constructing a desktop snapshot.
    ///
    /// This is the quiet-path counterpart to [`Self::service`] and
    /// [`Self::publish_owned`]: writable backpressure and an event
    /// subscriber's hangup still need maintenance, but neither can
    /// change the answer to a query or produce a compositor event.
    pub fn maintain(&mut self) {
        self.age_idle_request_clients();
        for client in &mut self.request_clients {
            if client.answered {
                client.flush();
            }
        }
        self.request_clients.retain(|client| !(client.doomed || client.answered && client.outbound.is_empty()));
        for client in &mut self.event_clients {
            client.flush();
        }
        self.event_clients.retain(|client| !client.doomed);
    }

    fn age_idle_request_clients(&mut self) {
        let mut reaped = 0_u64;
        for client in &mut self.request_clients {
            if client.doomed || client.answered || !client.inbound.is_empty() {
                continue;
            }
            client.idle_passes = client.idle_passes.saturating_add(1);
            if client.idle_passes >= REQUEST_IDLE_PASSES {
                client.doomed = true;
                reaped += 1;
            }
        }
        if reaped != 0 {
            tracing::warn!(reaped, idle_passes = REQUEST_IDLE_PASSES, "reaped silent hyprland IPC request clients");
        }
    }

    /// Whether output which hit socket backpressure still needs a
    /// later flush pass.
    #[must_use]
    pub fn has_pending_output(&self) -> bool {
        self.request_clients.iter().any(|client| !client.outbound.is_empty())
            || self.event_clients.iter().any(|client| !client.outbound.is_empty())
    }

    /// Number of refused request segments since this server started.
    pub fn refusal_count(&self) -> u64 {
        self.refusals
    }

    /// Derive events from the new state and stream them.
    pub fn publish(&mut self, snapshot: &Snapshot) {
        self.publish_owned(snapshot.clone());
    }

    /// Owned form of [`Self::publish`] for a compositor that created a
    /// snapshot specifically for this event pass.
    pub fn publish_owned(&mut self, snapshot: Snapshot) {
        let events = self.differ.diff_owned(snapshot);
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

    pub fn reset_diff(&mut self) {
        self.differ.reset();
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
        // Event clients never write, and an unchanged desktop queues
        // nothing to send. Without an explicit HUP check a client that
        // disconnected during that quiet period survived forever and
        // its permanently-readable fd spun the compositor loop.
        if self.stream.peer_gone() {
            self.outbound.clear();
            self.doomed = true;
            return;
        }
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
            Err(error) if error.kind() == io::ErrorKind::WouldBlock || error.kind() == io::ErrorKind::Interrupted => {
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

#[cfg(test)]
mod enabled_tests {
    use super::*;

    static SOCKET_COUNTER: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt as _;
            use std::sync::atomic::Ordering;

            let unique = SOCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "chonk-hyprland-ipc-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .unwrap();
            Self(directory)
        }

        fn socket(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The switch is read from the process environment, so these run
    /// under one lock rather than in parallel: `set_var`/`remove_var`
    /// are process-wide and two tests racing would flap.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_value(value: Option<&str>, check: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var(ENABLE_ENV).ok();
        match value {
            Some(v) => std::env::set_var(ENABLE_ENV, v),
            None => std::env::remove_var(ENABLE_ENV),
        }
        check();
        match previous {
            Some(v) => std::env::set_var(ENABLE_ENV, v),
            None => std::env::remove_var(ENABLE_ENV),
        }
    }

    #[test]
    fn the_common_case_is_an_unset_variable_and_it_means_yes() {
        with_value(None, || assert!(Server::enabled()));
    }

    #[test]
    fn configerrors_reports_the_retained_diagnostics_in_both_forms() {
        let snapshot = Snapshot {
            config_errors: vec!["bind: SUPER J — tiling-only".into(), "keyword: plugin — Hyprland-only".into()],
            ..Snapshot::default()
        };
        let plain = Request::parse(b"/configerrors").unwrap();
        assert_eq!(answer(&plain, &snapshot).0, snapshot.config_errors.join("\n"));

        let json = Request::parse(b"j/configerrors").unwrap();
        let value: serde_json::Value = serde_json::from_str(&answer(&json, &snapshot).0).unwrap();
        assert_eq!(value[0]["error"], snapshot.config_errors[0]);
        assert_eq!(value[1]["error"], snapshot.config_errors[1]);
    }

    #[test]
    fn only_a_deliberate_refusal_turns_it_off() {
        for refusal in ["0", "false", "no", ""] {
            with_value(Some(refusal), || {
                assert!(!Server::enabled(), "{refusal:?} should decline the server");
            });
        }
    }

    #[test]
    fn a_typo_leaves_the_feature_working_rather_than_silently_inert() {
        // The failure mode of a misspelling must be the desktop working,
        // not tooling that mysteriously falls back to its
        // no-compositor branch.
        for odd in ["maybe", "1", "true", "yes", "off "] {
            with_value(Some(odd), || {
                assert!(Server::enabled(), "{odd:?} should still answer");
            });
        }
    }

    #[test]
    fn descriptor_collection_reuses_caller_owned_capacity() {
        let server = Server {
            directory: PathBuf::new(),
            requests: None,
            events: None,
            request_clients: Vec::new(),
            event_clients: Vec::new(),
            differ: Differ::new(),
            refusals: 0,
            capacity_refusals: 0,
            request_cap_refusing: false,
            event_cap_refusing: false,
            request_read_cursor: 0,
        };
        let mut fds = Vec::with_capacity(8);
        let allocation = fds.as_ptr();
        server.extend_poll_fds(&mut fds);
        assert!(fds.is_empty());
        assert_eq!(fds.as_ptr(), allocation, "an unchanged source set must not replace caller storage");
    }

    #[test]
    fn accepting_past_the_named_caps_never_retains_the_excess() {
        let scratch = Scratch::new();
        let request_path = scratch.socket("requests.sock");
        let event_path = scratch.socket("events.sock");
        let mut server = Server {
            directory: PathBuf::new(),
            requests: Some(StreamListener::bind(&request_path).unwrap()),
            events: Some(StreamListener::bind(&event_path).unwrap()),
            request_clients: Vec::new(),
            event_clients: Vec::new(),
            differ: Differ::new(),
            refusals: 0,
            capacity_refusals: 0,
            request_cap_refusing: false,
            event_cap_refusing: false,
            request_read_cursor: 0,
        };
        let mut request_peers = Vec::new();
        let mut event_peers = Vec::new();

        // Accept after every connect so the kernel's small pending
        // backlog cannot become the quantity this test accidentally
        // measures. The server itself is the only intended ceiling.
        for _ in 0..MAX_REQUEST_CLIENTS + 8 {
            request_peers.push(Stream::connect(&request_path).unwrap());
            server.accept();
        }
        for _ in 0..MAX_EVENT_CLIENTS + 8 {
            event_peers.push(Stream::connect(&event_path).unwrap());
            server.accept();
        }

        assert_eq!(server.request_clients.len(), MAX_REQUEST_CLIENTS);
        assert_eq!(server.event_clients.len(), MAX_EVENT_CLIENTS);
        assert_eq!(server.poll_fds().len(), 2 + MAX_REQUEST_CLIENTS + MAX_EVENT_CLIENTS);
        assert_eq!(server.capacity_refusals, 16);
        assert!(server.request_cap_refusing && server.event_cap_refusing);
    }

    #[test]
    fn request_clients_share_one_read_budget_and_the_remainder_gets_the_next_pass() {
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;

        let payload_len = request::MAX_REQUEST * 3 / 4;
        let mut payload = b"/dispatch workspace 2".to_vec();
        payload.resize(payload_len, b' ');
        let mut requesters = Vec::new();
        let mut clients = Vec::new();
        for _ in 0..3 {
            let (mut requester, accepted) = UnixStream::pair().unwrap();
            requester.write_all(&payload).unwrap();
            requesters.push(requester);
            clients.push(RequestClient {
                stream: Stream::from_fd(accepted.into()),
                inbound: Vec::new(),
                outbound: Vec::new(),
                answered: false,
                doomed: false,
                idle_passes: 0,
            });
        }
        let mut server = Server {
            directory: PathBuf::new(),
            requests: None,
            events: None,
            request_clients: clients,
            event_clients: Vec::new(),
            differ: Differ::new(),
            refusals: 0,
            capacity_refusals: 0,
            request_cap_refusing: false,
            event_cap_refusing: false,
            request_read_cursor: 0,
        };
        let mut applied = 0;

        let _ = server.service(&Snapshot::default(), |_| {
            applied += 1;
            true
        });
        assert_eq!(applied, 2, "three 48 KiB requests must not all fit in one 128 KiB pass");
        assert_eq!(server.request_clients.len(), 1, "the client behind the spent budget stays queued");

        let _ = server.service(&Snapshot::default(), |_| {
            applied += 1;
            true
        });
        assert_eq!(applied, 3, "the queued client receives the next pass rather than starving");
        assert!(server.request_clients.is_empty());
    }

    #[test]
    fn a_silent_one_shot_request_client_is_reaped_at_the_idle_bound() {
        use std::os::unix::net::UnixStream;

        let (_silent_peer, accepted) = UnixStream::pair().unwrap();
        let mut server = Server {
            directory: PathBuf::new(),
            requests: None,
            events: None,
            request_clients: vec![RequestClient {
                stream: Stream::from_fd(accepted.into()),
                inbound: Vec::new(),
                outbound: Vec::new(),
                answered: false,
                doomed: false,
                idle_passes: 0,
            }],
            event_clients: Vec::new(),
            differ: Differ::new(),
            refusals: 0,
            capacity_refusals: 0,
            request_cap_refusing: false,
            event_cap_refusing: false,
            request_read_cursor: 0,
        };

        for _ in 1..REQUEST_IDLE_PASSES {
            server.maintain();
            assert_eq!(server.request_clients.len(), 1, "the documented idle grace must be real");
        }
        server.maintain();
        assert!(server.request_clients.is_empty());
    }

    #[test]
    fn an_empty_probe_connection_is_dropped_on_its_first_eof() {
        use std::os::unix::net::UnixStream;

        let (probe, accepted) = UnixStream::pair().expect("socket pair");
        let stream = Stream::from_fd(accepted.into());
        let mut server = Server {
            directory: PathBuf::new(),
            requests: None,
            events: None,
            request_clients: vec![RequestClient {
                stream,
                inbound: Vec::new(),
                outbound: Vec::new(),
                answered: false,
                doomed: false,
                idle_passes: 0,
            }],
            event_clients: Vec::new(),
            differ: Differ::new(),
            refusals: 0,
            capacity_refusals: 0,
            request_cap_refusing: false,
            event_cap_refusing: false,
            request_read_cursor: 0,
        };

        // The stale-instance health check only connects. Dropping it
        // without a request must make the accepted fd disappear in one
        // service pass, or its permanent HUP keeps waking the desktop.
        drop(probe);
        let _ = server.service(&Snapshot::default(), |_| false);
        assert!(server.request_clients.is_empty());
        assert!(server.poll_fds().is_empty(), "no dead fd may survive into the next event-loop wait");
    }

    #[test]
    fn service_reports_only_actions_the_compositor_applied() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut requester, accepted) = UnixStream::pair().expect("socket pair");
        requester.write_all(b"/dispatch workspace 2").expect("request is written");
        let stream = Stream::from_fd(accepted.into());
        let mut server = Server {
            directory: PathBuf::new(),
            requests: None,
            events: None,
            request_clients: vec![RequestClient {
                stream,
                inbound: Vec::new(),
                outbound: Vec::new(),
                answered: false,
                doomed: false,
                idle_passes: 0,
            }],
            event_clients: Vec::new(),
            differ: Differ::new(),
            refusals: 0,
            capacity_refusals: 0,
            request_cap_refusing: false,
            event_cap_refusing: false,
            request_read_cursor: 0,
        };

        let applied = server.service(&Snapshot::default(), |action| {
            assert!(matches!(action, Action::FocusWorkspace(1)));
            true
        });

        assert!(applied, "the caller needs to know that the requested action was accepted");
    }

    #[test]
    fn a_quiet_event_client_is_dropped_when_its_peer_closes() {
        use std::os::unix::net::UnixStream;

        let (subscriber, accepted) = UnixStream::pair().expect("socket pair");
        let stream = Stream::from_fd(accepted.into());
        let mut server = Server {
            directory: PathBuf::new(),
            requests: None,
            events: None,
            request_clients: Vec::new(),
            event_clients: vec![EventClient { stream, outbound: Vec::new(), doomed: false }],
            differ: Differ::new(),
            refusals: 0,
            capacity_refusals: 0,
            request_cap_refusing: false,
            event_cap_refusing: false,
            request_read_cursor: 0,
        };

        // Establish the differ's baseline while the subscriber is
        // alive, then close it while no state is changing. Snapshot-
        // free maintenance has no event bytes whose failed send could
        // reveal the death; its explicit HUP check must still prune the
        // fd.
        let snapshot = Snapshot::default();
        server.publish(&snapshot);
        drop(subscriber);
        server.maintain();
        assert!(server.event_clients.is_empty());
        assert!(server.poll_fds().is_empty(), "no dead event fd may survive into the next wait");
    }
}
