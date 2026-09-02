//! End-to-end harness for the chonkstep Wayland compositor: boot a
//! real nested compositor, launch real clients, inject real input,
//! screenshot the real renderer, assert.
//!
//! # Why this crate exists
//!
//! Three consecutive rounds of work each shipped a regression that no
//! unit test could see and a human found by hand:
//!
//! - a drag that never ended (the release went to the client and
//!   `wm-core` never heard it);
//! - clicks landing offset from where they visually landed (a stale
//!   pointer anchor teleporting the drag);
//! - a scale change that collapsed the whole desktop into a quarter
//!   of the screen (wallpaper clipped, dock gone).
//!
//! All three live in the seam between the real input path and the
//! real renderer — exactly the seam the fake backend the unit suite
//! drives replaces. This crate closes that gap: [`Session::boot`]
//! starts `chonkstep-wayland` on its winit backend as an ordinary
//! window inside the developer's own Wayland session, with its config
//! and state directories isolated into a per-test scratch directory;
//! [`Door`] speaks the compositor's `CHONKSTEP_TEST_SOCKET` control
//! protocol (see `wm-wayland/src/test_door.rs` for the wire spec);
//! [`Screenshot`] captures through the compositor's own
//! `zwlr_screencopy_v1` implementation via `grim`, so an assertion on
//! a pixel is an assertion on what a real screenshot tool — and a
//! real user — actually sees.
//!
//! The tests in `tests/e2e.rs` are those regressions spelled as
//! assertions, named after the behavior they pin. They need a live
//! Wayland session to nest inside, which GitHub CI does not have (see
//! ci.yml's wayland job comment), so they are `#[ignore]`d; run them
//! locally with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.
//!
//! # House rules this crate enforces on itself
//!
//! - **Every wait is a bounded poll on an observable condition** — a
//!   socket accepting, a log line, a window appearing in the ledger,
//!   a `barrier` ack — never a bare sleep. Flaky end-to-end tests are
//!   worse than none; a test that sleeps "long enough" is one CI-box
//!   hiccup away from flaky.
//! - **Everything started gets killed.** [`Session`]'s `Drop` kills
//!   the clients it launched and then the compositor, so a failing
//!   assertion (a panic mid-test) cannot leave a nested compositor
//!   window squatting on the developer's desktop.
//! - **No blocking child-process calls.** The workspace bans
//!   `Command::output`/`status`/`Child::wait` outright (see the root
//!   `clippy.toml` for the wifi-tile post-mortem). This crate never
//!   needs an exemption: children are reaped with `try_wait` inside
//!   the same bounded polls everything else uses.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long [`Session::boot`] waits for the compositor to open its
/// wayland socket and its test door. Generous because the first boot
/// after a rebuild pays cold caches; the poll returns the instant the
/// door accepts.
const BOOT_TIMEOUT: Duration = Duration::from_secs(20);

/// Default deadline for everything after boot: a window appearing, a
/// barrier acking, grim finishing. Anything slower than this on an
/// otherwise idle machine is a bug, not a slow day.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll cadence. Fine enough that a test never waits noticeably past
/// the condition becoming true, coarse enough to cost nothing.
const POLL_STEP: Duration = Duration::from_millis(25);

/// Drops ANSI CSI escape sequences (`\x1b[...m` and friends) from a
/// line, so a log assertion matches the text a human reads rather
/// than the bytes a colorizer wrote around it.
///
/// Two writers on the harness's log files color them: `tracing`
/// colors the compositor's log unconditionally, and libwayland (1.26
/// on this machine) colors `WAYLAND_DEBUG` output whenever
/// `FORCE_COLOR` is in the client's environment — even into a plain
/// file. The escapes land *inside* the tokens tests match on
/// (`wl_keyboard\x1b[35m#15\x1b[36m.enter\x1b[0m(`), so a substring
/// like `"wl_keyboard#"` silently never matches again the day the
/// environment starts forcing color — which is exactly how the
/// miniaturize-restore e2e went red with a perfectly healthy
/// compositor. Every log matcher in this crate and its tests goes
/// through here first; none may grep raw bytes.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC '[' parameters, terminated by a byte in @..~.
            if chars.next() == Some('[') {
                for end in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&end) {
                        break;
                    }
                }
            }
            // A bare ESC (or ESC + one non-CSI byte) is dropped too;
            // no log this crate reads contains one legitimately.
        } else {
            out.push(c);
        }
    }
    out
}

