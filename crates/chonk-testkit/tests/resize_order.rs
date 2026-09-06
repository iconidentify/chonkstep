//! A stale client commit must not overwrite an unsent compositor configure.

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};
use std::time::Duration;

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn an_old_rendered_commit_cannot_erase_a_staged_ipc_resize() {
    const PROBE: &str = "chonk-resize-order-probe";
    const EVENT: Duration = Duration::from_secs(10);
    for round in 0..16 {
        let mut session = Session::boot(
            &format!("resize-order-{round}"),
            SessionOptions {
                config_extra: "show_dock = false\nomarchy_menu = false\nhyprland_config = false\n"
                    .into(),
                env: vec![
                    ("CHONKSTEP_HYPRLAND_IPC".into(), "1".into()),
                    (
                        "RUST_LOG".into(),
                        "info,wm_wayland::xdg=trace,wm_wayland::backend_impl=trace".into(),
                    ),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        let act = session.dir.join("resize-act");
        let binary = profile_binary(PROBE).unwrap();
        session
            .launch_isolated(binary.to_str().unwrap(), &[act.to_str().unwrap()])
            .unwrap();
        let original = session.wait_for_window("resize-order-probe").unwrap();
        assert_eq!((original.w, original.h), (400, 300));
        session.door().barrier().unwrap();
        std::fs::write(act, b"resize").unwrap();
        let answer = poll_until(EVENT, "the configure answering the IPC resize", || {
            session
                .client_log(PROBE)
                .lines()
                .find(|line| line.starts_with("configure ") && line.ends_with("requested=true"))
                .map(str::to_owned)
        })
        .unwrap_or_else(|error| {
            panic!("{error}; {}\n{}", session.client_log(PROBE), session.log())
        });
        assert_eq!(
            answer,
            "configure 520 380 requested=true",
            "round {round}\n{}\n{}",
            session.client_log(PROBE),
            session.log()
        );
        println!("resize-order round={round}: old commit did not erase 520x380 configure");
    }
}
