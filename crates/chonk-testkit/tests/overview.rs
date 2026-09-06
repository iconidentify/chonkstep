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

use chonk_testkit::{keys, poll_until, profile_binary, Screenshot, Session, SessionOptions, ShellInfo, World};
use std::time::Duration;

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
            shell.buffer_bytes, 0,
            "the native Overview input surface must never allocate output-sized pixels"
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
    // Click the other window at its actual native presentation rectangle.
    let target = world
        .window_matching(if after == "OverviewA" {
            "OverviewB"
        } else {
            "OverviewA"
        })
        .unwrap();
    let cell = world
        .overview_windows
        .iter()
        .find(|w| w.id == target.id)
        .unwrap()
        .rect;
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

/// Visible pixels, not merely layout math: different window shapes, wallpaper
/// between them, live content without captures, modal input and storage bounds.
#[test]
#[ignore = "requires nested Wayland: scripts/e2e.sh --headless --test overview"]
fn native_overview_keeps_wallpaper_proportions_and_live_pixels() {
    for scale in [1.0, 2.0] {
        let mut session = Session::boot(
            &format!("overview-native-{scale}"),
            SessionOptions {
                scale: Some(scale),
                config_extra: "show_dock = false\n".into(),
                ..Default::default()
            },
        )
        .unwrap();
        session.door().barrier().unwrap();
        let wallpaper = session.screenshot("wallpaper").unwrap();
        let signal = session.dir.join("change-color");
        session.launch("foot", &[
            "--title=LivePortrait", "--window-size-pixels=160x320", "--override", "locked-title=yes",
            "sh", "-c", r#"printf '\033[48;2;220;30;40m\033[2J'; while ! test -f "$1"; do sleep .05; done; printf '\033[48;2;30;210;70m\033[2J'; exec sleep 120"#,
            "overview-probe", signal.to_str().unwrap(),
        ]).unwrap();
        session
            .launch(
                "foot",
                &[
                    "--title=WideWindow",
                    "--window-size-pixels=400x160",
                    "--override",
                    "locked-title=yes",
                ],
            )
            .unwrap();
        poll_until(
            Duration::from_secs(40),
            "two differently shaped windows",
            || {
                let world = session.world().ok()?;
                (world.window_matching("LivePortrait").is_some()
                    && world.window_matching("WideWindow").is_some())
                .then_some(())
            },
        )
        .unwrap();
        open_overview(&mut session);
        session.door().barrier().unwrap();
        let world = session.world().unwrap();
        let native = world.overview.as_ref().expect("native scene");
        assert_eq!(native.preview_edge, 0, "no capture-resolution boost");
        assert!(
            native.label_bytes < world.output_w as usize * world.output_h as usize / 2,
            "captions must be smaller than 1/8 of a full RGBA output"
        );
        assert_eq!(overview_shell(&world).unwrap().buffer_bytes, 0);
        assert_eq!(world.overview_windows.len(), 2);
        let portrait_id = world.window_matching("LivePortrait").unwrap().id;
        let portrait = world
            .overview_windows
            .iter()
            .find(|w| w.id == portrait_id)
            .unwrap();
        assert!(portrait.rect.size.h > portrait.rect.size.w);
        for window in &world.overview_windows {
            assert!(window.rect.size.w <= window.source.w && window.rect.size.h <= window.source.h);
            let ratio = window.rect.size.w as f64 / window.source.w as f64;
            assert!((window.rect.size.h as f64 - window.source.h as f64 * ratio).abs() < 2.0);
        }
        let shot = session.screenshot("live-windows").unwrap();
        let mut unchanged = 0;
        let mut sampled = 0;
        for y in (world.output_h / 2..world.output_h).step_by(16) {
            for x in (0..world.output_w).step_by(16) {
                if world.overview_windows.iter().any(|w| {
                    let r = w.rect;
                    x as i32 >= r.pos.x - 150
                        && x as i32 <= r.pos.x + r.size.w as i32 + 150
                        && y as i32 >= r.pos.y - 8
                        && y as i32 <= r.pos.y + r.size.h as i32 + 70
                }) {
                    continue;
                }
                unchanged += usize::from(shot.pixel(x, y) == wallpaper.pixel(x, y));
                sampled += 1;
            }
        }
        assert!(
            sampled > 100 && unchanged * 10 > sampled * 9,
            "exposed desktop keeps the original wallpaper pixels"
        );
        let rect = portrait.rect;
        let (x, y) = (
            rect.pos.x as u32 + rect.size.w / 2,
            rect.pos.y as u32 + rect.size.h / 2,
        );
        let red = shot.pixel(x, y);
        assert!(
            red[0] > 180 && red[1] < 70,
            "real red client content: {red:?}"
        );
        std::fs::write(&signal, "go").unwrap();
        poll_until(
            Duration::from_secs(10),
            "live preview changing without a snapshot refresh",
            || {
                let shot = session.screenshot("live-change").ok()?;
                let pixel = shot.pixel(x, y);
                (pixel[1] > 180 && pixel[0] < 70).then_some(())
            },
        )
        .unwrap();
        session.door().motion(x as f64, y as f64).unwrap();
        session.door().barrier().unwrap();
        let hovered = session.world().unwrap();
        assert_eq!(
            hovered.overview.as_ref().unwrap().label_bytes,
            native.label_bytes
        );
        assert_eq!(
            hovered
                .windows
                .iter()
                .find(|w| w.id == portrait_id)
                .map(|w| (w.w, w.h)),
            world
                .windows
                .iter()
                .find(|w| w.id == portrait_id)
                .map(|w| (w.w, w.h)),
            "Overview does not resize clients"
        );
        // Mapping a window while modal must refresh the scene; the old restore
        // handler returned before delivering this notification to Overview.
        launch_terminal(&mut session, "AddedDuringOverview");
        poll_until(
            Duration::from_secs(10),
            "new window joins the live scene",
            || (session.world().ok()?.overview_windows.len() == 3).then_some(()),
        )
        .unwrap();
        session.door().tap_key(keys::ESC).unwrap();
        assert_overview_closed(&mut session, "Escape");
        let closed = session.world().unwrap();
        assert!(
            closed.overview.is_none() && closed.overview_windows.is_empty(),
            "scene and captions released"
        );
    }
}

