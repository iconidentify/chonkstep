//! Launches apps from the root menu as detached child processes.

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Returns the spawned child's PID on success — lets a caller correlate
/// a specific launch with whichever window later reports that same PID
/// via `_NET_WM_PID` (see `Backend::window_pid`), rather than matching
/// on something as loose as "any window of the expected class."
pub fn spawn_detached(program: &str, args: &[&str]) -> Option<u32> {
    spawn_detached_with_env(program, args, &[], &[])
}

/// The variables that must be *removed* from a dockapp's environment,
/// and the reason the removal is mandatory rather than tidy.
///
/// The dockapp boundary's headline claim is that a dockapp holds no
/// display connection, so `wl_shm`, `zwlr_screencopy_v1` and
/// `zwlr_foreign_toplevel_management_v1` (`wm-wayland/src/protocols.rs`)
/// are *unreachable* rather than merely denied — a stronger guarantee
/// than Wayland's own, because there is no object to ask.
///
/// That claim is false by default. Nothing stops the dockapp *process*
/// from opening `$WAYLAND_DISPLAY` or `$DISPLAY` itself: it inherits
/// the environment (this function does not clear it, and
/// `wm-wayland/src/state.rs` deliberately sets both for children so
/// that ordinary launched apps work). A dockapp that connected on its
/// own would get everything a normal client gets — screen capture, the
/// window list, the clipboard — while presenting as a tile.
///
/// Unsetting both is what turns "a dockapp is granted nothing extra"
/// into "a dockapp is granted strictly less than a normal app". It is a
/// hurdle, not a cage — a determined program can guess `:0` or
/// enumerate `$XDG_RUNTIME_DIR/wayland-*` — and the SDK documentation
/// says so plainly. What it buys is that reaching a display server
/// becomes a deliberate, auditable act by the dockapp rather than
/// something it gets for free by calling a toolkit's `init()`.
pub const DISPLAY_SERVER_ENV: [&str; 2] = ["WAYLAND_DISPLAY", "DISPLAY"];

/// Same as [`spawn_detached`], with extra environment variables set on
/// top of whatever chonkstep's own process already has (which the child
/// inherits regardless — `Command` doesn't clear the parent environment
/// unless asked to). Exists for [`chromium_scale_args`]/[`gtk_qt_scale_env`]:
/// a third-party binary chonkstep doesn't control needs to be *told*
/// about the desktop's scale through whatever convention its own
/// toolkit understands, since it has no way to ask chonkstep for one.
///
/// `unset` names variables to *remove* from the child's environment.
/// It exists for dockapps, which must be launched with
/// [`DISPLAY_SERVER_ENV`] cleared — see that constant for why that is a
/// requirement of the design rather than a precaution. Pass `&[]` when
/// there is nothing to remove.
pub fn spawn_detached_with_env(
    program: &str,
    args: &[&str],
    env: &[(String, String)],
    unset: &[&str],
) -> Option<u32> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    apply_env(&mut command, env, unset);
    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            tracing::info!(program, pid, "launched");
            // Reap the child when it eventually exits, or it lingers as
            // a zombie under the WM for the whole session (confirmed
            // live once the Applications menu made launches routine:
            // two exited Chromiums sat `<defunct>` in the process
            // table). A dedicated thread per launch that just `wait`s
            // its own pid is deliberately chosen over the classic
            // SIGCHLD-ignore trick: globally ignoring SIGCHLD makes the
            // kernel auto-reap *every* child, which breaks the
            // `Command::output()` calls the instrument widgets rely on
            // (their `waitpid` would race the auto-reaper and fail).
            // Launches are user gestures, so the thread count is
            // bounded by concurrently running launched apps.
            std::thread::spawn(move || {
                // Audited exception to `clippy.toml`'s ban on blocking
                // child-process calls: this closure is the entire body
                // of a thread whose only job is to outlive the child and
                // reap it. Nothing waits on this thread, so the only
                // thing a never-returning `wait` can hold up is one
                // thread-sized allocation until the session ends.
                #[allow(clippy::disallowed_methods)]
                let _ = child.wait();
            });
            Some(pid)
        }
        Err(e) => {
            tracing::warn!(program, ?e, "failed to launch");
            None
        }
    }
}

/// How a supervised child ended.
///
/// Mirrors `std::process::ExitStatus` in the two fields anything here
/// actually asks about, because `ExitStatus` cannot be constructed in a
/// test and a restart policy that cannot be tested is a restart policy
/// nobody has checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitReport {
    /// The exit code, or `None` if it was killed by a signal.
    pub code: Option<i32>,
    /// The signal that killed it, if one did.
    pub signal: Option<i32>,
}

