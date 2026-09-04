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
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions};

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
    std::fs::write(&stub, format!("#!/bin/sh\necho run >> \"{}\"\nexit {exit_code}\n", runs.display())).unwrap();
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
        .env("CHONKSTEP_SESSION_TESTING", "1")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent-but-present")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the session script should launch under bash")
}

fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    poll_until(Duration::from_secs(30), "the session script to exit", || child.try_wait().ok().flatten())
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

    // The marker is only for a re-exec inside this supervisor. Once the
    // brake returns to the greeter, it must not leak into a later login.
    assert!(
        !scratch.dir.join("state/chonkstep/recovery").exists(),
        "a braked crash loop must clear its stale recovery marker"
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
    std::fs::write(&stub, format!("#!/bin/sh\necho run >> \"{}\"\nkill -ABRT $$\n", runs.display())).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scratch = Scratch { dir, stub, runs };

    let mut child = launch(&scratch);
    let status = wait_exit(&mut child);

    assert!(!status.success());
    assert_eq!(run_count(&scratch), 4, "signal deaths ride the same brake as nonzero exits");
    assert!(!scratch.dir.join("state/chonkstep/recovery").exists());
}

#[test]
fn terminating_the_supervisor_is_a_logout_not_a_recovery() {
    let dir = std::env::temp_dir().join("chonk-testkit-supervisor").join("session-term");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("run")).unwrap();
    std::fs::create_dir_all(dir.join("state")).unwrap();
    let runs = dir.join("runs");
    let stub = dir.join("stub-compositor.sh");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\necho run >> \"{}\"\ntrap 'exit 0' TERM HUP INT\nwhile :; do sleep 1; done\n",
            runs.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scratch = Scratch { dir, stub, runs };
    let mut child = launch(&scratch);
    poll_until(Duration::from_secs(5), "stub compositor to start", || (run_count(&scratch) == 1).then_some(()))
        .expect("stub did not start");

    // SAFETY: the child was spawned by this test, has not been reaped, and
    // `kill` only passes its numeric pid and a valid signal to the kernel.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let status = wait_exit(&mut child);
    assert!(status.success(), "session-manager TERM must be a clean logout: {status}");
    assert_eq!(run_count(&scratch), 1, "a terminating session must never restart its compositor");
    assert!(!scratch.dir.join("state/chonkstep/recovery").exists());
}

