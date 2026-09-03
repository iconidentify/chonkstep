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
//! cannot explain. A script that gets an error takes its *error* branch
//! — which its author wrote, and tested, and which usually falls back to
//! something that works.
//!
//! Omarchy proves the point in its own source.
//! `omarchy-launch-or-focus` does this:
//!
//! ```sh
//! hyprctl dispatch "hl.dsp.focus({ window = \"address:$ADDR\" })" \
//!   || hyprctl dispatch focuswindow "address:$ADDR"
//! ```
//!
//! A server that rejects the Lua form cleanly gets handed the classic
//! form on the next line, for free. A server that accepts the Lua form
//! and does nothing gets a script that opens a second copy of the app
//! every time — which is precisely the bug
//! `docs/omarchy-integration.md` records as "broken, silently".
//!
//! So: [`Outcome::Unsupported`] is a first-class result here, not a
//! shortfall, and it is reported to the caller as an error string
//! beginning with `Invalid dispatcher`, which is what Hyprland itself
//! says and therefore what callers already branch on.

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
    /// Close a specific window.
    CloseWindow(u64),
    /// Close the focused window.
    KillActive,
    /// Move a window (or the focused one) to a 0-based workspace index.
    MoveToWorkspace { window: Option<u64>, workspace: usize },
    /// Run a command line.
    Exec(String),
    /// Set or toggle fullscreen on the focused window.
    Fullscreen(Fullscreen),
    /// Focus the next/previous window.
    CycleFocus { forward: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fullscreen {
    Toggle,
    On,
    Off,
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
    ("movetoworkspacesilent", "chonkstep cannot move a window without following it"),
    ("workspaceopt", "chonkstep has no per-workspace layout options"),
    ("dpms", "chonkstep does not control output power from IPC"),
    ("submap", "chonkstep's keybindings do not have submaps"),
];

/// Verbs chonkstep understands but which target something it does not
/// model, listed separately from the tiling ones because the reason is
/// different and a reader deserves to know which kind of "no" this is.
const NOT_MODELLED: &[(&str, &str)] = &[
    ("pin", "chonkstep has no always-on-top pin"),
    ("togglefloating", "every chonkstep window already floats; there is nothing to toggle"),
    ("setfloating", "every chonkstep window already floats"),
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
        "closewindow" => match resolve_window(rest, snapshot) {
            Some(window) => Outcome::Run(Action::CloseWindow(window.id)),
            None => Outcome::Unsupported(format!("no window matches {rest:?}")),
        },
        "killactive" => Outcome::Run(Action::KillActive),
        "movetoworkspace" => {
            // `movetoworkspace 3` or `movetoworkspace 3,address:0x...`
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
                    None => {
                        return Outcome::Unsupported(format!("no window matches {selector:?}"))
                    }
                },
            };
            Outcome::Run(Action::MoveToWorkspace { window, workspace })
        }
        "exec" => {
            // `hyprctl dispatch exec -- bash -lc '...'` is the form
            // `omarchy-launch-screensaver` uses; the `--` is Hyprland's
            // own "no more flags" marker and is not part of the command.
            let command = rest.strip_prefix("--").unwrap_or(rest).trim();
            if command.is_empty() {
                Outcome::Unknown("exec with no command".to_string())
            } else {
                Outcome::Run(Action::Exec(command.to_string()))
            }
        }
        "fullscreen" => Outcome::Run(Action::Fullscreen(match rest.trim() {
            "" | "0" => Fullscreen::Toggle,
            // Hyprland's `1` is "maximize to the window's monitor",
            // which for a floating window manager with no tiling to
            // return to is the same operation as fullscreen.
            "1" | "2" => Fullscreen::On,
            _ => Fullscreen::Toggle,
        })),
        "fullscreenstate" => Outcome::Unsupported(
            "chonkstep has one fullscreen state, not a client/internal pair".to_string(),
        ),
        "cyclenext" => Outcome::Run(Action::CycleFocus { forward: !rest.contains("prev") }),
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
            Some(command) => Outcome::Run(Action::Exec(command)),
            None => Outcome::Unknown("hl.dsp.exec_cmd with no command".to_string()),
        },
        "window.float" | "window.set_prop" | "window.pin" => Outcome::Unsupported(format!(
            "hl.dsp.{path} controls a window property chonkstep does not model"
        )),
        "window.resize" | "window.move" | "window.center" | "window.alter_zorder"
        | "window.tag" => Outcome::Unsupported(format!(
            "hl.dsp.{path} is not implemented yet"
        )),
        "cursor.move" => {
            Outcome::Unsupported("chonkstep does not warp the pointer from IPC".to_string())
        }
        "dpms" => Outcome::Unsupported("chonkstep does not control output power from IPC".to_string()),
        other => Outcome::Unknown(format!("unknown Lua dispatcher hl.dsp.{other}")),
    }
}

/// Pull `key = "value"` (or `key = value`) out of a Lua table literal.
fn lua_field(body: &str, key: &str) -> Option<String> {
    let at = body.find(key)?;
    // Guard against `key` matching inside a longer identifier —
    // `subworkspace = ...` must not answer a lookup for `workspace`.
    if at > 0 {
        let before = body[..at].chars().next_back().unwrap_or(' ');
        if before.is_alphanumeric() || before == '_' || before == '.' {
            return None;
        }
    }
    let rest = body[at + key.len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    Some(match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or_default().to_string(),
        None => rest
            .split([',', '}', ' '])
            .next()
            .unwrap_or_default()
            .trim()
            .to_string(),
    })
}

/// The first bare string literal in a Lua argument list.
fn lua_string(body: &str) -> Option<String> {
    let start = body.find('"')?;
    let rest = &body[start + 1..];
    Some(rest.split('"').next()?.to_string())
}

/// The workspaces a switch may name.
///
/// `wm-core`'s `switch_workspace` grows the workspace row on demand, so
/// mechanically any index is reachable. The question is which ones this
/// socket *should* reach, and the answer comes from the security
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
/// Mirrors `wm_config::MAX_WORKSPACE`, deliberately by value rather than
/// by dependency: this crate stays free of chonkstep's own crates so
/// that every promise it makes to somebody else's binary can be tested
/// without booting a window manager (see the crate doc). If that
/// constant moves, this one follows.
const MAX_WORKSPACE: usize = 99;

/// Reject a workspace index the keyboard could not reach either.
fn in_range(index: usize) -> Result<usize, String> {
    if index < MAX_WORKSPACE {
        return Ok(index);
    }
    Err(format!(
        "chonkstep has workspaces 1 to {MAX_WORKSPACE}; {} is past the end",
        index + 1
    ))
}

/// Resolve a workspace selector to a 0-based chonkstep index.
fn workspace_target(target: &str, snapshot: &Snapshot) -> Result<usize, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("workspace with no argument".to_string());
    }
    if let Ok(id) = target.parse::<i32>() {
        let index = workspace_index_from_hypr_id(id)
            .ok_or_else(|| format!("chonkstep has no workspace {id}"))?;
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
    value
        .trim_start_matches('^')
        .trim_end_matches('$')
        .trim_start_matches('(')
        .trim_end_matches(')')
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}
