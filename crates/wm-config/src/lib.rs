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
//! terminal_font_px = 18              # optional; terminal font size at 1x
//! drag_modifier = "alt"              # optional; move/resize drag modifier, or "none"
//! restore_session = true             # optional; relaunch last session's windows
//! lock_command = "swaylock"          # optional; locker for post-crash recovery
//! omarchy_menu = true                # optional; Omarchy's menu under right-click
//! omarchy_shell = true               # optional; host Omarchy's shell (bar, panels, OSD)
//! terminal = "alacritty"             # optional; the terminal the shell spawns (string or argv)
//! autostart = [["udiskie", "--automount"]]  # optional; run once, in order, on a fresh session
//!
//! [commands]                         # optional; named argv lists for `run <name>`
//! menu = "omarchy-menu toggle"       # a string is split on whitespace; an array is literal
//!
//! [decorations]                      # optional; per-application overrides
//! server_side = ["bare.kde.app"]     # frame a client whose own chrome never shows up
//! client_side = ["borderless-game"]  # let an xdg client stay bare
//!
//! [keybindings]
//! "alt+shift+return" = "spawn-terminal"
//! "super+t" = "spawn-terminal"       # extra binding for the same action
//! "super+space" = "run menu"         # a `[commands]` entry, by name
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

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use wm_core::FocusPolicy;
pub use wm_core::DecorationRules;
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
    /// Open the window commands menu for the focused window, at the
    /// keyboard rather than by right-clicking a titlebar.
    ///
    /// The titlebar is not always there to right-click: a client that
    /// negotiated client-side decorations has no chrome of ours at all,
    /// and before this verb existed the only route to its commands was
    /// the Overview. Window Maker's `Control+Escape`, and its inspector
    /// says why it exists: "To access the window commands menu of a
    /// window without its titlebar, press Control+Esc."
    WindowMenu,
    /// Re-read this file and apply it to the running session — theme,
    /// UI scale, focus policy, placement, edge resistance and these
    /// very bindings, with no restart and nothing closed.
    Reload,
    /// Re-exec the session's on-disk binary. Distinct from [`Self::Reload`]
    /// on purpose: reloading applies a changed *config*, restarting
    /// applies a changed *build*, and only the second one has to cost
    /// the user anything.
    Restart,
    /// Run the argv named by this string in the `[commands]` table.
    ///
    /// The verb set above is closed because the WM owns its own
    /// semantics, and that rule still holds: this variant carries a
    /// *name*, never a command line. Bindings stay a closed vocabulary
    /// the parser can validate; the argv lives in one table where it is
    /// declared once, checked once, and can be reported on by name when
    /// something is wrong with it.
    ///
    /// The distinction is not pedantry. It is what lets an unknown
    /// command in a binding be diagnosed as "no such command" at parse
    /// time — before any key is ever pressed — instead of failing
    /// silently at spawn time with no line number to blame. A binding
    /// naming a command that does not exist is dropped by [`parse`],
    /// exactly as an unparsable key spec is.
    ///
    /// It exists because a desktop that cannot launch anything the user
    /// names cannot host another desktop's tooling. Omarchy publishes
    /// 382 commands through one router; without this verb not one of
    /// them can reach a key.
    Run(String),
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
        "window-menu" => Some(Action::WindowMenu),
        "reload" => Some(Action::Reload),
        "restart" => Some(Action::Restart),
        // `run <name>` is the one action name that carries an argument,
        // and the argument is a key into `[commands]` — not a command
        // line. Everything after the verb is taken whole, including
        // inner whitespace, so a command may be named "lock screen" if
        // its author prefers that to "lock-screen"; only the ends are
        // trimmed. An empty name is rejected here rather than becoming
        // a lookup for "" that could never match anything.
        //
        // The name arrives already lowercased by the caller, and
        // `[commands]` lowercases its keys for the same reason, so
        // `run Lock` and a `Lock = ...` entry find each other. Command
        // names are case-insensitive like every other name in this
        // file; nothing is gained by making capitalization a silent
        // way to lose a binding.
        rest => {
            let name = rest.strip_prefix("run ")?.trim();
            (!name.is_empty()).then(|| Action::Run(name.to_string()))
        }
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
    /// Per-application decoration overrides, in both directions. Empty
    /// by default: the compositor concludes every xdg-decoration
    /// negotiation with its own chrome and believes the KDE protocol's
    /// declarations, which is right for every client observed, and a
    /// list is the exception, not the mechanism.
    pub decorations: DecorationRules,
    /// The modifier that turns a drag anywhere on a window into a move
    /// (left button) or a resize (right button), the way every stacking
    /// window manager since the eighties has offered.
    ///
    /// This is the floor under the whole decoration policy, not a
    /// convenience: a window whose client asked to draw its own chrome
    /// and then drew none has no titlebar to grab, and without this
    /// gesture it cannot be moved or resized at all. Window Maker binds
    /// it to Alt and grabs on the *client* window for exactly that
    /// reason; KWin moved to Meta in Plasma 5.20 and labwc to Super in
    /// 0.9.0, because Alt collides with CAD and creative applications.
    /// Alt is the default here to match the ancestor this desktop
    /// imitates; `drag_modifier = "super"` picks the modern convention,
    /// and `"none"` disables the gesture.
    pub drag_modifier: Option<Modifiers>,
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
    /// Named argv lists a binding can reach through [`Action::Run`],
    /// keyed by lowercased name. Empty by default: the desktop's own
    /// verbs need no entry here, and this table exists for the things
    /// only the user knows the name of.
    ///
    /// A `BTreeMap` rather than a `Vec` of pairs because the only
    /// operation is lookup by name, and because a stable order makes
    /// the "no such command" diagnostic list candidates the same way
    /// twice.
    pub commands: BTreeMap<String, Vec<String>>,
    /// The terminal emulator `spawn-terminal` launches, as argv.
    ///
    /// `None` means the built-in default, which is the only terminal
    /// this desktop can theme end-to-end — the palette is passed on its
    /// command line, so a replacement gets the desktop's colors only if
    /// it happens to read them from somewhere else. That tradeoff is
    /// the user's to make: a session hosting another desktop's tooling
    /// has to be able to use that desktop's terminal.
    pub terminal: Option<Vec<String>>,
    /// Commands to run once, in order, when the session comes up.
    ///
    /// Named argv like [`Self::commands`] but a list rather than a map:
    /// these are not addressed by name, they are simply run, and the
    /// order they appear in the file is the order they start in. Empty
    /// by default — a desktop that launches processes the user did not
    /// ask for is the thing `restore_session` is deliberately opt-in to
    /// avoid, and this is the same rule.
    pub autostart: Vec<Vec<String>>,
    /// Whether the root menu carries an `Omarchy` submenu mirroring
    /// Omarchy's own JSONC-defined command menu (see
    /// `chonk_shell::omarchy_menu`). On by default, and inert on a
    /// machine without Omarchy — the submenu only appears when the menu
    /// definition file exists — so the key is there to turn the
    /// integration *off* on a machine that has Omarchy but wants a
    /// plain chonkstep root menu.
    pub omarchy_menu: bool,
    /// Whether a Wayland session starts Omarchy's shell — the Quickshell
    /// process behind its bar, menus, panels, notifications, OSD and
    /// lock screen — the way Omarchy's own Hyprland configuration does
    /// (see `chonk_shell::omarchy_shell`). On by default, and inert on
    /// a machine without Omarchy's shell or on the X11 stack, where
    /// Quickshell cannot run. Most rows of the Omarchy menu are only
    /// half a feature without it: a speed test, a theme picker or a
    /// volume key each ends in a panel that shell draws.
    pub omarchy_shell: bool,
    pub keybindings: Vec<(KeyCombo, Action)>,
}

