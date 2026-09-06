//! Harness evidence must survive client replacement and teardown.

use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions};

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn each_client_launch_keeps_a_unique_log_after_individual_and_bulk_reaping() {
    let mut session =
        Session::boot("session-launch-log-lifetime", SessionOptions::default()).unwrap();
    for (index, output) in ["first launch", "second launch", "third launch"]
        .into_iter()
        .enumerate()
    {
        session.launch("printf", &["%s\n", output]).unwrap();
        poll_until(
            Duration::from_secs(5),
            "the newest child's exact output",
            || (session.client_log("printf") == format!("{output}\n")).then_some(()),
        )
        .unwrap();
        if index == 0 {
            session.kill_client("printf");
        } else {
            session.kill_clients();
        }
    }
    for (index, expected) in ["first launch", "second launch", "third launch"]
        .into_iter()
        .enumerate()
    {
        let log = std::fs::read_to_string(session.dir.join(format!("client-{index}-printf.log")))
            .expect("every launch's evidence remains available after its child is reaped");
        assert_eq!(
            log,
            format!("{expected}\n"),
            "an earlier launch's log was overwritten"
        );
    }
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn isolated_x11_client_uses_only_the_nested_display_and_private_xdg_paths() {
    let mut session =
        Session::boot("session-x11-client-environment", SessionOptions::default()).unwrap();
    // Only these non-sensitive session handles are printed, never the entire
    // inherited environment. Missing printenv variables produce no lines.
    session
        .launch_x11_isolated(
            "printenv",
            &[
                "DISPLAY",
                "GDK_BACKEND",
                "QT_QPA_PLATFORM",
                "XDG_CONFIG_HOME",
                "XDG_CACHE_HOME",
                "XDG_DATA_HOME",
                "XDG_STATE_HOME",
                "WAYLAND_DISPLAY",
                "WAYLAND_SOCKET",
            ],
        )
        .unwrap();
    poll_until(
        Duration::from_secs(5),
        "the isolated environment probe to exit",
        || session.client_status("printenv").ok().flatten(),
    )
    .unwrap();
    let expected = format!(
        "{}\nx11\nxcb\n{}/client-config\n{}/client-cache\n{}/client-data\n{}/client-state\n",
        session.x11_display().unwrap(),
        session.dir.display(),
        session.dir.display(),
        session.dir.display(),
        session.dir.display()
    );
    assert_eq!(
        session.client_log("printenv"),
        expected,
        "a client inherited an unintended desktop handle or non-private XDG path"
    );
    // This display is not just an arbitrary unused name: the harness can
    // connect to the corresponding real nested XWayland server.
    session.connect_x11().unwrap();
}
