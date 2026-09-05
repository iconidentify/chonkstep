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
//! Wayland session to nest inside, so they are `#[ignore]`d for an
//! ordinary `cargo test`; CI creates an isolated headless Weston host.
//! Run them locally with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.
//!
//! # What every posed session has in common
//!
//! Each session runs from a fresh scratch directory ([`session_dir`])
//! with a config file the harness writes itself. That file always
//! begins `omarchy_shell = false` unless [`SessionOptions::omarchy_shell`]
//! is set: the compositor's own default is to host Omarchy's shell
//! when it finds one, and on a machine with Omarchy installed every
//! nested compositor this suite boots would otherwise start a real
//! Quickshell against itself — a bar, notifications and an OSD
//! fighting the test for the desk. The one test that wants the shell
//! hosted asks for it and supplies its own stand-in launcher.
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
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long [`Session::boot`] waits for the compositor to open its
/// wayland socket and its test door. GitHub's cold llvmpipe path has
/// taken 16 seconds in `eglInitialize` alone before falling back from
/// Zink, leaving the former 20-second bound only a few seconds for the
/// rest of boot and producing a false red build. This larger bound does
/// not slow a success (the poll returns immediately) or hide a crashed
/// compositor (`try_wait` fails fast); it only gives a genuinely slow
/// software renderer enough time to become observable.
const BOOT_TIMEOUT: Duration = Duration::from_secs(45);

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

fn error_with_log_tail(error: &str, log: &str) -> String {
    let mut lines: Vec<_> = log.lines().rev().take(80).collect();
    lines.reverse();
    let tail = if lines.is_empty() { "<empty>".to_string() } else { lines.join("\n") };
    format!("{error}\n--- compositor log tail ---\n{tail}")
}

/// Runs `condition` until it yields `Some`, at most until `timeout`
/// has elapsed. The only wait primitive in this crate — every use
/// names the observable condition it polls, which is the whole
/// anti-flake policy in one function signature.
pub fn poll_until<T>(timeout: Duration, what: &str, mut condition: impl FnMut() -> Option<T>) -> Result<T, String> {
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

/// evdev keycodes (`KEY_*` from input-event-codes.h), which is what
/// [`Door::key`] speaks — the door applies the xkb +8 offset itself.
/// One list, so no test restates a number it could get wrong.
pub mod keys {
    pub const ESC: u32 = 1;
    /// The number row, as evdev counts it: `KEY_1` is 2, so the digit
    /// `n` is `n + 1` and `KEY_0` is 11 at the far end. The workspace
    /// chords are the reason these are here.
    pub const ONE: u32 = 2;
    pub const TWO: u32 = 3;
    pub const THREE: u32 = 4;
    pub const ENTER: u32 = 28;
    pub const LEFTSHIFT: u32 = 42;
    pub const X: u32 = 45;
    pub const LEFTALT: u32 = 56;
    pub const SPACE: u32 = 57;
    /// The lock key and keypad digit used together to prove keypad
    /// bindings are independent of the active Num Lock level.
    pub const NUMLOCK: u32 = 69;
    pub const KP1: u32 = 79;
    pub const UP: u32 = 103;
    pub const LEFT: u32 = 105;
    pub const RIGHT: u32 = 106;
    /// The bare volume-up key — one of the media keys the config
    /// parser had no name for until the `[commands]` seam landed.
    pub const VOLUMEUP: u32 = 115;
    pub const LEFTMETA: u32 = 125;
}

/// The solid colour `chonk-fake-bar` (this crate's layer-shell
/// client) fills its surface with, as a screenshot reads it back:
/// RGB, a strong orange nothing in the shell's palettes comes near.
/// The bar derives its premultiplied little-endian ARGB pixel from
/// this, so the fixture and the assertions on it cannot drift apart.
pub const FAKE_BAR_RGB: [u8; 3] = [0xE0, 0x70, 0x10];

/// Clients this suite needs that CI genuinely cannot install, with the
/// reason — the allow-list [`require_client`] consults before turning a
/// missing client into a red build.
///
/// Being on this list is not permission to skip quietly. A skip is
/// still recorded and still printed by `scripts/e2e.sh`; the list only
/// says "we already know, and it is not a regression". Deleting an
/// entry the day the client becomes installable is the point: the
/// build goes red until someone adds it to the workflow.
const CI_CANNOT_INSTALL: &[(&str, &str)] = &[
    // Hyprland's night-light daemon. Not packaged for Ubuntu, and
    // building it on a runner would pull the whole Hyprland toolchain
    // for one gamma assertion. `gamma.rs`'s other tests drive the
    // protocol directly and do run.
    ("hyprsunset", "not packaged for Ubuntu; the rest of gamma.rs drives the protocol directly"),
];

/// Where [`require_client`] records a client it did not find, so a run
/// can say plainly which tests did not run. `scripts/e2e.sh` prints
/// this file next to its closing line.
pub fn skip_log_path() -> PathBuf {
    std::env::temp_dir().join("chonk-testkit").join("skipped.log")
}

/// Whether `program` is on `PATH`, and the only sanctioned way for a
/// test to decide it cannot run.
///
/// # Why this is not an `eprintln!` and a `return`
///
/// That was the idiom, in three tests, and it is invisible: `cargo
/// test` captures a *passing* test's output, so the skip line is never
/// printed and the test reports `ok`. On CI the
/// `wlr-output-management` conformance test was in that state
/// permanently — the only test in the tree that exercises that
/// protocol against a real client, reporting `1 passed` in 0.00s while
/// never booting a compositor. A skip that is indistinguishable from a
/// pass is the same silent-failure class this project labels
/// everywhere else, turned on its own suite.
///
/// So: under `CI`, a missing client is a **panic** naming it, unless it
/// is in [`CI_CANNOT_INSTALL`]. Off CI it returns `false` — a developer
/// without every client should still be able to run most of the suite —
/// but the skip is recorded to [`skip_log_path`] either way, and
/// `scripts/e2e.sh` prints the file, so a local run says which tests did
/// not run rather than implying they all did.
///
/// A `PATH` scan rather than running the program: the workspace bans
/// the blocking wait (`clippy::disallowed_methods`), and existence is
/// all that is being asked.
pub fn require_client(program: &str) -> bool {
    let found = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
    });
    if found {
        return true;
    }
    let known = CI_CANNOT_INSTALL.iter().find(|(name, _)| *name == program);
    let note = match known {
        Some((_, reason)) => format!("{program} is not installed ({reason})"),
        None => format!("{program} is not installed"),
    };
    let verdict = client_verdict(std::env::var_os("CI").is_some(), known.is_some());
    // Recorded before the panic, so even the red build leaves the same
    // artifact a green one would.
    let path = skip_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write as _;
        let _ = writeln!(file, "{note}");
    }
    if verdict == MissingClient::Fatal {
        panic!(
            "{note}. On CI a missing client is a failure, not a skip: install it in \
             .github/workflows/ci.yml's wayland job, or add it to \
             chonk_testkit::CI_CANNOT_INSTALL with the reason it cannot be installed."
        );
    }
    false
}