/// Runs `condition` until it yields `Some`, at most until `timeout`
/// has elapsed. The only wait primitive in this crate — every use
/// names the observable condition it polls, which is the whole
/// anti-flake policy in one function signature.
pub fn poll_until<T>(
    timeout: Duration,
    what: &str,
    mut condition: impl FnMut() -> Option<T>,
) -> Result<T, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = condition() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out after {timeout:?} waiting for {what}"));
        }
        std::thread::sleep(POLL_STEP);
    }
}

// -- the session ---------------------------------------------------------

/// Options for [`Session::boot`]. `scale` lands in the isolated
/// config file's `scale =` line — the same file a user would edit —
/// so the compositor resolves it through the very
/// `SessionState::resolve` path the scale-2 regression shipped in.
#[derive(Default)]
pub struct SessionOptions {
    pub scale: Option<f32>,
    /// Extra lines appended verbatim to the isolated config file —
    /// how a test opts into keys the harness has no dedicated field
    /// for (`restore_session = true`, `lock_command = ...`).
    pub config_extra: String,
    /// State files seeded into the isolated `state/chonkstep/`
    /// directory *before* the compositor boots, as `(file name,
    /// contents)` — how a restore test plants the previous session's
    /// layout for the fresh compositor to find.
    pub state_files: Vec<(String, String)>,
    /// Files seeded into the isolated `config/chonkstep/` directory
    /// before boot, as `(relative path, contents)` — parent directories
    /// created as needed. How the instrument-panel e2e registers a
    /// dockapp (`dockapps/probe.dockapp`) with the fresh shell, which
    /// scans that directory at startup.
    pub config_files: Vec<(String, String)>,
    /// Extra environment for the compositor process, as `(name,
    /// value)` — how a test points the shell at something it discovers
    /// from the environment rather than from config, such as a scratch
    /// `OMARCHY_PATH` holding a menu definition of the test's own
    /// making. Applied after the harness's own variables, so a test
    /// can also deliberately override one of those.
    pub env: Vec<(String, String)>,
    /// Files seeded into the isolated `XDG_STATE_HOME` root itself
    /// (not under `chonkstep/`) before boot, as `(relative path,
    /// contents)` — for state that belongs to *another* program the
    /// session reads: the Omarchy e2e plants
    /// `omarchy/current/theme/colors.toml` and `omarchy/current/theme.name`
    /// here, exactly where `omarchy-theme-set` would put them.
    pub state_root_files: Vec<(String, String)>,
}

/// One booted nested compositor plus everything needed to drive and
/// observe it. Killed (clients first, compositor second) on drop.
pub struct Session {
    /// Per-test scratch: config, state, logs, screenshots, the door
    /// socket. Left on disk after the test so a failure can be
    /// investigated from the artifacts.
    pub dir: PathBuf,
    compositor: Child,
    clients: Vec<Child>,
    door: Door,
    /// The nested compositor's own wayland socket name (e.g.
    /// "wayland-2"), parsed from its log — what clients and grim get
    /// as `WAYLAND_DISPLAY`.
    pub wayland_display: String,
    log_path: PathBuf,
    screenshot_serial: u32,
}

impl Session {
    /// Boots `chonkstep-wayland` nested (winit backend) with isolated
    /// `XDG_CONFIG_HOME`/`XDG_STATE_HOME`, the test door enabled, and
    /// waits — bounded — until both the wayland socket and the door
    /// are demonstrably up.
    pub fn boot(name: &str, options: SessionOptions) -> Result<Session, String> {
        let dir = std::env::temp_dir().join("chonk-testkit").join(name);
        // A fresh directory per boot: leftovers from the previous run
        // (an old door socket, an old config) must not leak into this
        // one. Failure to remove is fine the first time around.
        let _ = std::fs::remove_dir_all(&dir);
        let config_home = dir.join("config");
        let state_home = dir.join("state");
        std::fs::create_dir_all(config_home.join("chonkstep")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(state_home.join("chonkstep")).map_err(|e| e.to_string())?;

        let mut config = String::new();
        if let Some(scale) = options.scale {
            config.push_str(&format!("scale = {scale}\n"));
        }
        config.push_str(&options.config_extra);
        std::fs::write(config_home.join("chonkstep/config.toml"), config)
            .map_err(|e| e.to_string())?;
        for (name, contents) in &options.state_files {
            std::fs::write(state_home.join("chonkstep").join(name), contents)
                .map_err(|e| e.to_string())?;
        }
        for (name, contents) in &options.config_files {
            let path = config_home.join("chonkstep").join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, contents).map_err(|e| e.to_string())?;
        }
        for (name, contents) in &options.state_root_files {
            let path = state_home.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, contents).map_err(|e| e.to_string())?;
        }

        let door_path = dir.join("door.sock");
        let log_path = dir.join("compositor.log");
        let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
        let log_err = log.try_clone().map_err(|e| e.to_string())?;

