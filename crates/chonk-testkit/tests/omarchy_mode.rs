//! `desktop = "omarchy"`, end to end: one config line, and the desk
//! comes up as the window manager for Omarchy's desktop rather than as
//! a desktop of its own.
//!
//! # Why this test and not unit tests
//!
//! The preset itself is pinned in `wm_config::preset` — the values it
//! resolves to, and that an explicit key beats each of them. What no
//! unit test can see is whether those resolved values actually *reach*
//! anything. Every one of them travels a different road out of the
//! config: `show_dock` through `DockVisibility::resolve` into a surface
//! and a workarea, `omarchy_bar` through `BarVisibility::resolve` into
//! a layer-shell namespace the compositor hides, `theme = "omarchy"`
//! through a file poll, and the keymap through the grab table. A
//! posture that resolved perfectly and wired up to nothing would pass
//! the whole unit suite and show the user a chonkstep desk.
//!
//! So this boots it and looks: no Dock column on screen, Omarchy's bar
//! hosted *and shown* (which is the preset's doing — the desk's own
//! default is hidden, as `omarchy_bar.rs` next door pins), Omarchy's
//! palette on the chrome, and one of the preset's own chords driving a
//! chonkstep action on a real window.
//!
//! Omarchy is simulated the way the two tests it borrows from simulate
//! it: a scratch Omarchy root whose `omarchy-launch-shell` runs
//! `chonk-fake-bar` under the real bar's namespace (`omarchy_bar.rs`),
//! and a palette planted in the isolated state root exactly where
//! `omarchy-theme-set` writes it (`omarchy.rs`). The real Omarchy on
//! the machine is never touched.
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest in,
//! so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit --test omarchy_mode -- --ignored`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chonk_testkit::{
    keys, near, poll_until, profile_binary, session_dir, RootMenu, Screenshot, Session, SessionOptions, FAKE_BAR_RGB,
};

/// The fake bar's thickness in the nested output's pixels (scale 1),
/// the same figure `omarchy_bar.rs` uses.
const BAR: u32 = 48;

/// `KEY_W` from input-event-codes.h — `super+w` is the Omarchy keymap's
/// close binding, and the one chord this test drives.
const KEY_W: u32 = 17;

/// Tokyo Night's dark background, the colour the desk's chrome and
/// wallpaper are derived from once the palette below is followed.
const TOKYO_NIGHT_BG: [u8; 3] = [0x1a, 0x1b, 0x26];

/// Tokyo Night as Omarchy ships it, trimmed to the keys
/// `wm_theme::omarchy` reads.
const TOKYO_NIGHT: &str = r##"mode = "dark"
accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"
background = "#1a1b26"
dark_background = "#13141c"
darker_background = "#0e0e14"
lighter_background = "#24283b"
foreground = "#a9b1d6"
dark_foreground = "#565f89"
light_foreground = "#b4bee6"
bright_foreground = "#c0caf5"
red = "#f7768e"
yellow = "#e0af68"
orange = "#eb927b"
green = "#9ece6a"
cyan = "#449dab"
blue = "#7aa2f7"
magenta = "#ad8ee6"
brown = "#75493d"
bright_red = "#ff7a93"
bright_yellow = "#ff9e64"
bright_green = "#b9f27c"
bright_cyan = "#0db9d7"
bright_blue = "#7da6ff"
bright_magenta = "#bb9af7"
"##;

/// The root menu the posture produces: a hosted shell, so the
/// `Omarchy Bar` row is there, and the scratch Omarchy root below
/// carries no menu definition, so the `Omarchy` submenu is not.
const HOSTED: RootMenu = RootMenu { omarchy_bar: true, omarchy: false };

/// The Omarchy root the compositor will find: the QML file the launcher
/// would hand Quickshell (never read), and a launcher that runs the
/// fake bar under Omarchy's own namespace. Lifted from
/// `omarchy_bar.rs`, which explains why standing this up is enough to
/// fool the compositor completely.
fn write_omarchy_root(beside: &Path) -> PathBuf {
    let root = beside.join("omarchy");
    std::fs::create_dir_all(root.join("shell")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("shell/shell.qml"), "// stands in for Omarchy's shell\n").unwrap();
    let bar = profile_binary("chonk-fake-bar").expect("cargo build -p chonk-testkit builds the bar");
    let launcher = root.join("bin/omarchy-launch-shell");
    std::fs::write(&launcher, format!("#!/bin/bash\nexec '{}' {BAR} top omarchy-bar\n", bar.display())).unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    root
}

