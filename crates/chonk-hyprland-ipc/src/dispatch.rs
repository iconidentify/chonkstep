//! Dispatch: turning Hyprland's verbs into chonkstep's, or refusing.
//!
//! # The rule this module exists to enforce
//!
//! **A verb we cannot honour must fail, loudly, rather than succeed
//! plausibly.**
//!
//! chonkstep is a floating window manager. Hyprland is a tiling one, and
//! a large part of its dispatch vocabulary — `layoutmsg`, `togglesplit`,
//! `swapwindow`, `pseudo`, `movewindow l` — means nothing here. The
//! tempting thing is to answer `ok` and move on, because `ok` is what
//! callers expect and nothing visibly breaks.
//!
//! It is the wrong thing, and the reason is worth being concrete about.
//! A script that gets `ok` from `togglesplit` believes the layout
//! changed and takes its next branch accordingly; the mistake is now
//! invisible and permanent, and it surfaces later as behaviour the user
//! cannot explain.
//!
//! Omarchy often appears to provide a fallback:
//!
//! ```sh
//! hyprctl dispatch "hl.dsp.focus({ window = \"address:$ADDR\" })" \
//!   || hyprctl dispatch focuswindow "address:$ADDR"
//! ```
//!
//! But `hyprctl` 0.56.2 exits zero regardless of the response text, so
//! that `||` branch is dead when the refusal is discarded. Refusal is
//! therefore not treated as a compatibility mechanism: caller-visible
//! paths are implemented, hidden from chonkstep-owned menus, or tracked
//! as a bug. It remains the only truthful protocol answer for a request
//! with no meaning on this floating desktop.
//!
//! So: [`Outcome::Unsupported`] is a first-class result here, not a
//! shortfall, and it is reported to the caller as an error string
//! beginning with `Invalid dispatcher`. The server logs and counts each
//! one because most non-interactive callers will otherwise hide it.

use crate::state::{workspace_index_from_hypr_id, Snapshot, Window};

/// What a dispatch request asks chonkstep to do.
///
/// Deliberately chonkstep's vocabulary, not Hyprland's: this is the
/// point where the translation is finished. The host maps these onto
/// `WindowManager` calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Switch to a workspace, by **0-based chonkstep index**.
    FocusWorkspace(usize),
    /// Focus a specific window, by `ClientId::as_u64()`.
    FocusWindow(u64),
    /// Focus the nearest visible window in a root-coordinate direction.
    FocusDirection(Direction),
    /// Close a specific window.
    CloseWindow(u64),
    /// Close the focused window.
    KillActive,
    /// Move a window (or the focused one) to a 0-based workspace index.
    /// `follow` distinguishes Hyprland's ordinary and `silent` verbs.
    MoveToWorkspace { window: Option<u64>, workspace: usize, follow: bool },
    /// Run a command line through the user's POSIX shell. This is the
    /// spelling used by Lua's `hl.dsp.exec_cmd`, whose single string is
    /// explicitly shell source.
    ExecShell(String),
    /// Execute an argv exactly. `hyprctl dispatch exec -- <argv...>`
    /// removes its `--` client-side and flattens the arguments on the
    /// wire, so the private `classic_exec` reconstructs the only unambiguous
    /// direct-argv forms before this crosses into the compositor.
    ExecArgv(Vec<String>),
    /// Set or toggle fullscreen on the focused window.
    Fullscreen(Fullscreen),
    /// Focus the next/previous window.
    CycleFocus { forward: bool },
    MoveWindow { window: u64, x: i32, y: i32, relative: bool },
    ResizeWindow { window: u64, width: i32, height: i32, relative: bool },
    CenterWindow(u64),
    RaiseWindow(u64),
    SetPinned { window: u64, pinned: Option<bool> },
    SetTag { window: u64, tag: String, present: bool },
    /// Scale in protocol units (120 == 1.0), avoiding floating-point
    /// equality in an action that is compared in conformance tests.
    SetMonitorScale { output: String, scale_120: u32 },
    /// Power one named output, or every output when `output` is `None`.
    SetDpms { output: Option<String>, powered: bool },
    /// Select a group from the seat keymap. Hyprland accepts next,
    /// previous, or a zero-based numeric group.
    SwitchKeyboardLayout { device: String, target: LayoutTarget },
    /// Hide or restore the compositor-owned pointer image. This is a
    /// live session property used by Omarchy's screensaver, not a
    /// persisted Hyprland configuration mutation.
    SetCursorHidden(bool),
    ReloadConfig,
    SetDiagnostic { name: String, enabled: bool },
    SetLogFilter(String),
    /// The requested window is already floating; applying this still
    /// validates that the target survived until the action ran.
    ConfirmFloating(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fullscreen {
    Toggle,
    On,
    Off,
}

/// A root-coordinate direction, kept protocol-local so this crate
/// remains independent of `wm-core` as promised by its public design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutTarget {
    Next,
    Previous,
    Index(u32),
}

