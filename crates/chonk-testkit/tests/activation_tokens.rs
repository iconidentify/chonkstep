//! `xdg_activation_v1` end to end: the abandoned-token resource bound,
//! and the focus policy `misc:focus_on_activate` names.
//!
//! The policy tests all have the same shape. Window A is the probe's
//! (`chonk-activation-token-probe`), window B maps after it and so
//! holds the keyboard, and then something asks for A to be activated.
//! Whether the keyboard moves to A is the observation, read from the
//! compositor's own idea of focus (`World::logical_focus`) rather than
//! from either client, because the bug this pins is the compositor
//! moving it when it should not have.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chonk_testkit::{keys, poll_until, profile_binary, require_client, skip_log_path, Session, SessionOptions, WindowInfo};

const EVENT: Duration = Duration::from_secs(10);
const PROBE: &str = "chonk-activation-token-probe";
/// The other window, whose only job is to hold the keyboard.
const OTHER: &str = "chonk-fullscreen-probe";
/// `KEY_A`, the letter the exec-bind test's chord is on.
const KEY_A: u32 = 30;

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn abandoned_activation_tokens_are_bounded_per_client() {
    let mut session = Session::boot("activation-token-bound", SessionOptions::default()).unwrap();
    let probe = profile_binary(PROBE).expect("cargo build -p chonk-testkit builds the activation-token probe");
    session.launch(probe.to_str().unwrap(), &[]).expect("the activation-token probe launches");

    let report = poll_until(Duration::from_secs(10), "the token request burst to complete", || {
        let report = session.client_log(PROBE);
        report.contains("**requested").then_some(report)
    })
    .expect("the probe should complete its wire lifecycle");
    assert!(
        report.contains("**requested 512; completed 512**"),
        "every valid protocol request must receive a completion event: {report}"
    );
    assert_eq!(
        session.door().activation_tokens().unwrap(),
        256,
        "one connection may retain no more than its admission quota"
    );
    assert!(session.compositor_alive(), "the compositor survives the request burst");
}

/// The session's Hyprland IPC directory, from the signature the
/// compositor logs — the same way `scripts/wayland-session.sh` reads it.
fn socket_dir(session: &Session) -> PathBuf {
    let signature = poll_until(EVENT, "the hyprland ipc log line", || {
        let log = session.log();
        let at = log.find("hyprland ipc listening")?;
        // `tracing` colours the field keys even into a file, so match
        // the quoted value rather than the key.
        let rest = &log[at..];
        let start = rest.find('"')? + 1;
        let end = rest[start..].find('"')? + start;
        Some(rest[start..end].to_string())
    })
    .expect("the compositor logs the signature it bound");
    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    PathBuf::from(runtime).join("hypr").join(signature)
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
            let line = line.trim_end().to_string();
            if let Some(data) = line.strip_prefix(&prefix) {
                return data.to_string();
            }
            seen.push(line);
        }
        panic!("never saw {name}; events seen: {seen:#?}");
    }
}

/// The session every policy test boots: the Hyprland IPC on, for the
/// `urgent>>` event, and a Hyprland configuration of the test's own
/// under the isolated config home, read because `desktop = "omarchy"`
/// asks for Omarchy's vocabulary.
fn boot(name: &str, hyprland_conf: &str) -> Session {
    Session::boot(
        name,
        SessionOptions {
            config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
            config_root_files: vec![("hypr/hyprland.conf".into(), hyprland_conf.to_string())],
            env: vec![("CHONKSTEP_HYPRLAND_IPC".into(), "1".into())],
            ..Default::default()
        },
    )
    .expect("the nested compositor boots")
}

fn probe_path() -> String {
    profile_binary(PROBE).expect("cargo build -p chonk-testkit builds the activation-token probe").display().to_string()
}

/// Maps the keyboard-holding window B and waits for the compositor to
/// have focused it.
fn map_other(session: &mut Session, title: &str) -> WindowInfo {
    let other = profile_binary(OTHER).expect("cargo build -p chonk-testkit builds the fullscreen probe");
    session.launch(other.to_str().unwrap(), &[title, "other-app"]).expect("the other window launches");
    let window = session.wait_for_window(title).expect("the other window maps");
    poll_until(EVENT, "the newest window to take the keyboard", || {
        (session.world().ok()?.logical_focus == Some(window.id)).then_some(())
    })
    .expect("a freshly mapped window is focused");
    window
}

