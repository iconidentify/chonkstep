//! The control socket (`docs/control-socket.md`), end to end: a client
//! that knows nothing but the document connects to a live nested
//! session and sees what the document promises. Same running story as
//! `e2e.rs` — needs a Wayland session to nest inside, so `#[ignore]`d;
//! run with `cargo test -p chonk-testkit --test control_socket -- --ignored`.
//!
//! The unit tests in `chonk-shell/src/control.rs` prove the socket's
//! mechanics against a fake window manager. What only a live session
//! can prove is the plumbing around it: that the path the document
//! names is the path the shell binds, that a real window's arrival is
//! narrated, that a request reaches the real window manager and comes
//! back as the event the document says, and that a process the shell
//! launched can find the socket in its environment. That is what is
//! checked here, and nothing the unit tests already cover.
//!
//! The client is deliberately the dumbest thing that works — a
//! blocking `UnixStream` and a `BufReader` — because that is what a
//! third-party author will write first, and the socket must be right
//! for it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chonk_testkit::{keys, poll_until, session_dir, Session, SessionOptions};

/// Long enough for a window to map and the shell's next tick to notice.
const EVENT: Duration = Duration::from_secs(10);
/// Long enough for a spawn, an exec and a small write to land.
const SPAWNED: Duration = Duration::from_secs(8);

/// A bar, as the document describes one: connected, reading lines.
struct Bar {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Bar {
    fn connect(path: &Path) -> Bar {
        let stream = poll_until(EVENT, &format!("the control socket at {}", path.display()), || UnixStream::connect(path).ok())
            .expect("the shell binds the control socket during startup");
        stream.set_read_timeout(Some(EVENT)).expect("read timeout");
        let writer = stream.try_clone().expect("clone");
        Bar { reader: BufReader::new(stream), writer }
    }

    fn next(&mut self) -> serde_json::Value {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).expect("a line from the shell");
        assert!(n > 0, "the shell closed the connection");
        assert!(line.ends_with('\n'), "every line ends in a newline");
        serde_json::from_str(line.trim_end_matches('\n')).unwrap_or_else(|e| panic!("not JSON: {line:?}: {e}"))
    }

    /// Reads events until one satisfies `accept`, or panics after
    /// `EVENT`. Events the predicate rejects are simply dropped: the
    /// document makes no coalescing promise, so a title flicker may
    /// produce `focus` lines the test did not ask about.
    fn wait_for(&mut self, what: &str, mut accept: impl FnMut(&serde_json::Value) -> bool) -> serde_json::Value {
        let deadline = Instant::now() + EVENT;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let event = self.next();
            if accept(&event) {
                return event;
            }
            seen.push(event);
        }
        panic!("never saw {what}; events seen: {seen:#?}");
    }

    fn send(&mut self, line: &str) {
        self.writer.write_all(line.as_bytes()).expect("write to the shell");
        self.writer.write_all(b"\n").expect("write to the shell");
    }
}

fn control_socket_path(session: &Session) -> PathBuf {
    // §1.1: `$XDG_RUNTIME_DIR/chonkstep/control-<display>.sock`, with
    // the display sanitised — a nested session's `wayland-N` is already
    // clean, so the derivation a client does is a plain join.
    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR is set in any session this can run in");
    PathBuf::from(runtime).join("chonkstep").join(format!("control-{}.sock", session.wayland_display))
}

