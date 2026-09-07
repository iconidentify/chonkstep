//! Drive the actual swipe input route against real windows and layer surfaces.
use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions, World};
use std::time::Duration;

fn swipe(session: &mut Session, fingers: u32, x: f64, y: f64, cancelled: bool) {
    let door = session.door();
    door.swipe_begin_at(fingers, 0).unwrap();
    door.swipe_update_at(x, y, 200).unwrap();
    door.swipe_end_at(cancelled, 350).unwrap();
    door.barrier().unwrap();
    settled(session);
}

fn settled(session: &mut Session) -> World {
    poll_until(Duration::from_secs(4), "gesture spring settlement", || {
        let world = session.world().ok()?;
        world.gesture.is_none().then_some(world)
    })
    .unwrap()
}

fn probe(session: &mut Session, scale: f64) -> u64 {
    let binary = profile_binary("chonk-input-probe").unwrap();
    session
        .launch(binary.to_str().unwrap(), &[&scale.to_string()])
        .unwrap();
    poll_until(Duration::from_secs(15), "gesture input probe", || {
        session
            .world()
            .ok()?
            .window_matching("input-probe")
            .map(|w| w.id)
    })
    .unwrap()
}

fn following(session: &mut Session, x: f64, y: f64, time: u32) -> chonk_testkit::GestureInfo {
    session.door().swipe_update_at(x, y, time).unwrap();
    session
        .world()
        .unwrap()
        .gesture
        .expect("finger-following scene")
}

fn red_center(shot: &chonk_testkit::Screenshot) -> (f64, f64) {
    let (mut xsum, mut ysum, mut count) = (0_u64, 0_u64, 0_u64);
    for y in (0..shot.height).step_by(3) {
        for x in (0..shot.width).step_by(3) {
            let [r, g, b, _] = shot.pixel(x, y);
            if r > 140 && g < 100 && b < 100 {
                xsum += u64::from(x);
                ysum += u64::from(y);
                count += 1;
            }
        }
    }
    assert!(
        count > 100,
        "live red client pixels: {}",
        shot.path.display()
    );
    (xsum as f64 / count as f64, ysum as f64 / count as f64)
}

