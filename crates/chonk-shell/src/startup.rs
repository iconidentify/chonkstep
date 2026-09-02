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

use std::collections::BTreeMap;
use wm_config::{Action, Config};
use wm_core::{DecorationRules, FocusPolicy, KeyCombo, Modifiers, PlacementPolicy};
use wm_theme::{Appearance, Theme};

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
    /// The chosen theme at 1x — the thing `scale` multiplies. Always
    /// the rendition of its id that matches [`Self::appearance`].
    pub base_theme: Theme,
    /// The session-wide light/dark mode `base_theme` was resolved in —
    /// see [`crate::appearance`] for the resolution layers and the
    /// published/request file contract.
    pub appearance: Appearance,
    /// `Some(wm_theme::omarchy::ID)` while this session *follows*
    /// Omarchy — its theme choice is "whatever Omarchy's current theme
    /// is" rather than one of the built-ins — and `None` otherwise.
    /// Set from the choice, not from success: a session that chose to
    /// follow but found no palette wears the flagship *and* keeps
    /// following, so the poll in `Shell::tick` picks the palette up the
    /// moment Omarchy writes one. The control socket reports this as
    /// the theme event's `following`.
    pub following: Option<&'static str>,
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
    /// Whether the root menu hosts Omarchy's own command menu as an
    /// `Omarchy` submenu (`crate::omarchy_menu`). Carried here so a
    /// reload can add or drop the submenu in place, like every other
    /// setting; the shell also re-reads the menu definition itself on
    /// every reload, which is how a fresh `omarchy update` reaches the
    /// menu without a restart.
    pub omarchy_menu: bool,
    /// Per-application decoration overrides, handed to the backend.
    ///
    /// Carried here rather than read straight off the config by each
    /// backend for the reason the doc comment above gives: this is the
    /// one path config takes into a running session, and a setting that
    /// travels beside it cannot be the one a reload forgets. It was —
    /// the marker-file reload updated the decoration policy and the
    /// bound `reload` key did not, so the same edit applied or did not
    /// depending on how the user asked for it.
    pub decorations: DecorationRules,
    /// The modifier for the move/resize drag gesture. `None` disables
    /// it.
    pub drag_modifier: Option<Modifiers>,
    /// Named argv the `run` bindings resolve against, straight off the
    /// config. Carried here rather than looked up from a reloaded
    /// config at press time so a binding and the command it names can
    /// never disagree: they arrive together, through this one path, or
    /// not at all.
    pub commands: BTreeMap<String, Vec<String>>,
    /// The terminal `spawn-terminal` launches, or `None` for the
    /// built-in one.
    pub terminal: Option<Vec<String>>,
    /// Commands to run once at session start. Startup-only in effect,
    /// like [`Self::restore_session`], and carried here for the same
    /// reason: one resolution path, so a reload cannot silently apply a
    /// different rule than the startup it replaces. A reload does not
    /// re-run these — "run once at session start" would otherwise mean
    /// "run again every time the user edits their config".
    pub autostart: Vec<Vec<String>>,
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
        let look = resolve_look(config.theme.as_deref(), config.appearance.as_deref());
        Self {
            base_theme: look.theme,
            appearance: look.appearance,
            following: look.following,
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
            omarchy_menu: config.omarchy_menu,
            decorations: config.decorations.clone(),
            drag_modifier: config.drag_modifier,
            commands: config.commands.clone(),
            terminal: config.terminal.clone(),
            autostart: config.autostart.clone(),
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

/// Which theme this session dresses in, by id. Precedence: the
/// persisted theme-menu choice wins over the config file's `theme`,
/// which wins over the flagship default - the menu is the more recent,
/// more deliberate gesture (picking a theme from it restarts on the
/// spot), so a config line written once must not keep overriding it
/// forever.
///
/// [`theme_select::load_id`] already implements the outer layer (the
/// state file, when it names something this build knows); the config
/// layer slots after it here rather than inside that module, which is
/// a pure persist/recall mechanism the menu itself also uses. Which
/// theme wins at *startup* is session policy.
///
/// The id may be [`wm_theme::omarchy::ID`] from either layer — the
/// "follow Omarchy" choice is a theme id like any other here, and only
/// [`resolve_look`] knows it is resolved by reading a file rather than
/// by lookup.
pub fn resolve_theme_id(config_theme: Option<&str>) -> String {
    theme_select::load_id()
        .or_else(|| config_theme_fallback(config_theme))
        .unwrap_or_else(|| wm_theme::default_theme::nextstep_classic().id)
}

/// What [`resolve_look`] decides: the theme at 1x, the appearance it
/// was resolved in, and whether the session is following Omarchy.
#[derive(Clone, Debug)]
pub struct Look {
    pub theme: Theme,
    pub appearance: Appearance,
    pub following: Option<&'static str>,
}

/// [`resolve_theme_id`] with the appearance axis on top: which theme,
/// and which of its two renditions.
///
/// The identity question resolves first (persisted menu choice over
/// config over flagship). The appearance then resolves through
/// [`crate::appearance::resolve`] — published state, else the config's
/// `appearance`, else the theme's own native mood — and the chosen id
/// is re-dressed in that rendition. The native-mood floor is what
/// makes the axis invisible until it is used: with nothing published
/// and nothing configured, every theme looks exactly as it always has.
///
/// When the id is [`wm_theme::omarchy::ID`] the theme is built from
/// Omarchy's current `colors.toml` instead, and **the palette's `mode`
/// is the appearance** — a light Omarchy theme is a light desk, no
/// matter what was published or configured. An Omarchy theme has one
/// rendition, the one its author wrote; there is no other to switch to,
/// and re-deriving one would put colours on screen the author never
/// chose while every Omarchy terminal beside them kept the real ones.
/// When Omarchy has no readable palette (not installed, no theme set,
/// a file that does not parse) the session wears the flagship exactly
/// as it would for an unknown id, warns once per distinct reason, and
/// keeps `following` set so a palette appearing later is picked up.
pub fn resolve_look(config_theme: Option<&str>, config_appearance: Option<&str>) -> Look {
    let id = resolve_theme_id(config_theme);
    let following = (id == wm_theme::omarchy::ID).then_some(wm_theme::omarchy::ID);
    if following.is_some() {
        match wm_theme::omarchy::load_current() {
            Ok(theme) => {
                omarchy_trouble(None);
                return Look { appearance: theme.appearance, theme, following };
            }
            Err(reason) => omarchy_trouble(Some(reason)),
        }
    }
    let native = wm_theme::default_theme::theme_by_id(&id).unwrap_or_else(wm_theme::default_theme::nextstep_classic);
    let appearance = crate::appearance::resolve(config_appearance, native.appearance);
    let theme = wm_theme::default_theme::theme_variant(&native.id, appearance).unwrap_or(native);
    Look { theme, appearance, following }
}

/// Warns about an unreadable Omarchy palette once per distinct reason
/// rather than once per resolve: `resolve_look` runs on every reload
/// and on every change the Omarchy poll sees, and a machine without
/// Omarchy that was told to follow it should say so once, not on a
/// timer. `None` clears the memory so the next failure is reported.
fn omarchy_trouble(reason: Option<String>) {
    static LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if reason.is_some() && *last != reason {
        tracing::warn!(reason = reason.as_deref().unwrap_or_default(), "following Omarchy, but its current theme has no readable colors.toml; wearing the default until it does");
    }
    *last = reason;
}

/// The config-file layer of [`resolve_theme_id`], kept free of
/// filesystem access so its edges are testable: `None` when no theme is
/// configured, and - critically - `None` with a warning, not an error,
/// when the configured id names a theme this build does not ship. A
/// misspelled theme must cost the user the flagship look for one
/// session, never the session itself.
pub fn config_theme_fallback(config_theme: Option<&str>) -> Option<String> {
    let requested = config_theme?;
    if !theme_select::is_known(requested) {
        tracing::warn!(theme = requested, "config names an unknown theme; using the default instead");
        return None;
    }
    Some(requested.to_string())
}

/// Where this session keeps the small state files it writes for
/// itself: the theme-menu choice, the dock order, the published
/// appearance (see `crate::appearance`), and the request markers below.
pub(crate) fn state_dir() -> std::path::PathBuf {
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
    *SESSION_CONTINUES.get_or_init(|| std::env::var_os(SESSION_CONTINUES_VAR).is_some())
}

/// The environment variable [`session_continues`] answers from. Set by
/// both binaries' `restart_in_place` on the process they exec into.
const SESSION_CONTINUES_VAR: &str = "CHONKSTEP_SESSION_CONTINUES";

/// Answered once, so the value survives [`consume_session_continuation`]
/// taking the variable back out of the environment.
static SESSION_CONTINUES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Reads the continuation marker and removes it from the environment.
///
/// Call once, first thing in `main`, before any thread exists — the
/// same placement and the same reason as
/// [`crate::spawn::declare_display_stack`]. `remove_var` is only sound
/// while this process is single-threaded.
///
/// It has to be *consumed* rather than merely read, because a marker
/// left in the environment is inherited by every process the desktop
/// launches, and it means the wrong thing to all of them. It says "you
/// are a continuation of a session that is already running". A terminal,
/// a browser, a dockapp — none of them care. But a nested chonkstep
/// session started from a terminal inside a restarted one reads it,
/// concludes it is a hot restart, and silently skips both session
/// restore and autostart. That is a genuinely confusing failure: the
/// same binary and the same config behave differently depending on
/// whether the session that launched the terminal had ever been
/// restarted.
///
/// Found exactly that way — an autostart entry that worked from a fresh
/// login did nothing when launched from a terminal, because the
/// terminal's parent session had been hot-restarted hours earlier.
pub fn consume_session_continuation() {
    let present = std::env::var_os(SESSION_CONTINUES_VAR).is_some();
    // Set before removing, so a `session_continues()` racing in from
    // anywhere still resolves to the value the variable actually had.
    let _ = SESSION_CONTINUES.set(present);
    if present {
        std::env::remove_var(SESSION_CONTINUES_VAR);
    }
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
            appearance: Appearance::Dark,
            following: None,
            scale: 2.0,
            focus: FocusPolicy::ClickToFocus,
            placement: PlacementPolicy::Smart,
            edge_resistance: 10,
            terminal_font_px: 20.0,
            restore_session: false,
            commands: BTreeMap::new(),
            terminal: None,
            autostart: Vec::new(),
            omarchy_menu: true,
            decorations: DecorationRules::default(),
            drag_modifier: Some(wm_core::DEFAULT_DRAG_MODIFIER),
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
            assert_eq!(config_theme_fallback(Some(id)), Some(id.to_string()), "built-in theme id {id} must resolve from config");
        }
    }
    #[test]
    fn config_theme_omarchy_is_the_follow_choice() {
        assert_eq!(config_theme_fallback(Some("omarchy")), Some(wm_theme::omarchy::ID.to_string()));
    }
    #[test]
    fn unknown_config_theme_degrades_to_none() {
        // `resolve_theme_id` then falls through to the flagship — a
        // misspelled theme name must never cost the user the session.
        assert!(config_theme_fallback(Some("no-such-theme")).is_none());
        assert!(config_theme_fallback(Some("")).is_none());
    }
    #[test]
    fn absent_config_theme_is_not_an_error() {
        assert!(config_theme_fallback(None).is_none());
    }
}

#[cfg(test)]
mod continuation_tests {
    /// The marker must not survive into the environment children
    /// inherit. A leaked one makes a nested session skip its own
    /// autostart and layout restore, which is how it was found.
    ///
    /// Single test on purpose: `consume_session_continuation` writes
    /// process-global state exactly once (a `OnceLock` plus a
    /// `remove_var`), so splitting this across tests would have them
    /// race for the one initialization that is allowed to happen.
    #[test]
    fn consuming_the_marker_takes_it_out_of_the_environment() {
        std::env::set_var(super::SESSION_CONTINUES_VAR, "1");
        super::consume_session_continuation();
        assert!(super::session_continues(), "the marker was set, so this process is a continuation");
        assert!(
            std::env::var_os(super::SESSION_CONTINUES_VAR).is_none(),
            "the marker must be gone from the environment so children do not inherit it"
        );
        // A second consume must not flip the answer: the value is
        // settled once, and the variable is already gone by now.
        super::consume_session_continuation();
        assert!(super::session_continues(), "the answer is settled once and stays settled");
    }
}
