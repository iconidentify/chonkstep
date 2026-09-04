//! Reading the user's *live* Hyprland configuration, end to end.
//!
//! # What only this test can see
//!
//! `wm_config::hyprland` is unit-tested hard, against byte-for-byte
//! copies of a real Omarchy 4 machine's files — 44 tests over the
//! parser, the tag resolution, the precedence and a battery of hostile
//! input. All of that proves the *reading* is right. None of it proves
//! the reading reaches anything.
//!
//! Every piece of a read travels a different road out of the config: a
//! binding through `SessionState` into the compositor's real grab
//! table, a float rule through `Config::float_policy` into
//! `WindowManager::set_float_policy` and out the other side as a
//! window's actual size at map time, and both of them again — from
//! scratch — when the watch notices the file changed. A read that
//! parsed perfectly and wired up to nothing would pass the entire unit
//! suite and leave the user pressing dead keys.
//!
//! So this boots a real compositor against a **scratch copy of an
//! Omarchy config tree**, and looks:
//!
//! 1. A chord written in *their* file drives a chonkstep verb. The
//!    chord is deliberately one the baked preset does not bind, so the
//!    only thing that could have bound it is the live read.
//! 2. A `windowrule` written in *their* file sizes a real window.
//! 3. Both change when the file changes, **without a restart** — which
//!    is the entire point of the feature, because Omarchy's own menu is
//!    what writes these files.
//!
//! # The scratch tree
//!
//! `crates/wm-config/tests/fixtures/hyprland/machine` is a copy of the
//! development machine: Omarchy 4.0.0.alpha's whole shipped
//! `default/hypr` tree, and the user's own `~/.config/hypr`. It is
//! copied into the session's isolated roots and `OMARCHY_PATH` is
//! pointed at it, so the compositor reads a real Omarchy configuration
//! through the real include graph — `hyprland.lua` requiring
//! `default.hypr.omarchy`, which fans out over `bindings/` and
//! `apps/`. **The real Omarchy on the machine is never touched.**
//!
//! Same run rules as `e2e.rs`: needs a session to nest in, so
//! `#[ignore]`d. `scripts/e2e.sh`, or
//! `cargo test -p chonk-testkit --test hyprland_config -- --ignored`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chonk_testkit::{keys, poll_until, profile_binary, session_dir, Session, SessionOptions};

/// `KEY_K` from input-event-codes.h. `super+shift+k` is the chord the
/// scratch configuration binds, and it is chosen precisely because
/// **the baked preset does not bind it**: `preset.rs` leaves `super+k`
/// deliberately unbound (Omarchy's cheatsheet, declined) and never
/// mentions the shifted form. If this chord closes a window, a live
/// read is the only thing that could have made it.
const KEY_K: u32 = 37;
/// `KEY_J` — what the *edited* configuration moves the close verb onto.
const KEY_J: u32 = 36;
/// `KEY_R` — deliberately unbound by this test's otherwise-minimal
/// config, except for the repeating probe below.
const KEY_R: u32 = 19;
/// `KEY_LEFTMETA`, the Super the chords are held with.
const KEY_LEFTMETA: u32 = 125;
/// `KEY_LEFTSHIFT`.
const KEY_LEFTSHIFT: u32 = 42;
/// `KEY_W` — the baked Omarchy keymap's close chord, `super+w`, which
/// the broken-configuration test uses to show that a read yielding
/// nothing leaves that keymap standing as the fallback.
const KEY_W: u32 = 17;

/// The window the rule acts on: this crate's own probe, whose app id
/// is `chonk-fullscreen-probe` and which asks for 400x300 and honours
/// whatever size it is configured at.
///
/// A terminal was the obvious first choice and is the wrong one: foot
/// quantises its height to whole character cells, so a rule asking for
/// 400 produced a 385-pixel window and the assertion would have been
/// testing the terminal's font metrics rather than the rule. The probe
/// has no such opinion, which makes the number in the config file and
/// the number on the screen the same number.
const PROBE: &str = "chonk-fullscreen-probe";

