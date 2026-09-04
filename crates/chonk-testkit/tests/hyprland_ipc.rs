//! Hyprland's IPC, end to end against a live nested session.
//!
//! The unit tests in `chonk-hyprland-ipc` prove every JSON shape and
//! every parser against a hand-built `Snapshot`. What only a live
//! session can prove is the plumbing between that crate and the real
//! window manager: that the sockets appear where the protocol says, at
//! the modes the security posture claims; that a real window's arrival
//! reaches the event stream with the address `j/clients` gives it; that
//! a dispatch reaches `wm-core` and *actually changes the desktop*
//! rather than merely answering `ok`; and that a verb chonkstep cannot
//! honour is refused instead of accepted.
//!
//! That last pair is why this file exists at all. The unit tests cannot
//! catch the failure mode this whole feature is built around — a verb
//! that answers `ok` and does nothing — because the answer and the
//! effect live on opposite sides of the crate boundary. An end-to-end
//! run against the real `hyprctl` found exactly that bug in
//! `dispatch workspace N`, which the unit tests were green through.
//! `switches_the_workspace_for_real` is its regression test.
//!
//! The client here is deliberately the dumbest thing that works — a
//! blocking `UnixStream`, one connection per request — because that is
//! precisely what `hyprctl` is, and the socket has to be right for it.
//!
//! Same run rules as `e2e.rs`: needs a Wayland session to nest inside,
//! so `#[ignore]`d; run with
//! `cargo test -p chonk-testkit --test hyprland_ipc -- --ignored`.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(10);

/// The directory the compositor says it bound, read out of its log the
/// same way `scripts/wayland-session.sh` reads it — so this test also
/// covers the log line that session script depends on.
fn socket_dir(session: &Session) -> PathBuf {
    let signature = poll_until(EVENT, "the hyprland ipc log line", || {
        let log = session.log();
        let at = log.find("hyprland ipc listening")?;
        // `tracing` colours the field *keys* with ANSI escapes even into
        // a file, so match the quoted value rather than the key — the
        // same reason the session script matches on shape.
        let rest = &log[at..];
        let start = rest.find('"')? + 1;
        let end = rest[start..].find('"')? + start;
        Some(rest[start..end].to_string())
    })
    .expect("the compositor logs the signature it bound");

    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    PathBuf::from(runtime).join("hypr").join(signature)
}

/// One request: connect, write, read to EOF. Exactly `hyprctl`'s shape.
fn request(dir: &Path, payload: &str) -> String {
    let mut stream = UnixStream::connect(dir.join(".socket.sock"))
        .unwrap_or_else(|e| panic!("connecting to the request socket: {e}"));
    stream.set_read_timeout(Some(EVENT)).expect("read timeout");
    stream.write_all(payload.as_bytes()).expect("write the request");
    stream.flush().expect("flush");
    // `hyprctl` does not shut down its write side; the server must
    // answer on the bytes alone and close, or a real client hangs.
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read the response");
    response
}

fn json(dir: &Path, payload: &str) -> serde_json::Value {
    let raw = request(dir, payload);
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{payload} was not JSON: {raw:?}: {e}"))
}

/// A bar, tailing the event socket.
struct Events(BufReader<UnixStream>);

impl Events {
    fn connect(dir: &Path) -> Events {
        let stream = UnixStream::connect(dir.join(".socket2.sock")).expect("event socket");
        stream.set_read_timeout(Some(EVENT)).expect("read timeout");
        Events(BufReader::new(stream))
    }

    /// Read until a line starts with `name>>`, or panic.
    fn wait_for(&mut self, name: &str) -> String {
        let deadline = Instant::now() + EVENT;
        let prefix = format!("{name}>>");
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let mut line = String::new();
            if self.0.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            assert!(line.ends_with('\n'), "every event line ends in a newline: {line:?}");
            let line = line.trim_end().to_string();
            if let Some(data) = line.strip_prefix(&prefix) {
                return data.to_string();
            }
            seen.push(line);
        }
        panic!("never saw {name}; events seen: {seen:#?}");
    }
}

fn boot(name: &str) -> Session {
    let mut options = SessionOptions::default();
    options.env.push(("CHONKSTEP_HYPRLAND_IPC".to_string(), "1".to_string()));
    Session::boot(name, options).expect("nested session")
}

