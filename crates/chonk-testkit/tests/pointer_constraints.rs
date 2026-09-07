//! Real locked/confined pointer policy at integer and fractional surface scales.
//! Tests request constraints via client key handling, and synchronize on a
//! Wayland display fence after the request—not an arbitrary delay.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(10);
const PROBE: &str = "chonk-input-probe";

#[derive(Clone, Copy, Debug)]
struct Input {
    sequence: u64,
    x: f64,
    y: f64,
}

fn latest(session: &Session, kind: &str) -> Option<Input> {
    session.client_log(PROBE).lines().rev().find_map(|line| {
        let mut fields = line.strip_prefix("input ")?.split_whitespace();
        let sequence = fields.next()?.parse().ok()?;
        if fields.next()? != kind {
            return None;
        }
        Some(Input {
            sequence,
            x: fields.next()?.parse().ok()?,
            y: fields.next()?.parse().ok()?,
        })
    })
}

fn count(session: &Session, exact: &str) -> usize {
    session
        .client_log(PROBE)
        .lines()
        .filter(|line| *line == exact)
        .count()
}

fn wait_line(session: &Session, exact: &str, expected: usize) {
    poll_until(EVENT, exact, || {
        (count(session, exact) >= expected).then_some(())
    })
    .unwrap_or_else(|error| panic!("{error}; {}", session.client_log(PROBE)));
}

fn fence_key(session: &mut Session, key: u32) {
    let fence = format!("constraint fence {key}");
    let expected = count(session, &fence) + 1;
    session.door().tap_key(key).unwrap();
    wait_line(session, &fence, expected);
}

fn boot(mode: &str, scale: f32) -> (Session, (f64, f64)) {
    boot_named(mode, scale, "")
}

fn boot_named(mode: &str, scale: f32, suffix: &str) -> (Session, (f64, f64)) {
    let mut session = Session::boot(
        &format!("pointer-{mode}-{scale}{suffix}"),
        SessionOptions {
            scale: Some(scale),
            config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\nomarchy_menu = false\nhyprland_config = false\n[keybindings]\n\"super+up\" = \"overview\"\n".into(),
            ..Default::default()
        },
    ).unwrap();
    let binary = profile_binary(PROBE).unwrap();
    session
        .launch_isolated(binary.to_str().unwrap(), &[&scale.to_string(), mode])
        .unwrap();
    let window = session.wait_for_window("input-probe").unwrap();
    let origin = (
        f64::from(window.x - window.offset_x),
        f64::from(window.y - window.offset_y),
    );
    session
        .door()
        .click(
            origin.0 + 50.0 * f64::from(scale),
            origin.1 + 60.0 * f64::from(scale),
        )
        .unwrap();
    wait_line(&session, "keyboard enter", 1);
    (session, origin)
}

fn move_to(session: &mut Session, origin: (f64, f64), scale: f32, x: f64, y: f64) -> Input {
    let before = latest(session, "motion").map_or(0, |event| event.sequence);
    session
        .door()
        .motion(
            origin.0 + x * f64::from(scale),
            origin.1 + y * f64::from(scale),
        )
        .unwrap();
    poll_until(EVENT, "the next absolute client motion", || {
        latest(session, "motion").filter(|event| event.sequence > before)
    })
    .unwrap_or_else(|error| panic!("{error}; {}", session.client_log(PROBE)))
}

fn assert_at(event: Input, x: f64, y: f64) {
    assert!(
        (event.x - x).abs() < 0.01 && (event.y - y).abs() < 0.01,
        "expected ({x},{y}), received {event:?}"
    );
}

fn region_activation(scale: f32, confined: bool) {
    let mode = if confined {
        "confine-region"
    } else {
        "lock-region"
    };
    let active = if confined {
        "constraint confined"
    } else {
        "constraint locked"
    };
    let (mut session, origin) = boot(mode, scale);
    // The pointer is on the surface but outside the requested local region
    // [100,220) x [80,180). Activation must respect both input regions.
    fence_key(&mut session, 59);
    assert_eq!(
        count(&session, active),
        0,
        "a surface's focus is not permission to ignore its constraint region"
    );
    assert_at(move_to(&mut session, origin, scale, 70.0, 65.0), 70.0, 65.0);
    assert_eq!(count(&session, active), 0);
    assert_at(
        move_to(&mut session, origin, scale, 110.0, 100.0),
        110.0,
        100.0,
    );
    wait_line(&session, active, 1);
    if confined {
        assert_at(
            move_to(&mut session, origin, scale, 150.0, 120.0),
            150.0,
            120.0,
        );
    } else {
        let before = latest(&session, "motion").unwrap().sequence;
        session.door().motion_relative(13.0, -7.0).unwrap();
        let relative = poll_until(EVENT, "locked relative motion", || {
            latest(&session, "relative")
        })
        .unwrap();
        assert_at(relative, 13.0, -7.0);
        assert_eq!(latest(&session, "motion").unwrap().sequence, before);
    }
    fence_key(&mut session, 61);
    assert_at(move_to(&mut session, origin, scale, 50.0, 60.0), 50.0, 60.0);
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn locked_region_activation_scale_one() {
    region_activation(1.0, false);
}
#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn locked_region_activation_fractional() {
    region_activation(1.5, false);
}
#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn locked_region_activation_scale_two() {
    region_activation(2.0, false);
}
#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn confined_region_activation_scale_one() {
    region_activation(1.0, true);
}
#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn confined_region_activation_fractional() {
    region_activation(1.5, true);
}
#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn confined_region_activation_scale_two() {
    region_activation(2.0, true);
}

