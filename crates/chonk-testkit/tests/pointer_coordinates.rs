//! Assert coordinates received over the wire, including the implicit button
//! grab. Hover-only assertions miss a scale-dependent drag-selection error.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(10);
const PROBE: &str = "chonk-input-probe";

#[derive(Debug)]
struct InputEvent {
    sequence: u64,
    kind: String,
    x: f64,
    y: f64,
}

fn event_after(session: &Session, after: u64, kind: &str) -> Option<InputEvent> {
    session.client_log(PROBE).lines().rev().find_map(|line| {
        let mut fields = line.strip_prefix("input ")?.split_whitespace();
        let event = InputEvent {
            sequence: fields.next()?.parse().ok()?,
            kind: fields.next()?.into(),
            x: fields.next()?.parse().ok()?,
            y: fields.next()?.parse().ok()?,
        };
        (event.sequence > after && event.kind == kind).then_some(event)
    })
}

fn wait_event(session: &Session, after: u64, kind: &str) -> InputEvent {
    poll_until(EVENT, "client's next pointer event", || event_after(session, after, kind))
        .unwrap_or_else(|error| panic!("{error}\n{}", session.client_log(PROBE)))
}

fn drag_at_scale(scale: f32, subsurface: bool) {
    let mut session = Session::boot(
        &format!("pointer-coordinates-{scale}-{}", if subsurface { "child" } else { "root" }),
        SessionOptions { scale: Some(scale), config_extra: "show_dock = false\n".into(), ..SessionOptions::default() },
    )
    .expect("nested compositor");
    let binary = profile_binary(PROBE).expect("input probe built");
    session
        .launch(binary.to_str().unwrap(), &[&scale.to_string(), "dnd", if subsurface { "subsurface" } else { "root" }])
        .expect("probe launches");
    let window = session.wait_for_window("input-probe").expect("probe maps");
    session.door().barrier().unwrap();

    // A fresh explicit hover after enter avoids assuming that map-time focus
    // produced only one event. Each later action waits for its client receipt.
    session.door().motion(f64::from(window.x + 20), f64::from(window.y + 20)).unwrap();
    session.door().button("left", true).unwrap();
    let first = wait_event(&session, 0, "press");
    session.door().button("left", false).unwrap();
    let mut last = wait_event(&session, first.sequence, "release");
    let scale = f64::from(scale);
    let offset = if subsurface { (40.0 * scale, 30.0 * scale) } else { (0.0, 0.0) };
    let origin = (f64::from(window.x - window.offset_x) + offset.0, f64::from(window.y - window.offset_y) + offset.1);
    let start = (80.0, 70.0);
    session.door().motion(origin.0 + start.0 * scale, origin.1 + start.1 * scale).unwrap();
    last = wait_event(&session, last.sequence, if subsurface { "enter" } else { "motion" });
    assert!((last.x - start.0).abs() < 0.01 && (last.y - start.1).abs() < 0.01, "hover: {last:?}");
    session.door().button("left", true).unwrap();
    last = wait_event(&session, last.sequence, "press");

    // Include motion past the client's right edge and into its titlebar.
    // An implicit grab must keep the original surface and allow coordinates
    // outside its bounds until the last held button is released.
    for (x, y) in [(120.0, 90.0), (210.0, 160.0), (420.0, 90.0), (15.0, -10.0), (35.0, 45.0)] {
        session.door().motion(origin.0 + x * scale, origin.1 + y * scale).unwrap();
        last = wait_event(&session, last.sequence, "motion");
        assert!(
            (last.x - x).abs() < 0.01 && (last.y - y).abs() < 0.01,
            "scale={scale}, held-button client coordinates expected ({x}, {y}), got {last:?}"
        );
    }
    session.door().button("left", false).unwrap();
    last = wait_event(&session, last.sequence, "release");

    // Smithay delivers data-device coordinates without calling PointerTarget.
    // Check this distinct path as well as pointer delivery before and after it.
    session.door().button("right", true).unwrap();
    last = wait_event(&session, last.sequence, "press");
    poll_until(EVENT, "client to request internal drag", || {
        session.client_log(PROBE).contains("started internal drag").then_some(())
    })
    .unwrap();
    session.door().motion(origin.0 + start.0 * scale, origin.1 + start.1 * scale).unwrap();
    last = wait_event(&session, last.sequence, "dnd-enter");
    assert!((last.x - start.0).abs() < 0.01 && (last.y - start.1).abs() < 0.01, "DnD enter: {last:?}");
    poll_until(EVENT, "drag action negotiated", || {
        session.client_log(PROBE).contains("drag copy accepted").then_some(())
    })
    .unwrap();
    for (x, y) in [(120.0, 90.0), (170.0, 100.0), (35.0, 45.0)] {
        session.door().motion(origin.0 + x * scale, origin.1 + y * scale).unwrap();
        last = wait_event(&session, last.sequence, "dnd-motion");
        assert!(
            (last.x - x).abs() < 0.01 && (last.y - y).abs() < 0.01,
            "scale={scale}, data-device coordinates expected ({x}, {y}), got {last:?}"
        );
    }
    session.door().button("right", false).unwrap();
    last = wait_event(&session, last.sequence, "dnd-drop");
    session.door().motion(origin.0 + start.0 * scale, origin.1 + start.1 * scale).unwrap();
    last = wait_event(&session, last.sequence, "motion");
    assert!((last.x - start.0).abs() < 0.01 && (last.y - start.1).abs() < 0.01, "hover after DnD: {last:?}");
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn unscaled_drag_coordinates_match_hover() {
    drag_at_scale(1.0, false);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn integer_scaled_drag_coordinates_match_hover() {
    drag_at_scale(2.0, false);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn fractional_scaled_drag_coordinates_match_hover() {
    drag_at_scale(1.5, false);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn unscaled_subsurface_drag_coordinates_match_hover() {
    drag_at_scale(1.0, true);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn integer_scaled_subsurface_drag_coordinates_match_hover() {
    drag_at_scale(2.0, true);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn fractional_scaled_subsurface_drag_coordinates_match_hover() {
    drag_at_scale(1.5, true);
}

fn touch_at_scale(scale: f32, subsurface: bool) {
    let mut session = Session::boot(
        &format!("touch-coordinates-{scale}-{}", if subsurface { "child" } else { "root" }),
        SessionOptions { scale: Some(scale), config_extra: "show_dock = false\n".into(), ..SessionOptions::default() },
    )
    .expect("nested compositor");
    let binary = profile_binary(PROBE).expect("input probe built");
    session
        .launch(binary.to_str().unwrap(), &[&scale.to_string(), if subsurface { "subsurface" } else { "root" }])
        .expect("probe launches");
    let window = session.wait_for_window("input-probe").expect("probe maps");
    session.door().barrier().unwrap();
    let scale = f64::from(scale);
    let offset = if subsurface { (40.0 * scale, 30.0 * scale) } else { (0.0, 0.0) };
    let origin = (f64::from(window.x - window.offset_x) + offset.0, f64::from(window.y - window.offset_y) + offset.1);
    let global = |x, y| (origin.0 + x * scale, origin.1 + y * scale);
    let (x, y) = global(80.0, 70.0);
    session.door().touch_down(0, x, y).unwrap();
    session.door().touch_frame().unwrap();
    let mut last = wait_event(&session, 0, "touch-down-0");
    assert!((last.x - 80.0).abs() < 0.01 && (last.y - 70.0).abs() < 0.01, "touch down: {last:?}");

    let (x, y) = global(100.0, 90.0);
    session.door().touch_down(1, x, y).unwrap();
    session.door().touch_frame().unwrap();
    last = wait_event(&session, last.sequence, "touch-down-1");
    assert!((last.x - 100.0).abs() < 0.01 && (last.y - 90.0).abs() < 0.01, "second touch: {last:?}");

    for (slot, x, y) in [(0, 120.0, 90.0), (1, 170.0, 100.0), (0, 420.0, 90.0), (1, 15.0, -10.0)] {
        let (gx, gy) = global(x, y);
        session.door().touch_motion(slot, gx, gy).unwrap();
        session.door().touch_frame().unwrap();
        last = wait_event(&session, last.sequence, &format!("touch-motion-{slot}"));
        assert!(
            (last.x - x).abs() < 0.01 && (last.y - y).abs() < 0.01,
            "scale={scale}, touch slot {slot} expected ({x}, {y}), got {last:?}"
        );
    }

    // Lifting one finger must not end the remaining finger's grab.
    session.door().touch_up(0).unwrap();
    session.door().touch_frame().unwrap();
    last = wait_event(&session, last.sequence, "touch-up-0");
    let (x, y) = global(35.0, 45.0);
    session.door().touch_motion(1, x, y).unwrap();
    session.door().touch_frame().unwrap();
    last = wait_event(&session, last.sequence, "touch-motion-1");
    assert!((last.x - 35.0).abs() < 0.01 && (last.y - 45.0).abs() < 0.01, "remaining touch: {last:?}");
    session.door().touch_cancel().unwrap();
    last = wait_event(&session, last.sequence, "touch-cancel");

    // A device may still emit a sample for the cancelled physical contact.
    // It must not reach the previous focus (especially across session lock).
    // The client's subsequent fresh down proves ordering without a sleep-based
    // negative assertion; a stale motion makes the probe fail its slot check.
    session.door().touch_motion(1, x, y).unwrap();
    session.door().touch_frame().unwrap();

    // Cancellation releases slot IDs and the implicit grab for a fresh sequence.
    session.door().touch_down(0, x, y).unwrap();
    session.door().touch_frame().unwrap();
    last = wait_event(&session, last.sequence, "touch-down-0");
    assert!((last.x - 35.0).abs() < 0.01 && (last.y - 45.0).abs() < 0.01, "fresh touch: {last:?}");
    session.door().touch_up(0).unwrap();
    session.door().touch_frame().unwrap();
    wait_event(&session, last.sequence, "touch-up-0");
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn unscaled_touch_coordinates_and_slots() {
    touch_at_scale(1.0, false);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn integer_scaled_touch_coordinates_and_slots() {
    touch_at_scale(2.0, false);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn fractional_scaled_touch_coordinates_and_slots() {
    touch_at_scale(1.5, false);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn unscaled_subsurface_touch_coordinates_and_slots() {
    touch_at_scale(1.0, true);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn integer_scaled_subsurface_touch_coordinates_and_slots() {
    touch_at_scale(2.0, true);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn fractional_scaled_subsurface_touch_coordinates_and_slots() {
    touch_at_scale(1.5, true);
}