#[test]
#[ignore = "requires nested Wayland"]
fn fingers_move_live_pixels_without_switching_configuring_or_exposing_pointer_targets() {
    for scale in [1.0, 1.5, 2.0] {
        let mut session = Session::boot(
            &format!("gesture-follow-{scale}"),
            SessionOptions {
                scale: Some(scale),
                config_extra: "show_dock = false\n".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let id = probe(&mut session, f64::from(scale));
        session.door().barrier().unwrap();
        let initial = session.world().unwrap();
        let window = initial.windows.iter().find(|w| w.id == id).unwrap();
        let (cx, cy) = (
            window.x + window.w as i32 / 2,
            window.y + window.h as i32 / 2,
        );
        session.door().motion(cx as f64, cy as f64).unwrap();
        let before = red_center(&session.screenshot("before").unwrap());
        let configures = session
            .client_log("chonk-input-probe")
            .matches("surface configure ")
            .count();
        session.door().swipe_begin_at(3, 0).unwrap();
        session.door().swipe_update_at(-5.0, -2.0, 20).unwrap();
        assert!(
            session.world().unwrap().gesture.is_none(),
            "jitter has no visual owner"
        );
        let first = following(&mut session, -15.0, 0.0, 40);
        assert_eq!(first.progress, 0.125);
        assert_eq!(session.door().hit(cx, cy).unwrap(), "root");
        let moved = red_center(&session.screenshot("following").unwrap());
        // New clients begin at the left edge. Measure the visible clipped
        // rectangle, not the centroid of pixels that have left the output.
        let shift = initial.output_w as f64 * 0.125;
        let expected = ((window.x as f64 - shift).max(0.0)
            + (window.x as f64 + window.w as f64 - shift).min(initial.output_w as f64))
            / 2.0;
        assert!(
            (moved.0 - expected).abs() < 3.0,
            "normalized slide: {moved:?}, expected x={expected}"
        );
        let second = following(&mut session, -20.0, 0.0, 60);
        assert_eq!(second.progress, 0.25);
        let reverse = following(&mut session, 20.0, 0.0, 80);
        assert_eq!(reverse.progress, 0.125);
        assert_eq!(session.world().unwrap().current_workspace, 0);
        let unchanged = session.world().unwrap();
        let actual = unchanged.windows.iter().find(|w| w.id == id).unwrap();
        assert_eq!(
            (actual.x, actual.y, actual.w, actual.h),
            (window.x, window.y, window.w, window.h)
        );
        assert_eq!(
            session
                .client_log("chonk-input-probe")
                .matches("surface configure ")
                .count(),
            configures
        );
        session.door().swipe_end_at(false, 250).unwrap();
        assert_eq!(
            settled(&mut session).current_workspace,
            0,
            "slow early release cancels"
        );
        let restored = red_center(&session.screenshot("restored").unwrap());
        assert!((before.0 - restored.0).abs() < 1.0);

        // A fast release at 1/4 span must commit, despite insufficient distance.
        session.door().swipe_begin_at(4, 1000).unwrap();
        following(&mut session, -20.0, 0.0, 1020);
        following(&mut session, -20.0, 0.0, 1040);
        session.door().swipe_end_at(false, 1040).unwrap();
        assert_eq!(settled(&mut session).current_workspace, 1);
        swipe(&mut session, 3, 100.0, 0.0, false);
        // Cross midpoint, then reverse briskly: trajectory wins over max distance.
        session.door().swipe_begin_at(3, 2000).unwrap();
        following(&mut session, -120.0, 0.0, 2200);
        following(&mut session, 16.0, 0.0, 2220);
        following(&mut session, 16.0, 0.0, 2240);
        session.door().swipe_end_at(false, 2240).unwrap();
        assert_eq!(settled(&mut session).current_workspace, 0);
        // Continuous topology crosses origin; first-desktop edge is elastic.
        session.door().swipe_begin_at(3, 3000).unwrap();
        following(&mut session, -40.0, 0.0, 3020);
        let edge = following(&mut session, 80.0, 0.0, 3040);
        assert!(edge.progress < 0.0 && edge.progress > -0.18);
        let farther = following(&mut session, 10000.0, 0.0, 3060);
        assert!(farther.progress < edge.progress && farther.progress > -0.18);
        session.door().swipe_end_at(false, 3060).unwrap();
        assert_eq!(settled(&mut session).current_workspace, 0);
    }
}

#[test]
#[ignore = "requires nested Wayland"]
fn overview_morph_follows_reverses_flicks_and_never_resizes_live_clients() {
    for scale in [1.0, 2.0] {
        let mut session = Session::boot(
            &format!("overview-morph-{scale}"),
            SessionOptions {
                scale: Some(scale),
                config_extra: "show_dock = false\n".into(),
                ..Default::default()
            },
        )
        .unwrap();
        probe(&mut session, f64::from(scale));
        session.door().barrier().unwrap();
        let before = red_center(&session.screenshot("desktop").unwrap());
        let configures = session
            .client_log("chonk-input-probe")
            .matches("surface configure ")
            .count();
        session.door().swipe_begin_at(3, 0).unwrap();
        assert_eq!(following(&mut session, 0.0, -40.0, 100).progress, 0.25);
        let quarter = red_center(&session.screenshot("quarter").unwrap());
        assert_eq!(session.world().unwrap().overview.unwrap().progress, 0.25);
        assert_eq!(following(&mut session, 0.0, -40.0, 200).progress, 0.5);
        let half_shot = session.screenshot("half").unwrap();
        let half = red_center(&half_shot);
        let half_world = session.world().unwrap();
        let card = &half_world.overview_windows[0];
        let source = half_world.frame_of(card.id).unwrap();
        let ring_x = ((source.x + card.rect.pos.x) as f64 / 2.0).round() as u32 - 1;
        let ring_y = ((source.y + card.rect.pos.y) as f64 / 2.0
            + (source.h + card.rect.size.h) as f64 / 4.0).round() as u32;
        let [r, g, b, _] = half_shot.pixel(ring_x, ring_y);
        assert!(b > g.saturating_add(20) && b > r.saturating_add(40),
            "selection ring must follow the interpolated window, not float at its final position: {r},{g},{b}");
        assert!((half.1 - before.1).abs() > (quarter.1 - before.1).abs() + 1.0);
        assert_eq!(following(&mut session, 0.0, 40.0, 300).progress, 0.25);
        session.door().swipe_end_at(false, 450).unwrap();
        assert!(settled(&mut session).overview.is_none());
        assert_eq!(
            session
                .client_log("chonk-input-probe")
                .matches("surface configure ")
                .count(),
            configures,
            "morphs transform client textures without issuing configure events"
        );
        session.door().swipe_begin_at(4, 1000).unwrap();
        following(&mut session, 0.0, -20.0, 1020);
        following(&mut session, 0.0, -20.0, 1040);
        session.door().swipe_end_at(false, 1040).unwrap();
        assert_eq!(settled(&mut session).overview.unwrap().progress, 1.0);
        session.door().swipe_begin_at(3, 2000).unwrap();
        assert_eq!(following(&mut session, 0.0, 40.0, 2100).progress, 0.75);
        assert_eq!(following(&mut session, 0.0, -20.0, 2200).progress, 0.875);
        session.door().swipe_end_at(false, 2350).unwrap();
        assert_eq!(settled(&mut session).overview.unwrap().progress, 1.0);
        session.door().swipe_begin_at(3, 3000).unwrap();
        following(&mut session, 0.0, 20.0, 3020);
        following(&mut session, 0.0, 20.0, 3040);
        session.door().swipe_end_at(false, 3040).unwrap();
        assert!(settled(&mut session).overview.is_none());
    }
}

#[test]
#[ignore = "requires nested Wayland"]
fn cancellation_resume_device_loss_and_spring_interruption_release_scene_ownership() {
    let mut session = Session::boot("gesture-ownership", SessionOptions::default()).unwrap();
    let id = probe(&mut session, 2.0);
    for (x, y) in [(-60.0, 0.0), (0.0, -60.0)] {
        for resume in [false, true] {
            session.door().swipe_begin_at(3, 0).unwrap();
            following(&mut session, x, y, 20);
            session.door().reset_input(resume).unwrap();
            let world = session.world().unwrap();
            assert!(world.gesture.is_none() && world.overview.is_none());
            session.door().swipe_update_at(x, y, 40).unwrap();
            session.door().swipe_end_at(false, 60).unwrap();
            assert_eq!(settled(&mut session).current_workspace, 0);
        }
        swipe(&mut session, 3, x * 3.0, y * 3.0, true);
        session.door().swipe_begin_at(3, 0).unwrap();
        following(&mut session, x, y, 20);
        session.door().pause_gesture_input().unwrap();
        assert!(session.world().unwrap().gesture.is_none());
        session.door().swipe_end_at(false, 40).unwrap();
        assert_eq!(session.world().unwrap().current_workspace, 0);
        assert!(session.world().unwrap().overview.is_none());
    }
    session.door().swipe_begin_at(3, 1000).unwrap();
    following(&mut session, -100.0, 0.0, 1200);
    session.door().swipe_end_at(false, 1350).unwrap();
    session.door().swipe_begin_at(4, 1400).unwrap();
    let caught = session.world().unwrap().gesture.unwrap();
    assert!(!caught.settling && caught.progress >= 0.625);
    let reversed = following(&mut session, 40.0, 0.0, 1420);
    assert!(reversed.progress < caught.progress);
    session.door().swipe_end_at(true, 1440).unwrap();
    assert_eq!(settled(&mut session).current_workspace, 0);
    assert!(session.world().unwrap().frame_of(id).unwrap().mapped);
    // A modal command interrupts before it can operate on a visual neighbor.
    session.door().swipe_begin_at(3, 2000).unwrap();
    following(&mut session, -60.0, 0.0, 2020);
    session.door().chord(keys::LEFTMETA, keys::UP).unwrap();
    assert!(session.world().unwrap().gesture.is_none());
    session.door().tap_key(keys::ESC).unwrap();
}

#[test]
#[ignore = "requires nested Wayland"]
fn unclaimed_swipe_protocol_streams_reach_clients_whole() {
    for enabled in [true, false] {
        let mut session = Session::boot(
            &format!("gesture-passthrough-{enabled}"),
            SessionOptions {
                scale: Some(1.0),
                config_extra: format!("[input.gestures]\nenabled = {enabled}\n"),
                ..Default::default()
            },
        )
        .unwrap();
        let id = probe(&mut session, 1.0);
        let world = session.world().unwrap();
        let window = world.windows.iter().find(|w| w.id == id).unwrap();
        session
            .door()
            .motion((window.x + 100) as f64, (window.y + 100) as f64)
            .unwrap();
        for fingers in [2, 5, 3, 4] {
            swipe(&mut session, fingers, -30.0, 0.0, false);
        }
        let count = if enabled { 2 } else { 4 };
        poll_until(
            Duration::from_secs(5),
            "complete client swipe streams",
            || {
                let log = session.client_log("chonk-input-probe");
                (log.matches("swipe end 0").count() == count).then_some(())
            },
        )
        .unwrap();
        let log = session.client_log("chonk-input-probe");
        assert_eq!(log.matches("swipe begin ").count(), count);
        assert_eq!(log.matches("swipe update ").count(), count);
    }
}

#[test]
#[ignore = "requires nested Wayland"]
fn adjacent_live_workspace_has_no_focus_until_the_single_commit_and_then_stops_animating() {
    let mut session = Session::boot(
        "gesture-adjacent-focus",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "show_dock = false\n[keybindings]\n\"super+2\" = \"workspace 2\"\n"
                .into(),
            ..Default::default()
        },
    )
    .unwrap();
    let red = probe(&mut session, 1.0);
    session.door().chord(keys::LEFTMETA, keys::TWO).unwrap();
    let green = terminal(&mut session);
    session.door().barrier().unwrap();
    let initial = session.world().unwrap();
    assert_eq!(
        (
            initial.current_workspace,
            initial.logical_focus,
            initial.seat_focus
        ),
        (1, Some(green), Some(green))
    );
    session.door().swipe_begin_at(3, 0).unwrap();
    following(&mut session, 140.0, 0.0, 200);
    let visible = session.world().unwrap();
    assert_eq!(
        (
            visible.current_workspace,
            visible.logical_focus,
            visible.seat_focus
        ),
        (1, Some(green), Some(green))
    );
    assert!(
        !visible.frame_of(red).unwrap().mapped,
        "neighbor remains semantically hidden"
    );
    let red_pixels = red_center(&session.screenshot("adjacent-live-red").unwrap());
    assert!(
        red_pixels.0 < 250.0,
        "hidden neighbor's live pixels enter from the left"
    );
    session.door().motion(red_pixels.0, red_pixels.1).unwrap();
    session.door().barrier().unwrap();
    assert_eq!(session.world().unwrap().logical_focus, Some(green));
    session.door().swipe_end_at(false, 350).unwrap();
    settled(&mut session);
    session.door().barrier().unwrap();
    let committed = session.world().unwrap();
    assert_eq!(
        (
            committed.current_workspace,
            committed.logical_focus,
            committed.seat_focus
        ),
        (0, Some(red), Some(red))
    );

    // Warm a scene, then measure genuine frame work independently of test-door
    // round-trip wall time. Metrics are diagnostic, not flaky timing limits.
    session.door().swipe_begin_at(3, 1000).unwrap();
    following(&mut session, -20.0, 0.0, 1020);
    session.door().barrier().unwrap();
    session.door().frame_stats().unwrap();
    for i in 1..=120 {
        following(
            &mut session,
            if i % 40 < 20 { -0.5 } else { 0.5 },
            0.0,
            1020 + i * 8,
        );
        session.door().barrier().unwrap();
    }
    let frames = session.door().frame_stats().unwrap();
    eprintln!("120 warm finger-following updates (nested GLES): {frames:?}");
    session.door().swipe_end_at(true, 2100).unwrap();
    settled(&mut session);
    session.door().barrier().unwrap();
    session.door().frame_stats().unwrap();
    let until = std::time::Instant::now() + Duration::from_millis(160);
    poll_until(Duration::from_secs(2), "settled scene remains idle", || {
        let world = session.world().ok()?;
        assert!(world.gesture.is_none());
        (std::time::Instant::now() >= until).then_some(())
    })
    .unwrap();
    assert_eq!(
        session.door().frame_stats().unwrap().render_calls,
        0,
        "no spring deadline or animation frames survive settlement"
    );
}

#[test]
#[ignore = "requires nested Wayland"]
fn lock_and_held_client_pointer_grabs_cannot_inherit_partial_desktop_scenes() {
    for (name, x, y) in [("workspace", -60.0, 0.0), ("overview", 0.0, -60.0)] {
        let mut session = Session::boot(
            &format!("gesture-lock-{name}"),
            SessionOptions {
                scale: Some(1.0),
                ..Default::default()
            },
        )
        .unwrap();
        let id = probe(&mut session, 1.0);
        let world = session.world().unwrap();
        let window = world.windows.iter().find(|w| w.id == id).unwrap();
        session
            .door()
            .motion((window.x + 100) as f64, (window.y + 100) as f64)
            .unwrap();
        session.door().button("left", true).unwrap();
        session.door().barrier().unwrap();
        session.door().swipe_begin_at(3, 0).unwrap();
        session.door().swipe_update_at(x, y, 20).unwrap();
        assert!(session.world().unwrap().gesture.is_none());
        session.door().swipe_end_at(false, 40).unwrap();
        session.door().button("left", false).unwrap();
        session.door().barrier().unwrap();
        session.door().swipe_begin_at(4, 1000).unwrap();
        following(&mut session, x, y, 1020);
        let locker = profile_binary("chonk-lock-probe").unwrap();
        session
            .launch(locker.to_str().unwrap(), &["--hold"])
            .unwrap();
        poll_until(Duration::from_secs(10), "lock owns presentation", || {
            session
                .client_log("chonk-lock-probe")
                .contains("locked ")
                .then_some(())
        })
        .unwrap();
        let locked = session.world().unwrap();
        assert!(locked.gesture.is_none() && locked.overview.is_none());
        session.door().swipe_update_at(x, y, 1040).unwrap();
        session.door().swipe_end_at(false, 1060).unwrap();
        session.door().barrier().unwrap();
        assert_eq!(session.world().unwrap().current_workspace, 0);
        assert!(session.world().unwrap().gesture.is_none());
    }
}

fn overview(world: &World) -> Option<u64> {
    world
        .shells
        .iter()
        .find(|s| s.mapped && s.above && s.w == world.output_w && s.h == world.output_h)
        .map(|s| s.id)
}

fn terminal(session: &mut Session) -> u64 {
    session
        .launch(
            "foot",
            &[
                "--title=GestureWindow",
                "--window-size-pixels=300x180",
                "--override",
                "locked-title=yes",
            ],
        )
        .unwrap();
    poll_until(Duration::from_secs(40), "gesture terminal", || {
        session
            .world()
            .ok()?
            .window_matching("GestureWindow")
            .map(|w| w.id)
    })
    .unwrap()
}

fn visible(session: &mut Session, window: u64) -> bool {
    session.door().barrier().unwrap();
    session.world().unwrap().frame_of(window).unwrap().mapped
}

#[test]
#[ignore = "requires nested Wayland: scripts/e2e.sh --headless --test desktop_gestures"]
fn swipes_switch_once_and_open_close_reusable_overview_at_both_scales() {
    for scale in [1.0, 2.0] {
        let mut session = Session::boot(
            &format!("desktop-gestures-{scale}"),
            SessionOptions {
                scale: Some(scale),
                ..Default::default()
            },
        )
        .unwrap();
        let window = terminal(&mut session);
        session.door().swipe_begin(3).unwrap();
        for _ in 0..100 {
            session.door().swipe_update(-10.0, 0.0).unwrap();
        }
        assert!(
            visible(&mut session, window),
            "motion alone does not commit"
        );
        session.door().swipe_end(false).unwrap();
        settled(&mut session);
        assert!(
            !visible(&mut session, window),
            "left advances to the next workspace"
        );
        for _ in 0..12 {
            swipe(&mut session, 3, -100.0, 0.0, false);
        }
        let empty = session.world().unwrap();
        assert_eq!(
            (empty.current_workspace, empty.workspace_count),
            (1, 2),
            "swiping on the empty final desktop must not create a chain of empty desktops"
        );
        swipe(&mut session, 4, 100.0, 0.0, false);
        assert!(
            visible(&mut session, window),
            "one right swipe returns, even after a long stroke"
        );
        for (x, y, cancelled) in [
            (-100.0, 0.0, true),
            (-5.0, 0.0, false),
            (-100.0, -100.0, false),
        ] {
            swipe(&mut session, 3, x, y, cancelled);
            assert!(visible(&mut session, window));
            assert!(overview(&session.world().unwrap()).is_none());
        }
        session.door().swipe_begin_at(4, 0).unwrap();
        session.door().swipe_update_at(-120.0, 0.0, 20).unwrap();
        session.door().swipe_update_at(115.0, 0.0, 40).unwrap();
        session.door().swipe_end_at(false, 200).unwrap();
        settled(&mut session);
        assert!(
            visible(&mut session, window),
            "returning the fingers cancels"
        );

        swipe(&mut session, 3, 0.0, -100.0, false);
        let panel = overview(&session.world().unwrap()).expect("up opens window thumbnails");
        session.screenshot("gesture-overview").unwrap();
        swipe(&mut session, 4, 0.0, -100.0, false);
        assert_eq!(
            overview(&session.world().unwrap()),
            Some(panel),
            "up is idempotent"
        );
        swipe(&mut session, 4, -100.0, 0.0, false);
        assert!(!visible(&mut session, window));
        assert_eq!(overview(&session.world().unwrap()), Some(panel));
        swipe(&mut session, 3, 100.0, 0.0, false);
        assert!(visible(&mut session, window));
        swipe(&mut session, 4, 0.0, 100.0, false);
        let world = session.world().unwrap();
        assert!(overview(&world).is_none());
        assert_eq!(
            world
                .shells
                .iter()
                .find(|s| s.id == panel)
                .unwrap()
                .buffer_bytes,
            0
        );
        swipe(&mut session, 3, 0.0, 100.0, false);
        assert!(
            overview(&session.world().unwrap()).is_none(),
            "down never opens Overview"
        );
    }
}

#[test]
#[ignore = "requires nested Wayland: scripts/e2e.sh --headless --test desktop_gestures"]
fn configured_fingers_distance_and_disabling_reach_the_live_seat() {
    for enabled in [true, false] {
        let mut session = Session::boot(
            &format!("gesture-settings-{enabled}"),
            SessionOptions {
                scale: Some(1.0),
                config_extra: format!(
                    "[input.gestures]\nenabled = {enabled}\nfingers = 4\ndistance = 150\n"
                ),
                ..Default::default()
            },
        )
        .unwrap();
        swipe(&mut session, 3, 0.0, -200.0, false);
        assert!(overview(&session.world().unwrap()).is_none());
        swipe(&mut session, 4, 0.0, -100.0, false);
        assert!(overview(&session.world().unwrap()).is_none());
        swipe(&mut session, 4, 0.0, -200.0, false);
        assert_eq!(overview(&session.world().unwrap()).is_some(), enabled);
    }
}

#[test]
#[ignore = "requires nested Wayland: scripts/e2e.sh --headless --test desktop_gestures"]
fn a_late_layer_bar_stops_titlebar_drags_and_releases_its_boundary_on_exit() {
    let mut session = Session::boot(
        "gesture-bar-boundary",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "edge_resistance = 0\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let window = terminal(&mut session);
    let bar = profile_binary("chonk-fake-bar").unwrap();
    session
        .launch(bar.to_str().unwrap(), &["48", "top", "omarchy-bar"])
        .unwrap();
    poll_until(Duration::from_secs(10), "the bar reservation", || {
        let world = session.world().ok()?;
        (world.frame_of(window)?.y >= 48).then_some(())
    })
    .unwrap();
    session.door().barrier().unwrap();
    let frame = session.world().unwrap().frame_of(window).unwrap().clone();
    session
        .door()
        .drag_to(
            (frame.x as f64 + frame.w as f64 / 2.0, frame.y as f64 + 10.0),
            (350.0, 0.0),
        )
        .unwrap();
    session.door().barrier().unwrap();
    assert_eq!(session.world().unwrap().frame_of(window).unwrap().y, 48);
    session.screenshot("drag-stopped-below-bar").unwrap();
    session.kill_client("chonk-fake-bar");
    session.door().barrier().unwrap();
    let frame = session.world().unwrap().frame_of(window).unwrap().clone();
    session
        .door()
        .drag_to(
            (frame.x as f64 + frame.w as f64 / 2.0, frame.y as f64 + 10.0),
            (350.0, 0.0),
        )
        .unwrap();
    session.door().barrier().unwrap();
    assert!(session.world().unwrap().frame_of(window).unwrap().y < 48);
}
