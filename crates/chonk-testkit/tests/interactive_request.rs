//! Real xdg_toplevel move/resize authorization.
//!
//! The xdg-shell serial is an authority token, not decoration: only the
//! client that owns the currently active implicit pointer grab may turn it
//! into an interactive window-manager drag.  These tests drive the actual
//! client request and observe the real window ledger, with display/test-door
//! fences establishing order instead of sleeps.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions, WindowInfo};

const EVENT: Duration = Duration::from_secs(10);
const PROBE: &str = "chonk-input-probe";
const F1: u32 = 59;
const F2: u32 = 60;
const F3: u32 = 61;

fn boot(operation: &str) -> (Session, String) {
    let app = format!("interactive-{operation}");
    let mut session = Session::boot(
        &app,
        SessionOptions {
            config_extra: "show_dock = false\nomarchy_menu = false\nhyprland_config = false\n"
                .into(),
            env: vec![(
                "RUST_LOG".into(),
                "info,wm_core::manager=debug,wm_wayland::xdg=debug".into(),
            )],
            ..Default::default()
        },
    )
    .unwrap();
    let binary = profile_binary(PROBE).unwrap();
    session
        .launch_isolated(
            binary.to_str().unwrap(),
            &[
                "1",
                &format!("--interactive={operation}"),
                &format!("--app-id={app}"),
            ],
        )
        .unwrap();
    let window = session.wait_for_window(&app).unwrap();
    session
        .door()
        .click(f64::from(window.x + 80), f64::from(window.y + 80))
        .unwrap();
    wait_for_prefix(&session, "keyboard enter");
    (session, app)
}

fn wait_for_prefix(session: &Session, prefix: &str) {
    poll_until(EVENT, prefix, || {
        session
            .client_log(PROBE)
            .lines()
            .any(|line| line.starts_with(prefix))
            .then_some(())
    })
    .unwrap_or_else(|error| panic!("{error}; {}", session.client_log(PROBE)));
}

fn window(session: &mut Session, app: &str) -> WindowInfo {
    session
        .world()
        .unwrap()
        .window_matching(app)
        .unwrap_or_else(|| panic!("missing {app:?} from compositor ledger"))
        .clone()
}

