//! End-to-end coverage for the `Dock` row: hiding the Dock takes the
//! column off the screen *and* gives back the strip of desk it was
//! reserving, so a maximized window covers ground it could not reach a
//! moment earlier — and showing it again takes both back. The Clip, the
//! workspace tile in the opposite corner, leaves and returns with it:
//! one visibility choice, every piece of desk furniture it owns.
//!
//! # Why this test and not a unit test
//!
//! The arithmetic is pinned by unit tests in `chonk_shell::desktop`
//! (`hiding_the_dock_unmaps_it_and_gives_its_column_back_to_the_
//! workarea`). What those cannot see is the seam this whole crate
//! exists for: the workarea has to actually reach `wm-core`, and
//! `wm-core` has to actually reflow the windows already maximized
//! against the old one. A hidden Dock that still reserved its strip
//! would pass every unit test, look perfectly right in a screenshot,
//! and leave one tile of dead screen down the right edge that nobody
//! discovers until they maximize something — which is the exact shape
//! of bug the seam produces and only a real client on a real socket
//! catches.
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest
//! in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use chonk_testkit::{keys, poll_until, session_dir, MenuMetrics, RootMenu, Session, SessionOptions};
use std::time::Duration;

/// The root menu on the harness's default session: no hosted shell, so
/// no `Omarchy Bar` row, and an empty `OMARCHY_PATH` so no `Omarchy`
/// submenu either. The `Dock` row is unconditional.
const PLAIN: RootMenu = RootMenu { omarchy_bar: false, omarchy: false };

/// Toggles maximize on the focused window with the default binding.
/// Two modifiers, so the door's single-modifier `chord` does not fit.
fn toggle_maximize(session: &mut Session) {
    let door = session.door();
    door.key(keys::LEFTALT, true).unwrap();
    door.key(keys::LEFTSHIFT, true).unwrap();
    door.barrier().unwrap();
    door.tap_key(keys::X).unwrap();
    door.key(keys::LEFTSHIFT, false).unwrap();
    door.key(keys::LEFTALT, false).unwrap();
    door.barrier().unwrap();
}

