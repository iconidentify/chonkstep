//! User-facing configuration: the `config.toml` format, keybinding spec
//! parsing, and the built-in defaults everything merges over.
//!
//! Kept as its own crate from the start (even though its needs are
//! small) because config parsing is genuinely orthogonal to
//! windowing/rendering — a future control tool could reuse it without
//! pulling in either.
//!
//! Two rules shape every decision here:
//!
//! - **A broken config must never cost the user their session.**
//!   [`load`] is infallible: a missing file silently yields the
//!   defaults; an unreadable or syntactically invalid file logs a
//!   warning and falls back to the defaults; and an individually bad
//!   entry (a typo'd key spec, an unknown action name, a wrongly typed
//!   value) is warned about and skipped while every *other* entry still
//!   applies. A window manager that refuses to start over one bad line
//!   would strand the user at a blank X session with no way to fix it.
//! - **User bindings merge over the defaults, they never replace the
//!   set wholesale.** Listing one combo in `[keybindings]` overrides
//!   only that combo; unlisted defaults survive. The sentinel value
//!   `"none"` unbinds a combo outright, so every default is escapable
//!   without the user re-listing the rest. Within the file, a combo
//!   spelled twice (possible via case or aliases like `ctrl`/`control`)
//!   resolves to the *last* occurrence, matching how people read a file
//!   top to bottom.
//!
//! The format, in full:
//!
//! ```toml
//! focus_follows_mouse = false        # optional; default false
//! scale = 2.0                        # optional; UI scale factor
//! theme = "nextstep-classic"         # optional; theme name
//! appearance = "dark"                # optional; "light" | "dark"
//! placement = "smart"                # optional; "smart" | "cascade" | "center"
//! edge_resistance = 10               # optional; px, 0 disables edge snapping
//! terminal_font_px = 20              # optional; terminal font size at 1x
//! self_decorating_apps = ["chrome"]  # optional; app_ids that draw their own chrome
//! restore_session = true             # optional; relaunch last session's windows
//! lock_command = "swaylock"          # optional; locker for post-crash recovery
//!
//! [keybindings]
//! "alt+shift+return" = "spawn-terminal"
//! "super+t" = "spawn-terminal"       # extra binding for the same action
//! "alt+ctrl+right" = "none"          # unbind a default
//! ```
//!
//! Key specs are case-insensitive, `+`-separated modifier tokens
//! followed by exactly one key token (see [`parse_key`]). Action names
//! are the kebab-case of the [`Action`] variants. Precedence against
//! environment variables and persisted UI state (`CHONKSTEP_SCALE`,
//! the theme-menu state file, ...) is the binary's business, not this
//! crate's — which is why `scale` and `theme` stay `Option` here
//! instead of being defaulted: the caller must be able to tell "user
//! said nothing" apart from "user chose the default value".

use std::path::PathBuf;

pub use wm_core::FocusPolicy;
use wm_core::{KeyCombo, Modifiers, PlacementPolicy};

/// Everything a keybinding can do. Deliberately a closed set of verbs
/// rather than free-form commands: the WM owns the semantics (which
/// window, which workspace, what "toggle" means mid-drag), so exposing
/// anything finer-grained than these verbs would leak state-machine
/// internals into the config format.
///
/// Config files name these in kebab-case (`"spawn-terminal"`,
/// `"workspace-carry-next"`, ...). The pseudo-action `"none"` is *not*
/// a variant on purpose — it means "remove the binding", and letting it
/// exist as an `Action` would force every dispatch site to handle a
/// do-nothing case that should have been filtered out at parse time.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    SpawnTerminal,
    Close,
    ToggleMaximize,
    ToggleShade,
    Miniaturize,
    ToggleFullscreen,
    WorkspaceNext,
    WorkspacePrev,
    WorkspaceCarryNext,
    WorkspaceCarryPrev,
    /// Toggle the modal Overview: every window on the current
    /// workspace as a grid of live thumbnails plus a workspace strip,
    /// drawn and driven by the desktop shell. One verb on purpose —
    /// while the Overview is open the shell owns the whole keyboard
    /// (arrows move, Return commits, Escape dismisses), so those keys
    /// are modal machinery like the Alt-Tab switcher's, not
    /// per-binding config.
    Overview,
    /// Re-read this file and apply it to the running session — theme,
    /// UI scale, focus policy, placement, edge resistance and these
    /// very bindings, with no restart and nothing closed.
    Reload,
    /// Re-exec the session's on-disk binary. Distinct from [`Self::Reload`]
    /// on purpose: reloading applies a changed *config*, restarting
    /// applies a changed *build*, and only the second one has to cost
    /// the user anything.
    Restart,
}

/// Maps a kebab-case action name from a config file to its [`Action`].
/// Case-insensitive for the same reason key specs are: nothing is
/// gained by making `"Close"` a startup-breaking typo. Returns `None`
/// for unknown names — including `"none"`, which the caller must treat
/// as unbinding *before* asking here.
fn action_from_name(name: &str) -> Option<Action> {
    match name.trim().to_ascii_lowercase().as_str() {
        "spawn-terminal" => Some(Action::SpawnTerminal),
        "close" => Some(Action::Close),
        "toggle-maximize" => Some(Action::ToggleMaximize),
        "toggle-shade" => Some(Action::ToggleShade),
        "miniaturize" => Some(Action::Miniaturize),
        "toggle-fullscreen" => Some(Action::ToggleFullscreen),
        "workspace-next" => Some(Action::WorkspaceNext),
        "workspace-prev" => Some(Action::WorkspacePrev),
        "workspace-carry-next" => Some(Action::WorkspaceCarryNext),
        "workspace-carry-prev" => Some(Action::WorkspaceCarryPrev),
        "overview" => Some(Action::Overview),
        "reload" => Some(Action::Reload),
        "restart" => Some(Action::Restart),
        _ => None,
    }
}

