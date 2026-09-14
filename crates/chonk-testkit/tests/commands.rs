//! Running commands the user named, end to end: the `[commands]` table,
//! the `run` binding, and `autostart`. Same running story as `e2e.rs` —
//! these need a live Wayland session to nest inside, so they are
//! `#[ignore]`d; run them with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.
//!
//! These exist because this seam is the one place the desktop hands
//! control to something it knows nothing about, and every part of that
//! handover is observable only from outside the process. A unit test
//! can prove the config parses and that a key resolves to
//! `Action::Run("x")`. It cannot prove that pressing the key reaches
//! `x`, that `x` inherited a usable environment, or that autostart ran
//! once rather than never or twice. So each test here checks the same
//! way: give the command a side effect on disk, then look for the file.
//!
//! Only the Num Lock test starts a client (`foot`), because whether a
//! keypad key is a digit is a client's reading of it. Every other command
//! under test writes a file, which keeps what is being proved narrow.

use std::path::Path;
use std::time::Duration;

use chonk_testkit::{keys, poll_until, profile_binary, session_dir, Session, SessionOptions};

/// Long enough for a spawn, an exec and a small write to land, without
/// making a failing test wait on the full default.
const SPAWNED: Duration = Duration::from_secs(8);

/// A command line that writes `marker` into the session's own scratch
/// directory, as a TOML array so the path survives whitespace.
///
/// Arrays rather than a bare string on purpose: this is also the
/// documented escape hatch for arguments that contain spaces, and a
/// temp path is exactly where an unexpected space would show up.
fn writes(dir: &Path, marker: &str) -> String {
    let path = dir.join(marker);
    format!(r#"["sh", "-c", "echo ran > {}"]"#, path.display())
}

/// The marker file a command was asked to write, once it exists.
/// Shaped as `Option` because that is what `poll_until` polls on.
fn marker(dir: &Path, name: &str) -> Option<()> {
    dir.join(name).exists().then_some(())
}

/// The number of completed command writes, once at least `wanted`
/// have landed. A bound key can run a process asynchronously, so the
/// filesystem is the observable completion boundary rather than a
/// sleep after input injection.
fn marker_lines(dir: &Path, name: &str, wanted: usize) -> Option<usize> {
    let text = std::fs::read_to_string(dir.join(name)).ok()?;
    let lines = text.lines().count();
    (lines >= wanted).then_some(lines)
}

/// The whole point of the seam: a bound key runs the command it names.
///
/// Before this existed there was no way to bind a key to anything the
/// window manager did not already implement, which meant no way to
/// reach another desktop's tooling at all.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_bound_key_runs_the_command_it_names() {
    let dir = session_dir("commands-run");
    let config = format!(
        "[commands]\nmark = {}\n\n[keybindings]\n\"super+space\" = \"run mark\"\n",
        writes(&dir, "pressed")
    );
    let mut session = Session::boot(
        "commands-run",
        SessionOptions { config_extra: config, ..Default::default() },
    )
    .expect("session boots");

    session.door().chord(keys::LEFTMETA, keys::SPACE).expect("chord injects");
    poll_until(SPAWNED, "the marker written by the command `mark`", || marker(&dir, "pressed"))
        .expect("super+space should have run the command named `mark`");
}

/// The media keys are bindable, and bindable *bare* — no modifier.
///
/// This is the shape every volume and brightness binding on a laptop
/// takes, and until the parser had names for these keysyms they were
/// not merely unbound but unbindable: the spec was rejected before it
/// ever reached a keymap.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_bare_media_key_runs_a_command() {
    let dir = session_dir("commands-media");
    let config = format!(
        "[commands]\nlouder = {}\n\n[keybindings]\n\"volumeup\" = \"run louder\"\n",
        writes(&dir, "louder")
    );
    let mut session = Session::boot(
        "commands-media",
        SessionOptions { config_extra: config, ..Default::default() },
    )
    .expect("session boots");

    session.door().tap_key(keys::VOLUMEUP).expect("key injects");
    poll_until(SPAWNED, "the marker written by the command `louder`", || marker(&dir, "louder"))
        .expect("a bare volumeup press should have run the command named `louder`");
}

