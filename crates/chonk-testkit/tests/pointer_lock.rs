//! `zwp_locked_pointer_v1`, against a live session.
//!
//! A locked pointer does not move, and the protocol is explicit that
//! the compositor must stop sending `wl_pointer.motion` for the
//! duration — the client reads `zwp_relative_pointer_v1` instead. This
//! compositor pinned the position and then sent it anyway, on every
//! event, so an application integrating the relative stream was told an
//! absolute position it never asked to move to as well. That is the
//! shape of a first-person camera drifting and a 3D viewport spinning.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(10);

fn counts_after_arming(session: &Session) -> Option<(u32, u32)> {
    let log = session.client_log("chonk-pointer-lock-probe");
    let armed = log.split("**armed ").nth(1)?.split("**").next()?;
    let base_motions: u32 = armed.split("motions=").nth(1)?.split_whitespace().next()?.parse().ok()?;
    let base_relatives: u32 = armed.split("relatives=").nth(1)?.parse().ok()?;
    // Everything the probe reported after it armed.
    let tail = log.split("**armed ").nth(1)?;
    let motions = tail
        .rsplit("**motion ")
        .next()
        .and_then(|rest| rest.split("**").next())
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(base_motions);
    let relatives = tail
        .rsplit("**relative ")
        .next()
        .and_then(|rest| rest.split("**").next())
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(base_relatives);
    Some((motions.saturating_sub(base_motions), relatives.saturating_sub(base_relatives)))
}

/// The load-bearing assertion: relative motion keeps flowing, absolute
/// motion stops.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_locked_pointer_is_told_relative_motion_and_not_absolute() {
    let mut session =
        Session::boot("pointer-lock", SessionOptions::default()).expect("nested compositor boots");
    let probe = profile_binary("chonk-pointer-lock-probe").expect("pointer-lock probe built");
    session.launch(probe.to_str().unwrap(), &[]).expect("the probe launches");
    let window = session.wait_for_window("pointer-lock").expect("the probe maps");

    // Put the pointer over the surface: a lock is granted only to the
    // surface that holds pointer focus.
    session
        .door()
        .motion(window.x as f64 + window.w as f64 / 2.0, window.y as f64 + window.h as f64 / 2.0)
        .expect("the pointer moves over the probe");
    poll_until(EVENT, "the probe to take the lock", || {
        session.client_log("chonk-pointer-lock-probe").contains("**armed ").then_some(())
    })
    .expect("the probe locks the pointer once it has focus");

    // Relative motion, through the virtual pointer — the one route that
    // reaches the constraint block on this backend, since the nested
    // winit backend emits no relative events of its own.
    for _ in 0..8 {
        session.door().motion_relative(9.0, 7.0).expect("relative motion injects");
    }
    session.door().barrier().expect("the compositor settles");

    let (motions, relatives) = poll_until(EVENT, "the probe to report what it received", || {
        counts_after_arming(&session).filter(|(_, relatives)| *relatives > 0)
    })
    .expect("a locked client must still receive relative motion");

    assert!(relatives > 0, "the relative stream is what a locked client reads");
    assert_eq!(
        motions, 0,
        "a locked pointer did not move, so wl_pointer.motion must not be sent: \
         {motions} absolute events arrived alongside {relatives} relative ones"
    );
}