fn focus_of(session: &mut Session) -> Option<u64> {
    session.world().expect("world").logical_focus
}

/// Every level-triggered account of a refused activation the
/// compositor has logged so far.
fn refusals(session: &Session) -> usize {
    session.log().lines().filter(|line| line.contains("activation request refused the keyboard")).count()
}

/// Level-triggered urgency transitions, as `xwayland_input` reads them:
/// the event stream is edge-triggered and carries no history.
fn urgency_transitions(session: &Session, urgent: bool) -> usize {
    let wanted = format!("urgent={urgent}");
    session.log().lines().filter(|line| line.contains("window urgency changed") && line.contains(&wanted)).count()
}

/// A client that mints a token with no serial and activates its own
/// unfocused window is asking for attention nobody gave it. It gets an
/// urgency hint — on the bar, through `urgent>>` — and not the
/// keyboard the user is typing into B with.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn a_self_made_token_from_a_background_client_marks_it_urgent_instead_of_taking_focus() {
    let mut session = boot("activation-self-urgent", "");
    let probe = probe_path();
    session.launch(&probe, &["self-activate", "--on-blur"]).expect("the probe launches");
    let a = session.wait_for_window("activation-self").expect("window A maps");
    // Subscribe before B maps: the urgent edge fires the moment A sees
    // the keyboard leave, and the stream carries no history.
    let mut events = Events::connect(&socket_dir(&session));
    let b = map_other(&mut session, "activation-other");

    let activated = poll_until(EVENT, "the probe to activate itself on losing the keyboard", || {
        session.client_log(PROBE).contains("**activated own window").then_some(())
    });
    activated.expect("the keyboard leaving A is the probe's cue");
    poll_until(EVENT, "the compositor to refuse the request", || (refusals(&session) >= 1).then_some(()))
        .expect("a serial-less token from a background client is refused");
    let urgent = events.wait_for("urgent");
    assert!(!urgent.is_empty(), "the urgent event names the window");
    assert!(urgency_transitions(&session, true) >= 1, "the refusal marks the window urgent");
    assert_eq!(focus_of(&mut session), Some(b.id), "the keyboard stays with B, not A ({})", a.id);
    assert!(session.compositor_alive());
}

/// A token the focused client made from one of its own key presses,
/// handed to another client and redeemed at once, is the protocol's
/// whole purpose — a link clicked in a terminal opening in the browser
/// — and moves the keyboard.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn a_token_the_focused_client_made_from_a_key_press_moves_focus_when_handed_on() {
    let mut session = boot("activation-handed", "");
    let hand_off = session.dir.join("handed-token");
    let probe = probe_path();
    session.launch(&probe, &["activate-from-file", hand_off.to_str().unwrap()]).expect("A launches");
    let a = session.wait_for_window("activation-handed").expect("window A maps");
    session.launch(&probe, &["mint-on-key", hand_off.to_str().unwrap()]).expect("B launches");
    let b = session.wait_for_window("activation-mint").expect("window B maps");
    poll_until(EVENT, "B to take the keyboard", || (focus_of(&mut session) == Some(b.id)).then_some(()))
        .expect("the newest window is focused");

    // The user's own input, into B: the key press whose serial B mints
    // the token from.
    session.door().tap_key(keys::X).expect("a key press reaches B");
    poll_until(EVENT, "B to mint a token from the press", || {
        session.client_log(PROBE).contains("**minted a token for key serial").then_some(())
    })
    .expect("B mints on its first key press");
    poll_until(EVENT, "A to be focused with B's token", || (focus_of(&mut session) == Some(a.id)).then_some(()))
        .expect("a fresh token from the focused client's own input moves the keyboard");
    assert_eq!(refusals(&session), 0, "nothing about this request was refused");
    assert!(session.compositor_alive());
}

/// `misc:focus_on_activate = true` — what Omarchy ships — is today's
/// unconditional behaviour: the same self-made token now takes focus.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn misc_focus_on_activate_restores_unconditional_focus() {
    let mut session = boot("activation-misc-on", "misc {\n    focus_on_activate = true\n}\n");
    let probe = probe_path();
    session.launch(&probe, &["self-activate", "--on-blur"]).expect("the probe launches");
    let a = session.wait_for_window("activation-self").expect("window A maps");
    let _b = map_other(&mut session, "activation-other");
    poll_until(EVENT, "A to take the keyboard back with its own token", || {
        (focus_of(&mut session) == Some(a.id)).then_some(())
    })
    .expect("with misc:focus_on_activate on, any activation focuses");
    assert_eq!(refusals(&session), 0);
    assert!(session.compositor_alive());
}

