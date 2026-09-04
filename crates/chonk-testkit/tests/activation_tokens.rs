//! Resource-bound coverage for abandoned xdg-activation tokens.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn abandoned_activation_tokens_are_bounded_per_client() {
    let mut session = Session::boot("activation-token-bound", SessionOptions::default()).unwrap();
    let probe = profile_binary("chonk-activation-token-probe")
        .expect("cargo build -p chonk-testkit builds the activation-token probe");
    session.launch(probe.to_str().unwrap(), &[]).expect("the activation-token probe launches");

    let report = poll_until(Duration::from_secs(10), "the token request burst to complete", || {
        let report = session.client_log("chonk-activation-token-probe");
        report.contains("**requested").then_some(report)
    })
    .expect("the probe should complete its wire lifecycle");
    assert!(
        report.contains("**requested 512; completed 512**"),
        "every valid protocol request must receive a completion event: {report}"
    );
    assert_eq!(
        session.door().activation_tokens().unwrap(),
        256,
        "one connection may retain no more than its admission quota"
    );
    assert!(session.compositor_alive(), "the compositor survives the request burst");
}