fn exercise_invalid(operation: &str, key: u32) -> (WindowInfo, WindowInfo, String, String) {
    let (mut session, app) = boot(operation);
    let before = window(&mut session, &app);
    session.door().tap_key(key).unwrap();
    wait_for_prefix(&session, &format!("interactive fence {key} "));
    // The display fence orders the protocol request, and this door fence
    // orders the queued wm-core pass before the motion below.
    session.door().barrier().unwrap();
    let compositor_log = session.log();
    session
        .door()
        .motion(f64::from(before.x + 180), f64::from(before.y + 150))
        .unwrap();
    session.door().barrier().unwrap();
    let after = window(&mut session, &app);
    // Also ends the drag in the known-bad baseline, keeping teardown evidence
    // unambiguous even though Session::drop would isolate it regardless.
    session.door().button("left", false).unwrap();
    (before, after, session.client_log(PROBE), compositor_log)
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn a_zero_serial_cannot_begin_an_xdg_move() {
    let (before, after, client_log, compositor_log) = exercise_invalid("move", F2);
    assert!(
        !compositor_log.contains("interactive move begun"),
        "serial zero was admitted as a WM drag\n{compositor_log}"
    );
    assert_eq!(
        (after.x, after.y),
        (before.x, before.y),
        "serial zero moved the window\n{client_log}"
    );
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn a_released_pointer_serial_cannot_begin_an_xdg_move() {
    // boot() focused the window with a complete press/release, so F1 names a
    // real serial from this client but no longer names an active grab.
    let (before, after, client_log, compositor_log) = exercise_invalid("move", F1);
    assert!(
        !compositor_log.contains("interactive move begun"),
        "a released pointer serial was admitted as a WM drag\n{compositor_log}"
    );
    assert_eq!(
        (after.x, after.y),
        (before.x, before.y),
        "a stale pointer serial moved the window\n{client_log}"
    );
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn a_zero_serial_cannot_begin_an_xdg_resize() {
    let (before, after, client_log, compositor_log) = exercise_invalid("resize", F2);
    assert!(
        !compositor_log.contains("interactive resize begun"),
        "serial zero was admitted as a WM resize drag\n{compositor_log}"
    );
    assert_eq!(
        (after.w, after.h),
        (before.w, before.h),
        "serial zero resized the window\n{client_log}"
    );
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn a_wrong_serial_cannot_borrow_a_current_pointer_grab() {
    let (mut session, app) = boot("move");
    let before = window(&mut session, &app);
    session
        .door()
        .motion(f64::from(before.x + 80), f64::from(before.y + 80))
        .unwrap();
    session.door().button("left", true).unwrap();
    wait_for_prefix(&session, "interactive pointer serial ");
    session.door().tap_key(F3).unwrap();
    wait_for_prefix(&session, &format!("interactive fence {F3} "));
    session.door().barrier().unwrap();
    let compositor_log = session.log();
    session
        .door()
        .motion(f64::from(before.x + 180), f64::from(before.y + 150))
        .unwrap();
    session.door().barrier().unwrap();
    let after = window(&mut session, &app);
    session.door().button("left", false).unwrap();
    assert!(
        !compositor_log.contains("interactive move begun"),
        "a different serial borrowed the active pointer grab\n{compositor_log}"
    );
    assert_eq!((after.x, after.y), (before.x, before.y));
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn one_client_cannot_spend_another_clients_active_serial() {
    let mut session = Session::boot(
        "interactive-cross-client",
        SessionOptions {
            config_extra: "show_dock = false\nomarchy_menu = false\nhyprland_config = false\n"
                .into(),
            env: vec![(
                "RUST_LOG".into(),
                "info,wm_core::manager=debug,wm_wayland::xdg=debug".into(),
            )],
            ..Default::default()
        },
    )
    .unwrap();
    let trigger = session.dir.join("cross-client-serial");
    let binary = profile_binary(PROBE).unwrap();
    session
        .launch_isolated(
            binary.to_str().unwrap(),
            &[
                "1",
                "--interactive=move",
                "--app-id=serial-borrower",
                &format!("--auto-serial-file={}", trigger.display()),
            ],
        )
        .unwrap();
    let borrower = session.wait_for_window("serial-borrower").unwrap();
    session
        .launch_isolated(
            binary.to_str().unwrap(),
            &["1", "--interactive=move", "--app-id=serial-owner"],
        )
        .unwrap();
    let owner = session.wait_for_window("serial-owner").unwrap();
    session
        .door()
        .motion(f64::from(owner.x + 80), f64::from(owner.y + 80))
        .unwrap();
    session.door().button("left", true).unwrap();
    wait_for_prefix(&session, "interactive pointer serial ");
    let serial = session
        .client_log(PROBE)
        .lines()
        .rev()
        .find_map(|line| {
            line.strip_prefix("interactive pointer serial ")?
                .parse::<u32>()
                .ok()
        })
        .expect("serial reported by its owning client");
    std::fs::write(&trigger, serial.to_string()).unwrap();

    let admitted = poll_until(EVENT, "a decision on the cross-client serial", || {
        let log = session.log();
        if log.contains("ignored xdg move without an authorized pointer grab") {
            Some(false)
        } else if log.contains("interactive move begun") {
            Some(true)
        } else {
            None
        }
    })
    .unwrap_or_else(|error| panic!("{error}; {}", session.log()));
    session
        .door()
        .motion(f64::from(owner.x + 180), f64::from(owner.y + 150))
        .unwrap();
    session.door().barrier().unwrap();
    let borrower_after = window(&mut session, "serial-borrower");
    session.door().button("left", false).unwrap();

    assert!(
        !admitted,
        "another client spent serial {serial}\n{}",
        session.log()
    );
    assert_eq!(
        (borrower_after.x, borrower_after.y),
        (borrower.x, borrower.y),
        "the borrower moved using another client's click"
    );
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn a_drag_and_drop_grab_cannot_be_reused_as_a_window_move() {
    let app = "interactive-dnd";
    let mut session = Session::boot(
        app,
        SessionOptions {
            config_extra: "show_dock = false\nomarchy_menu = false\nhyprland_config = false\n"
                .into(),
            env: vec![(
                "RUST_LOG".into(),
                "info,wm_core::manager=debug,wm_wayland::xdg=debug".into(),
            )],
            ..Default::default()
        },
    )
    .unwrap();
    let binary = profile_binary(PROBE).unwrap();
    session
        .launch_isolated(
            binary.to_str().unwrap(),
            &["1", "dnd", "--interactive=move", "--app-id=interactive-dnd"],
        )
        .unwrap();
    let before = session.wait_for_window(app).unwrap();
    session
        .door()
        .click(f64::from(before.x + 80), f64::from(before.y + 80))
        .unwrap();
    wait_for_prefix(&session, "keyboard enter");
    session
        .door()
        .motion(f64::from(before.x + 80), f64::from(before.y + 80))
        .unwrap();
    session.door().button("right", true).unwrap();
    wait_for_prefix(&session, "started internal drag");
    wait_for_prefix(&session, "interactive pointer serial ");
    session.door().tap_key(F1).unwrap();
    wait_for_prefix(&session, &format!("interactive fence {F1} "));
    session.door().barrier().unwrap();
    let compositor_log = session.log();
    session
        .door()
        .motion(f64::from(before.x + 180), f64::from(before.y + 150))
        .unwrap();
    session.door().barrier().unwrap();
    let after = window(&mut session, app);
    session.door().button("right", false).unwrap();

    assert!(
        !compositor_log.contains("interactive move begun"),
        "a DnD serial was admitted as a window move\n{compositor_log}"
    );
    assert_eq!((after.x, after.y), (before.x, before.y));
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn a_current_pointer_grab_can_begin_an_xdg_move() {
    let (mut session, app) = boot("move");
    let before = window(&mut session, &app);
    session
        .door()
        .motion(f64::from(before.x + 80), f64::from(before.y + 80))
        .unwrap();
    session.door().button("left", true).unwrap();
    wait_for_prefix(&session, "interactive pointer serial ");
    session.door().tap_key(F1).unwrap();
    wait_for_prefix(&session, &format!("interactive fence {F1} "));
    session
        .door()
        .motion(f64::from(before.x + 180), f64::from(before.y + 150))
        .unwrap();
    let after = poll_until(EVENT, "the authorized interactive move", || {
        let candidate = window(&mut session, &app);
        ((candidate.x, candidate.y) != (before.x, before.y)).then_some(candidate)
    })
    .unwrap_or_else(|error| panic!("{error}; {}", session.client_log(PROBE)));
    session.door().button("left", false).unwrap();
    assert_eq!((after.x - before.x, after.y - before.y), (100, 70));
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn a_current_pointer_grab_can_begin_an_xdg_resize() {
    let (mut session, app) = boot("resize");
    let before = window(&mut session, &app);
    session
        .door()
        .motion(
            f64::from(before.x + before.w as i32 - 10),
            f64::from(before.y + before.h as i32 - 10),
        )
        .unwrap();
    session.door().button("left", true).unwrap();
    wait_for_prefix(&session, "interactive pointer serial ");
    session.door().tap_key(F1).unwrap();
    wait_for_prefix(&session, &format!("interactive fence {F1} "));
    session
        .door()
        .motion(
            f64::from(before.x + before.w as i32 + 90),
            f64::from(before.y + before.h as i32 + 60),
        )
        .unwrap();
    let after = poll_until(EVENT, "the authorized interactive resize", || {
        let candidate = window(&mut session, &app);
        (candidate.w > before.w && candidate.h > before.h).then_some(candidate)
    })
    .unwrap_or_else(|error| panic!("{error}; {}", session.client_log(PROBE)));
    session.door().button("left", false).unwrap();
    // The request is anchored on the frame while the probe reports content;
    // exact deltas include the server decoration extents. The important
    // contract here is that both axes substantially follow the gesture.
    assert!(after.w >= before.w + 80, "{before:?} -> {after:?}");
    assert!(after.h >= before.h + 40, "{before:?} -> {after:?}");
}