        let compositor = Command::new(compositor_binary()?)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_STATE_HOME", &state_home)
            .env("CHONKSTEP_BACKEND", "winit")
            .env("CHONKSTEP_TEST_SOCKET", &door_path)
            .env("RUST_LOG", "info")
            // GSettings is per-user, not per-scratch-dir: without this,
            // a posed session switching its appearance would run
            // `gsettings set` against the developer's real preferences.
            // The shell checks the variable before propagating.
            .env("CHONKSTEP_NO_APPEARANCE_PROPAGATION", "1")
            // The developer's shell may carry CHONKSTEP_SCALE (the
            // dev-nested script exports it); it would silently beat
            // the config file this harness just wrote.
            .env_remove("CHONKSTEP_SCALE")
            // Same hazard, worse symptom. A session that has ever been
            // hot-restarted leaves CHONKSTEP_SESSION_CONTINUES in the
            // environment of every process it launches, terminals
            // included — so a developer running this suite from a
            // restarted desktop hands each posed session a marker
            // saying "you are a continuation". The shell believes it
            // and skips exactly the two things a fresh session does:
            // the layout restore `session_restore.rs` exists to test,
            // and autostart. The suite would then pass or fail
            // depending on whether the developer had pressed the
            // restart key that day, which is the worst kind of flake.
            //
            // The compositor now consumes the marker at startup
            // (`chonk_shell::startup::consume_session_continuation`) so
            // it stops propagating, but a harness must not depend on
            // the thing it is testing having already fixed itself.
            .env_remove("CHONKSTEP_SESSION_CONTINUES")
            .envs(options.env.iter().map(|(name, value)| (name.as_str(), value.as_str())))
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| format!("could not spawn chonkstep-wayland: {e}"))?;

        let mut session = Session {
            dir,
            compositor,
            clients: Vec::new(),
            door: Door::unconnected(),
            wayland_display: String::new(),
            log_path,
            screenshot_serial: 0,
        };

        // Observable boot conditions, in dependency order: the log
        // names the wayland socket, then the door accepts a connect.
        // Both bounded; a compositor that died meanwhile fails fast
        // with its log tail instead of timing out mutely.
        session.wayland_display = {
            let log_path = session.log_path.clone();
            let compositor = &mut session.compositor;
            poll_until(BOOT_TIMEOUT, "the compositor to announce its wayland socket", || {
                if let Ok(Some(status)) = compositor.try_wait() {
                    return Some(Err(format!("compositor exited during boot: {status}")));
                }
                let log = std::fs::read_to_string(&log_path).unwrap_or_default();
                // The value is pulled out by its `"wayland-N"` shape
                // rather than by the `socket=` key, because tracing
                // writes ANSI color sequences into the file and the
                // escapes sit exactly between the key and the quote.
                log.lines()
                    .find(|line| line.contains("wayland socket listening"))
                    .and_then(|line| line.split("\"wayland-").nth(1))
                    .and_then(|rest| rest.split('"').next())
                    .map(|number| Ok(format!("wayland-{number}")))
            })??
        };
        session.door = poll_until(BOOT_TIMEOUT, "the test door to accept a connection", || {
            Door::connect(&door_path).ok()
        })?;
        // Connecting only proves the listener is bound (that happens
        // before the event loop starts). A session is booted when it
        // *answers*: one barrier round-trip, bounded by the door's own
        // read deadline, and every later wait in a test starts against
        // a responsive compositor. (The first pass used to pay ~11s
        // re-painting the whole desktop for winit's no-op initial
        // resize in an unoptimized build; both halves of that are
        // fixed — `on_output_resized`'s same-size guard, and the
        // workspace `[profile.dev.package.*]` opt-levels — but the
        // barrier stays, because "booted" should mean "answers", not
        // "probably fast now".)
        session
            .door
            .barrier()
            .map_err(|e| format!("the compositor never answered its first barrier: {e}"))?;
        Ok(session)
    }

    /// The injection door for this session.
    pub fn door(&mut self) -> &mut Door {
        &mut self.door
    }

    /// Launches a client inside the nested session. The environment
    /// points it at the nested compositor and *only* the nested
    /// compositor: `DISPLAY` is stripped and `GDK_BACKEND` pinned so a
    /// toolkit cannot quietly open on the host session and pass a test
    /// against the wrong desktop.
    pub fn launch(&mut self, program: &str, args: &[&str]) -> Result<(), String> {
        // Client output is kept per launch: "the client never mapped"
        // is undiagnosable from a /dev/null.
        let log_path = self.dir.join(format!("client-{}-{program}.log", self.clients.len()));
        let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
        let log_err = log.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new(program)
            .args(args)
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env_remove("DISPLAY")
            .env("GDK_BACKEND", "wayland")
            // The client's log is machine-read (`WAYLAND_DEBUG`
            // assertions match substrings), and libwayland ≥1.26
            // honors these by coloring its debug stream even into a
            // file — escapes landing mid-token. Tests strip ANSI
            // anyway (`strip_ansi`), but a harness that ASKS for
            // plain logs fails one environment change later instead
            // of two.
            .env_remove("FORCE_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| format!("could not launch {program}: {e}"))?;
        self.clients.push(child);
        Ok(())
    }

    /// Waits for a mapped window whose app id or title contains
    /// `needle`, polling the door's ledger query. This is the "did the
    /// client actually come up" condition every test starts from.
    pub fn wait_for_window(&mut self, needle: &str) -> Result<WindowInfo, String> {
        let door = &mut self.door;
        poll_until(DEFAULT_TIMEOUT, &format!("a mapped window matching {needle:?}"), || {
            let world = door.windows().ok()?;
            world.window_matching(needle).cloned()
        })
    }

    /// Waits for the window matching `needle` to be *gone* from the
    /// ledger — the observable meaning of "the dialog closed".
    pub fn wait_for_window_gone(&mut self, needle: &str) -> Result<(), String> {
        let door = &mut self.door;
        poll_until(DEFAULT_TIMEOUT, &format!("the window matching {needle:?} to close"), || {
            let world = door.windows().ok()?;
            world.window_matching(needle).is_none().then_some(())
        })
    }

    /// The current ledger snapshot, via the door.
    pub fn world(&mut self) -> Result<World, String> {
        self.door.windows()
    }

    /// Captures the nested session's output through its own
    /// screencopy protocol (`grim` is the client) and loads the
    /// pixels. The barrier the caller ran beforehand is what makes
    /// this a picture of a settled scene rather than a race.
    pub fn screenshot(&mut self, label: &str) -> Result<Screenshot, String> {
        self.screenshot_serial += 1;
        let path = self.dir.join(format!("{:02}-{label}.png", self.screenshot_serial));
        let mut grim = Command::new("grim")
            .arg(&path)
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not spawn grim: {e}"))?;
        let status = poll_until(DEFAULT_TIMEOUT, "grim to finish", || {
            grim.try_wait().ok().flatten()
        });
        let status = match status {
            Ok(status) => status,
            Err(timeout) => {
                let _ = grim.kill();
                return Err(timeout);
            }
        };
        if !status.success() {
            return Err(format!("grim exited with {status}"));
        }
        Screenshot::load(&path)
    }

    /// Rewrites the isolated config file — the file half of the
    /// live-reload gesture.
    pub fn rewrite_config(&self, contents: &str) -> Result<(), String> {
        std::fs::write(self.dir.join("config/chonkstep/config.toml"), contents)
            .map_err(|e| e.to_string())
    }

    /// Touches the reload marker in the isolated state dir — the
    /// trigger half, exactly what `scripts/reload.sh` does.
    pub fn request_reload(&self) -> Result<(), String> {
        std::fs::write(self.dir.join("state/chonkstep/reload"), "").map_err(|e| e.to_string())
    }

    /// Whether the compositor process is still running — the
    /// "reload must not kill the session" assertion.
    pub fn compositor_alive(&mut self) -> bool {
        matches!(self.compositor.try_wait(), Ok(None))
    }

    /// The compositor's captured log so far.
    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    /// A file in the isolated state directory — where the compositor's
    /// own state files (`session`, `theme`, `dock`, ...) land, and what
    /// a persistence test reads to assert on what would survive a
    /// crash.
    pub fn state_file(&self, name: &str) -> PathBuf {
        self.dir.join("state/chonkstep").join(name)
    }

    /// Kills every client this session launched — the test-side stand-in
    /// for the user closing their windows. Reaped with the same bounded
    /// polls everything else uses.
    pub fn kill_clients(&mut self) {
        for client in &mut self.clients {
            let _ = client.kill();
        }
        for client in &mut self.clients {
            let _ = poll_until(Duration::from_secs(2), "a killed client to be reaped", || {
                client.try_wait().ok().flatten()
            });
        }
        self.clients.clear();
    }

    /// Kills the compositor with SIGKILL — the harshest crash there is
    /// (no destructors, no flushes), for asserting that persisted state
    /// really was already on disk beforehand. The `Session` remains
    /// droppable afterwards; every later door call will simply fail.
    pub fn kill_compositor(&mut self) {
        let _ = self.compositor.kill();
        let _ = poll_until(Duration::from_secs(2), "the killed compositor to be reaped", || {
            self.compositor.try_wait().ok().flatten()
        });
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Clients first (they die anyway once the compositor's socket
        // goes, but killing them explicitly reaps them), compositor
        // last. Reaping is a bounded try_wait poll, never a blocking
        // wait — see the module docs.
        for client in &mut self.clients {
            let _ = client.kill();
        }
        let _ = self.compositor.kill();
        for client in &mut self.clients {
            let _ = poll_until(Duration::from_secs(2), "a client to be reaped", || {
                client.try_wait().ok().flatten()
            });
        }
        let _ = poll_until(Duration::from_secs(2), "the compositor to be reaped", || {
            self.compositor.try_wait().ok().flatten()
        });
    }
}

/// Where the compositor binary lives: `CHONKSTEP_WAYLAND_BIN` if set,
/// else `chonkstep-wayland` next to the test executable's profile dir
/// (`target/debug`). `scripts/e2e.sh` builds it first; a missing
/// binary fails with the command to run rather than a bare ENOENT.
fn compositor_binary() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("CHONKSTEP_WAYLAND_BIN") {
        return Ok(PathBuf::from(path));
    }
    profile_binary("chonkstep-wayland")
}

