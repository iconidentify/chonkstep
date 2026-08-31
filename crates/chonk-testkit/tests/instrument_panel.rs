//! End-to-end coverage for the instrument panel: boot the real nested
//! compositor with a real dockapp registered (`chonk-panel-probe`,
//! this crate's own scripted instrument), click its tile through the
//! injection door, and assert the observable outcomes — the framed
//! panel surface appearing beside the dock in the ledger, the probe's
//! streamed pixels (banded, 600x400 — bigger than any single datagram)
//! inside the shell's chiseled chrome in a real screenshot, the input
//! round trip as a color change, and Escape taking it down.
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest
//! in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use chonk_testkit::{poll_until, profile_binary, Screenshot, Session, SessionOptions, ShellInfo, World};
use std::time::Duration;

// evdev keycode (input-event-codes.h), what the door's `key` speaks.
const KEY_ESC: u32 = 1;

/// The probe's request; the workarea on the nested output is larger,
/// so the grant comes back verbatim.
const PANEL: (u32, u32) = (600, 400);

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

/// The whole panel lifecycle in one boot: register, click open, see
/// the streamed pixels in the frame, click inside (input round trip as
/// a color change), Escape to dismiss.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_dockapp_panel_opens_streams_receives_input_and_dismisses() {
    let probe = profile_binary("chonk-panel-probe").expect("cargo build -p chonk-testkit builds the probe");
    let registration = format!(
        "id = \"panel-probe\"\nname = \"PRB\"\nexec = [{:?}]\nrestart = \"on-crash\"\n",
        probe.display().to_string()
    );
    let mut session = Session::boot(
        "instrument-panel",
        SessionOptions {
            config_files: vec![("dockapps/panel-probe.dockapp".to_string(), registration)],
            ..SessionOptions::default()
        },
    )
    .unwrap();

    // -- the probe connects and draws its tile --------------------------
    {
        let log_probe = |session: &Session| session.log().contains("dockapp connected");
        poll_until(Duration::from_secs(15), "the probe dockapp to connect", || log_probe(&session).then_some(()))
            .expect("the registered dockapp should be launched and admitted");
    }
    session.door().barrier().unwrap();

    // The probe's tile: the identity tile plus the six built-in
    // instruments sit above it, so it is the seventh slot down the
    // column (default order; this session has no remembered one).
    let world = session.world().unwrap();
    let dock = world.dock().expect("the dock is in the ledger").clone();
    let tile = (56.0 * world.scale).round() as i32;
    let (tile_x, tile_y) = (dock.x as f64 + tile as f64 / 2.0, (tile * 7) as f64 + tile as f64 / 2.0);

    // -- click the tile: the panel unfolds beside the dock ---------------
    session.door().click(tile_x, tile_y).unwrap();
    let opened = {
        let door = session.door();
        poll_until(Duration::from_secs(10), "the instrument panel to appear beside the dock", || {
            let world = door.windows().ok()?;
            panel_shell(&world).cloned()
        })
        .expect("clicking the probe's tile should open its panel")
    };
    assert_eq!(opened.w - PANEL.0, opened.h - PANEL.1, "the chrome border is uniform");
    assert!(opened.w > PANEL.0, "and the shell draws it around the granted content");

    // -- the streamed pixels are on screen, framed -----------------------
    // Green must be *visible*, not merely sent: poll a real screenshot
    // until the streamed color is there (the first present can trail
    // the ledger entry by a pass or two).
    session.door().barrier().unwrap();
    let green_shot = poll_until(Duration::from_secs(10), "the probe's green panel pixels to reach the screen", || {
        let shot = session.screenshot("panel-open").ok()?;
        dominant(panel_center_rgb(&shot, &opened), 1).then_some(shot)
    })
    .expect("the panel should show the probe's streamed green");
    // The chrome is the shell's: just outside the content inset, the
    // pixels are theme chrome, not the probe's green.
    let border = green_shot.mean_rgb(opened.x.max(0) as u32, opened.y.max(0) as u32, 3, 3);
    assert!(!dominant(border, 1), "the border is the desktop's chiseled frame, not client pixels");

    // -- input round trip: a click inside turns it red -------------------
    let (cx, cy) = (opened.x as f64 + opened.w as f64 / 2.0, opened.y as f64 + opened.h as f64 / 2.0);
    session.door().click(cx, cy).unwrap();
    poll_until(Duration::from_secs(10), "the panel to repaint red after PanelInput", || {
        let shot = session.screenshot("panel-after-click").ok()?;
        dominant(panel_center_rgb(&shot, &opened), 0).then_some(())
    })
    .expect("a click inside the panel should reach the probe and come back as a red repaint");

    // -- Escape dismisses -------------------------------------------------
    session.door().tap_key(KEY_ESC).unwrap();
    {
        let door = session.door();
        poll_until(Duration::from_secs(10), "the panel to leave the ledger after Escape", || {
            let world = door.windows().ok()?;
            panel_shell(&world).is_none().then_some(())
        })
        .expect("Escape should dismiss the panel");
    }
    session.screenshot("panel-dismissed").unwrap();
    assert!(session.compositor_alive(), "and the desktop shrugs it all off");

    // -- toggle: the tile re-click opens and closes -----------------------
    session.door().click(tile_x, tile_y).unwrap();
    {
        let door = session.door();
        poll_until(Duration::from_secs(10), "the panel to reopen on a tile click", || {
            let world = door.windows().ok()?;
            panel_shell(&world).map(|_| ())
        })
        .expect("the tile click should reopen the panel");
    }
    session.door().click(tile_x, tile_y).unwrap();
    {
        let door = session.door();
        poll_until(Duration::from_secs(10), "the panel to close on the toggle re-click", || {
            let world = door.windows().ok()?;
            panel_shell(&world).is_none().then_some(())
        })
        .expect("re-clicking the owning tile is the toggle");
    }
    assert!(session.compositor_alive());
}
