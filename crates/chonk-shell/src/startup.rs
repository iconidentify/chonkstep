//! Session policy: the handful of decisions a chonkstep session makes
//! before it draws anything - how big the UI is, which theme it wears,
//! whether focus follows the mouse - and, deliberately, the one place
//! both binaries make them.
//!
//! These rules are the kind that quietly diverge. Each resolver
//! combines an environment variable, a config-file value, and a
//! built-in default, and every one of them was written for the X11
//! session first; when the Wayland compositor grew its own startup it
//! reimplemented two of them and dropped the environment layer from a
//! third, so a machine with `CHONKSTEP_SCALE=2` in `/etc/environment`
//! got a HiDPI X session and a tiny Wayland one. That is exactly the
//! class of difference the shared-shell architecture exists to
//! prevent, so the policy lives here with the rest of the shared
//! desktop rather than in either binary.
//!
//! Every resolver splits into a thin `read_*` that touches the process
//! environment and a pure `resolve_*` that does not: precedence logic
//! stays unit-testable without `set_var`, which tests running in
//! parallel threads of one process cannot use safely.

use wm_config::{Action, Config};
use wm_core::{FocusPolicy, KeyCombo, PlacementPolicy};
use wm_theme::Theme;

use crate::theme_select;

/// Everything about a running session that a user can change without
/// restarting it: the look (theme and UI scale) and the policy the
/// config file sets.
///
/// This type is why the module's doc comment above is written in the
/// present tense rather than about startup alone. Those resolvers began
/// as "the decisions a session makes before it draws anything"; they
/// are now the decisions a session makes *whenever* it is asked to,
/// because a theme pick and a config reload re-make exactly the same
/// set. Bundling them means a live change and a fresh start resolve
/// through one code path with one precedence order — the alternative,
/// which this repository has already paid for once, is two paths that
/// agree until the day someone edits one of them.
///
/// The theme is kept at 1x with the scale beside it rather than
/// pre-multiplied, because scaling is not reversible: `Theme::scaled`
/// rounds every metric to whole pixels, so a theme scaled to 2 and back
/// to 1 is not the theme it started as. Holding the unscaled original
/// is what lets the scale change twice without the chrome drifting.
#[derive(Clone, Debug)]
pub struct SessionState {
    /// The chosen theme at 1x — the thing `scale` multiplies.
    pub base_theme: Theme,
    /// UI scale factor; always finite and positive (see
    /// [`resolve_scale`]).
    pub scale: f32,
    pub focus: FocusPolicy,
    pub placement: PlacementPolicy,
    pub edge_resistance: u32,
    pub terminal_font_px: f32,
    /// Whether the shell should relaunch the previous session's layout
    /// at startup — see `crate::session_layout`. Startup-only in
    /// effect (a reload cannot un-launch what a fresh start launched),
    /// but carried here so it resolves through the same one path as
    /// everything else the config sets.
    pub restore_session: bool,
    pub keybindings: Vec<(KeyCombo, Action)>,
}

impl SessionState {
    /// Resolves a whole session's worth of state from a freshly loaded
    /// config, applying every precedence rule in this module: the
    /// environment over the config for scale and focus, the persisted
    /// theme-menu choice over the config for the theme.
    ///
    /// Called at startup and again on every reload, which is the point
    /// — a reload that resolved by different rules than the startup it
    /// replaces would make "restart to be sure" true again.
    pub fn resolve(config: &Config) -> Self {
        Self {
            base_theme: resolve_theme(config.theme.as_deref()),
            scale: read_scale_factor(config.scale),
            focus: if read_focus_follows_mouse(config.focus_follows_mouse) {
                FocusPolicy::FocusFollowsMouse
            } else {
                FocusPolicy::ClickToFocus
            },
            placement: config.placement,
            edge_resistance: config.edge_resistance,
            terminal_font_px: config.terminal_font_px,
            restore_session: config.restore_session,
            keybindings: config.keybindings.clone(),
        }
    }