/// A sibling binary from the same build profile as the running test —
/// how a test finds `chonkstep-wayland` and this crate's own
/// `chonk-panel-probe` without hardcoding a target directory. A
/// missing binary fails with the command that builds it rather than a
/// bare ENOENT.
pub fn profile_binary(name: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // target/debug/deps/e2e-... -> target/debug/<name>
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot locate the target profile directory")?;
    let bin = profile_dir.join(name);
    if !bin.exists() {
        return Err(format!(
            "{} not found — build it first: cargo build -p chonkstep-wayland -p chonk-testkit (scripts/e2e.sh does this)",
            bin.display()
        ));
    }
    Ok(bin)
}

// -- the door client -----------------------------------------------------

/// A parsed `window` line from the door's `windows` reply. Geometry
/// is in the ledger's physical-pixel space — the same space `motion`
/// coordinates are given in, so `press on the titlebar` is arithmetic
/// on these fields and nothing else.
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub id: u64,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub mapped: bool,
    pub app: String,
    pub title: String,
}

/// A parsed `frame` line: the server-drawn decoration around a
/// window. Its rectangle minus the window's rectangle is where the
/// titlebar and borders are.
#[derive(Clone, Debug)]
pub struct FrameInfo {
    pub id: u64,
    pub window: u64,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub mapped: bool,
}

