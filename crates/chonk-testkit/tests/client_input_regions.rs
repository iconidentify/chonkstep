//! Window geometry excludes shadows; wl_surface input regions include the
//! client's resize handles. Exercise that distinction through real pointer
//! events at integer and fractional scales, without depending on a GTK theme.

use std::time::Duration;

use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions, WindowInfo};

const EVENT: Duration = Duration::from_secs(10);
const PROBE: &str = "chonk-input-probe";
const APP: &str = "csd-region-probe";

fn window(session: &mut Session) -> WindowInfo {
    session
        .world()
        .unwrap()
        .window_matching(APP)
        .unwrap()
        .clone()
}

fn boot(scale: f32, scenario: &str) -> (Session, WindowInfo) {
    let mut session = Session::boot(
        &format!("client-input-region-{scenario}-{scale}"),
        SessionOptions {
            scale: Some(scale),
            config_extra: format!(
                "show_dock = false\nomarchy_menu = false\nhyprland_config = false\n\
                 [decorations]\nclient_side = [\"{APP}\"]\n"
            ),
            ..Default::default()
        },
    )
    .unwrap();
    let binary = profile_binary(PROBE).unwrap();
    session
        .launch_isolated(
            binary.to_str().unwrap(),
            &[
                &scale.to_string(),
                "--csd-input-region",
                "--interactive=resize",
                &format!("--app-id={APP}"),
            ],
        )
        .unwrap();
    session.wait_for_window(APP).unwrap();
    let before = poll_until(EVENT, "committed window geometry", || {
        let w = window(&mut session);
        (w.offset_x == (25.0 * scale).round() as i32 && w.w == (340.0 * scale) as u32).then_some(w)
    })
    .unwrap();
    assert!(session.world().unwrap().frame_of(before.id).is_none());
    // Keep all four shadow bands inside the output, including at 2x.
    let start = (f64::from(before.x + 40), f64::from(before.y + 40));
    session.door().key(keys::LEFTALT, true).unwrap();
    session.door().drag_to(start, (250.0, 200.0)).unwrap();
    session.door().button("left", false).unwrap();
    session.door().key(keys::LEFTALT, false).unwrap();
    session.door().barrier().unwrap();
    let w = window(&mut session);
    assert_eq!((w.x, w.y), (210, 160));
    (session, w)
}