fn workspace_layout(world: &World) -> wm_theme::overview::OverviewLayout {
    wm_theme::overview::live::layout(
        wm_theme_api::Size::new(world.output_w, world.output_h),
        world.dock().expect("default dock supplies the tile size").w,
        &[],
        world.workspace_count,
    )
}

fn center(rect: wm_theme_api::Rect) -> (f64, f64) {
    (rect.pos.x as f64 + rect.size.w as f64 / 2.0,
        rect.pos.y as f64 + rect.size.h as f64 / 2.0)
}

#[test]
#[ignore = "requires nested Wayland: scripts/e2e.sh --headless --test overview"]
fn desktop_close_controls_remove_empty_and_occupied_desktops_without_losing_windows() {
    for scale in [1.0, 2.0] {
        let mut session = Session::boot(
            &format!("overview-close-desktops-{scale}"),
            SessionOptions {
                scale: Some(scale),
                config_extra: "[keybindings]\n\"super+2\" = \"workspace 8\"\n".into(),
                ..Default::default()
            },
        ).unwrap();
        launch_terminal(&mut session, "KeepOnFirstDesktop");
        session.door().chord(keys::LEFTMETA, keys::TWO).unwrap();
        session.door().barrier().unwrap();
        launch_terminal(&mut session, "KeepOnLastDesktop");
        let probe = profile_binary("chonk-workspace-probe").unwrap();
        session.launch(probe.to_str().unwrap(), &["--activate-removed"]).unwrap();
        poll_until(Duration::from_secs(10), "native workspace client sees eight desktops", || {
            session.client_log("chonk-workspace-probe").contains("count=8 ").then_some(())
        }).unwrap();
        open_overview(&mut session);
        let world = session.world().unwrap();
        let panel = overview_shell(&world).unwrap().id;
        assert_eq!((world.current_workspace, world.workspace_count), (7, 8));
        let layout = workspace_layout(&world);
        let shot = session.screenshot("eight-desktops-close-glyphs").unwrap();
        for index in 0..8 {
            let close = layout.workspace_close_rect(index).unwrap();
            let (x, y) = center(close);
            let pixel = shot.pixel(x as u32, y as u32);
            assert!(pixel[0] > 190 && pixel[1] > 190 && pixel[2] > 190,
                "desktop {index} must show the white close glyph at scale {scale}: {pixel:?}");
        }

        // A cancelled click must neither close a desktop nor dismiss Overview.
        let (x, y) = center(layout.workspace_close_rect(1).unwrap());
        session.door().motion(x, y).unwrap();
        session.door().button("left", true).unwrap();
        session.door().barrier().unwrap();
        session.door().motion(2.0, world.output_h as f64 / 2.0).unwrap();
        session.door().button("left", false).unwrap();
        session.door().barrier().unwrap();
        assert_eq!(session.world().unwrap().workspace_count, 8);
        assert_eq!(overview_shell(&session.world().unwrap()).unwrap().id, panel);

        // Entry-set changes invalidate an armed close instead of targeting a
        // freshly rearranged row on release.
        session.door().motion(x, y).unwrap();
        session.door().button("left", true).unwrap();
        session.door().barrier().unwrap();
        launch_terminal(&mut session, "ArrivedDuringClosePress");
        session.door().button("left", false).unwrap();
        session.door().barrier().unwrap();
        assert_eq!(session.world().unwrap().workspace_count, 8);

        session.door().click(x, y).unwrap();
        let world = session.world().unwrap();
        assert_eq!((world.current_workspace, world.workspace_count), (6, 7));
        assert_eq!(overview_shell(&world).unwrap().id, panel);
        // Closing the occupied active desktop merges its windows leftward.
        let (x, y) = center(workspace_layout(&world).workspace_close_rect(6).unwrap());
        session.door().click(x, y).unwrap();
        let world = session.world().unwrap();
        assert_eq!((world.current_workspace, world.workspace_count), (5, 6));
        assert_eq!(world.overview_windows.len(), 2);

        // Switch to the first thumbnail's body, then close that first desktop.
        let first = workspace_layout(&world).strip[0];
        let (x, y) = center(first);
        session.door().click(x, y).unwrap();
        let world = session.world().unwrap();
        assert_eq!(world.current_workspace, 0);
        let (x, y) = center(workspace_layout(&world).workspace_close_rect(0).unwrap());
        session.door().click(x, y).unwrap();
        assert_eq!(session.world().unwrap().workspace_count, 5);

        // Repeated close clicks compact all remaining empty slots. The final
        // occupied inactive desktop joins the active one and stays accessible.
        while session.world().unwrap().workspace_count > 1 {
            let world = session.world().unwrap();
            let (x, y) = center(workspace_layout(&world)
                .workspace_close_rect(world.workspace_count - 1).unwrap());
            session.door().click(x, y).unwrap();
            assert_eq!(session.world().unwrap().workspace_count, world.workspace_count - 1);
        }
        poll_until(Duration::from_secs(10), "native clients retire every removed desktop", || {
            let log = session.client_log("chonk-workspace-probe");
            (log.contains("count=1 names=1 active=1")
                && log.contains("**removed activation checked**")).then_some(())
        }).unwrap();
        session.door().barrier().unwrap();
        let world = session.world().unwrap();
        assert_eq!((world.current_workspace, world.workspace_count), (0, 1),
            "stale Wayland activation requests cannot resurrect closed desktops");
        assert_eq!(world.overview_windows.len(), 3);
        assert_eq!(overview_shell(&world).unwrap().id, panel);
        assert_eq!(overview_shell(&world).unwrap().buffer_bytes, 0);
        assert!(workspace_layout(&world).workspace_close_rect(0).is_none());
        session.screenshot("one-desktop-windows-preserved").unwrap();
        session.door().tap_key(keys::ESC).unwrap();
        assert_overview_closed(&mut session, "closing desktops then Escape");
        for title in ["KeepOnFirstDesktop", "KeepOnLastDesktop", "ArrivedDuringClosePress"] {
            let window = world.window_matching(title).unwrap();
            assert!(world.frame_of(window.id).unwrap().mapped, "{title} survives and is visible");
        }
    }
}