/// A parsed `shell` line: a desktop-owned surface — the dock, the
/// pager, menus. The scale-2 regression's headline symptom was the
/// dock: present in the ledger, drawn wrong. Tests read the "should"
/// from these and the "is" from a screenshot.
#[derive(Clone, Debug)]
pub struct ShellInfo {
    pub id: u64,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub mapped: bool,
    pub above: bool,
}

/// The shell's own account of its dress, from the `theme` line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeInfo {
    pub id: String,
    pub name: String,
    /// `"light"` or `"dark"`.
    pub appearance: String,
    /// `"omarchy"` while the session follows Omarchy, else empty.
    pub following: String,
}

/// One `windows` reply: the compositor's whole idea of the screen.
#[derive(Clone, Debug, Default)]
pub struct World {
    pub scale: f32,
    pub output_w: u32,
    pub output_h: u32,
    pub theme: ThemeInfo,
    pub windows: Vec<WindowInfo>,
    pub frames: Vec<FrameInfo>,
    pub shells: Vec<ShellInfo>,
}

impl World {
    /// The first mapped window whose app id or title contains
    /// `needle`.
    pub fn window_matching(&self, needle: &str) -> Option<&WindowInfo> {
        self.windows
            .iter()
            .find(|w| w.mapped && (w.app.contains(needle) || w.title.contains(needle)))
    }

    /// The frame decorating `window`, if the server drew one.
    pub fn frame_of(&self, window: u64) -> Option<&FrameInfo> {
        self.frames.iter().find(|f| f.window == window)
    }

    /// The dock: the mapped shell surface that is a column (taller
    /// than wide) flush against the output's right edge — the shape
    /// the shell gives it at every scale.
    pub fn dock(&self) -> Option<&ShellInfo> {
        self.shells.iter().find(|s| {
            s.mapped && s.h > s.w && s.x + s.w as i32 == self.output_w as i32
        })
    }
}

/// Client for the compositor's test door (`CHONKSTEP_TEST_SOCKET`).
/// The wire protocol is documented in `wm-wayland/src/test_door.rs`;
/// this is its only speaker.
pub struct Door {
    stream: Option<BufReader<UnixStream>>,
}

