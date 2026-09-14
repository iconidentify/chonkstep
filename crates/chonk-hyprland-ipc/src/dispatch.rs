//! Dispatch: turning Hyprland's verbs into chonkstep's, or refusing.
//!
//! Mosaic and Flow translate spatial actions directly; Freeform preserves
//! traditional geometry. Inapplicable tree messages deliberately succeed
//! quietly, so the same shortcut stays predictable in all three styles.
//! Unavailable grouping and system actions remain explicit errors.
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
//! whose requested behavior is unavailable.
//!
//! So: [`Outcome::Unsupported`] is a first-class result here, not a
//! shortfall, and it is reported to the caller as an error string
//! beginning with `Invalid dispatcher`. The server logs and counts each
//! one because most non-interactive callers will otherwise hide it.

use crate::state::{workspace_index_from_hypr_id, Snapshot, Window, NESTED_DEVICES};

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
    ToggleMaximize,
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
    /// `hl.monitor` with a mode or position, with or without a scale. The
    /// mode and position keep their monitor-rule spellings (`preferred`,
    /// `WxH@RATE`, `auto`, `XxY`), already checked against the snapshot,
    /// and the compositor resolves them exactly as a reload of the same
    /// line would.
    ConfigureMonitor { output: String, scale_120: Option<u32>, mode: Option<String>, position: Option<String> },
    /// Power one named output, or every output when `output` is `None`.
    SetDpms { output: Option<String>, powered: bool },
    /// Select a group from the seat keymap. Hyprland accepts next,
    /// previous, or a zero-based numeric group.
    SwitchKeyboardLayout { device: String, target: LayoutTarget },
    /// Hide or restore the compositor-owned pointer image. This is a
    /// live session property used by Omarchy's screensaver, not a
    /// persisted Hyprland configuration mutation.
    SetCursorHidden(bool),
    /// Switch one input device on or off by its exact libinput name: the
    /// request Omarchy's touchpad and touchscreen toggles make. Parsing has
    /// already refused a name that no pointer, touch or tablet device
    /// carries, and switching off a name that any keyboard carries.
    SetInputDeviceEnabled { name: String, enabled: bool },
    /// Move the pointer to a point in logical layout coordinates, the
    /// units `cursorpos` and `clients` report. The host refuses it while
    /// the session is locked or a client holds a pointer constraint.
    WarpPointer { x: i32, y: i32 },
    ReloadConfig,
    SetDiagnostic { name: String, enabled: bool },
    SetLogFilter(String),
    /// Set or toggle membership in the current workspace layout.
    SetFloating {
        window: u64,
        floating: Option<bool>,
    },
    SetWorkspaceLayout {
        workspace: usize,
        mode: String,
    },
    ToggleLayout,
    MoveDirection(Direction),
    LayoutNoop,
    /// Run a ChonkStep binding that has no Hyprland dispatcher, named by
    /// the label `binds` reported for it (`chonkstep overview`).
    Binding(String),
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

/// Known operations outside ChonkStep's model, with their refusal reason.
///
/// Listing them explicitly — rather than letting them fall through to
/// "unknown dispatcher" — is the difference between "chonkstep does not
/// recognise this word" and "chonkstep understands exactly what you
/// asked for and is not able to do it". The second is a much better
/// error to read at 2am, and it is the one that tells a script author
/// their fallback path is the right one to write.
const UNSUPPORTED: &[(&str, &str)] = &[
    ("togglegroup", "chonkstep has no window groups"),
    ("changegroupactive", "chonkstep has no window groups"),
    ("moveintogroup", "chonkstep has no window groups"),
    ("moveoutofgroup", "chonkstep has no window groups"),
    ("lockgroups", "chonkstep has no window groups"),
    ("togglespecialworkspace", "chonkstep has no special (scratchpad) workspaces"),
    ("workspaceopt", "chonkstep has no per-workspace layout options"),
    ("submap", "chonkstep's keybindings do not have submaps"),
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

/// Split the verb from its arguments on the first whitespace character,
/// whatever its width; see `Request::parse`, which splits the same way.
fn split_verb(args: &str) -> (String, &str) {
    match args.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb.to_ascii_lowercase(), rest.trim()),
        None => (args.to_ascii_lowercase(), ""),
    }
}