/// A named keypad binding reaches the compositor's real keyboard path
/// at both levels of the keypad keymap.
///
/// The config parser intentionally resolves `kp1` to the physical
/// key's level-0 cursor symbol. This test is the integration proof for
/// that policy: unlike table assertions, these evdev events update the
/// seat's real xkb state and pass through production binding matching.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_keypad_binding_fires_with_num_lock_off_and_on() {
    let dir = session_dir("commands-keypad-numlock");
    let presses = dir.join("presses");
    let config = format!(
        "[commands]\nmark = [\"sh\", \"-c\", \"echo ran >> {}\"]\n\n[keybindings]\n\"kp1\" = \"run mark\"\n",
        presses.display()
    );
    let mut session = Session::boot(
        "commands-keypad-numlock",
        SessionOptions { config_extra: config, ..Default::default() },
    )
    .expect("session boots with a named keypad binding");

    // A fresh default keymap starts with Num Lock off. KEY_KP1 must
    // match its level-0 KP_End symbol through the real input path.
    session.door().tap_key(keys::KP1).expect("keypad 1 injects with Num Lock off");
    poll_until(SPAWNED, "the first keypad command write", || marker_lines(&dir, "presses", 1))
        .expect("kp1 should run its command with Num Lock off");

    // Toggle the seat's real xkb modifier state, then inject the same
    // physical key. Binding lookup must remain on the stable raw symbol
    // even though client delivery would now see the Num Lock level.
    session.door().tap_key(keys::NUMLOCK).expect("Num Lock toggles on");
    session.door().tap_key(keys::KP1).expect("keypad 1 injects with Num Lock on");
    let writes = poll_until(SPAWNED, "the second keypad command write", || {
        marker_lines(&dir, "presses", 2)
    })
    .expect("kp1 should run its command with Num Lock on");

    assert_eq!(writes, 2, "one physical press in each Num Lock state must run exactly once");
}

/// Omarchy ships `numlock_by_default = true`, so a numpad has to type
/// digits from the first key of a session. The lock is applied as the
/// keymap is installed and not on every reload: a user who turns Num Lock
/// off keeps it off when the configuration is next re-read.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn num_lock_starts_locked_by_default_and_a_reload_does_not_lock_it_again() {
    let dir = session_dir("commands-numlock-default");
    let typed = dir.join("typed");
    let mut session = Session::boot(
        "commands-numlock-default",
        SessionOptions { config_extra: "[input]\nnumlock_by_default = true\n".into(), ..Default::default() },
    )
    .expect("session boots with Num Lock on by default");
    // Each line the terminal reads lands in `typed`, so the file holds
    // exactly what the client made of the keys.
    session
        .launch(
            "foot",
            &[
                "--title=numlock-typing",
                "--override=locked-title=yes",
                "sh",
                "-c",
                r#"while IFS= read -r line; do printf '%s\n' "$line" >> "$1"; done"#,
                "numlock-typing",
                typed.to_str().expect("a UTF-8 scratch path"),
            ],
        )
        .expect("foot launches");
    let window = session.wait_for_window("numlock-typing").expect("the terminal maps");
    session.door().click(f64::from(window.x + 40), f64::from(window.y + 40)).expect("focus the terminal");

    session.door().tap_key(keys::KP1).expect("keypad 1 at session start");
    session.door().tap_key(keys::ENTER).expect("end the line");
    poll_until(SPAWNED, "the first typed line", || marker_lines(&dir, "typed", 1))
        .expect("the terminal should read the first line");

    session.door().tap_key(keys::NUMLOCK).expect("the user turns Num Lock off");
    let reloads = session.log().matches("reload requested").count();
    session.request_reload().expect("request a reload");
    poll_until(SPAWNED, "the requested reload", || {
        (session.log().matches("reload requested").count() > reloads).then_some(())
    })
    .expect("the compositor should reload");
    session.door().barrier().expect("the reload has applied");
    session.door().tap_key(keys::KP1).expect("keypad 1 after the reload");
    session.door().tap_key(keys::ENTER).expect("end the line");
    poll_until(SPAWNED, "the second typed line", || marker_lines(&dir, "typed", 2))
        .expect("the terminal should read the second line");

    let text = std::fs::read_to_string(&typed).expect("typed lines readable");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "1", "keypad 1 types a digit from the first key of the session: {lines:?}");
    assert_ne!(lines[1], "1", "a reload must not lock Num Lock again after the user turned it off: {lines:?}");
}