/// The mean colour of the top strip, inset from the corners the Clip
/// and the launcher live in.
fn top_strip(shot: &Screenshot) -> [f64; 3] {
    shot.mean_rgb(shot.width / 4, 4, shot.width / 2, BAR - 8)
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn one_config_line_brings_up_the_window_manager_for_omarchys_desktop() {
    // The root is written *beside* the session directory, which
    // `Session::boot` clears.
    let beside = session_dir("omarchy-mode-root");
    let _ = std::fs::remove_dir_all(&beside);
    std::fs::create_dir_all(&beside).unwrap();
    let root = write_omarchy_root(&beside);

    let mut session = Session::boot(
        "omarchy-mode",
        SessionOptions {
            scale: Some(1.0),
            // The harness writes `omarchy_shell = true` itself, which
            // is what the posture would have defaulted anyway.
            omarchy_shell: true,
            // THE WHOLE INPUT. No `show_dock`, no `omarchy_bar`, no
            // `theme`, no `[keybindings]`, no `[commands]` — every
            // assertion below is the consequence of this one line.
            config_extra: "desktop = \"omarchy\"\n".to_string(),
            env: vec![("OMARCHY_PATH".to_string(), root.to_string_lossy().into_owned())],
            state_root_files: vec![
                ("omarchy/current/theme/colors.toml".to_string(), TOKYO_NIGHT.into()),
                ("omarchy/current/theme.name".to_string(), "tokyo-night\n".into()),
            ],
            ..SessionOptions::default()
        },
    )
    .unwrap();

    // -- Omarchy's shell is hosted and its bar maps ----------------------
    poll_until(Duration::from_secs(20), "the compositor to host the shell and the bar to map", || {
        let log = session.log();
        (log.contains("hosting Omarchy's shell") && log.contains("namespace=omarchy-bar mapped=true")).then_some(())
    })
    .expect("the posture hosts Omarchy's shell, so the fake launcher runs and its bar maps");

    // -- and the bar is SHOWN, which is the preset's doing ---------------
    //
    // This is the assertion the posture is worth having. A session that
    // merely hosts the shell keeps the bar off the screen until the
    // user asks (`omarchy_bar.rs` pins exactly that, on a config with
    // no `omarchy_bar` key). Nothing here asked. The bar is on screen
    // because `desktop = "omarchy"` defaulted `omarchy_bar = true`.
    poll_until(Duration::from_secs(10), "the bar to paint the top strip", || {
        let shot = session.screenshot("posture").ok()?;
        near(top_strip(&shot), FAKE_BAR_RGB).then_some(())
    })
    .expect("the posture shows the bar without being asked");
    assert!(
        !session.state_file("omarchy-bar").exists(),
        "and it is showing from the config, not from a remembered choice -- nobody has made one"
    );

    // -- there is no chonkstep Dock ---------------------------------------
    let world = session.world().unwrap();
    let (output_w, output_h) = (world.output_w, world.output_h);
    assert!(
        world.dock().is_none(),
        "the posture's `show_dock = false` must leave no Dock column on screen; shells: {:?}",
        world.shells
    );
    assert!(
        !session.state_file("dock-visibility").exists(),
        "and the Dock is absent from the config, not from a remembered choice"
    );

    // -- Omarchy's theme is followed --------------------------------------
    let world = poll_until(Duration::from_secs(10), "the desk to dress in Omarchy's palette", || {
        let world = session.world().ok()?;
        (world.theme.following == "omarchy").then_some(world)
    })
    .expect("the posture's `theme = \"omarchy\"` must make the session follow");
    assert_eq!(world.theme.name, "Omarchy (Tokyo Night)");
    assert_eq!(world.theme.appearance, "dark", "the palette's own mode, not a configured one");

    // -- the desk, as a user would see it ---------------------------------
    //
    // Below the bar and clear of the corners: bare desk wearing
    // Omarchy's background colour, where a Dock column would otherwise
    // be. The right-hand quarter is sampled deliberately — that is the
    // corner the Dock lives in.
    let shot = session.screenshot("desk").expect("a screenshot of the posture");
    let corner = shot.mean_rgb(output_w * 3 / 4, BAR + 8, output_w / 4 - 8, output_h / 3);
    assert!(
        near(corner, TOKYO_NIGHT_BG),
        "the top-right corner should be bare desk in Omarchy's colour, not a Dock: {corner:?}"
    );

    // -- the root menu is still chonkstep's -------------------------------
    //
    // The posture hands the furniture to Omarchy; it does not hand over
    // the desk. A right-click still opens chonkstep's own menu, with
    // the two rows that undo the posture's two visible choices in it.
    let menu = session
        .open_root_menu(&chonk_testkit::MenuMetrics::at_scale_1(), HOSTED.row_count())
        .expect("a right-click on the desk opens chonkstep's root menu");
    assert!(menu.above, "and it is raised over the guest's bar");
    // Dismissed with a click on bare desk well clear of the menu, the
    // bar's strip along the top and the Clip's corner at the bottom
    // right, so the click lands on nothing.
    let (w, h) = (output_w as f64, output_h as f64);
    session.door().click(w * 0.75, h * 0.25).unwrap();
    poll_until(Duration::from_secs(10), "the root menu to close", || {
        session.world().ok()?.menus().is_empty().then_some(())
    })
    .expect("a click on bare desk dismisses the root menu");

    // -- and one of the preset's chords does the chonkstep thing ----------
    //
    // `super+w` is Omarchy's close binding and nothing at all in
    // chonkstep's default keymap. A window opened and closed with it is
    // the whole keymap deliverable in one gesture: the chord an Omarchy
    // user arrives holding, reaching a chonkstep verb, through the real
    // grab table on a real client.
    session.launch("foot", &[]).unwrap();
    let window = session.wait_for_window("foot").expect("a client to close");
    session.door().chord(keys::LEFTMETA, KEY_W).unwrap();
    session
        .wait_for_window_gone("foot")
        .unwrap_or_else(|e| panic!("super+w must close the focused window (id {}): {e}", window.id));
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn an_explicit_key_beats_the_posture_on_a_running_desk() {
    // The other half of "a preset is a set of defaults, never a lock",
    // where it actually matters: not that the value resolves
    // differently, but that the *session* comes up differently. Same
    // one line as above, plus the two overrides a user who wants
    // chonkstep's furniture back would write.
    let mut session = Session::boot(
        "omarchy-mode-overridden",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "desktop = \"omarchy\"\nshow_dock = true\nkeymap = \"chonkstep\"\n".to_string(),
            ..SessionOptions::default()
        },
    )
    .unwrap();

    // The Dock is back, in its corner, reserving its strip.
    let dock = session.wait_for_dock_at(0, 0).expect("`show_dock = true` must beat the posture's default");

    // ...and so are the chonkstep chords, while the Omarchy ones are
    // gone: `keymap = "chonkstep"` took the whole table back, so
    // `super+w` is not merely bound to something else — it is unbound,
    // and the key falls through to the client.
    session.launch("foot", &[]).unwrap();
    session.wait_for_window("foot").expect("a client to close");
    session.door().chord(keys::LEFTMETA, KEY_W).unwrap();
    session.door().barrier().unwrap();
    assert!(
        session.world().unwrap().window_matching("foot").is_some(),
        "super+w must do nothing under the chonkstep keymap"
    );

    // The chonkstep chord does: alt+shift+q, two modifiers, so the
    // door's single-modifier `chord` does not fit.
    let door = session.door();
    door.key(keys::LEFTALT, true).unwrap();
    door.key(keys::LEFTSHIFT, true).unwrap();
    door.barrier().unwrap();
    door.tap_key(KEY_Q).unwrap();
    door.key(keys::LEFTSHIFT, false).unwrap();
    door.key(keys::LEFTALT, false).unwrap();
    door.barrier().unwrap();
    session.wait_for_window_gone("foot").expect("alt+shift+q must close it under the chonkstep keymap");

    // The posture's other choices are untouched by those two
    // overrides: still no remembered Dock choice (the config decided),
    // and the Dock is exactly where a chonkstep desk puts it.
    assert_eq!(dock.y, 0);
    assert!(!session.state_file("dock-visibility").exists());
}

/// `KEY_Q` from input-event-codes.h — chonkstep's own close chord.
const KEY_Q: u32 = 16;
