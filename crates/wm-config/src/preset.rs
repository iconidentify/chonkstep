//! Named presets: one config line that sets a whole posture's worth of
//! defaults, and the Omarchy keymap that is the point of having them.
//!
//! # Why a preset rather than more switches
//!
//! Chonkstep's Omarchy integration grew one key at a time —
//! `omarchy_menu`, `omarchy_shell`, `theme = "omarchy"`, `show_dock`,
//! the bar's own remembered visibility — and each of them is a good
//! switch. Together they are a pile: a user who wants "chonkstep as the
//! window manager for my Omarchy desktop" has to know that five
//! unrelated-looking keys add up to it, and has to get all five right
//! before anything looks like the thing they were promised.
//!
//! [`Desktop::Omarchy`] is that combination, named. It is a set of
//! **defaults**, not a mode and not a lock: [`base`] applies it to a
//! fresh [`Config`] *before* the file's own keys are read, so every
//! single value it sets is overridden by writing that key out — the
//! ordinary TOML rule, with no precedence table to memorise. Which is
//! also why it is not a boolean beside the switches it presets: a
//! boolean could not grow a third posture, and `omarchy_mode = true`
//! sitting next to `omarchy_shell = false` reads like a contradiction
//! rather than like a default and an override.
//!
//! # Why the keymap is a second key
//!
//! [`Keymap`] is separate from [`Desktop`] because the two answer
//! different questions — "whose furniture is on the screen" and "which
//! chords does my muscle memory hold" — and each is wanted without the
//! other. A chonkstep user on an Omarchy machine may want the guest's
//! bar and pickers with the NeXTSTEP chords they came for; an Omarchy
//! user evaluating chonkstep on a spare desk may want the reverse.
//!
//! It is still one line for the common case: `desktop = "omarchy"`
//! *defaults* the keymap to Omarchy's, and `keymap = "chonkstep"` takes
//! it back. That is the preset rule applied to the presets themselves.
//!
//! # The keymap replaces; it does not merge
//!
//! [`OMARCHY_BINDINGS`] is a whole table, and choosing it discards
//! chonkstep's default table outright rather than layering over it.
//! Merging would leave a desk answering to both vocabularies at once,
//! where `alt+shift+q` and `super+w` both close a window and neither
//! one is the documented answer — and, worse, where a chord one keymap
//! deliberately leaves unbound stays bound by the other. Every
//! remaining conflict is therefore resolved in the preset's favour by
//! construction, and a user who wants a binding from the other
//! vocabulary back writes that one line in `[keybindings]`.
//!
//! # Where the bindings come from
//!
//! Not from memory of Hyprland. From Omarchy's own configuration —
//! `$OMARCHY_PATH/default/hypr/bindings/{applications,clipboard,media,
//! tiling,utilities,voxtype}.lua` — read binding by binding, with the
//! `o.bind` helpers expanded the way `helpers.lua` expands them
//! (`{ omarchy = "browser" }` is `omarchy-launch-browser`,
//! `o.bind_toggle(.., "idle")` is `omarchy-toggle-idle`, `{ tui = "x" }`
//! is `omarchy-launch-tui x`).
//!
//! Three kinds of Omarchy binding get three different answers, and the
//! third is the one that keeps this honest:
//!
//! 1. **A window or workspace verb** chonkstep also has becomes that
//!    verb: `SUPER + W` closes, `SUPER + RETURN` spawns a terminal.
//! 2. **An Omarchy command** becomes [`Action::Run`] naming that same
//!    command, declared in [`OMARCHY_COMMANDS`]. This is the whole
//!    "install chonkstep, keep your Omarchy" claim made literal:
//!    `SUPER + SPACE` opens Omarchy's menu because it runs Omarchy's
//!    `omarchy-menu`, not an imitation of it.
//! 3. **Everything else stays unbound**, and says why in
//!    [`OMARCHY_UNBOUND`]. A tiling desktop's vocabulary is full of
//!    verbs that have no meaning on a stacking desk — split ratios,
//!    gaps, layout cycling, "toggle floating" on a desk where
//!    everything floats already — and the temptation is to map each one
//!    to whatever is nearest. An approximation is worse than a dead
//!    key: a dead key is discovered in five seconds and looked up,
//!    while `SUPER + J` that does something *else* is a bug report.
//!
//! The same filter the Omarchy menu already applies applies here: an
//! action that invokes `hyprctl` or an `omarchy-hyprland-*` script
//! commands a compositor that is not running, so it is left unbound
//! rather than bound to a command that will fail
//! (`chonk_shell::omarchy_menu`, `Skip::HyprlandOnly`).

use std::collections::BTreeMap;

use crate::{parse_key, Action, Config};

/// Whose desktop this session is: chonkstep's own, or Omarchy's with
/// chonkstep as its window manager.
///
/// Spelled as a named value rather than a boolean because it *picks a
/// posture*, the way `placement` and `appearance` pick one, and because
/// the value `"omarchy"` on a chonkstep key already means "defer to
/// Omarchy for this" everywhere else in the file — `theme = "omarchy"`
/// established that idiom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Desktop {
    /// A whole chonkstep desktop: its Dock in the corner, its own
    /// theme, Omarchy's bar off unless asked for. The default, and
    /// exactly what every existing config file already means.
    #[default]
    Chonkstep,
    /// Omarchy's desktop, with chonkstep doing the window management:
    /// no Dock, Omarchy's bar hosted and shown, its theme followed, its
    /// menu and pickers under the mouse and the keyboard.
    Omarchy,
}

/// Which vocabulary of chords the session answers to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keymap {
    /// The NeXTSTEP-style `alt+shift` chords chonkstep was designed
    /// around ([`Config::default_config`]).
    #[default]
    Chonkstep,
    /// Omarchy's own binding vocabulary, mapped onto chonkstep's
    /// actions ([`OMARCHY_BINDINGS`]).
    Omarchy,
}

impl Desktop {
    /// The name a config file spells this posture with, trimmed and
    /// case-insensitive like every other name in the format. `None` for
    /// an unknown name, which the caller warns about and ignores — a
    /// typo'd posture must cost the user the preset, never the session.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "chonkstep" => Some(Self::Chonkstep),
            "omarchy" => Some(Self::Omarchy),
            _ => None,
        }
    }

    /// The name this posture is spelled with.
    pub fn id(self) -> &'static str {
        match self {
            Self::Chonkstep => "chonkstep",
            Self::Omarchy => "omarchy",
        }
    }

    /// The keymap this posture *defaults* to. An explicit `keymap` key
    /// beats it, which is the whole preset rule turned on the presets:
    /// `desktop = "omarchy"` is one line for the whole posture, and
    /// `keymap = "chonkstep"` peels one layer back off it.
    pub fn keymap(self) -> Keymap {
        match self {
            Self::Chonkstep => Keymap::Chonkstep,
            Self::Omarchy => Keymap::Omarchy,
        }
    }
}