/// A switch binding runs its command when its own device toggles, and
/// while the session is locked only a `bindl` one does. Omarchy locks on
/// lid close through exactly this path, and marks its lid handlers
/// locked so they still answer on the lock screen.
///
/// The switch is named `ChonkStep Test Lid`, never `Lid Switch`: a
/// session that also read a real Omarchy install must not run its
/// lock-on-close handler from a test.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_lid_switch_runs_its_bindings_and_only_locked_ones_while_locked() {
    const LID: &str = "ChonkStep Test Lid";
    let dir = session_dir("commands-lid-switch");
    let append = |name: &str| format!("echo ran >> {}", dir.join(name).display());
    let lines = |name: &str| std::fs::read_to_string(dir.join(name)).map(|text| text.lines().count()).unwrap_or(0);
    let hyprland = format!(
        "bind = , switch:on:{LID}, exec, {}\nbindl = , switch:off:{LID}, exec, {}\nbind = , switch:on:chonkstep test lid, exec, {}\n",
        append("closed"),
        append("opened"),
        append("misnamed"),
    );
    let mut session = Session::boot(
        "commands-lid-switch",
        SessionOptions {
            config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
            config_root_files: vec![("hypr/hyprland.conf".into(), hyprland)],
            ..Default::default()
        },
    )
    .expect("session boots with switch bindings");

    session.door().switch("lid", true, LID).expect("the lid closes");
    poll_until(SPAWNED, "the lid-close command", || (lines("closed") == 1).then_some(()))
        .expect("closing the lid should run its switch binding");

    let probe = profile_binary("chonk-lock-probe").expect("cargo build -p chonk-testkit builds the probe");
    session.launch(probe.to_str().unwrap(), &["--hold"]).expect("the lock probe launches");
    poll_until(Duration::from_secs(15), "the probe to hold the lock", || {
        session.client_log("chonk-lock-probe").contains("holding the lock").then_some(())
    })
    .expect("the session locks");

    // The compositor reports each resolution in the pass that makes it,
    // so an unlocked binding that did not run is observed, not waited for.
    session.door().switch("lid", true, LID).expect("the lid closes while locked");
    poll_until(SPAWNED, "the locked lid-close toggle to resolve", || {
        session
            .log()
            .lines()
            .any(|line| line.contains("switch toggle resolved") && line.contains("locked=true") && line.contains("actions=0"))
            .then_some(())
    })
    .expect("a lid close on the lock screen resolves to no unlocked binding");
    session.door().switch("lid", false, LID).expect("the lid opens while locked");
    poll_until(SPAWNED, "the locked lid-open command", || (lines("opened") == 1).then_some(()))
        .expect("a locked switch binding runs on the lock screen");

    assert_eq!(lines("closed"), 1, "the unlocked binding must not run while locked");
    assert_eq!(lines("misnamed"), 0, "a switch binding matches its device name exactly");
    assert!(session.compositor_alive());
}