/// Picks the `Dock` row of an already-open root menu, by its label.
fn hide_dock_from_menu(session: &mut Session, metrics: &MenuMetrics, menu: &chonk_testkit::ShellInfo) {
    let (x, y) = metrics.row_center(menu, PLAIN.row_of("Dock").unwrap());
    session.door().click(x, y).unwrap();
    poll_until(Duration::from_secs(10), "the root menu to close after the pick", || {
        let world = session.world().ok()?;
        world.menus().is_empty().then_some(())
    })
    .expect("picking a row closes the menu");
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn hiding_the_dock_from_the_root_menu_gives_a_maximized_window_its_strip() {
    // An empty Omarchy root, so the menu's row count is exact whatever
    // this machine has installed under /usr/share/omarchy.
    let no_omarchy = session_dir("dock-toggle-omarchy");
    let _ = std::fs::remove_dir_all(&no_omarchy);
    std::fs::create_dir_all(&no_omarchy).unwrap();
    let mut session = Session::boot(
        "dock-toggle",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "[keybindings]\n\"super+d\" = \"toggle-dock\"\n".to_string(),
            env: vec![("OMARCHY_PATH".to_string(), no_omarchy.to_string_lossy().into_owned())],
            ..SessionOptions::default()
        },
    )
    .unwrap();
    let metrics = MenuMetrics::at_scale_1();

    // -- the Dock in its corner, and a window maximized short of it ----
    let world = session.world().unwrap();
    let output_w = world.output_w;
    let output_h = world.output_h;
    let home = world.dock().expect("the dock is in its corner on a fresh desk").clone();
    assert_eq!(home.y, 0);
    assert_eq!(home.x + home.w as i32, output_w as i32, "flush against the right edge");
    assert!(!session.state_file("dock-visibility").exists(), "no choice made yet, so none is stored");

    // Open the root menu while there is still bare desktop to click.
    // Once the window below is maximized there is intentionally no
    // background under the pointer, so asking a later right-click to
    // open the root menu would send that click to the client instead.
    let menu = session
        .open_root_menu(&metrics, PLAIN.row_count())
        .expect("a right-click on bare desk should open the root menu with a Dock row");

    session.launch("foot", &[]).unwrap();
    let window = session.wait_for_window("foot").unwrap();
    toggle_maximize(&mut session);
    let short = poll_until(Duration::from_secs(10), "the frame to stop at the dock's column", || {
        let world = session.world().ok()?;
        world.frame_of(window.id).filter(|f| f.x + f.w as i32 == home.x).cloned()
    })
    .expect("a maximized window must stop where the dock column begins");
    assert_eq!(short.x, 0, "and start at the left edge");

    // -- the menu hides it: the surface goes, the strip is released ----
    hide_dock_from_menu(&mut session, &metrics, &menu);
    poll_until(Duration::from_secs(10), "the dock surface to be gone", || {
        let world = session.world().ok()?;
        // Shape-based, like `World::dock` itself: nothing mapped,
        // taller than wide, flush against the right edge. An unmapped
        // surface is not merely undrawn — it is out of the hit test
        // too, so a click in that corner reaches the window under it.
        world.dock().is_none().then_some(())
    })
    .expect("hiding the dock unmaps its surface");
    // The load-bearing assertion of the whole feature: the maximized
    // window, untouched since it was maximized, grows into the tile
    // the dock used to hold. This is what a reservation nobody
    // released would silently refuse to do.
    let full = poll_until(Duration::from_secs(10), "the maximized frame to reach the right edge", || {
        let world = session.world().ok()?;
        world.frame_of(window.id).filter(|f| f.x + f.w as i32 == output_w as i32).cloned()
    })
    .expect("a hidden dock reserves nothing, so the window takes the strip");
    assert_eq!(full.w, short.w + home.w, "exactly one dock column wider than before");
    assert_eq!((full.x, full.y, full.h), (short.x, short.y, short.h), "and nothing else moved");
    assert_eq!(std::fs::read_to_string(session.state_file("dock-visibility")).unwrap().trim(), "hidden");

    // The Clip goes with it. This assertion used to read the other
    // way — "the Clip keeps its own corner", on the argument that it is
    // corner furniture of its own and hiding the Dock is not "hide
    // everything in the corner". That argument does not survive the
    // session this feature exists for: under a hosted Omarchy bar the
    // Clip is the last chonkstep object on a borrowed screen, showing a
    // workspace number the bar is already showing. `World::dock`'s
    // taller-than-wide shape test cannot see a square tile, so the
    // check is written out by hand here.
    poll_until(Duration::from_secs(10), "the Clip to leave with the Dock", || {
        let world = session.world().ok()?;
        world
            .shells
            .iter()
            .all(|s| !(s.mapped && s.w == s.h && s.x + s.w as i32 == output_w as i32 && s.y + s.h as i32 == output_h as i32))
            .then_some(())
    })
    .expect("hiding the Dock takes the Clip with it");

    // -- and the equivalent binding shows it again --------------------
    // With the window now covering the entire output there is still no
    // bare desktop on which to summon a root menu. The public action is
    // the same one used by the row, and the second test below separately
    // pins the configured-off startup path.
    session.door().chord(keys::LEFTMETA, KEY_D).unwrap();
    let back = session.wait_for_dock_at(0, 0).expect("showing the dock maps it back into its corner");
    assert_eq!((back.x, back.y, back.w, back.h), (home.x, home.y, home.w, home.h), "exactly where it started");
    poll_until(Duration::from_secs(10), "the maximized frame to give the column back", || {
        let world = session.world().ok()?;
        world.frame_of(window.id).filter(|f| f.x + f.w as i32 == home.x).cloned()
    })
    .expect("the window yields the strip the moment the dock reserves it again");
    poll_until(Duration::from_secs(10), "the Clip to come back to its corner", || {
        let world = session.world().ok()?;
        world
            .shells
            .iter()
            .any(|s| s.mapped && s.w == s.h && s.x + s.w as i32 == output_w as i32 && s.y + s.h as i32 == output_h as i32)
            .then_some(())
    })
    .expect("showing the Dock brings the Clip back");
    assert_eq!(std::fs::read_to_string(session.state_file("dock-visibility")).unwrap().trim(), "shown");
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_session_configured_dockless_never_shows_the_dock_and_a_binding_brings_it_back() {
    // `show_dock = false` is the configuration chonkstep is offered to
    // Omarchy as: the whole width of the screen from the first frame,
    // with no column ever mapped. Plus a key for the verb, since the
    // action is deliberately unbound by default.
    let mut session = Session::boot(
        "dock-configured-off",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "show_dock = false\n\n[keybindings]\n\"super+d\" = \"toggle-dock\"\n".to_string(),
            ..SessionOptions::default()
        },
    )
    .unwrap();

    let world = session.world().unwrap();
    let output_w = world.output_w;
    let output_h = world.output_h;
    assert!(world.dock().is_none(), "a session configured dockless has no dock on screen");
    // Nor a Clip. This is the boot path rather than the toggle, and it
    // is the one that could differ: `Desktop::new` maps both surfaces
    // unconditionally and the shell hides them a moment later through
    // the resolved choice. A startup that reached the Dock by some
    // other route would leave the Clip lit on a dockless desk.
    assert!(
        !world.shells.iter().any(|s| s.mapped && s.w == s.h && s.x + s.w as i32 == output_w as i32
            && s.y + s.h as i32 == output_h as i32),
        "nor a Clip: one choice hides every piece of desk furniture"
    );

    // A window maximizes across the whole width from the start — the
    // startup path composed the workareas from a Dock that was already
    // hidden, rather than reserving a strip and giving it back later.
    session.launch("foot", &[]).unwrap();
    let window = session.wait_for_window("foot").unwrap();
    let door = session.door();
    door.key(keys::LEFTALT, true).unwrap();
    door.key(keys::LEFTSHIFT, true).unwrap();
    door.barrier().unwrap();
    door.tap_key(keys::X).unwrap();
    door.key(keys::LEFTSHIFT, false).unwrap();
    door.key(keys::LEFTALT, false).unwrap();
    door.barrier().unwrap();
    let full = poll_until(Duration::from_secs(10), "the frame to reach the right edge", || {
        let world = session.world().ok()?;
        world.frame_of(window.id).filter(|f| f.x + f.w as i32 == output_w as i32).cloned()
    })
    .expect("with no dock the workarea is the whole monitor from the first frame");
    assert_eq!(full.x, 0, "the whole width, corner to corner");

    // The binding brings it back: `toggle-dock` on a key does exactly
    // what the menu row does, both halves included.
    session.door().chord(keys::LEFTMETA, KEY_D).unwrap();
    let dock = session.wait_for_dock_at(0, 0).expect("the binding maps the dock into its corner");
    poll_until(Duration::from_secs(10), "the maximized frame to yield the column", || {
        let world = session.world().ok()?;
        world.frame_of(window.id).filter(|f| f.x + f.w as i32 == dock.x).cloned()
    })
    .expect("and reserves its strip, which the maximized window must yield");
    assert_eq!(std::fs::read_to_string(session.state_file("dock-visibility")).unwrap().trim(), "shown");
}

/// `KEY_D` from input-event-codes.h — the one keycode this test needs
/// that `chonk_testkit::keys` has no reason to carry.
const KEY_D: u32 = 32;
