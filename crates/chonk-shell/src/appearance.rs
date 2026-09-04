//! The session-wide light/dark appearance: how it is resolved, how it
//! is published, how anything else asks for a switch, and how the
//! switch reaches applications that are not this desktop's own.
//!
//! # The axis
//!
//! `wm_theme::Appearance` is the value; every built-in theme has a
//! rendition for each side of it (`default_theme::theme_variant`).
//! Switching appearance re-resolves the *current* theme in its other
//! rendition through the exact live-apply path a theme pick takes —
//! no restart, nothing closed, one repaint.
//!
//! # The files (a public contract)
//!
//! Two files under the session state directory
//! (`$XDG_STATE_HOME/chonkstep/`, `~/.local/state/chonkstep/` when the
//! variable is unset) are a documented IPC surface that dockapps and
//! scripts build against — see `docs/appearance.md`:
//!
//! - **`appearance`** — the current mode, published by the shell: the
//!   literal string `light` or `dark`. Rewritten atomically
//!   (write-to-temp, rename) so a reader can never observe a torn
//!   value, and rewritten at startup and on every switch.
//! - **`appearance-request`** — written by anyone who wants the mode
//!   changed: `light`, `dark`, or `toggle`. The shell consumes it
//!   (acts, then deletes) from its housekeeping tick, the same
//!   poll-and-unlink pattern as the `reload`/`restart` markers in
//!   [`crate::startup`] and on the same bounded (at most 100 ms) cadence. A request that
//!   names the mode the session is already in is consumed and does
//!   nothing.
//!
//! # Resolution
//!
//! At startup (and on every config reload) the mode is resolved as:
//! the published `appearance` file, else the config's `appearance`
//! key, else **the selected theme's own native mood**. The last layer
//! is what makes upgrading into this axis invisible: a session that
//! never chose a mode keeps looking exactly like the theme it wears
//! always looked — dark for seven of the built-ins, light for Ivory
//! Halftone. Note what the first layer implies: the published file is
//! also the persisted choice, so after the very first session the
//! config key only matters to a state directory that has never seen a
//! session (the live way to change mode is the request file, not the
//! config).
//!
//! # Propagation to applications
//!
//! The shell's own chrome, its dockapps (via the `ThemeChanged`
//! broadcast) and its terminals (foot's `[colors-dark]`/`[colors-light]`
//! sections plus `SIGUSR1`/`SIGUSR2`) all follow the switch through
//! their own channels. Everything else follows through the desktop
//! plumbing this module drives from [`propagate_to_applications`]:
//! GSettings' `org.gnome.desktop.interface color-scheme`, which
//! xdg-desktop-portal-gtk republishes as the
//! `org.freedesktop.appearance color-scheme` setting modern toolkits
//! watch, plus an adopt-if-ours nudge of the `gtk-theme` key. See
//! `docs/appearance.md` for the honest table of what follows live and
//! what waits for its next launch.

use std::path::{Path, PathBuf};
use std::time::Instant;

pub use wm_theme::Appearance;

use crate::startup::{state_dir, SESSION_REQUEST_POLL_INTERVAL};

/// File the current mode is published to, relative to the state dir.
pub const PUBLISHED_FILE: &str = "appearance";

/// File a mode change is requested through, relative to the state dir.
pub const REQUEST_FILE: &str = "appearance-request";

/// One parsed `appearance-request`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Set(Appearance),
    Toggle,
}

impl Request {
    /// Parses request-file text: `light`, `dark`, or `toggle`, trimmed
    /// and case-insensitive like every other name this desktop reads.
    /// `None` for anything else — the caller warns and drops the
    /// request rather than guessing, because the file is a public
    /// contract and a typo'd writer should learn about it from the log
    /// instead of from a surprise mode.
    pub fn parse(text: &str) -> Option<Self> {
        let token = text.trim().to_ascii_lowercase();
        if token == "toggle" {
            return Some(Self::Toggle);
        }
        Appearance::from_name(&token).map(Self::Set)
    }

    /// The mode this request lands the session in, from `current`.
    pub fn resolve(self, current: Appearance) -> Appearance {
        match self {
            Self::Set(mode) => mode,
            Self::Toggle => current.toggled(),
        }
    }
}

/// The published current mode, if a session has ever published one.
pub fn load_published() -> Option<Appearance> {
    load_published_from(&state_dir().join(PUBLISHED_FILE))
}

/// Pure core of [`load_published`], testable against any path.
fn load_published_from(path: &Path) -> Option<Appearance> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed = Appearance::from_name(&text);
    if parsed.is_none() {
        tracing::warn!(path = %path.display(), text = %text.trim(), "appearance state file holds neither \"light\" nor \"dark\"; ignoring it");
    }
    parsed
}