/// The size the scratch configuration's own window rule gives the
/// probe, and then the size it gives after the edit. Neither is the
/// 400x300 the probe asks for, and neither is chonkstep's hardcoded
/// Omarchy float size (875x600) — so a window at either one can only
/// have got there through the rule in *their* file.
const FIRST_SIZE: (u32, u32) = (700, 400);
const SECOND_SIZE: (u32, u32) = (520, 320);

/// The user's `hyprland.lua`, as Omarchy's own template ships it, plus
/// the two lines a user would add.
///
/// Written in Omarchy 4's real syntax — `o.bind`, `o.window`,
/// `hl.dsp.window.close()` — and in the place their own template says
/// to put personal configuration ("Add any other personal Hyprland
/// configuration below"). The `require` lines are theirs, untouched:
/// this file drives the whole include graph.
fn user_hyprland_lua(size: (u32, u32), close_chord: &str) -> String {
    format!(
        r#"-- Learn how to configure Hyprland: https://wiki.hypr.land/Configuring/Start/

dofile((os.getenv("OMARCHY_PATH") or "/usr/share/omarchy") .. "/default/hypr/bootstrap.lua")

-- Load Omarchy defaults.
require("default.hypr.omarchy")

require("hypr.monitors")
require("hypr.input")
require("hypr.bindings")
require("hypr.looknfeel")
require("hypr.autostart")

-- Add any other personal Hyprland configuration below.
o.bind("{close_chord}", "Close window", hl.dsp.window.close())
o.window("^chonk-fullscreen-probe$", {{ float = true, size = {{ {}, {} }} }})
"#,
        size.0, size.1
    )
}

/// Copies the captured machine into the scratch roots this session
/// will read, and returns the options that point it at them.
///
/// The Omarchy half lives *outside* the session directory because
/// `Session::boot` wipes that directory on every boot, and
/// `OMARCHY_PATH` has to survive being pointed at before the boot
/// happens.
fn scratch_machine(name: &str, size: (u32, u32), close_chord: &str) -> (PathBuf, SessionOptions) {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../wm-config/tests/fixtures/hyprland/machine");
    let omarchy_root = session_dir(&format!("{name}-omarchy"));
    let _ = std::fs::remove_dir_all(&omarchy_root);
    copy_tree(&fixtures.join("omarchy"), &omarchy_root);

    // The user's own `~/.config/hypr`, seeded into the session's
    // isolated `XDG_CONFIG_HOME` — the same place a real user's files
    // sit relative to their config home.
    let mut config_root_files: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(fixtures.join(".config/hypr")).expect("captured user config") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("lua") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name == "hyprland.lua" {
            continue;
        }
        config_root_files.push((format!("hypr/{name}"), std::fs::read_to_string(&path).expect("readable")));
    }
    config_root_files.push(("hypr/hyprland.lua".into(), user_hyprland_lua(size, close_chord)));

    let options = SessionOptions {
        // The posture that asks for Omarchy's vocabulary, which is
        // also what switches the live read on — see
        // `wm_config::hyprland::wanted`. Nothing here says
        // `hyprland_config`; the point is that the one documented line
        // is enough.
        config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\n".into(),
        config_root_files,
        env: vec![("OMARCHY_PATH".into(), omarchy_root.display().to_string())],
        ..Default::default()
    };
    (omarchy_root, options)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("scratch dir");
    for entry in std::fs::read_dir(from).expect("fixture dir").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy fixture");
        }
    }
}

/// The path the session reads the user's `hyprland.lua` from — what an
/// edit through Omarchy's menu would rewrite.
fn user_config_path(session: &Session) -> PathBuf {
    session.dir.join("config/hypr/hyprland.lua")
}

/// Waits for the probe's window and returns its content size.
fn probe_size(session: &mut Session) -> (u32, u32) {
    let window = session.wait_for_window(PROBE).expect("the probe maps");
    (window.w, window.h)
}

