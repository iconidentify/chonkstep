//! Invisible Wayland clients cannot schedule visible work.
//!
//! A frame-callback-driven application naturally pauses when its
//! workspace is parked, so it cannot expose an over-eager commit
//! handler. This test uses the fullscreen probe's self-timed mode:
//! the client keeps issuing real `wl_surface.commit` requests at about
//! 60 Hz and reports its own progress. Damage telemetry then proves
//! the same commits draw while visible and schedule zero frames after
//! Omarchy's silent-send chord parks the window.

use std::time::Duration;

use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions};

const SETTLE: Duration = Duration::from_secs(10);

fn animation_frame(log: &str) -> Option<u64> {
    log.lines()
        .filter_map(|line| line.strip_prefix("animation frame=")?.parse().ok())
        .next_back()
}

fn wait_for_animation(session: &Session, program: &str, target: u64) {
    poll_until(SETTLE, &format!("the self-timed client to reach frame {target}"), || {
        animation_frame(&session.client_log(program)).filter(|frame| *frame >= target)
    })
    .unwrap_or_else(|error| panic!("{error}; client log:\n{}", session.client_log(program)));
}

fn rendered_frames(session: &Session) -> usize {
    session.log().lines().filter(|line| line.contains("frame damage")).count()
}

/// Omarchy's `super+shift+alt+2`: send the focused window to workspace
/// two without following it. The barriers make the before/after frame
/// counters strict boundaries rather than timer-dependent samples.
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

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_self_timed_client_on_a_parked_workspace_schedules_no_frames() {
    let options = SessionOptions {
        config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
        env: vec![("CHONKSTEP_DAMAGE_LOG".into(), "1".into())],
        ..Default::default()
    };
    let mut session = Session::boot("hidden-surface-damage", options).expect("session boots");
    let probe = profile_binary("chonk-fullscreen-probe").expect("probe is built");
    let program = probe.display().to_string();
    session
        .launch(&program, &["HiddenPulse", "hidden-pulse", "animate"])
        .expect("self-timed probe launches");
    session.wait_for_window("HiddenPulse").expect("self-timed probe maps");

    wait_for_animation(&session, &program, 30);
    session.door().barrier().expect("visible sample starts at a rendered boundary");
    let visible_start = rendered_frames(&session);
    let visible_target = animation_frame(&session.client_log(&program)).unwrap() + 60;
    wait_for_animation(&session, &program, visible_target);
    session.door().barrier().expect("visible sample ends at a rendered boundary");
    let visible_frames = rendered_frames(&session) - visible_start;
    assert!(
        visible_frames >= 50,
        "60 independently committed visible frames produced only {visible_frames} renders; compositor log:\n{}",
        session.log()
    );

    send_to_workspace_two(&mut session);
    let hidden_start = rendered_frames(&session);
    let hidden_target = animation_frame(&session.client_log(&program)).unwrap() + 60;
    wait_for_animation(&session, &program, hidden_target);
    let hidden_frames = rendered_frames(&session) - hidden_start;
    eprintln!(
        "self-timed damage sample: {visible_frames} visible render submissions, {hidden_frames} while parked"
    );
    assert_eq!(
        hidden_frames,
        0,
        "a parked client's 60-frame burst scheduled {hidden_frames} invisible renders; compositor log:\n{}",
        session.log()
    );
}

#[test]
fn animation_progress_uses_the_latest_complete_marker() {
    assert_eq!(animation_frame("noise\nanimation frame=30\nanimation frame=60\n"), Some(60));
    assert_eq!(animation_frame("animation frame=not-a-number\n"), None);
}
