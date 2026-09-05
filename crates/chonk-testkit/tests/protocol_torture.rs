//! The systematic sweep over untrusted client input.
//!
//! Every bound in this compositor arrived one incident at a time — an
//! absurd window geometry that panicked the decoration allocator, a
//! subsurface chain deep enough to walk the compositor off its own
//! stack, protocol ledgers with no ceiling. Each got a regression test
//! of its own, and nothing swept the surface for the next one.
//!
//! This is the sweep. Every strategy in `chonk-protocol-torture` runs
//! against a live session, and each one is held to the same contract:
//!
//! 1. the compositor survives and keeps answering;
//! 2. whatever the offender is told, it is the only casualty;
//! 3. an innocent client on the same desktop keeps its window.
//!
//! Point 3 is the one worth the harness. A bound that protects the
//! compositor by killing every connection is not a bound, it is a
//! different outage, and a per-incident test that only checks "the
//! session survived" would not notice.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(15);

/// Every strategy the probe implements, with the magnitude to drive it
/// at. Adding a bound to the compositor should add a line here.
const STRATEGIES: &[(&str, &str)] = &[
    // The two the issue names as prior art, seeded from the incidents
    // they were filed from.
    ("absurd-geometry", "1"),
    ("deep-subsurface", "4096"),
    // The ledgers, and the role lifecycle that outlives its hooks.
    ("object-flood", "8192"),
    ("role-churn", "256"),
];

/// The same strategies at a tenth of the size, for the soak.
///
/// Peak magnitude and repetition are different questions and want
/// different numbers. The sweep above asks "does one unreasonable
/// client get refused"; the soak asks "does the tenth reasonable-sized
/// offender cost more than the first", which is what a ledger that
/// never shrinks looks like. Driving the soak at peak magnitude
/// measures neither: the compositor spends so long refusing that a
/// responsiveness check cannot tell a leak from a busy pass.
const SOAK_STRATEGIES: &[(&str, &str)] = &[
    ("absurd-geometry", "1"),
    ("deep-subsurface", "256"),
    ("object-flood", "512"),
    ("role-churn", "64"),
];

/// The compositor's resident set in kB, read the way `memory.rs` reads
/// the heap: straight out of `/proc`, no instrumentation in the binary.
fn compositor_rss_kb(pid: u32) -> Option<usize> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
}

/// The innocent bystander: a real window, mapped before the abuse and
/// still there after it.
fn launch_bystander(session: &mut Session) {
    let probe = profile_binary("chonk-fullscreen-probe").expect("the fullscreen probe is built");
    session
        .launch(probe.to_str().unwrap(), &["bystander", "bystander"])
        .expect("the bystander launches");
    session.wait_for_window("bystander").expect("the bystander maps");
}

/// One session, every strategy in turn, one bystander throughout.
///
/// Deliberately one session rather than one per strategy: a bound that
/// leaks — a ledger that never shrinks, a hook that outlives its role —
/// shows up as the *fifth* client failing, not the first, and a fresh
/// compositor per case would hide exactly that.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn no_hostile_client_can_take_the_desktop_with_it() {
    let torture = profile_binary("chonk-protocol-torture").expect("the torture probe is built");
    let torture = torture.to_string_lossy().to_string();
    let mut session = Session::boot(
        "protocol-torture",
        SessionOptions { scale: Some(1.0), ..SessionOptions::default() },
    )
    .expect("the nested compositor boots");

    launch_bystander(&mut session);
    let before = session.wait_for_window("bystander").expect("the bystander is mapped");

    for (index, (strategy, magnitude)) in STRATEGIES.iter().enumerate() {
        session.launch(&torture, &[strategy, magnitude]).expect("the torture client launches");
        // Client logs are numbered by launch order; the bystander was 0.
        let log = format!("client-{}-chonk-protocol-torture.log", index + 1);
        poll_until(EVENT, &format!("{strategy} to finish its run"), || {
            let text = std::fs::read_to_string(session.dir.join(&log)).ok()?;
            text.contains("**done**").then_some(())
        })
        .unwrap_or_else(|timeout| {
            panic!(
                "{timeout}\n{}",
                std::fs::read_to_string(session.dir.join(&log)).unwrap_or_default()
            )
        });

        // 1. The compositor is still answering.
        session
            .door()
            .barrier()
            .unwrap_or_else(|error| panic!("the compositor stopped answering after {strategy}: {error}"));

        // 3. And the bystander still has its window, unchanged.
        let after = session
            .wait_for_window("bystander")
            .unwrap_or_else(|error| panic!("{strategy} took the bystander's window with it: {error}"));
        assert_eq!(
            (after.w, after.h),
            (before.w, before.h),
            "{strategy} must not resize an unrelated client's window"
        );
    }

    // Nothing in the whole run may have produced a compositor panic —
    // the marker `chonkstep-wayland`'s hook writes, which a `SIGSEGV`
    // would not even reach but a `SIGABRT` would.
    let log = session.log();
    assert!(
        !log.contains("compositor panicked"),
        "a hostile client panicked the compositor:\n{log}"
    );
}

/// The soak variant: the same abuse, repeated, watching for a ledger
/// that grows without bound rather than a crash.
///
/// Separately `#[ignore]`d and not part of the ordinary run — it is
/// minutes, not seconds, and its value is in being available when a
/// bound is suspected of leaking rather than in gating every push.
#[test]
#[ignore = "soak: minutes, run deliberately — cargo test -p chonk-testkit --test protocol_torture -- --ignored soak"]
fn soak_repeated_abuse_does_not_grow_the_compositor_without_bound() {
    let torture = profile_binary("chonk-protocol-torture").expect("the torture probe is built");
    let torture = torture.to_string_lossy().to_string();
    let mut session = Session::boot(
        "protocol-torture-soak",
        SessionOptions { scale: Some(1.0), ..SessionOptions::default() },
    )
    .expect("the nested compositor boots");
    launch_bystander(&mut session);

    let rounds = 12;
    let mut launched = 1;
    let mut high_water = 0;
    for round in 0..rounds {
        for (strategy, magnitude) in SOAK_STRATEGIES {
            session.launch(&torture, &[strategy, magnitude]).expect("the torture client launches");
            let log = format!("client-{launched}-chonk-protocol-torture.log");
            launched += 1;
            let _ = poll_until(EVENT, "the round to finish", || {
                let text = std::fs::read_to_string(session.dir.join(&log)).ok()?;
                text.contains("**done**").then_some(())
            });
        }
        session.door().barrier().expect("the compositor answers between rounds");
        // The compositor's own resident set, read the way `memory.rs`
        // reads it. A bound that leaks shows here as a line that keeps
        // climbing after the first couple of rounds have warmed the
        // allocator.
        if let Some(rss) = compositor_rss_kb(session.compositor_pid()) {
            if round >= 2 {
                assert!(
                    rss < high_water * 2,
                    "round {round}: the compositor's RSS doubled under repeated abuse \
                     ({high_water} kB -> {rss} kB), which is a ledger that does not shrink"
                );
            }
            high_water = high_water.max(rss);
        }
    }
    session.wait_for_window("bystander").expect("the bystander survives the soak");
}