/// What a missing client costs, kept pure so the rule is testable
/// without mutating the process environment — `CI` is global, and a
/// test that set it would race every other test in the binary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MissingClient {
    /// Off CI, or a client CI cannot install: the test returns early
    /// and the skip is recorded and printed.
    Skip,
    /// On CI, and a client CI could have installed: a red build.
    Fatal,
}

/// The rule: a missing client is fatal on CI unless it is one CI
/// genuinely cannot install. Everything else is a recorded skip.
pub fn client_verdict(on_ci: bool, ci_cannot_install: bool) -> MissingClient {
    if on_ci && !ci_cannot_install {
        MissingClient::Fatal
    } else {
        MissingClient::Skip
    }
}

/// Where the session called `name` keeps its scratch — config, state,
/// logs, screenshots, the door socket. Public so a test can name a
/// path inside the directory (a marker file for a command to write)
/// before the session that will use it exists; [`Session::boot`]
/// clears and recreates exactly this directory.
pub fn session_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join("chonk-testkit").join(name)
}

// -- the session ---------------------------------------------------------

/// Options for [`Session::boot`]. `scale` lands in the isolated
/// config file's `scale =` line — the same file a user would edit —
/// so the compositor resolves it through the very
/// `SessionState::resolve` path the scale-2 regression shipped in.
#[derive(Default)]
pub struct SessionOptions {
    pub scale: Option<f32>,
    /// Whether the session hosts Omarchy's shell (`omarchy_shell`).
    /// Off by default, unlike the compositor's own default: on a
    /// machine with Omarchy installed, every nested compositor these
    /// tests boot would otherwise start a real Quickshell against
    /// itself.
    pub omarchy_shell: bool,
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
    /// Files seeded into the isolated `XDG_CONFIG_HOME` root itself
    /// (not under `chonkstep/`) before boot, as `(relative path,
    /// contents)` — the config-side twin of [`Self::state_root_files`],
    /// for configuration belonging to *another* program this session
    /// reads. The Hyprland-config e2e plants a whole `hypr/` tree here,
    /// exactly where an Omarchy user's own files live, so the session
    /// reads somebody else's configuration from the place it really
    /// comes from rather than from a path invented for the test.
    pub config_root_files: Vec<(String, String)>,
    /// Files seeded into the isolated `XDG_STATE_HOME` root itself
    /// (not under `chonkstep/`) before boot, as `(relative path,
    /// contents)` — for state that belongs to *another* program the
    /// session reads: the Omarchy e2e plants
    /// `omarchy/current/theme/colors.toml` and `omarchy/current/theme.name`
    /// here, exactly where `omarchy-theme-set` would put them. Bytes,
    /// not text, because a planted Omarchy background is a picture.
    pub state_root_files: Vec<(String, Vec<u8>)>,
    /// Symlinks made in the isolated `XDG_STATE_HOME` root before
    /// boot, as `(relative link path, relative target path)` — the
    /// target resolved against the root to an absolute path, as
    /// `ln -nsf` in `omarchy-theme-set` leaves `current/background`.
    pub state_root_links: Vec<(String, String)>,
}

