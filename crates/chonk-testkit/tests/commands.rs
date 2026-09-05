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
//! No `foot` or other client is needed — the commands under test write
//! files, which keeps what is being proved narrow.

use std::path::Path;
use std::time::Duration;

use chonk_testkit::{keys, poll_until, session_dir, Session, SessionOptions};

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