/// The whole document, walked once in the order a bar would walk it.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_bar_sees_the_snapshot_the_windows_and_its_own_switches() {
    let dir = session_dir("control-socket");
    let dump = dir.join("env");
    // `super+space` carries the focused window to the next workspace —
    // the one way a *client* can make a second workspace exist, since
    // §4.2 forbids the socket from doing it. `volumeup` dumps the
    // environment a launched process sees.
    let config = format!(
        "[commands]\ndump = [\"sh\", \"-c\", \"env > {}\"]\n\n[keybindings]\n\"super+space\" = \"workspace-carry-next\"\n\"volumeup\" = \"run dump\"\n",
        dump.display()
    );
    let mut session = Session::boot("control-socket", SessionOptions { config_extra: config, ..Default::default() }).expect("session boots");

    let path = control_socket_path(&session);
    let mut bar = Bar::connect(&path);

    // §1.2 / §3: hello, then every facet in order, before anything else.
    let hello = bar.next();
    assert_eq!(hello["event"], "hello");
    assert_eq!(hello["protocol"], 1);
    assert_eq!(hello["session"], "wayland");
    assert!(hello["pid"].as_u64().unwrap() > 0);
    let workspaces = bar.next();
    assert_eq!(workspaces["event"], "workspaces");
    assert_eq!(workspaces["active"], 0);
    assert_eq!(workspaces["workspaces"][0]["index"], 0, "indices are 0-based");
    let outputs = bar.next();
    assert_eq!(outputs["event"], "outputs");
    assert!(!outputs["outputs"].as_array().unwrap().is_empty(), "a nested session has one output");
    let output = &outputs["outputs"][0];
    assert!(output["width"].as_u64().unwrap() > 0 && output["height"].as_u64().unwrap() > 0);
    let focus = bar.next();
    assert_eq!(focus["event"], "focus");
    assert_eq!(focus["window"], serde_json::Value::Null, "nothing is focused on an empty desktop");
    let theme = bar.next();
    assert_eq!(theme["event"], "theme");
    assert!(theme["id"].is_string() && theme["name"].is_string());
    assert!(matches!(theme["appearance"].as_str(), Some("dark" | "light")));
    assert!(theme.get("following").is_some(), "`following` is present even when null");

    // A window arrives: the workspace it landed on counts it, and focus
    // names it.
    let count_before = workspaces["workspaces"][0]["windows"].as_u64().unwrap();
    session.launch("foot", &[]).expect("foot launches");
    session.wait_for_window("foot").expect("foot maps");
    let after = bar.wait_for("a workspaces event counting the new window", |e| {
        e["event"] == "workspaces" && e["workspaces"][0]["windows"].as_u64() == Some(count_before + 1)
    });
    assert_eq!(after["active"], 0);
    let focused = bar.wait_for("a focus event naming foot", |e| e["event"] == "focus" && e["window"]["app_id"] == "foot");
    assert_eq!(focused["count"], count_before + 1);
    assert_eq!(focused["window"]["workspace"], 0);
    assert!(focused["window"]["id"].as_u64().unwrap() > 0);

    // §4.2, the error half: an index past the end is refused, and the
    // connection stays open.
    bar.send(r#"{"request":"focus-workspace","index":7}"#);
    let error = bar.wait_for("an error for workspace 7", |e| e["event"] == "error");
    assert_eq!(error["request"], "focus-workspace");
    assert!(error["message"].as_str().unwrap().contains('7'), "{}", error["message"]);

    // The keyboard grows a second workspace by carrying the window
    // there; the socket narrates it.
    session.door().chord(keys::LEFTMETA, keys::SPACE).expect("chord injects");
    let carried = bar.wait_for("a workspaces event with two workspaces and the second active", |e| {
        e["event"] == "workspaces" && e["workspaces"].as_array().map(Vec::len) == Some(2) && e["active"] == 1
    });
    assert_eq!(carried["workspaces"][1]["windows"], 1);
    assert_eq!(carried["workspaces"][0]["windows"], 0);

    // §4.2, the switch half: back to the first, then to the second
    // through the socket alone.
    bar.send(r#"{"request":"focus-workspace","index":0}"#);
    bar.wait_for("workspaces.active == 0", |e| e["event"] == "workspaces" && e["active"] == 0);
    bar.send(r#"{"request":"focus-workspace","index":1}"#);
    let switched = bar.wait_for("workspaces.active == 1", |e| e["event"] == "workspaces" && e["active"] == 1);
    assert_eq!(switched["workspaces"].as_array().unwrap().len(), 2, "a switch never creates a workspace");

    // §4.1: the snapshot, on demand, in order.
    bar.send(r#"{"request":"snapshot"}"#);
    let mut order = Vec::new();
    let first = bar.wait_for("the workspaces line that opens a snapshot", |e| e["event"] == "workspaces");
    order.push(first["event"].as_str().unwrap().to_string());
    for _ in 0..3 {
        order.push(bar.next()["event"].as_str().unwrap().to_string());
    }
    assert_eq!(order, ["workspaces", "outputs", "focus", "theme"]);

    // §2: an unknown verb is an error, not a disconnect.
    bar.send(r#"{"request":"make-coffee"}"#);
    let unknown = bar.wait_for("an error for the unknown verb", |e| e["event"] == "error");
    assert_eq!(unknown["request"], "make-coffee");

    // §1.1: a process the shell launches finds the socket in its
    // environment, under the name the document gives, at the path the
    // bar itself connected to.
    session.door().tap_key(keys::VOLUMEUP).expect("key injects");
    // The shell creates the redirection target before `env` writes its first
    // line. Waiting only for the path made this assertion race an empty file
    // on fast runs, so wait for the fact under test to become observable.
    poll_until(SPAWNED, "the control socket in the environment dump", || {
        std::fs::read_to_string(&dump)
            .ok()
            .filter(|contents| contents.lines().any(|line| line.starts_with("CHONKSTEP_CONTROL_SOCKET=")))
            .map(|_| ())
    })
    .expect("the dump command should have exported the control socket");
    let env = std::fs::read_to_string(&dump).expect("dump readable");
    let exported = env.lines().find_map(|l| l.strip_prefix("CHONKSTEP_CONTROL_SOCKET="));
    assert_eq!(exported, Some(path.to_str().unwrap()), "launched processes inherit the control socket path; env was:\n{env}");

    assert!(session.compositor_alive(), "nothing above may have cost the session");
}

/// A stopped shell is EOF on the client's side — the first half of the
/// reconnect story §1.2 tells a client to rely on. (The second half,
/// that the next shell on the same display binds the same path, is
/// not exercised here: nothing boots a successor.)
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_stopped_shell_reaches_its_client_as_eof() {
    let mut session = Session::boot("control-socket-exit", SessionOptions::default()).expect("session boots");
    let path = control_socket_path(&session);
    let mut bar = Bar::connect(&path);
    assert_eq!(bar.next()["event"], "hello");

    session.kill_compositor();
    let mut line = String::new();
    let deadline = Instant::now() + EVENT;
    loop {
        line.clear();
        match bar.reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        assert!(Instant::now() < deadline, "the shell's exit must reach the client as EOF");
    }
}