    /// The theme every surface is actually drawn from: [`Self::base_theme`]
    /// at [`Self::scale`].
    pub fn theme(&self) -> Theme {
        self.base_theme.scaled(self.scale)
    }
}

/// UI scale. Precedence: `CHONKSTEP_SCALE` beats the config file's
/// `scale`, which beats 1.0 (no scaling). The environment stays on top
/// because session launchers and dev scripts use it to override a
/// user's baseline per invocation.
pub fn read_scale_factor(config_scale: Option<f32>) -> f32 {
    resolve_scale(std::env::var("CHONKSTEP_SCALE").ok().as_deref(), config_scale)
}

/// Pure core of [`read_scale_factor`]. A value that fails validation -
/// unparseable, non-finite, zero, or negative - is *skipped*, not
/// clamped: a broken env var falls through to the config value and a
/// broken config value falls through to 1.0, so a typo degrades to the
/// next-best answer instead of a garbage scale or a dead session.
pub fn resolve_scale(env: Option<&str>, config: Option<f32>) -> f32 {
    let valid = |s: &f32| s.is_finite() && *s > 0.0;
    env.and_then(|s| s.trim().parse::<f32>().ok())
        .filter(valid)
        .or_else(|| config.filter(valid))
        .unwrap_or(1.0)
}

/// Whether to switch from click-to-focus to focus-follows-mouse.
/// `CHONKSTEP_FOCUS_FOLLOWS_MOUSE` wins over the config file in *both*
/// directions: set to anything other than `1` it forces the policy off
/// even when the config asks for it, so a session script can pin
/// either behavior. Only its total absence lets the config value
/// apply.
pub fn read_focus_follows_mouse(config_value: bool) -> bool {
    resolve_focus_follows_mouse(
        std::env::var("CHONKSTEP_FOCUS_FOLLOWS_MOUSE").ok().as_deref(),
        config_value,
    )
}

/// Pure core of [`read_focus_follows_mouse`].
pub fn resolve_focus_follows_mouse(env: Option<&str>, config: bool) -> bool {
    match env {
        Some(value) => value == "1",
        None => config,
    }
}

/// The theme this session dresses in. Precedence: the persisted
/// theme-menu choice wins over the config file's `theme`, which wins
/// over the flagship default - the menu is the more recent, more
/// deliberate gesture (picking a theme from it restarts on the spot),
/// so a config line written once must not keep overriding it forever.
///
/// [`theme_select::load`] already implements the outer two layers
/// (state file, else flagship); the config layer slots between them
/// here rather than inside that module, which is a pure persist/recall
/// mechanism the menu itself also uses. Which theme wins at *startup*
/// is session policy.
pub fn resolve_theme(config_theme: Option<&str>) -> Theme {
    if !persisted_theme_choice_exists() {
        if let Some(theme) = config_theme_fallback(config_theme) {
            return theme;
        }
    }
    theme_select::load()
}

/// The config-file layer of [`resolve_theme`], kept free of filesystem
/// access so its edges are testable: `None` when no theme is
/// configured, and - critically - `None` with a warning, not an error,
/// when the configured id names a theme this build does not ship. A
/// misspelled theme must cost the user the flagship look for one
/// session, never the session itself.
pub fn config_theme_fallback(config_theme: Option<&str>) -> Option<Theme> {
    let requested = config_theme?;
    let theme = wm_theme::default_theme::theme_by_id(requested);
    if theme.is_none() {
        tracing::warn!(theme = requested, "config names an unknown theme; using the default instead");
    }
    theme
}

/// `true` exactly when [`theme_select::load`] would return a persisted
/// menu choice rather than its flagship fallback: the state file
/// exists and names a theme this build still ships. Must stay in
/// lockstep with `theme_select`'s own state path.
pub fn persisted_theme_choice_exists() -> bool {
    let path = if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        std::path::PathBuf::from(root).join("chonkstep/theme")
    } else if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".local/state/chonkstep/theme")
    } else {
        return false;
    };
    std::fs::read_to_string(path)
        .ok()
        .map(|id| id.trim().to_string())
        .is_some_and(|id| wm_theme::default_theme::theme_by_id(&id).is_some())
}

