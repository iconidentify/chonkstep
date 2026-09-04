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

use chonk_hyprland_ipc::{MAX_EVENT_CLIENTS, MAX_REQUEST_CLIENTS};
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

fn control_socket(session: &Session) -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    PathBuf::from(runtime).join("chonkstep").join(format!("control-{}.sock", session.wayland_display))
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

/// A socket-existence probe connects and closes without writing. The
/// server's own stale-instance sweep does exactly that, so this is a
/// normal lifecycle event rather than a hostile-client curiosity.
/// Retaining the accepted EOF fd leaves HUP permanently readable: one
/// live session accumulated 23 of them, spun 1,560 passes/second, and
/// consumed 99% of one CPU core.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn disconnected_socket_probes_leave_no_fds_and_no_busy_loop_behind() {
    let mut session = boot("hypr-ipc-probe-eof");
    let dir = socket_dir(&session);
    let compositor_pid = session.compositor_pid();
    let fd_count = || {
        std::fs::read_dir(format!("/proc/{compositor_pid}/fd"))
            .expect("the harness can inspect its child process")
            .count()
    };

    // Settle startup/XWayland first. A tolerance of two covers a
    // transient render fence without being large enough to hide even
    // one full batch of leaked request/event connections.
    session.door().barrier().unwrap();
    let baseline = fd_count();
    for _ in 0..8 {
        for socket in [".socket.sock", ".socket2.sock"] {
            for _ in 0..4 {
                drop(UnixStream::connect(dir.join(socket)).expect("probe connects"));
            }
        }
        session.door().barrier().unwrap();
    }

    poll_until(EVENT, "disconnected probe fds to be pruned", || {
        let current = fd_count();
        (current <= baseline + 2).then_some(current)
    })
    .unwrap_or_else(|error| panic!("{error}; baseline={baseline}, now={}", fd_count()));

    assert!(json(&dir, "j/version").is_object(), "the request socket still answers after the probe storm");
}

/// Excess connections are accepted and closed, never retained as
/// compositor descriptors/calloop sources. The two populations are
/// capped independently so a real query can recover as soon as the
/// silent request peers leave even while a full event audience stays.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn a_connection_storm_is_capped_before_it_can_grow_the_source_set() {
    let mut session = boot("hypr-ipc-connection-cap");
    let dir = socket_dir(&session);
    let compositor_pid = session.compositor_pid();
    let fd_count = || {
        std::fs::read_dir(format!("/proc/{compositor_pid}/fd"))
            .expect("the harness can inspect its child process")
            .count()
    };
    fn connect(session: &mut Session, dir: &Path, socket: &str, count: usize) -> Vec<UnixStream> {
        let mut peers = Vec::new();
        for index in 0..count {
            peers.push(UnixStream::connect(dir.join(socket)).expect("storm connection"));
            // Stay below the transport's pending backlog. The property
            // under test is the accepted population and its one-source-
            // per-fd compositor cost, not connect(2) scheduling.
            if index % 8 == 7 {
                session.door().barrier().unwrap();
            }
        }
        session.door().barrier().unwrap();
        peers
    }

    session.door().barrier().unwrap();
    let baseline = fd_count();
    let mut requests = connect(&mut session, &dir, ".socket.sock", MAX_REQUEST_CLIENTS + 32);
    let mut events = connect(&mut session, &dir, ".socket2.sock", MAX_EVENT_CLIENTS + 32);

    let retained_bound = baseline + MAX_REQUEST_CLIENTS + MAX_EVENT_CLIENTS + 4;
    assert!(
        fd_count() <= retained_bound,
        "excess clients grew the compositor past its two caps: baseline={baseline}, now={}, bound={retained_bound}",
        fd_count()
    );
    for excess in [requests.last_mut().unwrap(), events.last_mut().unwrap()] {
        excess.set_read_timeout(Some(EVENT)).unwrap();
        let mut byte = [0_u8; 1];
        match excess.read(&mut byte) {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            result => panic!("an accepted excess client must be closed immediately, got {result:?}"),
        }
    }
    assert_eq!(
        session.log().matches("hyprland IPC client limit reached").count(),
        2,
        "one warning per continuously-full request/event population"
    );

    drop(requests);
    session.door().barrier().unwrap();
    assert!(json(&dir, "j/version").is_object(), "a slot reopens without disturbing full event subscribers");
    drop(events);
    session.door().barrier().unwrap();
    poll_until(EVENT, "storm descriptors and sources to be pruned", || {
        let current = fd_count();
        (current <= baseline + 2).then_some(current)
    })
    .unwrap_or_else(|error| panic!("{error}; baseline={baseline}, now={}", fd_count()));
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

