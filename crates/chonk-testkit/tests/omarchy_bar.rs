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

use chonk_testkit::{poll_until, profile_binary, strip_ansi, Screenshot, Session, SessionOptions, ShellInfo, World};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The bar's thickness in the nested output's pixels (scale 1).
const BAR: u32 = 48;

/// The fake bar's fill as a screenshot reads it (`BAR_ORANGE` is
/// little-endian ARGB, so the bytes come back reversed).
const ORANGE: [f64; 3] = [0xE0 as f64, 0x70 as f64, 0x10 as f64];

/// The root menu's rows on a desk with a hosted shell and no Omarchy
/// menu definition: Terminal, Applications, Theme, Wallpaper, Omarchy
/// Bar, Exit. The bar row's index is what the test clicks.
const ROOT_ROWS: usize = 6;
const OMARCHY_BAR_ROW: usize = 4;

/// Menu geometry at scale 1, from the same theme the compositor wears
/// by default — the arithmetic `omarchy_menu.rs` uses.
struct MenuMetrics {
    border: u32,
    title_h: u32,
    item_h: u32,
}

impl MenuMetrics {
    fn at_scale_1() -> Self {
        let theme = wm_theme::default_theme::nextstep_classic();
        let item_h = (theme.menu.item_height as u32).max(4);
        Self { border: (theme.border.width as u32).max(1), title_h: (theme.titlebar.height as u32).max(item_h), item_h }
    }

    fn height_for(&self, rows: usize) -> u32 {
        self.title_h + self.item_h * rows as u32 + self.border * 2
    }

    fn row_center(&self, menu: &ShellInfo, row: usize) -> (f64, f64) {
        let y = menu.y as u32 + self.border + self.title_h + self.item_h * row as u32 + self.item_h / 2;
        (menu.x as f64 + menu.w as f64 / 2.0, y as f64)
    }
}

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

/// The Dock, found by shape: the mapped column flush with the right
/// edge (see `layer_bar.rs`).
fn dock_column(world: &World) -> Option<&ShellInfo> {
    world.shells.iter().find(|s| s.mapped && s.h > s.w && s.x + s.w as i32 == world.output_w as i32)
}

fn wait_for_dock_at(session: &mut Session, top: i32) -> ShellInfo {
    poll_until(Duration::from_secs(10), &format!("the dock to hang at y={top}"), || {
        let world = session.world().ok()?;
        dock_column(&world).filter(|dock| dock.y == top).cloned()
    })
    .expect("the dock should follow the bar's reservation")
}

/// Mapped menu surfaces: raised shells not flush against the right
/// edge, where the Dock and launcher strip live.
fn menus(world: &World) -> Vec<ShellInfo> {
    world.shells.iter().filter(|s| s.mapped && s.above && s.x + (s.w as i32) < world.output_w as i32).cloned().collect()
}

/// Right-clicks the desk and returns the root menu once it maps with
/// the expected row count.
fn open_root_menu(session: &mut Session, metrics: &MenuMetrics) -> ShellInfo {
    let world = session.world().unwrap();
    let (x, y) = (world.output_w as f64 / 2.0, world.output_h as f64 * 0.75);
    let door = session.door();
    door.motion(x, y).unwrap();
    door.barrier().unwrap();
    door.button("right", true).unwrap();
    door.barrier().unwrap();
    door.button("right", false).unwrap();
    door.barrier().unwrap();
    let wanted = metrics.height_for(ROOT_ROWS);
    poll_until(Duration::from_secs(10), &format!("the root menu ({ROOT_ROWS} rows, {wanted}px) to map"), || {
        let world = door.windows().ok()?;
        menus(&world).into_iter().find(|m| m.h == wanted)
    })
    .expect("a right-click on the desk should open the root menu with an Omarchy Bar row")
}

/// Picks the `Omarchy Bar` row of an open root menu.
fn toggle_bar_from_menu(session: &mut Session, metrics: &MenuMetrics) {
    let menu = open_root_menu(session, metrics);
    let (x, y) = metrics.row_center(&menu, OMARCHY_BAR_ROW);
    session.door().click(x, y).unwrap();
    poll_until(Duration::from_secs(10), "the root menu to close after the pick", || {
        let world = session.world().ok()?;
        menus(&world).is_empty().then_some(())
    })
    .expect("picking a row closes the menu");
}

/// The mean colour of the top strip, inset from the corners where the
/// Clip and the Dock live.
fn top_strip(shot: &Screenshot) -> [f64; 3] {
    shot.mean_rgb(shot.width / 4, 4, shot.width / 2, BAR - 8)
}

fn near(actual: [f64; 3], expected: [f64; 3]) -> bool {
    actual.iter().zip(expected).all(|(a, e)| (a - e).abs() < 12.0)
}

fn wait_for_strip(session: &mut Session, label: &str, orange: bool) {
    poll_until(Duration::from_secs(10), &format!("the top strip to be {}", if orange { "the bar" } else { "the desk" }), || {
        let shot = session.screenshot(label).ok()?;
        (near(top_strip(&shot), ORANGE) == orange).then_some(())
    })
    .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn omarchys_bar_is_hidden_until_the_root_menu_shows_it() {
    // The root is written beside the session directory, which
    // `Session::boot` clears.
    let session_dir = std::env::temp_dir().join("chonk-testkit").join("omarchy-bar-omarchy");
    let _ = std::fs::remove_dir_all(&session_dir);
    std::fs::create_dir_all(&session_dir).unwrap();
    let root = write_omarchy_root(&session_dir);
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
        // The compositor colours its log; the field text is only
        // matchable once the escapes are stripped.
        let log = strip_ansi(&session.log());
        (log.contains("hosting Omarchy's shell") && log.contains("namespace=omarchy-bar mapped=true")).then_some(())
    })
    .expect("the fake launcher should be run and its bar should map");
    let home = wait_for_dock_at(&mut session, 0);
    wait_for_strip(&mut session, "hidden", false);
    assert!(!session.state_file("omarchy-bar").exists(), "no choice has been made yet, so none is stored");

    // -- the menu shows it: the strip paints, the dock steps down -----------
    toggle_bar_from_menu(&mut session, &metrics);
    let under = wait_for_dock_at(&mut session, BAR as i32);
    assert_eq!((under.x, under.w), (home.x, home.w), "the column moves down, not sideways");
    wait_for_strip(&mut session, "shown", true);
    assert_eq!(std::fs::read_to_string(session.state_file("omarchy-bar")).unwrap().trim(), "shown");

    // -- and hides it again: the corner comes back --------------------------
    toggle_bar_from_menu(&mut session, &metrics);
    let back = wait_for_dock_at(&mut session, 0);
    assert_eq!((back.x, back.w, back.h), (home.x, home.w, home.h));
    wait_for_strip(&mut session, "hidden-again", false);
    assert_eq!(std::fs::read_to_string(session.state_file("omarchy-bar")).unwrap().trim(), "hidden");
}