/// The sockets exist where the protocol says, and are private.
///
/// The modes are asserted rather than assumed because this socket
/// accepts commands: `docs/hyprland-ipc.md` §4 argues it needs no
/// authentication *on the grounds that* only its own user can reach it,
/// and that argument is only as good as these two numbers.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn binds_two_private_sockets_where_the_protocol_says() {
    use std::os::unix::fs::PermissionsExt;

    let session = boot("hypr-ipc-bind");
    let dir = socket_dir(&session);

    let mode = |p: PathBuf| {
        std::fs::metadata(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())).permissions().mode() & 0o777
    };

    assert_eq!(mode(dir.clone()), 0o700, "the instance directory must be private");
    assert_eq!(mode(dir.join(".socket.sock")), 0o600, "the request socket must be private");
    assert_eq!(mode(dir.join(".socket2.sock")), 0o600, "the event socket must be private");
}

/// The four requests Quickshell makes on connect, in the shapes it
/// parses. `j/status` is first because Quickshell will not even connect
/// to the event socket until it is answered.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn answers_the_queries_quickshell_makes_on_connect() {
    let session = boot("hypr-ipc-queries");
    let dir = socket_dir(&session);

    let status = json(&dir, "j/status");
    assert!(status.get("configProvider").is_some(), "Quickshell reads configProvider");

    let monitors = json(&dir, "j/monitors");
    let monitor = &monitors.as_array().expect("monitors is an array")[0];
    for key in ["id", "name", "description", "x", "y", "width", "height", "scale", "focused"] {
        assert!(!monitor[key].is_null(), "monitors[0].{key}");
    }
    assert!(monitor["activeWorkspace"]["id"].is_number(), "the nested workspace ref");

    let workspaces = json(&dir, "j/workspaces");
    let workspace = &workspaces.as_array().expect("workspaces is an array")[0];
    // Hyprland numbers from 1; chonkstep from 0. A workspace served as
    // 0 is one no bar button can match.
    assert_eq!(workspace["id"], serde_json::json!(1));
    assert_eq!(workspace["name"], serde_json::json!("1"));

    assert!(json(&dir, "j/clients").is_array(), "clients is an array");

    // Nothing is focused in a session with no windows, and Hyprland's
    // shape for that is an empty object — not null, not [].
    assert_eq!(json(&dir, "j/activewindow"), serde_json::json!({}));

    let devices = json(&dir, "j/devices");
    let keyboard = &devices["keyboards"][0];
    for key in ["name", "active_keymap", "layout", "active_layout_index"] {
        assert!(!keyboard[key].is_null(), "devices.keyboards[0].{key}");
    }
    assert!(devices["mice"].as_array().is_some_and(|mice| !mice.is_empty()));
    assert!(!request(&dir, "binds").trim().is_empty(), "the keybinding menu receives the live keymap");
}

/// A real window reaches both halves of the protocol, with one address.
///
/// Quickshell correlates event payloads with `j/clients` entries by
/// parsing both as base-16 numbers, so the two forms must agree in
/// value even though they differ in spelling (`0x…` in JSON, bare hex
/// in events).
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn a_real_window_appears_in_the_stream_and_the_client_list() {
    let mut session = boot("hypr-ipc-window");
    let dir = socket_dir(&session);
    let mut events = Events::connect(&dir);

    // `zenity --question` is the harness's long-lived window: it sits
    // on a dialog until dismissed, where a terminal with no command
    // exits the moment its shell finds no tty.
    session.launch("zenity", &["--question", "--title", "ipc-probe", "--text", "hold still"]).expect("launch a window");
    session.wait_for_window("ipc-probe").expect("the window maps");

    let data = events.wait_for("openwindow");
    let fields: Vec<&str> = data.splitn(4, ',').collect();
    assert_eq!(fields.len(), 4, "openwindow carries address,workspace,class,title: {data:?}");
    let from_event = u64::from_str_radix(fields[0], 16).unwrap_or_else(|e| panic!("event address {data:?}: {e}"));
    assert!(fields[3].contains("ipc-probe"), "the title travels with the event: {data:?}");

    let clients = json(&dir, "j/clients");
    let entry = clients
        .as_array()
        .expect("array")
        .iter()
        .find(|c| c["title"].as_str().is_some_and(|t| t.contains("ipc-probe")))
        .unwrap_or_else(|| panic!("the window is missing from j/clients: {clients}"));

    let address = entry["address"].as_str().expect("address is a string");
    assert!(address.starts_with("0x"), "j/clients spells addresses with 0x: {address}");
    let from_json = u64::from_str_radix(address.trim_start_matches("0x"), 16).expect("hex");
    assert_eq!(from_json, from_event, "the two spellings must name the same window");

    // The geometry keys `omarchy-capture-region` indexes as arrays.
    assert_eq!(entry["at"].as_array().map(Vec::len), Some(2), "at is [x, y]");
    assert_eq!(entry["size"].as_array().map(Vec::len), Some(2), "size is [w, h]");
}