impl ExitReport {
    /// Whether the child *chose* to end. Everything else — a non-zero
    /// code, any signal at all — is a crash for the purposes of a
    /// dockapp's `restart = "on-crash"` policy.
    pub fn is_success(self) -> bool {
        self.code == Some(0) && self.signal.is_none()
    }
}

/// A child the caller intends to outlive, and to ask about later.
///
/// # Why this exists beside `spawn_detached_with_env`
///
/// That function reaps its child on a dedicated thread and throws the
/// status away, which is exactly right for a launched application: the
/// shell has no opinion about how a text editor exits. A dockapp is
/// different. `restart = "on-crash"` has to distinguish a tile that
/// finished (a battery instrument on a desktop with no battery, exiting
/// zero) from one that died, and that distinction is *only* in the exit
/// status.
///
/// So the reaper thread stays — it is the thing that must never run on
/// the compositor's repaint thread — and it now writes what it learned
/// into a shared cell before it ends. The shell polls that cell from
/// its event loop and never waits for anything. This is the second of
/// the three crash signals in the dockapp design; the first is the
/// socket EOF, which is instant and definitive, and this one answers
/// the follow-up question of *how*.
pub struct SpawnedChild {
    pid: u32,
    /// `Some` once the reaper thread has seen the child exit. Written
    /// exactly once, from that thread, and read from the event loop.
    /// A `Mutex` rather than an atomic because the payload is two
    /// `Option<i32>`s and the lock is uncontended by construction: one
    /// writer, one reader, once.
    exit: Arc<Mutex<Option<ExitReport>>>,
}

impl SpawnedChild {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// How the child ended, or `None` if it has not been observed to
    /// end yet. Never blocks — a poisoned lock reads as "not yet",
    /// which is the same answer as a child still running and leads to
    /// the same (safe) behaviour.
    pub fn exited(&self) -> Option<ExitReport> {
        self.exit.lock().ok().and_then(|report| *report)
    }

    /// Asks the child to leave.
    ///
    /// Called when the shell has decided this process is no longer the
    /// tile's — its socket closed while it kept running, or the user
    /// removed the tile. Without it a dockapp that closes its socket
    /// and keeps going would be joined by its own replacement, and the
    /// user would pay for both forever.
    ///
    /// `SIGTERM`, not `SIGKILL`: a dockapp is a normal process of the
    /// user's, and one that wants to flush something on the way out
    /// should get to. There is deliberately no follow-up `SIGKILL`
    /// timer — that would need a timer, a second signal path and a
    /// policy about how long is long enough, to solve a problem
    /// ("a dockapp that ignores SIGTERM") that no shipped dockapp has
    /// and that the crash-loop cutoff already bounds the damage of.
    ///
    /// The pid race is real and is bounded by the reaper thread: this
    /// process reaps its own children, so a pid it spawned cannot be
    /// recycled by the kernel until that thread's `wait` has collected
    /// it — and once it has, `exited()` is `Some` and callers do not
    /// reach here.
    pub fn terminate(&self) {
        if self.exited().is_some() {
            return;
        }
        tracing::info!(pid = self.pid, "asking a supervised child to exit");
        // SAFETY: `kill` with a pid this process spawned and has not
        // yet reaped; the only effect is delivering a signal.
        unsafe {
            libc::kill(self.pid as libc::pid_t, libc::SIGTERM);
        }
    }
}

/// Like [`spawn_detached_with_env`], but the caller keeps a handle that
/// reports how the child ended.
///
/// The reaping thread is the same deliberate design as the one in
/// `spawn_detached_with_env`, for the same reason: globally ignoring
/// `SIGCHLD` would make the kernel auto-reap every child and break the
/// `Command::output()` the sampler workers depend on. One thread per
/// dockapp, bounded by the number of registered dockapps, and the only
/// thing a never-returning `wait` can hold up is that thread.
pub fn spawn_supervised(program: &str, args: &[&str], env: &[(String, String)], unset: &[&str]) -> Option<SpawnedChild> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    apply_env(&mut command, env, unset);
    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            let exit = Arc::new(Mutex::new(None));
            let reported = Arc::clone(&exit);
            std::thread::spawn(move || {
                // Audited exception to `clippy.toml`'s ban on blocking
                // child-process calls: this closure is the entire body
                // of a thread whose only job is to outlive this one
                // child and reap it. Nothing joins this thread and
                // nothing waits on it; the shell reads the cell below
                // from its event loop and never blocks.
                #[allow(clippy::disallowed_methods)]
                let status = child.wait();
                if let Ok(status) = status {
                    let report = ExitReport { code: status.code(), signal: std::os::unix::process::ExitStatusExt::signal(&status) };
                    if let Ok(mut slot) = reported.lock() {
                        *slot = Some(report);
                    }
                }
            });
            tracing::info!(program, pid, "launched (supervised)");
            Some(SpawnedChild { pid, exit })
        }
        Err(e) => {
            tracing::warn!(program, ?e, "failed to launch");
            None
        }
    }
}