/// The per-window rule keeps its veto whatever `misc` says: with the
/// key on and the probe's class ruled `focus_on_activate off`, the
/// request still does not move the keyboard.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn a_focus_on_activate_off_window_rule_still_refuses_with_misc_on() {
    let mut session = boot(
        "activation-rule-off",
        "misc {\n    focus_on_activate = true\n}\nwindowrule = focus_on_activate off, match:class ^chonk-activation-token-probe$\n",
    );
    let probe = probe_path();
    session.launch(&probe, &["self-activate", "--on-blur"]).expect("the probe launches");
    let a = session.wait_for_window("activation-self").expect("window A maps");
    let b = map_other(&mut session, "activation-other");
    // The probe's line follows a roundtrip on its `activate`, so the
    // compositor has already answered the request by the time it
    // appears; the barrier then lets the dispatch pass that answer
    // came from finish, and what focus is after that is the verdict.
    // `misc_focus_on_activate_restores_unconditional_focus` is the
    // same scaffold without the rule, and there the keyboard moves.
    poll_until(EVENT, "the probe to activate itself", || {
        session.client_log(PROBE).contains("**activated own window").then_some(())
    })
    .expect("the keyboard leaving A is the probe's cue");
    session.door().barrier().expect("the dispatch pass settles");
    assert_eq!(focus_of(&mut session), Some(b.id), "the rule keeps the keyboard with B, not A ({})", a.id);
    assert_eq!(refusals(&session), 0, "the rule's veto is the window's own, not the token policy's");
    assert!(session.compositor_alive());
}

/// Presses a `super+shift+<key>` chord through the door.
fn super_shift(session: &mut Session, key: u32) {
    let door = session.door();
    door.key(keys::LEFTMETA, true).unwrap();
    door.key(keys::LEFTSHIFT, true).unwrap();
    door.tap_key(key).unwrap();
    door.key(keys::LEFTSHIFT, false).unwrap();
    door.key(keys::LEFTMETA, false).unwrap();
}