/// The resolved configuration the binary runs with.
///
/// `keybindings` holds at most one entry per [`KeyCombo`] — the merge
/// in [`parse`] maintains that invariant — so the caller can grab each
/// combo exactly once without deduplicating. `scale` and `theme` stay
/// `Option` so the binary's precedence rules (env var over config over
/// built-in, persisted theme state over config) can distinguish an
/// absent setting from an explicit one. `placement` and
/// `edge_resistance` are plain values, not `Option`s, because nothing
/// outside the config file competes over them — "user said nothing"
/// and "user chose the default" are indistinguishable on purpose.
#[derive(Clone, Debug)]
pub struct Config {
    pub focus_follows_mouse: bool,
    pub scale: Option<f32>,
    pub theme: Option<String>,
    /// The session-wide light/dark appearance the desktop starts in:
    /// `"light"` or `"dark"`, validated at parse time so only those two
    /// spellings ever reach a caller. Kept an `Option<String>` for the
    /// same reason `theme` is: the shell's precedence rules (the
    /// published appearance state file over this value over the
    /// selected theme's own native mood) must be able to tell "user
    /// said nothing" apart from "user chose dark". The enum itself
    /// lives in `wm-theme`, which this crate deliberately does not
    /// depend on.
    pub appearance: Option<String>,
    /// Where newly mapped windows go when the client expressed no
    /// position preference. Fed to the WM's placement engine verbatim.
    pub placement: PlacementPolicy,
    /// Snap distance for interactive moves, in pixels: a dragged frame
    /// edge within this many pixels of a screen or window edge lands
    /// flush against it. `0` disables snapping entirely.
    pub edge_resistance: u32,
    /// Point size of the spawned terminal's font *at 1x*, which the UI
    /// scale then multiplies exactly as it multiplies the chrome — so
    /// this is one number for "how big is terminal text", independent
    /// of the display it lands on. Not per-theme on purpose: a theme
    /// restyles the terminal's colors, never its metrics.
    pub terminal_font_px: f32,
    /// `app_id` prefixes of clients that genuinely draw their own window
    /// chrome, and may therefore be taken at their word when they ask
    /// for client-side decorations.
    ///
    /// Everything else this desktop frames, whatever it asks for. That
    /// asymmetry is deliberate: asking for client-side decorations is
    /// not a promise to *draw* any, and the client that forced this list
    /// — a terminal configured `decorations = "None"` for a tiling
    /// desktop — draws none at all. Honouring it produced a window with
    /// chrome from neither side: nothing to drag, close or resize. Being
    /// wrong the other way costs a second titlebar, which is visible and
    /// fixed by adding an entry here; being wrong this way costs a
    /// window you cannot use at all.
    ///
    /// Matched as a case-insensitive prefix of the client's `app_id`.
    pub self_decorating_apps: Vec<String>,
    /// Relaunch the previous session's windows at startup, restoring
    /// each one's geometry, workspace and shape flags from the layout
    /// file the shell keeps. Off by default — a session that spawns
    /// applications the user did not just ask for has to be something
    /// the user opted into, not something an update turned on.
    pub restore_session: bool,
    /// A screen locker command line (e.g. `"swaylock"`), split on
    /// whitespace. Only consulted on the Wayland session, and only
    /// when the compositor comes back up after a crash: the watchdog
    /// re-execs a crashed compositor, and a desktop that reappears
    /// with the user away from the keyboard must reappear locked. No
    /// locker configured means the recovered session comes back
    /// unlocked, and the compositor says so in the log.
    pub lock_command: Option<String>,
    pub keybindings: Vec<(KeyCombo, Action)>,
}

/// Terminal font size when the config says nothing, in 1x pixels.
pub const DEFAULT_TERMINAL_FONT_PX: f32 = 20.0;

/// The clients known to draw their own chrome when they ask to.
/// Chromium and its rebrands draw a titlebar whatever the compositor
/// configures, which is the double titlebar this list prevents.
pub fn default_self_decorating_apps() -> Vec<String> {
    ["chrome", "chromium", "msedge", "microsoft-edge", "brave"].iter().map(|s| s.to_string()).collect()
}

/// The range a `terminal_font_px` value has to land in. Below the floor
/// the terminal is unreadable; above the ceiling a default-sized window
/// has room for almost no columns. Both ends are rejected loudly rather
/// than clamped silently, so a typo'd `2000` is reported instead of
/// quietly becoming something else.
const TERMINAL_FONT_PX_RANGE: std::ops::RangeInclusive<f32> = 6.0..=96.0;

impl Config {
    /// The configuration used when no file exists (and the base every
    /// file merges over). The default bindings deliberately mirror the
    /// NeXTSTEP-style alt+shift chords the rest of the WM was designed
    /// around; workspace switching sits on alt+ctrl so that carry
    /// (alt+shift) and plain switch differ by exactly one modifier.
    pub fn default_config() -> Config {
        // Routing the defaults through parse_key keeps them honest:
        // they exercise the exact same parser user specs go through, so
        // a parser regression fails the default-config test instead of
        // silently shipping different combos than the docs promise.
        fn bind(spec: &str, action: Action) -> (KeyCombo, Action) {
            let combo = parse_key(spec)
                .expect("default keybinding specs are constants and must always parse");
            (combo, action)
        }
        Config {
            focus_follows_mouse: false,
            scale: None,
            theme: None,
            appearance: None,
            // Smart is the classic default placement, and 10px
            // matches the stock edge-resistance feel: strong enough
            // to catch a deliberate drag toward an edge, weak enough
            // that sailing past it never feels sticky.
            placement: PlacementPolicy::Smart,
            edge_resistance: 10,
            terminal_font_px: DEFAULT_TERMINAL_FONT_PX,
            self_decorating_apps: default_self_decorating_apps(),
            restore_session: false,
            lock_command: None,
            keybindings: vec![
                bind("alt+shift+return", Action::SpawnTerminal),
                bind("alt+shift+q", Action::Close),
                bind("alt+shift+x", Action::ToggleMaximize),
                bind("alt+shift+s", Action::ToggleShade),
                bind("alt+shift+m", Action::Miniaturize),
                bind("alt+shift+f", Action::ToggleFullscreen),
                bind("alt+ctrl+right", Action::WorkspaceNext),
                bind("alt+ctrl+left", Action::WorkspacePrev),
                bind("alt+shift+right", Action::WorkspaceCarryNext),
                bind("alt+shift+left", Action::WorkspaceCarryPrev),
                // Super+Up "steps back" from the desk for the modal
                // Overview: the whole super row is otherwise free, and
                // the arrow pairs with the arrows that then drive the
                // selection inside it.
                bind("super+up", Action::Overview),
            ],
        }
    }
}

