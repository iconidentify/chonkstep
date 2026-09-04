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

/// The variable every process the shell launches finds the control
/// socket (`docs/control-socket.md` §1.1) under.
pub const CONTROL_SOCKET_ENV: &str = "CHONKSTEP_CONTROL_SOCKET";

/// Process-private compositor controls that must stop at the
/// application boundary.
///
/// These are backend/debug selectors, restart/session markers, and
/// test seams.  They are meaningful to the compositor (or its session
/// wrapper), but never to an application it launches.  More
/// importantly, a desktop shell may legitimately run
/// `dbus-update-activation-environment --all`; letting one of these
/// through would copy it into the persistent systemd/D-Bus activation
/// environment and poison the next login.  A nested test did exactly
/// that with `CHONKSTEP_BACKEND=winit` and
/// `CHONKSTEP_NO_APPEARANCE_PROPAGATION=1`, after which an SDDM session
/// inherited test behavior.
///
/// This is deliberately an allow-by-purpose list rather than every
/// `CHONKSTEP_*` variable.  `CHONKSTEP_SCALE`, `CHONKSTEP_THEME`,
/// `CHONKSTEP_APPEARANCE`, and `CHONKSTEP_CONTROL_SOCKET` are public
/// child-facing integration variables.  Dock socket/token variables
/// are passed only to their designated dockapp.
const INTERNAL_ENV: [&str; 21] = [
    "CHONKSTEP_BACKEND",
    "CHONKSTEP_DAMAGE_LOG",
    "CHONKSTEP_DRM_DEVICE",
    "CHONKSTEP_FOCUS_FOLLOWS_MOUSE",
    "CHONKSTEP_FULL_DAMAGE",
    "CHONKSTEP_HYPRLAND_IPC",
    "CHONKSTEP_NO_APPEARANCE_PROPAGATION",
    "CHONKSTEP_NO_CURSOR_PLANE",
    "CHONKSTEP_OWNS_XCURSOR_SIZE",
    "CHONKSTEP_SESSION_BIN",
    "CHONKSTEP_SESSION_CONTINUES",
    "CHONKSTEP_SESSION_TESTING",
    "CHONKSTEP_STRICT_BUFFER_RELEASE",
    "CHONKSTEP_TEST_CONFIG_HOME",
    "CHONKSTEP_TEST_GAMMA_SIZE",
    "CHONKSTEP_TEST_PANEL_TILE",
    "CHONKSTEP_TEST_RUST_LOG",
    "CHONKSTEP_TEST_SOCKET",
    "CHONKSTEP_WAYLAND_BIN",
    "_CHONKSTEP_BUS_WRAPPED",
    "_CHONKSTEP_UWSM",
];

/// What a dockapp is launched *without*: the display servers, and the
/// control socket. The socket is not a display connection, but it
/// answers "which windows exist, what is focused" and switches
/// workspaces — the window list is one of the things
/// [`DISPLAY_SERVER_ENV`] exists to keep out of a tile's reach, so
/// handing it the same list by another route would undo the hurdle.
/// A dockapp that wants any of it should be an application instead.
pub const DOCKAPP_WITHHELD_ENV: [&str; 3] = ["WAYLAND_DISPLAY", "DISPLAY", CONTROL_SOCKET_ENV];

/// Where the control socket was bound, once it has been. `None` until
/// `control::ControlSocket::new` says, and forever in a session whose
/// bind failed — a child should not be told about a socket nobody is
/// listening on.
static CONTROL_SOCKET: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Makes the control socket's path part of the environment of every
/// process this shell launches from now on.
///
/// Why a process-global read at spawn time, and not `std::env::set_var`
/// once at bind time: by the time `Shell::new` binds the socket, the
/// dock's sampler threads are already running `Command::output()`,
/// which walks `environ` — and `setenv` racing a reader of `environ` is
/// a use-after-free the Rust standard library documents as such (the
/// `XCURSOR_SIZE` export in `startup.rs` is safe only because it runs
/// before any thread exists). Injecting at `apply_env` instead
/// touches only the `Command` being built, needs no unsafety, reaches
/// every launch path the shell has — autostart, `[commands]`, menus,
/// terminals, session-layout relaunches — because they all go through
/// that one function, and is checkable with `Command::get_envs`. A hot
/// restart is a fresh process that binds and declares again, so the
/// once-only cell is never stale.
pub fn declare_control_socket(path: std::path::PathBuf) {
    // The socket is bound once per process; a second declaration would
    // be a second bind, which `StreamListener::bind` already refuses.
    let _ = CONTROL_SOCKET.set(path);
}