fn committed_region_excludes_anchor(confined: bool) {
    let (mode, active, inactive) = if confined {
        (
            "confine-region",
            "constraint confined",
            "constraint unconfined",
        )
    } else {
        ("lock-region", "constraint locked", "constraint unlocked")
    };
    let (mut session, origin) = boot_named(mode, 1.5, "-region-commit");
    fence_key(&mut session, 59);
    assert_at(
        move_to(&mut session, origin, 1.5, 110.0, 100.0),
        110.0,
        100.0,
    );
    wait_line(&session, active, 1);
    // The new [250,330) x [200,260) region excludes the current position.
    // A display fence follows the real region+surface commit. No additional
    // pointer event should be needed to notice its old anchor is now invalid.
    fence_key(&mut session, 62);
    assert_eq!(
        count(&session, inactive),
        1,
        "{}",
        session.client_log(PROBE)
    );
    assert_at(
        move_to(&mut session, origin, 1.5, 260.0, 210.0),
        260.0,
        210.0,
    );
    wait_line(&session, active, 2);
    fence_key(&mut session, 61);
    assert_at(move_to(&mut session, origin, 1.5, 50.0, 60.0), 50.0, 60.0);
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn locked_region_commit_reconciles_before_the_next_mouse_report() {
    committed_region_excludes_anchor(false);
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn confined_region_commit_reconciles_before_the_next_mouse_report() {
    committed_region_excludes_anchor(true);
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn opening_overview_releases_a_game_lock_without_waiting_for_mouse_motion() {
    let (mut session, origin) = boot_named("lock-full", 2.0, "-overview");
    fence_key(&mut session, 59);
    wait_line(&session, "constraint locked", 1);
    session
        .door()
        .chord(chonk_testkit::keys::LEFTMETA, chonk_testkit::keys::UP)
        .unwrap();
    wait_line(&session, "keyboard leave", 1);
    wait_line(&session, "constraint unlocked", 1);
    assert!(
        latest(&session, "leave").is_some(),
        "{}",
        session.client_log(PROBE)
    );
    session.door().tap_key(chonk_testkit::keys::ESC).unwrap();
    wait_line(&session, "keyboard enter", 2);
    wait_line(&session, "constraint locked", 2);
    fence_key(&mut session, 61);
    assert_at(move_to(&mut session, origin, 2.0, 75.0, 65.0), 75.0, 65.0);
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn locked_relative_input_delivers_every_delta_without_repainting_a_static_scene() {
    let (mut session, _) = boot("lock-full", 1.0);
    fence_key(&mut session, 59);
    wait_line(&session, "constraint locked", 1);
    session.door().barrier().unwrap();
    let absolute_before = latest(&session, "motion").map(|event| event.sequence);
    let _ = session.door().frame_stats().unwrap();
    for index in 0..128 {
        let before = latest(&session, "relative").map_or(0, |event| event.sequence);
        let (dx, dy) = (if index % 2 == 0 { 13.0 } else { -13.0 }, -7.0);
        session.door().motion_relative(dx, dy).unwrap();
        let event = poll_until(EVENT, "the next locked relative delta", || {
            latest(&session, "relative").filter(|event| event.sequence > before)
        })
        .unwrap();
        assert_at(event, dx, dy);
    }
    // This ordered query does not force a render. A barrier would manufacture
    // exactly the otherwise unnecessary work this test is intended to detect.
    let stats = session.door().frame_stats().unwrap();
    println!(
        "locked-input: 128 observed deltas; {:?} render attempts; {} submissions; {} dispatches; {} input us",
        stats.render_attempts, stats.render_calls, stats.dispatch_calls, stats.input_us
    );
    assert_eq!(
        latest(&session, "motion").map(|event| event.sequence),
        absolute_before
    );
    assert_eq!(
        stats.render_calls, 0,
        "relative-only input does not change a static scene"
    );
    assert_eq!(
        stats.render_attempts,
        Some(0),
        "even a discarded scene rebuild is unnecessary for relative-only input"
    );
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn full_surface_confinement_does_not_reenter_the_surface_state_lock() {
    let (mut session, origin) = boot("confine-full", 1.0);
    fence_key(&mut session, 59);
    wait_line(&session, "constraint confined", 1);
    assert_at(
        move_to(&mut session, origin, 1.0, 150.0, 120.0),
        150.0,
        120.0,
    );
    fence_key(&mut session, 61);
    assert_at(move_to(&mut session, origin, 1.0, 50.0, 60.0), 50.0, 60.0);
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn held_movement_key_keeps_camera_motion_and_capture_restores_typing_policy() {
    for mode in ["lock-full", "confine-full"] {
        let (mut session, _) = boot_named(mode, 2.0, "-typing");
        assert!(!session.door().touchpad_captured().unwrap());
        fence_key(&mut session, 59);
        poll_until(EVENT, "capture to suspend typing suppression", || {
            session.door().touchpad_captured().ok()?.then_some(())
        }).unwrap();
        session.door().key(17, true).unwrap(); // Hold W throughout camera motion.
        wait_line(&session, "keyboard key 17 down", 1);
        for _ in 0..8 {
            let before = latest(&session, "relative").map_or(0, |event| event.sequence);
            session.door().motion_relative(3.0, 2.0).unwrap();
            poll_until(EVENT, "relative camera motion while W remains held", || {
                latest(&session, "relative").filter(|event| event.sequence > before)
            }).unwrap();
        }
        assert_eq!(count(&session, "keyboard key 17 up"), 0);
        session.door().key(17, false).unwrap();
        fence_key(&mut session, 61); // Destroy the protocol object without mouse motion.
        poll_until(EVENT, "destroying capture to restore typing suppression", || {
            (!session.door().touchpad_captured().ok()?).then_some(())
        }).unwrap();
        fence_key(&mut session, 59);
        assert!(session.door().touchpad_captured().unwrap());
        session.door().chord(chonk_testkit::keys::LEFTMETA, chonk_testkit::keys::UP).unwrap();
        poll_until(EVENT, "overview to restore typing suppression", || {
            (!session.door().touchpad_captured().ok()?).then_some(())
        }).unwrap();
        session.door().tap_key(chonk_testkit::keys::ESC).unwrap();
        poll_until(EVENT, "returning to the captured client", || {
            session.door().touchpad_captured().ok()?.then_some(())
        }).unwrap();
    }
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn locked_relative_burst_preserves_all_scaled_clients_raw_deltas() {
    const REPORTS: usize = 8192;
    let (mut session, _) = boot("lock-full", 2.0);
    fence_key(&mut session, 59);
    wait_line(&session, "constraint locked", 1);
    session.door().barrier().unwrap();
    let absolute_before = latest(&session, "motion").map(|event| event.sequence);
    let _ = session.door().frame_stats().unwrap();
    for index in 0..REPORTS {
        session
            .door()
            .motion_relative(if index % 2 == 0 { 13.0 } else { -13.0 }, -7.0)
            .unwrap();
    }
    // An unpaced burst verifies delivery/backlog correctness, not a measured
    // 8 kHz device rate, native hardware latency or application frame rate.
    let log = poll_until(EVENT, "all burst reports to reach the client", || {
        let log = session.client_log(PROBE);
        (log.lines()
            .filter(|line| line.starts_with("relative raw "))
            .count()
            >= REPORTS)
            .then_some(log)
    })
    .unwrap();
    let received: Vec<_> = log
        .lines()
        .filter_map(|line| {
            let mut fields = line.strip_prefix("input ")?.split_whitespace();
            fields.next()?;
            (fields.next()? == "relative").then(|| {
                (
                    fields.next().unwrap().parse::<f64>().unwrap(),
                    fields.next().unwrap().parse::<f64>().unwrap(),
                )
            })
        })
        .collect();
    assert_eq!(received.len(), REPORTS);
    for (index, observed) in received.into_iter().enumerate() {
        assert_eq!(observed, (if index % 2 == 0 { 13.0 } else { -13.0 }, -7.0));
    }
    let stats = session.door().frame_stats().unwrap();
    println!("locked-input-burst: {REPORTS} exact relative reports at scale 2; {:?} render attempts; {} submissions",
        stats.render_attempts, stats.render_calls);
    assert_eq!(
        latest(&session, "motion").map(|event| event.sequence),
        absolute_before
    );
    assert_eq!(stats.render_attempts, Some(0));
    assert_eq!(stats.render_calls, 0);
}
