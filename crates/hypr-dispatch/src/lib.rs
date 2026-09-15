//! The Hyprland dispatcher vocabulary chonkstep answers, in one place.
//!
//! Two front ends read Hyprland dispatchers. `wm-config` reads one out
//! of a keybinding in `hyprland.conf` or Omarchy's Lua, and
//! `chonk-hyprland-ipc` reads one off the socket when a script runs
//! `hyprctl dispatch`. Each keeps its own *lowering*, because a binding
//! and a request really are different: a binding always acts on the
//! focused window and has no live desktop to resolve a selector
//! against, while a request can name any window and must be refused,
//! loudly, when it names one that is not there. What the two must not
//! keep separately is the *vocabulary* — which names are recognised,
//! which are served, and the reason each unserved one is refused —
//! because two copies of that table drift, and did: `pin` was refused
//! as a binding for needing window groups while `hyprctl dispatch pin`
//! worked.
//!
//! So this crate owns the table, [`CLASSIC`], and both front ends gate
//! their own match on it: a name the table does not list is unknown to
//! both, a name it lists as [`Support::Unsupported`] is refused by
//! both with the same words, and a name it lists as served has to be
//! served on every side the table says — a conformance test in
//! `chonk-hyprland-ipc` walks the whole table and fails otherwise. The
//! differences that remain are deliberate and enumerated in
//! [`BindingGap`], each with the reason a user reads.
//!
//! It also owns the Lua spelling. Omarchy 4 writes every dispatcher as
//! `hl.dsp.<path>(<table>)`, and each front end used to translate that
//! by hand into whatever it understood; [`flatten`] turns a Lua call
//! into the classic `name, argument` pair exactly once, so the Lua
//! vocabulary is the classic one under another spelling and can never
//! be wider on one side than the other.

/// The largest one-based workspace number a dispatcher may name, the
/// same on the keyboard and on the socket.
///
/// The socket is unauthenticated because it grants nothing the user's
/// own keyboard does not already grant, and that argument only holds
/// while the two reach the same workspaces. `wm_core::MAX_WORKSPACES`
/// is the authoritative ceiling; `wm-config` restates this value and
/// checks it against the core at compile time, which this crate cannot
/// do without a dependency it deliberately does not have. If the core
/// constant moves, this one follows, and that assertion says so.
pub const MAX_WORKSPACE: usize = 99;

/// How chonkstep answers one dispatcher, on both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// A binding reaches a chonkstep action and so does `hyprctl`.
    Served,
    /// Recognised, and outside chonkstep's model. Both front ends
    /// refuse it with this one reason, written in terms of what
    /// chonkstep is rather than what it lacks.
    Unsupported(&'static str),
    /// `hyprctl dispatch` serves it; a binding does not, for a reason
    /// that is a difference of context rather than a gap in the table.
    IpcOnly(BindingGap),
    /// A binding serves it; `hyprctl dispatch` refuses it with this
    /// reason.
    BindingOnly(&'static str),
}

/// Why a dispatcher `hyprctl` serves is not a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingGap {
    /// Chonkstep could bind it and declines to: the modal window
    /// switcher already answers Alt-Tab, and it is not a binding at
    /// all — while it is up the shell owns the keyboard. Binding the
    /// dispatcher from a config file would break it.
    Declined,
    /// It names a window by selector. A binding acts on the focused
    /// window, so the same dispatcher without the selector is the one
    /// to bind; the socket resolves the selector against the live
    /// desktop, which a config file does not have.
    Selector,
    /// Chonkstep has no binding verb for it yet. The socket reaches
    /// the action through its own request table; a binding will when a
    /// verb exists.
    NotBindableYet,
}

impl BindingGap {
    /// The one-line reason a binding is refused for, as the config
    /// reader logs it and the docs table prints it.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Declined => "declined on purpose: the window switcher owns Alt-Tab",
            Self::Selector => "names a window by selector; a binding acts on the focused window",
            Self::NotBindableYet => "served over hyprctl dispatch; ChonkStep has no binding verb for it yet",
        }
    }
}

