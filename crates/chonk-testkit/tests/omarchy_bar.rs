//! End-to-end coverage for the `Omarchy Bar` row: a session that hosts
//! Omarchy's shell keeps the shell's bar off the screen until the user
//! switches it on from the root menu, and remembers the choice.
//!
//! The real shell is a Quickshell process a test cannot reasonably
//! boot, so the test stands up an Omarchy root of its own whose
//! `omarchy-launch-shell` is a two-line script that runs
//! `chonk-fake-bar` under the bar's namespace. The compositor cannot
//! tell the difference: it checks for the two files the launcher needs,
//! runs the launcher by its path, and a top-anchored layer surface
//! called `omarchy-bar` turns up a moment later — exactly what the real
//! shell does, minus the clock.
//!
//! What is pinned: the bar maps but is not shown and reserves nothing
//! (the Dock stays in its corner); the root menu carries an `Omarchy
//! Bar` row; picking it shows the bar (the strip paints, the Dock steps
//! down) and writes the choice to chonkstep's own state; picking it
//! again hides the bar and takes the strip back.
//!
//! Same run rules as `e2e.rs`: `#[ignore]`d, run with `scripts/e2e.sh`
//! or `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use chonk_testkit::{
    near, poll_until, profile_binary, session_dir, MenuMetrics, RootMenu, Screenshot, Session, SessionOptions,
    FAKE_BAR_RGB,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The bar's thickness in the nested output's pixels (scale 1).
const BAR: u32 = 48;

/// The root menu on a desk with a hosted shell and no Omarchy menu
/// definition (the scratch root below carries none): the bar toggle is
/// listed, the Omarchy submenu is not.
const HOSTED: RootMenu = RootMenu { omarchy_bar: true, omarchy: false };

/// Writes the Omarchy root the compositor will find: the QML file the
/// launcher would hand Quickshell (never read here) and a launcher that
/// runs the fake bar under Omarchy's namespace.
fn write_omarchy_root(session_dir: &Path) -> PathBuf {
    let root = session_dir.join("omarchy");
    std::fs::create_dir_all(root.join("shell")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("shell/shell.qml"), "// stands in for Omarchy's shell\n").unwrap();
    let bar = profile_binary("chonk-fake-bar").expect("cargo build -p chonk-testkit builds the bar");
    let launcher = root.join("bin/omarchy-launch-shell");
    std::fs::write(&launcher, format!("#!/bin/bash\nexec '{}' {BAR} top omarchy-bar\n", bar.display())).unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    root
}

/// Picks the `Omarchy Bar` row of the root menu, by its label.
fn toggle_bar_from_menu(session: &mut Session, metrics: &MenuMetrics) {
    let menu = session
        .open_root_menu(metrics, HOSTED.row_count())
        .expect("a right-click on the desk should open the root menu with an Omarchy Bar row");
    let (x, y) = metrics.row_center(&menu, HOSTED.row_of("Omarchy Bar").unwrap());
    session.door().click(x, y).unwrap();
    poll_until(Duration::from_secs(10), "the root menu to close after the pick", || {
        let world = session.world().ok()?;
        world.menus().is_empty().then_some(())
    })
    .expect("picking a row closes the menu");
}

/// The mean colour of the top strip, inset from the corners where the
/// Clip and the Dock live.
fn top_strip(shot: &Screenshot) -> [f64; 3] {
    shot.mean_rgb(shot.width / 4, 4, shot.width / 2, BAR - 8)
}

fn wait_for_strip(session: &mut Session, label: &str, orange: bool) {
    poll_until(Duration::from_secs(10), &format!("the top strip to be {}", if orange { "the bar" } else { "the desk" }), || {
        let shot = session.screenshot(label).ok()?;
        (near(top_strip(&shot), FAKE_BAR_RGB) == orange).then_some(())
    })
    .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn omarchys_bar_is_hidden_until_the_root_menu_shows_it() {
    // The root is written beside the session directory, which
    // `Session::boot` clears.
    let beside = session_dir("omarchy-bar-omarchy");
    let _ = std::fs::remove_dir_all(&beside);
    std::fs::create_dir_all(&beside).unwrap();
    let root = write_omarchy_root(&beside);
    let mut session = Session::boot(
        "omarchy-bar",
        SessionOptions {
            scale: Some(1.0),
            omarchy_shell: true,
            env: vec![("OMARCHY_PATH".to_string(), root.to_string_lossy().into_owned())],
            ..SessionOptions::default()
        },
    )
    .unwrap();
    let metrics = MenuMetrics::at_scale_1();

    // -- the shell is hosted and its bar maps, unseen -----------------------
    poll_until(Duration::from_secs(20), "the compositor to host the shell and the bar to map", || {
        // Two log lines: `chonk_shell::shell::host_omarchy_shell`
        // announcing the launch, and the layer-shell "layer surface
        // map state changed" record `wm_wayland::layers::handle_commit`
        // writes with its `namespace` and `mapped` fields — the second
        // is matched on those field names, so a renamed field lands
        // here.
        let log = session.log();
        (log.contains("hosting Omarchy's shell") && log.contains("namespace=omarchy-bar mapped=true")).then_some(())
    })
    .expect("the fake launcher should be run and its bar should map");
    let home = session.wait_for_dock_at(0, 0).expect("a hidden bar reserves nothing, so the dock stays in its corner");
    wait_for_strip(&mut session, "hidden", false);
    assert!(!session.state_file("omarchy-bar").exists(), "no choice has been made yet, so none is stored");

    // -- the menu shows it: the strip paints, the dock steps down -----------
    toggle_bar_from_menu(&mut session, &metrics);
    let under = session.wait_for_dock_at(0, BAR as i32).expect("the dock should follow the bar's reservation");
    assert_eq!((under.x, under.w), (home.x, home.w), "the column moves down, not sideways");
    wait_for_strip(&mut session, "shown", true);
    assert_eq!(std::fs::read_to_string(session.state_file("omarchy-bar")).unwrap().trim(), "shown");

    // -- and hides it again: the corner comes back --------------------------
    toggle_bar_from_menu(&mut session, &metrics);
    let back = session.wait_for_dock_at(0, 0).expect("hiding the bar gives the corner back");
    assert_eq!((back.x, back.w, back.h), (home.x, home.w, home.h));
    wait_for_strip(&mut session, "hidden-again", false);
    assert_eq!(std::fs::read_to_string(session.state_file("omarchy-bar")).unwrap().trim(), "hidden");
}
