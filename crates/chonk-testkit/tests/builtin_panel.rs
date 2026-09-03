//! End-to-end coverage for the *built-in* instrument panel: boot the
//! real nested compositor with the in-process panel probe enabled
//! (`CHONKSTEP_TEST_PANEL_TILE`, the built-in twin of
//! `chonk-panel-probe`), right-click its tile through the injection
//! door, and assert the observable outcomes — the framed panel surface
//! appearing beside the dock in the ledger, the probe's rendered
//! pixels inside the shell's chiseled chrome in a real screenshot, the
//! input round trip as a color change, Escape taking it down, and the
//! right-click toggle closing what it opened.
//!
//! The point of the file is the symmetry: everything asserted here is
//! what `instrument_panel.rs` asserts for a remote dockapp's panel,
//! reached through the built-in gesture (right-click) instead of a
//! client's `OpenPanel` — same surface, same chrome, same dismissals.
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest
//! in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use chonk_testkit::{keys, poll_until, Screenshot, Session, SessionOptions, ShellInfo, World};
use std::time::Duration;

/// The probe's spec (`chonk-shell`'s `widgets::panel_probe`); the
/// workarea on the nested output is larger, so the grant comes back
/// verbatim.
const PANEL: (u32, u32) = (300, 200);

/// The open panel in the ledger: a mapped, above surface whose right
/// edge touches the dock's left edge, sized like the grant plus chrome
/// — nothing else the shell raises sits flush against the dock.
fn panel_shell(world: &World) -> Option<&ShellInfo> {
    let dock = world.dock()?;
    world.shells.iter().find(|s| {
        s.mapped
            && s.above
            && s.x + s.w as i32 == dock.x
            && s.w >= PANEL.0
            && s.w <= PANEL.0 + 40
            && s.h >= PANEL.1
            && s.h <= PANEL.1 + 40
    })
}

/// Mean RGB of the panel's content center — inside the chrome by a
/// wide margin, so the sample is the probe's pixels alone.
fn panel_center_rgb(shot: &Screenshot, panel: &ShellInfo) -> [f64; 3] {
    shot.mean_rgb(
        (panel.x + panel.w as i32 / 2 - 8).max(0) as u32,
        (panel.y + panel.h as i32 / 2 - 8).max(0) as u32,
        16,
        16,
    )
}

fn dominant(mean: [f64; 3], channel: usize) -> bool {
    (0..3).all(|other| other == channel || mean[channel] > mean[other] + 50.0)
}

/// The whole built-in panel lifecycle in one boot: right-click open,
/// see the rendered pixels in the frame, click inside (input round
/// trip as a color change), Escape to dismiss, right-click to reopen
/// and again to toggle closed.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_builtin_panel_opens_on_right_click_repaints_on_input_and_dismisses() {
    let mut session = Session::boot(
        "builtin-panel",
        SessionOptions {
            env: vec![("CHONKSTEP_TEST_PANEL_TILE".to_string(), "1".to_string())],
            ..SessionOptions::default()
        },
    )
    .unwrap();
    session.door().barrier().unwrap();

    // The probe's tile: the identity tile plus the seven built-in
    // instruments sit above it, so it is the eighth slot down the
    // column (default order; this session has no remembered one, and
    // registers no dockapps).
    let world = session.world().unwrap();
    let dock = world.dock().expect("the dock is in the ledger").clone();
    let tile = (56.0 * world.scale).round() as i32;
    let (tile_x, tile_y) = (dock.x as f64 + tile as f64 / 2.0, (tile * 8) as f64 + tile as f64 / 2.0);

    // -- right-click the tile: the panel unfolds beside the dock ---------
    session.door().right_click(tile_x, tile_y).unwrap();
    let opened = {
        let door = session.door();
        poll_until(Duration::from_secs(10), "the built-in panel to appear beside the dock", || {
            let world = door.windows().ok()?;
            panel_shell(&world).cloned()
        })
        .expect("right-clicking the probe's tile should open its panel")
    };
    assert_eq!(opened.w - PANEL.0, opened.h - PANEL.1, "the chrome border is uniform");
    assert!(opened.w > PANEL.0, "and the shell draws it around the granted content");

    // -- the rendered pixels are on screen, framed ------------------------
    // Green must be *visible*, not merely rendered: poll a real
    // screenshot until the probe's color is there (the first present
    // can trail the ledger entry by a pass or two).
    session.door().barrier().unwrap();
    let green_shot = poll_until(Duration::from_secs(10), "the probe's green panel pixels to reach the screen", || {
        let shot = session.screenshot("builtin-panel-open").ok()?;
        dominant(panel_center_rgb(&shot, &opened), 1).then_some(shot)
    })
    .expect("the panel should show the probe's rendered green");
    // The chrome is the shell's: at the surface corner, the pixels are
    // theme chrome, not the probe's green.
    let border = green_shot.mean_rgb(opened.x.max(0) as u32, opened.y.max(0) as u32, 3, 3);
    assert!(!dominant(border, 1), "the border is the desktop's chiseled frame, not widget pixels");

    // -- input round trip: a click inside turns it red --------------------
    let (cx, cy) = (opened.x as f64 + opened.w as f64 / 2.0, opened.y as f64 + opened.h as f64 / 2.0);
    session.door().click(cx, cy).unwrap();
    poll_until(Duration::from_secs(10), "the panel to repaint red after the panel click", || {
        let shot = session.screenshot("builtin-panel-after-click").ok()?;
        dominant(panel_center_rgb(&shot, &opened), 0).then_some(())
    })
    .expect("a click inside the panel should reach the widget and come back as a red repaint");

    // -- Escape dismisses -------------------------------------------------
    session.door().tap_key(keys::ESC).unwrap();
    {
        let door = session.door();
        poll_until(Duration::from_secs(10), "the panel to leave the ledger after Escape", || {
            let world = door.windows().ok()?;
            panel_shell(&world).is_none().then_some(())
        })
        .expect("Escape should dismiss the built-in panel");
    }
    session.screenshot("builtin-panel-dismissed").unwrap();
    assert!(session.compositor_alive(), "and the desktop shrugs it all off");

    // -- toggle: right-click reopens, and again closes --------------------
    session.door().right_click(tile_x, tile_y).unwrap();
    {
        let door = session.door();
        poll_until(Duration::from_secs(10), "the panel to reopen on a tile right-click", || {
            let world = door.windows().ok()?;
            panel_shell(&world).map(|_| ())
        })
        .expect("the right-click should reopen the panel");
    }
    session.door().right_click(tile_x, tile_y).unwrap();
    {
        let door = session.door();
        poll_until(Duration::from_secs(10), "the panel to close on the toggle right-click", || {
            let world = door.windows().ok()?;
            panel_shell(&world).is_none().then_some(())
        })
        .expect("right-clicking the owning tile again is the toggle");
    }
    assert!(session.compositor_alive());
}
