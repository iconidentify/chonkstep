//! Drive the actual swipe input route against real windows and layer surfaces.
use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions, World};
use std::time::Duration;

fn swipe(session: &mut Session, fingers: u32, x: f64, y: f64, cancelled: bool) {
    let door = session.door();
    door.swipe_begin(fingers).unwrap();
    door.swipe_update(x, y).unwrap();
    door.swipe_end(cancelled).unwrap();
    door.barrier().unwrap();
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
        assert!(
            !visible(&mut session, window),
            "left advances to the next workspace"
        );
        for _ in 0..12 {
            swipe(&mut session, 3, -100.0, 0.0, false);
        }
        let empty = session.world().unwrap();
        assert_eq!((empty.current_workspace, empty.workspace_count), (1, 2),
            "swiping on the empty final desktop must not create a chain of empty desktops");
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
        session.door().swipe_begin(4).unwrap();
        session.door().swipe_update(-120.0, 0.0).unwrap();
        session.door().swipe_update(115.0, 0.0).unwrap();
        session.door().swipe_end(false).unwrap();
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