#[test]
fn direct_session_owns_graphical_targets_and_publishes_only_curated_environment() {
    let dir = std::env::temp_dir().join("chonk-testkit-supervisor").join("graphical-targets");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("run")).unwrap();
    std::fs::create_dir_all(dir.join("state")).unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let calls = dir.join("calls");

    let command = |name: &str, body: &str| {
        let path = dir.join("bin").join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    command("systemctl", &format!("printf 'systemctl %s\\n' \"$*\" >> '{}'\nexit 0", calls.display()));
    command(
        "dbus-update-activation-environment",
        &format!("printf 'dbus %s\\n' \"$*\" >> '{}'\nexit 0", calls.display()),
    );

    let socket_name = "wayland-session-test";
    let _socket = UnixListener::bind(dir.join("run").join(socket_name)).unwrap();
    let stub = dir.join("stub-compositor.sh");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf 'private=%s|%s|%s|%s|%s|%s|%s|%s|%s\\n' \\\n \"${{CHONKSTEP_BACKEND-unset}}\" \\\n \"${{CHONKSTEP_CONTROL_SOCKET-unset}}\" \\\n \"${{CHONKSTEP_NO_APPEARANCE_PROPAGATION-unset}}\" \\\n \"${{CHONKSTEP_OWNS_XCURSOR_SIZE-unset}}\" \\\n \"${{CHONKSTEP_SESSION_CONTINUES-unset}}\" \\\n \"${{CHONKSTEP_SESSION_BIN-unset}}\" \\\n \"${{CHONKSTEP_SESSION_TESTING-unset}}\" \\\n \"${{CHONKSTEP_TEST_SOCKET-unset}}\" \\\n \"${{XCURSOR_SIZE-unset}}\" >> '{}'\nprintf '%s\\n' 'wayland socket listening socket=\"{socket_name}\"'\nprintf '%s\\n' 'hyprland ipc listening signature=\"session-test\"'\ntrap 'exit 0' TERM HUP INT\nwhile :; do sleep 1; done\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let path = format!("{}:{}", dir.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut child = Command::new("bash")
        .arg(script_path())
        .env_clear()
        .env("HOME", &dir)
        .env("PATH", path)
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("CHONKSTEP_SESSION_BIN", &stub)
        .env("CHONKSTEP_SESSION_TESTING", "1")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/isolated")
        .env("CHONKSTEP_BACKEND", "winit")
        .env("CHONKSTEP_CONTROL_SOCKET", "/run/user/1000/chonkstep/stale.sock")
        .env("CHONKSTEP_NO_APPEARANCE_PROPAGATION", "1")
        .env("CHONKSTEP_OWNS_XCURSOR_SIZE", "1")
        .env("CHONKSTEP_SESSION_CONTINUES", "1")
        .env("CHONKSTEP_TEST_SOCKET", "/tmp/stale-test-door.sock")
        .env("XCURSOR_SIZE", "96")
        .env("CARGO_POISON", "must-not-be-published")
        .env("LD_LIBRARY_PATH", "/also/not/published")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let started = poll_until(Duration::from_secs(10), "the direct session to start its user targets", || {
        let text = std::fs::read_to_string(&calls).ok()?;
        text.contains("--user start graphical-session.target xdg-desktop-autostart.target").then_some(text)
    })
    .expect("the non-uwsm session owns graphical-session.target");
    assert!(started.contains("WAYLAND_DISPLAY=wayland-session-test"));
    assert!(started.contains("XDG_MENU_PREFIX=chonkstep-"));
    assert!(started.contains("XDG_BACKEND=wayland"));
    assert!(
        started.contains("private=unset|unset|unset|unset|unset|unset|unset|unset|unset"),
        "a login session must discard every stale private/test control before launching the compositor; calls were:\n{started}"
    );
    assert!(
        started.contains("--user unset-environment DISPLAY WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE CHONKSTEP_BACKEND"),
        "the persistent activation environment must be scrubbed before the compositor starts; calls were:\n{started}"
    );
    assert!(!started.contains("CARGO_POISON"));
    assert!(!started.contains("LD_LIBRARY_PATH"));

    // SAFETY: the child was spawned by this test, has not been reaped, and
    // `kill` only passes its numeric pid and a valid signal to the kernel.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert!(wait_exit(&mut child).success());
    let stopped = std::fs::read_to_string(&calls).unwrap();
    assert!(stopped.contains("--user stop xdg-desktop-autostart.target graphical-session.target"));
    assert!(stopped.contains("--user unset-environment WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE"));
    assert!(
        !stopped.contains("dbus --systemd --unset"),
        "dbus-update-activation-environment has no --unset operation; systemd owns deletion on Omarchy"
    );
}

#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn terminating_the_real_compositor_is_a_clean_logout() {
    let mut session = Session::boot("compositor-term", SessionOptions::default()).expect("nested compositor boots");
    // SAFETY: the session owns this still-running compositor process, and
    // `kill` only passes its numeric pid and a valid signal to the kernel.
    unsafe { libc::kill(session.compositor_pid() as i32, libc::SIGTERM) };
    let status = session
        .wait_for_compositor_exit(Duration::from_secs(10))
        .expect("the signalfd handler must end the event loop");
    assert!(status.success(), "SIGTERM is a requested logout, not a crash: {status}");
    assert!(
        session.log().contains("session termination requested; logging out cleanly"),
        "the reason should be explicit in the session log"
    );
}