fn parse_classic(verb: &str, rest: &str, snapshot: &Snapshot) -> Outcome {
    if let Some((_, why)) = UNSUPPORTED.iter().find(|(name, _)| *name == verb) {
        return Outcome::Unsupported((*why).to_string());
    }

    match verb {
        "layoutmsg" | "togglesplit" | "swapsplit" | "pseudo" | "splitratio" => {
            Outcome::Run(Action::LayoutNoop)
        }
        "togglelayout" => Outcome::Run(Action::ToggleLayout),
        "layout" => layout_action(snapshot.active_workspace().map_or(0, |w| w.index), rest),
        "swapnext" => Outcome::Run(Action::MoveDirection(if rest.contains("prev") {
            Direction::Left
        } else {
            Direction::Right
        })),
        "movewindow" | "swapwindow" => match rest.trim() {
            "l" | "left" => Outcome::Run(Action::MoveDirection(Direction::Left)),
            "r" | "right" => Outcome::Run(Action::MoveDirection(Direction::Right)),
            "u" | "up" => Outcome::Run(Action::MoveDirection(Direction::Up)),
            "d" | "down" => Outcome::Run(Action::MoveDirection(Direction::Down)),
            _ => Outcome::Unsupported("window movement requires a direction".into()),
        },
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
        "fullscreen" => {
            if rest.trim() == "1" {
                Outcome::Run(Action::ToggleMaximize)
            } else if rest.trim() == "2" {
                Outcome::Run(Action::Fullscreen(Fullscreen::On))
            } else {
                Outcome::Run(Action::Fullscreen(Fullscreen::Toggle))
            }
        }
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
        "togglefloating" | "setfloating" | "settiled" => selected_window(rest, snapshot)
            .map(|window| {
                Outcome::Run(Action::SetFloating {
                    window: window.id,
                    floating: match verb {
                        "setfloating" => Some(true),
                        "settiled" => Some(false),
                        _ => None,
                    },
                })
            })
            .unwrap_or_else(|| Outcome::Unsupported(format!("no window matches {rest:?}"))),
        "tagwindow" => classic_tag(rest, snapshot),
        "movecursor" => {
            let mut fields = rest.split_whitespace().map(str::parse::<i32>);
            match (fields.next(), fields.next(), fields.next()) {
                (Some(Ok(x)), Some(Ok(y)), None) => Outcome::Run(Action::WarpPointer { x, y }),
                _ => Outcome::Unsupported("movecursor requires integer x and y".to_string()),
            }
        }
        // ChonkStep's own verbs, as `binds` reports a binding with no
        // Hyprland dispatcher; Omarchy's keybindings menu hands that pair
        // straight back. Only a label this snapshot reports replays, so a
        // caller cannot name an arbitrary action, and while the session is
        // locked only a binding marked locked does.
        "chonkstep" => {
            let label = rest.trim();
            match snapshot
                .bindings
                .iter()
                .find(|binding| binding.dispatcher == "chonkstep" && binding.argument == label)
            {
                Some(binding) if snapshot.locked && !binding.locked => Outcome::Unsupported(format!(
                    "chonkstep {label} is not a locked binding and the session is locked"
                )),
                Some(_) => Outcome::Run(Action::Binding(label.to_string())),
                None => Outcome::Unsupported(format!("chonkstep has no bound action named {label:?}")),
            }
        }
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
///
/// The arguments, though, are read as real Lua literals: a string is
/// decoded with Lua's own escape rules, and a field is a key of a table
/// rather than a word found somewhere in the text. `exec_cmd`'s string
/// is a command to run, so reading it as "whatever lies between the
/// first two quotes" ran a different command and answered `ok`.
fn parse_lua(rest: &str, snapshot: &Snapshot) -> Outcome {
    let (path, body) = match rest.split_once('(') {
        Some((path, body)) => (path.trim(), body.trim_end().trim_end_matches(')')),
        None => (rest.trim(), ""),
    };
    let args = match lua_arguments(body) {
        Ok(args) => args,
        Err(error) => return Outcome::Unsupported(format!("hl.dsp.{path}: invalid Lua arguments: {error}")),
    };

    match path {
        "focus" => {
            if let Some(value) = lua_field(&args, "workspace") {
                return match workspace_target(&value, snapshot) {
                    Ok(index) => Outcome::Run(Action::FocusWorkspace(index)),
                    Err(why) => Outcome::Unsupported(why),
                };
            }
            if let Some(value) = lua_field(&args, "window") {
                return match resolve_window(&value, snapshot) {
                    Some(window) => Outcome::Run(Action::FocusWindow(window.id)),
                    None => Outcome::Unsupported(format!("no window matches {value:?}")),
                };
            }
            Outcome::Unknown("hl.dsp.focus with no workspace or window".to_string())
        }
        "window.close" => match lua_field(&args, "window") {
            Some(value) => match resolve_window(&value, snapshot) {
                Some(window) => Outcome::Run(Action::CloseWindow(window.id)),
                None => Outcome::Unsupported(format!("no window matches {value:?}")),
            },
            None => Outcome::Run(Action::KillActive),
        },
        "exec_cmd" => match lua_field(&args, "cmd").or_else(|| lua_string(&args)) {
            Some(command) => Outcome::Run(Action::ExecShell(command)),
            None => Outcome::Unknown("hl.dsp.exec_cmd with no command".to_string()),
        },
        "window.float" => lua_window(&args, snapshot, |window| Action::SetFloating {
            window: window.id,
            floating: match lua_field(&args, "action").as_deref() {
                Some("on" | "set") => Some(true),
                Some("off" | "unset") => Some(false),
                _ => None,
            },
        }),
        "layout" => Outcome::Run(Action::LayoutNoop),
        "window.pin" => lua_window(&args, snapshot, |window| Action::SetPinned { window: window.id, pinned: None }),
        "window.resize" => lua_geometry(&args, snapshot, true),
        "window.move" => lua_geometry(&args, snapshot, false),
        "window.center" => lua_window(&args, snapshot, |window| Action::CenterWindow(window.id)),
        "window.alter_zorder" => {
            if lua_field(&args, "mode").as_deref() != Some("top") {
                Outcome::Unsupported("hl.dsp.window.alter_zorder supports mode=top only".to_string())
            } else {
                lua_window(&args, snapshot, |window| Action::RaiseWindow(window.id))
            }
        }
        "window.tag" => {
            let Some(tag) = lua_field(&args, "tag") else {
                return Outcome::Unknown("hl.dsp.window.tag with no tag".to_string());
            };
            let (present, tag) = match tag.strip_prefix('-') {
                Some(tag) => (false, tag.to_string()),
                None => (true, tag.trim_start_matches('+').to_string()),
            };
            lua_window(&args, snapshot, |window| Action::SetTag { window: window.id, tag, present })
        }
        "window.fullscreen_state" => {
            let client = lua_field(&args, "client").and_then(|value| value.parse::<i32>().ok()).unwrap_or(0);
            Outcome::Run(Action::Fullscreen(if client == 0 { Fullscreen::Off } else { Fullscreen::On }))
        }
        "window.set_prop" => Outcome::Unsupported("window opacity and other dynamic properties are not modeled".to_string()),
        "cursor.move" => {
            let coordinate = |key: &str| lua_field(&args, key).and_then(|value| value.trim().parse::<i32>().ok());
            match (coordinate("x"), coordinate("y")) {
                (Some(x), Some(y)) => Outcome::Run(Action::WarpPointer { x, y }),
                _ => Outcome::Unsupported("hl.dsp.cursor.move requires integer x and y".to_string()),
            }
        }
        "dpms" => parse_dpms_lua(&args, snapshot),
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
        let args = match lua_arguments(body) {
            Ok(args) => args,
            Err(error) => return Outcome::Unsupported(format!("hl.monitor: invalid Lua arguments: {error}")),
        };
        // Every key is accounted for before anything changes. A request that
        // also asked to disable or mirror the output must not come back `ok`
        // having applied only its scale.
        for arg in &args {
            let Literal::Table(fields) = arg else {
                return Outcome::Unsupported("hl.monitor takes one table of named keys".to_string());
            };
            for (key, _) in fields {
                match key.as_deref() {
                    Some("output" | "mode" | "position" | "scale") => {}
                    Some(other) => {
                        return Outcome::Unsupported(format!(
                            "hl.monitor key {other:?} is not supported; output, mode, position and scale are"
                        ))
                    }
                    None => return Outcome::Unsupported("hl.monitor takes named keys only".to_string()),
                }
            }
        }
        let Some(output) = lua_field(&args, "output") else {
            return Outcome::Unsupported("hl.monitor requires a named output".to_string());
        };
        let Some(monitor) = snapshot.monitors.iter().find(|monitor| monitor.name == output) else {
            return Outcome::Unsupported(format!("hl.monitor names unknown output {output:?}"));
        };
        let scale_120 = match lua_field(&args, "scale") {
            None => None,
            Some(value) => match value.parse::<f64>() {
                Ok(scale) if scale.is_finite() && (0.5..=4.0).contains(&scale) => Some((scale * 120.0).round() as u32),
                _ => return Outcome::Unsupported("hl.monitor scale must be a number between 0.5 and 4".to_string()),
            },
        };
        let mode = match lua_field(&args, "mode") {
            None => None,
            Some(mode) if monitor_advertises(monitor, &mode) => Some(mode),
            Some(mode) => {
                return Outcome::Unsupported(format!("hl.monitor mode {mode:?} is not one {output} advertises"))
            }
        };
        let position = match lua_field(&args, "position") {
            None => None,
            Some(position) if position.trim().eq_ignore_ascii_case("auto") || monitor_position(&position).is_some() => {
                Some(position)
            }
            Some(position) => {
                return Outcome::Unsupported(format!("hl.monitor position {position:?} must be auto or XxY"))
            }
        };
        return Outcome::Run(match (scale_120, mode, position) {
            (None, None, None) => {
                return Outcome::Unsupported("hl.monitor needs a mode, position or scale".to_string())
            }
            (Some(scale_120), None, None) => Action::SetMonitorScale { output, scale_120 },
            (scale_120, mode, position) => Action::ConfigureMonitor { output, scale_120, mode, position },
        });
    }
    if let Some(body) = source.strip_prefix("hl.config(").and_then(|value| value.strip_suffix(')')) {
        let args = match lua_arguments(body) {
            Ok(args) => args,
            Err(error) => return Outcome::Unsupported(format!("hl.config: invalid Lua arguments: {error}")),
        };
        if let Some(cursor @ Literal::Table(_)) = lua_value(&args, "cursor") {
            if let Some(value) = lua_field(std::slice::from_ref(cursor), "invisible") {
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
    if let Some(call) = source.strip_prefix("hl.device(") {
        return parse_device(call.strip_suffix(')'), snapshot);
    }
    if let Some(call) = source.strip_prefix("hl.workspace_rule(") {
        let args = match lua_arguments(call.strip_suffix(')').unwrap_or(call)) {
            Ok(args) => args,
            Err(error) => return Outcome::Unsupported(format!("hl.workspace_rule: invalid Lua arguments: {error}")),
        };
        let workspace = lua_field(&args, "workspace")
            .and_then(|s| s.parse::<i32>().ok())
            .and_then(workspace_index_from_hypr_id);
        return match (workspace, lua_field(&args, "layout")) {
            (Some(workspace), Some(mode)) if workspace < 99 => layout_action(workspace, &mode),
            _ => Outcome::Unsupported(
                "workspace_rule requires a workspace from 1 to 99 and a layout".into(),
            ),
        };
    }
    Outcome::Unknown(format!("unknown eval expression {source:?}"))
}

/// The longest device name `hl.device` accepts: the bound a configuration's
/// device rules have, since both name the same devices.
const MAX_DEVICE_NAME: usize = 256;

/// `hl.device({ name = "…", enabled = BOOL })`. Only `enabled` changes at
/// runtime, and every other device setting belongs in the configuration.
/// The name is a Lua string with its escapes decoded, and has to be exactly
/// a device the snapshot lists: a name cut short at an escaped quote would
/// be a plausible, different device.
fn parse_device(body: Option<&str>, snapshot: &Snapshot) -> Outcome {
    let args = match body.map(lua_arguments) {
        Some(Ok(args)) => args,
        Some(Err(error)) => return Outcome::Unsupported(format!("hl.device: invalid Lua arguments: {error}")),
        None => return Outcome::Unsupported("hl.device: invalid Lua arguments: unterminated call".to_string()),
    };
    let Some(Literal::Str(name)) = lua_value(&args, "name") else {
        return Outcome::Unsupported("hl.device requires the device's name as a quoted string".to_string());
    };
    if name.is_empty() || name.len() > MAX_DEVICE_NAME {
        return Outcome::Unsupported(format!("hl.device names a device in 1 to {MAX_DEVICE_NAME} bytes"));
    }
    let Some(enabled) = lua_field(&args, "enabled").as_deref().and_then(parse_bool) else {
        return Outcome::Unsupported("hl.device requires enabled = true or false".to_string());
    };
    let keys = args.iter().flat_map(|arg| match arg {
        Literal::Table(fields) => fields.as_slice(),
        _ => &[],
    });
    if let Some(key) = keys.filter_map(|(key, _)| key.as_deref()).find(|key| !matches!(*key, "name" | "enabled")) {
        return Outcome::Unsupported(format!("hl.device changes only enabled at runtime; {key} belongs in the configuration"));
    }
    if NESTED_DEVICES.contains(&name.as_str()) {
        return Outcome::Unsupported(format!(
            "{name:?} is the nested session's logical device, not a libinput device, and cannot be switched"
        ));
    }
    let devices = &snapshot.devices;
    if !enabled && devices.keyboards.iter().any(|keyboard| keyboard.name == *name) {
        return Outcome::Unsupported(format!(
            "hl.device will not disable {name:?}: a device with keys stays on, so the lock screen can always be typed into"
        ));
    }
    let known = devices.mice.iter().chain(&devices.touch).chain(&devices.tablets).any(|device| device.name == *name);
    if !known {
        return Outcome::Unsupported(format!("hl.device names no pointer, touch or tablet device {name:?}"));
    }
    Outcome::Run(Action::SetInputDeviceEnabled { name: name.clone(), enabled })
}

fn layout_action(workspace: usize, mode: &str) -> Outcome {
    if matches!(
        mode.trim(),
        "freeform" | "mosaic" | "flow" | "dwindle" | "scrolling"
    ) {
        Outcome::Run(Action::SetWorkspaceLayout {
            workspace,
            mode: mode.trim().into(),
        })
    } else {
        Outcome::Unsupported(
            "layout must be Freeform, Mosaic/dwindle or Flow/scrolling (lowercase)".into(),
        )
    }
}

/// Parse the supported workspace, monitor and diagnostic keyword mutations.
pub fn parse_keyword(source: &str) -> Outcome {
    if let Some(spec) = source.trim().strip_prefix("workspace ") {
        if let Some((workspace, mode)) = spec.split_once(',') {
            if let Some(index) = workspace
                .trim()
                .parse::<i32>()
                .ok()
                .and_then(workspace_index_from_hypr_id)
                .filter(|&i| i < 99)
            {
                if let Some(mode) = mode.trim().strip_prefix("layout:") {
                    return layout_action(index, mode);
                }
            }
        }
        return Outcome::Unsupported("workspace requires N, layout:MODE".into());
    }
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

fn parse_dpms_lua(args: &[Literal], snapshot: &Snapshot) -> Outcome {
    let state = lua_field(args, "state")
        .or_else(|| lua_field(args, "enabled"))
        .or_else(|| lua_field(args, "action"))
        .or_else(|| lua_string(args));
    let Some(state) = state else {
        return Outcome::Unsupported("hl.dsp.dpms requires state=on or state=off".to_string());
    };
    let state = match state.to_ascii_lowercase().as_str() {
        "enable" | "enabled" => "on",
        "disable" | "disabled" => "off",
        _ => state.as_str(),
    };
    let output = lua_field(args, "output").or_else(|| lua_field(args, "monitor"));
    parse_dpms(&format!("{}{}", state, output.map_or_else(String::new, |name| format!(" {name}"))), snapshot)
}

fn selected_window<'a>(selector: &str, snapshot: &'a Snapshot) -> Option<&'a Window> {
    if selector.trim().is_empty() {
        snapshot.focused_window()
    } else {
        resolve_window(selector.trim(), snapshot)
    }
}

fn lua_window<F>(args: &[Literal], snapshot: &Snapshot, action: F) -> Outcome
where
    F: FnOnce(&Window) -> Action,
{
    let window = lua_field(args, "window")
        .as_deref()
        .and_then(|selector| resolve_window(selector, snapshot))
        .or_else(|| snapshot.focused_window());
    window.map(|window| Outcome::Run(action(window)))
        .unwrap_or_else(|| Outcome::Unsupported("window dispatcher has no matching target".to_string()))
}

fn lua_geometry(args: &[Literal], snapshot: &Snapshot, resize: bool) -> Outcome {
    let Some(x) = lua_field(args, "x").and_then(|value| value.parse::<i32>().ok()) else {
        return Outcome::Unsupported("window geometry requires an integer x".to_string());
    };
    let Some(y) = lua_field(args, "y").and_then(|value| value.parse::<i32>().ok()) else {
        return Outcome::Unsupported("window geometry requires an integer y".to_string());
    };
    let relative = lua_field(args, "relative").is_some_and(|value| value == "true");
    lua_window(args, snapshot, |window| {
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

/// The value of `key = ...` in the first table argument that has the key.
/// Only a table's own keys count: a key name that appears inside a
/// string value, or in a nested table, is not a field of the call.
fn lua_value<'a>(args: &'a [Literal], key: &str) -> Option<&'a Literal> {
    args.iter().find_map(|arg| match arg {
        Literal::Table(fields) => {
            fields.iter().find(|(name, _)| name.as_deref() == Some(key)).map(|(_, value)| value)
        }
        _ => None,
    })
}

/// The text of `key = "value"` (or `key = value`) in an argument list.
/// A table value has no text and is left to the caller's named refusal.
fn lua_field(args: &[Literal], key: &str) -> Option<String> {
    match lua_value(args, key)? {
        Literal::Str(text) | Literal::Word(text) => Some(text.clone()),
        Literal::Table(_) => None,
    }
}

/// Whether `request` names a mode `monitor` can drive, in the spellings a
/// monitor rule accepts. The named choices are resolved by the compositor
/// against the same list; `WxH` and `WxH@RATE` must match an advertised
/// size, with the rate within the 1 Hz a printed `refreshRate` can drift
/// from the timing it came from.
///
/// A snapshot with no mode list for the output (the nested backend has no
/// connector to enumerate) cannot refuse anything here. The spelling is
/// still checked, and the compositor resolves the mode against the live
/// output before it answers, refusing there what it cannot drive.
fn monitor_advertises(monitor: &crate::state::Monitor, request: &str) -> bool {
    let request = request.trim().to_ascii_lowercase();
    if matches!(request.as_str(), "preferred" | "highrr" | "highres") {
        return true;
    }
    let (size, rate) = request.split_once('@').map_or((request.as_str(), None), |(size, rate)| (size, Some(rate)));
    let Some((Ok(width), Ok(height))) = size.split_once('x').map(|(w, h)| (w.parse::<i64>(), h.parse::<i64>())) else {
        return false;
    };
    let millihertz = match rate.map(str::parse::<f64>) {
        None => None,
        Some(Ok(hz)) if hz.is_finite() && hz > 0.0 && hz <= 10_000.0 => Some((hz * 1000.0).round() as i64),
        Some(_) => return false,
    };
    if monitor.modes.is_empty() {
        return true;
    }
    monitor.modes.iter().any(|mode| {
        mode.width as i64 == width
            && mode.height as i64 == height
            && millihertz.is_none_or(|millihertz| (mode.refresh_millihertz as i64 - millihertz).abs() <= 1000)
    })
}

/// A monitor-rule position, `XxY`.
fn monitor_position(value: &str) -> Option<(i32, i32)> {
    let (x, y) = value.trim().split_once(['x', 'X'])?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// The first argument that is a bare string literal.
fn lua_string(args: &[Literal]) -> Option<String> {
    args.iter().find_map(|arg| match arg {
        Literal::Str(text) => Some(text.clone()),
        _ => None,
    })
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" | "1" | "on" | "yes" => Some(true),
        "false" | "0" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// The deepest table nesting an argument list may use. Omarchy's
/// deepest is two (`hl.config({ cursor = { ... } })`); the bound exists
/// because tables are read recursively and the payload is untrusted.
/// Length needs no bound of its own: strings are read iteratively, and
/// the whole request is already capped at [`crate::request::MAX_REQUEST`].
const MAX_LUA_DEPTH: usize = 16;

/// One value in a Lua argument list, as far as dispatch reads Lua.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Literal {
    /// A string literal in any of Lua's three spellings, decoded.
    Str(String),
    /// A number, `true`, `false`, `nil` or a name, kept as its source
    /// text. Every caller reads these as text (an integer, a scale, a
    /// boolean), so deciding here what a number is would be a second
    /// place to get it wrong.
    Word(String),
    /// A table constructor: each field with its key, or `None` for a
    /// positional one.
    Table(Vec<(Option<String>, Literal)>),
}

/// Why an argument list is not made of the Lua literals dispatch reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LuaError {
    /// The input ends inside a string or table.
    Unterminated(&'static str),
    /// A backslash escape Lua does not have, or a malformed one.
    InvalidEscape(char),
    /// Byte escapes that together are not UTF-8. Lua would accept the
    /// bytes, but they cannot be a command line or a selector.
    InvalidUtf8,
    TooDeep,
    /// The input ends where a value should be.
    MissingValue,
    /// Anything that is not a literal: an operator, a call, a stray
    /// character.
    Unexpected(char),
}

impl std::fmt::Display for LuaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LuaError::Unterminated(what) => write!(f, "unterminated {what}"),
            LuaError::InvalidEscape(escape) => write!(f, "invalid escape \\{}", escape.escape_debug()),
            LuaError::InvalidUtf8 => f.write_str("string escapes decode to bytes that are not UTF-8"),
            LuaError::TooDeep => write!(f, "tables nested deeper than {MAX_LUA_DEPTH}"),
            LuaError::MissingValue => f.write_str("a value is missing"),
            LuaError::Unexpected(character) => write!(f, "unexpected {character:?}"),
        }
    }
}

/// Read a whole argument list: comma-separated literals and nothing else.
fn lua_arguments(body: &str) -> Result<Vec<Literal>, LuaError> {
    let mut reader = LuaReader { text: body, at: 0 };
    let mut args = Vec::new();
    reader.skip_space();
    if reader.peek().is_none() {
        return Ok(args);
    }
    loop {
        args.push(reader.value(0)?);
        reader.skip_space();
        match reader.peek() {
            None => return Ok(args),
            Some(b',') => {
                reader.at += 1;
                reader.skip_space();
            }
            Some(_) => return Err(reader.unexpected()),
        }
    }
}

/// A cursor over an argument list. `at` only ever advances over ASCII
/// bytes or over a whole literal, so it always sits on a character
/// boundary; the slices below use `get` regardless, because the input
/// is a client's.
struct LuaReader<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> LuaReader<'a> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn peek_after(&self, offset: usize) -> Option<u8> {
        self.text.as_bytes().get(self.at + offset).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(is_lua_space) {
            self.at += 1;
        }
    }

    fn unexpected(&self) -> LuaError {
        self.text.get(self.at..).and_then(|rest| rest.chars().next()).map_or(LuaError::MissingValue, LuaError::Unexpected)
    }

    /// Step over `byte`, after any whitespace, or say what is there instead.
    fn expect(&mut self, byte: u8, within: &'static str) -> Result<(), LuaError> {
        self.skip_space();
        match self.peek() {
            Some(found) if found == byte => {
                self.at += 1;
                self.skip_space();
                Ok(())
            }
            None => Err(LuaError::Unterminated(within)),
            Some(_) => Err(self.unexpected()),
        }
    }

    fn value(&mut self, depth: usize) -> Result<Literal, LuaError> {
        match self.peek() {
            None => Err(LuaError::MissingValue),
            Some(b'"' | b'\'') => self.string(),
            Some(b'[') if matches!(self.peek_after(1), Some(b'[' | b'=')) => self.string(),
            Some(b'{') => self.table(depth + 1),
            Some(byte) if is_word_byte(byte) => Ok(Literal::Word(self.word().to_string())),
            Some(_) => Err(self.unexpected()),
        }
    }

    fn string(&mut self) -> Result<Literal, LuaError> {
        let (text, length) = string_literal(self.text.get(self.at..).unwrap_or_default())?;
        self.at += length;
        Ok(Literal::Str(text))
    }

    fn word(&mut self) -> &'a str {
        let start = self.at;
        while self.peek().is_some_and(is_word_byte) {
            self.at += 1;
        }
        self.text.get(start..self.at).unwrap_or_default()
    }

    fn table(&mut self, depth: usize) -> Result<Literal, LuaError> {
        if depth > MAX_LUA_DEPTH {
            return Err(LuaError::TooDeep);
        }
        self.at += 1;
        let mut fields = Vec::new();
        loop {
            self.skip_space();
            match self.peek() {
                None => return Err(LuaError::Unterminated("table")),
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Literal::Table(fields));
                }
                Some(_) => {}
            }
            fields.push(self.field(depth)?);
            self.skip_space();
            match self.peek() {
                None => return Err(LuaError::Unterminated("table")),
                Some(b',' | b';') => self.at += 1,
                Some(b'}') => {}
                Some(_) => return Err(self.unexpected()),
            }
        }
    }

    /// One table field: `name = value`, `[key] = value`, or a positional
    /// value.
    fn field(&mut self, depth: usize) -> Result<(Option<String>, Literal), LuaError> {
        if self.peek() == Some(b'[') && !matches!(self.peek_after(1), Some(b'[' | b'=')) {
            self.at += 1;
            self.skip_space();
            let key = match self.value(depth)? {
                Literal::Str(key) | Literal::Word(key) => key,
                Literal::Table(_) => return Err(LuaError::Unexpected('}')),
            };
            self.expect(b']', "table")?;
            self.expect(b'=', "table")?;
            return Ok((Some(key), self.value(depth)?));
        }
        if self.peek().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_') {
            let start = self.at;
            let name = self.word();
            self.skip_space();
            if self.peek() == Some(b'=') && self.peek_after(1) != Some(b'=') {
                // `a.b = 1` is an assignment, not a table field.
                if !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
                    return Err(LuaError::Unexpected('='));
                }
                self.at += 1;
                self.skip_space();
                return Ok((Some(name.to_string()), self.value(depth)?));
            }
            self.at = start;
        }
        Ok((None, self.value(depth)?))
    }
}