/// The result of parsing one dispatch request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Understood, and chonkstep can do it.
    Run(Action),
    /// Understood, and chonkstep cannot do it. The string says why, in
    /// terms of what chonkstep is rather than what it lacks.
    Unsupported(String),
    /// Not understood at all.
    Unknown(String),
}

impl Outcome {
    /// The line Hyprland would put on the wire for this outcome.
    ///
    /// `ok` is Hyprland's success response and callers test for it;
    /// failures begin with `Invalid dispatcher` for the same reason.
    pub fn response(&self) -> String {
        match self {
            Outcome::Run(_) => "ok".to_string(),
            Outcome::Unsupported(why) | Outcome::Unknown(why) => {
                format!("Invalid dispatcher: {why}")
            }
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Outcome::Run(_))
    }
}

/// Every tiling-only verb in Hyprland's dispatch table, with the reason
/// chonkstep cannot honour it.
///
/// Listing them explicitly — rather than letting them fall through to
/// "unknown dispatcher" — is the difference between "chonkstep does not
/// recognise this word" and "chonkstep understands exactly what you
/// asked for and is not able to do it". The second is a much better
/// error to read at 2am, and it is the one that tells a script author
/// their fallback path is the right one to write.
const TILING_ONLY: &[(&str, &str)] = &[
    ("layoutmsg", "chonkstep has no tiling layout to message"),
    ("togglesplit", "chonkstep has no split direction; every window floats"),
    ("swapsplit", "chonkstep has no split direction; every window floats"),
    ("swapwindow", "chonkstep has no tiling order to swap within"),
    ("swapnext", "chonkstep has no tiling order to swap within"),
    ("pseudo", "pseudotiling is meaningless in a floating window manager"),
    ("togglegroup", "chonkstep has no window groups"),
    ("changegroupactive", "chonkstep has no window groups"),
    ("moveintogroup", "chonkstep has no window groups"),
    ("moveoutofgroup", "chonkstep has no window groups"),
    ("lockgroups", "chonkstep has no window groups"),
    ("togglespecialworkspace", "chonkstep has no special (scratchpad) workspaces"),
    ("workspaceopt", "chonkstep has no per-workspace layout options"),
    ("submap", "chonkstep's keybindings do not have submaps"),
];

/// Verbs chonkstep understands but which target something it does not
/// model, listed separately from the tiling ones because the reason is
/// different and a reader deserves to know which kind of "no" this is.
const NOT_MODELLED: &[(&str, &str)] = &[
    ("settiled", "chonkstep cannot tile a window"),
];

/// Parse a dispatch argument string.
///
/// `args` is everything after `dispatch` — either a classic verb with
/// its own arguments (`workspace 3`) or a Lua call
/// (`hl.dsp.focus({ workspace = "3" })`).
pub fn parse(args: &str, snapshot: &Snapshot) -> Outcome {
    let args = args.trim();
    if args.is_empty() {
        return Outcome::Unknown("empty dispatch".to_string());
    }

    // Omarchy 4 configures Hyprland in Lua and writes dispatch as Lua.
    // Recognising it is not optional: it is the *first* form every
    // Omarchy script and the bar's workspace widget sends.
    if let Some(rest) = args.strip_prefix("hl.dsp.") {
        return parse_lua(rest, snapshot);
    }
    // `hl.dispatch(hl.dsp....)` is the form `omarchy-capture-region`
    // sends through `hyprctl eval`.
    if let Some(rest) = args.strip_prefix("hl.dispatch(hl.dsp.") {
        return parse_lua(rest.trim_end_matches(')'), snapshot);
    }

    let (verb, rest) = split_verb(args);
    parse_classic(&verb, rest, snapshot)
}

fn split_verb(args: &str) -> (String, &str) {
    match args.find(char::is_whitespace) {
        Some(space) => (args[..space].to_ascii_lowercase(), args[space + 1..].trim()),
        None => (args.to_ascii_lowercase(), ""),
    }
}