/// One booted nested compositor plus everything needed to drive and
/// observe it. Killed (clients first, compositor second) on drop.
pub struct Session {
    /// Per-test scratch: config, state, logs, screenshots, the door
    /// socket. Left on disk after the test so a failure can be
    /// investigated from the artifacts.
    pub dir: PathBuf,
    compositor: Child,
    /// Launched clients with the program name each was started as,
    /// so a test can single one out to kill (`kill_client`).
    clients: Vec<(String, Child)>,
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
        let dir = session_dir(name);
        // A fresh directory per boot: leftovers from the previous run
        // (an old door socket, an old config) must not leak into this
        // one. Failure to remove is fine the first time around.
        let _ = std::fs::remove_dir_all(&dir);
        let config_home = dir.join("config");
        let state_home = dir.join("state");
        std::fs::create_dir_all(config_home.join("chonkstep")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(state_home.join("chonkstep")).map_err(|e| e.to_string())?;
        // Always present, even empty: the compositor reads Omarchy's
        // current theme from `$XDG_STATE_HOME/omarchy/current` only
        // when `$XDG_STATE_HOME/omarchy` already exists as a directory,
        // and otherwise from `$HOME/.local/state/omarchy/current` —
        // Omarchy's own hard-coded path. Without this directory a test
        // that plants no palette on purpose would be dressed in the
        // developer's real Omarchy theme, and pass or fail with it.
        std::fs::create_dir_all(state_home.join("omarchy")).map_err(|e| e.to_string())?;

        // TOML forbids a repeated key, so the harness writes this one
        // itself rather than leaving it to `config_extra`.
        let mut config = format!("omarchy_shell = {}\n", options.omarchy_shell);
        if let Some(scale) = options.scale {
            config.push_str(&format!("scale = {scale}\n"));
        }
        config.push_str(&options.config_extra);
        std::fs::write(config_home.join("chonkstep/config.toml"), config).map_err(|e| e.to_string())?;
        for (name, contents) in &options.state_files {
            std::fs::write(state_home.join("chonkstep").join(name), contents).map_err(|e| e.to_string())?;
        }
        for (name, contents) in &options.config_files {
            let path = config_home.join("chonkstep").join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, contents).map_err(|e| e.to_string())?;
        }
        for (name, contents) in &options.config_root_files {
            let path = config_home.join(name);
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
        for (link, target) in &options.state_root_links {
            let path = state_home.join(link);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::os::unix::fs::symlink(state_home.join(target), path).map_err(|e| e.to_string())?;
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
            // Keep ordinary runs readable, but let a failing live test
            // turn on one subsystem without patching the harness.  The
            // compositor is a child rather than the test process itself,
            // so inheriting RUST_LOG implicitly is too easy to do by
            // accident; this deliberately named knob is test-only.
            .env(
                "RUST_LOG",
                std::env::var("CHONKSTEP_TEST_RUST_LOG").unwrap_or_else(|_| "info".to_string()),
            )
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
        let announced_display = {
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
            })
        };
        session.wayland_display = match announced_display {
            Ok(Ok(display)) => display,
            Ok(Err(error)) | Err(error) => return Err(session.boot_error(error)),
        };
        let door = poll_until(BOOT_TIMEOUT, "the test door to accept a connection", || Door::connect(&door_path).ok())
            .map_err(|error| session.boot_error(error))?;
        session.door = door;
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
        if let Err(error) = session.door.barrier() {
            return Err(session.boot_error(format!("the compositor never answered its first barrier: {error}")));
        }
        Ok(session)
    }

    /// Adds the useful end of the compositor log to a boot failure. Boot is
    /// the one point at which callers do not yet own a `Session`, so without
    /// doing this here the persistent artifact exists but the CI failure says
    /// only "timed out" and gives no clue what the child was doing.
    fn boot_error(&self, error: String) -> String {
        error_with_log_tail(&error, &self.log())
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
        // A program given by path (this crate's own probe binaries)
        // logs under its file name: the path's slashes are not a
        // directory tree the log should be filed into.
        let short = Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or(program);
        let log_path = self.dir.join(format!("client-{}-{short}.log", self.clients.len()));
        let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
        let log_err = log.try_clone().map_err(|e| e.to_string())?;
        let signature = self.hyprland_signature();
        let mut command = Command::new(program);
        command
            .args(args)
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env_remove("DISPLAY")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .env("GDK_BACKEND", "wayland")
            // The client's log is machine-read (`WAYLAND_DEBUG`
            // assertions match substrings), and libwayland ≥1.26
            // honors these by coloring its debug stream even into a
            // file — escapes landing mid-token. Tests strip ANSI
            // anyway (`strip_ansi`), but a harness that ASKS for
            // plain logs fails one environment change later instead
            // of two.
            .env_remove("FORCE_COLOR")
            .env_remove("CLICOLOR_FORCE");
        if let Some(signature) = signature {
            // Clients spawned by the real shell inherit this value
            // from the compositor. Harness clients are siblings of
            // the compositor, so copy its announced value explicitly;
            // otherwise hyprsunset creates its control socket in the
            // wrong directory and an IPC integration test can only
            // prove the Wayland half of its behavior.
            command.env("HYPRLAND_INSTANCE_SIGNATURE", signature);
        }
        let child = command
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| format!("could not launch {program}: {e}"))?;
        self.clients.push((short.to_string(), child));
        Ok(())
    }

    /// The signature the compositor exported before entering its
    /// event loop. `None` only when the compatibility server was
    /// explicitly disabled or failed to bind.
    pub fn hyprland_signature(&self) -> Option<String> {
        let log = self.log();
        let line = log.lines().find(|line| line.contains("hyprland ipc listening"))?;
        line.split("signature=\"").nth(1)?.split('"').next().map(str::to_owned)
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

    /// Returns the exit status of the most recently launched instance of
    /// `program`, or `None` while it is still running. This is intentionally
    /// non-blocking: heavyweight-client startup tests can distinguish a slow
    /// live process from one that has already failed without weakening their
    /// bounded-poll contract.
    pub fn client_status(&mut self, program: &str) -> Result<Option<ExitStatus>, String> {
        let short = Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or(program);
        let (_, child) = self
            .clients
            .iter_mut()
            .rev()
            .find(|(name, _)| name == short)
            .ok_or_else(|| format!("no launched client named {short:?}"))?;
        child.try_wait().map_err(|error| format!("could not query {short:?}: {error}"))
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
        let status = poll_until(DEFAULT_TIMEOUT, "grim to finish", || grim.try_wait().ok().flatten());
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
        std::fs::write(self.dir.join("config/chonkstep/config.toml"), contents).map_err(|e| e.to_string())
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

    /// The compositor process id, for signal-lifecycle tests. Keeping
    /// the child private prevents tests from bypassing the harness's
    /// bounded reaping; a pid is enough to deliver the same signal a
    /// session manager does.
    pub fn compositor_pid(&self) -> u32 {
        self.compositor.id()
    }

    /// Wait for the compositor to exit without a blocking `wait(2)`.
    pub fn wait_for_compositor_exit(&mut self, timeout: Duration) -> Result<std::process::ExitStatus, String> {
        poll_until(timeout, "the compositor to exit", || self.compositor.try_wait().ok().flatten())
    }

    /// The compositor's captured log so far, with the colour escapes
    /// `tracing` writes already stripped ([`strip_ansi`]). Stripping
    /// here rather than at each matcher is what stops one test from
    /// forgetting: a `key=value` pair in the raw file has escapes
    /// between the key and the value, so a substring match against
    /// the raw bytes fails against a healthy compositor.
    pub fn log(&self) -> String {
        strip_ansi(&std::fs::read_to_string(&self.log_path).unwrap_or_default())
    }

    /// What a launched client has written to stdout/stderr so far,
    /// with colour escapes stripped for the same reason [`log`] strips
    /// them.
    ///
    /// The probes in `src/bin/` are scripted clients that *report* —
    /// `chonk-gamma-probe` prints the gamma size it was told and
    /// whether its second claim was refused — so for those the client's
    /// own log is the observation, not merely the diagnosis of a
    /// failure. `program` is the name it was launched under (a path's
    /// file name), and clients are matched newest first, so relaunching
    /// the same probe reads the run that just happened.
    ///
    /// [`log`]: Session::log
    pub fn client_log(&self, program: &str) -> String {
        let short = Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or(program);
        let suffix = format!("-{short}.log");
        // Found on disk by name rather than by this client's position
        // in `clients`: `kill_client` removes entries, so a position
        // there stops matching the `client-N-` the file was named with
        // the moment a test kills anything.
        let mut newest: Option<(usize, PathBuf)> = None;
        for entry in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(rest) = name.strip_prefix("client-") else {
                continue;
            };
            let Some(number) = rest.strip_suffix(&suffix) else {
                continue;
            };
            let Ok(number) = number.parse::<usize>() else {
                continue;
            };
            if newest.as_ref().is_none_or(|(seen, _)| number >= *seen) {
                newest = Some((number, path));
            }
        }
        let Some((_, path)) = newest else {
            return String::new();
        };
        strip_ansi(&std::fs::read_to_string(path).unwrap_or_default())
    }

    /// Polls the ledger until the Dock column sits with its right edge
    /// `right_inset` pixels in from the output's and its top at `top`
    /// — where a layer-shell reservation puts it. A poll and not a
    /// barrier because the shell moves the Dock in the dispatch pass
    /// after another client's surface maps, and the door's barrier
    /// orders nothing against a separate client.
    pub fn wait_for_dock_at(&mut self, right_inset: u32, top: i32) -> Result<ShellInfo, String> {
        let door = &mut self.door;
        poll_until(DEFAULT_TIMEOUT, &format!("the dock to hang {right_inset} in from the right at y={top}"), || {
            let world = door.windows().ok()?;
            world.dock_inset(right_inset).filter(|dock| dock.y == top).cloned()
        })
    }

    /// Right-clicks bare desk and returns the root menu once a menu
    /// surface with exactly `rows` rows has mapped — the row count is
    /// the check that the menu that opened is the one expected, since
    /// the ledger does not carry labels.
    ///
    /// The click lands 30% across and halfway down the output: inside
    /// the desk, clear of the Dock column and the launcher strip on
    /// the right edge, and far enough left that two cascades can open
    /// to the right of the menu without either being pushed flush
    /// against the edge, where [`World::menus`] would stop seeing it.
    pub fn open_root_menu(&mut self, metrics: &MenuMetrics, rows: usize) -> Result<ShellInfo, String> {
        let world = self.door.windows()?;
        let (x, y) = (world.output_w as f64 * 0.3, world.output_h as f64 / 2.0);
        self.door.right_click(x, y)?;
        let wanted = metrics.height_for(rows);
        let door = &mut self.door;
        poll_until(DEFAULT_TIMEOUT, &format!("the root menu ({rows} rows, {wanted}px tall) to map"), || {
            let world = door.windows().ok()?;
            world.menus().into_iter().find(|m| m.h == wanted)
        })
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
        for (_, client) in &mut self.clients {
            let _ = client.kill();
        }
        for (_, client) in &mut self.clients {
            let _ =
                poll_until(Duration::from_secs(2), "a killed client to be reaped", || client.try_wait().ok().flatten());
        }
        self.clients.clear();
    }

    /// Kills every launched client started as `program` (its file
    /// name, as `launch` was given it) and reaps them — the test-side
    /// stand-in for one program exiting while the rest of the desktop
    /// carries on, for asserting what the compositor gives back when
    /// it does (a bar's exclusive zone, say). A no-op for a name never
    /// launched.
    pub fn kill_client(&mut self, program: &str) {
        let short = Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or(program);
        let (mut doomed, kept): (Vec<_>, Vec<_>) = self.clients.drain(..).partition(|(name, _)| name == short);
        self.clients = kept;
        for (_, client) in &mut doomed {
            let _ = client.kill();
        }
        for (_, client) in &mut doomed {
            let _ = poll_until(Duration::from_secs(2), "the killed client to be reaped", || {
                client.try_wait().ok().flatten()
            });
        }
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
        for (_, client) in &mut self.clients {
            let _ = client.kill();
        }
        let _ = self.compositor.kill();
        for (_, client) in &mut self.clients {
            let _ = poll_until(Duration::from_secs(2), "a client to be reaped", || client.try_wait().ok().flatten());
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
    let profile_dir = exe.parent().and_then(Path::parent).ok_or("cannot locate the target profile directory")?;
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
    /// Buffer pixels to the left of the declared window geometry. A
    /// client-decorated window uses this transparent band for shadows
    /// and resize grips.
    pub offset_x: i32,
    /// Buffer pixels above the declared window geometry.
    pub offset_y: i32,
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
    /// CPU-side bytes retained by the compositor for this surface's
    /// current pixels. Zero after a hidden transient releases its backing.
    pub buffer_bytes: usize,
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
        self.windows.iter().find(|w| w.mapped && (w.app.contains(needle) || w.title.contains(needle)))
    }

    /// The frame decorating `window`, if the server drew one.
    pub fn frame_of(&self, window: u64) -> Option<&FrameInfo> {
        self.frames.iter().find(|f| f.window == window)
    }

    /// The dock: the mapped shell surface that is a column (taller
    /// than wide) flush against the output's right edge — the shape
    /// the shell gives it at every scale.
    pub fn dock(&self) -> Option<&ShellInfo> {
        self.dock_inset(0)
    }

    /// The dock found by shape alone, with its right edge
    /// `right_inset` pixels in from the output's: zero for the
    /// unobstructed corner, a right-edge panel's width once one has
    /// reserved the edge and the column has stepped left.
    pub fn dock_inset(&self, right_inset: u32) -> Option<&ShellInfo> {
        self.shells.iter().find(|s| s.mapped && s.h > s.w && s.x + s.w as i32 == (self.output_w - right_inset) as i32)
    }

    /// Mapped menu surfaces: every mapped, raised shell that does not
    /// sit flush against the output's right edge. The Dock column and
    /// the launcher strip under it do, and they are the only other
    /// shells the desktop keeps raised; a menu opened on bare desk
    /// never does.
    pub fn menus(&self) -> Vec<ShellInfo> {
        self.shells
            .iter()
            .filter(|s| s.mapped && s.above && s.x + (s.w as i32) < self.output_w as i32)
            .cloned()
            .collect()
    }
}

// -- menus ---------------------------------------------------------------

/// Every row the root menu can carry, in the order
/// `chonk_shell::desktop::root_menu_items` builds them. Two are
/// conditional: "Omarchy Bar" is present only while the session hosts
/// Omarchy's shell, and "Omarchy" only when an Omarchy menu definition
/// is installed and `omarchy_menu` is on. [`RootMenu`] says which of
/// the two a given session has, so a test clicks a row by its label
/// and never by a hand-counted index.
///
/// "Dock" is unconditional — the Dock is chonkstep's own furniture, so
/// every session has one to show or hide — which is why it is not one
/// of [`RootMenu`]'s fields.
pub const ROOT_MENU_ROWS: &[&str] =
    &["Terminal", "Applications", "Theme", "Wallpaper", "Dock", "Omarchy Bar", "Omarchy", "Exit"];

/// Which of the root menu's optional rows a session carries. The
/// default — neither — is the menu the harness's own default session
/// shows on a machine with no Omarchy menu definition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RootMenu {
    /// The session hosts Omarchy's shell, so the bar toggle is listed.
    pub omarchy_bar: bool,
    /// An Omarchy menu definition was found, so its submenu is listed.
    pub omarchy: bool,
}