impl Door {
    fn unconnected() -> Door {
        Door { stream: None }
    }

    fn connect(path: &Path) -> Result<Door, String> {
        let stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
        // A barrier ack can legitimately trail a heavy pass by many
        // seconds in a debug build — a live restyle to scale 2 was
        // measured at ~11s — so the read deadline is the long one.
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| e.to_string())?;
        Ok(Door { stream: Some(BufReader::new(stream)) })
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        let stream = self.stream.as_mut().ok_or("door not connected")?;
        stream
            .get_mut()
            .write_all(format!("{line}\n").as_bytes())
            .map_err(|e| format!("door write failed: {e}"))
    }

    fn read_line(&mut self) -> Result<String, String> {
        let stream = self.stream.as_mut().ok_or("door not connected")?;
        let mut line = String::new();
        match stream.read_line(&mut line) {
            Ok(0) => Err("door closed by the compositor".into()),
            Ok(_) => Ok(line.trim_end().to_string()),
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                Err("door read timed out".into())
            }
            Err(e) => Err(format!("door read failed: {e}")),
        }
    }

    /// Absolute pointer motion in output coordinates.
    pub fn motion(&mut self, x: f64, y: f64) -> Result<(), String> {
        self.send(&format!("motion {x} {y}"))
    }

    /// Pointer button by name; `pressed` true for press.
    pub fn button(&mut self, button: &str, pressed: bool) -> Result<(), String> {
        self.send(&format!("button {button} {}", if pressed { "press" } else { "release" }))
    }

    /// Keyboard key by *evdev* keycode (`KEY_*` from
    /// input-event-codes.h — e.g. 125 LEFTMETA, 103 UP, 28 ENTER, 1
    /// ESC); the door applies the xkb +8 offset itself. `pressed` true
    /// for press.
    pub fn key(&mut self, code: u32, pressed: bool) -> Result<(), String> {
        self.send(&format!("key {code} {}", if pressed { "press" } else { "release" }))
    }

    /// A full tap: press, settle, release, settle — the two edges in
    /// different dispatch passes, the way a human's land.
    pub fn tap_key(&mut self, code: u32) -> Result<(), String> {
        self.key(code, true)?;
        self.barrier()?;
        self.key(code, false)?;
        self.barrier()
    }

    /// A modified tap — hold `modifier`, tap `code`, release — for
    /// injecting a keybinding chord like super+up.
    pub fn chord(&mut self, modifier: u32, code: u32) -> Result<(), String> {
        self.key(modifier, true)?;
        self.barrier()?;
        self.tap_key(code)?;
        self.key(modifier, false)?;
        self.barrier()
    }

    /// Waits until everything sent so far has been dispatched and a
    /// frame rendered — the door's `barrier`, and the only ordering
    /// guarantee any test relies on. The read timeout bounds it.
    pub fn barrier(&mut self) -> Result<(), String> {
        self.send("barrier")?;
        loop {
            let line = self.read_line()?;
            if line == "ok" {
                return Ok(());
            }
            // `err` replies to earlier malformed commands may be
            // queued ahead of the ack; surface them, don't skip them.
            if line.starts_with("err ") {
                return Err(format!("door reported: {line}"));
            }
        }
    }

    /// Move, press, settle, release, settle: a full click at (x, y),
    /// with a barrier between press and release so the two edges land
    /// in different dispatch passes the way a human's do.
    pub fn click(&mut self, x: f64, y: f64) -> Result<(), String> {
        self.motion(x, y)?;
        self.button("left", true)?;
        self.barrier()?;
        self.button("left", false)?;
        self.barrier()
    }

    /// Press at `from`, then travel to `to` in small settled steps —
    /// press and each motion get their own barrier, so a client-side
    /// drag gesture (GTK's headerbar move, its edge resize) sees the
    /// stream of positions it needs to cross its own drag threshold,
    /// exactly as a physical mouse would deliver them. The button is
    /// left DOWN when this returns: releasing (or not) is the half
    /// the caller is testing.
    pub fn drag_to(&mut self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
        self.motion(from.0, from.1)?;
        self.barrier()?;
        self.button("left", true)?;
        self.barrier()?;
        // Small steps first (the threshold crossing), then the cruise.
        const STEPS: u32 = 8;
        for step in 1..=STEPS {
            let t = step as f64 / STEPS as f64;
            // Ease-in: early steps are a few pixels, like a hand.
            let t = t * t;
            let x = from.0 + (to.0 - from.0) * t;
            let y = from.1 + (to.1 - from.1) * t;
            self.motion(x, y)?;
            self.barrier()?;
        }
        Ok(())
    }

    /// The compositor's current ledger, via the `windows` query.
    pub fn windows(&mut self) -> Result<World, String> {
        self.send("windows")?;
        let mut world = World::default();
        loop {
            let line = self.read_line()?;
            if line == "done" {
                return Ok(world);
            }
            if let Some(rest) = line.strip_prefix("scale ") {
                world.scale = rest.parse().unwrap_or(0.0);
            } else if let Some(rest) = line.strip_prefix("output ") {
                let mut parts = rest.split(' ');
                world.output_w = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                world.output_h = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            } else if line.starts_with("theme ") {
                world.theme = ThemeInfo {
                    id: quoted_field(&line, "id"),
                    name: quoted_field(&line, "name"),
                    appearance: field::<String>(&line, "appearance=").unwrap_or_default(),
                    following: quoted_field(&line, "following"),
                };
            } else if line.starts_with("window ") {
                if let Some(window) = parse_window_line(&line) {
                    world.windows.push(window);
                }
            } else if line.starts_with("frame ") {
                if let Some(frame) = parse_frame_line(&line) {
                    world.frames.push(frame);
                }
            } else if line.starts_with("shell ") {
                if let Some(shell) = parse_shell_line(&line) {
                    world.shells.push(shell);
                }
            } else if line.starts_with("err ") {
                return Err(format!("door reported: {line}"));
            }
        }
    }
}