#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn foreign_toplevel_mapping_returns_the_same_live_ipc_address() {
    let mut session = boot("hypr-toplevel-mapping");
    let dir = socket_dir(&session);
    let probe = profile_binary("chonk-toplevel-mapping-probe").expect("mapping probe built");
    session.launch(probe.to_str().unwrap(), &[]).expect("mapping probe launches");
    poll_until(EVENT, "the mapping probe to bind both globals", || {
        session.client_log("chonk-toplevel-mapping-probe").contains("**mapping ready**").then_some(())
    })
    .expect("mapping globals are available");

    session.launch("zenity", &["--question", "--title", "mapping-probe", "--text", "hold still"]).unwrap();
    session.wait_for_window("mapping-probe").expect("window maps");
    let mapped = poll_until(EVENT, "the protocol to return a window address", || {
        let report = session.client_log("chonk-toplevel-mapping-probe");
        let address = report.split("**mapped address ").nth(1)?.split("**").next()?;
        Some(address.to_string())
    })
    .expect("mapping returns an address");

    let clients = json(&dir, "j/clients");
    let ipc = clients
        .as_array().unwrap()
        .iter()
        .find(|client| client["title"].as_str().is_some_and(|title| title.contains("mapping-probe")))
        .and_then(|client| client["address"].as_str())
        .expect("the same window appears in IPC");
    assert_eq!(mapped, ipc, "the protocol is a join, so both surfaces must return one identity");

    session.kill_client("zenity");
    session.wait_for_window_gone("mapping-probe").expect("window unmaps cleanly");
    assert!(session.compositor_alive(), "stale foreign handles are cleaned rather than dereferenced");
}

#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn window_geometry_plain_fields_and_monitor_eval_are_applied_before_ok() {
    let mut session = boot("hypr-ipc-window-actions");
    let dir = socket_dir(&session);
    session.launch("zenity", &["--question", "--title", "geometry-probe", "--text", "hold still"]).unwrap();
    session.wait_for_window("geometry-probe").expect("window maps");

    let client = || {
        json(&dir, "j/clients")
            .as_array().unwrap()
            .iter()
            .find(|client| client["title"].as_str().is_some_and(|title| title.contains("geometry-probe")))
            .cloned()
            .expect("client stays mapped")
    };
    let before = client();
    let address = before["address"].as_str().unwrap().to_string();
    let original_size = before["size"].clone();

    assert_eq!(
        request(&dir, &format!("/dispatch resizewindowpixel exact 500 320,address:{address}")).trim(),
        "ok"
    );
    poll_until(EVENT, "the exact resize to reach the live client", || {
        (client()["size"] == serde_json::json!([500, 320])).then_some(())
    })
    .expect("resize changes geometry rather than only replying");

    assert_eq!(
        request(
            &dir,
            &format!(
                "eval hl.dispatch(hl.dsp.window.resize({{ window = \"address:{address}\", x = 25, y = 10, relative = true }}))"
            ),
        )
        .trim(),
        "ok"
    );
    poll_until(EVENT, "the Lua relative resize to apply", || {
        (client()["size"] == serde_json::json!([525, 330])).then_some(())
    })
    .expect("Lua window dispatch reaches the same geometry path");

    let original_w = original_size[0].as_i64().unwrap();
    let original_h = original_size[1].as_i64().unwrap();
    assert_eq!(
        request(
            &dir,
            &format!("/dispatch resizewindowpixel exact {original_w} {original_h},address:{address}"),
        )
        .trim(),
        "ok"
    );
    poll_until(EVENT, "saved dimensions to round-trip", || (client()["size"] == original_size).then_some(()))
        .expect("a saved width/height can be restored exactly");

    let plain = request(&dir, "activewindow");
    for field in [
        "\tpid:", "\tclass:", "\ttitle:", "\tat:", "\tsize:", "\tworkspace:", "\tfloating:",
        "\tmonitor:", "\txwayland:", "\tpinned:", "\tfullscreen:",
    ] {
        assert!(plain.contains(field), "plain activewindow omitted {field:?}: {plain}");
    }
    let pid = client()["pid"].as_u64().expect("live client pid");
    assert!(pid > 0, "the pid must come from client credentials, not a zero placeholder");
    assert!(plain.contains(&format!("\tpid: {pid}")), "plain and JSON views must report one pid: {plain}");
    assert_eq!(
        std::fs::read_link(format!("/proc/{pid}/cwd")).expect("the focused client's cwd remains inspectable"),
        std::env::current_dir().unwrap(),
        "omarchy-cmd-terminal-cwd reads this pid and must reach the real client cwd"
    );

    assert_eq!(request(&dir, "eval hl.monitor({ output = \"chonkstep\", scale = 1.5 })").trim(), "ok");
    poll_until(EVENT, "monitor scale eval to update live output state", || {
        ((json(&dir, "j/monitors")[0]["scale"].as_f64()? - 1.5).abs() < f64::EPSILON).then_some(())
    })
    .expect("monitor mutation must happen before success is observable");
}

