//! One dispatcher, one verdict.
//!
//! `wm-config` reads a Hyprland dispatcher out of a keybinding and this
//! crate reads the same dispatcher off the socket, and each keeps its
//! own lowering. What they share is `hypr_dispatch`'s table, and these
//! tests are what make the sharing real: every name the table lists is
//! walked through both front ends, and the two verdicts have to agree
//! unless the table itself says why they differ — a selector a config
//! file cannot resolve, a verb this desktop has not grown yet, or the
//! window switcher's Alt-Tab. Any other disagreement is the drift this
//! table exists to end, and it fails here.
//!
//! The tests live in this crate because the dependency points this way
//! by design: `wm-config` is a dev-dependency here, and this crate is
//! nothing to `wm-config`.

use chonk_hyprland_ipc::dispatch::{self, Action, Direction, Outcome};
use chonk_hyprland_ipc::state::{Binding, Devices, Monitor, MonitorMode, Snapshot, Window, Workspace};
use hypr_dispatch::{BindingGap, Support, CLASSIC, LUA};
use wm_config::hyprland::directive::{Directive, Dispatcher};
use wm_config::hyprland::dispatch::{verb_for, Verb};
use wm_config::hyprland::lua;
use wm_config::preset::Unbound;

const FOCUSED: u64 = 4_294_967_297;

#[test]
fn lua_window_selectors_keep_spaces_and_commas_through_lowering() {
    let mut snapshot = desk();
    let mut target = snapshot.windows[0].clone();
    target.id = FOCUSED + 1;
    target.title = "Notes,  Work".into();
    snapshot.windows.push(target);
    for (source, expected) in [
        (r#"hl.dsp.window.set_prop({ window = "title:Notes,  Work", prop = "opaque", value = "toggle" })"#,
            Action::SetOpaque { window: FOCUSED + 1, opaque: None }),
        (r#"hl.dsp.window.resize({ window = "title:Notes,  Work", x = 640, y = 480 })"#,
            Action::ResizeWindow { window: FOCUSED + 1, width: 640, height: 480, relative: false }),
        (r#"hl.dsp.window.move({ window = "title:Notes,  Work", x = 10, y = 20, relative = true })"#,
            Action::MoveWindow { window: FOCUSED + 1, x: 10, y: 20, relative: true }),
        (r#"hl.dsp.window.fullscreen_state({ window = "title:Notes,  Work", internal = 0, client = 2 })"#,
            Action::FullscreenState { window: Some(FOCUSED + 1), internal: 0, client: 2 }),
        (r#"hl.dsp.window.alter_zorder({ window = "title:Notes,  Work", mode = "top" })"#,
            Action::RaiseWindow(FOCUSED + 1)),
        (r#"hl.dsp.window.tag({ window = "title:Notes,  Work", tag = "+chosen" })"#,
            Action::SetTag { window: FOCUSED + 1, tag: "chosen".into(), present: true }),
    ] {
        assert_eq!(dispatch::parse(source, &snapshot), Outcome::Run(expected), "{source}");
    }
}

/// A desk every served example is served on: one focused window, one
/// output with a neighbour for the monitor verbs, separate Spaces so a
/// workspace move has a Space to move, and a reported `chonkstep`
/// binding for the replay verb to find.
fn desk() -> Snapshot {
    let monitor = |id: i32, name: &str, x: i32, focused: bool| Monitor {
        id,
        name: name.to_string(),
        description: format!("a {name}"),
        x,
        y: 0,
        width: 2560,
        height: 1600,
        scale: 2.0,
        powered: true,
        vrr_supported: false,
        vrr_enabled: false,
        focused,
        active_workspace: 0,
        special_workspace: None,
        make: "Sharp".to_string(),
        model: name.to_string(),
        serial: "0x01020304".to_string(),
        refresh_millihertz: 60_000,
        transform: 0,
        modes: vec![MonitorMode { width: 2560, height: 1600, refresh_millihertz: 60_000 }],
    };
    Snapshot {
        monitors: vec![monitor(0, "eDP-1", 0, true), monitor(1, "DP-1", 1280, false)],
        workspaces: vec![Workspace {
            layout: "freeform".into(),
            index: 0,
            monitor: "eDP-1".to_string(),
            monitor_id: 0,
            windows: 1,
            has_fullscreen: false,
        }],
        windows: vec![Window {
            floating: true,
            id: FOCUSED,
            title: "~ — foot".to_string(),
            class: "foot".to_string(),
            x: 10,
            y: 20,
            width: 800,
            height: 600,
            workspace: 0,
            special: None,
            monitor: 0,
            pid: 4242,
            xwayland: false,
            fullscreen: false,
            maximized: false,
            client_fullscreen: false,
            hidden: false,
            urgent: false,
            pinned: false,
            inhibiting_idle: false,
            tags: Vec::new(),
            xdg_tag: String::new(),
            xdg_description: String::new(),
            focus_history_id: 0,
        }],
        focused: Some(FOCUSED),
        bindings: vec![Binding {
            modifiers: 64,
            key: "Up".to_string(),
            description: "Overview".to_string(),
            dispatcher: "chonkstep".to_string(),
            argument: "overview".to_string(),
            locked: false,
            repeating: false,
            release: false,
        }],
        devices: Devices::default(),
        separate_spaces: true,
        ..Snapshot::default()
    }
}