impl Keymap {
    /// As [`Desktop::from_name`], for the `keymap` key.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "chonkstep" => Some(Self::Chonkstep),
            "omarchy" => Some(Self::Omarchy),
            _ => None,
        }
    }

    /// The name this keymap is spelled with.
    pub fn id(self) -> &'static str {
        match self {
            Self::Chonkstep => "chonkstep",
            Self::Omarchy => "omarchy",
        }
    }
}

/// The base [`Config`] a file's own keys are then read over: the
/// built-in defaults with the file's chosen presets applied.
///
/// Resolving the two preset keys *before* the walk over the rest of the
/// table is what makes "an explicit setting always beats a preset
/// default" true without a precedence rule anywhere: by the time any
/// other key is read, the preset is just the starting value that key
/// overwrites. It also has to happen first for a second reason — TOML
/// tables arrive in an order this crate does not control, and
/// `keybindings` sorts before `keymap`, so a keymap resolved during the
/// walk would replace the table the user's own `[keybindings]` entries
/// had already merged into.
pub fn base(table: &toml::Table) -> Config {
    let desktop = preset_name(table, "desktop", Desktop::from_name).unwrap_or_default();
    let keymap =
        preset_name(table, "keymap", Keymap::from_name).unwrap_or_else(|| desktop.keymap());
    let mut config = Config::default_config();
    config.desktop = desktop;
    config.keymap = keymap;
    apply_desktop(&mut config, desktop);
    apply_keymap(&mut config, keymap);
    if desktop == Desktop::Omarchy {
        for key in ["show_dock", "omarchy_bar", "theme"] {
            config
                .provenance
                .insert(key.into(), "desktop preset (omarchy)".into());
        }
    }
    if keymap == Keymap::Omarchy {
        for key in ["keybindings", "commands"] {
            config
                .provenance
                .insert(key.into(), "keymap preset (omarchy)".into());
        }
    }
    if matches!(table.get("desktop"), Some(toml::Value::String(name)) if Desktop::from_name(name).is_some())
    {
        config
            .provenance
            .insert("desktop".into(), "config file".into());
    }
    if matches!(table.get("keymap"), Some(toml::Value::String(name)) if Keymap::from_name(name).is_some())
    {
        config
            .provenance
            .insert("keymap".into(), "config file".into());
    }
    config
}

/// One preset key's value, warning exactly once about a value that is
/// not a string or not a name we know. The warning lives here rather
/// than in `parse`'s walk so the key is diagnosed once, in the place
/// that acts on it, instead of twice from two readers.
fn preset_name<T>(
    table: &toml::Table,
    key: &str,
    from_name: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    match table.get(key)? {
        toml::Value::String(name) => match from_name(name) {
            Some(value) => Some(value),
            None => {
                tracing::warn!(
                    key = %key,
                    value = %name,
                    "config: unknown preset name, keeping the chonkstep default"
                );
                None
            }
        },
        other => {
            tracing::warn!(
                key = %key,
                value = ?other,
                "config: preset must be a name string, keeping the chonkstep default"
            );
            None
        }
    }
}

/// Applies a [`Desktop`] posture's non-binding defaults.
///
/// Only the values that actually differ are touched, so this stays
/// readable as "what the posture changes" rather than as a second copy
/// of the defaults table.
pub fn apply_desktop(config: &mut Config, desktop: Desktop) {
    match desktop {
        // The built-in defaults already *are* the chonkstep posture.
        Desktop::Chonkstep => {}
        Desktop::Omarchy => {
            // One desk, one set of furniture. Omarchy's bar is at the
            // top and this desk's Dock would be a second instrument
            // strip in the corner beside it, duplicating its clock,
            // its volume and its network readout.
            config.show_dock = false;
            // ...and the guest's bar, which chonkstep otherwise hosts
            // but keeps off the screen until asked. Here it is the
            // furniture, so it is shown.
            config.omarchy_bar = Some(true);
            // Follow Omarchy's theme rather than wearing a built-in:
            // the point of the posture is that this desk looks like the
            // desktop it joined, and re-dresses when `omarchy-theme-set`
            // does. A theme picked from the root menu still wins over
            // this, because that is a more recent, more deliberate
            // gesture (`chonk_shell::startup::resolve_theme_id`).
            // The literal id rather than `wm_theme::omarchy::ID`: this
            // crate deliberately does not depend on `wm-theme` (see the
            // crate docs), and the id is part of the *config format*,
            // which is this crate's own contract. The example config and
            // `startup::resolve_look` are the other two places it is
            // spelled, and a test in `wm-theme` pins them together.
            config.theme = Some("omarchy".to_string());
            // Crash recovery happens before the relaunched shell can
            // lock anything itself.  This is Omarchy's stable session
            // entry point (and the same command its shipped hypridle
            // configuration calls), so it preserves whichever locker
            // the installed shell owns rather than guessing a client
            // such as hyprlock or swaylock here.
            config.lock_command = Some("omarchy-system-lock".to_string());
            // Both of these are already on by default and are restated
            // here on purpose: the posture is a *statement* of what the
            // session is, and a default that quietly flips the other way
            // one release from now must not silently take the posture
            // with it.
            config.omarchy_menu = true;
            config.omarchy_shell = true;
        }
    }
}

/// Applies a [`Keymap`]'s binding table, replacing rather than merging
/// (see the module docs), and declaring the `[commands]` its `run`
/// bindings name.
///
/// The command declarations are inserted rather than assigned, so a
/// user's own `[commands]` entry of the same name — read later, from
/// the file — replaces the preset's. That is the same override rule as
/// everywhere else here, applied one command at a time.
pub fn apply_keymap(config: &mut Config, keymap: Keymap) {
    match keymap {
        Keymap::Chonkstep => {}
        Keymap::Omarchy => {
            config.commands.extend(omarchy_commands());
            config.keybindings = omarchy_keybindings();
        }
    }
}

/// [`OMARCHY_COMMANDS`] as the map `[commands]` parses into.
pub fn omarchy_commands() -> BTreeMap<String, Vec<String>> {
    OMARCHY_COMMANDS
        .iter()
        .map(|(name, argv)| {
            (
                name.to_string(),
                argv.iter().map(|arg| arg.to_string()).collect(),
            )
        })
        .collect()
}