impl RootMenu {
    /// The rows this menu shows, in order.
    pub fn rows(self) -> Vec<&'static str> {
        ROOT_MENU_ROWS
            .iter()
            .copied()
            .filter(|row| match *row {
                "Omarchy Bar" => self.omarchy_bar,
                "Omarchy" => self.omarchy,
                _ => true,
            })
            .collect()
    }

    /// How many rows this menu shows — what [`Session::open_root_menu`]
    /// is asked to wait for.
    pub fn row_count(self) -> usize {
        self.rows().len()
    }

    /// The index of the row labelled `label`, if this menu carries it.
    pub fn row_of(self, label: &str) -> Option<usize> {
        self.rows().iter().position(|row| *row == label)
    }
}

/// The menu's row geometry restated from `wm_theme::menu::render_menu`
/// at scale 1: the title strip is the titlebar's height (never shorter
/// than a row), rows are `menu.item_height` tall, and a `border.width`
/// outline wraps everything. Aiming with the same numbers the shell
/// hit-tests through is what keeps a test from encoding a magic
/// coordinate that breaks the day a pad changes.
pub struct MenuMetrics {
    pub border: u32,
    pub title_h: u32,
    pub item_h: u32,
}

impl MenuMetrics {
    /// The geometry of the theme the compositor wears by default, at
    /// scale 1 — the only scale the menu tests boot at.
    pub fn at_scale_1() -> Self {
        let theme = wm_theme::default_theme::nextstep_classic();
        let item_h = (theme.menu.item_height as u32).max(4);
        Self { border: (theme.border.width as u32).max(1), title_h: (theme.titlebar.height as u32).max(item_h), item_h }
    }