/// `hyprctl binds` must name keys, because the thing reading it prints
/// the name to a human.
///
/// `omarchy-menu-keybindings` — the script behind `SUPER + K` — parses
/// the PLAIN bind block (deliberately, not `-j`: its own comment says
/// Hyprland 0.56.0 emits invalid JSON for binds), awk-splits out the
/// `key` field and puts it straight into a menu row. So the field is
/// read by a person, and a `key` of `0x1008ff13` is a cheat sheet that
/// has failed at the one thing it exists for.
///
/// The four chords bound here are the four shapes that were broken:
/// the whole `XF86` block, the function keys, the editing keys, and
/// `space` — which is not merely wrong but blank, and which on a real
/// Omarchy desktop is `SUPER + SPACE`, the chord that opens the menu.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn binds_names_every_key_rather_than_reporting_its_keysym_in_hex() {
    let mut options = SessionOptions::default();
    options.env.push(("CHONKSTEP_HYPRLAND_IPC".to_string(), "1".to_string()));
    options.config_extra = "\n[keybindings]\n\
        \"super+space\" = \"overview\"\n\
        \"volumeup\" = \"toggle-dock\"\n\
        \"super+f9\" = \"miniaturize\"\n\
        \"super+shift+backspace\" = \"close\"\n"
        .to_string();
    let session = Session::boot("hypr-ipc-bind-names", options).expect("nested session");
    let dir = socket_dir(&session);

    let plain = request(&dir, "binds");
    let keys: Vec<&str> = plain
        .lines()
        .filter_map(|line| line.trim().strip_prefix("key: "))
        .map(str::trim_end)
        .collect();
    assert!(!keys.is_empty(), "no key fields in the bind block:\n{plain}");

    // Not one row may be a hex number or blank — the two failures.
    for key in &keys {
        assert!(!key.starts_with("0x"), "a bind is reported as hex: {key:?}\n{plain}");
        assert!(!key.is_empty(), "a bind is reported as blank\n{plain}");
    }
    // And the specific keys that used to be, by name.
    for expected in ["space", "F9", "BackSpace", "XF86AudioRaiseVolume"] {
        assert!(keys.contains(&expected), "no bind reported as {expected:?}, saw {keys:?}");
    }

    // The JSON encoding carries the same field and had the same bug.
    let binds = json(&dir, "j/binds");
    for bind in binds.as_array().expect("binds is an array") {
        let key = bind["key"].as_str().expect("every bind has a string key");
        assert!(!key.starts_with("0x") && !key.is_empty(), "j/binds key {key:?}");
    }
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

    // The subscriber must hear later state changes too. This is the
    // invalidation edge the demand-driven publisher relies on: an
    // unchanged client commit sends nothing, while a real WM mutation
    // closes one new atomic foreign-toplevel batch with `done`.
    let done_before = session.client_log("chonk-toplevel-mapping-probe").matches("**foreign done**").count();
    assert_eq!(request(&dir, "dispatch fullscreen 1").trim(), "ok");
    poll_until(EVENT, "the foreign-toplevel handle to publish fullscreen", || {
        (session.client_log("chonk-toplevel-mapping-probe").matches("**foreign done**").count() > done_before)
            .then_some(())
    })
    .expect("the foreign-toplevel stream follows WM state changes");
    let fullscreen = json(&dir, "j/clients")
        .as_array()
        .unwrap()
        .iter()
        .find(|client| client["title"].as_str().is_some_and(|title| title.contains("mapping-probe")))
        .and_then(|client| client["fullscreen"].as_i64());
    assert_eq!(fullscreen, Some(1), "Hyprland IPC and foreign-toplevel observe the same fullscreen transition");

    session.kill_client("zenity");
    session.wait_for_window_gone("mapping-probe").expect("window unmaps cleanly");
    assert!(session.compositor_alive(), "stale foreign handles are cleaned rather than dereferenced");
}