/// [`OMARCHY_BINDINGS`] as the list [`Config::keybindings`] holds.
///
/// Routed through [`parse_key`] and `action_from_name`'s public spelling
/// for the same reason [`Config::default_config`] routes its own
/// defaults through them: the preset is exercised by the exact parser a
/// user's specs go through, so a parser regression fails the preset's
/// test rather than silently shipping a different table than the docs
/// promise.
pub fn omarchy_keybindings() -> Vec<(wm_core::KeyCombo, Action)> {
    OMARCHY_BINDINGS
        .iter()
        .map(|(spec, action)| {
            let combo =
                parse_key(spec).unwrap_or_else(|| panic!("preset key spec {spec:?} must parse"));
            let action = crate::action_from_name(action)
                .unwrap_or_else(|| panic!("preset action name {action:?} must be a known action"));
            (combo, action)
        })
        .collect()
}

/// The Omarchy keymap: every chord from Omarchy's own `bindings/*.lua`
/// that has a true chonkstep answer, paired with the action name a
/// config file would spell.
///
/// Grouped by the Omarchy file each chord comes from, so a diff against
/// a future Omarchy release is a file-by-file read rather than a hunt.
/// A `run <name>` action names an entry in [`OMARCHY_COMMANDS`].
pub const OMARCHY_BINDINGS: &[(&str, &str)] = &[
    // -- applications.lua: the ungated essentials ---------------------
    //
    // Omarchy's own gate (`o.preinstalled_bindings_enabled()`) is what
    // divides this group from the twenty-odd webapp and TUI chords
    // below it in that file: these seven are the ones Omarchy binds
    // unconditionally, so they are the ones a preset can bind
    // unconditionally too. The rest are the user's application
    // choices, not the desktop's vocabulary — see `OMARCHY_UNBOUND`.
    ("super+return", "spawn-terminal"),
    ("super+shift+return", "run omarchy-browser"),
    ("super+shift+b", "run omarchy-browser"),
    ("super+shift+alt+b", "run omarchy-browser-private"),
    ("super+shift+f", "run omarchy-files"),
    ("super+alt+shift+f", "run omarchy-files-here"),
    ("super+shift+n", "run omarchy-editor"),
    // -- tiling.lua: the window and workspace verbs -------------------
    //
    // Thirty of forty. The rest of that file is the tiling
    // vocabulary itself, and `OMARCHY_UNBOUND` says so one chord at a
    // time.
    ("super+w", "close"),
    ("super+f", "toggle-fullscreen"),
    // Omarchy's "Full width" is Hyprland's `maximized` mode, which
    // fills the workarea and keeps the chrome — chonkstep's
    // `toggle-maximize` exactly. Their `SUPER + F` fullscreen (no
    // chrome, whole output) is `toggle-fullscreen` above, so the pair
    // keeps its shape: the plain chord takes the screen, the modified
    // one takes the workarea.
    ("super+alt+f", "toggle-maximize"),
    // Floating windows still occupy a real two-dimensional arrangement.
    // These select the closest focusable frame in the requested
    // direction, which preserves Omarchy's intent without inventing a
    // tiling tree.
    ("super+left", "focus-left"),
    ("super+right", "focus-right"),
    ("super+up", "focus-up"),
    ("super+down", "focus-down"),
    ("super+tab", "workspace-next"),
    ("super+shift+tab", "workspace-prev"),
    // The twenty chords an Omarchy user has in their fingers before
    // they have a mouse in their hand: the workspace row by number,
    // and the same row with the window in tow. `SUPER + 0` is
    // workspace 10, which is where Omarchy puts it and why these verbs
    // take a number rather than being spelled out nine times.
    //
    // Chonkstep's workspaces grow on demand and are never destroyed,
    // so `SUPER + 7` on a desk with three workspaces creates the four
    // in between rather than refusing — the same thing `super+tab`
    // does one workspace at a time, and the same thing Hyprland does.
    // The bar draws whatever the row grew to.
    ("super+1", "workspace 1"),
    ("super+2", "workspace 2"),
    ("super+3", "workspace 3"),
    ("super+4", "workspace 4"),
    ("super+5", "workspace 5"),
    ("super+6", "workspace 6"),
    ("super+7", "workspace 7"),
    ("super+8", "workspace 8"),
    ("super+9", "workspace 9"),
    ("super+0", "workspace 10"),
    ("super+shift+1", "workspace-carry 1"),
    ("super+shift+2", "workspace-carry 2"),
    ("super+shift+3", "workspace-carry 3"),
    ("super+shift+4", "workspace-carry 4"),
    ("super+shift+5", "workspace-carry 5"),
    ("super+shift+6", "workspace-carry 6"),
    ("super+shift+7", "workspace-carry 7"),
    ("super+shift+8", "workspace-carry 8"),
    ("super+shift+9", "workspace-carry 9"),
    ("super+shift+0", "workspace-carry 10"),
    ("super+shift+alt+1", "workspace-send 1"),
    ("super+shift+alt+2", "workspace-send 2"),
    ("super+shift+alt+3", "workspace-send 3"),
    ("super+shift+alt+4", "workspace-send 4"),
    ("super+shift+alt+5", "workspace-send 5"),
    ("super+shift+alt+6", "workspace-send 6"),
    ("super+shift+alt+7", "workspace-send 7"),
    ("super+shift+alt+8", "workspace-send 8"),
    ("super+shift+alt+9", "workspace-send 9"),
    ("super+shift+alt+0", "workspace-send 10"),
    // "Move window to scratchpad": send this window out of the way and
    // leave it recoverable. Chonkstep's nearest true verb is
    // `miniaturize` — the window collapses to an icon tile on the desk
    // rather than onto a special workspace, and it comes back by
    // double-clicking that tile rather than by the same chord. See
    // docs/omarchy-mode.md for the difference spelled out.
    ("super+alt+s", "miniaturize"),
    // -- clipboard.lua ------------------------------------------------
    ("super+ctrl+v", "run omarchy-clipboard"),
    // -- utilities.lua: the menu, the pickers, the panels -------------
    ("super+space", "run omarchy-menu"),
    // Omarchy binds the same menu on Apple keyboards through the
    // evdev/XKB `code:201` spelling. The live reader normalizes that
    // spelling to F23; keep the baked fallback equivalent too.
    ("super+shift+f23", "run omarchy-menu"),
    ("super+alt+space", "run omarchy-menu-apps"),
    ("super+escape", "run omarchy-menu-system"),
    ("poweroff", "run omarchy-menu-system"),
    ("super+ctrl+c", "run omarchy-menu-capture"),
    ("super+ctrl+o", "run omarchy-menu-toggles"),
    ("super+ctrl+h", "run omarchy-menu-hardware"),
    ("super+ctrl+s", "run omarchy-menu-share"),
    ("super+ctrl+space", "run omarchy-menu-background"),
    ("super+shift+ctrl+space", "run omarchy-menu-theme"),
    ("super+ctrl+e", "run omarchy-emojis"),
    ("super+alt+k", "run omarchy-keybindings-tmux"),
    ("super+ctrl+k", "run omarchy-keybindings-herdr"),
    ("super+ctrl+q", "run omarchy-calculator"),
    ("calculator", "run omarchy-calculator"),
    // Notifications: Omarchy's shell draws them, so its own commands
    // are the only ones that can reach them.
    ("super+comma", "run omarchy-notification-dismiss"),
    ("super+shift+comma", "run omarchy-notification-dismiss-all"),
    ("super+alt+comma", "run omarchy-notification-invoke"),
    ("super+shift+alt+comma", "run omarchy-notification-history"),
    ("super+ctrl+comma", "run omarchy-notification-silence"),
    ("super+ctrl+i", "run omarchy-toggle-idle"),
    ("super+ctrl+n", "run omarchy-toggle-nightlight"),
    // Capture. `print` and its friends are why this crate's keysym
    // table grew the Print key: a desktop that cannot bind the key with
    // a picture of a screen on it cannot host another desktop's
    // screenshot tooling.
    ("print", "run omarchy-screenshot"),
    ("alt+print", "run omarchy-screenrecord"),
    ("super+print", "run omarchy-colorpicker"),
    ("super+ctrl+print", "run omarchy-ocr"),
    ("super+alt+bracketleft", "run omarchy-webcam-smaller"),
    ("super+alt+bracketright", "run omarchy-webcam-larger"),
    ("super+ctrl+period", "run omarchy-transcode"),
    // Reminders and the little notifications.
    ("super+ctrl+r", "run omarchy-reminder-set"),
    ("super+ctrl+alt+r", "run omarchy-reminder-show"),
    ("super+shift+ctrl+r", "run omarchy-reminder-clear"),
    ("super+ctrl+alt+t", "run omarchy-show-time"),
    ("super+ctrl+alt+b", "run omarchy-show-battery"),
    ("super+ctrl+alt+w", "run omarchy-show-weather"),
    ("super+shift+ctrl+a", "run omarchy-agent"),
    // The bar's panels, by name. These are the rows chonkstep's own
    // Dock instruments duplicate — which is exactly why the posture
    // hides the Dock and keeps these.
    ("super+ctrl+a", "run omarchy-panel-audio"),
    ("super+ctrl+b", "run omarchy-panel-bluetooth"),
    ("super+ctrl+w", "run omarchy-panel-network"),
    ("super+ctrl+p", "run omarchy-panel-power"),
    ("super+ctrl+alt+d", "run omarchy-panel-clock"),
    ("super+ctrl+t", "run omarchy-activity"),
    // ...and by position in the bar's right section, as Omarchy binds
    // them: 1 is the leftmost panel there.
    ("super+ctrl+1", "run omarchy-panel-1"),
    ("super+ctrl+2", "run omarchy-panel-2"),
    ("super+ctrl+3", "run omarchy-panel-3"),
    ("super+ctrl+4", "run omarchy-panel-4"),
    ("super+ctrl+5", "run omarchy-panel-5"),
    ("super+ctrl+6", "run omarchy-panel-6"),
    ("super+ctrl+7", "run omarchy-panel-7"),
    ("super+ctrl+8", "run omarchy-panel-8"),
    ("super+ctrl+9", "run omarchy-panel-9"),
    ("super+ctrl+l", "run omarchy-lock"),
    // -- media.lua: the keys with pictures on them --------------------
    //
    // Omarchy marks these `locked = true` (they work over the lock
    // screen) and the ramps `repeating = true` (they fire while held).
    // Chonkstep's bindings do neither yet; see `docs/omarchy-mode.md`.
    ("volumeup", "run omarchy-volume-up"),
    ("volumedown", "run omarchy-volume-down"),
    ("volumemute", "run omarchy-volume-mute"),
    ("micmute", "run omarchy-mic-mute"),
    ("alt+volumeup", "run omarchy-volume-up-fine"),
    ("alt+volumedown", "run omarchy-volume-down-fine"),
    ("shift+volumemute", "run omarchy-audio-output-switch"),
    ("brightnessup", "run omarchy-brightness-up"),
    ("brightnessdown", "run omarchy-brightness-down"),
    ("shift+brightnessup", "run omarchy-brightness-max"),
    ("shift+brightnessdown", "run omarchy-brightness-min"),
    ("alt+brightnessup", "run omarchy-brightness-up-fine"),
    ("alt+brightnessdown", "run omarchy-brightness-down-fine"),
    ("kbdbrightnessup", "run omarchy-kbd-brightness-up"),
    ("kbdbrightnessdown", "run omarchy-kbd-brightness-down"),
    ("kbdlightonoff", "run omarchy-kbd-brightness-cycle"),
    ("playpause", "run omarchy-media-play-pause"),
    ("audiopause", "run omarchy-media-play-pause"),
    ("audionext", "run omarchy-media-next"),
    ("audioprev", "run omarchy-media-prev"),
    ("alt+playpause", "run omarchy-media-next"),
    ("alt+shift+playpause", "run omarchy-media-prev"),
    ("shift+playpause", "run omarchy-audio-source-switch"),
    ("shift+audiopause", "run omarchy-audio-source-switch"),
    ("eject", "run omarchy-eject"),
    // -- chonkstep's own, on a chord Omarchy leaves us ----------------
    //
    // The window menu has no Omarchy vocabulary and keeps chonkstep's
    // own default chord, which is otherwise unused there.
    ("control+escape", "window-menu"),
];