/// The binding reader's verdict on a classic dispatcher line.
fn binding(name: &str, arg: &str) -> Verb {
    verb_for(&Dispatcher::Verb { name: name.to_string(), arg: arg.to_string() })
}

/// The socket's verdict on the same line.
fn ipc(name: &str, arg: &str, snapshot: &Snapshot) -> Outcome {
    dispatch::parse(format!("{name} {arg}").trim(), snapshot)
}

fn is_bound(verb: &Verb) -> bool {
    matches!(verb, Verb::Action(_) | Verb::Run(_))
}

/// Every classic name has a verdict on both sides, and the two agree
/// except where the table says they differ, and then differ exactly as
/// it says.
#[test]
fn every_classic_dispatcher_gets_one_verdict_on_both_sides() {
    let snapshot = desk();
    for entry in CLASSIC {
        let (name, example) = (entry.name, entry.example);
        let bound = binding(name, example);
        let served = ipc(name, example, &snapshot);
        match entry.support {
            Support::Served => {
                assert!(is_bound(&bound), "{name} {example}: a binding must reach an action, got {bound:?}");
                assert!(served.is_ok(), "{name} {example}: the socket must serve it, got {served:?}");
            }
            Support::Unsupported(why) => {
                assert_eq!(
                    bound,
                    Verb::Unbound(Unbound::Unsupported(why)),
                    "{name} {example}: a binding is refused with the table's reason"
                );
                assert_eq!(
                    served,
                    Outcome::Unsupported(why.to_string()),
                    "{name} {example}: the socket is refused with the table's reason"
                );
            }
            Support::IpcOnly(gap) => {
                assert!(served.is_ok(), "{name} {example}: the socket must serve it, got {served:?}");
                let expected = match gap {
                    BindingGap::Declined => Unbound::Declined,
                    BindingGap::Selector => Unbound::Selector,
                    BindingGap::NotBindableYet => Unbound::NotBindableYet,
                };
                assert_eq!(bound, Verb::Unbound(expected), "{name} {example}: a binding is refused for the documented gap");
            }
            Support::BindingOnly(why) => {
                assert!(is_bound(&bound), "{name} {example}: a binding must reach an action, got {bound:?}");
                assert_eq!(served, Outcome::Unsupported(why.to_string()), "{name} {example}");
            }
        }
    }
}

/// A name outside the table is unknown to both sides, so neither match
/// can quietly serve a dispatcher the other has never heard of.
#[test]
fn a_name_the_table_does_not_list_is_unknown_on_both_sides() {
    let snapshot = desk();
    for name in ["nosuchdispatcher", "window.drag", "focusurgentorlast"] {
        assert_eq!(binding(name, ""), Verb::Unbound(Unbound::NoVerb), "{name}");
        assert!(matches!(ipc(name, "", &snapshot), Outcome::Unknown(_)), "{name}");
    }
}