/// Maps a single (already lowercased, trimmed) key token to its X11
/// keysym. Only the tokens the WM documents — guessing at arbitrary
/// keysym names here would make config files silently non-portable
/// across whatever name table we happened to guess from.
fn keysym_for(token: &str) -> Option<u32> {
    // Single-character tokens: letters and digits carry their ASCII
    // value as keysym (X11 keeps Latin-1 keysyms identical to their
    // character codes).
    if token.len() == 1 {
        let b = token.as_bytes()[0];
        if b.is_ascii_lowercase() || b.is_ascii_digit() {
            return Some(b as u32);
        }
        return None;
    }
    let keysym = match token {
        // "enter" accepted alongside "return" because both names are in
        // common use and rejecting one would be a pointless papercut.
        "return" | "enter" => 0xff0d,
        "tab" => 0xff09,
        "space" => 0x20,
        "escape" => 0xff1b,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        // Punctuation gets word names ("minus", not "-") because "+" is
        // the spec separator and a literal "-"/"=" next to it reads
        // like a typo; word names keep specs unambiguous.
        "minus" => 0x2d,
        "equal" => 0x3d,
        "comma" => 0x2c,
        "period" => 0x2e,
        // Function keys spelled out rather than computed so exactly
        // f1..f12 exist — no accidental "f01"/"f13" acceptance.
        "f1" => 0xffbe,
        "f2" => 0xffbf,
        "f3" => 0xffc0,
        "f4" => 0xffc1,
        "f5" => 0xffc2,
        "f6" => 0xffc3,
        "f7" => 0xffc4,
        "f8" => 0xffc5,
        "f9" => 0xffc6,
        "f10" => 0xffc7,
        "f11" => 0xffc8,
        "f12" => 0xffc9,
        _ => return None,
    };
    Some(keysym)
}

/// Parses a `"alt+shift+return"`-style key spec into a [`KeyCombo`].
///
/// Case-insensitive; tokens are `+`-separated and whitespace around
/// each token is ignored. Modifier tokens: `alt`, `shift`, `ctrl` (or
/// `control`), `super` (or `mod4`, `win`). Exactly one non-modifier
/// key token is required and it must come last — a modifier *after*
/// the key (`"return+alt"`) is rejected rather than reordered, because
/// silently accepting it would also silently accept `"a+b"`-style
/// two-key typos as "b with some garbage".
///
/// Returns `None` (rather than an error type) because every caller —
/// the config merge, the defaults table — has the same reaction to a
/// bad spec: skip it and say which spec was bad; the spec string itself
/// is the only diagnostic worth carrying.
pub fn parse_key(spec: &str) -> Option<KeyCombo> {
    let mut modifiers = Modifiers::empty();
    let mut keysym: Option<u32> = None;
    for raw in spec.split('+') {
        // Once the key token has been seen, *any* further token —
        // second key, trailing modifier, trailing '+' — is malformed.
        if keysym.is_some() {
            return None;
        }
        let token = raw.trim().to_ascii_lowercase();
        match token.as_str() {
            "alt" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "super" | "mod4" | "win" => modifiers |= Modifiers::SUPER,
            // Not a modifier, so it must be the key token; unknown
            // names (and the empty token from "" / "alt+") fail here.
            other => keysym = Some(keysym_for(other)?),
        }
    }
    // Modifier-only specs ("alt+shift") fall through with no keysym.
    keysym.map(|keysym| KeyCombo { keysym, modifiers })
}

/// Applies a `[keybindings]` table on top of the current binding list,
/// entry by entry in document order (last spelling of a combo wins).
///
/// Every failure mode here is per-entry: a bad key spec, a non-string
/// value, or an unknown action name each warn and skip that one entry,
/// leaving the combo's default binding (if any) intact. Skipping —
/// rather than erroring, and rather than unbinding — is what makes a
/// typo cost the user one binding at most, never the file or a default
/// they still rely on.
fn apply_keybindings(bindings: &mut Vec<(KeyCombo, Action)>, table: &toml::Table) {
    for (spec, value) in table {
        let Some(combo) = parse_key(spec) else {
            tracing::warn!(key = %spec, "config: unparsable key spec in [keybindings], skipping entry");
            continue;
        };
        let toml::Value::String(name) = value else {
            tracing::warn!(
                key = %spec,
                value = ?value,
                "config: [keybindings] value must be an action name string, skipping entry"
            );
            continue;
        };
        // "none" means "this combo does nothing": drop any existing
        // binding (default or earlier file entry) for it.
        if name.trim().eq_ignore_ascii_case("none") {
            bindings.retain(|(existing, _)| *existing != combo);
            continue;
        }
        let Some(action) = action_from_name(name) else {
            tracing::warn!(
                key = %spec,
                action = %name,
                "config: unknown action name, skipping entry (any default binding for this combo is kept)"
            );
            continue;
        };
        // Replace-then-append keeps the one-entry-per-combo invariant
        // and gives "last occurrence in the file wins" for combos the
        // file spells more than once (case / alias variants).
        bindings.retain(|(existing, _)| *existing != combo);
        bindings.push((combo, action));
    }
}

/// Validates a `scale` value from the file. Integers are accepted
/// alongside floats because `scale = 2` is the obvious thing to type;
/// non-positive or non-finite values are rejected as nonsense that
/// would otherwise propagate NaN/zero into every geometry computation.
fn scale_from_value(value: &toml::Value) -> Option<f32> {
    let scale = match value {
        toml::Value::Float(f) => *f as f32,
        toml::Value::Integer(i) => *i as f32,
        _ => return None,
    };
    (scale.is_finite() && scale > 0.0).then_some(scale)
}

/// Validates a `terminal_font_px` value. Integers are accepted beside
/// floats for the same reason `scale` accepts them — `terminal_font_px
/// = 20` is the obvious thing to type — and the value must land inside
/// [`TERMINAL_FONT_PX_RANGE`].
fn terminal_font_px_from_value(value: &toml::Value) -> Option<f32> {
    let px = match value {
        toml::Value::Float(f) => *f as f32,
        toml::Value::Integer(i) => *i as f32,
        _ => return None,
    };
    (px.is_finite() && TERMINAL_FONT_PX_RANGE.contains(&px)).then_some(px)
}

/// Maps a `placement` value from the file to its policy. Only a string
/// is meaningful; matching is trimmed and case-insensitive for the
/// same reason action names are — `"Cascade"` should not silently cost
/// the user their placement preference.
fn placement_from_value(value: &toml::Value) -> Option<PlacementPolicy> {
    let toml::Value::String(name) = value else {
        return None;
    };
    match name.trim().to_ascii_lowercase().as_str() {
        "smart" => Some(PlacementPolicy::Smart),
        "cascade" => Some(PlacementPolicy::Cascade),
        "center" => Some(PlacementPolicy::Center),
        _ => None,
    }
}