/// Holds Super+Shift, taps `code`, releases — the chord as a keyboard
/// really delivers it, through the compositor's real grab table.
fn super_shift(session: &mut Session, code: u32) {
    let door = session.door();
    door.key(KEY_LEFTMETA, true).expect("meta down");
    door.key(KEY_LEFTSHIFT, true).expect("shift down");
    door.tap_key(code).expect("chord");
    door.key(KEY_LEFTSHIFT, false).expect("shift up");
    door.key(KEY_LEFTMETA, false).expect("meta up");
}

/// The whole feature in one session: their binding drives a chonkstep
/// verb, their window rule sizes a real window, and both follow an
/// edit to the file with no restart.
///
/// One test rather than three because the third assertion is *about*
/// the first two — "the same session, after an edit" is not something
/// three separate boots can demonstrate.
#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn the_desktops_own_hyprland_config_drives_the_session_and_follows_an_edit() {
    let (_omarchy_root, options) = scratch_machine("hyprland-config", FIRST_SIZE, "SUPER + SHIFT + K");
    let mut session = Session::boot("hyprland-config", options).expect("session boots");

    // The read happened at all, and said so.
    let log = session.log();
    assert!(
        log.contains("read the desktop's live Hyprland configuration"),
        "the session should have reported reading the configuration:\n{}",
        tail(&log)
    );

    // ---- 1. Their window rule sizes a real window --------------------
    //
    // `o.window("^foot$", { float = true, size = { 700, 400 } })` is in
    // their file and nowhere else. 700x400 is not a size foot would
    // choose, and it is not chonkstep's hardcoded Omarchy float size
    // (875x600) either, so a window this size can only have come from
    // the rule that was read.
    let probe = profile_binary(PROBE).expect("cargo build -p chonk-testkit builds the probe");
    session.launch(&probe.display().to_string(), &[]).expect("the probe launches");
    assert_eq!(
        probe_size(&mut session),
        FIRST_SIZE,
        "the window rule from their own hyprland.lua should have sized this window"
    );

    // ---- 2. Their binding drives a chonkstep verb --------------------
    //
    // `super+shift+k` is bound to `hl.dsp.window.close()` in their file
    // and by nothing else on this desk — the baked preset does not
    // bind it. The chord goes in as real key events, through the real
    // grab table, and the window has to go away.
    super_shift(&mut session, KEY_K);
    session
        .wait_for_window_gone(PROBE)
        .expect("super+shift+k, bound in their file to hl.dsp.window.close(), should close the window");

    // ---- 3. An edit takes effect with no restart ---------------------
    //
    // This is the point of the whole module: Omarchy's menu edits these
    // files, so a rebind through their UI has to reach a session that
    // is already running. The edit moves the close verb onto another
    // chord and changes the window rule's size — two different roads
    // out of the config, so a watch that only re-applied one of them
    // would be caught here.
    let path = user_config_path(&session);
    std::fs::write(&path, user_hyprland_lua(SECOND_SIZE, "SUPER + SHIFT + J")).expect("rewrite their config");

    // The watch polls at one hertz. Wait for the compositor to say it
    // noticed, rather than sleeping a guessed interval.
    let log_path = session.dir.join("compositor.log");
    poll_until(Duration::from_secs(15), "the session to notice the edited Hyprland config", || {
        std::fs::read_to_string(&log_path).ok().filter(|log| log.contains("Hyprland configuration changed")).map(|_| ())
    })
    .expect("the watch should have noticed the edit");

    // The re-read is applied on the compositor's own thread a moment
    // after the log line, so both assertions below are polled rather
    // than taken once.
    session.launch(&probe.display().to_string(), &[]).expect("the probe launches again");
    let resized = poll_until(Duration::from_secs(10), "the re-read window rule to size a new window", || {
        let world = session.world().ok()?;
        let window = world.window_matching(PROBE)?;
        (window.w == SECOND_SIZE.0 && window.h == SECOND_SIZE.1).then_some(())
    });
    let observed = probe_size(&mut session);
    resized.unwrap_or_else(|e| {
        panic!("the edited window rule should size a new window {SECOND_SIZE:?}, got {observed:?}: {e}")
    });

    // ...and the rebound chord, on the new key, closes it — while the
    // old key does not, because their file no longer binds it.
    super_shift(&mut session, KEY_K);
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        session.world().expect("world").window_matching(PROBE).is_some(),
        "super+shift+k is no longer bound in their file and must no longer close anything"
    );
    super_shift(&mut session, KEY_J);
    session
        .wait_for_window_gone(PROBE)
        .expect("super+shift+j, the chord the edit moved the close verb onto, should close the window");
}