fn is_lua_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The bytes of a number, a boolean, `nil` or a name, including the sign
/// and exponent characters of a number such as `-1.5e+3`.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-')
}

/// Decode one Lua string literal at the start of `src`: `"..."`, `'...'`
/// or a long bracket, with Lua 5.4's escapes. Returns the text and the
/// number of bytes the literal spans.
///
/// Lua strings are bytes, and `\xNN` and `\ddd` each name one byte, so
/// the decoded bytes are collected first and checked as UTF-8 once at the
/// end. Decoding escape by escape into `char`s would garble a multibyte
/// character spelled as separate byte escapes.
fn string_literal(src: &str) -> Result<(String, usize), LuaError> {
    let bytes = src.as_bytes();
    let quote = match bytes.first() {
        Some(b'[') => return long_bracket(src),
        Some(&quote @ (b'"' | b'\'')) => quote,
        _ => return Err(LuaError::MissingValue),
    };
    let mut out = Vec::new();
    let mut at = 1;
    loop {
        let Some(&byte) = bytes.get(at) else {
            return Err(LuaError::Unterminated("string"));
        };
        at += 1;
        match byte {
            _ if byte == quote => break,
            // Lua refuses an unescaped line break in a short string: it
            // does not end the string, so the string never ended.
            b'\n' | b'\r' => return Err(LuaError::Unterminated("string")),
            b'\\' => {
                let Some(&escape) = bytes.get(at) else {
                    return Err(LuaError::Unterminated("string"));
                };
                at += 1;
                match escape {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'v' => out.push(0x0b),
                    b'\\' | b'"' | b'\'' => out.push(escape),
                    // A backslash before a line break is one newline;
                    // `\r\n` and `\n\r` count as one break.
                    b'\n' | b'\r' => {
                        if bytes.get(at).is_some_and(|&next| matches!(next, b'\n' | b'\r') && next != escape) {
                            at += 1;
                        }
                        out.push(b'\n');
                    }
                    // `\z` skips the whitespace after it, line breaks included.
                    b'z' => {
                        while bytes.get(at).copied().is_some_and(is_lua_space) {
                            at += 1;
                        }
                    }
                    // Exactly two hex digits.
                    b'x' => {
                        let digit = |offset: usize| bytes.get(at + offset).copied().and_then(hex_digit);
                        let (Some(high), Some(low)) = (digit(0), digit(1)) else {
                            return Err(LuaError::InvalidEscape('x'));
                        };
                        out.push((high << 4) | low);
                        at += 2;
                    }
                    // Up to three decimal digits, naming a byte.
                    b'0'..=b'9' => {
                        let mut value = u32::from(escape - b'0');
                        for _ in 0..2 {
                            let Some(&digit @ b'0'..=b'9') = bytes.get(at) else { break };
                            value = value * 10 + u32::from(digit - b'0');
                            at += 1;
                        }
                        let byte = u8::try_from(value).map_err(|_| LuaError::InvalidEscape(char::from(escape)))?;
                        out.push(byte);
                    }
                    b'u' => {
                        let character = unicode_escape(bytes, &mut at)?;
                        out.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
                    }
                    _ => {
                        let escape = src.get(at - 1..).and_then(|rest| rest.chars().next()).unwrap_or('\\');
                        return Err(LuaError::InvalidEscape(escape));
                    }
                }
            }
            // Every delimiter is ASCII, so the bytes of a multibyte
            // character are copied through whole.
            _ => out.push(byte),
        }
    }
    let text = String::from_utf8(out).map_err(|_| LuaError::InvalidUtf8)?;
    Ok((text, at))
}