/// Input-shaped activity is not a protocol invalidation. This observes
/// work rather than only wire output: a full reconciliation that allocates
/// and then finds an empty diff would otherwise look just as quiet as the
/// optimized path from outside the compositor.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn focused_click_is_snapshot_free_and_drag_syncs_only_its_toplevel() {
    let mut session = boot("hypr-protocol-invalidation");
    let dir = socket_dir(&session);
    let _events = Events::connect(&dir);

    let observer = profile_binary("chonk-toplevel-mapping-probe").expect("mapping probe built");
    session.launch(observer.to_str().unwrap(), &[]).expect("mapping probe launches");
    poll_until(EVENT, "the foreign-toplevel manager to bind", || {
        session
            .client_log("chonk-toplevel-mapping-probe")
            .contains("**mapping ready**")
            .then_some(())
    })
    .expect("foreign-toplevel manager is available");

    let probe = profile_binary("chonk-fullscreen-probe").expect("window probe built");
    session
        .launch(probe.to_str().unwrap(), &["protocol-invalidation"])
        .expect("window probe launches");
    let window = session
        .wait_for_window("protocol-invalidation")
        .expect("window maps and takes focus");
    poll_until(EVENT, "the foreign-toplevel handle to settle", || {
        session
            .client_log("chonk-toplevel-mapping-probe")
            .contains("**foreign done**")
            .then_some(())
    })
    .expect("foreign-toplevel published the probe");
    session.door().barrier().expect("initial protocol publications settle");

    let before_click = session.door().protocol_publishes().expect("read publisher counters");
    session
        .door()
        .click(
            window.x as f64 + window.w as f64 / 2.0,
            window.y as f64 + window.h as f64 / 2.0,
        )
        .expect("click the already-focused content");
    let after_click = session.door().protocol_publishes().expect("read counters after click");
    assert_eq!(
        after_click, before_click,
        "a focused content click must build neither protocol snapshot"
    );

    let world = session.door().windows().expect("read the decorated window");
    let frame = world.frame_of(window.id).expect("the probe has server decorations");
    let titlebar = (frame.x as f64 + frame.w as f64 / 2.0, frame.y as f64 + 8.0);
    session
        .door()
        .drag_to(titlebar, (titlebar.0 + 96.0, titlebar.1 + 64.0))
        .expect("drag the titlebar");
    session.door().button("left", false).expect("release the drag");
    session.door().barrier().expect("drag settles");

    let after_drag = session.door().protocol_publishes().expect("read counters after drag");
    assert_eq!(
        after_drag.hyprland, after_click.hyprland,
        "geometry-only drag has no Hyprland event snapshot"
    );
    assert_eq!(
        after_drag.foreign_full, after_click.foreign_full,
        "dragging one window must not rebuild the full toplevel view"
    );
    assert_eq!(
        after_drag.foreign_drag - after_click.foreign_drag,
        8,
        "each of the eight settled drag steps must sync exactly one toplevel"
    );
    assert!(session.compositor_alive(), "optimized protocol paths keep the session alive");
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

/// The direct dispatcher must preserve the semantic difference between
/// `movetoworkspace` and `movetoworkspacesilent`: move the active
/// window, leave the output on its current workspace, and focus what
/// the move exposed.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn silently_moves_a_real_window_without_following_it() {
    let mut session = boot("hypr-ipc-workspace-send");
    let dir = socket_dir(&session);
    let probe = profile_binary("chonk-fullscreen-probe").expect("probe is built");
    let program = probe.display().to_string();
    session.launch(&program, &["SilentA"]).expect("first probe launches");
    session.wait_for_window("SilentA").expect("first probe maps");
    session.launch(&program, &["SilentB"]).expect("second probe launches");
    session.wait_for_window("SilentB").expect("second probe maps and takes focus");

    assert_eq!(request(&dir, "/dispatch movetoworkspacesilent 2").trim(), "ok");

    poll_until(EVENT, "the active window to move while the desktop stays", || {
        let clients = json(&dir, "j/clients");
        let moved = clients.as_array()?.iter().find(|client| client["title"] == "SilentB")?;
        (moved["workspace"]["id"] == serde_json::json!(2)
            && json(&dir, "j/activeworkspace")["id"] == serde_json::json!(1)
            && json(&dir, "j/activewindow")["title"] == "SilentA")
            .then_some(())
    })
    .expect("silent move must send B, remain on workspace 1, and focus A");
}

/// A mutation arriving through chonkstep's native bar protocol must
/// invalidate the Hyprland event baseline too. Omarchy can have both
/// clients connected at once; treating only Hyprland requests as dirty
/// leaves its workspace indicator stale after the native bar switches.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn native_control_mutations_reach_the_hyprland_event_stream() {
    let session = boot("hypr-ipc-native-control");
    let dir = socket_dir(&session);
    let mut events = Events::connect(&dir);

    assert_eq!(request(&dir, "/dispatch workspace 2").trim(), "ok");
    assert_eq!(events.wait_for("workspacev2"), "2,2");

    let path = control_socket(&session);
    let mut native = poll_until(EVENT, "the native control socket", || UnixStream::connect(&path).ok())
        .expect("the native control socket accepts a bar");
    native
        .write_all(b"{\"request\":\"focus-workspace\",\"index\":0}\n")
        .expect("send the native workspace switch");

    assert_eq!(events.wait_for("workspacev2"), "1,1");
    assert_eq!(json(&dir, "j/activeworkspace")["id"], serde_json::json!(1));
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