/// Same as [`spawn_detached`], with extra environment variables set on
/// top of whatever chonkstep's own process already has (which the child
/// inherits regardless — `Command` doesn't clear the parent environment
/// unless asked to). Exists for [`chromium_scale_args`]/[`gtk_qt_scale_env`]:
/// a third-party binary chonkstep doesn't control needs to be *told*
/// about the desktop's scale through whatever convention its own
/// toolkit understands, since it has no way to ask chonkstep for one.
///
/// Compositor-private variables are always removed at this boundary.
/// `unset` names any additional variables to *remove* from the child's
/// environment. It exists for dockapps, which must be launched with
/// [`DOCKAPP_WITHHELD_ENV`] cleared — see [`DISPLAY_SERVER_ENV`] for
/// why that is a requirement of the design rather than a precaution.
/// Pass `&[]` when there are no additional removals.
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

    /// Delivers an arbitrary signal to the child, if it has not been
    /// observed to exit.
    ///
    /// Exists for the terminals: foot swaps between its
    /// `colors-dark`/`colors-light` sections on SIGUSR1/SIGUSR2, which
    /// is how a live appearance switch reaches terminals that are
    /// already running. The same pid-race argument as
    /// [`Self::terminate`] applies: this process reaps its own
    /// children, so an unreaped pid is provably still this child's.
    pub fn signal(&self, signal: i32) {
        if self.exited().is_some() {
            return;
        }
        // SAFETY: `kill` with a pid this process spawned and has not
        // yet reaped; the only effect is delivering a signal.
        unsafe {
            libc::kill(self.pid as libc::pid_t, signal);
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
/// of an ambiguous instruction, and for [`DISPLAY_SERVER_ENV`] and
/// [`INTERNAL_ENV`] it is the only acceptable one.
fn apply_env(command: &mut Command, env: &[(String, String)], unset: &[&str]) {
    // First, so a caller's own `env` can override it and a caller's
    // `unset` can withhold it (as a dockapp's does).
    if let Some(path) = CONTROL_SOCKET.get() {
        command.env(CONTROL_SOCKET_ENV, path);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    for key in INTERNAL_ENV {
        command.env_remove(key);
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

/// Which of the two display stacks this desktop is running as: the X11
/// window manager (the `chonkstep` binary over `wm-x11`) or the Wayland
/// compositor (`chonkstep-wayland` over `wm-wayland`).
///
/// Deliberately a different question from `CHONKSTEP_BACKEND`, which the
/// compositor reads to pick between its DRM and winit halves. That one
/// is about what the compositor renders *through*; this one is about
/// which protocol the applications it launches should speak, and the
/// two have no bearing on each other - a nested winit session is every
/// bit as much a Wayland session to its clients as a DRM one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayStack {
    /// An X11 session: chonkstep is the window manager of an Xorg
    /// server somebody else started.
    X11,
    /// A Wayland session: chonkstep *is* the display server, and the
    /// apps it launches are its own clients.
    Wayland,
}

/// The file name of the compositor binary, and so the whole of the
/// evidence [`resolve_display_stack`] treats as positive proof of a
/// Wayland session. `crates/chonkstep-wayland` is the only binary in
/// the workspace that reaches `Shell` through `wm-wayland`, and
/// `scripts/wayland-session.sh` and `scripts/install.sh` both point a
/// real login session straight at `target/release/chonkstep-wayland`.
const WAYLAND_SESSION_IMAGE: &str = "chonkstep-wayland";

/// The stack this process is the desktop of.
///
/// The shell is generic over `Backend` and, by design, never learns
/// which one it was handed - that ignorance is what keeps the desktop
/// identical on both stacks. But *something* has to know here, because
/// a Chromium-family browser has to be told at launch which display
/// protocol to speak, and getting it wrong in the X11 direction means a
/// browser that never appears at all. So the answer is worked out once,
/// here, from the two facts a running process can be sure of.
///
/// The first and decisive one is the identity of the image actually
/// executing: `current_exe` resolves `/proc/self/exe`, which is the
/// kernel's own answer about which file this process is running, and
/// nothing inherited from a parent can forge it. The shell already
/// trusts it for the same class of question in `shell.rs`'s
/// `about_binary_path`.
///
/// The tempting alternative - "is `WAYLAND_DISPLAY` set?" - cannot
/// answer this on its own, and it is worth writing down why, because it
/// looks like it should. The compositor does export it (`wm-wayland`
/// sets it to its own socket before `Shell::new` runs, so that the apps
/// the shell launches find the session), and `scripts/wayland-session.sh`
/// clears both display variables at login precisely so that this kind of
/// deduction is made from the truth. But the *X11* session is not
/// always so clean: `scripts/dev-nested.sh` runs the X11 binary inside a
/// Xephyr window on the developer's ordinary Wayland desktop, where it
/// inherits that host's `WAYLAND_DISPLAY` alongside the Xephyr
/// `DISPLAY`. Both variables are set in both sessions there (the
/// compositor exports `DISPLAY` too, once XWayland is up), so no
/// combination of them separates the stacks - and a browser told to use
/// the Wayland platform in that session would connect to the *host*
/// compositor and map its window on the host's desktop, outside the
/// session that launched it.
///
/// `WAYLAND_DISPLAY` keeps a narrower job: a veto. The Wayland platform
/// is only ever selected when there is a socket in the environment for
/// the child to connect to, so the failure this whole area exists
/// because of - "Failed to connect to Wayland display", then no window -
/// stays impossible by construction rather than by argument.
/// What the running binary *told* us it is, which beats any amount of
/// deduction about it. `None` until a binary says.
static DECLARED_STACK: std::sync::OnceLock<DisplayStack> = std::sync::OnceLock::new();

/// Declares which session this process is the desktop of. Called once,
/// early, by each of the two binaries.
///
/// This exists because the deduction below was wrong in a way nobody
/// would guess and nothing would report. `current_exe` resolves
/// `/proc/self/exe`, and when the file behind a running process is
/// REPLACED — a rebuild, a package upgrade, `cargo build` while the
/// session it built is still running — the kernel answers with the path
/// plus a literal " (deleted)" suffix. The file name stops matching,
/// the compositor stops recognising itself, every Chromium-family
/// launch is told `--ozone-platform=x11` and
/// `--force-device-scale-factor`, and the browser comes back on
/// XWayland at double scale: text and layout wrong, clicks landing
/// somewhere other than where they were aimed. Reported as "X.com is
/// formatting weird and my clicks register in the wrong place", which
/// is not a description anyone would map onto a rebuilt binary.
///
/// The deduction was a good-faith answer to a real constraint (the
/// shell is generic over its backend and must not learn which one it
/// has), but it was inferring something the process already knew for
/// certain. A binary that links `wm-wayland` IS the Wayland session;
/// there is nothing to work out.
pub fn declare_display_stack(stack: DisplayStack) {
    // A second, differing declaration would mean two binaries in one
    // process, which cannot happen — so the first answer stands and a
    // repeat is harmless.
    let _ = DECLARED_STACK.set(stack);
}

pub fn current_display_stack() -> DisplayStack {
    let image = std::env::current_exe().ok();
    let image_name = image.as_deref().and_then(|path| path.file_name()).and_then(|name| name.to_str());
    stack_with_declaration(
        DECLARED_STACK.get().copied(),
        image_name,
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
    )
}

/// Pure core of [`current_display_stack`]: a declaration is the answer
/// when there is one, and the deduction is the fallback. Split out for
/// the same reason [`resolve_display_stack`] is — a rule reachable only
/// by setting a process-global is a rule the tests cannot exercise
/// twice.
fn stack_with_declaration(
    declared: Option<DisplayStack>,
    image_name: Option<&str>,
    wayland_display: Option<&str>,
) -> DisplayStack {
    declared.unwrap_or_else(|| resolve_display_stack(image_name, wayland_display))
}

/// Pure core of [`current_display_stack`], split out for the reason
/// every resolver in [`crate::startup`] is: the rule is the interesting
/// part, and a rule that can only be exercised by rewriting the process
/// environment is a rule the tests cannot safely reach.
///
/// Anything unrecognized answers `X11`. That is not a coin toss - it is
/// the answer that shipped unconditionally on both stacks until this
/// function existed, and it is the survivable one: an X11 browser under
/// the compositor is a browser with the wrong scaling and a doubled
/// titlebar, while a Wayland browser under an X session is no browser.
pub fn resolve_display_stack(image_name: Option<&str>, wayland_display: Option<&str>) -> DisplayStack {
    // `/proc/self/exe` answers with a " (deleted)" suffix once the file
    // behind a running process has been replaced, which is the ordinary
    // state of any session whose binary was rebuilt or upgraded under
    // it. Stripping it keeps this fallback honest for the same reason
    // `declare_display_stack` exists; with both binaries declaring,
    // nothing reaches here but the tests.
    let image_name = image_name.map(|name| name.strip_suffix(" (deleted)").unwrap_or(name));
    let compositor = image_name == Some(WAYLAND_SESSION_IMAGE);
    let socket_to_connect_to = wayland_display.is_some_and(|name| !name.is_empty());
    if compositor && socket_to_connect_to {
        DisplayStack::Wayland
    } else {
        DisplayStack::X11
    }
}

/// Pins a Chromium-family browser to the ozone platform of the stack it
/// is being launched from.
///
/// The X11 half is the older and the more urgent one. Omarchy is
/// Wayland-first, and its Chromium configuration selects the Wayland
/// platform (`~/.config/chromium-flags.conf` on this machine still opens
/// with `--ozone-platform=wayland`) - which does not exist inside an X11
/// session, so the browser printed "Failed to connect to Wayland
/// display" and exited without ever mapping a window (confirmed live
/// from the Applications menu). Chromium honors the *last* occurrence of
/// a switch, and these launcher args are appended after any flags file -
/// the `microsoft-edge-stable` wrapper script execs the browser with the
/// config file's flags first and `"$@"` after - so this wins.
///
/// It used to be sent on both stacks, because it was written when there
/// was only one. Under the compositor that quietly turned every
/// Chromium-family browser into an XWayland client for no reason: it
/// gave up the native Wayland scaling path, took its input through
/// XWayland's translation rather than the compositor's own seat, and -
/// because this desktop imposes server-side decorations - drew its own
/// titlebar directly underneath the one the window manager draws. The
/// browser should be a first-class client of whichever session it was
/// launched from, the same argument the terminal comment in `shell.rs`
/// makes for foot over urxvt.
///
/// Nothing is sent alongside the platform switch to make the server-side
/// decorations stick, and that is a decision rather than an oversight.
/// The compositor does not *ask* clients to accept them: `wm-wayland`'s
/// `XdgDecorationHandler` answers `ServerSide` to every request and
/// every unset, and a toplevel that never binds
/// `zxdg_decoration_manager_v1` at all is configured `ServerSide` from
/// its first configure anyway, so the protocol side needs no help from a
/// command line. The only switch that could plausibly be added here is
/// an `--enable-features=` entry, and that is exactly the switch that
/// must not be appended blindly: last-occurrence-wins applies to a
/// switch's entire value, so ours would not extend the user's
/// `--enable-features=TouchpadOverscrollHistoryNavigation` line but
/// replace it. Trading away a setting the user really has, for a feature
/// name whose spelling and default have moved between Chromium
/// versions, is the wrong side of the rule that a wrong flag which stops
/// a browser launching costs far more than a missing nicety.
pub fn chromium_platform_args(stack: DisplayStack) -> Vec<String> {
    match stack {
        DisplayStack::Wayland => vec!["--ozone-platform=wayland".to_string()],
        DisplayStack::X11 => vec!["--ozone-platform=x11".to_string()],
    }
}

/// Environment variables that make Qt-based UI — including the native
/// file-open/save dialogs a Chromium browser itself delegates to on
/// Linux — honor this desktop's scale too.
///
/// GTK is deliberately absent from this list. It used to be here
/// (`GDK_SCALE`/`GDK_DPI_SCALE`), from back when this WM ran no
/// XSETTINGS daemon and had no other way to tell a GTK client its
/// scale. `chonk_xsettings::XSettingsManager` (wired up in `main.rs`)
/// replaced that: it publishes `Gdk/WindowScalingFactor`, `Xft/DPI` and
/// `Gdk/UnscaledDPI`, which is the same mechanism a full desktop
/// environment uses and — unlike the env var — doesn't double-scale.
/// Setting `GDK_SCALE` puts a GTK client's X11 screen into "fixed window
/// scale" mode, and GTK's own xsettings client only substitutes
/// `Gdk/UnscaledDPI` for `Xft/DPI` when *not* in that mode (see
/// `gdk/x11/xsettings-client.c`) — so a client handed both the env var
/// and this desktop's XSETTINGS drew at the scale twice: once from the
/// forced backing-store scale, once more from the now-unguarded, already
/// -scaled `Xft/DPI` (confirmed live: LibreOffice, a GTK3 app, rendered
/// at 4x on a 2x-scale session). Qt has no equivalent XSETTINGS client,
/// so it still needs telling directly.
///
/// `QT_SCALE_FACTOR` accepts a plain float, unlike `GDK_SCALE`, so no
/// integer/remainder split is needed here.
pub fn gtk_qt_scale_env(scale: f32) -> Vec<(String, String)> {
    vec![
        ("QT_SCALE_FACTOR".to_string(), format!("{scale:.4}")),
        ("QT_AUTO_SCREEN_SCALE_FACTOR".to_string(), "0".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qt_scale_factor_carries_the_exact_requested_scale() {
        for scale in [1.0f32, 1.5, 2.0, 3.0, 2.25] {
            let env = gtk_qt_scale_env(scale);
            let qt_scale: f32 = env.iter().find(|(k, _)| k == "QT_SCALE_FACTOR").unwrap().1.parse().unwrap();
            assert!((qt_scale - scale).abs() < 0.01, "scale {scale}: QT_SCALE_FACTOR should reconstruct it exactly");
        }
    }

    #[test]
    fn gdk_scale_is_not_set_xsettings_owns_gtk_scale_now() {
        let env = gtk_qt_scale_env(1.5);
        assert!(env.iter().all(|(k, _)| k != "GDK_SCALE" && k != "GDK_DPI_SCALE"), "GDK_SCALE/GDK_DPI_SCALE would fix a GTK client's window scale and disable the Gdk/UnscaledDPI xsettings override, double-scaling it");
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
    fn a_declared_control_socket_reaches_every_launch_but_a_dockapps() {
        // The route `declare_control_socket` documents, asserted on the
        // `Command` rather than on a spawned process: the cell is
        // process-global, so this is the one test in the binary that
        // sets it, and it sets it to a path no other test looks for.
        declare_control_socket(std::path::PathBuf::from("/run/user/1000/chonkstep/control-test.sock"));
        let mut launch = Command::new("/bin/true");
        apply_env(&mut launch, &[], &[]);
        let exported = launch.get_envs().find(|(key, _)| *key == std::ffi::OsStr::new(CONTROL_SOCKET_ENV));
        assert_eq!(
            exported.and_then(|(_, value)| value),
            Some(std::ffi::OsStr::new("/run/user/1000/chonkstep/control-test.sock")),
            "an ordinary launch inherits the control socket"
        );
        let mut dockapp = Command::new("/bin/true");
        apply_env(&mut dockapp, &[], &DOCKAPP_WITHHELD_ENV);
        let withheld = dockapp.get_envs().find(|(key, _)| *key == std::ffi::OsStr::new(CONTROL_SOCKET_ENV));
        assert_eq!(withheld.map(|(_, value)| value), Some(None), "a dockapp has it removed, not merely unset");
    }

    #[test]
    fn ordinary_launches_keep_public_environment_but_drop_internal_controls() {
        // The public toolkit values supplied by a caller still win,
        // while private controls are removals even with no
        // caller-specific `unset` list.
        let mut command = Command::new("/bin/true");
        apply_env(&mut command, &gtk_qt_scale_env(2.0), &[]);
        let envs: Vec<_> = command.get_envs().collect();
        for variable in ["QT_SCALE_FACTOR", "QT_AUTO_SCREEN_SCALE_FACTOR"] {
            let entry = envs.iter().find(|(key, _)| *key == std::ffi::OsStr::new(variable));
            assert!(entry.is_some_and(|(_, value)| value.is_some()), "{variable} must still be supplied");
        }
        for variable in INTERNAL_ENV {
            let entry = envs.iter().find(|(key, _)| *key == std::ffi::OsStr::new(variable));
            assert_eq!(entry.map(|(_, value)| *value), Some(None), "{variable} must be removed from every child");
        }
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

    #[test]
    fn the_compositors_own_image_launches_browsers_as_native_wayland_clients() {
        // The whole point of the change: under `chonkstep-wayland` a
        // browser is a first-class client of the session, not an
        // XWayland guest wearing two titlebars.
        assert_eq!(resolve_display_stack(Some("chonkstep-wayland"), Some("wayland-1")), DisplayStack::Wayland);
        assert_eq!(chromium_platform_args(DisplayStack::Wayland), vec!["--ozone-platform=wayland".to_string()]);
    }

    /// The reported bug, as the kernel actually presents it. Rebuild a
    /// running session's binary — `cargo build`, a package upgrade —
    /// and `/proc/self/exe` starts answering with a " (deleted)"
    /// suffix. The compositor stopped recognising its own image, every
    /// browser it launched was pushed onto XWayland at double scale,
    /// and the user saw "the page is formatted weird and my clicks land
    /// in the wrong place".
    #[test]
    fn a_rebuilt_binary_is_still_the_compositor() {
        assert_eq!(
            resolve_display_stack(Some("chonkstep-wayland (deleted)"), Some("wayland-1")),
            DisplayStack::Wayland,
            "a replaced-on-disk compositor must not start launching X11 browsers"
        );
        // The X11 half of the same trap.
        assert_eq!(resolve_display_stack(Some("chonkstep (deleted)"), Some("wayland-1")), DisplayStack::X11);
        // And the suffix is only ever stripped from the end, so it
        // cannot smuggle an unrelated name into a match.
        assert_eq!(resolve_display_stack(Some("chonkstep-wayland (deleted) x"), Some("wayland-1")), DisplayStack::X11);
    }

    /// Better than any deduction: the binary says which session it is,
    /// and nothing about the file it was loaded from can contradict it.
    #[test]
    fn a_declared_stack_beats_the_deduction() {
        // The case that was broken in the wild: a compositor whose
        // binary has been replaced under it, which the deduction reads
        // as X11 and a declaration reads correctly.
        assert_eq!(
            stack_with_declaration(Some(DisplayStack::Wayland), Some("chonkstep-wayland (deleted)"), None),
            DisplayStack::Wayland,
            "a declaration outranks both the image name and the missing socket"
        );
        assert_eq!(
            stack_with_declaration(Some(DisplayStack::X11), Some("chonkstep-wayland"), Some("wayland-1")),
            DisplayStack::X11,
            "the X11 binary nested in a Wayland desktop stays X11 whatever the environment says"
        );
        // With nothing declared, the deduction still answers.
        assert_eq!(
            stack_with_declaration(None, Some("chonkstep-wayland"), Some("wayland-1")),
            DisplayStack::Wayland
        );
        assert_eq!(stack_with_declaration(None, Some("chonkstep"), Some("wayland-1")), DisplayStack::X11);
    }

    #[test]
    fn the_x11_window_manager_still_pins_x11_even_from_inside_a_wayland_desktop() {
        // The load-bearing branch, and the reason the decision is not a
        // bare `WAYLAND_DISPLAY` check: `scripts/dev-nested.sh` runs the
        // X11 binary in a Xephyr window on a Wayland host, so the host's
        // socket is right there in the environment. Choosing Wayland on
        // that evidence would put the browser on the host's desktop; in
        // a real X session it would produce no browser at all.
        assert_eq!(resolve_display_stack(Some("chonkstep"), Some("wayland-1")), DisplayStack::X11);
        assert_eq!(resolve_display_stack(Some("chonkstep"), None), DisplayStack::X11);
        assert_eq!(chromium_platform_args(DisplayStack::X11), vec!["--ozone-platform=x11".to_string()]);
    }

    #[test]
    fn a_compositor_with_no_socket_in_its_environment_declines_to_send_a_browser_at_one() {
        // The veto half. This should be unreachable - the compositor
        // exports its socket before the shell can launch anything - and
        // it is asserted anyway because the failure it guards against is
        // the silent one: a browser that connects to nothing and exits.
        assert_eq!(resolve_display_stack(Some("chonkstep-wayland"), None), DisplayStack::X11);
        assert_eq!(resolve_display_stack(Some("chonkstep-wayland"), Some("")), DisplayStack::X11);
    }

    #[test]
    fn an_unrecognized_image_gets_the_answer_that_used_to_ship_unconditionally() {
        // A test binary, a renamed build, some future embedder: none of
        // them are proof of a Wayland session, and the safe default is
        // the flag both stacks have been running with all along.
        assert_eq!(resolve_display_stack(None, Some("wayland-1")), DisplayStack::X11);
        assert_eq!(resolve_display_stack(Some("spawn-8f3c1d2e"), Some("wayland-1")), DisplayStack::X11);
    }
}
