//! Exercise signal delivery through real compositor launches and upgrades.
//! These tests never stop the developer's graphical session or user manager.

#![cfg(target_os = "linux")]

use std::path::Path;
use std::time::Duration;

use chonk_testkit::{keys, poll_until, session_dir, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(5);
const TERMINATION_MASK: u64 = (1 << (libc::SIGTERM - 1)) | (1 << (libc::SIGHUP - 1)) | (1 << (libc::SIGINT - 1));

/// Model an upgrade from the old signalfd compositor, which execed with
/// these signals blocked. This changes only the test's calling thread.
struct BlockedSignals(libc::sigset_t);

impl BlockedSignals {
    fn new() -> Self {
        // SAFETY: both sets are valid local storage. pthread_sigmask writes
        // the previous mask to `old` and only changes this calling thread.
        unsafe {
            let mut signals = std::mem::zeroed();
            let mut old = std::mem::zeroed();
            libc::sigemptyset(&mut signals);
            for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
                libc::sigaddset(&mut signals, signal);
            }
            assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &signals, &mut old), 0);
            Self(old)
        }
    }
}

impl Drop for BlockedSignals {
    fn drop(&mut self) {
        // SAFETY: the stored mask came from pthread_sigmask on this thread.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut()); }
    }
}

fn blocked(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let mask = status.lines().find_map(|line| line.strip_prefix("SigBlk:\t")).unwrap();
    u64::from_str_radix(mask, 16).unwrap()
}

fn signal(pid: u32, signal: i32) {
    // SAFETY: these are live processes launched in this test's private
    // session, and the only effect is delivering a valid numeric signal.
    assert_eq!(unsafe { libc::kill(pid as i32, signal) }, 0);
}

/// Owns cleanup even when a mask or delivery assertion fails. The real
/// shell reaps the process; this test only waits for /proc to disappear.
struct Sleeper(Option<u32>);

impl Sleeper {
    fn ready(path: &Path) -> Self {
        let pid = poll_until(EVENT, "the shell-launched application's PID", || {
            std::fs::read_to_string(path).ok()?.trim().parse().ok()
        }).unwrap();
        let sleeper = Self(Some(pid));
        poll_until(EVENT, "the launcher shell to exec sleep", || {
            (std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim() == "sleep").then_some(())
        }).unwrap();
        sleeper
    }

    fn pid(&self) -> u32 {
        self.0.unwrap()
    }

    fn terminate(mut self, termination: i32) {
        let pid = self.pid();
        signal(pid, termination);
        poll_until(EVENT, "the application to honor termination and be reaped", || {
            (!Path::new(&format!("/proc/{pid}")).exists()).then_some(())
        }).unwrap();
        self.0 = None;
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let Some(pid) = self.0 else { return };
        if Path::new(&format!("/proc/{pid}")).exists() {
            // SAFETY: cleanup is restricted to the child this guard owns.
            unsafe { libc::kill(pid as i32, libc::SIGKILL); }
            let _ = poll_until(EVENT, "probe cleanup", || {
                (!Path::new(&format!("/proc/{pid}")).exists()).then_some(())
            });
        }
    }
}

fn sleeper_command(path: &Path) -> String {
    // Positional arguments keep paths out of shell syntax. exec deliberately
    // leaves signal setup untouched, just as background desktop helpers do.
    serde_json::to_string(&[
        "sh", "-c", "echo $$ > \"$1\"; exec sleep 60", "logout-probe", path.to_str().unwrap(),
    ]).unwrap()
}