#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn exec_and_reload_have_observable_effects_before_success() {
    let session = boot("hypr-ipc-exec-reload");
    let dir = socket_dir(&session);

    let direct = session.dir.join("direct-argv-executed");
    assert_eq!(
        request(&dir, &format!("/dispatch exec -- /usr/bin/touch {}", direct.display())).trim(),
        "ok"
    );
    poll_until(EVENT, "direct exec argv to create its marker", || direct.exists().then_some(()))
        .expect("exec -- must preserve argv rather than reinterpret it as shell source");

    let lua = session.dir.join("lua-long-string-executed");
    assert_eq!(
        request(
            &dir,
            &format!("/dispatch hl.dsp.exec_cmd([=[/usr/bin/touch {}]=])", lua.display()),
        )
        .trim(),
        "ok"
    );
    poll_until(EVENT, "Lua long-string exec to create its marker", || lua.exists().then_some(()))
        .expect("Lua long brackets and quoted strings must reach the same shell-command path");

    let mut events = Events::connect(&dir);
    assert_eq!(request(&dir, "/reload").trim(), "ok");
    assert_eq!(events.wait_for("configreloaded"), "", "a live re-read must tell Quickshell to refresh");
}

/// The regression test for the bug an end-to-end run found and the unit
/// tests could not: `dispatch workspace N` answered `ok` and left the
/// desktop where it was, because the answer was written on one side of
/// the crate boundary and the effect attempted on the other.
///
/// Asserting the response alone would still pass against that bug. The
/// assertion that matters is the one after it.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn switches_the_workspace_for_real() {
    let session = boot("hypr-ipc-dispatch");
    let dir = socket_dir(&session);
    let mut events = Events::connect(&dir);

    assert_eq!(json(&dir, "j/activeworkspace")["id"], serde_json::json!(1));

    assert_eq!(request(&dir, "/dispatch workspace 3").trim(), "ok");

    // The desktop actually moved — this is the assertion the bug failed.
    poll_until(EVENT, "the compositor to be on workspace 3", || {
        (json(&dir, "j/activeworkspace")["id"] == serde_json::json!(3)).then_some(())
    })
    .expect("dispatch workspace 3 must switch, not merely answer ok");

    // And it was narrated, in Hyprland's spelling and numbering.
    assert_eq!(events.wait_for("workspacev2"), "3,3");

    // The Lua dialect Omarchy 4 sends first reaches the same place.
    assert_eq!(request(&dir, r#"dispatch hl.dsp.focus({ workspace = "1" })"#).trim(), "ok");
    poll_until(EVENT, "the compositor to be back on workspace 1", || {
        (json(&dir, "j/activeworkspace")["id"] == serde_json::json!(1)).then_some(())
    })
    .expect("the Lua dispatch dialect must work too");
}

/// The load-bearing rule, live: a verb chonkstep cannot honour fails,
/// and is seen to change nothing.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn tiling_verbs_fail_honestly_against_a_live_desktop() {
    let session = boot("hypr-ipc-refusal");
    let dir = socket_dir(&session);

    let before = json(&dir, "j/activeworkspace");

    for verb in ["togglesplit", "layoutmsg orientationtop", "pseudo", "togglegroup"] {
        let response = request(&dir, &format!("/dispatch {verb}"));
        assert_ne!(response.trim(), "ok", "{verb} must not claim success");
        assert!(response.starts_with("Invalid dispatcher"), "{verb} must fail the way Hyprland does, got {response:?}");
    }

    assert_eq!(json(&dir, "j/activeworkspace"), before, "a refused verb changes nothing");

    // An unknown verb is answered, not dropped — a client that gets no
    // reply blocks in read() forever.
    assert!(request(&dir, "/nonsense").starts_with("unknown request"));
}
