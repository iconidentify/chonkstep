//! Clipboard and primary device ownership must end with the client, even if
//! it never sends a protocol destructor. Each batch includes both data-control
//! protocols, whose retained device lists are walked on selection changes.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, SelectionDevices, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(10);
const COUNT: usize = 128;
const PROBE: &str = "chonk-selection-lifecycle-probe";

fn lifecycle(mode: &str) {
    let mut session = Session::boot(
        &format!("selection-devices-{mode}"),
        SessionOptions {
            config_extra: "show_dock = false\nomarchy_menu = false\nhyprland_config = false\n"
                .into(),
            ..Default::default()
        },
    )
    .unwrap();
    // Complete asynchronous XWayland registration before taking the baseline.
    let _ = session.connect_x11().unwrap();
    let baseline = session.door().selection_devices().unwrap();
    assert_eq!(baseline.dead, 0);
    let binary = profile_binary(PROBE).unwrap();
    for cycle in 0..4 {
        let gates = session.dir.join(format!("gates-{cycle}"));
        std::fs::create_dir(&gates).unwrap();
        session
            .launch_isolated(
                binary.to_str().unwrap(),
                &[mode, &COUNT.to_string(), gates.to_str().unwrap()],
            )
            .unwrap();
        let expected = SelectionDevices {
            core: baseline.core + COUNT,
            primary: baseline.primary + COUNT,
            wlr: baseline.wlr + COUNT,
            ext: baseline.ext + COUNT,
            dead: 0,
        };
        poll_until(EVENT, "all four live device kinds to be registered", || {
            (session.door().selection_devices().ok()? == expected).then_some(())
        })
        .unwrap_or_else(|error| panic!("{error}; {}", session.client_log(PROBE)));
        if mode == "killed" {
            session.kill_client(PROBE);
        } else {
            std::fs::write(gates.join("act"), []).unwrap();
            if matches!(mode, "disconnect" | "legacy") {
                let status = poll_until(EVENT, "the client to disconnect", || {
                    session.client_status(PROBE).unwrap()
                })
                .unwrap();
                assert!(status.success(), "{}", session.client_log(PROBE));
            }
        }
        let mut last = expected;
        poll_until(EVENT, "all selection devices to return to baseline", || {
            last = session.door().selection_devices().ok()?;
            (last == baseline).then_some(())
        })
        .unwrap_or_else(|error| {
            panic!("{error}; cycle {cycle}; baseline {baseline:?}; retained {last:?}")
        });
        println!("selection-devices {mode} cycle={cycle} live={expected:?} retired={last:?}");
        if matches!(mode, "release" | "seat-first") {
            assert!(
                session.client_status(PROBE).unwrap().is_none(),
                "release was observed while client lived"
            );
            std::fs::write(gates.join("finish"), []).unwrap();
            let status = poll_until(EVENT, "the released client to exit", || {
                session.client_status(PROBE).unwrap()
            })
            .unwrap();
            assert!(status.success());
        }
    }
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn selection_devices_are_retired_on_normal_disconnect() {
    lifecycle("disconnect");
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn selection_devices_are_retired_when_the_client_is_killed() {
    lifecycle("killed");
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn selection_devices_are_retired_while_the_client_stays_connected() {
    lifecycle("release");
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn selection_devices_outlive_their_released_seat_resource_safely() {
    lifecycle("seat-first");
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn legacy_data_devices_without_release_are_retired_on_disconnect() {
    lifecycle("legacy");
}
