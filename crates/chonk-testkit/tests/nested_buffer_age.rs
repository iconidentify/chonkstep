//! Captures temporarily make an offscreen context current. The next
//! nested presentation must restore its EGL surface before querying age.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

#[test]
#[ignore = "needs a nested Wayland session: scripts/e2e.sh --headless --release"]
fn offscreen_captures_do_not_break_the_next_presentations_buffer_age() {
    let mut session = Session::boot("nested-buffer-age", SessionOptions {
        config_extra: "show_dock = false\n".into(),
        env: vec![("CHONKSTEP_DAMAGE_LOG".into(), "1".into())],
        ..Default::default()
    }).expect("nested session boots");
    let probe = profile_binary("chonk-fullscreen-probe").expect("probe is built");
    session.launch(probe.to_str().unwrap(), &["BufferAgeProbe", "buffer-age-probe", "animate"])
        .expect("animated client launches");
    session.wait_for_window("BufferAgeProbe").expect("client maps");
    for capture in 0..4 {
        session.screenshot(&format!("capture-{capture}")).expect("offscreen screencopy succeeds");
        session.door().barrier().expect("presentation continues after capture");
    }
    if session.log().contains("EGL_EXT_buffer_age") {
        poll_until(Duration::from_secs(10), "a retained EGL backbuffer after capture", || {
            session.log().lines().any(|line| {
                line.contains("frame damage") && line.split_whitespace().any(|field| {
                    field.strip_prefix("age=").and_then(|value| value.parse::<usize>().ok())
                        .is_some_and(|age| age > 0)
                })
            }).then_some(())
        }).expect("a driver advertising buffer age must not force every frame to age zero");
    }
    assert!(!session.log().contains("BAD_SURFACE"), "buffer age must be queried with the EGL surface current");
    assert!(session.compositor_alive());
}