/// The single-instance flow behind an `exec` bind: the first instance
/// has a window, the second — launched by the bind, carrying the token
/// the compositor minted for that launch — hands the token over and
/// exits, and the first raises its window with it.
fn exec_bind_raises_a_running_single_instance(name: &str, launcher_prefix: &str) {
    let probe = probe_path();
    let hand_off = std::env::temp_dir().join(format!("chonkstep-{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&hand_off);
    let conf = format!(
        "bind = SUPER SHIFT, A, exec, {launcher_prefix}{probe} single-instance {}\n",
        hand_off.display()
    );
    let mut session = boot(name, &conf);
    session.launch(&probe, &["single-instance", hand_off.to_str().unwrap()]).expect("the first instance launches");
    let a = session.wait_for_window("activation-single").expect("window A maps");
    let b = map_other(&mut session, "activation-other");
    assert_eq!(focus_of(&mut session), Some(b.id));

    super_shift(&mut session, KEY_A);
    let outcome = poll_until(Duration::from_secs(20), "the second instance to hand its token over", || {
        let log = session.client_log(PROBE);
        if log.contains("**activated with a handed token**") {
            Some(Ok(()))
        } else if log.contains("**second instance had no token**") {
            Some(Err("the exec bind launched its command without XDG_ACTIVATION_TOKEN"))
        } else {
            None
        }
    })
    .expect("the bind's launch reaches the first instance");
    outcome.unwrap_or_else(|why| panic!("{why}"));
    poll_until(EVENT, "A to be raised by the compositor-issued token", || {
        (focus_of(&mut session) == Some(a.id)).then_some(())
    })
    .expect("re-launching a running single-instance application raises its window");
    assert_eq!(refusals(&session), 0, "a compositor-issued token is never refused while fresh");
    let _ = std::fs::remove_file(&hand_off);
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn an_exec_bind_hands_its_launch_a_token_that_raises_a_running_single_instance() {
    exec_bind_raises_a_running_single_instance("activation-exec-bind", "");
}

/// The same flow through `uwsm app`, Omarchy's launcher wrapper. The
/// default scope unit is forked from the launching process and keeps
/// its environment, token included; this pins that on a machine that
/// has uwsm and a systemd user manager for it to launch into.
///
/// `scripts/e2e.sh --headless` has the first and not the second — its
/// private `dbus-run-session` bus carries no manager — so there this
/// test cannot run, and records that where a missing client would
/// rather than reporting `ok`. Nested in a live session (`cargo test
/// -p chonk-testkit --test activation_tokens -- --ignored`) it runs
/// the real launcher.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn an_exec_bind_through_uwsm_app_still_hands_its_launch_a_token() {
    if !require_client("uwsm") {
        return;
    }
    // The test's own thread, not the compositor's: waiting here
    // freezes nothing but this test.
    #[allow(clippy::disallowed_methods)]
    let uwsm_works = std::process::Command::new("uwsm")
        .args(["app", "--", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !uwsm_works {
        // Recorded the way `require_client` records a missing client,
        // so the run's closing summary names this test as not run.
        let note = "uwsm app cannot launch here (no systemd user manager on this session bus); \
                    an_exec_bind_through_uwsm_app_still_hands_its_launch_a_token did not run";
        let path = skip_log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            use std::io::Write as _;
            let _ = writeln!(file, "{note}");
        }
        eprintln!("skipping: {note}");
        return;
    }
    exec_bind_raises_a_running_single_instance("activation-exec-uwsm", "uwsm app -- ");
}

/// Behind the session lock an activation is neither honoured nor
/// parked for the moment of unlock: the window turns urgent and the
/// compositor's own focus stays where the user left it.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn activation_behind_the_session_lock_marks_urgent_and_parks_nothing() {
    let mut session = boot("activation-locked", "");
    let cue = session.dir.join("activate-now");
    let probe = probe_path();
    session.launch(&probe, &["self-activate", "--on-file", cue.to_str().unwrap()]).expect("A launches");
    let a = session.wait_for_window("activation-self").expect("window A maps");
    let b = map_other(&mut session, "activation-other");
    let workspace = session.world().expect("world").current_workspace;

    let locker = profile_binary("chonk-lock-probe").expect("cargo build -p chonk-testkit builds the lock probe");
    session.launch(locker.to_str().unwrap(), &["--hold"]).expect("the locker launches");
    poll_until(Duration::from_secs(15), "the locker to hold the lock", || {
        session.client_log("chonk-lock-probe").contains("holding the lock").then_some(())
    })
    .expect("the session locks");

    std::fs::write(&cue, "go").expect("the cue file");
    poll_until(EVENT, "the probe to activate itself behind the lock", || {
        session.client_log(PROBE).contains("**activated own window").then_some(())
    })
    .expect("the cue reaches the probe");
    poll_until(EVENT, "the compositor to refuse the request behind the lock", || {
        session
            .log()
            .lines()
            .any(|line| line.contains("activation request refused the keyboard") && line.contains("locked=true"))
            .then_some(())
    })
    .expect("a request behind the lock is refused as such");
    assert!(urgency_transitions(&session, true) >= 1, "the refusal marks the window urgent");
    let world = session.world().expect("world");
    assert_eq!(world.logical_focus, Some(b.id), "no focus change is parked for the unlock; A is {}", a.id);
    assert_eq!(world.current_workspace, workspace, "the workspace does not move under the lock");
    assert!(session.compositor_alive());
}

/// A taskbar's click — `zwlr_foreign_toplevel_handle_v1.activate`,
/// no token at all — is the user's own act and stays unconditional.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn a_taskbar_click_through_foreign_toplevel_activate_still_moves_focus() {
    let mut session = boot("activation-taskbar", "");
    let never = session.dir.join("never-written");
    let probe = probe_path();
    session.launch(&probe, &["self-activate", "--on-file", never.to_str().unwrap()]).expect("A launches");
    let a = session.wait_for_window("activation-self").expect("window A maps");
    let b = map_other(&mut session, "activation-other");
    assert_eq!(focus_of(&mut session), Some(b.id));

    session.launch(&probe, &["taskbar-activate", "activation-self"]).expect("the bar launches");
    poll_until(EVENT, "the bar to activate A", || {
        session.client_log(PROBE).contains("**taskbar activated").then_some(())
    })
    .expect("the bar finds A in the foreign-toplevel list");
    poll_until(EVENT, "A to be focused by the bar's click", || (focus_of(&mut session) == Some(a.id)).then_some(()))
        .expect("foreign-toplevel activate is unaffected by the token policy");
    assert_eq!(refusals(&session), 0);
    assert!(session.compositor_alive());
}