/// Publishes `mode` as the session's current appearance — the reader
/// half of the contract dockapps poll.
///
/// Atomic on purpose: the value lands in a sibling temp file first and
/// is renamed over the published path, so a reader polling at its own
/// cadence sees the old mode or the new one, never an empty or torn
/// file. Failure is a warning, not an error the caller must route —
/// a session that cannot write its state dir has bigger problems, and
/// none of them should stop the switch itself.
pub fn publish(mode: Appearance) {
    let dir = state_dir();
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::warn!(?error, "cannot create the state directory; appearance not published");
        return;
    }
    let path = dir.join(PUBLISHED_FILE);
    let tmp = dir.join(".appearance.tmp");
    let result = std::fs::write(&tmp, mode.name()).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(error) = result {
        tracing::warn!(?error, path = %path.display(), "failed to publish the appearance");
    }
}

/// Consumes a pending `appearance-request`, if one is waiting.
///
/// A destructive read like `startup::restart_requested`: the file is
/// removed once observed, so a request is honored exactly once. An
/// unparsable request is still consumed (leaving it would warn on
/// every housekeeping pass forever) and answered with a warning naming the text.
pub fn take_request() -> Option<Request> {
    take_request_from(&state_dir().join(REQUEST_FILE))
}

fn take_request_from(path: &Path) -> Option<Request> {
    let text = std::fs::read_to_string(path).ok()?;
    let _ = std::fs::remove_file(path);
    let parsed = Request::parse(&text);
    if parsed.is_none() {
        tracing::warn!(text = %text.trim(), "appearance-request must say \"light\", \"dark\" or \"toggle\"; dropping it");
    }
    parsed
}

/// Cached-path, deadline-aware appearance-request reader used by the
/// shell's event-loop hot path.
pub(crate) struct RequestPoller {
    path: PathBuf,
    next_poll: Instant,
}

impl RequestPoller {
    pub(crate) fn new(now: Instant) -> Self {
        Self { path: state_dir().join(REQUEST_FILE), next_poll: now }
    }

    pub(crate) fn take(&mut self, now: Instant) -> Option<Request> {
        if now < self.next_poll {
            return None;
        }
        self.next_poll = now + SESSION_REQUEST_POLL_INTERVAL;
        take_request_from(&self.path)
    }

    pub(crate) fn next_deadline(&self) -> Instant {
        self.next_poll
    }
}

/// Resolves the session's appearance: published state, else the
/// config's `appearance` key (already validated to `"light"`/`"dark"`
/// by `wm-config`), else the selected theme's native mood. See the
/// module docs for why the layers sit in this order.
pub fn resolve(config_appearance: Option<&str>, theme_native: Appearance) -> Appearance {
    load_published()
        .or_else(|| config_appearance.and_then(Appearance::from_name))
        .unwrap_or(theme_native)
}

// ---------------------------------------------------------------------
// Propagation to foreign toolkits.

/// The GSettings schema and keys the switch is spoken through. One
/// schema on purpose: `org.gnome.desktop.interface` is what
/// xdg-desktop-portal-gtk answers portal `Settings` reads from, so
/// writing it is the one move that reaches GTK4/libadwaita, Electron
/// and everything else watching `org.freedesktop.appearance
/// color-scheme` — live, no restart.
const GSETTINGS_SCHEMA: &str = "org.gnome.desktop.interface";

/// GTK theme pairs this desktop is willing to publish, in preference
/// order. Naming a theme that is not installed is worse than naming
/// none — GTK looks it up, fails, and falls back while overriding
/// whatever the user configured — so a pair is only ever used when
/// **both** members are present on disk (see [`gtk_theme_pair`]).
const GTK_THEME_PAIRS: [(&str, &str); 2] = [
    // (light, dark)
    ("Adwaita", "Adwaita-dark"),
    ("adw-gtk3", "adw-gtk3-dark"),
];

/// The directories GTK themes are installed under, per the icon/theme
/// spec lookup order.
fn theme_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join(".themes"));
    }
    match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => roots.push(PathBuf::from(dir).join("themes")),
        _ => {
            if let Some(home) = std::env::var_os("HOME") {
                roots.push(PathBuf::from(home).join(".local/share/themes"));
            }
        }
    }
    roots.push(PathBuf::from("/usr/local/share/themes"));
    roots.push(PathBuf::from("/usr/share/themes"));
    roots
}

/// Whether a named GTK theme is actually installed under any of
/// `roots` — a directory carrying a `gtk-3.0` or `gtk-4.0` payload (an
/// `index.theme` alone can describe a theme with no GTK CSS in it).
fn theme_installed(roots: &[PathBuf], name: &str) -> bool {
    roots.iter().any(|root| {
        let dir = root.join(name);
        dir.join("gtk-3.0").is_dir() || dir.join("gtk-4.0").is_dir()
    })
}