fn parse_classic(verb: &str, rest: &str, snapshot: &Snapshot) -> Outcome {
    if let Some((_, why)) = TILING_ONLY.iter().find(|(name, _)| *name == verb) {
        return Outcome::Unsupported((*why).to_string());
    }
    if let Some((_, why)) = NOT_MODELLED.iter().find(|(name, _)| *name == verb) {
        return Outcome::Unsupported((*why).to_string());
    }

    match verb {
        "workspace" => match workspace_target(rest, snapshot) {
            Ok(index) => Outcome::Run(Action::FocusWorkspace(index)),
            Err(why) => Outcome::Unsupported(why),
        },
        "focuswindow" => match resolve_window(rest, snapshot) {
            Some(window) => Outcome::Run(Action::FocusWindow(window.id)),
            None => Outcome::Unsupported(format!("no window matches {rest:?}")),
        },
        "movefocus" => match rest.trim().to_ascii_lowercase().as_str() {
            "l" | "left" => Outcome::Run(Action::FocusDirection(Direction::Left)),
            "r" | "right" => Outcome::Run(Action::FocusDirection(Direction::Right)),
            "u" | "up" => Outcome::Run(Action::FocusDirection(Direction::Up)),
            "d" | "down" => Outcome::Run(Action::FocusDirection(Direction::Down)),
            other => Outcome::Unsupported(format!("unknown focus direction {other:?}")),
        },
        "closewindow" => match resolve_window(rest, snapshot) {
            Some(window) => Outcome::Run(Action::CloseWindow(window.id)),
            None => Outcome::Unsupported(format!("no window matches {rest:?}")),
        },
        "killactive" => Outcome::Run(Action::KillActive),
        "movetoworkspace" | "movetoworkspacesilent" => {
            // `movetoworkspace 3`, its `silent` counterpart, or either
            // spelling with `,address:0x...` selecting a window.
            let follow = verb == "movetoworkspace";
            let (target, window) = match rest.split_once(',') {
                Some((target, window)) => (target.trim(), Some(window.trim())),
                None => (rest, None),
            };
            let workspace = match workspace_target(target, snapshot) {
                Ok(index) => index,
                Err(why) => return Outcome::Unsupported(why),
            };
            let window = match window {
                None => None,
                Some(selector) => match resolve_window(selector, snapshot) {
                    Some(window) => Some(window.id),
                    None => return Outcome::Unsupported(format!("no window matches {selector:?}")),
                },
            };
            Outcome::Run(Action::MoveToWorkspace { window, workspace, follow })
        }
        "exec" => classic_exec(rest),
        "fullscreen" => Outcome::Run(Action::Fullscreen(match rest.trim() {
            "" | "0" => Fullscreen::Toggle,
            // Hyprland's `1` is "maximize to the window's monitor",
            // which for a floating window manager with no tiling to
            // return to is the same operation as fullscreen.
            "1" | "2" => Fullscreen::On,
            _ => Fullscreen::Toggle,
        })),
        "fullscreenstate" => {
            let client = rest.split_whitespace().nth(1).unwrap_or("0");
            Outcome::Run(Action::Fullscreen(if client == "0" { Fullscreen::Off } else { Fullscreen::On }))
        }
        "cyclenext" => Outcome::Run(Action::CycleFocus { forward: !rest.contains("prev") }),
        "resizeactive" => classic_geometry(rest, snapshot, true, true),
        "resizewindowpixel" => classic_geometry(rest, snapshot, true, false),
        "moveactive" => classic_geometry(rest, snapshot, false, true),
        "movewindowpixel" => classic_geometry(rest, snapshot, false, false),
        "centerwindow" => selected_window(rest, snapshot)
            .map(|window| Outcome::Run(Action::CenterWindow(window.id)))
            .unwrap_or_else(|| Outcome::Unsupported(format!("no window matches {rest:?}"))),
        "alterzorder" => {
            let mut fields = rest.split_whitespace();
            let mode = fields.next().unwrap_or("");
            let selector = fields.collect::<Vec<_>>().join(" ");
            if mode != "top" {
                Outcome::Unsupported(format!("alterzorder mode {mode:?} is not supported; only top is available"))
            } else {
                selected_window(&selector, snapshot)
                    .map(|window| Outcome::Run(Action::RaiseWindow(window.id)))
                    .unwrap_or_else(|| Outcome::Unsupported(format!("no window matches {selector:?}")))
            }
        }
        "pin" => selected_window(rest, snapshot)
            .map(|window| Outcome::Run(Action::SetPinned { window: window.id, pinned: None }))
            .unwrap_or_else(|| Outcome::Unsupported(format!("no window matches {rest:?}"))),
        "togglefloating" | "setfloating" => selected_window(rest, snapshot)
            .map(|window| Outcome::Run(Action::ConfirmFloating(window.id)))
            .unwrap_or_else(|| Outcome::Unsupported(format!("no window matches {rest:?}"))),
        "tagwindow" => classic_tag(rest, snapshot),
        "dpms" => parse_dpms(rest, snapshot),
        "focusmonitor" | "movecurrentworkspacetomonitor" | "focuswindowbyclass" => {
            Outcome::Unsupported(format!("{verb} is not implemented yet"))
        }
        other => Outcome::Unknown(format!("unknown dispatcher {other:?}")),
    }
}