/// One classic dispatcher name and chonkstep's answer to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Classic {
    /// The name, as `hyprland.conf` and `hyprctl` spell it.
    pub name: &'static str,
    /// An argument the dispatcher is served with (or refused with,
    /// when it is not served at all), for the conformance test and the
    /// docs table. Empty for a dispatcher that takes none.
    pub example: &'static str,
    pub support: Support,
}

const NO_GROUPS: &str = "chonkstep has no window groups";
const SEAT_INPUT: &str = "synthesises a key at the seat, which is the compositor's own input path";

/// Every classic dispatcher name chonkstep recognises, in either front
/// end. A name that is not here is unknown to both.
///
/// Grouped by what the dispatcher acts on, so a diff against Hyprland's
/// own list is a read down one table.
pub const CLASSIC: &[Classic] = &[
    // The focused window.
    Classic { name: "killactive", example: "", support: Support::Served },
    Classic { name: "closewindow", example: "activewindow", support: Support::Served },
    Classic { name: "fullscreen", example: "", support: Support::Served },
    Classic { name: "fullscreenstate", example: "0 2", support: Support::Served },
    Classic { name: "togglefloating", example: "", support: Support::Served },
    Classic { name: "setfloating", example: "", support: Support::Served },
    Classic { name: "settiled", example: "", support: Support::Served },
    Classic { name: "pin", example: "", support: Support::Served },
    Classic { name: "centerwindow", example: "", support: Support::Served },
    Classic { name: "toggleopaque", example: "", support: Support::Served },
    Classic { name: "setprop", example: "activewindow opaque toggle", support: Support::Served },
    Classic { name: "resizeactive", example: "10 0", support: Support::Served },
    Classic { name: "moveactive", example: "10 0", support: Support::IpcOnly(BindingGap::NotBindableYet) },
    Classic { name: "resizewindowpixel", example: "10 0,activewindow", support: Support::IpcOnly(BindingGap::Selector) },
    Classic { name: "movewindowpixel", example: "10 0,activewindow", support: Support::IpcOnly(BindingGap::Selector) },
    Classic { name: "alterzorder", example: "top", support: Support::IpcOnly(BindingGap::NotBindableYet) },
    Classic { name: "tagwindow", example: "+pop", support: Support::IpcOnly(BindingGap::NotBindableYet) },
    // Focus and the window switcher.
    Classic { name: "movefocus", example: "l", support: Support::Served },
    Classic { name: "focuswindow", example: "activewindow", support: Support::IpcOnly(BindingGap::Selector) },
    Classic { name: "focusmonitor", example: "+1", support: Support::Served },
    Classic { name: "cyclenext", example: "", support: Support::IpcOnly(BindingGap::Declined) },
    Classic { name: "bringactivetotop", example: "", support: Support::IpcOnly(BindingGap::Declined) },
    Classic {
        name: "focuscurrentorlast",
        example: "",
        support: Support::Unsupported("chonkstep has no verb for the previously focused window"),
    },
    Classic {
        name: "focuswindowbyclass",
        example: "foot",
        support: Support::Unsupported("is not a Hyprland dispatcher; focuswindow class:NAME is served"),
    },
    // The layout.
    Classic { name: "movewindow", example: "l", support: Support::Served },
    Classic { name: "swapwindow", example: "l", support: Support::Served },
    Classic { name: "movewindoworgroup", example: "l", support: Support::Served },
    Classic { name: "swapnext", example: "", support: Support::Served },
    Classic { name: "layoutmsg", example: "togglesplit", support: Support::Served },
    Classic { name: "togglesplit", example: "", support: Support::Served },
    Classic { name: "swapsplit", example: "", support: Support::Served },
    Classic { name: "pseudo", example: "", support: Support::Served },
    Classic { name: "splitratio", example: "0.5", support: Support::Served },
    Classic { name: "togglelayout", example: "", support: Support::Served },
    Classic { name: "layout", example: "mosaic", support: Support::Served },
    Classic { name: "workspaceopt", example: "allfloat", support: Support::Unsupported("chonkstep has no per-workspace layout options") },
    // Groups, which this desktop does not have.
    Classic { name: "togglegroup", example: "", support: Support::Unsupported(NO_GROUPS) },
    Classic { name: "changegroupactive", example: "f", support: Support::Unsupported(NO_GROUPS) },
    Classic { name: "moveintogroup", example: "l", support: Support::Unsupported(NO_GROUPS) },
    Classic { name: "moveoutofgroup", example: "", support: Support::Unsupported(NO_GROUPS) },
    Classic { name: "lockgroups", example: "toggle", support: Support::Unsupported(NO_GROUPS) },
    Classic { name: "lockactivegroup", example: "toggle", support: Support::Unsupported(NO_GROUPS) },
    Classic { name: "denywindowfromgroup", example: "toggle", support: Support::Unsupported(NO_GROUPS) },
    // Workspaces.
    Classic { name: "workspace", example: "3", support: Support::Served },
    Classic { name: "focusworkspaceoncurrentmonitor", example: "3", support: Support::Served },
    Classic { name: "movetoworkspace", example: "3", support: Support::Served },
    Classic { name: "movetoworkspacesilent", example: "3", support: Support::Served },
    Classic { name: "togglespecialworkspace", example: "", support: Support::Served },
    Classic { name: "movecurrentworkspacetomonitor", example: "l", support: Support::Served },
    Classic {
        name: "moveworkspacetomonitor",
        example: "3 l",
        support: Support::Unsupported("names a workspace other than the active one; movecurrentworkspacetomonitor moves the active Space"),
    },
    Classic {
        name: "swapactiveworkspaces",
        example: "l r",
        support: Support::Unsupported("swaps the workspaces of two displays, which the shared desktop does not have"),
    },
    Classic {
        name: "renameworkspace",
        example: "3 mail",
        support: Support::Unsupported("names a workspace; chonkstep workspaces are numbered"),
    },
    // Commands, keys and the compositor itself.
    Classic { name: "exec", example: "foot", support: Support::Served },
    Classic { name: "chonkstep", example: "overview", support: Support::Served },
    Classic {
        name: "global",
        example: "app:id",
        support: Support::BindingOnly("fires a portal global shortcut from its key, which the socket does not press"),
    },
    Classic { name: "movecursor", example: "10 20", support: Support::IpcOnly(BindingGap::NotBindableYet) },
    Classic { name: "dpms", example: "off", support: Support::IpcOnly(BindingGap::NotBindableYet) },
    Classic { name: "sendshortcut", example: "CTRL, c,", support: Support::Unsupported(SEAT_INPUT) },
    Classic { name: "sendkeystate", example: "CTRL, c, down,", support: Support::Unsupported(SEAT_INPUT) },
    Classic { name: "send_key_state", example: "", support: Support::Unsupported(SEAT_INPUT) },
    Classic { name: "sendkey", example: "", support: Support::Unsupported(SEAT_INPUT) },
    Classic { name: "submap", example: "resize", support: Support::Unsupported("chonkstep's keybindings do not have submaps") },
    Classic { name: "exit", example: "", support: Support::Unsupported("ends the compositor, which chonkstep does not do for a dispatcher") },
    Classic {
        name: "forcerendererreload",
        example: "",
        support: Support::Unsupported("reloads Hyprland's renderer, which is not running"),
    },
    Classic {
        name: "exec-shutdown",
        example: "true",
        support: Support::Unsupported("runs a command as Hyprland exits, which is not running"),
    },
];