/// Value of `key=` in a `key=value` word list, numerics only.
fn field<T: std::str::FromStr>(line: &str, key: &str) -> Option<T> {
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(key))
        .and_then(|value| value.parse().ok())
}

/// Value of a trailing quoted field like `app="..."`. The door quotes
/// with `{:?}`, so embedded quotes are escaped and the terminator to
/// look for is a bare `"` — good enough for the substring matching
/// tests do (no test names a window with an escaped quote).
fn quoted_field(line: &str, key: &str) -> String {
    line.split_once(&format!("{key}=\""))
        .and_then(|(_, rest)| rest.split('"').next())
        .unwrap_or("")
        .to_string()
}

fn parse_window_line(line: &str) -> Option<WindowInfo> {
    Some(WindowInfo {
        id: field(line, "id=")?,
        x: field(line, "x=")?,
        y: field(line, "y=")?,
        w: field(line, "w=")?,
        h: field(line, "h=")?,
        mapped: field(line, "mapped=")?,
        app: quoted_field(line, "app"),
        title: quoted_field(line, "title"),
    })
}

fn parse_shell_line(line: &str) -> Option<ShellInfo> {
    Some(ShellInfo {
        id: field(line, "id=")?,
        x: field(line, "x=")?,
        y: field(line, "y=")?,
        w: field(line, "w=")?,
        h: field(line, "h=")?,
        mapped: field(line, "mapped=")?,
        above: field(line, "above=")?,
    })
}

fn parse_frame_line(line: &str) -> Option<FrameInfo> {
    Some(FrameInfo {
        id: field(line, "id=")?,
        window: field(line, "window=")?,
        x: field(line, "x=")?,
        y: field(line, "y=")?,
        w: field(line, "w=")?,
        h: field(line, "h=")?,
        mapped: field(line, "mapped=")?,
    })
}

// -- screenshots ---------------------------------------------------------

/// A decoded capture of the nested output, with the pixel-poking
/// verbs the tests assert through.
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    /// RGBA, row-major.
    rgba: Vec<u8>,
    /// Where the PNG lives, for post-mortem viewing.
    pub path: PathBuf,
}