/// Parse the Lua dispatch forms Omarchy 4 actually sends.
///
/// This is not a Lua interpreter and does not try to be. It recognises
/// the handful of shapes that appear in Omarchy's source and rejects
/// everything else *as unsupported rather than as understood*, which is
/// the safe direction: a Lua call we mis-parse into a plausible action
/// would be exactly the confident wrong answer this module forbids.
fn parse_lua(rest: &str, snapshot: &Snapshot) -> Outcome {
    let (path, body) = match rest.split_once('(') {
        Some((path, body)) => (path.trim(), body.trim_end().trim_end_matches(')')),
        None => (rest.trim(), ""),
    };

    match path {
        "focus" => {
            if let Some(value) = lua_field(body, "workspace") {
                return match workspace_target(&value, snapshot) {
                    Ok(index) => Outcome::Run(Action::FocusWorkspace(index)),
                    Err(why) => Outcome::Unsupported(why),
                };
            }
            if let Some(value) = lua_field(body, "window") {
                return match resolve_window(&value, snapshot) {
                    Some(window) => Outcome::Run(Action::FocusWindow(window.id)),
                    None => Outcome::Unsupported(format!("no window matches {value:?}")),
                };
            }
            Outcome::Unknown("hl.dsp.focus with no workspace or window".to_string())
        }
        "window.close" => match lua_field(body, "window") {
            Some(value) => match resolve_window(&value, snapshot) {
                Some(window) => Outcome::Run(Action::CloseWindow(window.id)),
                None => Outcome::Unsupported(format!("no window matches {value:?}")),
            },
            None => Outcome::Run(Action::KillActive),
        },
        "exec_cmd" => match lua_field(body, "cmd").or_else(|| lua_string(body)) {
            Some(command) => Outcome::Run(Action::ExecShell(command)),
            None => Outcome::Unknown("hl.dsp.exec_cmd with no command".to_string()),
        },
        "window.float" => lua_window(body, snapshot, |window| Action::ConfirmFloating(window.id)),
        "window.pin" => lua_window(body, snapshot, |window| Action::SetPinned { window: window.id, pinned: None }),
        "window.resize" => lua_geometry(body, snapshot, true),
        "window.move" => lua_geometry(body, snapshot, false),
        "window.center" => lua_window(body, snapshot, |window| Action::CenterWindow(window.id)),
        "window.alter_zorder" => {
            if lua_field(body, "mode").as_deref() != Some("top") {
                Outcome::Unsupported("hl.dsp.window.alter_zorder supports mode=top only".to_string())
            } else {
                lua_window(body, snapshot, |window| Action::RaiseWindow(window.id))
            }
        }
        "window.tag" => {
            let Some(tag) = lua_field(body, "tag") else {
                return Outcome::Unknown("hl.dsp.window.tag with no tag".to_string());
            };
            let (present, tag) = match tag.strip_prefix('-') {
                Some(tag) => (false, tag.to_string()),
                None => (true, tag.trim_start_matches('+').to_string()),
            };
            lua_window(body, snapshot, |window| Action::SetTag { window: window.id, tag, present })
        }
        "window.fullscreen_state" => {
            let client = lua_field(body, "client").and_then(|value| value.parse::<i32>().ok()).unwrap_or(0);
            Outcome::Run(Action::Fullscreen(if client == 0 { Fullscreen::Off } else { Fullscreen::On }))
        }
        "window.set_prop" => Outcome::Unsupported("window opacity and other dynamic properties are not modeled".to_string()),
        "cursor.move" => Outcome::Unsupported("chonkstep does not warp the pointer from IPC".to_string()),
        "dpms" => parse_dpms_lua(body, snapshot),
        other => Outcome::Unknown(format!("unknown Lua dispatcher hl.dsp.{other}")),
    }
}

/// Parse an expression sent through `hyprctl eval`. Eval is mutation in
/// Omarchy's Lua configuration API; known families are either lowered
/// to an action or refused by name, never misreported as an unknown
/// request.
pub fn parse_eval(source: &str, snapshot: &Snapshot) -> Outcome {
    let source = source.trim();
    if source.starts_with("hl.dispatch(hl.dsp.") {
        return parse(source, snapshot);
    }
    if let Some(body) = source.strip_prefix("hl.monitor(").and_then(|value| value.strip_suffix(')')) {
        let Some(output) = lua_field(body, "output") else {
            return Outcome::Unsupported("hl.monitor requires a named output".to_string());
        };
        if !snapshot.monitors.iter().any(|monitor| monitor.name == output) {
            return Outcome::Unsupported(format!("hl.monitor names unknown output {output:?}"));
        }
        let Some(scale) = lua_field(body, "scale").and_then(|value| value.parse::<f64>().ok()) else {
            return Outcome::Unsupported("hl.monitor currently changes scale only, and needs a numeric scale".to_string());
        };
        if !scale.is_finite() || !(0.5..=4.0).contains(&scale) {
            return Outcome::Unsupported("hl.monitor scale must be between 0.5 and 4".to_string());
        }
        return Outcome::Run(Action::SetMonitorScale { output, scale_120: (scale * 120.0).round() as u32 });
    }
    if let Some(body) = source.strip_prefix("hl.config(").and_then(|value| value.strip_suffix(')')) {
        if let Some(cursor) = lua_table_field(body, "cursor") {
            if let Some(value) = lua_field(cursor, "invisible") {
                return match parse_bool(&value) {
                    Some(hidden) => Outcome::Run(Action::SetCursorHidden(hidden)),
                    None => Outcome::Unsupported(
                        "hl.config cursor.invisible requires true or false".to_string(),
                    ),
                };
            }
        }
        return Outcome::Unsupported("hl.config property mutation is not supported by chonkstep".to_string());
    }
    if source.starts_with("hl.device(") {
        return Outcome::Unsupported("hl.device enable/disable is not supported by this input backend".to_string());
    }
    if source.starts_with("hl.workspace_rule(") {
        return Outcome::Unsupported("chonkstep is floating-only and cannot apply a tiled workspace layout".to_string());
    }
    Outcome::Unknown(format!("unknown eval expression {source:?}"))
}