/// Where this session keeps the small state files it writes for
/// itself: the theme-menu choice, the dock order, and the two
/// request markers below.
fn state_dir() -> std::path::PathBuf {
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        return std::path::PathBuf::from(root).join("chonkstep");
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| ".".into());
    home.join(".local/state/chonkstep")
}

/// Whether something has asked this session to re-exec its on-disk
/// binary since the last call (`scripts/restart.sh` writes the marker).
///
/// A destructive read: the marker is consumed by observing it, so a
/// request is honored exactly once. Lives here, shared, because both
/// binaries poll it once per wakeup and had grown their own copy of
/// this function — one of which had already drifted to inlining the
/// path the other had factored out.
pub fn restart_requested() -> bool {
    std::fs::remove_file(state_dir().join("restart")).is_ok()
}

/// Whether something has asked this session to re-read its config file
/// and apply it in place since the last call (`scripts/reload.sh`).
///
/// The cheaper half of the pair, and the one to reach for: a reload
/// keeps every window, every client connection and every dockapp,
/// where a restart on the Wayland session keeps only the compositor
/// and its dockapps. Polled rather than watched with inotify — the
/// same argument the dockapp theme broadcast makes for polling, plus
/// a much smaller one: the session already wakes at 16ms to do
/// housekeeping, so a poll costs a `unlink` syscall on a path that is
/// almost never there.
pub fn reload_requested() -> bool {
    std::fs::remove_file(state_dir().join("reload")).is_ok()
}

/// Whether the previous compositor process *crashed* and the session
/// supervisor (`scripts/wayland-session.sh`) restarted it — the
/// supervisor drops a `recovery` marker in the state directory right
/// before re-execing after an abnormal exit, and only then.
///
/// A destructive read like [`restart_requested`], and for the same
/// reason: recovery is acknowledged exactly once. Call it once at
/// startup — the marker is a statement about how *this* process came
/// to exist, not something to poll.
pub fn recovering_from_crash() -> bool {
    std::fs::remove_file(state_dir().join("recovery")).is_ok()
}

/// The state-file path the session layout store persists to — beside
/// the theme, wallpaper and dock files it behaves like. Exposed from
/// here (rather than duplicated in `session_layout`) so every state
/// file resolves through the one `state_dir` rule.
pub fn session_layout_path() -> std::path::PathBuf {
    state_dir().join("session")
}

/// Whether this process is the continuation of a session that is still
/// running in every way that matters — a hot restart (`Action::Restart`,
/// `scripts/restart.sh`) re-execing the on-disk binary in place. Both
/// binaries set the marker variable in their `restart_in_place` before
/// exec, and the environment is exactly what survives an exec.
///
/// Session-layout restore must not fire on a continuation: on the X11
/// stack every client *survives* the re-exec via the SaveSet, so
/// relaunching the recorded layout there would duplicate every window
/// on the screen. A crash recovery is not a continuation — the
/// supervisor starts a fresh process with a fresh environment — which
/// is exactly the case restore exists for.
pub fn session_continues() -> bool {
    std::env::var_os("CHONKSTEP_SESSION_CONTINUES").is_some()
}

/// Whether `XCURSOR_SIZE` was the user's own when this session started,
/// rather than something [`ensure_xcursor_size`] put there.
///
/// Recorded because after startup the two are indistinguishable — the
/// variable is set either way — and they must be treated differently:
/// a value the user pinned is a preference this desktop has no business
/// overriding at any scale, while one this session derived is stale the
/// moment the scale changes. See [`xcursor_size_env`].
static XCURSOR_SIZE_WAS_PRESET: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// The Xcursor size this desktop implies at `scale`. 24px is Xcursor's
/// own conventional 1x base size.
fn xcursor_size_for(scale: f32) -> u32 {
    (24.0 * scale).round().max(1.0) as u32
}