/// Why an Omarchy chord is deliberately left unbound by
/// [`OMARCHY_BINDINGS`].
///
/// Data rather than prose because the docs and the tests both need it:
/// the docs table is transcribed from here, and the test that these
/// chords are *not* bound reads the same list. A reason invented in one
/// place and forgotten in the other is how a table starts lying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unbound {
    /// Names a tiling-layout action — split direction, gaps, ratios,
    /// grouping, layout cycling — that has no meaning on a stacking
    /// desktop where every window already floats at its own size.
    TilingOnly,
    /// Commands Hyprland itself (`hyprctl`, an `omarchy-hyprland-*`
    /// script, or a `hl.config` write from Lua). The compositor it
    /// talks to is not running, so the binding could only fail. Same
    /// filter `chonk_shell::omarchy_menu` applies to menu rows.
    HyprlandOnly,
    /// Chonkstep has no verb for it yet, and no command could stand in
    /// because the semantics are the window manager's own.
    NoVerb,
    /// Not a key chord this config format can express: a mouse button
    /// or wheel binding, a hardware switch, or a bare X keycode with no
    /// keysym behind it.
    NotAKey,
    /// Omarchy binds it only when something else is installed — its own
    /// preinstalled-applications flag, or a tool it probes for with
    /// `o.cmd_present`. A table of constants cannot make that test, and
    /// binding it unconditionally would put dead keys on a machine that
    /// never had the thing.
    Conditional,
    /// Chonkstep *could* bind it and declines to, because what it would
    /// do here is not what the user pressing it is asking for.
    Declined,
}