/// Terminal font size when the config says nothing, in 1x pixels.
pub const DEFAULT_TERMINAL_FONT_PX: f32 = 18.0;

/// The modifier the move/resize drag gesture rides on when the config
/// says nothing: Alt, as Window Maker has always bound it.
pub const DEFAULT_DRAG_MODIFIER: Modifiers = Modifiers::ALT;

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
            // Deliberately empty, in both directions. The `client_side`
            // list this desktop once decided *from* held the Chromium
            // family, and it was wrong in both directions within a day:
            // it matched `chrome-<host>-<profile>` (a `--app` window,
            // which asks for *server*-side decorations and draws none)
            // while missing `google-chrome` (the browser window, which
            // asks for client-side and draws its own). A `server_side`
            // list naming Omarchy's terminals lasted about as long: the
            // `org.omarchy.*` classes are an open set, one per script.
            // The compositor answering every xdg negotiation with its
            // own chrome gets all of them right with no list at all
            // (see `wm-wayland`'s `decoration` module).
            decorations: DecorationRules::default(),
            drag_modifier: Some(DEFAULT_DRAG_MODIFIER),
            restore_session: false,
            lock_command: None,
            commands: BTreeMap::new(),
            terminal: None,
            autostart: Vec::new(),
            omarchy_menu: true,
            omarchy_shell: true,
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
                // The window commands menu, reachable without a
                // titlebar to right-click. Window Maker binds exactly
                // this, on exactly this key, and documents it as the
                // way into a window with `NoTitlebar` — the escape
                // hatch that makes honoring a client's request to
                // decorate itself a safe policy rather than a gamble.
                bind("control+escape", Action::WindowMenu),
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
        // The XF86 block: the keys with pictures on them rather than
        // letters. Spelled here in the same run-together style as
        // "pageup" rather than as their X11 names, because
        // `XF86AudioRaiseVolume` in a config file is shouting a
        // vendor prefix at someone who just wants the volume key.
        //
        // These exist because a desktop cannot host another desktop's
        // tooling without them. Every volume, brightness and media
        // binding any Linux desktop ships lands on one of these
        // keysyms, and until now this parser had no name for a single
        // one of them — a laptop's volume keys were not merely
        // unbound, they were unbindable.
        "volumeup" => 0x1008ff13,
        "volumedown" => 0x1008ff11,
        "volumemute" | "mute" => 0x1008ff12,
        "micmute" => 0x1008ffb2,
        "playpause" | "audioplay" => 0x1008ff14,
        "audiopause" => 0x1008ff31,
        "audiostop" => 0x1008ff15,
        "audionext" => 0x1008ff17,
        "audioprev" => 0x1008ff16,
        "brightnessup" => 0x1008ff02,
        "brightnessdown" => 0x1008ff03,
        "kbdbrightnessup" => 0x1008ff05,
        "kbdbrightnessdown" => 0x1008ff06,
        "poweroff" => 0x1008ff2a,
        "search" => 0x1008ff1b,
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
            "omarchy_menu" => match value {
                toml::Value::Boolean(b) => config.omarchy_menu = *b,
                other => tracing::warn!(
                    value = ?other,
                    "config: omarchy_menu must be a boolean, keeping default"
                ),
            },
            "omarchy_shell" => match value {
                toml::Value::Boolean(b) => config.omarchy_shell = *b,
                other => tracing::warn!(
                    value = ?other,
                    "config: omarchy_shell must be a boolean, keeping default"
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
            // The one-directional ancestor of `[decorations]`, kept
            // reading so an existing config file does not start
            // warning about an unknown key and silently lose the
            // override it was written for. It only ever meant "do not
            // frame these", which is exactly `decorations.client_side`.
            "self_decorating_apps" => match value {
                toml::Value::Array(_) => {
                    tracing::warn!(
                        "config: self_decorating_apps is now decorations.client_side — reading it as that; \
                         see docs/config.example.toml for the [decorations] table, which also forces chrome ON"
                    );
                    config.decorations.client_side = string_list(value, "self_decorating_apps");
                }
                other => tracing::warn!(
                    value = ?other,
                    "config: self_decorating_apps must be an array of strings, ignoring it"
                ),
            },
            "drag_modifier" => match value {
                toml::Value::String(name) => match drag_modifier_from_name(name) {
                    Some(mods) => config.drag_modifier = mods,
                    None => tracing::warn!(
                        value = %name,
                        "config: drag_modifier must be \"alt\", \"super\", \"control\" or \"none\", keeping default"
                    ),
                },
                other => tracing::warn!(
                    value = ?other,
                    "config: drag_modifier must be a string, keeping default"
                ),
            },
            "decorations" => match value {
                toml::Value::Table(entries) => {
                    for (key, value) in entries {
                        match key.as_str() {
                            "server_side" => config.decorations.server_side = string_list(value, "decorations.server_side"),
                            "client_side" => config.decorations.client_side = string_list(value, "decorations.client_side"),
                            unknown => tracing::warn!(
                                key = %unknown,
                                "config: unknown key in [decorations], ignoring it"
                            ),
                        }
                    }
                }
                other => tracing::warn!(
                    value = ?other,
                    "config: [decorations] must be a table, ignoring it"
                ),
            },
            "commands" => match value {
                toml::Value::Table(entries) => {
                    for (name, value) in entries {
                        let name = name.trim().to_ascii_lowercase();
                        if name.is_empty() {
                            tracing::warn!("config: [commands] entry with an empty name, skipping it");
                            continue;
                        }
                        match argv_from_value(value, "commands") {
                            Some(argv) => {
                                config.commands.insert(name, argv);
                            }
                            None => tracing::warn!(
                                name = %name,
                                "config: [commands] entry must be a command-line string or an array of arguments, skipping it"
                            ),
                        }
                    }
                }
                other => tracing::warn!(
                    value = ?other,
                    "config: [commands] must be a table, ignoring it"
                ),
            },
            "terminal" => match argv_from_value(value, "terminal") {
                Some(argv) => config.terminal = Some(argv),
                None => tracing::warn!(
                    value = ?value,
                    "config: terminal must be a command-line string or an array of arguments, keeping the built-in terminal"
                ),
            },
            // A list of command lines rather than a table: these are
            // run, not named, and their file order is their start
            // order — which a table could not promise.
            "autostart" => match value {
                toml::Value::Array(items) => {
                    for item in items {
                        match argv_from_value(item, "autostart") {
                            Some(argv) => config.autostart.push(argv),
                            None => tracing::warn!(
                                value = ?item,
                                "config: autostart entries must be command-line strings or arrays of arguments, skipping one"
                            ),
                        }
                    }
                }
                other => tracing::warn!(
                    value = ?other,
                    "config: autostart must be an array of command lines, ignoring it"
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
    // `run <name>` is checked here, after the whole file has been read,
    // rather than inside `apply_keybindings`. TOML tables reach us in
    // an order we do not control, so a binding may well be parsed
    // before the `[commands]` table it refers to; validating during the
    // walk would reject perfectly good configs based on where the user
    // happened to put a section.
    //
    // A binding naming a command that does not exist is dropped, not
    // kept and failed at spawn time. The whole point of routing argv
    // through a named table is that a typo becomes one warning at
    // startup naming both the key and the command, instead of a key
    // that silently does nothing whenever it is pressed.
    config.keybindings.retain(|(combo, action)| {
        let Action::Run(name) = action else { return true };
        if config.commands.contains_key(name) {
            return true;
        }
        tracing::warn!(
            command = %name,
            key = ?combo,
            known = ?config.commands.keys().collect::<Vec<_>>(),
            "config: binding runs a command that is not in [commands], dropping the binding"
        );
        false
    });
    Ok(config)
}

/// A command line from the file, as argv.
///
/// Two spellings, because they answer different needs and neither one
/// alone is enough. A bare string is what a user reaches for and is
/// split on whitespace, exactly as `lock_command` is. An array is the
/// escape hatch for the case that split cannot express — an argument
/// with a space in it — and is taken verbatim, so
/// `["notify-send", "hello world"]` sends one argument, not two.
///
/// `None` means "not a command line at all". An empty result is also
/// `None`: a string of only spaces and an empty array both describe no
/// program to run, and every caller would otherwise have to guard
/// against spawning "".
fn argv_from_value(value: &toml::Value, what: &str) -> Option<Vec<String>> {
    let argv: Vec<String> = match value {
        toml::Value::String(line) => line.split_whitespace().map(str::to_string).collect(),
        toml::Value::Array(items) => {
            let mut argv = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    toml::Value::String(arg) => argv.push(arg.clone()),
                    other => {
                        tracing::warn!(
                            value = ?other,
                            key = %what,
                            "config: command arguments must be strings, rejecting this command"
                        );
                        return None;
                    }
                }
            }
            argv
        }
        _ => return None,
    };
    (!argv.is_empty()).then_some(argv)
}

/// A TOML array of strings, trimmed and lowercased for the
/// case-insensitive prefix matching every identity list here does.
/// Non-string entries are skipped individually rather than voiding the
/// whole list — one typo'd entry should cost one entry.
fn string_list(value: &toml::Value, what: &str) -> Vec<String> {
    let toml::Value::Array(items) = value else {
        tracing::warn!(value = ?value, key = %what, "config: expected an array of strings, ignoring it");
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| match v {
            toml::Value::String(name) => Some(name.trim().to_ascii_lowercase()),
            other => {
                tracing::warn!(value = ?other, key = %what, "config: entries must be strings, skipping one");
                None
            }
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// Parses `drag_modifier`. The outer `Option` is "was this a valid
/// name"; the inner one is the setting itself, where `None` is the
/// explicit `"none"` that turns the gesture off.
fn drag_modifier_from_name(name: &str) -> Option<Option<Modifiers>> {
    match name.trim().to_ascii_lowercase().as_str() {
        "alt" | "mod1" => Some(Some(Modifiers::ALT)),
        "super" | "mod4" | "win" => Some(Some(Modifiers::SUPER)),
        "control" | "ctrl" => Some(Some(Modifiers::CONTROL)),
        "none" | "off" => Some(None),
        _ => None,
    }
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
            (combo(0xff1b, Modifiers::CONTROL), Action::WindowMenu),
        ];
        let config = Config::default_config();
        assert_eq!(config.keybindings, expected);
        assert!(!config.focus_follows_mouse);
        assert_eq!(config.scale, None);
        assert_eq!(config.theme, None);
        assert_eq!(config.placement, PlacementPolicy::Smart);
        assert_eq!(config.edge_resistance, 10);
        // The decoration policy ships no per-application list at all:
        // the compositor concludes the negotiation for every client
        // observed, and an entry here is a correction, not the
        // mechanism.
        assert_eq!(config.decorations, DecorationRules::default());
        assert_eq!(config.decorations.decision_for(Some("org.omarchy.terminal")), None);
        assert_eq!(config.drag_modifier, Some(DEFAULT_DRAG_MODIFIER));
    }

    /// The gesture that makes an undecorated window usable at all, and
    /// the one the old policy's comments claimed already existed.
    #[test]
    fn the_drag_modifier_is_configurable_and_can_be_turned_off() {
        assert_eq!(parse("drag_modifier = \"super\"\n").unwrap().drag_modifier, Some(Modifiers::SUPER));
        assert_eq!(parse("drag_modifier = \"MOD1\"\n").unwrap().drag_modifier, Some(Modifiers::ALT));
        assert_eq!(parse("drag_modifier = \"none\"\n").unwrap().drag_modifier, None);
        // A typo keeps the default rather than silently disabling the
        // only way to move a window that has no titlebar.
        assert_eq!(parse("drag_modifier = \"hyper\"\n").unwrap().drag_modifier, Some(DEFAULT_DRAG_MODIFIER));
    }

    /// Both directions, matched as a case-insensitive prefix, with the
    /// rescue direction winning a contradiction.
    #[test]
    fn decoration_rules_override_in_both_directions() {
        let config = parse(
            "[decorations]\nserver_side = [\"Alacritty\"]\nclient_side = [\"google-chrome\"]\n",
        )
        .unwrap();
        assert_eq!(config.decorations.decision_for(Some("alacritty")), Some(true));
        assert_eq!(config.decorations.decision_for(Some("google-chrome")), Some(false));
        // Prefix, so one entry covers a family's per-profile ids.
        assert_eq!(config.decorations.decision_for(Some("GOOGLE-CHROME-beta")), Some(false));
        // Unmatched clients are left to their own negotiation, which is
        // the whole point: the lists are exceptions, not the policy.
        assert_eq!(config.decorations.decision_for(Some("foot")), None);
        assert_eq!(config.decorations.decision_for(None), None);
    }

    #[test]
    fn a_client_in_both_lists_keeps_its_chrome() {
        let rules = DecorationRules {
            server_side: vec!["contested".to_string()],
            client_side: vec!["contested".to_string()],
        };
        assert_eq!(rules.decision_for(Some("contested")), Some(true), "the usable window wins the tie");
    }

    /// An existing config file must not lose the override it was
    /// written for just because the key was renamed.
    #[test]
    fn the_old_one_directional_key_still_reads_as_client_side() {
        let config = parse("self_decorating_apps = [\"zenity\"]\n").unwrap();
        assert_eq!(config.decorations.client_side, vec!["zenity".to_string()]);
        assert!(config.decorations.server_side.is_empty());
    }

    /// An empty entry must never become a prefix that matches every
    /// window on the desk.
    #[test]
    fn an_empty_entry_matches_nothing() {
        let rules = DecorationRules { server_side: vec![String::new()], client_side: Vec::new() };
        assert_eq!(rules.decision_for(Some("anything")), None);
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
        // 12 defaults - 1 unbound + 2 new = 13.
        assert_eq!(config.keybindings.len(), 13);
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
            ("window-menu", Action::WindowMenu),
            ("restart", Action::Restart),
        ];
        // One letter per action rather than one function key: the list
        // outgrew F12 when the window-menu verb was added, and a test
        // that silently stops covering the tail is worse than useless.
        let spec_for = |n: usize| format!("super+{}", (b'a' + n as u8) as char);
        assert!(names.len() <= 26, "one binding per letter");
        let mut text = String::from("[keybindings]\n");
        for (n, (name, _)) in names.iter().enumerate() {
            text.push_str(&format!("\"{}\" = \"{}\"\n", spec_for(n), name));
        }
        let config = parse(&text).unwrap();
        for (n, (name, action)) in names.iter().enumerate() {
            assert_eq!(action_for(&config, &spec_for(n)).as_ref(), Some(action), "action {name:?}");
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
        assert_eq!(config.keybindings.len(), 12);
    }

    #[test]
    fn none_unbinds_a_default() {
        let config = parse("[keybindings]\n\"alt+shift+q\" = \"none\"\n").unwrap();
        assert_eq!(action_for(&config, "alt+shift+q"), None);
        assert_eq!(config.keybindings.len(), 11);
        assert!(!config.keybindings.iter().any(|(_, a)| *a == Action::Close));
    }

    #[test]
    fn none_spelled_differently_still_unbinds() {
        // The combo is matched semantically, not textually: unbinding
        // through an alias/case variant of the default's spelling works.
        let config = parse("[keybindings]\n\"ALT+CONTROL+RIGHT\" = \"None\"\n").unwrap();
        assert_eq!(action_for(&config, "alt+ctrl+right"), None);
        assert_eq!(config.keybindings.len(), 11);
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
        // ...and the untouched defaults survived: 12 - 1 + 1 = 12.
        assert_eq!(config.keybindings.len(), 12);
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
        assert_eq!(config.keybindings.len(), 12);
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
    fn omarchy_menu_defaults_on_and_parses_as_a_boolean() {
        // On by default: the submenu is invisible without Omarchy, so
        // the default costs nothing on a machine that lacks it.
        assert!(Config::default_config().omarchy_menu);
        assert!(!parse("omarchy_menu = false").unwrap().omarchy_menu);
        assert!(parse("omarchy_menu = true").unwrap().omarchy_menu);
    }

    #[test]
    fn wrongly_typed_omarchy_menu_keeps_the_default() {
        for text in ["omarchy_menu = \"off\"", "omarchy_menu = 0"] {
            assert!(parse(text).unwrap().omarchy_menu, "text {text:?}");
        }
    }

    #[test]
    fn omarchy_shell_defaults_on_and_parses_as_a_boolean() {
        // On by default for the same reason the menu is: without
        // Omarchy's shell installed there is nothing to launch, so the
        // default costs a machine without Omarchy nothing.
        assert!(Config::default_config().omarchy_shell);
        assert!(!parse("omarchy_shell = false").unwrap().omarchy_shell);
        assert!(parse("omarchy_shell = true").unwrap().omarchy_shell);
        // The two keys are independent: a desktop can carry the menu
        // and leave the shell to something else, or the reverse.
        let config = parse("omarchy_menu = false\nomarchy_shell = true").unwrap();
        assert!(!config.omarchy_menu && config.omarchy_shell);
    }

    #[test]
    fn wrongly_typed_omarchy_shell_keeps_the_default() {
        for text in ["omarchy_shell = \"off\"", "omarchy_shell = 1"] {
            assert!(parse(text).unwrap().omarchy_shell, "text {text:?}");
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

#[cfg(test)]
mod command_tests {
    use super::*;

    fn action_for(config: &Config, spec: &str) -> Option<Action> {
        let combo = parse_key(spec).expect("test spec must parse");
        config
            .keybindings
            .iter()
            .find(|(existing, _)| *existing == combo)
            .map(|(_, action)| action.clone())
    }

    /// The whole point of the seam: a key reaches a named command, and
    /// the name — not the command line — is what the binding carries.
    #[test]
    fn a_binding_runs_a_named_command() {
        let config = parse(
            r#"
            [commands]
            omarchy-menu = "omarchy-shell shell toggle omarchy.menu"

            [keybindings]
            "super+space" = "run omarchy-menu"
            "#,
        )
        .expect("valid config");
        assert_eq!(
            action_for(&config, "super+space"),
            Some(Action::Run("omarchy-menu".into()))
        );
        assert_eq!(
            config.commands.get("omarchy-menu").map(Vec::as_slice),
            Some(["omarchy-shell", "shell", "toggle", "omarchy.menu"].map(String::from).as_slice())
        );
    }

    /// Declaring the table *after* the binding that uses it must work.
    /// TOML hands us keys in an order the user does not control, so
    /// validating during the walk would make a correct config fail on
    /// section order alone. This is the regression test for that.
    #[test]
    fn command_table_may_come_after_the_binding_that_names_it() {
        let config = parse(
            r#"
            [keybindings]
            "super+space" = "run menu"

            [commands]
            menu = "omarchy-menu"
            "#,
        )
        .expect("valid config");
        assert_eq!(action_for(&config, "super+space"), Some(Action::Run("menu".into())));
    }

    /// A binding naming a command nobody declared is dropped at parse
    /// time rather than kept and failed at press time.
    #[test]
    fn a_binding_naming_an_unknown_command_is_dropped() {
        let config = parse(
            r#"
            [commands]
            menu = "omarchy-menu"

            [keybindings]
            "super+space" = "run typo"
            "#,
        )
        .expect("valid config");
        assert_eq!(action_for(&config, "super+space"), None);
    }

    /// An unknown `run` must not take the *default* binding for that
    /// combo down with it — the same contract every other unparsable
    /// entry honors.
    #[test]
    fn a_dropped_run_binding_leaves_other_bindings_alone() {
        let config = parse(
            r#"
            [keybindings]
            "super+space" = "run nope"
            "#,
        )
        .expect("valid config");
        let defaults = Config::default_config();
        assert_eq!(config.keybindings.len(), defaults.keybindings.len());
        assert_eq!(action_for(&config, "alt+shift+return"), Some(Action::SpawnTerminal));
    }

    /// Command names fold case on both sides, so capitalization can
    /// never be the silent reason a key does nothing.
    #[test]
    fn command_names_are_case_insensitive_on_both_sides() {
        let config = parse(
            r#"
            [commands]
            Lock = "omarchy-system-lock"

            [keybindings]
            "super+l" = "run LOCK"
            "#,
        )
        .expect("valid config");
        assert_eq!(action_for(&config, "super+l"), Some(Action::Run("lock".into())));
        assert!(config.commands.contains_key("lock"));
    }

    /// An array is the escape hatch a whitespace split cannot express:
    /// one argument that contains a space stays one argument.
    #[test]
    fn an_array_command_keeps_arguments_with_spaces_whole() {
        let config = parse(
            r#"
            [commands]
            greet = ["notify-send", "hello world"]
            "#,
        )
        .expect("valid config");
        assert_eq!(
            config.commands.get("greet").map(Vec::as_slice),
            Some(["notify-send", "hello world"].map(String::from).as_slice())
        );
    }

    /// Empty command lines describe no program to run, in either
    /// spelling, and must never reach a caller that would spawn "".
    #[test]
    fn empty_command_lines_are_rejected_in_both_spellings() {
        let config = parse(
            r#"
            [commands]
            blank = "   "
            nothing = []
            "#,
        )
        .expect("valid config");
        assert!(config.commands.is_empty());
    }

    /// `run` with no name is not an action. It must not become a
    /// lookup for the empty string.
    #[test]
    fn run_without_a_name_is_not_an_action() {
        assert_eq!(action_from_name("run"), None);
        assert_eq!(action_from_name("run   "), None);
    }

    /// Autostart is a list because its order is meaningful, and the
    /// file's order is the one that survives.
    #[test]
    fn autostart_keeps_file_order() {
        let config = parse(
            r#"
            autostart = ["first --a", ["second", "--b"]]
            "#,
        )
        .expect("valid config");
        assert_eq!(
            config.autostart,
            vec![
                vec!["first".to_string(), "--a".to_string()],
                vec!["second".to_string(), "--b".to_string()],
            ]
        );
    }

    /// The media keys exist now. Before this they were not merely
    /// unbound — there was no name for them, so a laptop's volume keys
    /// could not be bound at all.
    #[test]
    fn media_keys_parse() {
        for (spec, keysym) in [
            ("volumeup", 0x1008ff13),
            ("volumedown", 0x1008ff11),
            ("mute", 0x1008ff12),
            ("brightnessup", 0x1008ff02),
            ("playpause", 0x1008ff14),
        ] {
            let combo = parse_key(spec).unwrap_or_else(|| panic!("{spec} must parse"));
            assert_eq!(combo.keysym, keysym, "{spec}");
        }
    }

    /// A configured terminal is argv, in both spellings.
    #[test]
    fn terminal_accepts_a_string_or_an_array() {
        let from_string = parse(r#"terminal = "alacritty --class term""#).expect("valid");
        assert_eq!(
            from_string.terminal.as_deref(),
            Some(["alacritty", "--class", "term"].map(String::from).as_slice())
        );
        let from_array = parse(r#"terminal = ["ghostty"]"#).expect("valid");
        assert_eq!(from_array.terminal.as_deref(), Some(["ghostty".to_string()].as_slice()));
    }

    /// Nothing configured means the built-in terminal, and that has to
    /// stay distinguishable from "the user chose something".
    #[test]
    fn no_terminal_key_leaves_the_builtin_selected() {
        assert!(parse("").expect("valid").terminal.is_none());
        assert!(Config::default_config().terminal.is_none());
    }
}