fn last_sequence(session: &Session) -> u64 {
    session
        .client_log(PROBE)
        .lines()
        .rev()
        .find_map(|line| {
            line.strip_prefix("input ")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

fn wait_input(session: &Session, after: u64, kind: &str) -> (f64, f64) {
    poll_until(EVENT, kind, || {
        session.client_log(PROBE).lines().rev().find_map(|line| {
            let mut fields = line.strip_prefix("input ")?.split_whitespace();
            let sequence: u64 = fields.next()?.parse().ok()?;
            let event = fields.next()?;
            let x = fields.next()?.parse().ok()?;
            let y = fields.next()?.parse().ok()?;
            (sequence > after && event == kind).then_some((x, y))
        })
    })
    .unwrap_or_else(|error| panic!("{error}\n{}", session.client_log(PROBE)))
}

fn exercise_region(scale: f32) {
    let (mut session, w) = boot(scale, "bands");
    let global = |x: f64, y: f64| {
        (
            f64::from(w.x - w.offset_x) + x * f64::from(scale),
            f64::from(w.y - w.offset_y) + y * f64::from(scale),
        )
    };
    // Ten logical pixels outside every edge/corner, inside the client's
    // 12px input band. The old hard-coded 8 *physical* pixels miss all eight.
    for (x, y) in [
        (15.0, 20.0),
        (190.0, 20.0),
        (375.0, 20.0),
        (15.0, 140.0),
        (375.0, 140.0),
        (15.0, 270.0),
        (190.0, 270.0),
        (375.0, 270.0),
    ] {
        let (gx, gy) = global(x, y);
        assert_eq!(
            session.door().hit(gx as i32, gy as i32).unwrap(),
            "content",
            "scale={scale} grip=({x},{y})"
        );
        let sequence = last_sequence(&session);
        session.door().motion(gx, gy).unwrap();
        session.door().button("left", true).unwrap();
        let position = wait_input(&session, sequence, "press");
        assert!(
            (position.0 - x).abs() < 0.01 && (position.1 - y).abs() < 0.01,
            "scale={scale} expected ({x},{y}), got {position:?}"
        );
        session.door().button("left", false).unwrap();
        wait_input(&session, sequence, "release");
    }
    // Outside the input region, including a deliberately punched hole
    // inside the visible window: no fallback to the root wl_surface.
    for (x, y) in [
        (12.0, 140.0),
        (190.0, 17.0),
        (378.0, 140.0),
        (190.0, 273.0),
        (190.0, 140.0),
        (410.0, 140.0),
    ] {
        let (gx, gy) = global(x, y);
        assert_eq!(
            session.door().hit(gx as i32, gy as i32).unwrap(),
            "root",
            "scale={scale} excluded=({x},{y})"
        );
    }
    // A real authorized xdg resize can begin in the newly reachable band.
    let (gx, gy) = global(375.0, 270.0);
    let sequence = last_sequence(&session);
    session.door().motion(gx, gy).unwrap();
    session.door().button("left", true).unwrap();
    wait_input(&session, sequence, "press");
    session.door().tap_key(59).unwrap(); // F1 spends the actual press serial.
    poll_until(EVENT, "client resize request processed", || {
        session
            .client_log(PROBE)
            .contains("interactive fence 59 ")
            .then_some(())
    })
    .unwrap();
    session.door().barrier().unwrap();
    session.door().motion(gx + 60.0, gy + 40.0).unwrap();
    let resized = poll_until(EVENT, "resize from the CSD input band", || {
        let now = window(&mut session);
        (now.w > w.w && now.h > w.h).then_some(now)
    })
    .unwrap();
    session.door().button("left", false).unwrap();
    assert_eq!((resized.w - w.w, resized.h - w.h), (60, 40));
    session.door().motion(gx + 90.0, gy + 70.0).unwrap();
    session.door().barrier().unwrap();
    let after_release = window(&mut session);
    assert_eq!((after_release.w, after_release.h), (resized.w, resized.h));
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless"]
fn client_resize_handles_and_input_holes_at_1x() {
    exercise_region(1.0);
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless"]
fn client_resize_handles_and_input_holes_at_2x() {
    exercise_region(2.0);
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless"]
fn client_resize_handles_and_input_holes_at_fractional_scale() {
    exercise_region(1.5);
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless"]
fn shadows_and_input_holes_deliver_clicks_to_the_window_underneath() {
    let (mut session, w) = boot(1.0, "overlap");
    let binary = profile_binary(PROBE).unwrap();
    session
        .launch_isolated(binary.to_str().unwrap(), &["1", "--app-id=under-region"])
        .unwrap();
    let underneath = session.wait_for_window("under-region").unwrap();
    // Put the second window's content beneath the entire first buffer.
    session.door().key(keys::LEFTALT, true).unwrap();
    session
        .door()
        .drag_to(
            (f64::from(underneath.x + 40), f64::from(underneath.y + 40)),
            (
                f64::from(w.x - w.offset_x + 40),
                f64::from(w.y - w.offset_y + 40),
            ),
        )
        .unwrap();
    session.door().button("left", false).unwrap();
    session.door().key(keys::LEFTALT, false).unwrap();
    for (x, y) in [(12, 140), (190, 140)] {
        // Raise the CSD window first; clicking through must focus the one
        // beneath it, even when the hole lies inside its window geometry.
        session.door().chord(keys::LEFTALT, 15).unwrap(); // Alt+Tab
        session.door().barrier().unwrap();
        assert_eq!(session.world().unwrap().logical_focus, Some(w.id));
        let sequence = last_sequence(&session); // newest probe is underneath
        session
            .door()
            .click(
                f64::from(w.x - w.offset_x + x),
                f64::from(w.y - w.offset_y + y),
            )
            .unwrap();
        let position = wait_input(&session, sequence, "press");
        assert_eq!(position, (f64::from(x), f64::from(y)));
        assert_eq!(session.world().unwrap().logical_focus, Some(underneath.id));
    }
}