/// Omarchy's stock `SUPER + Arrow` binding crosses the complete live
/// path: Lua dispatcher, config action, seat grab, shell dispatch, and
/// geometry-ranked core focus. Closing after the focus move makes the
/// result observable without a test-only focus hook.
#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn stock_directional_focus_moves_between_real_floating_windows() {
    let (_omarchy_root, options) = scratch_machine("hyprland-directional-focus", FIRST_SIZE, "SUPER + SHIFT + K");
    let mut session = Session::boot("hyprland-directional-focus", options).expect("session boots");
    let probe = profile_binary(PROBE).expect("the probe is built");
    let program = probe.display().to_string();
    session.launch(&program, &["SpatialA"]).expect("left probe launches");
    let left = session.wait_for_window("SpatialA").expect("left probe maps");
    let frame = session
        .world()
        .expect("left probe geometry")
        .frame_of(left.id)
        .cloned()
        .expect("probe has server decorations");
    let grip = (f64::from(frame.x + frame.w as i32 / 2), f64::from(frame.y + 10));
    session.door().drag_to(grip, (grip.0 - 300.0, grip.1)).expect("move A left");
    session.door().button("left", false).expect("finish moving A");
    session.door().barrier().expect("move settles");
    session.launch(&program, &["SpatialB"]).expect("right probe launches");
    session.wait_for_window("SpatialB").expect("right probe maps and takes focus");

    let before = session.world().expect("window geometry");
    let a = before.window_matching("SpatialA").unwrap();
    let b = before.window_matching("SpatialB").unwrap();
    assert!(a.x < b.x, "the posed arrangement must put A to B's left: A={a:?}, B={b:?}");

    session.door().chord(keys::LEFTMETA, keys::LEFT).expect("stock super+left binding");
    session.door().chord(keys::LEFTMETA, KEY_W).expect("close whichever window now has focus");
    session.wait_for_window_gone("SpatialA").expect("super+left must have focused the left-hand window");
    assert!(
        session.world().expect("remaining windows").window_matching("SpatialB").is_some(),
        "directional focus must not have left the right-hand window focused"
    );
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn selection_layer_bindings_override_only_for_the_layers_lifetime() {
    let (omarchy_root, mut options) = scratch_machine("hyprland-layer-bindings", FIRST_SIZE, "SUPER + SHIFT + K");
    let marker = session_dir("hyprland-layer-binding-command");
    let _ = std::fs::remove_dir_all(&marker);
    let bin = marker.join("bin");
    let calls = marker.join("calls");
    std::fs::create_dir_all(&bin).unwrap();

    let write_command = |name: &str, source: &str| {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' {source} >> '{}'\n", calls.display())).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write_command("global-return-probe", "global");
    write_command("omarchy-capture-region", "\"$*\"");

    let user = options
        .config_root_files
        .iter_mut()
        .find(|(path, _)| path == "hypr/hyprland.lua")
        .expect("scratch machine has its user entry point");
    user.1.push_str("\no.bind(\"RETURN\", \"Global Return probe\", \"global-return-probe\")\n");
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    options.env.push(("PATH".into(), format!("{}:{inherited_path}", bin.display())));
    options.env.push(("OMARCHY_PATH".into(), omarchy_root.display().to_string()));

    let mut session = Session::boot("hyprland-layer-bindings", options).expect("session boots");

    // Outside the overlay, Return is the user's ordinary global bind.
    session.door().tap_key(keys::ENTER).unwrap();
    poll_until(Duration::from_secs(5), "the global Return binding", || {
        std::fs::read_to_string(&calls).ok().filter(|text| text.contains("global"))
    })
    .expect("the global binding works before the layer maps");
    std::fs::write(&calls, "").unwrap();

    // Omarchy's slurp surface maps with namespace=selection. The same
    // chord must now take the layer-local command, even though a global
    // binding exists for it.
    let bar = profile_binary("chonk-fake-bar").expect("fake layer client is built");
    session.launch(bar.to_str().unwrap(), &["24", "top", "selection"]).unwrap();
    poll_until(Duration::from_secs(5), "the selection layer to map", || {
        session.log().contains("namespace=selection mapped=true").then_some(())
    })
    .expect("selection layer maps");
    session.door().tap_key(keys::ENTER).unwrap();
    let scoped = poll_until(Duration::from_secs(5), "the selection-scoped Return binding", || {
        std::fs::read_to_string(&calls).ok().filter(|text| text.contains("--take-window"))
    })
    .expect("Return invokes Omarchy's capture action while selection is live");
    assert!(!scoped.contains("global"), "the scoped action, not both actions, owns the chord: {scoped}");

    // Destroying the final surface in that namespace restores the
    // exact global keymap; no scoped bind survives it.
    session.kill_client("chonk-fake-bar");
    poll_until(Duration::from_secs(5), "the selection layer to unmap", || {
        session.log().contains("layer surface destroyed").then_some(())
    })
    .expect("selection layer is destroyed");
    std::fs::write(&calls, "").unwrap();
    session.door().tap_key(keys::ENTER).unwrap();
    let after = poll_until(Duration::from_secs(5), "the global Return binding to be restored", || {
        std::fs::read_to_string(&calls).ok().filter(|text| text.contains("global"))
    })
    .expect("the global binding owns Return again after the layer closes");
    assert!(!after.contains("--take-window"), "the layer binding leaked after unmap: {after}");
}

/// The deadline-driven idle loop must honor Hyprland's `binde` cadence.
/// Hold a real injected key and inspect the compositor-owned repeat
/// state through the opt-in test door: this crosses the live config
/// reader, grab table, seat repeat state, scheduler deadline, and
/// action dispatch without coupling their correctness to child-process
/// scheduling on a loaded CI runner.
#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn a_held_binde_keeps_the_configured_repeat_rate_under_idle_scheduling() {
    let config = "input {\n  repeat_rate = 25\n  repeat_delay = 120\n}\n\
                  binde = SUPER, R, workspace, 1\n";
    let options = SessionOptions {
        config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\n".into(),
        config_root_files: vec![("hypr/hyprland.conf".into(), config.into())],
        ..Default::default()
    };
    let mut session = Session::boot("hyprland-repeat", options).expect("session boots");

    {
        let door = session.door();
        door.key(KEY_LEFTMETA, true).expect("meta down");
        door.barrier().expect("modifier settles");
        door.key(KEY_R, true).expect("repeating key down");
        door.barrier().expect("initial press settles");
    }
    let repeated = poll_until(Duration::from_secs(2), "five emissions from the held 25 Hz binding", || {
        let (emitted, interval) = session.door().repeating_binding().ok().flatten()?;
        (emitted >= 5).then_some((emitted, interval))
    });
    {
        let door = session.door();
        door.key(KEY_R, false).expect("repeating key up");
        door.key(KEY_LEFTMETA, false).expect("meta up");
        door.barrier().expect("release settles");
    }

    let (count, interval) = repeated.expect("the compositor-owned repeat deadline should keep firing the binding");
    assert!(count >= 5, "the configured repeats must keep running while held, got {count}");
    assert_eq!(interval, Duration::from_millis(40), "repeat_rate = 25 must become a 40 ms cadence");
    assert!(session.compositor_alive(), "a repeating binding must not destabilize the compositor");
}