/// Parse the one live `keyword` mutation chonkstep deliberately
/// supports. The broad namespace remains a refusal; this exception is
/// the exact fallback shipped by Omarchy's screensaver.
pub fn parse_keyword(source: &str) -> Outcome {
    let source = source.trim();
    if let Some(spec) = source.strip_prefix("monitor ") {
        if let Some((name, operation)) = spec.split_once(',') {
            if operation.trim().eq_ignore_ascii_case("disable") {
                return Outcome::Unsupported(format!(
                    "output {:?} cannot be disabled: chonkstep keeps every connected output in the desktop layout; configure persistent layout in ~/.config/hypr with hl.monitor, or use `hyprctl dispatch dpms off {}` for temporary power-off",
                    name.trim(), name.trim()
                ));
            }
        }
    }
    let mut fields = source.split_whitespace();
    match (fields.next(), fields.next(), fields.next()) {
        (Some("cursor:invisible"), Some(value), None) => match parse_bool(value) {
            Some(hidden) => Outcome::Run(Action::SetCursorHidden(hidden)),
            None => Outcome::Unsupported(
                "keyword cursor:invisible requires true or false".to_string(),
            ),
        },
        _ => Outcome::Unsupported(
            "keyword does not mutate chonkstep's configuration. \
             chonkstep reads ~/.config/hypr and re-reads it within a second of an edit, \
             so edit the file instead, or use `hyprctl eval hl.monitor({...})` for a live \
             scale change. `keyword monitor NAME,disable` cannot work at all: chonkstep \
             drives every connected output and has no disable path."
                .to_string(),
        ),
    }
}

/// Parse `switchxkblayout DEVICE next|prev|N` after the request table
/// has separated the command name from its arguments.
pub fn parse_switch_keyboard_layout(source: &str, snapshot: &Snapshot) -> Outcome {
    let mut fields = source.split_whitespace();
    let Some(device) = fields.next() else {
        return Outcome::Unsupported("switchxkblayout requires a device and layout".to_string());
    };
    let Some(target) = fields.next() else {
        return Outcome::Unsupported("switchxkblayout requires next, prev, or a layout index".to_string());
    };
    if fields.next().is_some() {
        return Outcome::Unsupported("switchxkblayout accepts exactly one device and one layout".to_string());
    }
    if device != "all" && !snapshot.devices.keyboards.iter().any(|keyboard| keyboard.name == device) {
        return Outcome::Unsupported(format!("switchxkblayout names unknown keyboard {device:?}"));
    }
    let target = match target.to_ascii_lowercase().as_str() {
        "next" => LayoutTarget::Next,
        "prev" | "previous" => LayoutTarget::Previous,
        value => match value.parse::<u32>() {
            Ok(index) => LayoutTarget::Index(index),
            Err(_) => return Outcome::Unsupported("layout must be next, prev, or a zero-based index".to_string()),
        },
    };
    Outcome::Run(Action::SwitchKeyboardLayout { device: device.to_string(), target })
}

fn parse_dpms(source: &str, snapshot: &Snapshot) -> Outcome {
    let mut fields = source.split_whitespace();
    let Some(state) = fields.next() else {
        return Outcome::Unsupported("dpms requires on, off, or toggle".to_string());
    };
    let output = fields.next().map(str::to_string);
    if fields.next().is_some() {
        return Outcome::Unsupported("dpms accepts one optional output name".to_string());
    }
    if output.as_deref().is_some_and(|name| !snapshot.monitors.iter().any(|monitor| monitor.name == name)) {
        return Outcome::Unsupported(format!("dpms names unknown output {:?}", output.as_deref().unwrap_or_default()));
    }
    let powered = match state.to_ascii_lowercase().as_str() {
        "on" => true,
        "off" => false,
        "toggle" => {
            let current = output.as_deref()
                .and_then(|name| snapshot.monitors.iter().find(|monitor| monitor.name == name))
                .or_else(|| snapshot.focused_monitor())
                .is_none_or(|monitor| monitor.powered);
            !current
        }
        _ => return Outcome::Unsupported("dpms state must be on, off, or toggle".to_string()),
    };
    Outcome::Run(Action::SetDpms { output, powered })
}