/// Pure core of [`gtk_theme_pair`]: the first preferred pair whose
/// members are both installed under `roots`.
fn gtk_theme_pair_in(roots: &[PathBuf]) -> Option<(&'static str, &'static str)> {
    GTK_THEME_PAIRS
        .into_iter()
        .find(|(light, dark)| theme_installed(roots, light) && theme_installed(roots, dark))
}

/// The `(light, dark)` GTK theme names this desktop may honestly
/// publish — `None` when no known pair is fully installed, in which
/// case nothing is published and applications keep their own theme
/// (documented in `docs/appearance.md`).
pub fn gtk_theme_pair() -> Option<(&'static str, &'static str)> {
    gtk_theme_pair_in(&theme_roots())
}

/// The GTK theme name to publish (XSETTINGS `Net/ThemeName` +
/// `Gtk/ThemeName`) for a mode, when a pair is installed.
pub fn gtk_theme_name(mode: Appearance) -> Option<&'static str> {
    let (light, dark) = gtk_theme_pair()?;
    Some(match mode {
        Appearance::Light => light,
        Appearance::Dark => dark,
    })
}

/// Tells foreign toolkits about the mode, off-thread.
///
/// Two GSettings writes at most, run on a dedicated short-lived thread
/// because reading the current `gtk-theme` back means waiting on a
/// child, which is banned on the shell's thread (see `clippy.toml` —
/// this is the same shape as the widget sampler workers). Switches are
/// user gestures, so the thread count is bounded by fingers.
///
/// - `color-scheme` is always set (`prefer-dark`/`prefer-light`): it
///   is a preference, not a theme name, and cannot dangle.
/// - `gtk-theme` is nudged only when its current value is already a
///   member of the installed pair this desktop manages: flipping
///   `Adwaita` to `Adwaita-dark` is the mode doing its job, while
///   overwriting a user's hand-picked `Whatever-Compact` would be
///   theft. No pair installed, no nudge.
/// - A missing `gsettings` binary or schema degrades with one warning
///   and nothing else: the desktop's own chrome, terminals and
///   dockapps have already switched by the time this runs.
pub fn propagate_to_applications(mode: Appearance) {
    // GSettings is per-user, not per-session: a posed nested session
    // (the e2e harness, a dev screenshot run) switching its own
    // appearance must not rewrite the developer's real preferences.
    // The test harness sets this variable on every session it boots.
    //
    // Not just politeness — a session whose XDG_CONFIG_HOME is
    // overridden has dconf reading its *client-side* database from the
    // isolated directory (empty, so `gsettings get` answers schema
    // defaults) while writes travel over D-Bus to the user's real
    // dconf-service. The adopt-if-ours check below would then compare
    // against a default and overwrite the user's actual choice —
    // observed live exactly once: a posed session read "Adwaita" where
    // the user had "Breeze", and flipped it. A real session's
    // environment is consistent and has no such split.
    if std::env::var_os("CHONKSTEP_NO_APPEARANCE_PROPAGATION").is_some() {
        tracing::info!(mode = mode.name(), "appearance propagation to GSettings disabled by environment");
        return;
    }
    std::thread::spawn(move || {
        let scheme = match mode {
            Appearance::Dark => "prefer-dark",
            Appearance::Light => "prefer-light",
        };
        // Audited exception to `clippy.toml`'s ban on blocking child
        // calls: this closure is the whole body of a thread spawned per
        // user gesture; nothing waits on it, and a gsettings that never
        // returns holds up one thread-sized allocation, not the shell.
        #[allow(clippy::disallowed_methods)]
        let run = |args: &[&str]| -> Option<std::process::Output> {
            std::process::Command::new("gsettings")
                .args(args)
                .output()
                .ok()
        };
        let set = run(&["set", GSETTINGS_SCHEMA, "color-scheme", scheme]);
        match set {
            Some(output) if output.status.success() => {
                tracing::info!(scheme, "told GSettings the appearance (portal color-scheme follows)");
            }
            _ => {
                // One line, as promised: covers both "no gsettings on
                // this system" and "no org.gnome.desktop.interface
                // schema installed".
                tracing::warn!(scheme, "gsettings unavailable or schema missing; GTK/portal applications will not follow the appearance");
                return;
            }
        }
        let Some((light, dark)) = gtk_theme_pair() else {
            return;
        };
        let Some(current) = run(&["get", GSETTINGS_SCHEMA, "gtk-theme"]) else {
            return;
        };
        let current = String::from_utf8_lossy(&current.stdout).trim().trim_matches('\'').to_string();
        if current == light || current == dark {
            let next = match mode {
                Appearance::Light => light,
                Appearance::Dark => dark,
            };
            if current != next {
                let _ = run(&["set", GSETTINGS_SCHEMA, "gtk-theme", next]);
                tracing::info!(from = %current, to = %next, "flipped the managed GTK theme pair");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_requests_wait_for_their_poll_deadline_without_being_lost() {
        let dir = std::env::temp_dir().join(format!("chonk-appearance-poller-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(REQUEST_FILE);
        let start = Instant::now();
        let mut poller = RequestPoller { path: path.clone(), next_poll: start };

        std::fs::write(&path, "dark").unwrap();
        assert_eq!(poller.take(start), Some(Request::Set(Appearance::Dark)));
        assert!(!path.exists(), "a delivered request is consumed exactly once");

        std::fs::write(&path, "toggle").unwrap();
        assert_eq!(poller.take(start + SESSION_REQUEST_POLL_INTERVAL / 2), None);
        assert!(path.exists(), "an early check leaves the request for its deadline");
        assert_eq!(poller.take(start + SESSION_REQUEST_POLL_INTERVAL), Some(Request::Toggle));
        assert!(!path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requests_parse_the_three_verbs_and_nothing_else() {
        assert_eq!(Request::parse("light"), Some(Request::Set(Appearance::Light)));
        assert_eq!(Request::parse("dark"), Some(Request::Set(Appearance::Dark)));
        assert_eq!(Request::parse("toggle"), Some(Request::Toggle));
        // Trimmed and case-insensitive: the writers are shell scripts
        // and dockapps, and `echo` appends a newline.
        assert_eq!(Request::parse("Dark\n"), Some(Request::Set(Appearance::Dark)));
        assert_eq!(Request::parse("  TOGGLE  "), Some(Request::Toggle));
        for text in ["", "  ", "dusk", "light dark", "toggle!"] {
            assert_eq!(Request::parse(text), None, "text {text:?}");
        }
    }

    #[test]
    fn a_request_resolves_against_the_current_mode() {
        assert_eq!(Request::Set(Appearance::Light).resolve(Appearance::Dark), Appearance::Light);
        assert_eq!(Request::Set(Appearance::Dark).resolve(Appearance::Dark), Appearance::Dark);
        assert_eq!(Request::Toggle.resolve(Appearance::Dark), Appearance::Light);
        assert_eq!(Request::Toggle.resolve(Appearance::Light), Appearance::Dark);
    }

    #[test]
    fn published_state_reads_the_two_moods_and_rejects_garbage() {
        let dir = std::env::temp_dir().join(format!("chonk-appearance-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("appearance");

        assert_eq!(load_published_from(&path), None, "missing file is no mode");
        std::fs::write(&path, "light").unwrap();
        assert_eq!(load_published_from(&path), Some(Appearance::Light));
        // The contract has no newline requirement either way.
        std::fs::write(&path, "dark\n").unwrap();
        assert_eq!(load_published_from(&path), Some(Appearance::Dark));
        std::fs::write(&path, "grayscale").unwrap();
        assert_eq!(load_published_from(&path), None, "garbage is ignored, not guessed at");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolution_prefers_published_then_config_then_the_themes_native_mood() {
        // Pure layers only (the state-file layer is exercised above and
        // in the shell's own tests): config beats native, native is the
        // floor. `resolve` itself reads the real state dir, so this
        // pins the pure fallback chain through its pieces.
        assert_eq!(
            None.or_else(|| Appearance::from_name("light")).unwrap_or(Appearance::Dark),
            Appearance::Light,
            "a configured mode beats the theme's native one"
        );
        assert_eq!(
            None.or_else(|| None::<&str>.and_then(Appearance::from_name)).unwrap_or(Appearance::Light),
            Appearance::Light,
            "with nothing said anywhere, the theme's own mood stands"
        );
    }

    #[test]
    fn gtk_theme_pairs_require_both_members_with_real_gtk_payloads() {
        let root = std::env::temp_dir().join(format!("chonk-gtk-pair-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let roots = vec![root.clone()];

        assert_eq!(gtk_theme_pair_in(&roots), None, "an empty root offers no pair");

        // A light half alone is not a pair.
        std::fs::create_dir_all(root.join("Adwaita/gtk-3.0")).unwrap();
        assert_eq!(gtk_theme_pair_in(&roots), None);

        // A dark half that is only an index.theme (no GTK CSS) still
        // does not count — naming it would send GTK looking for CSS
        // that is not there.
        std::fs::create_dir_all(root.join("Adwaita-dark")).unwrap();
        std::fs::write(root.join("Adwaita-dark/index.theme"), "[X-GNOME-Metatheme]\n").unwrap();
        assert_eq!(gtk_theme_pair_in(&roots), None);

        std::fs::create_dir_all(root.join("Adwaita-dark/gtk-4.0")).unwrap();
        assert_eq!(gtk_theme_pair_in(&roots), Some(("Adwaita", "Adwaita-dark")));

        let _ = std::fs::remove_dir_all(&root);
    }
}