/// Reading somebody else's configuration must never be able to break
/// the session — at the one altitude where "never" can actually be
/// checked.
///
/// The parser's own hostile-input tests
/// (`wm_config::hyprland::tests`) prove it cannot panic on unterminated
/// strings, five thousand nested braces, loops asking for a hundred
/// million iterations, patterns that would hang a backtracking engine,
/// random bytes, or every truncation of the real files. What they
/// cannot show is what a *session* does with the result, and that is
/// the promise a user actually holds: their desk still comes up, and
/// comes up usable, on a day when their config is garbage.
///
/// So this boots a whole compositor against a configuration tree made
/// of exactly those inputs and checks two things. The desk works — a
/// window maps and the desk's own default close chord still closes it,
/// because a configuration that yielded nothing leaves the built-in
/// keymap standing. And nothing was executed: the tree contains an
/// `o.shell_succeeds` whose command would leave a file behind, and the
/// file must not be there. A config file must not be a code-execution
/// path into the window manager.
#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn a_broken_configuration_costs_the_user_the_configuration_and_nothing_else() {
    // Named into the session's own scratch so a failure leaves it
    // behind to look at, and so two runs cannot collide.
    let witness = session_dir("hyprland-config-hostile").join("must-never-exist");
    let _ = std::fs::remove_file(&witness);
    let omarchy_root = session_dir("hyprland-config-hostile-omarchy");
    let _ = std::fs::remove_dir_all(&omarchy_root);
    std::fs::create_dir_all(omarchy_root.join("default/hypr")).expect("scratch omarchy");

    let hostile = format!(
        concat!(
            // An include of itself, and of something that is not there.
            "require(\"hypr.hyprland\")\n",
            "require(\"../../../../etc/passwd\")\n",
            // A loop asking for the world, and one with no bounds at all.
            "for i = 1, 100000000 do o.bind(\"SUPER + \" .. i, nil, \"x\") end\n",
            "for i = 1, 1e400 do o.bind(\"SUPER + W\", nil, \"x\") end\n",
            // Keys and rules from nowhere.
            "o.bind(\"SUPER + code:99999\", nil, \"x\")\n",
            "o.window({{ class = \"(((((\" }}, {{ float = true, size = {{ -1, 0 }} }})\n",
            "hl.monitor({{ scale = 0/0, mode = 1e999 }})\n",
            // The one that must not run.
            "if o.shell_succeeds(\"touch {}\") then o.bind(\"SUPER + Z\", nil, \"x\") end\n",
            // ...and a file that simply stops in the middle.
            "o.bind(\"unterminated\n",
        ),
        witness.display()
    );

    let options = SessionOptions {
        config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\n".into(),
        config_root_files: vec![
            ("hypr/hyprland.lua".into(), hostile),
            ("hypr/more.lua".into(), "{{{{{{{{\n--[[ unterminated\n".into()),
        ],
        env: vec![("OMARCHY_PATH".into(), omarchy_root.display().to_string())],
        ..Default::default()
    };
    let mut session = Session::boot("hyprland-config-hostile", options).expect("the session must still boot");

    assert!(!witness.exists(), "a configuration file must never be able to run anything");

    // Usable, not merely alive — and usable in the specific way the
    // fallback promises. A read that yielded no bindings leaves the
    // *baked* Omarchy keymap standing (`hyprland::apply` refuses to
    // replace a working keymap with an empty one), so this desk should
    // still answer `super+w`, the preset's close chord. That is the
    // whole "the preset is the fallback, not a second source of truth"
    // claim, observed rather than asserted about a struct.
    let probe = profile_binary(PROBE).expect("the probe is built");
    session.launch(&probe.display().to_string(), &[]).expect("the probe launches");
    session.wait_for_window(PROBE).expect("a window still maps on a desk with a broken config");
    {
        let door = session.door();
        door.key(KEY_LEFTMETA, true).expect("meta down");
        door.tap_key(KEY_W).expect("super+w");
        door.key(KEY_LEFTMETA, false).expect("meta up");
    }
    session
        .wait_for_window_gone(PROBE)
        .expect(
            "the baked Omarchy keymap's super+w must close the client and retire its ledger record when the live configuration is garbage",
        );
    assert!(session.compositor_alive(), "a broken configuration must never cost the user their session");
}

/// The last few lines of a log, for a failure message.
fn tail(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(25)..].join("\n")
}