/// The `XCURSOR_SIZE` to hand a freshly launched application, or `None`
/// when the user pinned one of their own.
///
/// This exists because [`ensure_xcursor_size`] cannot be re-run. It
/// writes the *process* environment, and its safety argument is that it
/// happens before any other thread exists — which stops being true the
/// instant the session is up, and a live scale change happens well
/// after that. So the scale that reaches an application is put in that
/// application's own environment at spawn time instead, where no shared
/// state is mutated and no thread can race.
///
/// The consequence worth knowing: an app launched after a live rescale
/// gets a correctly sized cursor, and an app that was already running
/// does not. Xcursor reads this once, at the point a client sets up its
/// cursor theme, and there is no protocol for telling it again.
///
/// # The value is per-stack, and being explicit about it is the fix
///
/// On the Wayland session the variable is handed over *unscaled* (the
/// 24px base), and explicitly rather than absent: absent would mean
/// "inherit the process environment", which [`ensure_xcursor_size`]
/// already scaled. The convention every Wayland client follows is that
/// `XCURSOR_SIZE` is a **logical** size — the client multiplies it by
/// the output scale the protocol advertises when it loads its cursor
/// theme. Handing a native client the pre-multiplied number therefore
/// scales the pointer twice: observed live as foot drawing a 96px
/// cursor on a scale-2 session whose every other pointer was 48px —
/// exactly the double-scaling `terminal_args`' font math and the
/// launcher's withheld `GDK_SCALE` already guard against elsewhere.
/// The X11 session keeps the pre-multiplied value, because there is no
/// output scale there for a client to multiply by; that is the whole
/// reason the multiplication exists.
///
/// The known cost, accepted on purpose: an X11-only application under
/// XWayland reads this without multiplying, so it now sees the 1x base
/// and draws a small pointer — unless it speaks XSETTINGS, where
/// `Gtk/CursorThemeSize` still carries the scaled size (see
/// `chonk-xsettings`). One environment variable cannot be right for
/// both client families at once, and native clients are the common
/// case on the stack whose protocol can correct the rest.
pub fn xcursor_size_env(scale: f32) -> Option<(String, String)> {
    if XCURSOR_SIZE_WAS_PRESET.get().copied().unwrap_or(false) {
        return None;
    }
    let effective = xcursor_env_scale(crate::spawn::current_display_stack(), scale);
    Some(("XCURSOR_SIZE".to_string(), xcursor_size_for(effective).to_string()))
}

/// The pure per-stack rule behind [`xcursor_size_env`]: the scale the
/// child should multiply into `XCURSOR_SIZE`'s base — 1 where the
/// display protocol will tell the client the real scale (Wayland),
/// the session's own scale where nothing else can (X11).
pub fn xcursor_env_scale(stack: crate::spawn::DisplayStack, scale: f32) -> f32 {
    match stack {
        crate::spawn::DisplayStack::Wayland => 1.0,
        crate::spawn::DisplayStack::X11 => scale,
    }
}