#[test]
#[ignore = "needs an isolated Wayland host; scripts/e2e.sh --headless --test logout"]
fn applications_and_library_children_keep_termination_signals_deliverable() {
    let name = "logout-child-signals";
    let dir = session_dir(name);
    let pid_file = dir.join("child.pid");
    let command = sleeper_command(&pid_file);
    let config = format!(
        "autostart = [{command}]\n[commands]\nprobe = {command}\n[keybindings]\n\"super+space\" = \"run probe\"\n"
    );
    let mut session = Session::boot(name, SessionOptions { config_extra: config, ..Default::default() }).unwrap();
    let first = Sleeper::ready(&pid_file);
    assert_eq!(blocked(first.pid()) & TERMINATION_MASK, 0, "autostart inherited the compositor's blocked signals");
    first.terminate(libc::SIGTERM);

    // A later interactive launch must be healthy too, after all compositor
    // services and their workers have started.
    for termination in [libc::SIGHUP, libc::SIGINT] {
        std::fs::remove_file(&pid_file).unwrap();
        session.door().chord(keys::LEFTMETA, keys::SPACE).unwrap();
        let child = Sleeper::ready(&pid_file);
        assert_eq!(blocked(child.pid()) & TERMINATION_MASK, 0);
        child.terminate(termination);
        session.door().barrier().unwrap();
    }

    // XWayland is spawned inside Smithay, outside the shell's launch helpers.
    // Fixing only Command construction in chonk-shell misses this boundary.
    let compositor = session.compositor_pid();
    let xwayland = poll_until(EVENT, "the library-spawned XWayland process", || {
        std::fs::read_to_string(format!("/proc/{compositor}/task/{compositor}/children")).ok()?
            .split_whitespace().filter_map(|pid| pid.parse::<u32>().ok()).find(|pid| {
                std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|name| name.trim() == "Xwayland")
            })
    }).unwrap();
    assert_eq!(blocked(xwayland) & TERMINATION_MASK, 0, "library-spawned child inherited blocked signals");
}

#[test]
#[ignore = "needs an isolated Wayland host; scripts/e2e.sh --headless --test logout"]
fn inherited_termination_masks_are_repaired_before_children_and_logout() {
    let name = "logout-inherited-mask";
    let dir = session_dir(name);
    let pid_file = dir.join("child.pid");
    let config = format!("autostart = [{}]\n", sleeper_command(&pid_file));
    let inherited_mask = BlockedSignals::new();
    let mut session = Session::boot(name, SessionOptions { config_extra: config, ..Default::default() }).unwrap();
    drop(inherited_mask);
    let child = Sleeper::ready(&pid_file);
    assert_eq!(blocked(child.pid()) & TERMINATION_MASK, 0, "upgrade must clear the old compositor's inherited mask");
    child.terminate(libc::SIGTERM);

    signal(session.compositor_pid(), libc::SIGTERM);
    let status = session.wait_for_compositor_exit(EVENT).unwrap();
    assert!(status.success(), "logout after repairing an inherited mask must be clean: {status}");
    assert!(session.log().contains("session termination requested; logging out cleanly"));
}

#[test]
#[ignore = "needs an isolated Wayland host; scripts/e2e.sh --headless --test logout"]
fn each_session_termination_signal_exits_the_compositor_cleanly() {
    for termination in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        let mut session = Session::boot(&format!("logout-signal-{termination}"), SessionOptions::default()).unwrap();
        signal(session.compositor_pid(), termination);
        let status = session.wait_for_compositor_exit(EVENT).unwrap();
        assert!(status.success(), "signal {termination} must request logout, not crash: {status}");
        assert!(session.log().contains("session termination requested; logging out cleanly"));
    }
}

#[test]
#[ignore = "needs an isolated Wayland host; scripts/e2e.sh --headless --test logout"]
fn repeated_nested_restarts_reconnect_to_the_host_and_keep_logout_working() {
    let name = "logout-after-reexec";
    let dir = session_dir(name);
    let pid_file = dir.join("child.pid");
    let config = format!("autostart = [{}]\n", sleeper_command(&pid_file));
    let mut session = Session::boot(name, SessionOptions { config_extra: config, ..Default::default() }).unwrap();
    let compositor = session.compositor_pid();
    for _ in 0..2 {
        let child = Sleeper::ready(&pid_file);
        assert_eq!(blocked(child.pid()) & TERMINATION_MASK, 0);
        child.terminate(libc::SIGTERM);
        std::fs::remove_file(&pid_file).unwrap();
        std::fs::write(dir.join("state/chonkstep/restart"), []).unwrap();
        poll_until(EVENT, "the restarted compositor to autostart its child", || {
            pid_file.exists().then_some(())
        }).unwrap_or_else(|error| panic!("{error}\n{}", session.log()));
        assert_eq!(session.compositor_pid(), compositor, "restart must exec in place");
    }
    Sleeper::ready(&pid_file).terminate(libc::SIGTERM);
    signal(compositor, libc::SIGTERM);
    assert!(session.wait_for_compositor_exit(EVENT).unwrap().success());
}