/// The drifts this table was made to end, pinned by name: each of these
/// used to be refused as a binding for a reason the socket contradicted.
#[test]
fn the_drifted_verbs_bind_or_say_exactly_why_not() {
    assert_eq!(binding("pin", ""), Verb::Action(wm_config::Action::TogglePin));
    assert_eq!(binding("centerwindow", ""), Verb::Action(wm_config::Action::Center));
    assert_eq!(binding("togglelayout", ""), Verb::Action(wm_config::Action::ToggleLayout));
    let named = |name: &str, arg: &str| match binding(name, arg) {
        Verb::Action(action) => action.config_name(),
        other => panic!("{name} {arg}: {other:?}"),
    };
    assert_eq!(named("layout", "flow").as_deref(), Some("layout-flow"));
    assert_eq!(named("layout", "dwindle").as_deref(), Some("layout-mosaic"), "Hyprland's names for the styles read too");
    assert_eq!(binding("toggleopaque", ""), Verb::Action(wm_config::Action::ToggleOpaque));
    assert_eq!(binding("setprop", "activewindow opaque toggle"), Verb::Action(wm_config::Action::ToggleOpaque));
    assert_eq!(binding("swapnext", "prev"), Verb::Action(wm_config::Action::Move(wm_config::FocusDirection::Left)));
    assert_eq!(binding("dpms", "off"), Verb::Unbound(Unbound::NotBindableYet));
    assert_eq!(binding("tagwindow", "+pop"), Verb::Unbound(Unbound::NotBindableYet));
    assert_eq!(binding("cyclenext", ""), Verb::Unbound(Unbound::Declined), "the modal switcher keeps Alt-Tab");
    assert_eq!(binding("togglegroup", ""), Verb::Unbound(Unbound::Unsupported("chonkstep has no window groups")));
    // A selector a config file cannot resolve, on a verb a binding
    // otherwise has: the same dispatcher without it is the one to bind.
    for (name, arg) in [
        ("pin", "address:0x100000001"),
        ("closewindow", "class:foot"),
        ("togglefloating", "class:foot"),
        ("movetoworkspacesilent", "3,class:foot"),
        ("resizeactive", "10 0 class:foot"),
        ("setprop", "class:foot opaque toggle"),
        ("fullscreenstate", "0 2 class:foot"),
    ] {
        assert_eq!(binding(name, arg), Verb::Unbound(Unbound::Selector), "{name} {arg}");
    }
    assert!(!Unbound::Selector.reason().is_empty() && !Unbound::NotBindableYet.reason().is_empty());
}

/// The classic line a Lua row flattens to, split as the conf reader
/// hands it on.
fn classic_of(row: &hypr_dispatch::Lua) -> (String, String) {
    let (name, arg) = row.classic.split_once(' ').unwrap_or((row.classic, ""));
    (name.to_string(), arg.to_string())
}

/// What the binding reader makes of `hl.bind("SUPER + F12", hl.dsp.<path>(<example>))`.
fn lua_binding(row: &hypr_dispatch::Lua) -> Dispatcher {
    let source = format!("hl.bind(\"SUPER + F12\", hl.dsp.{}({}))\n", row.path, row.example);
    let facts = lua::Facts { path: Vec::new(), home: None, state_home: None };
    let mut globals = lua::Globals::default();
    let mut out = Vec::new();
    lua::read(&source, &facts, &mut globals, &mut out);
    match out.as_slice() {
        [Directive::Bind { dispatcher, .. }] => dispatcher.clone(),
        other => panic!("{source:?} read as {other:?}"),
    }
}

/// Every Lua form is the classic form under another spelling, on both
/// sides: the binding reader flattens it to exactly the classic line
/// the table names, and the socket answers it exactly as it answers
/// that line.
#[test]
fn every_lua_dispatcher_is_its_classic_spelling_on_both_sides() {
    let snapshot = desk();
    for row in LUA {
        let call = format!("hl.dsp.{}({})", row.path, row.example);
        let (name, arg) = classic_of(row);
        let dispatcher = lua_binding(row);
        let over_socket = dispatch::parse(&call, &snapshot);
        if row.path == "exec_cmd" {
            // Shell source on both sides; the classic `exec` is an
            // argv, so the two lower differently and both run.
            assert_eq!(dispatcher, Dispatcher::Exec(arg.clone()), "{call}");
            assert!(over_socket.is_ok(), "{call}: {over_socket:?}");
            assert!(ipc(&name, &arg, &snapshot).is_ok());
            continue;
        }
        assert_eq!(dispatcher, Dispatcher::Verb { name: name.clone(), arg: arg.clone() }, "{call}");
        assert_eq!(over_socket, ipc(&name, &arg, &snapshot), "{call} must be answered as {:?} is", row.classic);
        assert_eq!(
            verb_for(&dispatcher),
            binding(&name, &arg),
            "{call} must bind as {:?} does",
            row.classic
        );
    }
}