/// The table's entry for a classic dispatcher name, or `None` for a
/// name chonkstep does not recognise on either side. Case-insensitive,
/// as Hyprland's own dispatcher names are.
pub fn classic(name: &str) -> Option<&'static Classic> {
    let name = name.trim();
    CLASSIC.iter().find(|entry| entry.name.eq_ignore_ascii_case(name))
}

/// One Lua `hl.dsp.<path>` form and the classic spelling it flattens to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lua {
    /// The path after `hl.dsp.`.
    pub path: &'static str,
    /// The call's argument list, as Omarchy writes it.
    pub example: &'static str,
    /// The classic dispatcher and argument the example becomes, in one
    /// string as `hyprctl dispatch` would take it. `exec_cmd` is the
    /// one path with no classic spelling: its string is shell source,
    /// where classic `exec` is an argv, and the two are kept apart on
    /// purpose because `hyprctl` flattens an argv on the wire.
    pub classic: &'static str,
}

/// Every Lua dispatcher path [`flatten`] recognises, with each of its
/// forms. A path that is not here is unknown to both front ends.
pub const LUA: &[Lua] = &[
    Lua { path: "exec_cmd", example: "\"foot\"", classic: "exec foot" },
    Lua { path: "window.close", example: "", classic: "killactive" },
    Lua { path: "window.close", example: "{ window = \"activewindow\" }", classic: "closewindow activewindow" },
    Lua { path: "window.fullscreen", example: "{ mode = \"fullscreen\" }", classic: "fullscreen 0" },
    Lua { path: "window.fullscreen", example: "{ mode = \"maximized\" }", classic: "fullscreen 1" },
    Lua { path: "window.fullscreen_state", example: "{ internal = 0, client = 2 }", classic: "fullscreenstate 0 2" },
    Lua { path: "window.pseudo", example: "", classic: "pseudo" },
    Lua { path: "window.float", example: "{ action = \"toggle\" }", classic: "togglefloating" },
    Lua { path: "window.float", example: "{ action = \"on\" }", classic: "setfloating" },
    Lua { path: "window.float", example: "{ action = \"off\" }", classic: "settiled" },
    Lua { path: "window.pin", example: "", classic: "pin" },
    Lua { path: "window.center", example: "", classic: "centerwindow" },
    Lua { path: "window.alter_zorder", example: "{ mode = \"top\" }", classic: "alterzorder top" },
    Lua { path: "window.tag", example: "{ tag = \"+pop\" }", classic: "tagwindow +pop" },
    Lua { path: "window.set_prop", example: "{ prop = \"opaque\", value = \"toggle\" }", classic: "setprop activewindow opaque toggle" },
    Lua { path: "window.swap", example: "{ direction = \"l\" }", classic: "swapwindow l" },
    Lua { path: "window.resize", example: "{ x = 10, y = 0, relative = true }", classic: "resizeactive 10 0" },
    Lua { path: "window.resize", example: "{ x = 1300, y = 900 }", classic: "resizeactive exact 1300 900" },
    Lua { path: "window.move", example: "{ workspace = \"3\" }", classic: "movetoworkspace 3" },
    Lua { path: "window.move", example: "{ workspace = \"3\", follow = false }", classic: "movetoworkspacesilent 3" },
    Lua { path: "window.move", example: "{ x = 10, y = 0, relative = true }", classic: "moveactive 10 0" },
    Lua { path: "window.move", example: "{ x = 40, y = 30 }", classic: "moveactive exact 40 30" },
    Lua { path: "window.move", example: "{ into_group = \"l\" }", classic: "moveintogroup l" },
    Lua { path: "window.move", example: "{ out_of_group = true }", classic: "moveoutofgroup" },
    Lua { path: "window.cycle_next", example: "", classic: "cyclenext" },
    Lua { path: "window.cycle_next", example: "{ next = false }", classic: "cyclenext prev" },
    Lua { path: "window.bring_to_top", example: "", classic: "bringactivetotop" },
    Lua { path: "focus", example: "{ direction = \"l\" }", classic: "movefocus l" },
    Lua { path: "focus", example: "{ workspace = \"3\" }", classic: "workspace 3" },
    Lua { path: "focus", example: "{ window = \"activewindow\" }", classic: "focuswindow activewindow" },
    Lua { path: "focus", example: "{ monitor = \"+1\" }", classic: "focusmonitor +1" },
    Lua { path: "workspace.toggle_special", example: "\"scratchpad\"", classic: "togglespecialworkspace scratchpad" },
    Lua { path: "workspace.move", example: "{ monitor = \"l\" }", classic: "movecurrentworkspacetomonitor l" },
    Lua { path: "layout", example: "\"togglesplit\"", classic: "layoutmsg togglesplit" },
    Lua { path: "group.toggle", example: "", classic: "togglegroup" },
    Lua { path: "group.next", example: "", classic: "changegroupactive f" },
    Lua { path: "group.prev", example: "", classic: "changegroupactive b" },
    Lua { path: "group.active", example: "{ index = 1 }", classic: "changegroupactive 1" },
    Lua { path: "send_key_state", example: "{ mods = \"CTRL\", key = \"c\", state = \"down\" }", classic: "sendkeystate" },
    Lua { path: "send_shortcut", example: "{ mods = \"CTRL\", key = \"c\" }", classic: "sendshortcut" },
    Lua { path: "cursor.move", example: "{ x = 10, y = 20 }", classic: "movecursor 10 20" },
    Lua { path: "dpms", example: "{ state = \"off\" }", classic: "dpms off" },
    Lua { path: "dpms", example: "{ action = \"disable\", monitor = \"eDP-1\" }", classic: "dpms off eDP-1" },
];

