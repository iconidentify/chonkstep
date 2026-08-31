//! The session watchdog, exercised as a script: `scripts/
//! wayland-session.sh` supervises the compositor, re-execing it after
//! an abnormal exit and braking a crash loop. These tests run the real
//! script against a stub "compositor" (the `CHONKSTEP_SESSION_BIN`
//! seam) inside an isolated `$HOME`/`$XDG_STATE_HOME`, so the loop,
//! the brake and the recovery marker are asserted end to end — no
//! Wayland session needed, which is why nothing here is `#[ignore]`d.
//!
//! Same house rules as the e2e suite: no blocking child waits (the
//! workspace bans them — see the root clippy.toml), every wait a
//! bounded `try_wait` poll.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use chonk_testkit::poll_until;

fn script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/wayland-session.sh")
}

/// A scratch world for one supervisor run: isolated HOME, state and
/// runtime dirs, plus a stub binary that appends one line to a count
/// file per run and exits with `exit_code`.
struct Scratch {
    dir: PathBuf,
    stub: PathBuf,
    runs: PathBuf,
}

fn scratch(name: &str, exit_code: i32) -> Scratch {
    let dir = std::env::temp_dir().join("chonk-testkit-supervisor").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("run")).unwrap();
    std::fs::create_dir_all(dir.join("state")).unwrap();
    let runs = dir.join("runs");
    let stub = dir.join("stub-compositor.sh");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\necho run >> \"{}\"\nexit {exit_code}\n", runs.display()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    Scratch { dir, stub, runs }
}

/// Launches the session script against the scratch world and returns
/// the child. `DBUS_SESSION_BUS_ADDRESS` is pinned so the script's
/// bus wrapper stays out of the way — the loop under test is the same
/// either side of it.
fn launch(scratch: &Scratch) -> Child {
    Command::new("bash")
        .arg(script_path())
        .env_clear()
        .env("HOME", &scratch.dir)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_STATE_HOME", scratch.dir.join("state"))
        .env("XDG_RUNTIME_DIR", scratch.dir.join("run"))
        .env("CHONKSTEP_SESSION_BIN", &scratch.stub)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent-but-present")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the session script should launch under bash")
}

fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    poll_until(Duration::from_secs(30), "the session script to exit", || {
        child.try_wait().ok().flatten()
    })
    .expect("the supervisor loop must terminate")
}

fn run_count(scratch: &Scratch) -> usize {
    std::fs::read_to_string(&scratch.runs).unwrap_or_default().lines().count()
}

#[test]
fn a_crash_loop_is_braked_after_the_allowed_retries_with_the_marker_dropped() {
    let scratch = scratch("crash-loop", 1);
    let mut child = launch(&scratch);
    let status = wait_exit(&mut child);

    // The brake: the initial run plus MAX_CRASHES retries, and the
    // crash that takes the count past the limit stops the loop with a
    // nonzero exit so a display manager returns to its greeter.
    assert!(!status.success(), "a braked crash loop must exit nonzero, got {status}");
    assert_eq!(run_count(&scratch), 4, "one initial run plus exactly three re-execs before the brake");

    // Each re-exec was preceded by the recovery marker — the channel
    // the recovering compositor reads. The stub never consumes it, so
    // it must still be there.
    assert!(
        scratch.dir.join("state/chonkstep/recovery").exists(),
        "the supervisor must drop the recovery marker before re-execing after a crash"
    );

    // And the log says what happened and where to look.
    let log = std::fs::read_to_string(scratch.dir.join("state/chonkstep/wayland-session.log")).unwrap_or_default();
    assert!(log.contains("crash loop"), "the brake must name itself in the session log; log was:\n{log}");
    assert!(log.contains("restarting"), "each recovery must be logged; log was:\n{log}");
}

#[test]
fn a_clean_exit_is_a_logout_not_a_crash() {
    let scratch = scratch("clean-exit", 0);
    let mut child = launch(&scratch);
    let status = wait_exit(&mut child);

    assert!(status.success(), "a clean compositor exit must end the session cleanly, got {status}");
    assert_eq!(run_count(&scratch), 1, "a logout must not be re-execed");
    assert!(
        !scratch.dir.join("state/chonkstep/recovery").exists(),
        "a logout must never look like a crash to the next session"
    );
}

#[test]
fn a_signal_death_counts_as_a_crash() {
    // The panic hook aborts (SIGABRT) and a wedged compositor gets
    // SIGKILLed by hand; both surface to the supervisor as 128+sig,
    // which must take the recovery path, not the logout one. The stub
    // kills itself to simulate it.
    let dir = std::env::temp_dir().join("chonk-testkit-supervisor").join("signal-death");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("run")).unwrap();
    std::fs::create_dir_all(dir.join("state")).unwrap();
    let runs = dir.join("runs");
    let stub = dir.join("stub-compositor.sh");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\necho run >> \"{}\"\nkill -ABRT $$\n", runs.display()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scratch = Scratch { dir, stub, runs };

    let mut child = launch(&scratch);
    let status = wait_exit(&mut child);

    assert!(!status.success());
    assert_eq!(run_count(&scratch), 4, "signal deaths ride the same brake as nonzero exits");
    assert!(scratch.dir.join("state/chonkstep/recovery").exists());
}