/// Validates an `edge_resistance` value: a non-negative integer that
/// fits the `u32` the WM's snap threshold takes (zero included — it is
/// the documented way to disable snapping). Floats are rejected rather
/// than truncated: `edge_resistance = 7.5` misunderstands the unit
/// (whole pixels), and silently picking a rounding direction would
/// hide that from the user.
fn edge_resistance_from_value(value: &toml::Value) -> Option<u32> {
    let toml::Value::Integer(px) = value else {
        return None;
    };
    u32::try_from(*px).ok()
}

/// The pure core [`load`] wraps: parses config-file text into a
/// [`Config`], merging over [`Config::default_config`].
///
/// `Err` is reserved for text that is not valid TOML at all — the one
/// case where nothing can be salvaged. Everything below that (wrongly
/// typed fields, unknown keys, bad `[keybindings]` entries) degrades
/// per-item with a `tracing::warn!`, keeping the rest of the file. The
/// warnings go through `tracing` rather than being accumulated in the
/// return value so the function stays trivially callable from tests
/// and from `load` alike; without a subscriber they cost nothing.
pub fn parse(text: &str) -> Result<Config, String> {
    let table: toml::Table = text
        .parse()
        .map_err(|err: toml::de::Error| format!("invalid TOML: {err}"))?;
    let mut config = Config::default_config();
    for (key, value) in &table {
        match key.as_str() {
            "focus_follows_mouse" => match value {
                toml::Value::Boolean(b) => config.focus_follows_mouse = *b,
                other => tracing::warn!(
                    value = ?other,
                    "config: focus_follows_mouse must be a boolean, keeping default"
                ),
            },
            "scale" => match scale_from_value(value) {
                Some(scale) => config.scale = Some(scale),
                None => tracing::warn!(
                    value = ?value,
                    "config: scale must be a positive number, ignoring it"
                ),
            },
            "theme" => match value {
                toml::Value::String(name) => config.theme = Some(name.clone()),
                other => tracing::warn!(
                    value = ?other,
                    "config: theme must be a string, ignoring it"
                ),
            },
            // Trimmed and case-insensitive like every other name in
            // this file, and normalized here so consumers only ever
            // see the two canonical spellings. An unknown mood is
            // skipped, not guessed at: "drak" must cost the user this
            // one setting, never a mode they did not ask for.
            "appearance" => match value {
                toml::Value::String(name)
                    if matches!(name.trim().to_ascii_lowercase().as_str(), "light" | "dark") =>
                {
                    config.appearance = Some(name.trim().to_ascii_lowercase());
                }
                other => tracing::warn!(
                    value = ?other,
                    "config: appearance must be \"light\" or \"dark\", ignoring it"
                ),
            },
            "placement" => match placement_from_value(value) {
                Some(policy) => config.placement = policy,
                None => tracing::warn!(
                    value = ?value,
                    "config: placement must be \"smart\", \"cascade\", or \"center\", keeping default"
                ),
            },
            "edge_resistance" => match edge_resistance_from_value(value) {
                Some(px) => config.edge_resistance = px,
                None => tracing::warn!(
                    value = ?value,
                    "config: edge_resistance must be a non-negative integer, keeping default"
                ),
            },
            "terminal_font_px" => match terminal_font_px_from_value(value) {
                Some(px) => config.terminal_font_px = px,
                None => tracing::warn!(
                    value = ?value,
                    "config: terminal_font_px must be a number between 6 and 96, keeping default"
                ),
            },
            "restore_session" => match value {
                toml::Value::Boolean(b) => config.restore_session = *b,
                other => tracing::warn!(
                    value = ?other,
                    "config: restore_session must be a boolean, keeping default"
                ),
            },
            "lock_command" => match value {
                // An empty or whitespace-only command means the same
                // thing as no key at all: nothing to run. Filtering it
                // here keeps every consumer from having to guard
                // against spawning "".
                toml::Value::String(command) if !command.trim().is_empty() => {
                    config.lock_command = Some(command.clone());
                }
                toml::Value::String(_) => tracing::warn!(
                    "config: lock_command is empty, treating it as unset"
                ),
                other => tracing::warn!(
                    value = ?other,
                    "config: lock_command must be a command-line string, ignoring it"
                ),
            },
            "self_decorating_apps" => match value {
                toml::Value::Array(items) => {
                    config.self_decorating_apps = items
                        .iter()
                        .filter_map(|v| match v {
                            toml::Value::String(name) => Some(name.trim().to_ascii_lowercase()),
                            other => {
                                tracing::warn!(value = ?other, "config: self_decorating_apps entries must be strings, skipping one");
                                None
                            }
                        })
                        .filter(|name| !name.is_empty())
                        .collect();
                }
                other => tracing::warn!(
                    value = ?other,
                    "config: self_decorating_apps must be an array of strings, keeping the default list"
                ),
            },
            "keybindings" => match value {
                toml::Value::Table(entries) => apply_keybindings(&mut config.keybindings, entries),
                other => tracing::warn!(
                    value = ?other,
                    "config: [keybindings] must be a table, keeping default bindings"
                ),
            },
            unknown => tracing::warn!(
                key = %unknown,
                "config: unknown top-level key, ignoring it"
            ),
        }
    }
    Ok(config)
}

/// Where the config file lives: `$XDG_CONFIG_HOME/chonkstep/config.toml`
/// with the standard `~/.config` fallback. Per the XDG basedir spec, a
/// relative (or empty) `$XDG_CONFIG_HOME` is treated as unset rather
/// than resolved against some accidental working directory. `None`
/// only when `$HOME` is also missing — at which point there is nowhere
/// sane to look and the defaults are the right answer.
fn config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() && PathBuf::from(&dir).is_absolute() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("chonkstep").join("config.toml"))
}