/// The arguments of one `hl.dsp.*` call, as the front end read them.
///
/// Each front end has its own Lua reader — the config reader walks
/// whole files, the IPC reads one argument list off the wire — and the
/// flattening needs only this much of either: the text of a named
/// field, and the first positional string.
pub trait LuaCall {
    /// The text of `key = value` in the call's first table argument: a
    /// string's decoded text, a number's digits, or `true` / `false`.
    /// `None` for a missing key and for a value that is itself a table.
    fn field(&self, key: &str) -> Option<String>;
    /// The first positional argument that is a string.
    fn positional(&self) -> Option<String>;
}

/// A Lua call in classic spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flattened {
    /// A classic dispatcher and its argument, exactly as a
    /// `hyprland.conf` line or `hyprctl dispatch` would give them.
    Verb { name: &'static str, arg: String },
    /// `hl.dsp.exec_cmd`: a command line that is shell source, kept
    /// apart from classic `exec` on purpose. Empty when the call
    /// carried no command; each front end refuses that in its own
    /// words.
    ExecShell(String),
    /// A path this table does not know. Each front end reports it as
    /// unknown by its own name.
    Unknown,
}

/// Flatten one `hl.dsp.<path>(...)` call onto the classic vocabulary.
///
/// `path` is the part after `hl.dsp.`. Only the forms Omarchy writes
/// are translated, and every one of them is a row of [`LUA`]; a form
/// missing a field it needs becomes the classic dispatcher with an
/// argument that dispatcher refuses, rather than a guess. A missing
/// `window` field means the focused window, which is what an empty
/// classic selector means too, so it is simply left out.
pub fn flatten(path: &str, call: &impl LuaCall) -> Flattened {
    let field = |key: &str| call.field(key).unwrap_or_default();
    let verb = |name: &'static str, arg: String| Flattened::Verb { name, arg };
    // A window selector after an argument, as `pin address:0x…` and
    // `tagwindow +pop address:0x…` take one.
    let with_window = |arg: String| match call.field("window") {
        Some(window) if !window.trim().is_empty() => {
            if arg.is_empty() {
                window.trim().to_string()
            } else {
                format!("{arg} {}", window.trim())
            }
        }
        _ => arg,
    };
    // `x` and `y` are a delta with `relative = true` and an exact
    // position or size without it, which is the classic `exact` prefix.
    let geometry = |name: &'static str| {
        let arg = match (call.field("x"), call.field("y")) {
            (Some(x), Some(y)) if call.field("relative").as_deref() == Some("true") => format!("{x} {y}"),
            (Some(x), Some(y)) => format!("exact {x} {y}"),
            _ => String::new(),
        };
        verb(name, with_window(arg))
    };
    match path {
        "exec_cmd" => Flattened::ExecShell(call.field("cmd").or_else(|| call.positional()).unwrap_or_default()),
        "window.close" => match call.field("window") {
            Some(window) if !window.trim().is_empty() => verb("closewindow", window.trim().to_string()),
            _ => verb("killactive", String::new()),
        },
        "window.fullscreen" => verb("fullscreen", if field("mode") == "maximized" { "1".into() } else { "0".into() }),
        // Both axes, in the order the classic dispatcher takes them; a
        // missing axis keeps the argument short, which is refused.
        "window.fullscreen_state" => {
            let arg = match (call.field("internal"), call.field("client")) {
                (Some(internal), Some(client)) => format!("{internal} {client}"),
                _ => String::new(),
            };
            verb("fullscreenstate", with_window(arg))
        }
        "window.pseudo" => verb("pseudo", String::new()),
        "window.float" => verb(
            match call.field("action").as_deref() {
                Some("on" | "set") => "setfloating",
                Some("off" | "unset") => "settiled",
                _ => "togglefloating",
            },
            with_window(String::new()),
        ),
        "window.pin" => verb("pin", with_window(String::new())),
        "window.center" => verb("centerwindow", with_window(String::new())),
        "window.alter_zorder" => verb("alterzorder", with_window(field("mode"))),
        "window.tag" => verb("tagwindow", with_window(field("tag"))),
        // Classic `setprop` names its window first and has no "focused"
        // default of its own, so the Lua default is spelled out.
        "window.set_prop" => {
            let window = call.field("window").filter(|window| !window.trim().is_empty());
            let window = window.map_or_else(|| "activewindow".to_string(), |window| window.trim().to_string());
            let value = call.field("value").unwrap_or_else(|| "toggle".to_string());
            verb("setprop", format!("{window} {} {value}", field("prop")))
        }
        "window.swap" => verb("swapwindow", field("direction")),
        "window.resize" => geometry("resizeactive"),
        "window.move" => {
            // Three dispatchers wearing one name: to a workspace, into
            // or out of a group, or to a position.
            if call.field("into_group").is_some() {
                return verb("moveintogroup", field("into_group"));
            }
            if call.field("out_of_group").is_some() {
                return verb("moveoutofgroup", String::new());
            }
            match call.field("workspace") {
                Some(workspace) => {
                    let name = if call.field("follow").as_deref() == Some("false") {
                        "movetoworkspacesilent"
                    } else {
                        "movetoworkspace"
                    };
                    let arg = match call.field("window") {
                        Some(window) if !window.trim().is_empty() => format!("{workspace},{}", window.trim()),
                        _ => workspace,
                    };
                    verb(name, arg)
                }
                None => geometry("moveactive"),
            }
        }
        "window.cycle_next" => verb(
            "cyclenext",
            if call.field("next").as_deref() == Some("false") { "prev".into() } else { String::new() },
        ),
        "window.bring_to_top" => verb("bringactivetotop", String::new()),
        "focus" => {
            if let Some(workspace) = call.field("workspace") {
                return verb("workspace", workspace);
            }
            if let Some(window) = call.field("window") {
                return verb("focuswindow", window);
            }
            if let Some(monitor) = call.field("monitor") {
                return verb("focusmonitor", monitor);
            }
            verb("movefocus", field("direction"))
        }
        "workspace.toggle_special" => {
            verb("togglespecialworkspace", call.field("name").or_else(|| call.positional()).unwrap_or_default())
        }
        "workspace.move" => verb("movecurrentworkspacetomonitor", field("monitor")),
        "layout" => verb("layoutmsg", call.positional().unwrap_or_default()),
        "group.toggle" => verb("togglegroup", String::new()),
        "group.next" => verb("changegroupactive", "f".into()),
        "group.prev" => verb("changegroupactive", "b".into()),
        "group.active" => verb("changegroupactive", field("index")),
        "send_key_state" => verb("sendkeystate", String::new()),
        "send_shortcut" => verb("sendshortcut", String::new()),
        "cursor.move" => verb("movecursor", format!("{} {}", field("x"), field("y")).trim().to_string()),
        "dpms" => {
            let state = call
                .field("state")
                .or_else(|| call.field("enabled"))
                .or_else(|| call.field("action"))
                .or_else(|| call.positional())
                .unwrap_or_default();
            let state = match state.to_ascii_lowercase().as_str() {
                "enable" | "enabled" | "true" => "on".to_string(),
                "disable" | "disabled" | "false" => "off".to_string(),
                _ => state,
            };
            let output = call.field("output").or_else(|| call.field("monitor"));
            verb("dpms", format!("{state} {}", output.unwrap_or_default()).trim().to_string())
        }
        _ => Flattened::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every classic name is listed once, and every Lua row flattens to
    /// a classic name the table lists, so a Lua spelling can never
    /// reach a dispatcher the classic side does not know.
    #[test]
    fn the_tables_are_closed_over_each_other() {
        for (index, entry) in CLASSIC.iter().enumerate() {
            assert!(
                !CLASSIC[..index].iter().any(|earlier| earlier.name.eq_ignore_ascii_case(entry.name)),
                "{} is listed twice",
                entry.name
            );
            assert_eq!(classic(entry.name).map(|found| found.name), Some(entry.name));
        }
        for row in LUA {
            if row.path == "exec_cmd" {
                continue;
            }
            let name = row.classic.split_whitespace().next().unwrap_or_default();
            assert!(classic(name).is_some(), "hl.dsp.{} flattens to {name:?}, which CLASSIC does not list", row.path);
        }
        assert!(classic("Killactive").is_some(), "names are case-insensitive, as Hyprland's are");
        assert!(classic("nosuchdispatcher").is_none());
    }

    struct Call(Vec<(&'static str, &'static str)>, Option<&'static str>);

    impl LuaCall for Call {
        fn field(&self, key: &str) -> Option<String> {
            self.0.iter().find(|(name, _)| *name == key).map(|(_, value)| value.to_string())
        }
        fn positional(&self) -> Option<String> {
            self.1.map(str::to_string)
        }
    }

    #[test]
    fn a_window_selector_follows_the_argument_and_a_missing_one_means_the_focused_window() {
        assert_eq!(
            flatten("window.pin", &Call(vec![("window", "address:0x7")], None)),
            Flattened::Verb { name: "pin", arg: "address:0x7".into() }
        );
        assert_eq!(flatten("window.pin", &Call(vec![], None)), Flattened::Verb { name: "pin", arg: String::new() });
        assert_eq!(
            flatten("window.tag", &Call(vec![("tag", "-pop"), ("window", "address:0x7")], None)),
            Flattened::Verb { name: "tagwindow", arg: "-pop address:0x7".into() }
        );
        assert_eq!(
            flatten("window.move", &Call(vec![("workspace", "special:scratchpad"), ("follow", "false"), ("window", "address:0x7")], None)),
            Flattened::Verb { name: "movetoworkspacesilent", arg: "special:scratchpad,address:0x7".into() }
        );
        assert_eq!(
            flatten("window.resize", &Call(vec![("x", "25"), ("y", "10"), ("relative", "true"), ("window", "address:0x7")], None)),
            Flattened::Verb { name: "resizeactive", arg: "25 10 address:0x7".into() }
        );
        assert_eq!(
            flatten("window.set_prop", &Call(vec![("prop", "opaque"), ("value", "1")], None)),
            Flattened::Verb { name: "setprop", arg: "activewindow opaque 1".into() }
        );
    }

    #[test]
    fn a_call_missing_what_its_dispatcher_needs_keeps_the_dispatcher_and_loses_the_argument() {
        assert_eq!(flatten("window.resize", &Call(vec![("x", "10")], None)), Flattened::Verb { name: "resizeactive", arg: String::new() });
        assert_eq!(flatten("window.move", &Call(vec![], None)), Flattened::Verb { name: "moveactive", arg: String::new() });
        assert_eq!(flatten("focus", &Call(vec![], None)), Flattened::Verb { name: "movefocus", arg: String::new() });
        assert_eq!(flatten("exec_cmd", &Call(vec![], None)), Flattened::ExecShell(String::new()));
        assert_eq!(flatten("window.drag", &Call(vec![], None)), Flattened::Unknown);
    }
}