impl Unbound {
    /// The one-line reason, as the docs table prints it.
    pub fn reason(self) -> &'static str {
        match self {
            Self::TilingOnly => "tiling-only: no meaning on a stacking desk",
            Self::HyprlandOnly => "commands Hyprland, which is not running",
            Self::NoVerb => "chonkstep has no verb for it, and no command can stand in",
            Self::NotAKey => "not a key chord this config format can express",
            Self::Conditional => "Omarchy binds it conditionally; a table of constants cannot",
            Self::Declined => "declined on purpose -- see the note under the table",
        }
    }
}

/// Every Omarchy chord this keymap leaves unbound, with what Omarchy
/// does with it and why chonkstep does not.
///
/// The list is the deliverable, not an apology. An Omarchy user needs
/// to know which of their chords are dead here *before* they press one
/// and wonder whether it is broken, and the shortest honest way to tell
/// them is to enumerate it.
pub const OMARCHY_UNBOUND: &[(&str, &str, Unbound)] = &[
    // applications.lua
    (
        "super+alt+return, super+ctrl+return, super+shift+{a,c,d,e,g,m,o,p,s,w,x,y,/}, +alt/ctrl variants",
        "Omarchy's preinstalled application, TUI and webapp chords",
        Unbound::Conditional,
    ),
    // clipboard.lua
    (
        "super+c / super+v / super+x",
        "universal copy / paste / cut, by synthesising Ctrl+C/V/X at the seat",
        Unbound::NoVerb,
    ),
    // tiling.lua
    ("super+j", "toggle window split", Unbound::TilingOnly),
    ("super+p", "pseudo-tile the window", Unbound::TilingOnly),
    ("super+t", "toggle floating / tiling", Unbound::TilingOnly),
    ("super+ctrl+f", "tiled fullscreen", Unbound::TilingOnly),
    ("super+o", "pop the window out, floating and pinned", Unbound::TilingOnly),
    ("super+home / super+alt+home", "restore / save window width", Unbound::HyprlandOnly),
    ("super+l", "cycle the workspace layout", Unbound::HyprlandOnly),
    ("super+g / super+alt+g", "toggle grouping / move out of group", Unbound::TilingOnly),
    ("super+alt+left/right/up/down", "move the window into the group in that direction", Unbound::TilingOnly),
    ("super+alt+tab / super+alt+shift+tab", "next / previous window in the group", Unbound::TilingOnly),
    ("super+ctrl+left / super+ctrl+right", "move the grouped-window focus", Unbound::TilingOnly),
    ("super+alt+1..5", "focus the nth window of the group", Unbound::TilingOnly),
    ("super+shift+left/right/up/down", "swap the window with its neighbour", Unbound::TilingOnly),
    (
        "super+minus / super+equal, +shift/alt/ctrl variants",
        "grow and shrink the window by 25 / 100 / 300 px",
        Unbound::TilingOnly,
    ),
    ("super+s", "toggle the scratchpad workspace", Unbound::NoVerb),
    ("super+ctrl+tab", "the workspace before this one", Unbound::NoVerb),
    ("super+shift+alt+left/right/up/down", "move the workspace to the monitor in that direction", Unbound::NoVerb),
    ("ctrl+alt+tab / ctrl+alt+shift+tab", "focus the next / previous monitor", Unbound::NoVerb),
    ("ctrl+alt+delete", "close every window", Unbound::HyprlandOnly),
    ("super+slash / super+alt+slash", "monitor scaling up / down", Unbound::HyprlandOnly),
    ("super+mouse wheel, super+drag", "scroll through workspaces; move and resize by mouse", Unbound::NotAKey),
    // utilities.lua
    ("super+k", "Omarchy's keybinding cheatsheet", Unbound::Declined),
    ("super+shift+space", "toggle Omarchy's top bar", Unbound::NoVerb),
    ("super+ctrl+d", "Omarchy's display panel", Unbound::HyprlandOnly),
    (
        "super+backspace / super+shift+backspace / super+ctrl+backspace",
        "window transparency; window gaps; single-window square aspect",
        Unbound::HyprlandOnly,
    ),
    ("super+ctrl+delete / super+ctrl+alt+delete", "toggle the laptop display; toggle mirroring", Unbound::HyprlandOnly),
    ("super+ctrl+z / super+ctrl+alt+z", "cursor zoom in / reset", Unbound::HyprlandOnly),
    ("switch:on/off:Lid Switch", "run the lid-close and clamshell handlers", Unbound::NotAKey),
    // media.lua
    ("touchpad toggle / on / off", "enable and disable the touchpad", Unbound::HyprlandOnly),
    // voxtype.lua
    ("super+ctrl+x, f9", "voxtype dictation: toggle, and push-to-talk", Unbound::Conditional),
];