fn parse_dpms_lua(body: &str, snapshot: &Snapshot) -> Outcome {
    let state = lua_field(body, "state")
        .or_else(|| lua_field(body, "enabled"))
        .or_else(|| lua_field(body, "action"))
        .or_else(|| lua_string(body));
    let Some(state) = state else {
        return Outcome::Unsupported("hl.dsp.dpms requires state=on or state=off".to_string());
    };
    let state = match state.to_ascii_lowercase().as_str() {
        "enable" | "enabled" => "on",
        "disable" | "disabled" => "off",
        _ => state.as_str(),
    };
    let output = lua_field(body, "output").or_else(|| lua_field(body, "monitor"));
    parse_dpms(&format!("{}{}", state, output.map_or_else(String::new, |name| format!(" {name}"))), snapshot)
}

fn selected_window<'a>(selector: &str, snapshot: &'a Snapshot) -> Option<&'a Window> {
    if selector.trim().is_empty() {
        snapshot.focused_window()
    } else {
        resolve_window(selector.trim(), snapshot)
    }
}

fn lua_window<F>(body: &str, snapshot: &Snapshot, action: F) -> Outcome
where
    F: FnOnce(&Window) -> Action,
{
    let window = lua_field(body, "window")
        .as_deref()
        .and_then(|selector| resolve_window(selector, snapshot))
        .or_else(|| snapshot.focused_window());
    window.map(|window| Outcome::Run(action(window)))
        .unwrap_or_else(|| Outcome::Unsupported("window dispatcher has no matching target".to_string()))
}

fn lua_geometry(body: &str, snapshot: &Snapshot, resize: bool) -> Outcome {
    let Some(x) = lua_field(body, "x").and_then(|value| value.parse::<i32>().ok()) else {
        return Outcome::Unsupported("window geometry requires an integer x".to_string());
    };
    let Some(y) = lua_field(body, "y").and_then(|value| value.parse::<i32>().ok()) else {
        return Outcome::Unsupported("window geometry requires an integer y".to_string());
    };
    let relative = lua_field(body, "relative").is_some_and(|value| value == "true");
    lua_window(body, snapshot, |window| {
        if resize {
            Action::ResizeWindow { window: window.id, width: x, height: y, relative }
        } else {
            Action::MoveWindow { window: window.id, x, y, relative }
        }
    })
}

fn classic_geometry(rest: &str, snapshot: &Snapshot, resize: bool, active_form: bool) -> Outcome {
    let original = rest.trim();
    let (rest, exact) = original
        .strip_prefix("exact")
        .map(|rest| (rest.trim(), true))
        .unwrap_or((original, false));
    let (numbers, selector) = rest.split_once(',').map_or((rest, ""), |(a, b)| (a.trim(), b.trim()));
    let mut fields = numbers.split_whitespace();
    let Some(x) = fields.next().and_then(|value| value.parse::<i32>().ok()) else {
        return Outcome::Unsupported("window geometry requires two integer coordinates".to_string());
    };
    let Some(y) = fields.next().and_then(|value| value.parse::<i32>().ok()) else {
        return Outcome::Unsupported("window geometry requires two integer coordinates".to_string());
    };
    let selector = if active_form { fields.collect::<Vec<_>>().join(" ") } else { selector.to_string() };
    let Some(window) = selected_window(&selector, snapshot) else {
        return Outcome::Unsupported(format!("no window matches {selector:?}"));
    };
    if resize {
        Outcome::Run(Action::ResizeWindow { window: window.id, width: x, height: y, relative: !exact })
    } else {
        Outcome::Run(Action::MoveWindow { window: window.id, x, y, relative: !exact })
    }
}

fn classic_tag(rest: &str, snapshot: &Snapshot) -> Outcome {
    let mut fields = rest.split_whitespace();
    let Some(raw_tag) = fields.next() else { return Outcome::Unsupported("tagwindow requires a tag".to_string()) };
    let selector = fields.collect::<Vec<_>>().join(" ");
    let Some(window) = selected_window(&selector, snapshot) else {
        return Outcome::Unsupported(format!("no window matches {selector:?}"));
    };
    let (present, tag) = raw_tag.strip_prefix('-').map_or((true, raw_tag.trim_start_matches('+')), |tag| (false, tag));
    Outcome::Run(Action::SetTag { window: window.id, tag: tag.to_string(), present })
}