/// Sets `XCURSOR_SIZE` on this process (inherited by every app the
/// session spawns) unless it is already set. Both sessions scale their
/// own chrome and their own pointer, but an application that draws its
/// *own* cursor through Xcursor - most toolkits, and every X11 client
/// under XWayland - has no way to learn about that scale and reads
/// this variable instead. Without it, such an app's cursor is visibly
/// out of proportion the instant the pointer crosses onto its content.
/// 24px is Xcursor's own conventional 1x base size.
pub fn ensure_xcursor_size(scale: f32) {
    // "Already set" is not the same as "the user set it", and telling
    // the two apart is what the marker variable is for. A hot restart
    // re-execs this process, so the replacement inherits the
    // `XCURSOR_SIZE` its predecessor computed — at whatever scale that
    // session happened to start at. Treating it as a user preference
    // froze the cursor size at the very first boot's scale forever:
    // observed live as a session at scale 2 handing every child
    // `XCURSOR_SIZE=24`, because the first boot predated the config's
    // `scale` line. The marker survives the exec precisely because the
    // environment does, so a value this desktop set is recognized as
    // its own and recomputed rather than honored.
    //
    // THE ONE CASE THIS CANNOT REPAIR is the session that was already
    // running when the marker was introduced. Its `XCURSOR_SIZE`
    // predates the variable that would identify it, so the test below
    // reads it as a user preference and stands down — and no hot
    // restart can clear it, because preserving the environment across
    // the exec is the very mechanism the marker depends on. The
    // symptom is the double-scaled pointer `xcursor_size_env` exists
    // to prevent, still there after the fix has landed: observed live
    // as a session carrying a pre-marker `XCURSOR_SIZE=48` across
    // every restart for two days. Only a full logout sheds it, since
    // that is the only path that starts the compositor from an
    // environment this desktop did not hand itself. Deliberately not
    // papered over by adopting any value that happens to equal
    // `xcursor_size_for(scale)`: that would silently overwrite the
    // preference of a user who really did pin 48 on a scale-2 session,
    // which is exactly who the marker is here to protect. A one-time
    // logout is the cheaper price, and it is paid once per install.
    let ours = std::env::var_os("CHONKSTEP_OWNS_XCURSOR_SIZE").is_some();
    let preset = std::env::var_os("XCURSOR_SIZE").is_some() && !ours;
    // Recorded before the early return, so the answer is available
    // whichever branch is taken — see `XCURSOR_SIZE_WAS_PRESET`.
    let _ = XCURSOR_SIZE_WAS_PRESET.set(preset);
    if preset {
        return;
    }
    let size = xcursor_size_for(scale);
    // SAFETY: called once at the very start of a session's startup,
    // before any other thread exists, so no concurrent env access is
    // possible.
    unsafe {
        std::env::set_var("XCURSOR_SIZE", size.to_string());
        std::env::set_var("CHONKSTEP_OWNS_XCURSOR_SIZE", "1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_xcursor_size_tracks_the_scale_from_the_conventional_base() {
        assert_eq!(xcursor_size_for(1.0), 24);
        assert_eq!(xcursor_size_for(2.0), 48);
        assert_eq!(xcursor_size_for(1.5), 36);
        // Floored, for the same reason the tile edge is: a hand-edited
        // `scale = 0.001` must not produce a zero-pixel cursor.
        assert_eq!(xcursor_size_for(0.001), 1);
    }

    #[test]
    fn a_session_state_scales_a_theme_it_keeps_at_1x() {
        let base = wm_theme::default_theme::nextstep_classic();
        let state = SessionState {
            base_theme: base.clone(),
            scale: 2.0,
            focus: FocusPolicy::ClickToFocus,
            placement: PlacementPolicy::Smart,
            edge_resistance: 10,
            terminal_font_px: 20.0,
            restore_session: false,
            keybindings: Vec::new(),
        };
        assert_eq!(state.theme(), base.scaled(2.0));
        // The load-bearing half: the state still holds the *unscaled*
        // theme afterwards. `Theme::scaled` rounds every metric to whole
        // pixels and is not reversible, so a session that kept only the
        // scaled theme would drift a little further from the original
        // every time the scale changed.
        assert_eq!(state.base_theme, base);
    }

    #[test]
    fn the_xcursor_env_scale_is_withheld_exactly_where_the_protocol_carries_it() {
        use crate::spawn::DisplayStack;
        // Wayland clients multiply XCURSOR_SIZE by the advertised
        // output scale themselves — handing them the session's factor
        // doubles the pointer (observed live: a 96px cursor over the
        // terminal on a scale-2 session). X11 clients have no such
        // channel, which is why the multiplication exists at all.
        assert_eq!(xcursor_env_scale(DisplayStack::Wayland, 2.0), 1.0);
        assert_eq!(xcursor_env_scale(DisplayStack::X11, 2.0), 2.0);
        assert_eq!(xcursor_size_for(xcursor_env_scale(DisplayStack::Wayland, 2.0)), 24);
        assert_eq!(xcursor_size_for(xcursor_env_scale(DisplayStack::X11, 2.0)), 48);
    }

    #[test]
    fn scale_defaults_to_one_with_neither_source() {
        assert_eq!(resolve_scale(None, None), 1.0);
    }
    #[test]
    fn unparseable_env_scale_falls_through_to_config() {
        // A typo'd env var must degrade to the next-best answer, not to
        // the hard default — the config value is still a deliberate
        // user choice.
        assert_eq!(resolve_scale(Some("garbage"), Some(1.5)), 1.5);
    }
    #[test]
    fn out_of_range_env_scale_falls_through_to_config() {
        assert_eq!(resolve_scale(Some("0"), Some(1.5)), 1.5);
        assert_eq!(resolve_scale(Some("-2"), Some(1.5)), 1.5);
        // `parse::<f32>` accepts these spellings; the validity filter
        // must still reject them (a NaN scale poisons every pixel
        // computation downstream).
        assert_eq!(resolve_scale(Some("inf"), Some(1.5)), 1.5);
        assert_eq!(resolve_scale(Some("NaN"), Some(1.5)), 1.5);
    }
    #[test]
    fn invalid_config_scale_falls_through_to_default() {
        assert_eq!(resolve_scale(None, Some(0.0)), 1.0);
        assert_eq!(resolve_scale(None, Some(-1.0)), 1.0);
        assert_eq!(resolve_scale(None, Some(f32::NAN)), 1.0);
        assert_eq!(resolve_scale(None, Some(f32::INFINITY)), 1.0);
    }
    #[test]
    fn both_sources_invalid_still_yields_a_usable_scale() {
        // The end-to-end graceful-degradation guarantee: no combination
        // of broken inputs may leave the session without a scale.
        assert_eq!(resolve_scale(Some("bogus"), Some(-3.0)), 1.0);
    }
    #[test]
    fn whitespace_around_env_scale_is_tolerated() {
        assert_eq!(resolve_scale(Some(" 1.5 "), None), 1.5);
    }
    #[test]
    fn env_focus_var_enables_over_config_off() {
        assert!(resolve_focus_follows_mouse(Some("1"), false));
    }
    #[test]
    fn env_focus_var_disables_over_config_on() {
        // The env var wins in BOTH directions: any present value other
        // than "1" pins the policy off regardless of the config.
        assert!(!resolve_focus_follows_mouse(Some("0"), true));
        assert!(!resolve_focus_follows_mouse(Some(""), true));
        assert!(!resolve_focus_follows_mouse(Some("true"), true));
    }
    #[test]
    fn config_focus_value_applies_without_env() {
        assert!(resolve_focus_follows_mouse(None, true));
        assert!(!resolve_focus_follows_mouse(None, false));
    }
    #[test]
    fn config_theme_resolves_known_ids() {
        for id in ["nextstep-classic", "amber-phosphor", "teal-blueprint", "graphite", "next-lavender"] {
            let theme = config_theme_fallback(Some(id));
            assert_eq!(theme.map(|t| t.id), Some(id.to_string()), "built-in theme id {id} must resolve from config");
        }
    }
    #[test]
    fn unknown_config_theme_degrades_to_none() {
        // `resolve_theme` then falls through to the flagship — a
        // misspelled theme name must never cost the user the session.
        assert!(config_theme_fallback(Some("no-such-theme")).is_none());
        assert!(config_theme_fallback(Some("")).is_none());
    }
    #[test]
    fn absent_config_theme_is_not_an_error() {
        assert!(config_theme_fallback(None).is_none());
    }
}