/// The `[commands]` table [`OMARCHY_BINDINGS`]' `run` actions name:
/// Omarchy's own commands, as argv.
///
/// Argv rather than command-line strings so nothing depends on how this
/// crate splits words — two of these carry a `||` that only a shell can
/// read, and are spelled as the `bash -lc` Omarchy itself uses
/// (`chonk_shell::omarchy_menu::action_argv`).
///
/// Every name is prefixed `omarchy-`: these are the guest desktop's
/// commands, the prefix says so in the docs table and in any warning
/// that names one, and it keeps the preset out of the short names a
/// user's own `[commands]` table wants. A user who declares one of
/// these names anyway wins — theirs is read from the file, after this.
pub const OMARCHY_COMMANDS: &[(&str, &[&str])] = &[
    ("omarchy-activity", &["omarchy-launch-tui", "btop"]),
    ("omarchy-agent", &["omarchy-agent", "--pick"]),
    ("omarchy-audio-output-switch", &["omarchy-audio-output-switch"]),
    ("omarchy-audio-source-switch", &["omarchy-audio-source-switch"]),
    ("omarchy-brightness-down", &["omarchy-brightness-display", "5%-"]),
    ("omarchy-brightness-down-fine", &["omarchy-brightness-display", "1%-"]),
    ("omarchy-brightness-max", &["omarchy-brightness-display", "100%"]),
    ("omarchy-brightness-min", &["omarchy-brightness-display", "1%"]),
    ("omarchy-brightness-up", &["omarchy-brightness-display", "+5%"]),
    ("omarchy-brightness-up-fine", &["omarchy-brightness-display", "+1%"]),
    ("omarchy-browser", &["omarchy-launch-browser"]),
    ("omarchy-browser-private", &["omarchy-launch-browser", "--private"]),
    ("omarchy-calculator", &["omacalc"]),
    ("omarchy-clipboard", &["omarchy-shell", "shell", "toggle", "omarchy.clipboard"]),
    // `pkill hyprpicker || hyprpicker -a`, Omarchy's own line. The
    // name carries Hyprland's prefix but the tool does not: hyprpicker
    // is an ordinary wlr-layer-shell + wlr-screencopy client, both of
    // which this compositor implements, and `omarchy_menu`'s
    // Hyprland-only filter deliberately does not match it either.
    ("omarchy-colorpicker", &["bash", "-lc", "pkill hyprpicker || hyprpicker -a"]),
    ("omarchy-editor", &["omarchy-launch-editor"]),
    ("omarchy-eject", &["eject"]),
    ("omarchy-emojis", &["omarchy-shell", "shell", "toggle", "omarchy.emojis"]),
    ("omarchy-files", &["omarchy-launch-nautilus"]),
    ("omarchy-files-here", &["omarchy-launch-nautilus-cwd"]),
    ("omarchy-kbd-brightness-cycle", &["omarchy-brightness-keyboard", "cycle"]),
    ("omarchy-kbd-brightness-down", &["omarchy-brightness-keyboard", "down"]),
    ("omarchy-kbd-brightness-up", &["omarchy-brightness-keyboard", "up"]),
    ("omarchy-keybindings-herdr", &["omarchy-menu-herdr-keybindings"]),
    ("omarchy-keybindings-tmux", &["omarchy-menu-tmux-keybindings"]),
    ("omarchy-lock", &["omarchy-system-lock"]),
    ("omarchy-media-next", &["omarchy-shell", "media", "next"]),
    ("omarchy-media-play-pause", &["omarchy-shell", "media", "playPause"]),
    ("omarchy-media-prev", &["omarchy-shell", "media", "previous"]),
    ("omarchy-menu", &["omarchy-menu", "toggle"]),
    ("omarchy-menu-apps", &["omarchy-menu", "toggle", "apps"]),
    ("omarchy-menu-background", &["omarchy-menu", "toggle", "background"]),
    ("omarchy-menu-capture", &["omarchy-menu", "toggle", "capture"]),
    ("omarchy-menu-hardware", &["omarchy-menu", "toggle", "hardware"]),
    ("omarchy-menu-share", &["omarchy-menu", "toggle", "share"]),
    ("omarchy-menu-system", &["omarchy-menu", "toggle", "system"]),
    ("omarchy-menu-theme", &["omarchy-menu", "toggle", "theme"]),
    ("omarchy-menu-toggles", &["omarchy-menu", "toggle", "toggle"]),
    ("omarchy-mic-mute", &["omarchy-audio-input-mute"]),
    ("omarchy-notification-dismiss", &["omarchy-shell", "notifications", "dismissOne"]),
    ("omarchy-notification-dismiss-all", &["omarchy-shell", "notifications", "dismissAll"]),
    ("omarchy-notification-history", &["omarchy-shell", "notifications", "showHistory"]),
    ("omarchy-notification-invoke", &["omarchy-shell", "notifications", "invokeLast"]),
    ("omarchy-notification-silence", &["omarchy-toggle-notification-silencing"]),
    ("omarchy-ocr", &["omarchy-capture-text"]),
    ("omarchy-panel-1", &["omarchy-shell", "-q", "shell", "togglePanelAt", "right", "1"]),
    ("omarchy-panel-2", &["omarchy-shell", "-q", "shell", "togglePanelAt", "right", "2"]),
    ("omarchy-panel-3", &["omarchy-shell", "-q", "shell", "togglePanelAt", "right", "3"]),
    ("omarchy-panel-4", &["omarchy-shell", "-q", "shell", "togglePanelAt", "right", "4"]),
    ("omarchy-panel-5", &["omarchy-shell", "-q", "shell", "togglePanelAt", "right", "5"]),
    ("omarchy-panel-6", &["omarchy-shell", "-q", "shell", "togglePanelAt", "right", "6"]),
    ("omarchy-panel-7", &["omarchy-shell", "-q", "shell", "togglePanelAt", "right", "7"]),
    ("omarchy-panel-8", &["omarchy-shell", "-q", "shell", "togglePanelAt", "right", "8"]),
    ("omarchy-panel-9", &["omarchy-shell", "-q", "shell", "togglePanelAt", "right", "9"]),
    ("omarchy-panel-audio", &["omarchy-shell", "shell", "toggle", "omarchy.audio"]),
    ("omarchy-panel-bluetooth", &["omarchy-shell", "shell", "toggle", "omarchy.bluetooth"]),
    ("omarchy-panel-clock", &["omarchy-shell", "shell", "toggle", "omarchy.clock"]),
    ("omarchy-panel-network", &["omarchy-shell", "shell", "toggle", "omarchy.network"]),
    ("omarchy-panel-power", &["omarchy-shell", "shell", "toggle", "omarchy.power"]),
    ("omarchy-reminder-clear", &["omarchy-reminder", "clear"]),
    ("omarchy-reminder-set", &["omarchy-menu", "toggle", "reminder-set"]),
    ("omarchy-reminder-show", &["omarchy-reminder", "show"]),
    // Omarchy's own line, `||` and all: stop a recording in progress,
    // or open the menu that starts one.
    (
        "omarchy-screenrecord",
        &[
            "bash",
            "-lc",
            "omarchy-capture-screenrecording --stop-recording || omarchy-menu toggle trigger.capture.screenrecord",
        ],
    ),
    ("omarchy-screenshot", &["omarchy-capture-screenshot"]),
    ("omarchy-show-battery", &["omarchy-notification-battery"]),
    ("omarchy-show-time", &["omarchy-notification-time"]),
    ("omarchy-show-weather", &["omarchy-notification-weather"]),
    ("omarchy-toggle-idle", &["omarchy-toggle-idle"]),
    ("omarchy-toggle-nightlight", &["omarchy-toggle-nightlight"]),
    ("omarchy-transcode", &["omarchy-transcode"]),
    ("omarchy-volume-down", &["omarchy-audio-output-volume", "lower"]),
    ("omarchy-volume-down-fine", &["omarchy-audio-output-volume", "-1"]),
    ("omarchy-volume-mute", &["omarchy-audio-output-volume", "mute-toggle"]),
    ("omarchy-volume-up", &["omarchy-audio-output-volume", "raise"]),
    ("omarchy-volume-up-fine", &["omarchy-audio-output-volume", "+1"]),
    ("omarchy-webcam-larger", &["omarchy-capture-webcam-resize", "larger"]),
    ("omarchy-webcam-smaller", &["omarchy-capture-webcam-resize", "smaller"]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    /// The whole table has to be real: every spec parses, every action
    /// name is known, and every `run` names a declared command. The
    /// panics in `omarchy_keybindings` cover the first two; this pins
    /// the third, which no parser would catch until a key was pressed.
    #[test]
    fn every_preset_binding_is_a_real_spec_naming_a_real_action() {
        let commands = omarchy_commands();
        for (combo, action) in omarchy_keybindings() {
            if let Action::Run(name) = &action {
                assert!(
                    commands.contains_key(name),
                    "{combo:?} runs undeclared command {name:?}"
                );
            }
        }
        // And nothing is declared that nothing runs: a dangling command
        // is a row in the docs table that no key reaches.
        let named: Vec<&str> = OMARCHY_BINDINGS
            .iter()
            .filter_map(|(_, action)| action.strip_prefix("run "))
            .collect();
        for (name, _) in OMARCHY_COMMANDS {
            assert!(
                named.contains(name),
                "[commands] entry {name:?} is not named by any binding"
            );
        }
    }

    /// No chord may be spelled twice, and no *action* may be reachable
    /// from two chords unless it is deliberately aliased. The first
    /// would silently drop a row; the second is Omarchy's own doing
    /// (their media keys alias, and `SUPER + SHIFT + B` repeats
    /// `SUPER + SHIFT + RETURN`) and is listed here so a new accidental
    /// alias has to be added on purpose.
    #[test]
    fn the_preset_binds_no_chord_twice_and_no_action_by_accident() {
        let mut seen = std::collections::HashSet::new();
        for (spec, _) in OMARCHY_BINDINGS {
            let combo = parse_key(spec).expect("a real spec");
            assert!(seen.insert(combo), "{spec} is bound twice");
        }
        assert_eq!(seen.len(), OMARCHY_BINDINGS.len());

        // The intended aliases, as (action, how many chords reach it).
        let expected: BTreeMap<&str, usize> = [
            ("run omarchy-browser", 2),
            ("run omarchy-menu", 2),
            ("run omarchy-menu-system", 2),
            ("run omarchy-calculator", 2),
            ("run omarchy-media-play-pause", 2),
            ("run omarchy-media-next", 2),
            ("run omarchy-media-prev", 2),
            ("run omarchy-audio-source-switch", 2),
        ]
        .into_iter()
        .collect();
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, action) in OMARCHY_BINDINGS {
            *counts.entry(action).or_default() += 1;
        }
        for (action, count) in counts {
            let allowed = expected.get(action).copied().unwrap_or(1);
            assert_eq!(
                count, allowed,
                "{action} is reachable from {count} chords, expected {allowed}"
            );
        }
    }

    /// The chords the docs promise are dead really are dead. Written
    /// against the same list the docs table is transcribed from, so the
    /// promise and the table cannot drift apart.
    #[test]
    fn every_chord_declared_unbound_is_unbound() {
        let bound: std::collections::HashSet<wm_core::KeyCombo> = omarchy_keybindings()
            .into_iter()
            .map(|(combo, _)| combo)
            .collect();
        // Only the entries that name a single literal chord can be
        // checked mechanically; the rest describe a family (`super+1..0`)
        // and are expanded here by hand where a family is checkable.
        let literal = [
            "super+j",
            "super+p",
            "super+t",
            "super+ctrl+f",
            "super+o",
            "super+home",
            "super+alt+home",
            "super+l",
            "super+g",
            "super+alt+g",
            "super+alt+tab",
            "super+alt+shift+tab",
            "super+ctrl+left",
            "super+ctrl+right",
            "super+shift+left",
            "super+shift+right",
            "super+shift+up",
            "super+shift+down",
            "super+minus",
            "super+equal",
            "super+s",
            "super+ctrl+tab",
            "ctrl+alt+tab",
            "super+k",
            "super+shift+space",
            "super+ctrl+d",
            "super+ctrl+x",
            "f9",
            "super+c",
            "super+v",
            "super+x",
        ];
        for spec in literal {
            let combo = parse_key(spec).unwrap_or_else(|| panic!("{spec} should parse"));
            assert!(
                !bound.contains(&combo),
                "{spec} is documented unbound but the preset binds it"
            );
        }
        // The group-focus digit family stays dead, every member.
        for digit in '1'..='9' {
            let spec = format!("super+alt+{digit}");
            let combo = parse_key(&spec).unwrap();
            assert!(
                !bound.contains(&combo),
                "{spec} is documented unbound but the preset binds it"
            );
        }
        // ...and the three that are alive, asserted rather than left to
        // the gap between the lists: `super+n` is workspace n and
        // `super+shift+n` carries the window there, while adding Alt
        // sends it without following. All count from one with zero as
        // the tenth, which is where Omarchy puts them.
        let by_number = |spec: &str| {
            let combo = parse_key(spec).unwrap();
            omarchy_keybindings()
                .into_iter()
                .find(|(c, _)| *c == combo)
                .map(|(_, a)| a)
        };
        for digit in 1..=10 {
            let key = if digit == 10 {
                "0".to_string()
            } else {
                digit.to_string()
            };
            assert_eq!(
                by_number(&format!("super+{key}")),
                Some(Action::Workspace(digit - 1))
            );
            assert_eq!(
                by_number(&format!("super+shift+{key}")),
                Some(Action::WorkspaceCarry(digit - 1))
            );
            assert_eq!(
                by_number(&format!("super+shift+alt+{key}")),
                Some(Action::WorkspaceSend(digit - 1))
            );
        }
        for direction in ["left", "right", "up", "down"] {
            assert!(
                bound.contains(&parse_key(&format!("super+{direction}")).unwrap()),
                "directional focus must survive in the frozen fallback too"
            );
        }
    }

    /// The posture, resolved: what `desktop = "omarchy"` is worth as
    /// one line.
    #[test]
    fn the_omarchy_desktop_preset_resolves_to_the_documented_defaults() {
        let config = parse("desktop = \"omarchy\"").expect("the preset must parse");
        assert_eq!(config.desktop, Desktop::Omarchy);
        assert!(!config.show_dock, "the Dock steps aside for Omarchy's bar");
        assert_eq!(
            config.omarchy_bar,
            Some(true),
            "and the bar it steps aside for is shown"
        );
        assert_eq!(
            config.theme.as_deref(),
            Some("omarchy"),
            "the desk follows Omarchy's theme"
        );
        assert_eq!(
            config.lock_command.as_deref(),
            Some("omarchy-system-lock"),
            "crash recovery uses Omarchy's own lock entry point"
        );
        assert!(
            config.omarchy_menu && config.omarchy_shell,
            "the menu and the shell are hosted"
        );
        // One line also picks the keymap up.
        assert_eq!(config.keymap, Keymap::Omarchy);
        assert_eq!(config.keybindings.len(), OMARCHY_BINDINGS.len());
        assert!(config.commands.contains_key("omarchy-menu"));
    }

    /// The chonkstep posture is what every existing config file already
    /// means: naming it explicitly must change nothing at all.
    #[test]
    fn the_chonkstep_posture_is_exactly_the_built_in_defaults() {
        for text in ["", "desktop = \"chonkstep\"", "keymap = \"chonkstep\""] {
            let config = parse(text).unwrap();
            let default = Config::default_config();
            assert!(config.show_dock, "text {text:?}");
            assert_eq!(config.omarchy_bar, None, "text {text:?}");
            assert_eq!(config.theme, None, "text {text:?}");
            assert_eq!(config.keybindings, default.keybindings, "text {text:?}");
            assert!(config.commands.is_empty(), "text {text:?}");
        }
    }

    /// A preset is a set of defaults, never a lock. Every key the
    /// Omarchy posture touches, overridden one at a time.
    #[test]
    fn an_explicit_setting_always_beats_a_preset_default() {
        let config = parse("desktop = \"omarchy\"\nshow_dock = true").unwrap();
        assert!(config.show_dock, "the user asked for the Dock back");

        let config = parse("desktop = \"omarchy\"\nomarchy_bar = false").unwrap();
        assert_eq!(config.omarchy_bar, Some(false));

        let config = parse("desktop = \"omarchy\"\ntheme = \"amber-phosphor\"").unwrap();
        assert_eq!(config.theme.as_deref(), Some("amber-phosphor"));

        let config =
            parse("desktop = \"omarchy\"\nlock_command = \"swaylock --daemonize\"").unwrap();
        assert_eq!(config.lock_command.as_deref(), Some("swaylock --daemonize"));

        let config = parse("desktop = \"omarchy\"\nomarchy_shell = false").unwrap();
        assert!(!config.omarchy_shell);

        let config = parse("desktop = \"omarchy\"\nomarchy_menu = false").unwrap();
        assert!(!config.omarchy_menu);

        // The keymap the posture defaults to, taken back on its own.
        let config = parse("desktop = \"omarchy\"\nkeymap = \"chonkstep\"").unwrap();
        assert_eq!(config.keymap, Keymap::Chonkstep);
        assert_eq!(config.keybindings, Config::default_config().keybindings);
        assert!(config.commands.is_empty(), "and its commands go with it");
        // ...while the rest of the posture stays.
        assert!(!config.show_dock);

        // And the keymap without the posture.
        let config = parse("keymap = \"omarchy\"").unwrap();
        assert_eq!(
            (config.desktop, config.keymap),
            (Desktop::Chonkstep, Keymap::Omarchy)
        );
        assert!(
            config.show_dock,
            "the keymap says nothing about the furniture"
        );
        assert_eq!(config.keybindings.len(), OMARCHY_BINDINGS.len());
    }

    /// Individual bindings and commands override the preset the same
    /// way they override the built-in defaults — per entry, with
    /// everything unlisted surviving.
    #[test]
    fn user_bindings_and_commands_merge_over_the_preset() {
        let config = parse(
            r#"
            keymap = "omarchy"
            [commands]
            omarchy-menu = "my-own-launcher"
            [keybindings]
            "super+space" = "none"
            "super+shift+m" = "miniaturize"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.commands.get("omarchy-menu").map(Vec::as_slice),
            Some(["my-own-launcher".to_string()].as_slice()),
            "the user's command of that name wins"
        );
        let has = |spec: &str| {
            config
                .keybindings
                .iter()
                .any(|(c, _)| *c == parse_key(spec).unwrap())
        };
        assert!(!has("super+space"), "\"none\" unbinds a preset binding");
        assert!(has("super+shift+m"), "and a new one is added");
        assert!(has("super+w"), "while the rest of the preset survives");
    }

    /// A typo'd posture costs the user the preset, not the session.
    #[test]
    fn an_unknown_or_wrongly_typed_preset_name_keeps_the_default() {
        for text in ["desktop = \"omarhcy\"", "desktop = true", "desktop = \"\""] {
            let config = parse(text).unwrap();
            assert_eq!(config.desktop, Desktop::Chonkstep, "text {text:?}");
            assert!(config.show_dock, "text {text:?}");
        }
        for text in ["keymap = \"hyprland\"", "keymap = 3"] {
            let config = parse(text).unwrap();
            assert_eq!(config.keymap, Keymap::Chonkstep, "text {text:?}");
        }
        // A bad `keymap` beside a good `desktop` still gets the
        // posture's own keymap, not the built-in one: the fallback is
        // "keep the default", and the posture *is* the default here.
        let config = parse("desktop = \"omarchy\"\nkeymap = \"hyprland\"").unwrap();
        assert_eq!(config.keymap, Keymap::Omarchy);
    }

    /// Names round-trip, and are read the way every other name in this
    /// format is: trimmed, case-insensitively.
    #[test]
    fn posture_names_round_trip_and_are_case_insensitive() {
        for desktop in [Desktop::Chonkstep, Desktop::Omarchy] {
            assert_eq!(Desktop::from_name(desktop.id()), Some(desktop));
        }
        for keymap in [Keymap::Chonkstep, Keymap::Omarchy] {
            assert_eq!(Keymap::from_name(keymap.id()), Some(keymap));
        }
        assert_eq!(Desktop::from_name("  Omarchy \n"), Some(Desktop::Omarchy));
        assert_eq!(Keymap::from_name("OMARCHY"), Some(Keymap::Omarchy));
    }

    /// The headline chord, end to end through the real parser: an
    /// Omarchy user's `SUPER + RETURN` spawns a terminal, and their
    /// `SUPER + SPACE` runs Omarchy's own menu command.
    #[test]
    fn the_chords_an_omarchy_user_arrives_with_do_the_chonkstep_thing() {
        let config = parse("desktop = \"omarchy\"").unwrap();
        let action = |spec: &str| {
            let combo = parse_key(spec).unwrap();
            config
                .keybindings
                .iter()
                .find(|(c, _)| *c == combo)
                .map(|(_, a)| a.clone())
        };
        assert_eq!(action("super+return"), Some(Action::SpawnTerminal));
        assert_eq!(action("super+w"), Some(Action::Close));
        assert_eq!(action("super+f"), Some(Action::ToggleFullscreen));
        assert_eq!(action("super+alt+f"), Some(Action::ToggleMaximize));
        assert_eq!(action("super+tab"), Some(Action::WorkspaceNext));
        assert_eq!(action("super+shift+tab"), Some(Action::WorkspacePrev));
        assert_eq!(action("super+alt+s"), Some(Action::Miniaturize));
        assert_eq!(
            action("super+space"),
            Some(Action::Run("omarchy-menu".to_string()))
        );
        assert_eq!(
            config.commands.get("omarchy-menu").map(Vec::as_slice),
            Some(["omarchy-menu".to_string(), "toggle".to_string()].as_slice())
        );
        // And the chonkstep chords are gone, not merged alongside.
        assert_eq!(action("alt+shift+return"), None);
        assert_eq!(action("alt+shift+q"), None);
    }
}