/// Pull `key = "value"` (or `key = value`) out of a Lua table literal.
fn lua_field(body: &str, key: &str) -> Option<String> {
    for (at, _) in body.match_indices(key) {
        // Guard against `key` matching inside a longer identifier or a
        // value. In particular, looking for `x` must skip the `x` in an
        // address such as `address:0x12` and continue to the real field.
        if at > 0 {
            let before = body[..at].chars().next_back().unwrap_or(' ');
            if before.is_alphanumeric() || before == '_' || before == '.' {
                continue;
            }
        }
        let after_key = &body[at + key.len()..];
        if after_key.chars().next().is_some_and(|after| after.is_alphanumeric() || after == '_') {
            continue;
        }
        let rest = after_key.trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        return Some(match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or_default().to_string(),
            None => rest.split([',', '}', ' ']).next().unwrap_or_default().trim().to_string(),
        });
    }
    None
}

/// Pull the contents of `key = { ... }` out of a Lua table literal.
/// Only balanced braces are recognized; malformed or non-table fields
/// are left to the caller's named refusal.
fn lua_table_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    for (at, _) in body.match_indices(key) {
        if at > 0 {
            let before = body[..at].chars().next_back().unwrap_or(' ');
            if before.is_alphanumeric() || before == '_' || before == '.' {
                continue;
            }
        }
        let after_key = &body[at + key.len()..];
        if after_key.chars().next().is_some_and(|after| after.is_alphanumeric() || after == '_') {
            continue;
        }
        let Some(rest) = after_key.trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(table) = rest.strip_prefix('{') else {
            continue;
        };
        let mut depth = 1_u32;
        for (index, character) in table.char_indices() {
            match character {
                '{' => depth = depth.saturating_add(1),
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(&table[..index]);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" | "1" | "on" | "yes" => Some(true),
        "false" | "0" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// The first bare string literal in a Lua argument list.
fn lua_string(body: &str) -> Option<String> {
    // Lua long-bracket strings are ordinary string literals. Omarchy's
    // screensaver launcher uses exactly this spelling so accepting only
    // quotes turns a visible menu action into a silent refusal.
    if let Some(rest) = body.trim_start().strip_prefix("[[") {
        return rest.find("]]").map(|end| rest[..end].to_string());
    }
    // Also accept the delimiter-with-equals form (`[=[...]=]`). It
    // costs a small bounded scan and avoids making the same parser gap
    // reappear the first time a command itself contains `]]`.
    let trimmed = body.trim_start();
    if let Some(after_open) = trimmed.strip_prefix('[') {
        let equals = after_open.bytes().take_while(|byte| *byte == b'=').count();
        if after_open.as_bytes().get(equals) == Some(&b'[') {
            let content = &after_open[equals + 1..];
            let close = format!("]{}]", "=".repeat(equals));
            if let Some(end) = content.find(&close) {
                return Some(content[..end].to_string());
            }
        }
    }
    let start = body.find('"')?;
    let rest = &body[start + 1..];
    Some(rest.split('"').next()?.to_string())
}

/// Decode classic `dispatch exec` without changing its argv.
///
/// `hyprctl`'s wire format has no argument framing: it joins its argv
/// with spaces and even consumes the conventional `--` before sending.
/// Shell metacharacters and quotes therefore still mean "one shell
/// command", while a plain word sequence is safest as direct argv.
/// The important ambiguous case is a shell with `-c`/`-lc`: the shell
/// has already removed the quotes around its command argument before
/// `hyprctl` sees them, so everything after that option must be joined
/// back into the one argument the shell was asked to evaluate.
fn classic_exec(rest: &str) -> Outcome {
    let command = rest.strip_prefix("--").unwrap_or(rest).trim();
    if command.is_empty() {
        return Outcome::Unknown("exec with no command".to_string());
    }
    if command.chars().any(|ch| {
        matches!(
            ch,
            '\'' | '"' | '$' | '`' | '|' | '&' | ';' | '<' | '>' | '(' | ')' | '*' | '?' | '[' | ']' | '{' | '}'
        )
    }) {
        return Outcome::Run(Action::ExecShell(command.to_string()));
    }

    let mut argv: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        return Outcome::Unknown("exec with no command".to_string());
    }
    let shell = argv[0].rsplit('/').next().unwrap_or(&argv[0]);
    let command_option = argv.get(1).is_some_and(|option| {
        option.starts_with('-')
            && option.contains('c')
            && matches!(shell, "sh" | "bash" | "dash" | "zsh" | "ksh" | "mksh" | "busybox")
    });
    if command_option && argv.len() > 3 {
        let source = argv.drain(2..).collect::<Vec<_>>().join(" ");
        argv.push(source);
    }
    Outcome::Run(Action::ExecArgv(argv))
}

/// The workspaces a switch may name.
///
/// `wm-core`'s `switch_workspace` grows the workspace row on demand up
/// to its fixed ceiling, so mechanically any index below that ceiling
/// is reachable. The question is which ones this socket *should* reach,
/// and the answer comes from the security
/// argument in [`crate::server`]: this socket is unauthenticated
/// because it grants nothing the user's own keyboard already grants.
/// That argument only holds if the two grant the same thing — so the
/// bound here is the one chonkstep's keybindings use for their own
/// `workspace <n>` action, and it is deliberately the same number
/// rather than an independently-chosen one.
///
/// Note this is *more* permissive than chonkstep's control socket,
/// whose `focus-workspace` is documented as "a switch, never a create"
/// (`docs/control-socket.md` §4.2). The difference is intentional and
/// is the whole reason this bound is written down. Omarchy's bar draws
/// buttons for workspaces 1-5 unconditionally
/// (`plugins/bar/widgets/Workspaces.qml`), so a rule that refused a
/// switch to a workspace that does not exist yet would leave three of
/// those five buttons permanently dead while the same key on the
/// keyboard worked — which is not a compositor that Omarchy's
/// unmodified shell runs on, and running on it is the point.
///
/// Mirrors `wm_core::MAX_WORKSPACES`, the authoritative core ceiling,
/// deliberately by value rather than by dependency: this crate stays
/// free of chonkstep's own crates so that every promise it makes to
/// somebody else's binary can be tested without booting a window
/// manager (see the crate doc). `wm_config::MAX_WORKSPACE` restates the
/// same one-based limit and checks it against the core at compile time.
/// If the core constant moves, this one follows.
const MAX_WORKSPACE: usize = 99;

/// Reject a workspace index the keyboard could not reach either.
fn in_range(index: usize) -> Result<usize, String> {
    if index < MAX_WORKSPACE {
        return Ok(index);
    }
    Err(format!("chonkstep has workspaces 1 to {MAX_WORKSPACE}; {} is past the end", index + 1))
}

/// Resolve a workspace selector to a 0-based chonkstep index.
fn workspace_target(target: &str, snapshot: &Snapshot) -> Result<usize, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("workspace with no argument".to_string());
    }
    if let Ok(id) = target.parse::<i32>() {
        let index = workspace_index_from_hypr_id(id).ok_or_else(|| format!("chonkstep has no workspace {id}"))?;
        return in_range(index);
    }
    // Relative and named selectors. `e+1`/`e-1` and `+1`/`-1` are what
    // Omarchy's keybindings send for next/previous workspace.
    let relative = target.strip_prefix('e').unwrap_or(target);
    if let Some(delta) = relative.strip_prefix('+').and_then(|d| d.parse::<usize>().ok()) {
        let current = snapshot.active_workspace().map_or(0, |w| w.index);
        return in_range(current.saturating_add(delta));
    }
    if let Some(delta) = relative.strip_prefix('-').and_then(|d| d.parse::<usize>().ok()) {
        let current = snapshot.active_workspace().map_or(0, |w| w.index);
        return in_range(current.saturating_sub(delta));
    }
    if target.starts_with("special") {
        return Err("chonkstep has no special (scratchpad) workspaces".to_string());
    }
    if let Some(name) = target.strip_prefix("name:") {
        return Err(format!("chonkstep workspaces are numbered, not named ({name:?})"));
    }
    Err(format!("unrecognised workspace selector {target:?}"))
}

