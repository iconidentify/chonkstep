//! End-to-end coverage for `zwlr_gamma_control_unstable_v1` — the
//! protocol `wlsunset`, `gammastep` and `redshift` warm the screen
//! through, and until it existed the reason nothing on this desktop
//! could tint a display at all.
//!
//! Two of these tests are about behaviors that protect a user's screen
//! and would be invisible in a unit test, because both are things a
//! *client* is told over a socket:
//!
//! - **Exclusivity.** Only one client at a time may hold an output; the
//!   second is answered `failed`. Without it two night-light daemons
//!   fight over one screen and each undoes the other every few seconds.
//! - **The restore.** A daemon that dies — crashed, killed, or merely
//!   closed — must leave the screen the colour it found it. This is the
//!   failure mode users hate most: an orange display with nothing left
//!   running to explain it.
//!
//! The third pins the *nested* backend's honest answer. There is no
//! crtc inside a window, so a nested session advertises no global and a
//! night-light tool says so and exits (see `wm-wayland/src/gamma.rs`).
//! A future change that advertised the global and quietly dropped the
//! ramps — the one outcome that module rules out, because a tool
//! reporting success while nothing changes is worse than one that fails
//! — would fail this test.
//!
//! # How a nested session gets a gamma ramp to test with
//!
//! `CHONKSTEP_TEST_GAMMA_SIZE` gives the nested backend a stand-in for
//! the crtc it does not have: the global is advertised, the whole
//! protocol runs against the real dispatch code, and the ramps are
//! recorded into the compositor's log rather than scanned out. Test
//! apparatus in the same shape as `CHONKSTEP_TEST_SOCKET` — inert
//! unless a test sets it. The first test below runs *without* it, which
//! is what a person nesting chonkstep actually gets.
//!
//! The client is `chonk-gamma-probe`, this crate's own gamma-control
//! client: it does what `wlsunset` does, in the same order, and prints
//! each step so a test can assert on it instead of on a colour.
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session (or an
//! `Xvfb`) to nest in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};
use std::time::Duration;

/// The ramp length the stand-in advertises. 256 is what the crtc
/// driving this developer's own 4K panel reports, so the tables the
/// probe builds are the size real hardware here asks for.
const RAMP: u32 = 256;

/// The nested session's options, with the gamma stand-in turned on.
fn with_gamma() -> SessionOptions {
    SessionOptions {
        scale: Some(1.0),
        env: vec![("CHONKSTEP_TEST_GAMMA_SIZE".into(), RAMP.to_string())],
        ..Default::default()
    }
}

/// Runs the probe in one of its modes and waits until its own log
/// carries `checkpoint`, returning everything it printed.
fn run_probe(session: &mut Session, args: &[&str], checkpoint: &str) -> String {
    let probe = profile_binary("chonk-gamma-probe").expect("cargo build -p chonk-testkit builds the probe");
    let probe = probe.to_str().unwrap().to_string();
    session.launch(&probe, args).expect("the probe launches");
    poll_until(Duration::from_secs(10), &format!("the probe to report {checkpoint:?}"), || {
        let log = session.client_log("chonk-gamma-probe");
        log.contains(checkpoint).then_some(log)
    })
    .expect("the probe should reach its checkpoint")
}

/// Waits for a compositor log line, so a test asserts on what the
/// compositor did rather than on when it got round to doing it.
fn wait_for_log(session: &mut Session, needle: &str) -> String {
    poll_until(Duration::from_secs(10), &format!("the compositor to log {needle:?}"), || {
        let log = session.log();
        log.contains(needle).then_some(log)
    })
    .unwrap_or_else(|_| panic!("the compositor should log {needle:?}; it logged:\n{}", session.log()))
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_nested_session_says_it_cannot_tint_rather_than_pretending() {
    // No stand-in: this is the session a person previewing chonkstep
    // inside their own desktop gets.
    let mut session =
        Session::boot("gamma-nested", SessionOptions { scale: Some(1.0), ..Default::default() }).unwrap();

    let report = run_probe(&mut session, &["report"], "**no gamma-control global**");
    assert!(
        !report.contains("manager present"),
        "a window has no crtc, so the nested backend must advertise no gamma-control global at all — \
         advertising one and dropping the ramps is the dishonest option gamma.rs rules out"
    );
    assert!(
        session.log().contains("wlr-gamma-control is NOT advertised"),
        "and the compositor should say so in its log, so the absence is diagnosable"
    );
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn only_one_night_light_daemon_at_a_time_owns_a_screen() {
    let mut session = Session::boot("gamma-exclusive", with_gamma()).unwrap();

    let report = run_probe(&mut session, &["exclusive"], "**second claim");
    assert!(
        report.contains(&format!("**gamma_size {RAMP}**")),
        "the first claim must be granted, with the ramp size sent immediately: {report}"
    );
    assert!(
        report.contains("**second claim refused**"),
        "a second claim on the same output must be answered `failed` — this is the whole point of the \
         protocol, and without it two night-light daemons undo each other forever: {report}"
    );
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_night_light_daemon_that_dies_does_not_leave_the_screen_orange() {
    let mut session = Session::boot("gamma-restore", with_gamma()).unwrap();

    // A daemon warms the screen and settles into its steady state.
    let report = run_probe(&mut session, &["hold", "3000"], "**holding**");
    assert!(report.contains(&format!("**gamma_size {RAMP}**")), "the claim is granted: {report}");
    assert!(report.contains("**set_gamma accepted"), "and the warm table is accepted: {report}");

    let warm = wait_for_log(&mut session, "gamma ramp programmed");
    let warm = last_white_point(&warm).expect("the compositor logs the white point it programmed");
    assert_eq!(warm.0, u16::MAX, "3000K leaves red at full");
    assert!(warm.2 < warm.0, "and pulls blue down — that is what warming a screen is: {warm:?}");

    // Now it dies the way a crashing daemon dies: killed, with no
    // chance to put anything back itself.
    session.kill_client("chonk-gamma-probe");

    wait_for_log(&mut session, "restoring the original ramp");
    let restored = poll_until(Duration::from_secs(10), "the restored ramp to be programmed", || {
        let log = session.log();
        last_white_point(&log).filter(|white| *white != warm)
    })
    .expect("the compositor should program a ramp back after the owner died");
    assert_eq!(
        restored,
        (u16::MAX, u16::MAX, u16::MAX),
        "the screen must come back to the neutral white point it had before the daemon claimed it, \
         not stay at the daemon's {warm:?}"
    );

    // And the output is free again: exclusivity released with the
    // client, not held by its ghost.
    let again = run_probe(&mut session, &["report"], "**gamma_size");
    assert!(
        again.contains(&format!("**gamma_size {RAMP}**")),
        "the next daemon must be able to claim the output the dead one held: {again}"
    );
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_wrongly_sized_gamma_table_is_a_protocol_error_not_a_crash() {
    let mut session = Session::boot("gamma-hostile", with_gamma()).unwrap();

    // One byte short of a correct table — the shape that makes a
    // compositor which trusts the length read off the end of its own
    // buffer.
    let report = run_probe(&mut session, &["bad-table"], "**bad table");
    assert!(
        report.contains("refused"),
        "a short table must earn the protocol's `invalid_gamma` error: {report}"
    );
    assert!(
        session.compositor_alive(),
        "and the compositor must survive it — a hostile client may lose its connection, never the session"
    );
}

/// The white point of the most recent `gamma ramp programmed` line: the
/// last entry of each channel, which is what separates a warm screen
/// from a neutral one. Parsed out of the compositor's own log rather
/// than read off the hardware, because there is no hardware behind a
/// nested output — and because the log line is what a person debugging
/// "why is my screen orange" reads too.
fn last_white_point(log: &str) -> Option<(u16, u16, u16)> {
    let line = log.lines().rfind(|line| line.contains("gamma ramp programmed"))?;
    let field = |name: &str| -> Option<u16> {
        line.split(name).nth(1)?.trim_start_matches('=').split(' ').next()?.parse().ok()
    };
    Some((field("white_r")?, field("white_g")?, field("white_b")?))
}
