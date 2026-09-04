//! End-to-end coverage for the modal Overview: boot the real nested
//! compositor at scale 2 (the reference desk), launch two real
//! terminals, open the Overview with its actual keybinding through the
//! injection door, drive it with arrows/Return/Escape and the pointer,
//! and assert the observable outcomes — the full-screen panel in the
//! ledger, the commit's focus change read off the frames' own
//! titlebar pixels (focused titlebars are dark in every built-in
//! theme; that contrast *is* the focus indicator a user sees).
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest
//! in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use chonk_testkit::{keys, poll_until, Screenshot, Session, SessionOptions, ShellInfo, World};
use std::time::Duration;

/// The dock/Clip tile edge at scale 2 — `chonk_shell::desktop::tile_px`
/// restated (56 at 1x, scaled), the number the Overview's strip and
/// gutters derive from.
const TILE_AT_SCALE_2: u32 = 112;

/// The Overview's surface in the ledger: mapped, above, and exactly
/// output-sized — nothing else the shell raises covers the whole head.
fn overview_shell(world: &World) -> Option<&ShellInfo> {
    world
        .shells
        .iter()
        .find(|s| s.mapped && s.above && s.w == world.output_w && s.h == world.output_h)
}

/// Mean brightness of a frame's titlebar band — the focus indicator
/// itself: every built-in theme paints the focused titlebar darker
/// than the unfocused one (that contrast is the whole point of the
/// black/gray treatment).
fn titlebar_brightness(shot: &Screenshot, world: &World, needle: &str) -> f64 {
    let window = world.window_matching(needle).expect("window in ledger");
    let frame = world
        .frame_of(window.id)
        .expect("server-side frame (foot draws no CSD)");
    let bar_h = (window.y - frame.y).max(4) as u32;
    let mean = shot.mean_rgb(
        (frame.x + frame.w as i32 / 4).max(0) as u32,
        frame.y.max(0) as u32 + bar_h / 3,
        frame.w / 2,
        (bar_h / 3).max(2),
    );
    (mean[0] + mean[1] + mean[2]) / 3.0
}

/// Which of the two terminals wears the focused (dark) titlebar.
fn focused_of_two(session: &mut Session, a: &str, b: &str) -> String {
    session.door().barrier().unwrap();
    let world = session.world().unwrap();
    let shot = session.screenshot("focus-probe").unwrap();
    let (ba, bb) = (
        titlebar_brightness(&shot, &world, a),
        titlebar_brightness(&shot, &world, b),
    );
    assert!(
        (ba - bb).abs() > 15.0,
        "the two titlebars should be visibly focused-vs-unfocused (brightness {ba:.0} vs {bb:.0}, screenshot: {})",
        shot.path.display()
    );
    if ba < bb {
        a.to_string()
    } else {
        b.to_string()
    }
}

/// Launches a foot terminal and waits — generously — for it to map.
/// foot pays a cold fontconfig cache on first launch inside the
/// isolated session, which can outlast the harness's default
/// 10-second window wait on a debug build (observed: 22s from boot to
/// map); the condition is still observable, not a sleep.
fn launch_terminal(session: &mut Session, title: &str) {
    // `locked-title=yes` is load-bearing: without it the shell inside
    // the terminal overwrites the title with its prompt (via OSC)
    // before the ledger poll ever sees the one we asked for —
    // confirmed on a live ledger dump.
    session
        .launch(
            "foot",
            &[
                &format!("--title={title}"),
                // Keep both titlebars independently visible. At foot's
                // machine-dependent default size the two frames can
                // overlap almost completely, making a screenshot of
                // the lower frame sample the upper frame's pixels and
                // falsely report identical focus treatment.
                "--window-size-pixels=260x140",
                "--override",
                "locked-title=yes",
            ],
        )
        .unwrap();
    let needle = title.to_string();
    let door = session.door();
    poll_until(
        Duration::from_secs(40),
        &format!("a mapped window titled {needle:?}"),
        || {
            let world = door.windows().ok()?;
            world.window_matching(&needle).map(|_| ())
        },
    )
    .expect("foot should launch and map");
}

fn open_overview(session: &mut Session) {
    session.door().chord(keys::LEFTMETA, keys::UP).unwrap();
    let door = session.door();
    poll_until(
        Duration::from_secs(10),
        "the overview panel to appear in the ledger",
        || {
            let world = door.windows().ok()?;
            overview_shell(&world).map(|_| ())
        },
    )
    .expect("super+up should open the Overview");
}

fn assert_overview_closed(session: &mut Session, why: &str) {
    let door = session.door();
    poll_until(
        Duration::from_secs(10),
        "the overview panel to leave the ledger",
        || {
            let world = door.windows().ok()?;
            overview_shell(&world).is_none().then_some(())
        },
    )
    .unwrap_or_else(|e| panic!("the Overview should have closed after {why}: {e}"));
}

