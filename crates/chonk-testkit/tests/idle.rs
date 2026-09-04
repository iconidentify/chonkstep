//! Idle inhibition follows what the user can actually see.
//!
//! A decorated Wayland toplevel stays protocol-mapped when its
//! workspace is parked; Chonkstep hides its compositor-owned frame.
//! Testing only the toplevel's mapped bit therefore lets an invisible
//! video player suppress Omarchy's lock and suspend indefinitely. This
//! regression binds both real protocols: the window owns an idle
//! inhibitor, while an idle notification proves the inhibitor holds
//! when visible and releases after Omarchy's silent-send chord parks
//! the frame.

use std::time::Duration;

use chonk_testkit::{keys, poll_until, profile_binary, session_dir, Session, SessionOptions};

const SETTLE: Duration = Duration::from_secs(5);

fn send_to_workspace_two(session: &mut Session) {
    let door = session.door();
    door.key(keys::LEFTMETA, true).unwrap();
    door.key(keys::LEFTSHIFT, true).unwrap();
    door.key(keys::LEFTALT, true).unwrap();
    door.barrier().unwrap();
    door.tap_key(keys::TWO).unwrap();
    door.key(keys::LEFTALT, false).unwrap();
    door.key(keys::LEFTSHIFT, false).unwrap();
    door.key(keys::LEFTMETA, false).unwrap();
    door.barrier().unwrap();
}

fn animation_frame(log: &str) -> Option<u64> {
    log.lines()
        .filter_map(|line| line.strip_prefix("animation frame=")?.parse().ok())
        .next_back()
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_parked_window_cannot_inhibit_omarchys_idle_policy() {
    let options = SessionOptions {
        config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
        env: vec![("CHONKSTEP_IDLE_LOG".into(), "1".into())],
        ..Default::default()
    };
    let mut session = Session::boot("parked-idle-inhibitor", options).expect("session boots");
    let probe = profile_binary("chonk-fullscreen-probe").expect("probe is built");
    let program = probe.display().to_string();
    session
        .launch(&program, &["IdleHolder", "idle-holder", "animate-inhibit-idle"])
        .expect("idle inhibitor launches");
    session.wait_for_window("IdleHolder").expect("inhibiting window maps");
    poll_until(SETTLE, "the client to arm both idle protocols", || {
        session.client_log(&program).contains("idle inhibition armed").then_some(())
    })
    .unwrap();

    // At least thirty self-timed commits and three whole notification
    // periods: a visible inhibitor must hold, while pixel-only commits
    // must not trigger policy reconciliation at animation rate.
    poll_until(SETTLE, "the visible inhibitor to commit thirty animation frames", || {
        animation_frame(&session.client_log(&program)).filter(|frame| *frame >= 30)
    })
    .unwrap();
    assert!(
        !session.client_log(&program).contains("idle state=idled"),
        "a visible inhibitor must prevent idle notification"
    );

    send_to_workspace_two(&mut session);
    poll_until(SETTLE, "idle policy to release after the inhibiting frame is parked", || {
        session.client_log(&program).contains("idle state=idled").then_some(())
    })
    .unwrap();
    let compositor_log = std::fs::read_to_string(session_dir("parked-idle-inhibitor").join("compositor.log"))
        .expect("compositor log is readable");
    let reconciliations = compositor_log.matches("idle policy reconciled").count();
    eprintln!("idle policy sample: {reconciliations} reconciliations across 30+ animated commits and one park");
    assert!(
        (3..=4).contains(&reconciliations),
        "startup, inhibitor, and visibility edges should be the only reconciliations (observed {reconciliations})"
    );
    assert!(session.compositor_alive(), "releasing a parked inhibitor keeps the session alive");
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn destroying_one_duplicate_inhibitor_keeps_the_other_until_disconnect() {
    let mut session = Session::boot("duplicate-idle-inhibitor", SessionOptions::default()).expect("session boots");
    assert_eq!(session.door().protocol_ledgers().unwrap().idle, 0);
    let probe = profile_binary("chonk-fullscreen-probe").expect("probe is built");
    let program = probe.display().to_string();
    session
        .launch(
            &program,
            &["DuplicateIdleHolder", "duplicate-idle-holder", "animate-duplicate-inhibit-idle"],
        )
        .expect("duplicate inhibitors launch");
    session.wait_for_window("DuplicateIdleHolder").expect("inhibiting window maps");
    poll_until(SETTLE, "one of two same-surface inhibitors to be destroyed", || {
        session
            .client_log(&program)
            .contains("one duplicate idle inhibitor destroyed")
            .then_some(())
    })
    .unwrap();
    poll_until(SETTLE, "the compositor ledger to retain exactly one inhibitor", || {
        (session.door().protocol_ledgers().ok()?.idle == 1).then_some(())
    })
    .unwrap();
    poll_until(SETTLE, "the surviving inhibitor to hold for thirty animation frames", || {
        animation_frame(&session.client_log(&program)).filter(|frame| *frame >= 30)
    })
    .unwrap();
    assert!(
        !session.client_log(&program).contains("idle state=idled"),
        "destroying one of two inhibitor objects must not release the surviving one"
    );

    session.kill_client(&program);
    poll_until(SETTLE, "client disconnect to clear its idle ledger entry", || {
        (session.door().protocol_ledgers().ok()?.idle == 0).then_some(())
    })
    .unwrap();
    assert!(session.compositor_alive(), "duplicate inhibitor teardown keeps the compositor alive");
}
