//! `wlr-output-management` against the real command-line client.
//!
//! Registry enumeration proves only that the global can be bound. This
//! test makes `wlr-randr` consume a complete head/mode publication,
//! applies a fractional scale, reads that state back through a second
//! manager, and checks that an unsupported disable is rejected without
//! costing the desktop. Those are the three distinct server paths:
//! announce, apply/update, and fail.

use std::process::Command;
use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions};

const CLIENT: &str = "wlr-randr";
const WAIT: Duration = Duration::from_secs(10);

fn run(session: &mut Session, args: &[&str]) -> Result<String, String> {
    session.launch(CLIENT, args)?;
    let status = poll_until(WAIT, "wlr-randr to finish", || {
        session.client_status(CLIENT).ok().flatten()
    })?;
    let report = session.client_log(CLIENT);
    if status.success() {
        Ok(report)
    } else {
        Err(format!("wlr-randr exited with {status}: {report}"))
    }
}

#[test]
#[ignore = "needs a live Wayland session and wlr-randr"]
// This ignored integration test runs on Cargo's test thread, never the
// compositor repaint thread. The synchronous probe is only an
// availability check before the real client is supervised by Session.
#[allow(clippy::disallowed_methods)]
fn wlr_randr_lists_applies_and_observes_output_state() {
    if Command::new(CLIENT).arg("--help").output().is_err() {
        eprintln!("SKIP: wlr-randr is not installed");
        return;
    }

    let mut session = Session::boot("output-management", SessionOptions::default()).unwrap();
    let initial = run(&mut session, &["--json"]).expect("initial output listing succeeds");
    let initial: serde_json::Value =
        serde_json::from_str(&initial).unwrap_or_else(|error| panic!("wlr-randr did not return JSON: {error}"));
    assert_eq!(initial[0]["name"], "chonkstep");

    run(&mut session, &["--output", "chonkstep", "--scale", "1.5"])
        .expect("fractional output scale applies");
    let updated = run(&mut session, &["--json"]).expect("updated output listing succeeds");
    let updated: serde_json::Value =
        serde_json::from_str(&updated).unwrap_or_else(|error| panic!("wlr-randr did not return JSON: {error}"));
    assert_eq!(updated[0]["name"], "chonkstep");
    assert_eq!(updated[0]["scale"], 1.5);

    session.launch(CLIENT, &["--output", "chonkstep", "--off"]).unwrap();
    let status = poll_until(WAIT, "unsupported output disable to be answered", || {
        session.client_status(CLIENT).ok().flatten()
    })
    .unwrap();
    assert!(!status.success(), "wlr-randr must report the compositor's refusal");
    assert!(
        session.client_log(CLIENT).contains("failed"),
        "the client should explain that the configuration failed"
    );
    assert!(session.compositor_alive(), "a refused configuration keeps the desktop alive");
}