/// Loads the user's config, never failing: this runs on the WM startup
/// path, and any outcome other than "the session starts" is worse than
/// any misreading of the config could be.
///
/// - No config file (or no `$HOME` to find one under): the defaults,
///   silently — an absent file is the normal case, not a problem.
/// - File unreadable or not valid TOML: `tracing::warn!` with the
///   error, then the defaults.
/// - File fine but individual entries bad: [`parse`] warns and skips
///   those entries, keeping the rest.
pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default_config();
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Config::default_config();
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "config: unreadable, using defaults");
            return Config::default_config();
        }
    };
    match parse(&text) {
        Ok(config) => config,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "config: using defaults");
            Config::default_config()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-side lookup: the action currently bound to `spec`, if any.
    fn action_for(config: &Config, spec: &str) -> Option<Action> {
        let combo = parse_key(spec).expect("test spec must parse");
        config
            .keybindings
            .iter()
            .find(|(existing, _)| *existing == combo)
            .map(|(_, action)| action.clone())
    }

    fn combo(keysym: u32, modifiers: Modifiers) -> KeyCombo {
        KeyCombo { keysym, modifiers }
    }

    // ---- parse_key: acceptance ----------------------------------------

    #[test]
    fn every_letter_and_digit_parses_to_its_ascii_keysym() {
        for c in ('a'..='z').chain('0'..='9') {
            let spec = c.to_string();
            assert_eq!(
                parse_key(&spec),
                Some(combo(c as u32, Modifiers::empty())),
                "spec {spec:?}"
            );
        }
    }

    #[test]
    fn every_named_key_parses_to_its_dictated_keysym() {
        let named: &[(&str, u32)] = &[
            ("return", 0xff0d),
            ("enter", 0xff0d),
            ("tab", 0xff09),
            ("space", 0x20),
            ("escape", 0xff1b),
            ("left", 0xff51),
            ("up", 0xff52),
            ("right", 0xff53),
            ("down", 0xff54),
            ("home", 0xff50),
            ("end", 0xff57),
            ("pageup", 0xff55),
            ("pagedown", 0xff56),
            ("minus", 0x2d),
            ("equal", 0x3d),
            ("comma", 0x2c),
            ("period", 0x2e),
        ];
        for (name, keysym) in named {
            assert_eq!(
                parse_key(name),
                Some(combo(*keysym, Modifiers::empty())),
                "spec {name:?}"
            );
        }
    }

    #[test]
    fn function_keys_f1_through_f12_parse_contiguously() {
        for n in 1u32..=12 {
            let spec = format!("f{n}");
            assert_eq!(
                parse_key(&spec),
                Some(combo(0xffbe + n - 1, Modifiers::empty())),
                "spec {spec:?}"
            );
        }
    }

    #[test]
    fn every_modifier_and_alias_maps_to_its_flag() {
        let cases: &[(&str, Modifiers)] = &[
            ("alt+a", Modifiers::ALT),
            ("shift+a", Modifiers::SHIFT),
            ("ctrl+a", Modifiers::CONTROL),
            ("control+a", Modifiers::CONTROL),
            ("super+a", Modifiers::SUPER),
            ("mod4+a", Modifiers::SUPER),
            ("win+a", Modifiers::SUPER),
        ];
        for (spec, modifiers) in cases {
            assert_eq!(parse_key(spec), Some(combo(0x61, *modifiers)), "spec {spec:?}");
        }
    }

    #[test]
    fn all_four_modifiers_combine() {
        assert_eq!(
            parse_key("alt+shift+ctrl+super+z"),
            Some(combo(
                0x7a,
                Modifiers::ALT | Modifiers::SHIFT | Modifiers::CONTROL | Modifiers::SUPER
            ))
        );
    }

    #[test]
    fn parse_key_is_case_insensitive() {
        let lower = parse_key("alt+shift+return");
        assert_eq!(parse_key("ALT+SHIFT+RETURN"), lower);
        assert_eq!(parse_key("Alt+Shift+Return"), lower);
        assert_eq!(parse_key("CTRL+F5"), parse_key("ctrl+f5"));
    }

    #[test]
    fn parse_key_tolerates_whitespace_around_tokens() {
        assert_eq!(parse_key(" alt + shift + a "), parse_key("alt+shift+a"));
    }

    #[test]
    fn bare_key_without_modifiers_is_valid() {
        assert_eq!(parse_key("f5"), Some(combo(0xffc2, Modifiers::empty())));
        assert_eq!(parse_key("space"), Some(combo(0x20, Modifiers::empty())));
    }

    // ---- parse_key: rejection -----------------------------------------

    #[test]
    fn rejects_empty_and_whitespace_and_bare_separator_specs() {
        for spec in ["", "   ", "+", "alt+", "+a", "alt++a"] {
            assert_eq!(parse_key(spec), None, "spec {spec:?}");
        }
    }

    #[test]
    fn rejects_modifier_only_specs() {
        for spec in ["alt", "shift", "ctrl", "super", "alt+shift", "alt+ctrl+shift"] {
            assert_eq!(parse_key(spec), None, "spec {spec:?}");
        }
    }

    #[test]
    fn rejects_unknown_tokens() {
        for spec in ["banana", "alt+foo", "hyper+a", "alt+esc", "alt+f0", "alt+f13", "alt+f01"] {
            assert_eq!(parse_key(spec), None, "spec {spec:?}");
        }
    }

    #[test]
    fn rejects_uppercase_only_single_chars_that_are_not_keys() {
        // Case-insensitivity lowercases first, so "A" is fine — but a
        // genuinely non-key single char is not.
        assert!(parse_key("A").is_some());
        assert_eq!(parse_key("-"), None);
        assert_eq!(parse_key("="), None);
    }

    #[test]
    fn rejects_duplicate_or_multiple_key_tokens() {
        for spec in ["a+a", "alt+a+b", "return+return", "alt+space+space"] {
            assert_eq!(parse_key(spec), None, "spec {spec:?}");
        }
    }

    #[test]
    fn rejects_anything_after_the_key_token() {
        for spec in ["return+alt", "alt+a+shift", "a+"] {
            assert_eq!(parse_key(spec), None, "spec {spec:?}");
        }
    }

    // ---- default_config -----------------------------------------------

    #[test]
    fn default_config_contains_exactly_the_dictated_bindings_in_order() {
        let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
        let alt_ctrl = Modifiers::ALT | Modifiers::CONTROL;
        // Expected combos written out with literal keysyms (not via
        // parse_key) so this test cannot be fooled by a parser bug.
        let expected = vec![
            (combo(0xff0d, alt_shift), Action::SpawnTerminal),
            (combo(0x71, alt_shift), Action::Close),
            (combo(0x78, alt_shift), Action::ToggleMaximize),
            (combo(0x73, alt_shift), Action::ToggleShade),
            (combo(0x6d, alt_shift), Action::Miniaturize),
            (combo(0x66, alt_shift), Action::ToggleFullscreen),
            (combo(0xff53, alt_ctrl), Action::WorkspaceNext),
            (combo(0xff51, alt_ctrl), Action::WorkspacePrev),
            (combo(0xff53, alt_shift), Action::WorkspaceCarryNext),
            (combo(0xff51, alt_shift), Action::WorkspaceCarryPrev),
            (combo(0xff52, Modifiers::SUPER), Action::Overview),
        ];
        let config = Config::default_config();
        assert_eq!(config.keybindings, expected);
        assert!(!config.focus_follows_mouse);
        assert_eq!(config.scale, None);
        assert_eq!(config.theme, None);
        assert_eq!(config.placement, PlacementPolicy::Smart);
        assert_eq!(config.edge_resistance, 10);
    }

    // ---- parse: whole-file behavior -----------------------------------

    #[test]
    fn empty_text_yields_the_defaults() {
        let config = parse("").expect("empty text is valid TOML");
        let defaults = Config::default_config();
        assert_eq!(config.focus_follows_mouse, defaults.focus_follows_mouse);
        assert_eq!(config.scale, defaults.scale);
        assert_eq!(config.theme, defaults.theme);
        assert_eq!(config.placement, defaults.placement);
        assert_eq!(config.edge_resistance, defaults.edge_resistance);
        assert_eq!(config.keybindings, defaults.keybindings);
    }

    #[test]
    fn invalid_toml_is_a_hard_error_with_a_message() {
        let err = parse("this = = is not toml").unwrap_err();
        assert!(err.contains("invalid TOML"), "message was: {err}");
        assert!(parse("[keybindings").is_err());
    }

    #[test]
    fn realistic_full_config_round_trips() {
        let text = r#"
            focus_follows_mouse = true
            scale = 1.5
            theme = "nextstep-classic"
            placement = "cascade"
            edge_resistance = 4

            [keybindings]
            "alt+shift+return" = "spawn-terminal"
            "super+t" = "spawn-terminal"
            "alt+shift+q" = "none"
            "alt+ctrl+right" = "workspace-next"
            "super+f11" = "toggle-fullscreen"
        "#;
        let config = parse(text).unwrap();
        assert!(config.focus_follows_mouse);
        assert_eq!(config.scale, Some(1.5));
        assert_eq!(config.theme.as_deref(), Some("nextstep-classic"));
        assert_eq!(config.placement, PlacementPolicy::Cascade);
        assert_eq!(config.edge_resistance, 4);
        // Restating a default is harmless; a new binding is added; the
        // unbound default is gone; everything unlisted survives.
        assert_eq!(action_for(&config, "alt+shift+return"), Some(Action::SpawnTerminal));
        assert_eq!(action_for(&config, "super+t"), Some(Action::SpawnTerminal));
        assert_eq!(action_for(&config, "alt+shift+q"), None);
        assert_eq!(action_for(&config, "alt+ctrl+right"), Some(Action::WorkspaceNext));
        assert_eq!(action_for(&config, "super+f11"), Some(Action::ToggleFullscreen));
        assert_eq!(action_for(&config, "alt+shift+x"), Some(Action::ToggleMaximize));
        assert_eq!(action_for(&config, "alt+shift+left"), Some(Action::WorkspaceCarryPrev));
        // 11 defaults - 1 unbound + 2 new = 12.
        assert_eq!(config.keybindings.len(), 12);
    }

    #[test]
    fn every_action_name_maps_to_its_variant() {
        let names: &[(&str, Action)] = &[
            ("spawn-terminal", Action::SpawnTerminal),
            ("close", Action::Close),
            ("toggle-maximize", Action::ToggleMaximize),
            ("toggle-shade", Action::ToggleShade),
            ("miniaturize", Action::Miniaturize),
            ("toggle-fullscreen", Action::ToggleFullscreen),
            ("workspace-next", Action::WorkspaceNext),
            ("workspace-prev", Action::WorkspacePrev),
            ("workspace-carry-next", Action::WorkspaceCarryNext),
            ("workspace-carry-prev", Action::WorkspaceCarryPrev),
            ("overview", Action::Overview),
            ("restart", Action::Restart),
        ];
        let mut text = String::from("[keybindings]\n");
        for (n, (name, _)) in names.iter().enumerate() {
            text.push_str(&format!("\"super+f{}\" = \"{}\"\n", n + 1, name));
        }
        let config = parse(&text).unwrap();
        for (n, (name, action)) in names.iter().enumerate() {
            let spec = format!("super+f{}", n + 1);
            assert_eq!(action_for(&config, &spec).as_ref(), Some(action), "action {name:?}");
        }
    }

    #[test]
    fn action_names_are_case_insensitive() {
        let text = "[keybindings]\n\"super+a\" = \"CLOSE\"\n\"super+b\" = \"Spawn-Terminal\"\n";
        let config = parse(text).unwrap();
        assert_eq!(action_for(&config, "super+a"), Some(Action::Close));
        assert_eq!(action_for(&config, "super+b"), Some(Action::SpawnTerminal));
    }

    // ---- parse: merge semantics ---------------------------------------

    #[test]
    fn user_entry_overrides_only_that_combo() {
        let config = parse("[keybindings]\n\"alt+shift+x\" = \"close\"\n").unwrap();
        assert_eq!(action_for(&config, "alt+shift+x"), Some(Action::Close));
        // Every other default is untouched, and no entry was duplicated.
        assert_eq!(action_for(&config, "alt+shift+q"), Some(Action::Close));
        assert_eq!(config.keybindings.len(), 11);
    }

    #[test]
    fn none_unbinds_a_default() {
        let config = parse("[keybindings]\n\"alt+shift+q\" = \"none\"\n").unwrap();
        assert_eq!(action_for(&config, "alt+shift+q"), None);
        assert_eq!(config.keybindings.len(), 10);
        assert!(!config.keybindings.iter().any(|(_, a)| *a == Action::Close));
    }

    #[test]
    fn none_spelled_differently_still_unbinds() {
        // The combo is matched semantically, not textually: unbinding
        // through an alias/case variant of the default's spelling works.
        let config = parse("[keybindings]\n\"ALT+CONTROL+RIGHT\" = \"None\"\n").unwrap();
        assert_eq!(action_for(&config, "alt+ctrl+right"), None);
        assert_eq!(config.keybindings.len(), 10);
    }

    #[test]
    fn none_on_an_unbound_combo_is_a_harmless_noop() {
        let config = parse("[keybindings]\n\"super+z\" = \"none\"\n").unwrap();
        assert_eq!(config.keybindings, Config::default_config().keybindings);
    }

    #[test]
    fn last_occurrence_of_a_combo_in_the_file_wins() {
        // TOML forbids literally identical duplicate keys, but the same
        // combo can appear under different spellings; document order
        // decides.
        let unbind_then_bind = "[keybindings]\n\
            \"alt+ctrl+right\" = \"none\"\n\
            \"alt+control+right\" = \"spawn-terminal\"\n";
        let config = parse(unbind_then_bind).unwrap();
        assert_eq!(action_for(&config, "alt+ctrl+right"), Some(Action::SpawnTerminal));

        let bind_then_unbind = "[keybindings]\n\
            \"alt+control+right\" = \"spawn-terminal\"\n\
            \"ALT+CTRL+RIGHT\" = \"none\"\n";
        let config = parse(bind_then_unbind).unwrap();
        assert_eq!(action_for(&config, "alt+ctrl+right"), None);
    }

    #[test]
    fn merged_bindings_keep_one_entry_per_combo() {
        let text = "[keybindings]\n\
            \"alt+shift+q\" = \"restart\"\n\
            \"ALT+SHIFT+Q\" = \"miniaturize\"\n";
        let config = parse(text).unwrap();
        let q = parse_key("alt+shift+q").unwrap();
        let entries: Vec<_> = config.keybindings.iter().filter(|(c, _)| *c == q).collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, Action::Miniaturize);
    }

    // ---- parse: per-entry graceful degradation ------------------------

    #[test]
    fn one_bad_entry_does_not_discard_the_good_ones() {
        let text = r#"
            [keybindings]
            "alt+shift+banana" = "close"
            "super+t" = "spawn-terminal"
            "super+u" = "launch-missiles"
            "super+v" = 17
            "alt+shift+q" = "none"
        "#;
        let config = parse(text).unwrap();
        // The good entries applied...
        assert_eq!(action_for(&config, "super+t"), Some(Action::SpawnTerminal));
        assert_eq!(action_for(&config, "alt+shift+q"), None);
        // ...the bad ones were skipped without binding anything...
        assert_eq!(action_for(&config, "super+u"), None);
        assert_eq!(action_for(&config, "super+v"), None);
        // ...and the untouched defaults survived: 11 - 1 + 1 = 11.
        assert_eq!(config.keybindings.len(), 11);
        assert_eq!(action_for(&config, "alt+shift+x"), Some(Action::ToggleMaximize));
    }

    #[test]
    fn unknown_action_name_keeps_that_combos_default() {
        // A typo'd action on a default combo must not unbind the default.
        let config = parse("[keybindings]\n\"alt+shift+q\" = \"cloes\"\n").unwrap();
        assert_eq!(action_for(&config, "alt+shift+q"), Some(Action::Close));
    }

    #[test]
    fn wrongly_typed_top_level_fields_keep_their_defaults() {
        let text = r#"
            focus_follows_mouse = "yes"
            scale = "big"
            theme = 3
            placement = 7
            edge_resistance = "sticky"
        "#;
        let config = parse(text).unwrap();
        assert!(!config.focus_follows_mouse);
        assert_eq!(config.scale, None);
        assert_eq!(config.theme, None);
        assert_eq!(config.placement, PlacementPolicy::Smart);
        assert_eq!(config.edge_resistance, 10);
        assert_eq!(config.keybindings.len(), 11);
    }

    #[test]
    fn nonsensical_scale_values_are_ignored() {
        for text in ["scale = 0.0", "scale = -1.5", "scale = -2", "scale = nan"] {
            let config = parse(text).unwrap();
            assert_eq!(config.scale, None, "text {text:?}");
        }
    }

    #[test]
    fn integer_scale_is_accepted() {
        assert_eq!(parse("scale = 2").unwrap().scale, Some(2.0));
    }

    // ---- parse: placement ---------------------------------------------

    #[test]
    fn every_placement_name_maps_to_its_policy() {
        let cases: &[(&str, PlacementPolicy)] = &[
            ("smart", PlacementPolicy::Smart),
            ("cascade", PlacementPolicy::Cascade),
            ("center", PlacementPolicy::Center),
        ];
        for (name, policy) in cases {
            let config = parse(&format!("placement = \"{name}\"")).unwrap();
            assert_eq!(config.placement, *policy, "name {name:?}");
        }
    }

    #[test]
    fn placement_is_case_insensitive_and_trimmed() {
        for text in [
            "placement = \"Cascade\"",
            "placement = \"CASCADE\"",
            "placement = \"cAsCaDe\"",
            "placement = \" cascade \"",
        ] {
            let config = parse(text).unwrap();
            assert_eq!(config.placement, PlacementPolicy::Cascade, "text {text:?}");
        }
    }

    #[test]
    fn unknown_or_wrongly_typed_placement_keeps_the_default() {
        for text in [
            // Unknown names: no prefix matching, no guessing.
            "placement = \"random\"",
            "placement = \"smartest\"",
            "placement = \"\"",
            // Wrong types entirely.
            "placement = 3",
            "placement = true",
            "placement = [\"smart\"]",
        ] {
            let config = parse(text).unwrap();
            assert_eq!(config.placement, PlacementPolicy::Smart, "text {text:?}");
        }
    }

    // ---- parse: edge_resistance ---------------------------------------

    #[test]
    fn edge_resistance_accepts_any_non_negative_integer() {
        let cases: &[(&str, u32)] = &[
            // Zero is valid, not a degenerate case: it is the documented
            // way to turn move-drag edge snapping off.
            ("edge_resistance = 0", 0),
            ("edge_resistance = 1", 1),
            ("edge_resistance = 32", 32),
            ("edge_resistance = 4294967295", u32::MAX),
        ];
        for (text, expected) in cases {
            let config = parse(text).unwrap();
            assert_eq!(config.edge_resistance, *expected, "text {text:?}");
        }
    }

    #[test]
    fn invalid_edge_resistance_keeps_the_default() {
        for text in [
            "edge_resistance = -1",
            "edge_resistance = -10",
            // One past u32::MAX: out of range, not silently clamped.
            "edge_resistance = 4294967296",
            // Fractional pixels are a unit misunderstanding, and even a
            // whole-valued float is the wrong type — rejecting both
            // keeps "integer" an honest description of the setting.
            "edge_resistance = 1.5",
            "edge_resistance = 10.0",
            "edge_resistance = \"10\"",
            "edge_resistance = true",
        ] {
            let config = parse(text).unwrap();
            assert_eq!(config.edge_resistance, 10, "text {text:?}");
        }
    }

    #[test]
    fn bad_placement_and_edge_resistance_do_not_cost_the_rest_of_the_file() {
        // Both new settings bad, everything else good: the per-entry
        // fallback must be surgical, exactly like [keybindings]'.
        let text = r#"
            placement = "diagonal"
            edge_resistance = -3
            focus_follows_mouse = true
            theme = "graphite"

            [keybindings]
            "super+t" = "spawn-terminal"
        "#;
        let config = parse(text).unwrap();
        assert_eq!(config.placement, PlacementPolicy::Smart);
        assert_eq!(config.edge_resistance, 10);
        assert!(config.focus_follows_mouse);
        assert_eq!(config.theme.as_deref(), Some("graphite"));
        assert_eq!(action_for(&config, "super+t"), Some(Action::SpawnTerminal));
    }

    #[test]
    fn keybindings_of_the_wrong_type_keeps_the_defaults() {
        let config = parse("keybindings = \"oops\"").unwrap();
        assert_eq!(config.keybindings, Config::default_config().keybindings);
    }

    #[test]
    fn unknown_top_level_keys_are_ignored_not_fatal() {
        let text = "focus_follow_mouse = true\ntheme = \"nextstep-classic\"\n";
        let config = parse(text).unwrap();
        // The typo'd key changed nothing; the valid key still applied.
        assert!(!config.focus_follows_mouse);
        assert_eq!(config.theme.as_deref(), Some("nextstep-classic"));
    }

    // ---- parse: appearance --------------------------------------------

    #[test]
    fn appearance_defaults_unset_and_parses_both_moods() {
        assert_eq!(Config::default_config().appearance, None);
        assert_eq!(parse("").unwrap().appearance, None);
        assert_eq!(parse("appearance = \"dark\"").unwrap().appearance.as_deref(), Some("dark"));
        assert_eq!(parse("appearance = \"light\"").unwrap().appearance.as_deref(), Some("light"));
    }

    #[test]
    fn appearance_is_trimmed_and_case_insensitive_and_normalized() {
        for text in ["appearance = \"Dark\"", "appearance = \"DARK\"", "appearance = \" dark \""] {
            assert_eq!(parse(text).unwrap().appearance.as_deref(), Some("dark"), "text {text:?}");
        }
    }

    #[test]
    fn unknown_or_wrongly_typed_appearance_stays_unset() {
        for text in [
            "appearance = \"dusk\"",
            "appearance = \"auto\"",
            "appearance = \"\"",
            "appearance = 1",
            "appearance = true",
        ] {
            assert_eq!(parse(text).unwrap().appearance, None, "text {text:?}");
        }
    }

    // ---- parse: restore_session and lock_command ----------------------

    #[test]
    fn restore_session_defaults_off_and_parses_as_a_boolean() {
        // Off by default: relaunching applications the user did not
        // just ask for must be an explicit opt-in.
        assert!(!Config::default_config().restore_session);
        assert!(parse("restore_session = true").unwrap().restore_session);
        assert!(!parse("restore_session = false").unwrap().restore_session);
    }

    #[test]
    fn wrongly_typed_restore_session_keeps_the_default() {
        for text in ["restore_session = \"yes\"", "restore_session = 1"] {
            assert!(!parse(text).unwrap().restore_session, "text {text:?}");
        }
    }

    #[test]
    fn lock_command_defaults_unset_and_parses_as_a_string() {
        assert_eq!(Config::default_config().lock_command, None);
        assert_eq!(
            parse("lock_command = \"swaylock -f -c 000000\"").unwrap().lock_command.as_deref(),
            Some("swaylock -f -c 000000")
        );
    }

    #[test]
    fn empty_or_wrongly_typed_lock_command_stays_unset() {
        // An empty command is indistinguishable in effect from no key,
        // and normalizing it here means no consumer ever spawns "".
        for text in ["lock_command = \"\"", "lock_command = \"   \"", "lock_command = 3", "lock_command = true"] {
            assert_eq!(parse(text).unwrap().lock_command, None, "text {text:?}");
        }
    }

    // ---- load ---------------------------------------------------------

    /// One test covers every load() path because they all mutate
    /// process-global environment variables; splitting them into
    /// parallel test threads would race.
    #[test]
    fn load_reads_the_xdg_path_and_never_fails() {
        let saved_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let saved_home = std::env::var_os("HOME");
        let scratch = std::env::temp_dir().join(format!("wm-config-load-test-{}", std::process::id()));
        let config_dir = scratch.join("chonkstep");
        std::fs::create_dir_all(&config_dir).unwrap();
        let config_file = config_dir.join("config.toml");

        // Missing file: silent defaults.
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        assert_eq!(load().keybindings, Config::default_config().keybindings);

        // Valid file: its contents win.
        std::fs::write(&config_file, "theme = \"test-theme\"\nscale = 2.0\n").unwrap();
        let config = load();
        assert_eq!(config.theme.as_deref(), Some("test-theme"));
        assert_eq!(config.scale, Some(2.0));

        // Garbage file: warn (unobserved here) and fall back to defaults.
        std::fs::write(&config_file, "!!! not toml at all [[[").unwrap();
        let config = load();
        assert_eq!(config.theme, None);
        assert_eq!(config.keybindings, Config::default_config().keybindings);

        // HOME fallback: with XDG_CONFIG_HOME unset, ~/.config is used.
        let home = scratch.join("home");
        let home_config_dir = home.join(".config").join("chonkstep");
        std::fs::create_dir_all(&home_config_dir).unwrap();
        std::fs::write(home_config_dir.join("config.toml"), "theme = \"from-home\"\n").unwrap();
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("HOME", &home);
        assert_eq!(load().theme.as_deref(), Some("from-home"));

        // A relative XDG_CONFIG_HOME is invalid per the basedir spec and
        // must fall back to ~/.config rather than resolve against cwd.
        std::env::set_var("XDG_CONFIG_HOME", "relative/config");
        assert_eq!(load().theme.as_deref(), Some("from-home"));

        match saved_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