/// The forms Omarchy's keybinding menu replays through `hyprctl
/// dispatch`, which the socket used to refuse while the same chord
/// worked from the keyboard.
#[test]
fn the_menus_replayed_lua_bindings_are_applied() {
    let snapshot = desk();
    assert_eq!(
        dispatch::parse(r#"hl.dsp.window.move({ workspace = "3" })"#, &snapshot),
        Outcome::Run(Action::MoveToWorkspace { window: None, workspace: 2, follow: true })
    );
    assert_eq!(
        dispatch::parse(r#"hl.dsp.window.move({ x = 40, y = 30 })"#, &snapshot),
        Outcome::Run(Action::MoveWindow { window: FOCUSED, x: 40, y: 30, relative: false }),
        "the geometry form keeps its meaning beside the workspace form"
    );
    assert_eq!(
        dispatch::parse(r#"hl.dsp.window.fullscreen({ mode = "maximized" })"#, &snapshot),
        Outcome::Run(Action::ToggleMaximize)
    );
    assert_eq!(
        dispatch::parse(r#"hl.dsp.focus({ direction = "l" })"#, &snapshot),
        Outcome::Run(Action::FocusDirection(Direction::Left))
    );
    assert_eq!(
        dispatch::parse(r#"hl.dsp.window.float({ action = "on" })"#, &snapshot),
        Outcome::Run(Action::SetFloating { window: FOCUSED, floating: Some(true) })
    );
    assert_eq!(
        dispatch::parse("hl.dsp.window.cycle_next({ next = false })", &snapshot),
        Outcome::Run(Action::CycleFocus { forward: false })
    );
    assert!(
        matches!(dispatch::parse(r#"hl.dsp.window.pin({ window = "address:0x9" })"#, &snapshot), Outcome::Unsupported(why) if why.contains("no window matches")),
        "a selector that matches nothing is refused, never quietly the focused window"
    );
}

const IPC_GUIDE: &str = include_str!("../../../docs/hyprland-ipc.md");
const CONFIG_GUIDE: &str = include_str!("../../../docs/hyprland-config.md");

/// The text between `<!-- hypr-dispatch: NAME begin -->` and its end
/// marker in a document.
fn region<'a>(document: &'a str, name: &str) -> &'a str {
    let begin = format!("<!-- hypr-dispatch: {name} begin -->\n");
    let end = format!("<!-- hypr-dispatch: {name} end -->");
    let start = document.find(&begin).unwrap_or_else(|| panic!("no {begin:?} marker")) + begin.len();
    let stop = document[start..].find(&end).unwrap_or_else(|| panic!("no {end:?} marker")) + start;
    &document[start..stop]
}

fn verdicts(support: Support) -> (String, String) {
    let served = "served".to_string();
    match support {
        Support::Served => (served.clone(), served),
        Support::Unsupported(why) => (format!("refused: {why}"), format!("refused: {why}")),
        Support::IpcOnly(gap) => (format!("refused: {}", gap.reason()), served),
        Support::BindingOnly(why) => (served, format!("refused: {why}")),
    }
}

fn classic_table(only_differences: bool) -> String {
    let mut table = String::from("| Dispatcher | Keybinding | `hyprctl dispatch` |\n|---|---|---|\n");
    for entry in CLASSIC {
        if only_differences && entry.support == Support::Served {
            continue;
        }
        let (bound, served) = verdicts(entry.support);
        let spelled = if entry.example.is_empty() {
            entry.name.to_string()
        } else {
            format!("{} {}", entry.name, entry.example)
        };
        table.push_str(&format!("| `{spelled}` | {bound} | {served} |\n"));
    }
    table
}

fn lua_table() -> String {
    let mut table = String::from("| Lua call | Classic spelling |\n|---|---|\n");
    for row in LUA {
        table.push_str(&format!("| `hl.dsp.{}({})` | `{}` |\n", row.path, row.example, row.classic));
    }
    table
}

/// The tables in the two guides are rendered from the shared table, and
/// a change to it has to update them: the checked-in text must match
/// what the table renders today.
#[test]
fn the_documented_dispatcher_tables_are_the_shared_table() {
    for (document, name, expected) in [
        (IPC_GUIDE, "classic", classic_table(false)),
        (IPC_GUIDE, "lua", lua_table()),
        (CONFIG_GUIDE, "bindings", classic_table(true)),
    ] {
        let actual = region(document, name);
        assert!(
            actual == expected,
            "the `{name}` table is out of date; replace it with:\n{expected}"
        );
    }
}