/// Applies the set/unset lists to a `Command`.
///
/// Split out so the policy is testable without spawning anything:
/// `Command::get_envs` reports the pending modifications (a removal
/// shows up as `(key, None)`), which lets the tests below assert what a
/// child *would* see. Verifying it by actually running a process would
/// mean waiting on a child, which this workspace's `clippy.toml` bans
/// outright for good reasons.
///
/// Removals are applied after additions on purpose: if a caller ever
/// passes the same key in both lists, "remove it" is the safer reading
/// of an ambiguous instruction, and for [`DISPLAY_SERVER_ENV`] it is
/// the only acceptable one.
fn apply_env(command: &mut Command, env: &[(String, String)], unset: &[&str]) {
    for (key, value) in env {
        command.env(key, value);
    }
    for key in unset {
        command.env_remove(key);
    }
}

/// Command-line flags that make a Chromium-family browser (Edge,
/// Chrome, Brave, ...) render at this desktop's actual `CHONKSTEP_SCALE`
/// instead of guessing its own — Chromium does its own DPI detection
/// and has no way to discover a WM-invented scale factor like this
/// desktop's, so it has to be told explicitly. Every third-party app the
/// root menu launches should scale the same way chonkstep's own chrome
/// does — see also [`gtk_qt_scale_env`] for the same idea applied to
/// GTK/Qt toolkits, which don't read this flag at all.
pub fn chromium_scale_args(scale: f32) -> Vec<String> {
    vec![format!("--force-device-scale-factor={scale}")]
}

/// Works around a real, confirmed hang on this desktop: Omarchy's
/// system-wide Edge/Chromium flags file defaults to
/// `--password-store=gnome-libsecret`, which makes the browser block on
/// the D-Bus-activatable `org.freedesktop.secrets` service before it'll
/// finish starting up. In a minimal WM session (no GNOME session bus
/// autostarting things the usual way), that activation conflicts with
/// the `gnome-keyring-daemon` already running outside of D-Bus
/// activation and never completes — reproduced directly with `busctl
/// --user call org.freedesktop.secrets ... Ping`, which times out after
/// exactly 25 seconds, matching the "spinning for ~30 seconds before a
/// page loads" symptom exactly. `--password-store=basic` switches to
/// Chromium's own local encrypted file store instead, skipping the
/// D-Bus secrets dance entirely. Command-line flags win over the flags
/// file (whichever occurrence of a switch comes *last* takes effect,
/// and the launcher script appends chonkstep's own args after the flags
/// file's contents — see `microsoft-edge-stable`'s wrapper script).
pub fn chromium_avoid_secrets_service_hang_args() -> Vec<String> {
    vec!["--password-store=basic".to_string()]
}

/// Pins a Chromium-family browser to the X11 ozone backend. Omarchy is
/// Wayland-first, and its Chromium configuration selects the Wayland
/// platform - which does not exist inside this X11 session, so the
/// browser prints "Failed to connect to Wayland display" and exits
/// without ever mapping a window (confirmed live from the Applications
/// menu). Chromium honors the *last* occurrence of a switch, and these
/// launcher args are appended after any flags file, so this wins.
pub fn chromium_x11_platform_args() -> Vec<String> {
    vec!["--ozone-platform=x11".to_string()]
}