/// Autostart runs on a genuinely new session, in file order.
///
/// Order is checked rather than assumed because it is the documented
/// promise and the reason `autostart` is a list rather than a table: a
/// list that starts a shell and then something which talks to that
/// shell has an order that matters.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn autostart_runs_once_on_a_new_session_in_file_order() {
    let dir = session_dir("commands-autostart");
    let ordered = dir.join("order");
    let config = format!(
        "autostart = [\n  [\"sh\", \"-c\", \"echo first >> {p}\"],\n  [\"sh\", \"-c\", \"sleep 0.3; echo second >> {p}\"],\n]\n",
        p = ordered.display()
    );
    let _session = Session::boot(
        "commands-autostart",
        SessionOptions { config_extra: config, ..Default::default() },
    )
    .expect("session boots");

    poll_until(SPAWNED, "both autostart entries to have written a line", || {
        let text = std::fs::read_to_string(&ordered).ok()?;
        (text.lines().count() >= 2).then_some(())
    })
    .expect("both autostart entries should have run");

    let text = std::fs::read_to_string(&ordered).expect("order file readable");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines, ["first", "second"], "autostart must run in the order the file lists");
}

/// A binding naming a command that does not exist costs the user that
/// binding and nothing else — the session still comes up, and every
/// other binding still works.
///
/// The config layer's standing rule is that a broken config must never
/// cost the user their session; this is that rule for the one action
/// that can refer to something absent.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_binding_naming_a_missing_command_does_not_cost_the_session() {
    let dir = session_dir("commands-missing");
    let config = format!(
        "[commands]\nreal = {}\n\n[keybindings]\n\"super+space\" = \"run typo\"\n\"volumeup\" = \"run real\"\n",
        writes(&dir, "real")
    );
    let mut session = Session::boot(
        "commands-missing",
        SessionOptions { config_extra: config, ..Default::default() },
    )
    .expect("a config with a bad binding must still boot a session");

    // The bad binding is gone rather than bound to a failing spawn.
    session.door().chord(keys::LEFTMETA, keys::SPACE).expect("chord injects");
    // The good one in the same file still works, which is the half
    // that proves one bad entry cost exactly one entry.
    session.door().tap_key(keys::VOLUMEUP).expect("key injects");
    poll_until(SPAWNED, "the marker written by the surviving command `real`", || marker(&dir, "real"))
        .expect("the valid binding beside the broken one must still run");

    assert!(session.compositor_alive(), "the session must survive a binding it could not resolve");
    assert!(
        session.log().contains("not in [commands]"),
        "the dropped binding must be reported, naming the command; log said:\n{}",
        session.log()
    );
}

/// Nothing a command starts inherits the hot-restart marker.
///
/// A leaked `CHONKSTEP_SESSION_CONTINUES` tells every descendant it is
/// the continuation of a running session. Most programs do not care,
/// but a nested chonkstep does: it skips its own autostart and layout
/// restore, so the same binary and config behave differently depending
/// on whether the session that launched it had ever been restarted.
/// This pins the consumption at startup that stops it propagating.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_spawned_command_does_not_inherit_the_continuation_marker() {
    let dir = session_dir("commands-marker");
    let dump = dir.join("env");
    let config = format!(
        "[commands]\ndump = [\"sh\", \"-c\", \"env > {}\"]\n\n[keybindings]\n\"super+space\" = \"run dump\"\n",
        dump.display()
    );
    let mut session = Session::boot(
        "commands-marker",
        SessionOptions { config_extra: config, ..Default::default() },
    )
    .expect("session boots");

    session.door().chord(keys::LEFTMETA, keys::SPACE).expect("chord injects");
    poll_until(SPAWNED, "the spawned command's environment dump", || dump.exists().then_some(()))
        .expect("the env dump should have been written");

    let env = std::fs::read_to_string(&dump).expect("env dump readable");
    assert!(
        !env.contains("CHONKSTEP_SESSION_CONTINUES"),
        "a spawned command must not inherit the hot-restart marker; its environment held:\n{env}"
    );
}