    /// The expected surface height of a menu with `rows` rows.
    pub fn height_for(&self, rows: usize) -> u32 {
        self.title_h + self.item_h * rows as u32 + self.border * 2
    }

    /// How many rows a menu surface of height `h` carries, or `None`
    /// for a height no whole number of rows explains.
    pub fn rows_in(&self, h: u32) -> Option<usize> {
        let body = h.checked_sub(self.title_h + self.border * 2)?;
        (body % self.item_h == 0).then_some((body / self.item_h) as usize)
    }

    /// The centre of row `row` of the menu surface `menu`, in the
    /// output coordinates the door's pointer speaks.
    pub fn row_center(&self, menu: &ShellInfo, row: usize) -> (f64, f64) {
        let y = menu.y as u32 + self.border + self.title_h + self.item_h * row as u32 + self.item_h / 2;
        (menu.x as f64 + menu.w as f64 / 2.0, y as f64)
    }
}

/// Client for the compositor's test door (`CHONKSTEP_TEST_SOCKET`).
/// The wire protocol is documented in `wm-wayland/src/test_door.rs`;
/// this is its only speaker.
pub struct Door {
    stream: Option<BufReader<UnixStream>>,
}

/// Work performed by demand-driven desktop protocol publishers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolPublishes {
    pub control: u64,
    pub hyprland: u64,
    pub foreign_full: u64,
    pub foreign_drag: u64,
}