/// Environment variables that make GTK/Qt-based UI — including the
/// native file-open/save dialogs a Chromium browser itself delegates to
/// GTK for on Linux — honor this desktop's scale too. There's no
/// running XSETTINGS daemon or desktop portal in this WM to advertise
/// DPI the way a full desktop environment would, so external toolkits
/// are told directly through the env vars they already fall back to.
///
/// `GDK_SCALE` only accepts a whole number (it's a literal backing-store
/// pixel-doubling factor, not a DPI hint), so any fractional remainder
/// of `scale` is carried by `GDK_DPI_SCALE` instead — GTK's own
/// documented recipe for fractional scaling is exactly this pairing,
/// and the two intentionally multiply back out to `scale` (e.g. 1.5 →
/// `GDK_SCALE=2`, `GDK_DPI_SCALE=0.75`). `QT_SCALE_FACTOR` handles the
/// Qt side directly since it accepts a plain float.
pub fn gtk_qt_scale_env(scale: f32) -> Vec<(String, String)> {
    let integer_scale = scale.round().max(1.0);
    let dpi_remainder = scale / integer_scale;
    vec![
        ("GDK_SCALE".to_string(), (integer_scale as u32).to_string()),
        ("GDK_DPI_SCALE".to_string(), format!("{dpi_remainder:.4}")),
        ("QT_SCALE_FACTOR".to_string(), format!("{scale:.4}")),
        ("QT_AUTO_SCREEN_SCALE_FACTOR".to_string(), "0".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtk_dpi_scale_and_gdk_scale_multiply_back_to_the_requested_scale() {
        for scale in [1.0f32, 1.5, 2.0, 3.0, 2.25] {
            let env = gtk_qt_scale_env(scale);
            let gdk_scale: f32 = env.iter().find(|(k, _)| k == "GDK_SCALE").unwrap().1.parse().unwrap();
            let dpi_scale: f32 = env.iter().find(|(k, _)| k == "GDK_DPI_SCALE").unwrap().1.parse().unwrap();
            assert!((gdk_scale * dpi_scale - scale).abs() < 0.01, "scale {scale}: {gdk_scale} * {dpi_scale} should reconstruct it");
        }
    }

    #[test]
    fn gdk_scale_is_always_a_whole_number_even_for_fractional_input() {
        let env = gtk_qt_scale_env(1.5);
        let gdk_scale = &env.iter().find(|(k, _)| k == "GDK_SCALE").unwrap().1;
        assert_eq!(gdk_scale, "2");
    }

    #[test]
    fn a_dockapps_launch_environment_has_no_display_server_in_it() {
        // The mandatory mitigation, asserted rather than trusted: a
        // dockapp that inherited WAYLAND_DISPLAY or DISPLAY could open
        // a display connection and help itself to screen capture, the
        // window list and the clipboard while presenting as a tile.
        let mut command = Command::new("/bin/true");
        apply_env(
            &mut command,
            &[("CHONKSTEP_DOCK_SOCKET".to_string(), "/run/user/1000/chonkstep/dock-1.sock".to_string())],
            &DISPLAY_SERVER_ENV,
        );
        let envs: Vec<_> = command.get_envs().collect();
        for variable in DISPLAY_SERVER_ENV {
            let entry = envs.iter().find(|(key, _)| *key == std::ffi::OsStr::new(variable));
            assert_eq!(entry.map(|(_, value)| *value), Some(None), "{variable} must be removed, not merely left unset");
        }
        assert!(
            envs.iter().any(|(key, value)| *key == std::ffi::OsStr::new("CHONKSTEP_DOCK_SOCKET") && value.is_some()),
            "the variables the dockapp actually needs still arrive"
        );
    }

    #[test]
    fn removal_wins_over_an_accidental_set_of_the_same_variable() {
        // An ambiguous instruction about DISPLAY has exactly one safe
        // reading.
        let mut command = Command::new("/bin/true");
        apply_env(&mut command, &[("DISPLAY".to_string(), ":0".to_string())], &["DISPLAY"]);
        let display = command.get_envs().find(|(key, _)| *key == std::ffi::OsStr::new("DISPLAY"));
        assert_eq!(display.map(|(_, value)| value), Some(None));
    }

    #[test]
    fn an_empty_unset_list_leaves_the_inherited_environment_alone() {
        // `spawn_detached_with_env`'s existing callers must be
        // unaffected: they pass no removals and inherit everything.
        let mut command = Command::new("/bin/true");
        apply_env(&mut command, &gtk_qt_scale_env(2.0), &[]);
        assert!(
            command.get_envs().all(|(_, value)| value.is_some()),
            "nothing should be marked for removal"
        );
    }

    #[test]
    fn an_exit_report_calls_only_a_clean_zero_a_success() {
        // The whole of `restart = "on-crash"` turns on this predicate,
        // and the three cases it has to get right are a tile that
        // decided it was done, one that failed, and one that was
        // killed.
        assert!(ExitReport { code: Some(0), signal: None }.is_success());
        assert!(!ExitReport { code: Some(1), signal: None }.is_success(), "a non-zero exit is a crash");
        assert!(!ExitReport { code: None, signal: Some(libc::SIGSEGV) }.is_success(), "a signal is a crash");
        assert!(!ExitReport { code: None, signal: Some(libc::SIGTERM) }.is_success(), "even one we sent");
    }

    #[test]
    fn chromium_flag_carries_the_exact_scale() {
        let args = chromium_scale_args(2.25);
        assert_eq!(args, vec!["--force-device-scale-factor=2.25".to_string()]);
    }
}