fn assert_overview_storage_released(session: &mut Session, surface_id: u64) {
    let world = session.world().unwrap();
    let shell = world.shells.iter().find(|shell| shell.id == surface_id).expect(
        "hiding Overview keeps its shell surface alive to avoid display-server churn",
    );
    assert!(!shell.mapped, "the hidden Overview surface remains unmapped");
    assert_eq!(
        shell.buffer_bytes, 0,
        "the hidden Overview surface must not retain monitor-sized pixels"
    );
}

/// The whole modal lifecycle in one boot (boots are the expensive
/// part): open with the real binding, arrow the selection, commit with
/// Return and watch focus actually move (titlebar pixels), reopen and
/// dismiss with Escape, reopen and commit by clicking a card aimed
/// with the same layout math the shell hit-tests through.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn overview_opens_navigates_and_commits() {
    let mut session = Session::boot(
        "overview",
        SessionOptions {
            scale: Some(2.0),
            ..SessionOptions::default()
        },
    )
    .unwrap();
    launch_terminal(&mut session, "OverviewA");
    launch_terminal(&mut session, "OverviewB");

    let before = focused_of_two(&mut session, "OverviewA", "OverviewB");

    // -- open, and look at it -------------------------------------------
    open_overview(&mut session);
    session.door().barrier().unwrap();
    let first_overview = {
        let world = session.world().unwrap();
        let shell = overview_shell(&world).expect("Overview surface is mapped");
        assert_eq!(
            shell.buffer_bytes,
            world.output_w as usize * world.output_h as usize * 4,
            "the diagnostic accounts for the live monitor-sized RGBA buffer"
        );
        shell.id
    };
    session.screenshot("overview-open").unwrap();

    // -- arrows move, Return commits ------------------------------------
    // The selection opens on the focused window's card; cards sit in
    // launch order (A, B), so stepping toward the *other* card is Left
    // when B is focused, Right when A is. The commit must then move
    // focus — observable as the dark titlebar changing windows.
    let step = if before == "OverviewB" {
        keys::LEFT
    } else {
        keys::RIGHT
    };
    session.door().tap_key(step).unwrap();
    session.screenshot("overview-selection-moved").unwrap();
    session.door().tap_key(keys::ENTER).unwrap();
    assert_overview_closed(&mut session, "Return committed the selection");
    assert_overview_storage_released(&mut session, first_overview);
    let after = focused_of_two(&mut session, "OverviewA", "OverviewB");
    assert_ne!(
        after, before,
        "committing the other card should move focus (dark titlebar)"
    );

    // -- Escape dismisses without committing ----------------------------
    open_overview(&mut session);
    let reopened = session.world().unwrap();
    assert_eq!(
        overview_shell(&reopened).expect("reopened Overview").id,
        first_overview,
        "reopening must reuse the surface whose pixels were released"
    );
    session.door().tap_key(keys::ESC).unwrap();
    assert_overview_closed(&mut session, "Escape");
    assert_overview_storage_released(&mut session, first_overview);
    let unchanged = focused_of_two(&mut session, "OverviewA", "OverviewB");
    assert_eq!(unchanged, after, "Escape must not move focus");

    // -- a click on a card focuses + raises that window and exits -------
    open_overview(&mut session);
    let world = session.world().unwrap();
    // Aim with the real layout math (same inputs the shell used:
    // panel = the output, tile 112 at scale 2, two cards, one desk).
    let theme = wm_theme::default_theme::nextstep_classic().scaled(2.0);
    let layout = wm_theme::overview::layout(
        wm_theme_api::Size::new(world.output_w, world.output_h),
        TILE_AT_SCALE_2,
        wm_theme::overview::header_height(&theme),
        2,
        1,
    );
    // Click the card of the window that is NOT focused: index 0 is A.
    let target_index = if after == "OverviewA" { 1 } else { 0 };
    let cell = layout.cells[target_index];
    let (cx, cy) = (
        cell.pos.x as f64 + cell.size.w as f64 / 2.0,
        cell.pos.y as f64 + cell.size.h as f64 / 2.0,
    );
    session.door().click(cx, cy).unwrap();
    assert_overview_closed(&mut session, "clicking a card");
    let clicked = focused_of_two(&mut session, "OverviewA", "OverviewB");
    assert_ne!(clicked, after, "clicking the other card should move focus");
    session.screenshot("after-click-commit").unwrap();
}

/// The N=0 quiet state: a workspace with no windows must show the
/// panel (header, empty-state line, workspace strip) rather than crash
/// or refuse — regression fence for the grid math's zero case reaching
/// the real render path.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn overview_on_an_empty_desk_is_quiet_not_a_crash() {
    let mut session = Session::boot(
        "overview-empty",
        SessionOptions {
            scale: Some(2.0),
            ..SessionOptions::default()
        },
    )
    .unwrap();
    open_overview(&mut session);
    session.door().barrier().unwrap();
    session.screenshot("overview-empty").unwrap();
    assert!(
        session.compositor_alive(),
        "an empty overview must not take the compositor down"
    );
    session.door().tap_key(keys::ESC).unwrap();
    assert_overview_closed(&mut session, "Escape on an empty desk");
    assert!(session.compositor_alive());
}
