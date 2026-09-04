//! Wire-level lifecycle coverage for `hyprland-focus-grab-v1`, the
//! Omarchy/Quickshell popup-dismissal protocol.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh"]
fn inert_grabs_surface_death_and_supersession_keep_one_exact_active_grab() {
    let mut session = Session::boot("focus-grab-lifecycle", SessionOptions::default()).unwrap();
    let probe = profile_binary("chonk-focus-grab-probe")
        .expect("cargo build -p chonk-testkit builds the focus-grab probe");
    session
        .launch(probe.to_str().unwrap(), &[])
        .expect("the focus-grab probe launches");

    let report = poll_until(
        Duration::from_secs(10),
        "the focus-grab lifecycle to complete",
        || {
            let report = session.client_log("chonk-focus-grab-probe");
            report.contains("**first cleared").then_some(report)
        },
    )
    .expect("the focus-grab probe should complete its wire lifecycle");
    assert!(
        report.contains("**first cleared 1; successor cleared 1**"),
        "supersession and implicit surface removal each owe exactly one event: {report}"
    );
    assert!(
        session.compositor_alive(),
        "the compositor survives the lifecycle"
    );
}