/// Resolve one of Hyprland's window selectors against the snapshot.
///
/// Hyprland's regex selectors (`class:^(foo)$`) are matched here as a
/// plain substring after stripping anchors, which is a *narrowing* of
/// what Hyprland accepts rather than a widening: a selector we cannot
/// interpret finds no window and the caller is told so, instead of
/// finding the wrong one.
fn resolve_window<'a>(selector: &str, snapshot: &'a Snapshot) -> Option<&'a Window> {
    let selector = selector.trim().trim_matches('"');
    if selector.is_empty() || selector == "activewindow" {
        return snapshot.focused_window();
    }
    let (kind, value) = selector.split_once(':')?;
    let value = value.trim();
    match kind {
        "address" => {
            let hex = value.trim_start_matches("0x");
            let id = u64::from_str_radix(hex, 16).ok()?;
            snapshot.windows.iter().find(|window| window.id == id)
        }
        "pid" => {
            let pid = value.parse::<i32>().ok()?;
            snapshot.windows.iter().find(|window| window.pid == pid)
        }
        "class" | "initialclass" => {
            let needle = unanchor(value);
            snapshot.windows.iter().find(|window| contains_ignore_case(&window.class, needle))
        }
        "title" | "initialtitle" => {
            let needle = unanchor(value);
            snapshot.windows.iter().find(|window| contains_ignore_case(&window.title, needle))
        }
        _ => None,
    }
}

/// Strip the `^(...)$` a Hyprland selector is usually written with.
fn unanchor(value: &str) -> &str {
    value.trim_start_matches('^').trim_end_matches('$').trim_start_matches('(').trim_end_matches(')')
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}