/// Entries retained by compositor protocol ledgers that participate in
/// rendering, hit-testing, or idle-policy walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolLedgers {
    pub ime: usize,
    pub idle: usize,
    pub lock: usize,
}

/// Hyprland IPC descriptors owned by the server and corresponding
/// calloop sources currently registered by the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HyprlandSources {
    pub desired: usize,
    pub registered: usize,
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
        stream.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
        Ok(Door { stream: Some(BufReader::new(stream)) })
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        let stream = self.stream.as_mut().ok_or("door not connected")?;
        stream.get_mut().write_all(format!("{line}\n").as_bytes()).map_err(|e| format!("door write failed: {e}"))
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

    /// The active compositor-owned binding's emitted-repeat count and
    /// interval. `None` means no repeating binding is currently held.
    /// This is deliberately a test-door observation rather than an
    /// inference from external command effects: process scheduling is
    /// unrelated to the compositor's key-repeat deadline.
    pub fn repeating_binding(&mut self) -> Result<Option<(u64, Duration)>, String> {
        self.send("repeat")?;
        let line = self.read_line()?;
        if line == "repeat none" {
            return Ok(None);
        }
        if !line.starts_with("repeat ") {
            return Err(format!("unexpected repeat reply: {line}"));
        }
        let emitted = field(&line, "emitted=").ok_or_else(|| format!("repeat reply has no count: {line}"))?;
        let interval_us: u64 =
            field(&line, "interval_us=").ok_or_else(|| format!("repeat reply has no interval: {line}"))?;
        Ok(Some((emitted, Duration::from_micros(interval_us))))
    }

    /// Number of unconsumed xdg-activation tokens the compositor is
    /// retaining. Exposed only by the opt-in test door so the wire-level
    /// abuse test can measure the server's state directly.
    pub fn activation_tokens(&mut self) -> Result<usize, String> {
        self.send("activation-tokens")?;
        let line = self.read_line()?;
        line.strip_prefix("activation-tokens ")
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| format!("unexpected activation-token reply: {line}"))
    }

    /// Live objects retained in the three bounded lifecycle ledgers.
    /// Idle is an object count (not merely distinct surfaces), so a test
    /// can observe that destroying one duplicate decremented exactly one.
    pub fn protocol_ledgers(&mut self) -> Result<ProtocolLedgers, String> {
        self.send("protocol-ledgers")?;
        let line = self.read_line()?;
        if !line.starts_with("protocol-ledgers ") {
            return Err(format!("unexpected protocol-ledgers reply: {line}"));
        }
        Ok(ProtocolLedgers {
            ime: field(&line, "ime=").ok_or_else(|| format!("protocol-ledgers reply has no IME count: {line}"))?,
            idle: field(&line, "idle=")
                .ok_or_else(|| format!("protocol-ledgers reply has no idle count: {line}"))?,
            lock: field(&line, "lock=")
                .ok_or_else(|| format!("protocol-ledgers reply has no lock count: {line}"))?,
        })
    }

    /// Number of protocol snapshots/synchronizations attempted so far.
    /// Unlike counting output events, this detects an expensive full diff
    /// that rebuilt state only to discover that nothing changed.
    pub fn protocol_publishes(&mut self) -> Result<ProtocolPublishes, String> {
        self.send("protocol-publishes")?;
        let line = self.read_line()?;
        if !line.starts_with("protocol-publishes ") {
            return Err(format!("unexpected protocol-publishes reply: {line}"));
        }
        Ok(ProtocolPublishes {
            control: field(&line, "control=")
                .ok_or_else(|| format!("protocol-publishes reply has no control count: {line}"))?,
            hyprland: field(&line, "hyprland=")
                .ok_or_else(|| format!("protocol-publishes reply has no Hyprland count: {line}"))?,
            foreign_full: field(&line, "foreign_full=")
                .ok_or_else(|| format!("protocol-publishes reply has no full-sync count: {line}"))?,
            foreign_drag: field(&line, "foreign_drag=")
                .ok_or_else(|| format!("protocol-publishes reply has no drag-sync count: {line}"))?,
        })
    }

    /// Exact Hyprland IPC source population from inside the
    /// compositor. Unlike `/proc/<pid>/fd`, this excludes independent
    /// render, D-Bus, XWayland, and child-process descriptor churn.
    pub fn hyprland_sources(&mut self) -> Result<HyprlandSources, String> {
        self.send("hyprland-sources")?;
        let line = self.read_line()?;
        if !line.starts_with("hyprland-sources ") {
            return Err(format!("unexpected Hyprland-source reply: {line}"));
        }
        Ok(HyprlandSources {
            desired: field(&line, "desired=")
                .ok_or_else(|| format!("Hyprland-source reply has no desired count: {line}"))?,
            registered: field(&line, "registered=")
                .ok_or_else(|| format!("Hyprland-source reply has no registered count: {line}"))?,
        })
    }

    /// The production scene hit-test's coarse target class at one
    /// output-global point (`root`, `shell`, `frame`, `content`,
    /// `layer`, `ime`, or `lock`). This observes input policy directly
    /// instead of inferring it from an application's side effects.
    pub fn hit(&mut self, x: i32, y: i32) -> Result<String, String> {
        self.send(&format!("hit {x} {y}"))?;
        let line = self.read_line()?;
        line.strip_prefix("hit ")
            .map(str::to_string)
            .ok_or_else(|| format!("unexpected hit reply: {line}"))
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

    /// The right-button click that opens the root menu on bare desk,
    /// settled at every edge like [`Door::click`]: the motion gets its
    /// own barrier because the shell decides what was clicked from
    /// where the pointer already is when the press arrives.
    pub fn right_click(&mut self, x: f64, y: f64) -> Result<(), String> {
        self.motion(x, y)?;
        self.barrier()?;
        self.button("right", true)?;
        self.barrier()?;
        self.button("right", false)?;
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
    line.split_whitespace().find_map(|word| word.strip_prefix(key)).and_then(|value| value.parse().ok())
}

/// Value of a trailing quoted field like `app="..."`. The door quotes
/// with `{:?}`, so embedded quotes are escaped and the terminator to
/// look for is a bare `"` — good enough for the substring matching
/// tests do (no test names a window with an escaped quote).
fn quoted_field(line: &str, key: &str) -> String {
    line.split_once(&format!("{key}=\"")).and_then(|(_, rest)| rest.split('"').next()).unwrap_or("").to_string()
}

fn parse_window_line(line: &str) -> Option<WindowInfo> {
    Some(WindowInfo {
        id: field(line, "id=")?,
        x: field(line, "x=")?,
        y: field(line, "y=")?,
        w: field(line, "w=")?,
        h: field(line, "h=")?,
        // Optional so a newer harness can still inspect a compositor
        // binary built before the test-door diagnostic was extended.
        offset_x: field(line, "offset_x=").unwrap_or(0),
        offset_y: field(line, "offset_y=").unwrap_or(0),
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
        buffer_bytes: field(line, "buffer_bytes=").unwrap_or(0),
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
            png::ColorType::Rgb => buffer.as_chunks::<3>().0.iter().flat_map(|px| [px[0], px[1], px[2], 255]).collect(),
            other => return Err(format!("unsupported screenshot color type {other:?}")),
        };
        if info.bit_depth != png::BitDepth::Eight {
            return Err(format!("unsupported screenshot bit depth {:?}", info.bit_depth));
        }
        Ok(Screenshot { width: info.width, height: info.height, rgba, path: path.to_path_buf() })
    }

    /// The RGBA pixel at (x, y). Out of range is a test bug and
    /// panics with the coordinates, which beats a wrapped index
    /// silently sampling the wrong row.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        assert!(
            x < self.width && y < self.height,
            "pixel ({x}, {y}) outside {}x{} screenshot",
            self.width,
            self.height
        );
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

    /// The colour of the desk at the output's centre, as a 40x40
    /// mean: on an empty desktop no chrome and no window covers that
    /// spot, so it reads the wallpaper — or whatever a layer surface
    /// has painted over it.
    pub fn centre_rgb(&self) -> [f64; 3] {
        self.mean_rgb(self.width / 2 - 20, self.height / 2 - 20, 40, 40)
    }

    /// Fraction of pixels (0.0–1.0) whose max per-channel difference
    /// from `other` exceeds `threshold`. The "did anything move"
    /// primitive: a window that followed a post-release drag shows up
    /// as a large fraction, compression noise does not.
    pub fn diff_fraction(&self, other: &Screenshot, threshold: u8) -> f64 {
        assert_eq!((self.width, self.height), (other.width, other.height), "diffing screenshots of different sizes");
        let mut differing = 0usize;
        let total = (self.width * self.height) as usize;
        for (a, b) in self.rgba.as_chunks::<4>().0.iter().zip(other.rgba.as_chunks::<4>().0) {
            let delta = a.iter().zip(b.iter()).take(3).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
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

/// Whether a sampled mean colour is the one expected, within the
/// slack a mean over real renderer output needs: every channel inside
/// 12 of the target. Tight enough to tell any two fixture colours
/// apart, loose enough for the rounding a scaled blit introduces.
pub fn near(actual: [f64; 3], expected: [u8; 3]) -> bool {
    actual.iter().zip(expected).all(|(a, e)| (a - f64::from(e)).abs() < 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_errors_include_only_the_useful_end_of_the_compositor_log() {
        let log = (0..100).map(|n| format!("line-{n:03}")).collect::<Vec<_>>().join("\n");
        let report = error_with_log_tail("boot timed out", &log);
        assert!(report.starts_with("boot timed out\n--- compositor log tail ---\nline-020\n"));
        assert!(report.ends_with("line-099"));
        assert!(!report.lines().any(|line| line == "line-019"));
        assert!(error_with_log_tail("boot failed", "").ends_with("<empty>"));
    }

    #[test]
    fn window_lines_parse_including_offsets_and_quoted_tails() {
        let line = r#"window id=3 x=100 y=-8 w=400 h=300 offset_x=12 offset_y=9 mapped=true app="org.gnome.zenity" title="Question two words""#;
        let window = parse_window_line(line).unwrap();
        assert_eq!(window.id, 3);
        assert_eq!(window.x, 100);
        assert_eq!(window.y, -8);
        assert_eq!(window.w, 400);
        assert_eq!(window.h, 300);
        assert_eq!(window.offset_x, 12);
        assert_eq!(window.offset_y, 9);
        assert!(window.mapped);
        assert_eq!(window.app, "org.gnome.zenity");
        assert_eq!(window.title, "Question two words");

        let old = parse_window_line(r#"window id=4 x=0 y=0 w=1 h=1 mapped=true app="old" title="door""#).unwrap();
        assert_eq!((old.offset_x, old.offset_y), (0, 0), "new harnesses still read the old door shape");
    }

    #[test]
    fn frame_lines_parse() {
        let line = "frame id=4 window=3 x=96 y=52 w=408 h=332 mapped=false";
        let frame = parse_frame_line(line).unwrap();
        assert_eq!(frame.window, 3);
        assert!(!frame.mapped);
    }

    /// The optional rows drop out without disturbing the order of the
    /// rest, and a label is found at its index in the menu that has
    /// it and nowhere in one that does not. `Dock` is in every one of
    /// them: the Dock is chonkstep's own furniture, so there is no
    /// session with no Dock to offer.
    #[test]
    fn root_menu_rows_keep_their_order_with_and_without_the_optional_ones() {
        let plain = RootMenu::default();
        assert_eq!(plain.rows(), ["Terminal", "Applications", "Theme", "Wallpaper", "Dock", "Exit"]);
        assert_eq!(plain.row_of("Dock"), Some(4));
        assert_eq!(plain.row_of("Exit"), Some(5));
        assert_eq!(plain.row_of("Omarchy Bar"), None);
        let hosted = RootMenu { omarchy_bar: true, omarchy: false };
        assert_eq!(hosted.row_count(), 7);
        assert_eq!(hosted.row_of("Dock"), Some(4), "this desk's own column, then the guest's bar");
        assert_eq!(hosted.row_of("Omarchy Bar"), Some(5));
        let full = RootMenu { omarchy_bar: true, omarchy: true };
        assert_eq!(full.rows(), ROOT_MENU_ROWS);
        assert_eq!(full.row_of("Omarchy"), Some(6));
        assert_eq!(full.row_of("Exit"), Some(7));
    }

    #[test]
    fn a_mangled_line_is_rejected_not_misparsed() {
        assert!(parse_window_line("window id=oops x=1 y=1 w=1 h=1 mapped=true").is_none());
    }

    #[test]
    fn a_missing_client_is_fatal_on_ci_and_a_skip_off_it() {
        // The whole point of the helper. Off CI a developer without
        // every client still gets most of the suite; on CI a missing
        // client is a red build, because a skip there is
        // indistinguishable from a pass and that is how the only
        // wlr-output-management test spent its life reporting `ok` in
        // 0.00s without booting anything.
        assert_eq!(client_verdict(true, false), MissingClient::Fatal, "CI, installable");
        assert_eq!(client_verdict(false, false), MissingClient::Skip, "developer machine");
        // The escape hatch, and it only opens on CI's side of the
        // decision: a client CI genuinely cannot install is still a
        // recorded, printed skip rather than a failure.
        assert_eq!(client_verdict(true, true), MissingClient::Skip, "CI, not installable");
        assert_eq!(client_verdict(false, true), MissingClient::Skip, "developer machine, not installable");
    }

    #[test]
    fn every_declared_unavailable_client_carries_a_reason() {
        // `CI_CANNOT_INSTALL` is an escape hatch, and an escape hatch
        // with no argument written next to it is how a temporary
        // exemption becomes permanent. An entry must say why.
        for (client, reason) in CI_CANNOT_INSTALL {
            assert!(!client.is_empty(), "a nameless entry");
            assert!(
                reason.len() > 20,
                "{client} is exempted from the CI check with no real reason given: {reason:?}"
            );
        }
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
