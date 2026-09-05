//! `ext_workspace_v1`, against a live session.
//!
//! The desktop published its workspace row to X11 through EWMH and to
//! Omarchy's bar through the Hyprland IPC, and told native Wayland
//! clients nothing. A panel, pager or switcher written against the
//! frozen protocol saw a desktop with no workspaces on it.
//!
//! Both directions are asserted, because a read-only advertisement
//! would be half the protocol: the row must be published *and* a
//! client's `activate` must move the desktop.

use std::time::Duration;

use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(10);

fn boot(name: &str) -> Session {
    // Two bound digits, so the row can be grown past the one workspace
    // a fresh session starts with — workspaces are created on demand.
    let config = "[keybindings]\n\"super+1\" = \"workspace 1\"\n\"super+2\" = \"workspace 2\"\n".to_string();
    Session::boot(name, SessionOptions { config_extra: config, ..SessionOptions::default() })
        .expect("nested compositor boots")
}

/// The `super+<digit>` chord this desktop binds to a workspace.
fn workspace_chord(session: &mut Session, digit: u32) {
    let door = session.door();
    door.key(keys::LEFTMETA, true).unwrap();
    door.barrier().unwrap();
    door.tap_key(digit).unwrap();
    door.key(keys::LEFTMETA, false).unwrap();
    door.barrier().unwrap();
}

/// The last `**row …**` line the probe printed — the state of a settled
/// transaction, since the probe only reports on `done`.
fn last_row(session: &Session) -> Option<String> {
    session
        .client_log("chonk-workspace-probe")
        .rsplit("**row ")
        .next()
        .and_then(|rest| rest.split("**").next())
        .map(str::to_string)
        .filter(|row| row.starts_with("groups="))
}

/// The row is advertised, and it names the workspace the desktop is
/// actually on.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn the_workspace_row_is_published_to_native_clients() {
    let mut session = boot("ext-workspace-row");
    let probe = profile_binary("chonk-workspace-probe").expect("workspace probe built");
    session.launch(probe.to_str().unwrap(), &[]).expect("the probe launches");

    poll_until(EVENT, "the probe to bind the manager", || {
        session.client_log("chonk-workspace-probe").contains("**workspace manager bound**").then_some(())
    })
    .expect("ext_workspace_manager_v1 must be advertised");

    let row = poll_until(EVENT, "a settled workspace transaction", || last_row(&session))
        .expect("the compositor must publish the row");

    // One group over every output: this desktop has a single global
    // current workspace, and a group per monitor would advertise
    // per-output workspaces no key could change independently.
    assert!(row.contains("groups=1"), "exactly one workspace group: {row}");
    assert!(row.contains("active=1"), "the session starts on workspace 1: {row}");
    assert!(
        row.contains("names=1"),
        "workspaces are named the way EWMH and the IPC name them, from 1: {row}"
    );
}

/// The write direction. A client asking for a workspace must move the
/// desktop, through the same path an EWMH pager's request takes.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_native_client_can_activate_a_workspace() {
    let mut session = boot("ext-workspace-activate");
    let probe = profile_binary("chonk-workspace-probe").expect("workspace probe built");

    // Give the row somewhere to go: workspaces are created on demand,
    // so a fresh session has exactly one until something reaches past it.
    workspace_chord(&mut session, keys::TWO);
    workspace_chord(&mut session, keys::ONE);

    session.launch(probe.to_str().unwrap(), &["2"]).expect("the probe launches");
    poll_until(EVENT, "the probe to ask for workspace 2", || {
        session.client_log("chonk-workspace-probe").contains("**requested 2**").then_some(())
    })
    .expect("the probe finds workspace 2 in the row it was told about");

    poll_until(EVENT, "the desktop to switch", || {
        let log = session.log();
        log.contains("switched workspace").then_some(())
    })
    .expect("a native client's activate must move the desktop");

    // And the compositor republishes the row with the new active
    // workspace, so a panel's highlight follows.
    poll_until(EVENT, "the row to report the new active workspace", || {
        last_row(&session).filter(|row| row.contains("active=2")).map(|_| ())
    })
    .expect("the active workspace must be republished after it moves");
}