fn hex_digit(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).and_then(|digit| u8::try_from(digit).ok())
}

/// The `{XXX}` after `\u`. Lua also accepts values past Unicode, up to
/// 2^31, and encodes them as extended UTF-8; those are not text, so any
/// value that is not a Unicode scalar is refused.
fn unicode_escape(bytes: &[u8], at: &mut usize) -> Result<char, LuaError> {
    let invalid = LuaError::InvalidEscape('u');
    if bytes.get(*at) != Some(&b'{') {
        return Err(invalid);
    }
    *at += 1;
    let mut value = 0_u32;
    let mut digits = 0;
    while let Some(digit) = bytes.get(*at).copied().and_then(hex_digit) {
        value = value.checked_mul(16).and_then(|value| value.checked_add(u32::from(digit))).ok_or(invalid)?;
        digits += 1;
        *at += 1;
    }
    if digits == 0 || bytes.get(*at) != Some(&b'}') {
        return Err(invalid);
    }
    *at += 1;
    char::from_u32(value).ok_or(invalid)
}

/// A long bracket string, `[[...]]` or `[==[...]==]`, closed only by a
/// bracket of the same level. Omarchy's screensaver launcher sends this
/// spelling, and the level lets a command contain `]]`. Nothing is
/// escaped inside one; as in Lua, a line break straight after the
/// opening bracket is skipped and every line break becomes `\n`.
fn long_bracket(src: &str) -> Result<(String, usize), LuaError> {
    let bytes = src.as_bytes();
    let level = bytes.iter().skip(1).take_while(|&&byte| byte == b'=').count();
    if bytes.get(level + 1) != Some(&b'[') {
        return Err(LuaError::Unexpected('['));
    }
    let open = level + 2;
    let mut at = open;
    let mut out = Vec::new();
    loop {
        let Some(&byte) = bytes.get(at) else {
            return Err(LuaError::Unterminated("long string"));
        };
        match byte {
            b']' if bytes.get(at + 1..at + 1 + level).is_some_and(|run| run.iter().all(|&byte| byte == b'='))
                && bytes.get(at + 1 + level) == Some(&b']') =>
            {
                at += level + 2;
                break;
            }
            b'\n' | b'\r' => {
                let leading = at == open;
                at += 1;
                if bytes.get(at).is_some_and(|&next| matches!(next, b'\n' | b'\r') && next != byte) {
                    at += 1;
                }
                if !leading {
                    out.push(b'\n');
                }
            }
            _ => {
                out.push(byte);
                at += 1;
            }
        }
    }
    let text = String::from_utf8(out).map_err(|_| LuaError::InvalidUtf8)?;
    Ok((text, at))
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
    // Relative selectors first. `e+1`/`e-1` and `+1`/`-1` are what
    // Omarchy's keybindings send for next/previous workspace, and read as
    // integers `+1` is workspace 1 and `-1` a special workspace id.
    let relative = target.strip_prefix('e').unwrap_or(target);
    if let Some(delta) = relative.strip_prefix('+').and_then(|d| d.parse::<usize>().ok()) {
        let current = snapshot.active_workspace().map_or(0, |w| w.index);
        return in_range(current.saturating_add(delta));
    }
    if let Some(delta) = relative.strip_prefix('-').and_then(|d| d.parse::<usize>().ok()) {
        let current = snapshot.active_workspace().map_or(0, |w| w.index);
        return in_range(current.saturating_sub(delta));
    }
    if let Ok(id) = target.parse::<i32>() {
        let index = workspace_index_from_hypr_id(id).ok_or_else(|| format!("chonkstep has no workspace {id}"))?;
        return in_range(index);
    }
    // Named selectors.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The verb split has the same shape as the request split, so it
    /// had the same panic: a multibyte space after the verb put the
    /// argument slice inside the character.
    #[test]
    fn non_ascii_whitespace_after_the_verb_splits_without_panicking() {
        assert_eq!(split_verb("exec\u{a0}foot"), ("exec".to_string(), "foot"));
        assert_eq!(
            parse("exec\u{a0}foot", &Snapshot::default()),
            Outcome::Run(Action::ExecArgv(vec!["foot".to_string()]))
        );
    }

    fn decode(literal: &str) -> Result<String, LuaError> {
        string_literal(literal).map(|(text, _)| text)
    }

    #[test]
    fn lua_short_strings_decode_every_escape_form() {
        assert_eq!(decode(r#""\n\t\r\a\b\f\v""#), Ok("\n\t\r\u{7}\u{8}\u{c}\u{b}".into()));
        assert_eq!(decode(r#""\\ \" \'""#), Ok(r#"\ " '"#.into()));
        assert_eq!(decode(r#"'it\'s "quoted"'"#), Ok(r#"it's "quoted""#.into()));
        // A backslash before a line break is one newline, whichever
        // break it is; two breaks of the same kind are two lines.
        for newline in ["\n", "\r", "\r\n", "\n\r"] {
            assert_eq!(decode(&format!("\"a\\{newline}b\"")), Ok("a\nb".into()), "{newline:?}");
        }
        assert_eq!(decode("\"a\\\n\nb\""), Err(LuaError::Unterminated("string")));
        assert_eq!(decode("\"one \\z\n\t\u{b}  two\""), Ok("one two".into()));
        assert_eq!(decode(r#""\x41\x6a""#), Ok("Aj".into()));
        // At most three digits: `\0067` is byte 6 and then a `7`.
        assert_eq!(decode(r#""\65\066\0067""#), Ok("AB\u{6}7".into()));
        assert_eq!(decode(r#""\u{48}\u{1F600}\u{000041}""#), Ok("H\u{1F600}A".into()));
        // Byte escapes form one character between them.
        assert_eq!(decode(r#""\228\189\160\xe4\xbd\xa0""#), Ok("你你".into()));
        assert_eq!(decode("\"日本\""), Ok("日本".into()));
        // The length ends at the closing quote, not at the end of input.
        assert_eq!(string_literal(r#""a\"b" , rest"#), Ok((r#"a"b"#.into(), 6)));
    }

    #[test]
    fn lua_strings_lua_itself_would_reject_are_errors() {
        assert_eq!(decode(r#""\q""#), Err(LuaError::InvalidEscape('q')));
        assert_eq!(decode(r#""\x4""#), Err(LuaError::InvalidEscape('x')));
        assert_eq!(decode(r#""\256""#), Err(LuaError::InvalidEscape('2')));
        assert_eq!(decode(r#""\u{}""#), Err(LuaError::InvalidEscape('u')));
        assert_eq!(decode(r#""\u48""#), Err(LuaError::InvalidEscape('u')));
        assert_eq!(decode(r#""\u{D800}""#), Err(LuaError::InvalidEscape('u')));
        assert_eq!(decode(r#""\u{FFFFFFFFF}""#), Err(LuaError::InvalidEscape('u')));
        assert_eq!(decode(r#""\xff""#), Err(LuaError::InvalidUtf8));
        assert_eq!(decode(r#""\228\189""#), Err(LuaError::InvalidUtf8));
        assert_eq!(decode("\"no end"), Err(LuaError::Unterminated("string")));
        assert_eq!(decode("\"ends in a backslash\\"), Err(LuaError::Unterminated("string")));
        assert_eq!(decode("'line\nbreak'"), Err(LuaError::Unterminated("string")));
        assert_eq!(decode("[==[mismatched]=]"), Err(LuaError::Unterminated("long string")));
    }

    #[test]
    fn lua_long_brackets_match_their_level_and_read_no_escapes() {
        assert_eq!(decode("[[x]]"), Ok("x".into()));
        assert_eq!(decode("[==[x]]y]=]z]==]"), Ok("x]]y]=]z".into()));
        assert_eq!(decode(r"[[\n is not an escape]]"), Ok(r"\n is not an escape".into()));
        // The break straight after the opening bracket is not part of
        // the string; later ones are, each as `\n`.
        assert_eq!(decode("[[\r\nfirst\r\nsecond\n]]"), Ok("first\nsecond\n".into()));
        assert_eq!(string_literal("[=[a]=], rest"), Ok(("a".into(), 7)));
    }

    #[test]
    fn lua_argument_lists_read_tables_by_key_and_position() {
        let args = lua_arguments(r#"{ window = "address:0x5", x = -25, relative = true; ["y"] = 1.5e+3, 'loose' }, "second""#);
        assert_eq!(
            args,
            Ok(vec![
                Literal::Table(vec![
                    (Some("window".into()), Literal::Str("address:0x5".into())),
                    (Some("x".into()), Literal::Word("-25".into())),
                    (Some("relative".into()), Literal::Word("true".into())),
                    (Some("y".into()), Literal::Word("1.5e+3".into())),
                    (None, Literal::Str("loose".into())),
                ]),
                Literal::Str("second".into()),
            ])
        );
        let args = args.unwrap_or_default();
        assert_eq!(lua_field(&args, "x").as_deref(), Some("-25"));
        assert_eq!(lua_string(&args).as_deref(), Some("second"));
        assert_eq!(lua_arguments("  "), Ok(Vec::new()));

        // Only a table's own keys are fields.
        let args = lua_arguments(r#"{ cursor = { invisible = true } }, "workspace = 3""#).unwrap_or_default();
        assert_eq!(lua_field(&args, "invisible"), None);
        assert_eq!(lua_field(&args, "workspace"), None);
        assert!(matches!(lua_value(&args, "cursor"), Some(Literal::Table(_))));
    }

    #[test]
    fn lua_argument_lists_refuse_what_is_not_a_literal() {
        assert_eq!(lua_arguments(r#""a" .. "b""#), Err(LuaError::Unexpected('.')));
        assert_eq!(lua_arguments("{ workspace = "), Err(LuaError::MissingValue));
        assert_eq!(lua_arguments("{ workspace = 1"), Err(LuaError::Unterminated("table")));
        assert_eq!(lua_arguments(r#""a","#), Err(LuaError::MissingValue));
        assert_eq!(lua_arguments("{ a.b = 1 }"), Err(LuaError::Unexpected('=')));
        assert_eq!(lua_arguments("{ [\"k\" = 1 }"), Err(LuaError::Unexpected('=')));
        assert_eq!(lua_arguments("os.exit()"), Err(LuaError::Unexpected('(')));
    }

    #[test]
    fn lua_table_nesting_is_bounded() {
        let nested = |depth: usize| format!("{}{}", "{".repeat(depth), "}".repeat(depth));
        assert!(lua_arguments(&nested(MAX_LUA_DEPTH)).is_ok());
        assert_eq!(lua_arguments(&nested(MAX_LUA_DEPTH + 1)), Err(LuaError::TooDeep));
        // A request-sized run of openers stops at the bound rather than
        // recursing once per brace.
        assert_eq!(lua_arguments(&"{".repeat(crate::request::MAX_REQUEST)), Err(LuaError::TooDeep));
    }
}