impl Screenshot {
    /// Decodes a PNG (RGB or RGBA, 8-bit — the two things grim
    /// writes) into RGBA.
    pub fn load(path: &Path) -> Result<Screenshot, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let decoder = png::Decoder::new(BufReader::new(file));
        let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
        let mut buffer = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
        buffer.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => buffer,
            png::ColorType::Rgb => buffer
                .chunks_exact(3)
                .flat_map(|px| [px[0], px[1], px[2], 255])
                .collect(),
            other => return Err(format!("unsupported screenshot color type {other:?}")),
        };
        if info.bit_depth != png::BitDepth::Eight {
            return Err(format!("unsupported screenshot bit depth {:?}", info.bit_depth));
        }
        Ok(Screenshot {
            width: info.width,
            height: info.height,
            rgba,
            path: path.to_path_buf(),
        })
    }

    /// The RGBA pixel at (x, y). Out of range is a test bug and
    /// panics with the coordinates, which beats a wrapped index
    /// silently sampling the wrong row.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        assert!(x < self.width && y < self.height, "pixel ({x}, {y}) outside {}x{} screenshot", self.width, self.height);
        let index = ((y * self.width + x) * 4) as usize;
        self.rgba[index..index + 4].try_into().unwrap()
    }

    /// Mean RGB over an axis-aligned region, clamped to the image.
    /// The region form of every "is the wallpaper there / is the dock
    /// there" assertion: single pixels are hostage to dithering and
    /// antialiasing, a 16x16 mean is not.
    pub fn mean_rgb(&self, x: u32, y: u32, w: u32, h: u32) -> [f64; 3] {
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        let mut sum = [0.0f64; 3];
        let mut count = 0.0f64;
        for py in y..y1 {
            for px in x..x1 {
                let p = self.pixel(px, py);
                sum[0] += p[0] as f64;
                sum[1] += p[1] as f64;
                sum[2] += p[2] as f64;
                count += 1.0;
            }
        }
        if count == 0.0 {
            return [0.0; 3];
        }
        [sum[0] / count, sum[1] / count, sum[2] / count]
    }

    /// Fraction of pixels (0.0–1.0) whose max per-channel difference
    /// from `other` exceeds `threshold`. The "did anything move"
    /// primitive: a window that followed a post-release drag shows up
    /// as a large fraction, compression noise does not.
    pub fn diff_fraction(&self, other: &Screenshot, threshold: u8) -> f64 {
        assert_eq!(
            (self.width, self.height),
            (other.width, other.height),
            "diffing screenshots of different sizes"
        );
        let mut differing = 0usize;
        let total = (self.width * self.height) as usize;
        for (a, b) in self.rgba.chunks_exact(4).zip(other.rgba.chunks_exact(4)) {
            let delta = a
                .iter()
                .zip(b.iter())
                .take(3)
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0);
            if delta > threshold {
                differing += 1;
            }
        }
        differing as f64 / total.max(1) as f64
    }
}

/// A region reads as "not black" when its mean brightness clears a
/// floor no wallpaper or chrome pixel sits under — the scale-2
/// regression's symptom was exactly corners at (0, 0, 0).
pub fn is_dark(mean: [f64; 3]) -> bool {
    mean.iter().sum::<f64>() / 3.0 < 12.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_lines_parse_including_quoted_tails() {
        let line = r#"window id=3 x=100 y=-8 w=400 h=300 mapped=true app="org.gnome.zenity" title="Question two words""#;
        let window = parse_window_line(line).unwrap();
        assert_eq!(window.id, 3);
        assert_eq!(window.x, 100);
        assert_eq!(window.y, -8);
        assert_eq!(window.w, 400);
        assert_eq!(window.h, 300);
        assert!(window.mapped);
        assert_eq!(window.app, "org.gnome.zenity");
        assert_eq!(window.title, "Question two words");
    }

    #[test]
    fn frame_lines_parse() {
        let line = "frame id=4 window=3 x=96 y=52 w=408 h=332 mapped=false";
        let frame = parse_frame_line(line).unwrap();
        assert_eq!(frame.window, 3);
        assert!(!frame.mapped);
    }

    #[test]
    fn a_mangled_line_is_rejected_not_misparsed() {
        assert!(parse_window_line("window id=oops x=1 y=1 w=1 h=1 mapped=true").is_none());
    }

    /// The exact bytes libwayland 1.26 writes under `FORCE_COLOR`,
    /// verbatim from a captured zenity log: the escapes sit *inside*
    /// the `object#id.event(` token, so a raw substring match for
    /// `wl_keyboard#` finds nothing while the wire plainly carried
    /// the enter. Stripping must restore the token exactly.
    #[test]
    fn ansi_stripping_reassembles_the_tokens_assertions_match_on() {
        let colored = "\u{1b}[32m[06:02:44.472322] \u{1b}[33m{Default Queue} \u{1b}[31m\u{1b}[0m\u{1b}[34mwl_keyboard\u{1b}[35m#15\u{1b}[36m.enter\u{1b}[0m(4, wl_surface#8, array[0])\u{1b}[0m";
        assert!(!colored.contains("wl_keyboard#"), "the raw bytes must not match, or this test pins nothing");
        let plain = strip_ansi(colored);
        assert_eq!(plain, "[06:02:44.472322] {Default Queue} wl_keyboard#15.enter(4, wl_surface#8, array[0])");
        assert!(plain.contains("wl_keyboard#") && plain.contains(".enter("));
        // Uncolored logs pass through untouched.
        assert_eq!(strip_ansi("wl_pointer#18.button(6, 11224, 272, 1)"), "wl_pointer#18.button(6, 11224, 272, 1)");
    }
}
