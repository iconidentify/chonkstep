//! Conformance tests for the promises this crate makes to somebody
//! else's binary.
//!
//! Every assertion here is traceable to a real consumer — a `jq` filter
//! in an Omarchy script, or a line of Quickshell's IPC client — rather
//! than to Hyprland's documentation. Where a test looks pedantic about
//! a field name or a bracket, the pedantry is the point: `.at[0]` on an
//! object yields `null` rather than an error, so a shape mistake here
//! becomes a silent wrong answer downstream, which is the exact failure
//! this whole crate is built to prevent.

use chonk_hyprland_ipc::dispatch::{self, Action, Direction, Fullscreen, LayoutTarget, MonitorTarget};
use chonk_hyprland_ipc::request::Request;
use chonk_hyprland_ipc::server::answer_payload;
use chonk_hyprland_ipc::state::{Devices, Keyboard, Monitor, MonitorMode, Snapshot, Window, Workspace};
use chonk_hyprland_ipc::{Differ, Outcome};
use wm_config::hyprland::dispatch::SERVED_OMARCHY_SCRIPTS;

fn monitor(id: i32, name: &str, focused: bool, active_workspace: usize) -> Monitor {
    Monitor {
        id,
        name: name.to_string(),
        description: format!("a {name}"),
        x: 0,
        y: 0,
        width: 2560,
        height: 1600,
        scale: 2.0,
        powered: true,
        vrr_supported: true,
        vrr_enabled: false,
        focused,
        active_workspace,
        make: "Sharp".to_string(),
        model: name.to_string(),
        serial: "0x01020304".to_string(),
        // 120 Hz on purpose: the value this used to report was a
        // conventional 60 for every panel, so a fixture that is also
        // 60 would pass either way.
        refresh_millihertz: 120_000,
        transform: 0,
        modes: vec![
            MonitorMode { width: 2560, height: 1600, refresh_millihertz: 120_000 },
            MonitorMode { width: 2560, height: 1600, refresh_millihertz: 60_000 },
        ],
    }
}

fn workspace(index: usize, windows: u32) -> Workspace {
    Workspace {
        layout: "freeform".into(),
        index,
        monitor: "eDP-1".to_string(),
        monitor_id: 0,
        windows,
        has_fullscreen: false,
    }
}

fn window(id: u64, title: &str, class: &str, workspace: usize) -> Window {
    Window {
        floating: true,
        id,
        title: title.to_string(),
        class: class.to_string(),
        x: 10,
        y: 20,
        width: 800,
        height: 600,
        workspace,
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
    }
}

fn desktop() -> Snapshot {
    Snapshot {
        monitors: vec![monitor(0, "eDP-1", true, 0)],
        workspaces: vec![workspace(0, 1), workspace(1, 0), workspace(2, 0)],
        windows: vec![window(4_294_967_297, "~ — foot", "foot", 0)],
        focused: Some(4_294_967_297),
        locked: false,
        cursor_position: Some((321, 654)),
        bindings: Vec::new(),
        config_errors: Vec::new(),
        devices: Devices::default(),
        system_info: "test system".into(),
        separate_spaces: false,
        previous_workspace: None,
    }
}

/// The same desk with a second head to the right, as `monitors` reports
/// it.
fn two_heads() -> Snapshot {
    let mut snapshot = desktop();
    let mut second = monitor(1, "DP-1", false, 1);
    second.x = 1280;
    snapshot.monitors.push(second);
    snapshot
}

/// The same desk with a session lock in force.
fn locked_desktop() -> Snapshot {
    Snapshot { locked: true, ..desktop() }
}

fn ask(wire: &str, snapshot: &Snapshot) -> String {
    answer_payload(wire.as_bytes(), snapshot).0
}

fn ask_json(wire: &str, snapshot: &Snapshot) -> serde_json::Value {
    serde_json::from_str(&ask(wire, snapshot)).expect("response should be JSON")
}

// ---------------------------------------------------------------------
// Workspace numbering: the translation most likely to be silently wrong.
// ---------------------------------------------------------------------

/// chonkstep numbers workspaces from 0; Hyprland from 1. Omarchy's bar
/// hard-codes `[1, 2, 3, 4, 5]` as the workspaces it always draws
/// (`plugins/bar/widgets/Workspaces.qml`), so a workspace served as 0
/// is one no bar button can ever match.
#[test]
fn workspaces_are_served_one_based() {
    let value = ask_json("j/workspaces", &desktop());
    let ids: Vec<i64> = value.as_array().unwrap().iter().map(|w| w["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, vec![1, 2, 3], "chonkstep workspace 0 must be served as Hyprland workspace 1");

    // The name must be the decimal id: Quickshell matches workspaces by
    // NAME in `focusedmon`, `openwindow` and `findWorkspaceByName`.
    let names: Vec<&str> = value.as_array().unwrap().iter().map(|w| w["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["1", "2", "3"]);
}

/// The 1-based/0-based conversion, pinned in both directions and at the
/// boundary, because it is converted in exactly one place and a silent
/// off-by-one here would be invisible until a user's third workspace
/// switched to their second.
///
/// `wm-core`'s `switch_workspace` and `carry_focused_to_workspace` are
/// both 0-based; chonkstep's config action strings and Hyprland's IPC
/// are both 1-based. The conversion therefore happens exactly once, on
/// the way in, and never again.
#[test]
fn the_one_based_conversion_happens_exactly_once() {
    let snapshot = desktop();
    for (hypr_id, chonk_index) in [(1_i32, 0_usize), (2, 1), (3, 2), (10, 9), (99, 98)] {
        let (_, actions) = answer_payload(format!("/dispatch workspace {hypr_id}").as_bytes(), &snapshot);
        assert_eq!(
            actions,
            vec![Action::FocusWorkspace(chonk_index)],
            "hyprland workspace {hypr_id} must be chonkstep index {chonk_index}"
        );
    }

    // And back out again: chonkstep index 0 is served as Hyprland id 1.
    let value = ask_json("j/workspaces", &snapshot);
    assert_eq!(value[0]["id"], serde_json::json!(1));

    // `movetoworkspace` converts through the same function, so it must
    // agree — a second conversion site is how the two would drift.
    let (_, actions) = answer_payload(b"/dispatch movetoworkspace 3", &snapshot);
    assert_eq!(actions, vec![Action::MoveToWorkspace { window: None, workspace: 2, follow: true }]);
    let (_, actions) = answer_payload(b"/dispatch movetoworkspacesilent 3", &snapshot);
    assert_eq!(actions, vec![Action::MoveToWorkspace { window: None, workspace: 2, follow: false }]);
}

/// `omarchy-capture-region --select-window` moves the pointer onto the next
/// window with `hl.dsp.cursor.move`, in the logical units `cursorpos`
/// reports. Both spellings reach one action; anything but two integers is
/// refused by name rather than guessed at.
#[test]
fn a_pointer_warp_parses_in_both_spellings_and_refuses_anything_else() {
    let snapshot = desktop();
    for payload in [&b"/eval hl.dispatch(hl.dsp.cursor.move({ x = 10, y = 20 }))"[..], b"/dispatch movecursor 10 20"] {
        let (_, actions) = answer_payload(payload, &snapshot);
        assert_eq!(actions, vec![Action::WarpPointer { x: 10, y: 20 }], "{}", String::from_utf8_lossy(payload));
    }
    for payload in [
        &b"/eval hl.dispatch(hl.dsp.cursor.move({ x = 1.5, y = 20 }))"[..],
        b"/eval hl.dispatch(hl.dsp.cursor.move({ x = 10 }))",
        b"/dispatch movecursor 10",
        b"/dispatch movecursor ten 20",
        b"/dispatch movecursor 10 20 30",
    ] {
        let (_, actions) = answer_payload(payload, &snapshot);
        assert!(actions.is_empty(), "{} must be refused", String::from_utf8_lossy(payload));
    }
}

/// Arriving on a workspace by a bare switch leaves nothing focused —
/// a real wart on chonkstep's side, tracked separately. The IPC layer
/// must report it rather than invent a focused window to fill the gap.
/// The shape for "nothing focused" is an empty object, which is what
/// the real `hyprctl activewindow -j` prints on a Hyprland box.
/// Omarchy's screensaver focuses each monitor by the name `monitors -j`
/// reports, its bindings by step (`CTRL+ALT+TAB`) and direction. Every
/// spelling is an action; a name no output carries is refused by name;
/// and behind a session lock the whole verb is refused, since it moves
/// the pointer and the keyboard.
#[test]
fn monitor_focus_parses_every_spelling_and_refuses_an_unknown_output_by_name() {
    let snapshot = two_heads();
    for (wire, target) in [
        ("/dispatch focusmonitor +1", MonitorTarget::Relative(1)),
        ("/dispatch focusmonitor -1", MonitorTarget::Relative(-1)),
        ("/dispatch focusmonitor current", MonitorTarget::Relative(0)),
        ("/dispatch focusmonitor l", MonitorTarget::Direction(Direction::Left)),
        ("/dispatch focusmonitor right", MonitorTarget::Direction(Direction::Right)),
        ("/dispatch focusmonitor u", MonitorTarget::Direction(Direction::Up)),
        ("/dispatch focusmonitor d", MonitorTarget::Direction(Direction::Down)),
        ("/dispatch focusmonitor DP-1", MonitorTarget::Name("DP-1".into())),
        ("/dispatch focusmonitor 1", MonitorTarget::Name("DP-1".into())),
        ("/dispatch hl.dsp.focus({ monitor = \"DP-1\" })", MonitorTarget::Name("DP-1".into())),
        ("/dispatch hl.dsp.focus({ monitor = \"+1\" })", MonitorTarget::Relative(1)),
        ("/eval hl.dispatch(hl.dsp.focus({ monitor = \"eDP-1\" }))", MonitorTarget::Name("eDP-1".into())),
    ] {
        let (response, actions) = answer_payload(wire.as_bytes(), &snapshot);
        assert_eq!(response.trim(), "ok", "{wire}");
        assert_eq!(actions, vec![Action::FocusMonitor(target)], "{wire}");
    }
    for wire in [
        "/dispatch focusmonitor",
        "/dispatch focusmonitor DP-9",
        "/dispatch focusmonitor 7",
        "/dispatch focusmonitor +",
        "/dispatch hl.dsp.focus({ monitor = \"HDMI-A-2\" })",
    ] {
        let (response, actions) = answer_payload(wire.as_bytes(), &snapshot);
        assert!(actions.is_empty(), "{wire} must not act");
        assert!(response.starts_with("Invalid dispatcher"), "{wire}: got {response:?}");
    }
    assert!(ask("/dispatch focusmonitor DP-9", &snapshot).contains("DP-9"), "refused by name");
    // Something that has never been a focus target remains distinguishable
    // from a monitor field.
    assert!(ask("/dispatch hl.dsp.focus({ })", &snapshot).starts_with("Invalid dispatcher"));

    let locked = Snapshot { locked: true, ..snapshot };
    for wire in ["/dispatch focusmonitor +1", "/dispatch hl.dsp.focus({ monitor = \"DP-1\" })"] {
        let (response, actions) = answer_payload(wire.as_bytes(), &locked);
        assert!(actions.is_empty(), "{wire} must not act behind the lock");
        assert!(response.contains("locked"), "{wire}: got {response:?}");
    }
}

/// Omarchy's SUPER+SHIFT+ALT+arrows. On the shared desktop the workspace
/// already spans every display, so the request is refused with the
/// setting that would change that — never answered `ok` and left undone.
#[test]
fn moving_the_workspace_to_a_monitor_needs_separate_spaces_and_is_refused_by_name_otherwise() {
    let shared = two_heads();
    let (response, actions) = answer_payload(b"/dispatch movecurrentworkspacetomonitor r", &shared);
    assert!(actions.is_empty(), "the shared desktop must not act");
    assert!(response.starts_with("Invalid dispatcher"), "got {response:?}");
    assert!(response.contains("separate_spaces"), "the refusal names the setting: {response:?}");

    let spaces = Snapshot { separate_spaces: true, ..two_heads() };
    for (wire, target) in [
        ("/dispatch movecurrentworkspacetomonitor r", MonitorTarget::Direction(Direction::Right)),
        ("/dispatch movecurrentworkspacetomonitor -1", MonitorTarget::Relative(-1)),
        ("/dispatch movecurrentworkspacetomonitor DP-1", MonitorTarget::Name("DP-1".into())),
        ("/dispatch hl.dsp.workspace.move({ monitor = \"l\" })", MonitorTarget::Direction(Direction::Left)),
    ] {
        let (response, actions) = answer_payload(wire.as_bytes(), &spaces);
        assert_eq!(response.trim(), "ok", "{wire}");
        assert_eq!(actions, vec![Action::MoveWorkspaceToMonitor(target)], "{wire}");
    }
    let (response, actions) = answer_payload(b"/dispatch movecurrentworkspacetomonitor DP-9", &spaces);
    assert!(actions.is_empty());
    assert!(response.contains("DP-9"), "refused by name: {response:?}");

    // A fullscreen Space is bound to the display of its window.
    let mut fullscreen = spaces.clone();
    fullscreen.workspaces[0].has_fullscreen = true;
    let (response, actions) = answer_payload(b"/dispatch movecurrentworkspacetomonitor r", &fullscreen);
    assert!(actions.is_empty());
    assert!(response.contains("fullscreen"), "got {response:?}");

    let locked = Snapshot { locked: true, ..spaces };
    let (response, actions) = answer_payload(b"/dispatch movecurrentworkspacetomonitor r", &locked);
    assert!(actions.is_empty());
    assert!(response.contains("locked"), "got {response:?}");
}

/// `workspace previous` is the workspace before this one, which only the
/// compositor knows; `e+1` / `e-1` walk the workspaces that have windows
/// on them, plus the current one, and wrap rather than grow the row.
/// The plain `+1` keeps stepping by index.
#[test]
fn workspace_previous_and_existing_steps_resolve_against_the_snapshot() {
    let mut snapshot = desktop();
    let (response, actions) = answer_payload(b"/dispatch workspace previous", &snapshot);
    assert!(actions.is_empty(), "nothing to go back to yet");
    assert!(response.starts_with("Invalid dispatcher"), "got {response:?}");
    snapshot.previous_workspace = Some(2);
    for wire in ["/dispatch workspace previous", "/dispatch hl.dsp.focus({ workspace = \"previous\" })"] {
        let (response, actions) = answer_payload(wire.as_bytes(), &snapshot);
        assert_eq!(response.trim(), "ok", "{wire}");
        assert_eq!(actions, vec![Action::FocusWorkspace(2)], "{wire}");
    }

    // Windows on workspaces 1 and 3 (indices 0 and 2), on 1: e+1 goes
    // to 3, and from 3 wraps back to 1, never to the empty 2 and never
    // to a new 4.
    snapshot.workspaces[2].windows = 1;
    let step = |wire: &str, snapshot: &Snapshot| {
        let (response, actions) = answer_payload(wire.as_bytes(), snapshot);
        assert_eq!(response.trim(), "ok", "{wire}");
        actions
    };
    assert_eq!(step("/dispatch workspace e+1", &snapshot), vec![Action::FocusWorkspace(2)]);
    assert_eq!(step("/dispatch workspace e-1", &snapshot), vec![Action::FocusWorkspace(2)]);
    assert_eq!(step("/dispatch workspace +1", &snapshot), vec![Action::FocusWorkspace(1)], "by index, as before");
    snapshot.monitors[0].active_workspace = 2;
    assert_eq!(step("/dispatch workspace e+1", &snapshot), vec![Action::FocusWorkspace(0)], "wraps");
    assert_eq!(step("/dispatch hl.dsp.focus({ workspace = \"e-1\" })", &snapshot), vec![Action::FocusWorkspace(0)]);
    assert_eq!(step("/dispatch workspace +1", &snapshot), vec![Action::FocusWorkspace(3)], "past the end, growing");
    // An empty current workspace is a stop on the way round.
    snapshot.monitors[0].active_workspace = 1;
    assert_eq!(step("/dispatch workspace e+1", &snapshot), vec![Action::FocusWorkspace(2)]);
    assert_eq!(step("/dispatch workspace e-1", &snapshot), vec![Action::FocusWorkspace(0)]);
    // Nothing else occupied: the answer is where the desktop already is.
    snapshot.workspaces[0].windows = 0;
    snapshot.workspaces[2].windows = 0;
    assert_eq!(step("/dispatch workspace e+1", &snapshot), vec![Action::FocusWorkspace(1)]);

    // Under separate Spaces only the focused display's row counts.
    let mut spaces = Snapshot { separate_spaces: true, ..two_heads() };
    spaces.workspaces[2].windows = 1;
    spaces.workspaces[2].monitor = "DP-1".into();
    spaces.workspaces[2].monitor_id = 1;
    assert_eq!(step("/dispatch workspace e+1", &spaces), vec![Action::FocusWorkspace(0)], "the other display's row is not walked");
    let (response, actions) = answer_payload(b"/dispatch workspace e+x", &spaces);
    assert!(actions.is_empty());
    assert!(response.starts_with("Invalid dispatcher"), "got {response:?}");
}

#[test]
fn nothing_focused_is_reported_as_nothing_not_papered_over() {
    let mut snapshot = desktop();
    snapshot.focused = None;

    assert_eq!(ask_json("j/activewindow", &snapshot), serde_json::json!({}));

    // The workspace's `lastwindow` must not name an arbitrary window
    // either — Hyprland's "none" is the null address.
    let workspaces = ask_json("j/workspaces", &snapshot);
    assert_eq!(workspaces[0]["lastwindow"], serde_json::json!("0x0"));
    assert_eq!(workspaces[0]["lastwindowtitle"], serde_json::json!(""));
}

/// A dispatch naming Hyprland workspace 3 must reach chonkstep index 2,
/// and workspace 0 — which Hyprland does not have — must be refused
/// rather than quietly aimed at chonkstep's first workspace.
#[test]
fn workspace_dispatch_round_trips_the_offset() {
    let snapshot = desktop();
    let (_, actions) = answer_payload(b"/dispatch workspace 3", &snapshot);
    assert_eq!(actions, vec![Action::FocusWorkspace(2)]);

    let (response, actions) = answer_payload(b"/dispatch workspace 0", &snapshot);
    assert!(actions.is_empty(), "workspace 0 must not act");
    assert!(response.starts_with("Invalid dispatcher"), "got {response:?}");
}

#[test]
fn directional_focus_dispatches_are_actions_not_tiling_refusals() {
    for (argument, direction) in [
        ("l", chonk_hyprland_ipc::dispatch::Direction::Left),
        ("right", chonk_hyprland_ipc::dispatch::Direction::Right),
        ("u", chonk_hyprland_ipc::dispatch::Direction::Up),
        ("down", chonk_hyprland_ipc::dispatch::Direction::Down),
    ] {
        let (response, actions) = answer_payload(format!("/dispatch movefocus {argument}").as_bytes(), &desktop());
        assert_eq!(response.trim(), "ok");
        assert_eq!(actions, vec![Action::FocusDirection(direction)]);
    }
    let (response, actions) = answer_payload(b"/dispatch movefocus sideways", &desktop());
    assert!(actions.is_empty());
    assert!(response.starts_with("Invalid dispatcher"), "got {response:?}");
}

/// The regression test for a bug an end-to-end run against the real
/// `hyprctl` found: `dispatch workspace 3` answered `ok` and then did
/// nothing, because feasibility was decided when the action was applied
/// rather than when the answer was written. That is exactly the
/// confident wrong answer this crate exists to prevent, produced by
/// this crate.
///
/// The fix is not "refuse workspace 3": `WindowManager::switch_workspace`
/// grows the workspace row on demand, so workspace 3 is reachable and
/// `ok` is the truthful answer. The fix is that the range check and the
/// action now agree, and both live at the dispatch boundary. So the
/// property to pin is *agreement*: whenever the answer is `ok` there is
/// an action, and whenever there is no action the answer is not `ok`.
#[test]
fn an_ok_answer_always_comes_with_an_action() {
    let snapshot = desktop();
    for verb in [
        "workspace 1",
        "workspace 3",
        "workspace 99",
        "workspace 100",
        "workspace 0",
        "workspace -1",
        "killactive",
        "movetoworkspace 4",
        "focuswindow class:^(foot)$",
        "focuswindow class:^(absent)$",
        "workspace previous",
        "workspace e+1",
        "workspace e-1",
        "focusmonitor +1",
        "focusmonitor eDP-1",
        "focusmonitor DP-9",
        "movecurrentworkspacetomonitor r",
    ] {
        let (response, actions) = answer_payload(format!("/dispatch {verb}").as_bytes(), &snapshot);
        let claimed = response.trim() == "ok";
        assert_eq!(
            claimed,
            !actions.is_empty(),
            "{verb:?} answered {response:?} but produced {actions:?} — \
             an ok with no action is a lie, and an action with an error is worse"
        );
    }
}

/// Growing the row is legal, so a workspace past the end is `ok` — and
/// the number the compositor is asked for is the 0-based one.
#[test]
fn a_workspace_past_the_end_is_reachable_because_switching_grows_the_row() {
    let mut snapshot = desktop();
    snapshot.workspaces.truncate(1);
    let (response, actions) = answer_payload(b"/dispatch workspace 3", &snapshot);
    assert_eq!(response.trim(), "ok");
    assert_eq!(actions, vec![Action::FocusWorkspace(2)]);
}

/// Out of range is a clean error, never a clamp: a clamp would switch
/// to some *other* workspace than the one asked for, which is a wrong
/// answer wearing a success.
#[test]
fn out_of_range_errors_rather_than_clamping() {
    let (response, actions) = answer_payload(b"/dispatch workspace 500", &desktop());
    assert!(actions.is_empty(), "must not silently clamp to the last workspace");
    assert!(response.starts_with("Invalid dispatcher"), "got {response:?}");
}

// ---------------------------------------------------------------------
// The JSON shapes Omarchy's jq filters are written against.
// ---------------------------------------------------------------------

/// `omarchy-capture-region` formats every window as
/// `"\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"`. If `at` and `size`
/// are objects rather than two-element arrays, that yields the string
/// `"null,null nullxnull"` — a wrong answer with no error anywhere.
#[test]
fn client_geometry_is_arrays_not_objects() {
    let value = ask_json("j/clients", &desktop());
    let client = &value[0];
    assert_eq!(client["at"], serde_json::json!([10, 20]));
    assert_eq!(client["size"], serde_json::json!([800, 600]));
}

/// Quickshell parses `address` with base-16 `toULongLong` and **skips
/// any entry whose address does not parse**, so a decimal id would make
/// every window invisible to the bar.
#[test]
fn client_address_is_hex_and_parses() {
    let value = ask_json("j/clients", &desktop());
    let address = value[0]["address"].as_str().unwrap();
    assert!(address.starts_with("0x"), "got {address}");
    let parsed = u64::from_str_radix(address.trim_start_matches("0x"), 16).expect("valid hex");
    assert_eq!(parsed, 4_294_967_297, "id must survive the round trip");
}

/// The exact keys Quickshell's `HyprlandMonitor::updateFromObject`
/// reads, plus the ones Omarchy's scripts filter on. `focused` is the
/// only way the focused monitor is identified, and `.focused` is the
/// single most-used jq field across the whole inventory.
#[test]
fn monitor_json_carries_every_consumed_key() {
    let value = ask_json("j/monitors", &desktop());
    let m = &value[0];
    for key in [
        "id",
        "name",
        "description",
        "x",
        "y",
        "width",
        "height",
        "scale",
        "focused",
        "activeWorkspace",
        "make",
        "model",
        "disabled",
        "dpmsStatus",
    ] {
        assert!(!m[key].is_null(), "monitors[0].{key} must be present");
    }
    assert_eq!(m["focused"], serde_json::json!(true));
    assert_eq!(m["activeWorkspace"]["id"], serde_json::json!(1));
    assert_eq!(m["activeWorkspace"]["name"], serde_json::json!("1"));
}

/// Quickshell's `HyprlandWorkspace::updateFromObject` reads exactly
/// these, and `hasfullscreen` is spelled all-lowercase in Hyprland.
/// A "corrected" `hasFullscreen` reads back as `null`.
#[test]
fn workspace_json_uses_hyprlands_spellings() {
    let value = ask_json("j/workspaces", &desktop());
    let w = &value[0];
    assert!(w.get("hasfullscreen").is_some(), "must be lowercase 'hasfullscreen'");
    assert!(w.get("monitorID").is_some(), "must be 'monitorID' with a capital ID");
    assert_eq!(w["monitor"], serde_json::json!("eDP-1"));
    assert_eq!(w["windows"], serde_json::json!(1));
}

/// `omarchy-cmd-terminal-cwd` pipes `activewindow` straight into `jq`.
/// Hyprland answers an empty object when nothing is focused; `null`
/// would be a different shape and `[]` a different one again.
#[test]
fn activewindow_is_an_object_even_when_nothing_is_focused() {
    let mut snapshot = desktop();
    snapshot.focused = None;
    let value = ask_json("j/activewindow", &snapshot);
    assert!(value.is_object(), "must be an object, got {value}");
    assert_eq!(value, serde_json::json!({}));

    let value = ask_json("j/activewindow", &desktop());
    assert_eq!(value["class"], serde_json::json!("foot"));
    assert_eq!(value["title"], serde_json::json!("~ — foot"));
}

/// Quickshell requests `j/status` FIRST and does not connect to the
/// event socket until it answers. Getting this wrong does not degrade
/// the bar, it disconnects it.
#[test]
fn status_answers_and_does_not_claim_lua() {
    let value = ask_json("j/status", &desktop());
    assert!(value.get("configProvider").is_some());
    assert_ne!(
        value["configProvider"],
        serde_json::json!("lua"),
        "claiming Lua config would make the bar send only Lua dispatch"
    );
}

/// `omarchy-capture-region` reads `hyprctl cursorpos` as
/// `${pos%,*}` / `${pos#*, }` — splitting on a comma AND a space.
#[test]
fn cursorpos_is_plain_text_with_comma_space() {
    let response = ask("/cursorpos", &desktop());
    assert_eq!(response, "321, 654");
    assert!(response.contains(", "), "got {response:?}");
    let (x, y) = response.split_once(", ").expect("comma-space separated");
    assert!(x.parse::<i32>().is_ok() && y.parse::<i32>().is_ok(), "got {response:?}");
}

#[test]
fn cursorpos_json_preserves_logical_coordinates_and_handles_an_unseen_pointer() {
    let mut snapshot = desktop();
    snapshot.cursor_position = Some((-123, 799));
    assert_eq!(ask_json("j/cursorpos", &snapshot), serde_json::json!({"x": -123, "y": 799}));
    assert_eq!(ask("/cursorpos", &snapshot), "-123, 799");
    snapshot.cursor_position = None;
    assert_eq!(ask_json("j/cursorpos", &snapshot), serde_json::json!({"x": 0, "y": 0}));
}

#[test]
fn live_diagnostic_commands_have_truthful_wire_shapes() {
    assert_eq!(ask("/systeminfo", &desktop()), "test system");

    let (response, actions) = answer_payload(b"/debug-set damage-log on", &desktop());
    assert_eq!(response, "ok");
    assert_eq!(
        actions,
        vec![Action::SetDiagnostic { name: "damage-log".into(), enabled: true }]
    );

    let (response, actions) = answer_payload(b"/log-filter info,wm_wayland::session=debug", &desktop());
    assert_eq!(response, "ok");
    assert_eq!(actions, vec![Action::SetLogFilter("info,wm_wayland::session=debug".into())]);

    let (response, actions) = answer_payload(b"/debug-set damage-log perhaps", &desktop());
    assert!(response.starts_with("Invalid dispatcher"));
    assert!(actions.is_empty());
}

// ---------------------------------------------------------------------
// The honest-failure rule.
// ---------------------------------------------------------------------

/// The rule this crate is built around. Each of these is a real
/// Hyprland dispatcher that means nothing in a floating window manager,
/// and each must produce an error a caller can branch on rather than
/// an `ok` it will believe.
#[test]
fn unsupported_group_and_special_workspace_dispatchers_fail_cleanly() {
    let snapshot = desktop();
    for verb in [
        "togglegroup",
        "togglespecialworkspace magic",
        "workspaceopt allfloat",
    ] {
        let (response, actions) = answer_payload(format!("/dispatch {verb}").as_bytes(), &snapshot);
        assert!(actions.is_empty(), "{verb} must not act, got {actions:?}");
        assert!(response.starts_with("Invalid dispatcher"), "{verb} must fail like Hyprland does, got {response:?}");
        assert_ne!(response.trim(), "ok", "{verb} must never claim success");
    }
}

/// A refusal should say what chonkstep *is*, not merely that something
/// went wrong — that is what tells a script author their fallback is
/// the right path.
#[test]
fn refusals_explain_themselves() {
    let (response, _) = answer_payload(b"/dispatch togglegroup", &desktop());
    assert!(response.contains("groups"), "got {response:?}");
}

/// Omarchy sends the Lua form first and the classic form on failure.
/// Both must work, because the bar's workspace buttons send only Lua.
#[test]
fn both_dispatch_dialects_reach_the_same_action() {
    let snapshot = desktop();

    // What `plugins/bar/widgets/Workspaces.qml` sends on a click.
    let (_, lua) = answer_payload(br#"dispatch hl.dsp.focus({ workspace = "3" })"#, &snapshot);
    assert_eq!(lua, vec![Action::FocusWorkspace(2)]);

    // What `omarchy-launch-or-focus` falls back to.
    let (_, classic) = answer_payload(b"/dispatch workspace 3", &snapshot);
    assert_eq!(classic, lua);
}

/// `omarchy-launch-or-focus`'s window form, both dialects.
#[test]
fn window_selectors_resolve() {
    let snapshot = desktop();
    let address = format!("0x{:x}", 4_294_967_297_u64);

    let (_, actions) = answer_payload(format!("/dispatch focuswindow address:{address}").as_bytes(), &snapshot);
    assert_eq!(actions, vec![Action::FocusWindow(4_294_967_297)]);

    let (_, actions) =
        answer_payload(format!(r#"dispatch hl.dsp.focus({{ window = "address:{address}" }})"#).as_bytes(), &snapshot);
    assert_eq!(actions, vec![Action::FocusWindow(4_294_967_297)]);

    // Hyprland's anchored-regex class selector, as Omarchy writes it.
    let (_, actions) = answer_payload(b"/dispatch focuswindow class:^(foot)$", &snapshot);
    assert_eq!(actions, vec![Action::FocusWindow(4_294_967_297)]);
}

/// A selector that matches nothing must fail, not fall back to the
/// focused window — silently focusing the wrong window is how
/// `omarchy-launch-or-focus` would raise a second copy of an app.
#[test]
fn an_unmatched_selector_fails_rather_than_guessing() {
    let (response, actions) = answer_payload(b"/dispatch focuswindow class:^(nothing-like-this)$", &desktop());
    assert!(actions.is_empty());
    assert!(response.starts_with("Invalid dispatcher"), "got {response:?}");
}

#[test]
fn classic_exec_preserves_direct_argv_and_shell_c_source() {
    let (_, actions) = answer_payload(b"/dispatch exec -- bash -lc 'echo hi'", &desktop());
    assert_eq!(actions, vec![Action::ExecShell("bash -lc 'echo hi'".to_string())]);

    let (_, actions) = answer_payload(b"/dispatch exec /usr/bin/touch /tmp/chonkstep-exec", &desktop());
    assert_eq!(actions, vec![Action::ExecArgv(vec!["/usr/bin/touch".into(), "/tmp/chonkstep-exec".into()])]);

    // This is the exact byte shape hyprctl sends after receiving
    // `dispatch exec -- bash -lc 'touch /tmp/x'`: it consumes both the
    // marker and the shell's grouping before joining its own argv.
    let (_, actions) = answer_payload(b"/dispatch exec bash -lc touch /tmp/chonkstep-shell-c", &desktop());
    assert_eq!(
        actions,
        vec![Action::ExecArgv(vec!["bash".into(), "-lc".into(), "touch /tmp/chonkstep-shell-c".into()])]
    );
}

#[test]
fn lua_long_bracket_exec_is_the_same_command_as_a_quoted_string() {
    let (_, quoted) = answer_payload(b"/dispatch hl.dsp.exec_cmd(\"touch /tmp/x\")", &desktop());
    let (_, long) = answer_payload(b"/dispatch hl.dsp.exec_cmd([[touch /tmp/x]])", &desktop());
    assert_eq!(quoted, vec![Action::ExecShell("touch /tmp/x".into())]);
    assert_eq!(long, quoted);
}

/// `hl.dsp.exec_cmd` takes a Lua string literal, and a caller that
/// builds one with a JSON encoder escapes `"` and `\`. Omarchy's
/// keybinding menu quotes with `jq @json` and falls back to classic
/// `exec` only when the answer is not `ok`. Reading up to the next quote
/// ran a truncated command and answered `ok`, so the fallback never ran.
#[test]
fn lua_exec_decodes_its_string_literal_rather_than_cutting_it_at_a_quote() {
    let exec = |literal: &str| answer_payload(format!("/dispatch hl.dsp.exec_cmd({literal})").as_bytes(), &desktop()).1;
    let shell = |command: &str| vec![Action::ExecShell(command.to_string())];

    assert_eq!(exec(r#""notify-send \"build finished\"""#), shell(r#"notify-send "build finished""#));
    assert_eq!(exec("'omarchy-launch-shell'"), shell("omarchy-launch-shell"));
    // A key name inside the string is text, not a table field.
    assert_eq!(exec(r#""env cmd=true touch /tmp/x""#), shell("env cmd=true touch /tmp/x"));
    assert_eq!(exec(r#""grep -E \"a\\.b\" f""#), shell(r#"grep -E "a\.b" f"#));
    assert_eq!(exec(r#""printf 'a\\nb'""#), shell(r"printf 'a\nb'"));
    assert_eq!(exec(r#""a\\.b""#), shell(r"a\.b"));
    assert_eq!(exec(r#""\u{48}i""#), shell("Hi"));
    // Lua strings are bytes: each escape names one byte of a UTF-8 character.
    assert_eq!(exec(r#""\228\189\160""#), shell("你"));
    assert_eq!(exec(r#""\xe4\xbd\xa0""#), shell("你"));
    assert_eq!(exec("[==[x]]y]==]"), shell("x]]y"));
}

/// A literal Lua would reject is refused, never run as the nearest
/// plausible command.
#[test]
fn an_invalid_lua_string_literal_is_refused_not_guessed_at() {
    for literal in [r#""\q""#, r#""unterminated"#, r#""\xff""#, "'line\nbreak'", "[[never closed", r#""a" .. "b""#] {
        let (response, actions) = answer_payload(format!("/dispatch hl.dsp.exec_cmd({literal})").as_bytes(), &desktop());
        assert!(actions.is_empty(), "{literal:?} produced {actions:?}");
        assert!(response.starts_with("Invalid dispatcher"), "{literal:?} answered {response:?}");
    }
}

/// Field lookup reads the table, not the text: `workspace = 3` inside a
/// window selector's string is part of the title being matched.
#[test]
fn a_key_name_inside_a_lua_string_value_is_not_a_field() {
    let mut snapshot = desktop();
    snapshot.windows[0].title = "notes: workspace = 3".into();
    let (response, actions) =
        answer_payload(br#"dispatch hl.dsp.focus({ window = "title:workspace = 3" })"#, &snapshot);
    assert_eq!(actions, vec![Action::FocusWindow(4_294_967_297)], "got {response:?}");
}

// ---------------------------------------------------------------------
// Omarchy's scripts, whole: the proof behind wm-config's allow-list.
// ---------------------------------------------------------------------

/// One request an Omarchy script sends, as `hyprctl` writes it.
enum ScriptRequest {
    /// A `-j` query, with the `jq` paths the script reads from each
    /// object in the answer.
    Query(&'static str, &'static [&'static str]),
    /// A dispatch or eval, which must be applied rather than refused.
    Mutation(String),
}

/// The requests of each script on `SERVED_OMARCHY_SCRIPTS`, transcribed
/// from Omarchy 4.0.2's `bin/`, with this desk's window, output and
/// workspace substituted where the script interpolates one. Only the
/// first half of each `lua || classic` pair is here: `hyprctl` exits
/// zero whatever the reply, so the fallback never runs, which is exactly
/// why the first half has to be served.
fn served_script_requests(snapshot: &Snapshot) -> Vec<(&'static str, Vec<ScriptRequest>)> {
    use ScriptRequest::{Mutation, Query};
    let window = format!("address:{}", snapshot.windows[0].address());
    let output = &snapshot.monitors[0].name;
    let dispatch = |lua: &str| Mutation(format!("/dispatch {lua}"));
    vec![
        (
            "omarchy-hyprland-window-pop",
            vec![
                Query("j/activewindow", &["pinned", "address"]),
                // Already popped: return it to the layout.
                dispatch(&format!(r#"hl.dsp.window.pin({{ window = "{window}" }})"#)),
                dispatch(&format!(r#"hl.dsp.window.float({{ window = "{window}", action = "toggle" }})"#)),
                dispatch(&format!(r#"hl.dsp.window.tag({{ window = "{window}", tag = "-pop" }})"#)),
                // Popping out, at the default size or one given as arguments.
                dispatch(&format!(r#"hl.dsp.window.resize({{ window = "{window}", x = 1300, y = 900 }})"#)),
                dispatch(&format!(r#"hl.dsp.window.move({{ window = "{window}", x = 40, y = 30 }})"#)),
                dispatch(&format!(r#"hl.dsp.window.center({{ window = "{window}" }})"#)),
                dispatch(&format!(r#"hl.dsp.window.alter_zorder({{ window = "{window}", mode = "top" }})"#)),
                dispatch(&format!(r#"hl.dsp.window.tag({{ window = "{window}", tag = "+pop" }})"#)),
            ],
        ),
        (
            "omarchy-hyprland-window-width",
            vec![
                Query("j/activewindow", &["class", "initialClass", "title", "workspace.id", "size.0", "address"]),
                Query("j/clients", &["address", "size.0"]),
                // The direction probe, then the correction.
                dispatch(&format!(r#"hl.dsp.window.resize({{ window = "{window}", x = 10, y = 0, relative = true }})"#)),
                dispatch(&format!(r#"hl.dsp.window.resize({{ window = "{window}", x = -10, y = 0, relative = true }})"#)),
            ],
        ),
        (
            "omarchy-hyprland-window-close-all",
            vec![
                Query("j/clients", &["address"]),
                dispatch(&format!(r#"hl.dsp.window.close({{ window = "{window}" }})"#)),
                dispatch(r#"hl.dsp.focus({ workspace = "1" })"#),
            ],
        ),
        (
            "omarchy-hyprland-monitor-scaling",
            vec![
                Query("j/monitors", &["focused", "name", "scale", "width", "height", "refreshRate"]),
                Mutation(format!(
                    r#"/eval hl.monitor({{ output = "{output}", mode = "2560x1600@120", position = "auto", scale = 1.6 }})"#
                )),
            ],
        ),
        (
            "omarchy-hyprland-workspace-layout-toggle",
            vec![
                Query("j/activeworkspace", &["id", "tiledLayout"]),
                Mutation(r#"/eval hl.workspace_rule({ workspace = "1", layout = "scrolling" })"#.to_string()),
                Mutation(r#"/eval hl.workspace_rule({ workspace = "1", layout = "dwindle" })"#.to_string()),
            ],
        ),
        (
            "omarchy-hyprland-window-tiled-fullscreen-toggle",
            vec![
                Query("j/activewindow", &["fullscreenClient"]),
                // Off when `fullscreenClient` reads 2, on otherwise, each
                // with the classic fallback the script keeps for an older
                // Hyprland.
                dispatch("hl.dsp.window.fullscreen_state({ internal = 0, client = 0 })"),
                dispatch("hl.dsp.window.fullscreen_state({ internal = 0, client = 2 })"),
                dispatch("fullscreenstate 0 0"),
                dispatch("fullscreenstate 0 2"),
            ],
        ),
    ]
}

/// A `jq`-style path into a JSON value; a missing step is `null`.
fn json_path<'a>(value: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    path.split('.').fold(value, |value, step| match step.parse::<usize>() {
        Ok(index) => &value[index],
        Err(_) => &value[step],
    })
}

/// `wm-config` binds an `omarchy-hyprland-*` script only when it is on
/// `SERVED_OMARCHY_SCRIPTS`, and this is what makes the list true: every
/// request each listed script sends is applied, and every field it reads
/// is present. A script on the list with no fixture fails, and so does a
/// fixture for a script that is not listed, so the list and the proof
/// cannot drift apart. A future Omarchy that adds a request to one of
/// these scripts has to be added here, where a refusal fails the test
/// instead of shipping a chord that quietly does nothing.
#[test]
fn omarchy_scripts_send_only_served_requests() {
    let snapshot = desktop();
    let fixtures = served_script_requests(&snapshot);
    for script in SERVED_OMARCHY_SCRIPTS {
        let Some((_, requests)) = fixtures.iter().find(|(name, _)| name == script) else {
            panic!("{script} is allow-listed without a fixture of the requests it sends");
        };
        for request in requests {
            match request {
                ScriptRequest::Query(wire, fields) => {
                    let value = ask_json(wire, &snapshot);
                    let objects: Vec<&serde_json::Value> = match value.as_array() {
                        Some(items) => items.iter().collect(),
                        None => vec![&value],
                    };
                    assert!(!objects.is_empty(), "{script}: {wire} answered nothing to read");
                    for object in objects {
                        for field in *fields {
                            assert!(!json_path(object, field).is_null(), "{script} reads .{field} from {wire}: {object}");
                        }
                    }
                }
                ScriptRequest::Mutation(wire) => {
                    let (response, actions) = answer_payload(wire.as_bytes(), &snapshot);
                    assert_eq!(response, "ok", "{script} sends {wire:?}");
                    assert_eq!(actions.len(), 1, "{script} sends {wire:?}, which must be one applied action");
                }
            }
        }
    }
    for (name, _) in &fixtures {
        assert!(SERVED_OMARCHY_SCRIPTS.contains(name), "{name} has a fixture but is not allow-listed");
    }
}

#[test]
fn lua_geometry_fields_are_not_hidden_by_hex_window_addresses() {
    let (_, actions) = answer_payload(
        br#"eval hl.dispatch(hl.dsp.window.resize({ window = "address:0x100000001", x = 25, y = 10, relative = true }))"#,
        &desktop(),
    );
    assert_eq!(
        actions,
        vec![Action::ResizeWindow { window: 4_294_967_297, width: 25, height: 10, relative: true }]
    );
}

#[test]
fn fullscreen_arguments_map() {
    let snapshot = desktop();
    let (_, actions) = answer_payload(b"/dispatch fullscreen", &snapshot);
    assert_eq!(actions, vec![Action::Fullscreen(Fullscreen::Toggle)]);
    let (_, actions) = answer_payload(b"/dispatch fullscreen 1", &snapshot);
    assert_eq!(actions, vec![Action::ToggleMaximize]);
    let (_, actions) = answer_payload(b"/dispatch fullscreen 2", &snapshot);
    assert_eq!(actions, vec![Action::Fullscreen(Fullscreen::On)]);
}

/// `fullscreenstate` carries two axes, and both reach the action in
/// either spelling; the Lua form also takes a window selector.
#[test]
fn fullscreen_state_parses_both_axes_in_both_spellings() {
    let snapshot = desktop();
    let focused = snapshot.windows[0].id;
    for (wire, internal, client) in [
        ("/dispatch fullscreenstate 0 2", 0, 2),
        ("/dispatch fullscreenstate 0 0", 0, 0),
        ("/dispatch fullscreenstate 2 1", 2, 1),
        ("/dispatch fullscreenstate  1   2 ", 1, 2),
    ] {
        let (response, actions) = answer_payload(wire.as_bytes(), &snapshot);
        assert_eq!(response, "ok", "{wire}");
        assert_eq!(actions, vec![Action::FullscreenState { window: None, internal, client }], "{wire}");
    }
    for (wire, internal, client) in [
        ("/dispatch hl.dsp.window.fullscreen_state({ internal = 0, client = 2 })", 0, 2),
        ("/dispatch hl.dsp.window.fullscreen_state({ internal = 0, client = 0 })", 0, 0),
        ("/dispatch hl.dsp.window.fullscreen_state({ client = 1, internal = 2 })", 2, 1),
        (
            "/dispatch hl.dsp.window.fullscreen_state({ window = \"address:0x100000001\", internal = 1, client = 1 })",
            1,
            1,
        ),
    ] {
        let (response, actions) = answer_payload(wire.as_bytes(), &snapshot);
        assert_eq!(response, "ok", "{wire}");
        assert_eq!(actions, vec![Action::FullscreenState { window: Some(focused), internal, client }], "{wire}");
    }
}

/// A mode outside 0..=2, a missing axis, or a non-number is refused by
/// the name of the field, never rounded to a state the window did not
/// ask for. `hyprctl` exits zero whatever the reply, so the reply text
/// is the only thing that tells a script author what was wrong.
#[test]
fn fullscreen_state_refuses_out_of_range_and_missing_axes_by_name() {
    let snapshot = desktop();
    for (wire, field, value) in [
        ("fullscreenstate 3 2", "internal", "\"3\""),
        ("fullscreenstate 0 -1", "client", "\"-1\""),
        ("fullscreenstate 0 on", "client", "\"on\""),
        ("hl.dsp.window.fullscreen_state({ internal = 0, client = 7 })", "client", "\"7\""),
        ("hl.dsp.window.fullscreen_state({ internal = 256, client = 2 })", "internal", "\"256\""),
    ] {
        let outcome = dispatch::parse(wire, &snapshot);
        let Outcome::Unsupported(why) = outcome else {
            panic!("{wire:?} must be refused, got {outcome:?}");
        };
        assert!(why.contains(field) && why.contains(value), "{wire:?} names the bad field and value: {why}");
        let (response, actions) = answer_payload(format!("/dispatch {wire}").as_bytes(), &snapshot);
        assert_ne!(response, "ok", "{wire}");
        assert!(actions.is_empty(), "{wire}: nothing applied");
    }
    for wire in [
        "fullscreenstate",
        "fullscreenstate 0",
        "hl.dsp.window.fullscreen_state({ client = 2 })",
        "hl.dsp.window.fullscreen_state()",
    ] {
        let outcome = dispatch::parse(wire, &snapshot);
        let Outcome::Unsupported(why) = outcome else {
            panic!("{wire:?} must be refused, got {outcome:?}");
        };
        assert!(why.contains("both internal and client"), "{wire:?}: {why}");
    }
}

/// `fullscreen` is the compositor's mode and `fullscreenClient` what the
/// window is told, in Hyprland's numbering: a maximized window reads 1
/// on both, one told it is fullscreen in its tile reads 0 and 2, and a
/// real fullscreen reads 2 and 2. `hasfullscreen` follows only the last.
#[test]
fn clients_report_fullscreen_and_fullscreen_client_modes_honestly() {
    let mut snapshot = desktop();
    let read = |snapshot: &Snapshot| {
        let active = ask_json("j/activewindow", snapshot);
        let clients = ask_json("j/clients", snapshot);
        let client = &clients.as_array().unwrap()[0];
        assert_eq!(client["fullscreen"], active["fullscreen"]);
        assert_eq!(client["fullscreenClient"], active["fullscreenClient"]);
        (active["fullscreen"].as_i64().unwrap(), active["fullscreenClient"].as_i64().unwrap())
    };
    assert_eq!(read(&snapshot), (0, 0));

    snapshot.windows[0].maximized = true;
    assert_eq!(read(&snapshot), (1, 1), "SUPER+ALT+F's maximize is Hyprland's mode 1");
    snapshot.windows[0].maximized = false;

    snapshot.windows[0].client_fullscreen = true;
    assert_eq!(read(&snapshot), (0, 2), "tiled fullscreen is told, not done");
    let plain = ask("activewindow", &snapshot);
    assert!(plain.contains("\tfullscreen: 0\n\tfullscreenClient: 2\n"), "{plain}");
    assert_eq!(ask_json("j/workspaces", &snapshot)[0]["hasfullscreen"], false, "hasfullscreen stays compositor-only");
    snapshot.windows[0].client_fullscreen = false;

    snapshot.windows[0].fullscreen = true;
    assert_eq!(read(&snapshot), (2, 2));
}

#[test]
fn classic_geometry_distinguishes_relative_from_exact_for_every_target_form() {
    let s = desktop();
    for (request, expected_relative) in [
        ("resizeactive 20 -10", true),
        ("resizeactive exact 800 600", false),
        ("resizewindowpixel 20 -10,address:0x100000001", true),
        ("resizewindowpixel exact 800 600,address:0x100000001", false),
        ("moveactive 20 -10", true),
        ("moveactive exact 100 200", false),
        ("movewindowpixel 20 -10,address:0x100000001", true),
        ("movewindowpixel exact 100 200,address:0x100000001", false),
    ] {
        let Outcome::Run(action) = dispatch::parse(request, &s) else {
            panic!("{request:?} was not accepted")
        };
        let relative = match action {
            Action::ResizeWindow { relative, .. } | Action::MoveWindow { relative, .. } => relative,
            other => panic!("wrong action for {request:?}: {other:?}"),
        };
        assert_eq!(relative, expected_relative, "{request}");
    }
}

/// `getoption` feeds `Commons/Style.qml`'s corner radius and gap. A
/// fabricated number would restyle the user's bar to match a compositor
/// they are not running; the documented "unset" shape leaves Style.qml's
/// `catch` to keep its previous value, which is the correct outcome.
#[test]
fn getoption_omits_values_that_javascript_would_coerce_to_zero() {
    let value = ask_json("j/getoption decoration:rounding", &desktop());
    assert!(value.get("int").is_none());
    assert!(value.get("float").is_none());
    assert!(value.get("css").is_none());
    assert_eq!(value["set"], serde_json::json!(false));
    assert_eq!(
        value.as_object().unwrap().keys().cloned().collect::<std::collections::BTreeSet<_>>(),
        ["option".to_string(), "set".to_string()].into_iter().collect(),
        "an unset reply must contain no value JavaScript can mistake for a configured zero"
    );
}

/// `KeyboardLayout.qml` refuses to speak for the seat unless
/// `parsed.keyboards` is an Array — that check is why it must be `[]`
/// and not a missing key.
#[test]
fn devices_keeps_the_shape_the_keyboard_widget_tests_for() {
    let value = ask_json("j/devices", &desktop());
    assert!(value["keyboards"].is_array(), "keyboards must be an array");
}

#[test]
fn dpms_dispatches_name_real_output_power_actions() {
    for (request, output, powered) in [
        ("/dispatch dpms off", None, false),
        ("/dispatch dpms on eDP-1", Some("eDP-1"), true),
        ("/dispatch dpms toggle eDP-1", Some("eDP-1"), false),
    ] {
        let (response, actions) = answer_payload(request.as_bytes(), &desktop());
        assert_eq!(response.trim(), "ok", "{request}");
        assert_eq!(
            actions,
            vec![Action::SetDpms { output: output.map(str::to_string), powered }],
            "{request}"
        );
    }
    let (response, actions) = answer_payload(b"/dispatch dpms off absent", &desktop());
    assert!(response.contains("unknown output"));
    assert!(actions.is_empty());

    let (response, actions) = answer_payload(
        br#"/dispatch hl.dsp.dpms({ action = "disable", monitor = "eDP-1" })"#,
        &desktop(),
    );
    assert_eq!(response.trim(), "ok");
    assert_eq!(
        actions,
        vec![Action::SetDpms { output: Some("eDP-1".into()), powered: false }]
    );
}

#[test]
fn switchxkblayout_is_a_named_group_action_not_an_unknown_request() {
    let mut desk = desktop();
    desk.devices.keyboards.push(Keyboard {
        name: "at-translated-set-2-keyboard".into(),
        layout: "English (US), German".into(),
        active_keymap: "English (US)".into(),
        active_layout_index: 0,
    });
    for (target, expected) in [
        ("next", LayoutTarget::Next),
        ("prev", LayoutTarget::Previous),
        ("1", LayoutTarget::Index(1)),
    ] {
        let request = format!("/switchxkblayout at-translated-set-2-keyboard {target}");
        let (response, actions) = answer_payload(request.as_bytes(), &desk);
        assert_eq!(response.trim(), "ok", "{request}");
        assert_eq!(
            actions,
            vec![Action::SwitchKeyboardLayout {
                device: "at-translated-set-2-keyboard".into(),
                target: expected,
            }]
        );
    }
}

/// Omarchy's touchpad and touchscreen toggles send
/// `hl.device({ name = "…", enabled = … })`, after escaping backslashes and
/// double quotes in the device's name. The name has to arrive whole, and only
/// a pointer, touch or tablet device with no keys can be switched off.
#[test]
fn hl_device_is_a_named_switch_for_one_pointing_device() {
    use chonk_hyprland_ipc::state::PointerDevice;
    let trackpad = r#"Apple "Magic" \Trackpad"#;
    let mut desk = desktop();
    desk.devices.mice.push(PointerDevice { name: trackpad.into() });
    desk.devices.touch.push(PointerDevice { name: "ELAN9008:00 04F3:2C82".into() });
    desk.devices.keyboards.push(Keyboard {
        name: "Logitech USB Receiver".into(),
        layout: "us".into(),
        active_keymap: "English (US)".into(),
        active_layout_index: 0,
    });
    desk.devices.mice.push(PointerDevice { name: "Logitech USB Receiver".into() });
    // `omarchy-toggle-input-device`'s own quoting of the name.
    let quoted = trackpad.replace('\\', r"\\").replace('"', r#"\""#);
    for (enabled, word) in [(false, "false"), (true, "true")] {
        let wire = format!(r#"/eval hl.device({{ name = "{quoted}", enabled = {word} }})"#);
        let (response, actions) = answer_payload(wire.as_bytes(), &desk);
        assert_eq!(response.trim(), "ok", "{wire}");
        assert_eq!(actions, vec![Action::SetInputDeviceEnabled { name: trackpad.into(), enabled }], "{wire}");
    }
    let (response, actions) = answer_payload(br#"/eval hl.device({ name = "ELAN9008:00 04F3:2C82", enabled = false })"#, &desk);
    assert_eq!((response.trim(), actions.len()), ("ok", 1), "a touchscreen is switched the same way");
    for (wire, reason) in [
        (r#"/eval hl.device({ name = "Apple ", enabled = false })"#, "no pointer, touch or tablet device"),
        (r#"/eval hl.device({ name = "Logitech USB Receiver", enabled = false })"#, "with keys stays on"),
        (r#"/eval hl.device({ name = "chonkstep-pointer", enabled = false })"#, "nested session"),
        (r#"/eval hl.device({ name = "chonkstep-keyboard", enabled = true })"#, "nested session"),
        (r#"/eval hl.device({ name = "ELAN9008:00 04F3:2C82" })"#, "enabled = true or false"),
        (r#"/eval hl.device({ name = ELAN, enabled = false })"#, "quoted string"),
        (
            r#"/eval hl.device({ name = "ELAN9008:00 04F3:2C82", enabled = false, sensitivity = 0.5 })"#,
            "sensitivity belongs in the configuration",
        ),
        (r#"/eval hl.device({ name = "ELAN9008:00 04F3:2C82", enabled = false }"#, "unterminated"),
    ] {
        let (response, actions) = answer_payload(wire.as_bytes(), &desk);
        assert!(response.starts_with("Invalid dispatcher") && response.contains(reason), "{wire}: {response}");
        assert!(!response.contains("unknown eval expression"), "{wire}: {response}");
        assert!(actions.is_empty(), "{wire}");
    }
}

#[test]
fn unknown_requests_answer_the_way_hyprland_does() {
    let response = ask("/nonsense", &desktop());
    assert!(response.starts_with("unknown request"), "got {response:?}");
}

// ---------------------------------------------------------------------
// The event stream.
// ---------------------------------------------------------------------

/// The first diff establishes a baseline and says nothing: a client
/// gets its initial state from the `j/` queries it makes on connect,
/// and replaying the desktop as a burst of `openwindow` would tell it
/// what it already knows.
#[test]
fn the_first_diff_is_silent() {
    let mut differ = Differ::new();
    assert!(differ.diff(&desktop()).is_empty());
}

#[test]
fn workspace_switch_emits_workspacev2_with_the_hyprland_id() {
    let mut differ = Differ::new();
    let before = desktop();
    differ.diff(&before);

    let mut after = before.clone();
    after.monitors[0].active_workspace = 1;
    let events = differ.diff(&after);

    let line = events.iter().find(|e| e.name() == "workspacev2").expect("workspacev2");
    assert_eq!(line.data(), "2,2", "chonkstep index 1 is Hyprland workspace 2");
    assert_eq!(line.line(), "workspacev2>>2,2\n");
}

#[test]
fn adding_a_monitor_emits_legacy_then_v2_events() {
    let mut differ = Differ::new();
    let before = desktop();
    differ.diff(&before);

    let mut after = before;
    after.monitors.push(monitor(1, "HDMI-A-1", false, 0));
    let events = differ.diff(&after);
    let names: Vec<_> = events.iter().map(chonk_hyprland_ipc::Event::name).collect();
    let legacy = names.iter().position(|name| *name == "monitoradded").expect("monitoradded");
    let v2 = names.iter().position(|name| *name == "monitoraddedv2").expect("monitoraddedv2");
    assert!(legacy < v2, "legacy consumers must learn the monitor before richer events reference it");
    assert_eq!(events[legacy].data(), "HDMI-A-1");
}

#[test]
fn keyboard_group_change_emits_activelayout_with_the_human_name() {
    let mut differ = Differ::new();
    let mut before = desktop();
    before.devices.keyboards.push(Keyboard {
        name: "keyboard".into(),
        layout: "English (US), German".into(),
        active_keymap: "English (US)".into(),
        active_layout_index: 0,
    });
    differ.diff(&before);
    let mut after = before;
    after.devices.keyboards[0].active_keymap = "German".into();
    after.devices.keyboards[0].active_layout_index = 1;

    let events = differ.diff(&after);
    let event = events.iter().find(|event| event.name() == "activelayout").expect("activelayout");
    assert_eq!(event.data(), "keyboard,German");
}

/// Quickshell's `openwindow` handler takes four comma-separated fields
/// and looks the workspace up by name; a window announced on a
/// workspace it has not been told about is dropped with a warning.
#[test]
fn opening_a_window_emits_the_four_field_payload() {
    let mut differ = Differ::new();
    let before = desktop();
    differ.diff(&before);

    let mut after = before.clone();
    after.windows.push(window(99, "a title", "Alacritty", 0));
    let events = differ.diff(&after);

    let line = events.iter().find(|e| e.name() == "openwindow").expect("openwindow");
    assert_eq!(line.data(), "63,1,Alacritty,a title");
    // Bare hex, no 0x — Hyprland's event payload form.
    assert!(!line.data().starts_with("0x"));
}

#[test]
fn closing_a_window_emits_closewindow_with_a_bare_hex_address() {
    let mut differ = Differ::new();
    differ.diff(&desktop());

    let mut after = desktop();
    after.windows.clear();
    after.focused = None;
    let events = differ.diff(&after);

    let line = events.iter().find(|e| e.name() == "closewindow").expect("closewindow");
    assert_eq!(line.data(), format!("{:x}", 4_294_967_297_u64));
}

#[test]
fn retitling_emits_windowtitlev2() {
    let mut differ = Differ::new();
    differ.diff(&desktop());

    let mut after = desktop();
    after.windows[0].title = "new title".to_string();
    let events = differ.diff(&after);

    let line = events.iter().find(|e| e.name() == "windowtitlev2").expect("windowtitlev2");
    assert_eq!(line.data(), format!("{:x},new title", 4_294_967_297_u64));
}

#[test]
fn focus_change_emits_activewindowv2() {
    let mut differ = Differ::new();
    let mut before = desktop();
    before.windows.push(window(99, "other", "other", 0));
    differ.diff(&before);

    let mut after = before.clone();
    after.focused = Some(99);
    let events = differ.diff(&after);

    let line = events.iter().find(|e| e.name() == "activewindowv2").expect("activewindowv2");
    assert_eq!(line.data(), "63");
}

/// Creations must precede the events that reference them, or Quickshell
/// drops those events with "was not previously tracked".
#[test]
fn creations_are_emitted_before_the_windows_that_reference_them() {
    let mut differ = Differ::new();
    differ.diff(&desktop());

    let mut after = desktop();
    after.workspaces.push(workspace(3, 1));
    after.windows.push(window(99, "t", "c", 3));
    let events = differ.diff(&after);

    let names: Vec<&str> = events.iter().map(chonk_hyprland_ipc::Event::name).collect();
    let created = names.iter().position(|n| *n == "createworkspacev2").expect("createworkspacev2");
    let opened = names.iter().position(|n| *n == "openwindow").expect("openwindow");
    assert!(created < opened, "workspace must be announced first, got {names:?}");
}

/// Removals must come last, for the mirror-image reason.
#[test]
fn removals_are_emitted_last() {
    let mut differ = Differ::new();
    let mut before = desktop();
    before.workspaces.push(workspace(3, 1));
    before.windows.push(window(99, "t", "c", 3));
    differ.diff(&before);

    let events = differ.diff(&desktop());
    let names: Vec<&str> = events.iter().map(chonk_hyprland_ipc::Event::name).collect();
    let closed = names.iter().position(|n| *n == "closewindow").expect("closewindow");
    let destroyed = names.iter().position(|n| *n == "destroyworkspacev2").expect("destroyworkspacev2");
    assert!(closed < destroyed, "window before its workspace, got {names:?}");
}

/// A window title is attacker-controlled — a web page picks its own
/// `<title>`. A newline in one would split the frame and desynchronise
/// every reader on the event socket.
#[test]
fn a_newline_in_a_title_cannot_split_a_frame() {
    let mut differ = Differ::new();
    differ.diff(&desktop());

    let mut after = desktop();
    after.windows[0].title = "evil\nclosewindow>>deadbeef\nmore".to_string();
    let events = differ.diff(&after);

    let line = events.iter().find(|e| e.name() == "windowtitlev2").expect("windowtitlev2");
    assert_eq!(line.line().matches('\n').count(), 1, "exactly one newline per frame");
    assert!(!line.data().contains('\n'));
}

/// The `focusedmon` payload's second field is a literal `?` when the
/// monitor has no workspace — Quickshell special-cases that exact
/// string, and anything else sends it looking up a workspace named "".
#[test]
fn focusedmon_uses_hyprlands_question_mark_sentinel() {
    let mut differ = Differ::new();
    let mut before = desktop();
    before.monitors.push(monitor(1, "HDMI-1", false, 0));
    differ.diff(&before);

    let mut after = before.clone();
    after.monitors[0].focused = false;
    after.monitors[1].focused = true;
    after.monitors[1].active_workspace = 77; // no such workspace
    let events = differ.diff(&after);

    let line = events.iter().find(|e| e.name() == "focusedmon").expect("focusedmon");
    assert_eq!(line.data(), "HDMI-1,?");
}

// ---------------------------------------------------------------------
// Hostile input.
// ---------------------------------------------------------------------

#[test]
fn hostile_payloads_do_not_panic() {
    let snapshot = desktop();
    for payload in [
        &b""[..],
        b"\0\0\0\0",
        b"j/",
        b"/",
        b"\xff\xfe\xfd",
        b"[[BATCH]]",
        b"[[BATCH]];;;;",
        b"dispatch",
        b"/dispatch ",
        b"/dispatch focuswindow address:zzz",
        b"/dispatch focuswindow address:0xffffffffffffffffffff",
        b"/dispatch workspace -99999999999999999999",
        b"/dispatch hl.dsp.focus({",
        b"/dispatch hl.dsp.focus({ workspace = ",
        b"j/clients extra args that mean nothing",
    ] {
        // The property under test is that none of these panics or
        // hangs. An empty answer is a legitimate outcome for some of
        // them — an empty batch has nothing to answer — so the only
        // assertion that would be honest here is that we got back a
        // value at all.
        let (response, actions) = answer_payload(payload, &snapshot);
        // A malformed request must never be interpreted as an action.
        assert!(actions.is_empty(), "payload {payload:?} produced {actions:?}");
        drop(response);
    }
}

/// Every Unicode whitespace character, in each place the parsers split
/// a word from what follows it: after the command, after a dispatcher
/// verb, and inside a `[[BATCH]]` segment. All but the ASCII ones are
/// several bytes long, and a split that stepped one byte past the start
/// of the match panicked on them, ending the session through the panic
/// hook. Each must separate the words exactly as a plain space does.
#[test]
fn every_unicode_whitespace_separates_words_without_panicking() {
    let snapshot = desktop();
    let spaces = (0..=u32::from(char::MAX)).filter_map(char::from_u32).filter(|c| c.is_whitespace());
    let mut seen = 0;
    for space in spaces {
        seen += 1;
        let (response, actions) = answer_payload(format!("dispatch{space}workspace 2").as_bytes(), &snapshot);
        assert_eq!(actions, vec![Action::FocusWorkspace(1)], "after the command, {space:?}: {response:?}");

        let (response, actions) = answer_payload(format!("/dispatch exec{space}foot").as_bytes(), &snapshot);
        assert_eq!(actions, vec![Action::ExecArgv(vec!["foot".into()])], "after the verb, {space:?}: {response:?}");

        let batch = format!("[[BATCH]]/dispatch{space}workspace 2;dispatch exec{space}foot");
        let (response, actions) = answer_payload(batch.as_bytes(), &snapshot);
        assert_eq!(
            actions,
            vec![Action::FocusWorkspace(1), Action::ExecArgv(vec!["foot".into()])],
            "inside a batch, {space:?}: {response:?}"
        );
    }
    // Unicode's White_Space property, which `char::is_whitespace`
    // follows, has 25 members; fewer would mean the sweep tested less
    // than it claims.
    assert_eq!(seen, 25);
}

/// A batch answers each segment in order and collects every action.
#[test]
fn batches_answer_each_segment() {
    let (response, actions) = answer_payload(b"[[BATCH]]/dispatch workspace 2;j/status", &desktop());
    assert_eq!(actions, vec![Action::FocusWorkspace(1)]);
    assert!(response.starts_with("ok"), "got {response:?}");
    assert!(response.contains("configProvider"));
}

/// `Request::parse` and the answer table must agree that a request with
/// no `/` is Quickshell's dispatch form, not an unknown command.
#[test]
fn quickshell_dispatch_form_is_not_an_unknown_request() {
    let request = Request::parse(b"dispatch killactive").expect("parses");
    assert_eq!(request.command, "dispatch");
    let (response, actions) = answer_payload(b"dispatch killactive", &desktop());
    assert_eq!(actions, vec![Action::KillActive]);
    assert_eq!(response.trim(), "ok");
}

#[test]
fn unsupported_outcomes_report_as_invalid_dispatcher() {
    let outcome = Outcome::Unsupported("because".to_string());
    assert!(outcome.response().starts_with("Invalid dispatcher"));
    assert!(!outcome.is_ok());
}

/// Past the keyboard's own reach, a switch is refused rather than
/// silently clamped — clamping would move the user somewhere they did
/// not ask for, which is the confident wrong answer again.
#[test]
fn a_workspace_past_the_end_is_refused_not_clamped() {
    let (response, actions) = answer_payload(b"/dispatch workspace 500", &desktop());
    assert!(actions.is_empty());
    assert!(response.starts_with("Invalid dispatcher"), "got {response:?}");
}

/// Hyprland exposes no lock state of its own, so anything asking
/// whether an Omarchy machine is locked reads `solitaryBlockedBy` and
/// looks for `LOCK` — `omarchy-hyprland-session-locked` does exactly
/// that, and `omarchy-restart-shell` branches on its answer before
/// killing the shell. Reporting null while locked told that script the
/// desk was open, so it would have killed the locker and not put it
/// back.
#[test]
fn a_locked_session_says_so_where_the_only_caller_looks() {
    let json = ask("j/monitors", &locked_desktop());
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("monitors is json");
    let blocked = &parsed[0]["solitaryBlockedBy"];
    assert_eq!(blocked, &serde_json::json!(["LOCK"]), "a locked session must name the lock: {json}");
}

#[test]
fn an_unlocked_session_blocks_nothing_which_is_what_hyprland_reports() {
    let json = ask("j/monitors", &desktop());
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("monitors is json");
    assert!(
        parsed[0]["solitaryBlockedBy"] == serde_json::json!([]),
        "nothing is blocking a solitary client on an unlocked desk: {json}"
    );
}

// ---------------------------------------------------------------------
// The monitor object: values a caller acts on, and the refusal a user
// reads when a control does nothing.

/// A 120 Hz panel used to be reported as 60 Hz, because the rate was a
/// constant rather than a measurement. A bar divides this into a frame
/// budget; on the panel in the fixture the old answer was off by half.
#[test]
fn a_monitors_refresh_rate_is_the_modes_rate_not_a_convention() {
    let monitors = ask_json("j/monitors", &desktop());
    let first = &monitors[0];
    assert_eq!(
        first["refreshRate"], 120.0,
        "the reported rate must come from the mode the session drives: {monitors}"
    );
}

/// Zero is the one case a convention is still right for: no consumer
/// may be handed a rate it will divide by.
#[test]
fn a_monitor_with_no_real_mode_falls_back_rather_than_reporting_zero() {
    let mut desk = desktop();
    desk.monitors[0].refresh_millihertz = 0;
    let monitors = ask_json("j/monitors", &desk);
    assert_eq!(monitors[0]["refreshRate"], 60.0, "0 must never reach a caller that divides by it");
}

/// `availableModes` was an empty list on a compositor that enumerates
/// every connector mode for `zwlr_output_management` at the same moment.
#[test]
fn a_monitors_mode_list_is_the_connectors_modes_in_hyprlands_spelling() {
    let monitors = ask_json("j/monitors", &desktop());
    let modes = monitors[0]["availableModes"].as_array().expect("availableModes is a list").clone();
    assert_eq!(
        modes,
        vec![
            serde_json::json!("2560x1600@120.00Hz"),
            serde_json::json!("2560x1600@60.00Hz")
        ],
        "current mode first, in WIDTHxHEIGHT@RATEHz"
    );
    assert_eq!(monitors[0]["make"], "Sharp", "make comes from the same EDID wl_output advertises");
    assert_eq!(monitors[0]["model"], "eDP-1");
    assert_eq!(monitors[0]["serial"], "0x01020304", "serial is the connector EDID value, not an invention");
}

/// The refusal is the only diagnostic `keyword` has — `hyprctl` exits
/// zero for it — so it must not be false. It used to claim chonkstep
/// does not read a Hyprland config, which is this compositor's headline
/// feature.
#[test]
fn the_keyword_refusal_is_true_and_names_the_routes_that_work() {
    let answer = ask("keyword monitor eDP-1,disable", &desktop());
    assert!(answer.starts_with("Invalid dispatcher:"), "keyword stays a refusal: {answer}");
    assert!(
        !answer.contains("does not read a Hyprland config"),
        "the compositor does read one; saying otherwise sends a reader the wrong way: {answer}"
    );
    assert!(answer.contains("~/.config/hypr"), "name the file that does work: {answer}");
    assert!(answer.contains("hl.monitor"), "name the live route that does work: {answer}");
    assert!(
        answer.contains("disable"),
        "the one shipped caller toggles a monitor off; say why that cannot work: {answer}"
    );
}

#[test]
fn the_two_screensaver_spellings_reach_one_cursor_visibility_action() {
    for (request, hidden) in [
        ("eval hl.config({ cursor = { invisible = true } })", true),
        ("eval hl.config({ cursor = { invisible = false } })", false),
        ("keyword cursor:invisible true", true),
        ("keyword cursor:invisible false", false),
    ] {
        let (response, actions) = answer_payload(request.as_bytes(), &desktop());
        assert_eq!(response.trim(), "ok", "{request}");
        assert_eq!(actions, vec![Action::SetCursorHidden(hidden)], "{request}");
    }
}

#[test]
fn cursor_visibility_does_not_turn_keyword_into_a_general_config_backdoor() {
    let (response, actions) = answer_payload(b"keyword general:border_size 99", &desktop());
    assert!(response.starts_with("Invalid dispatcher:"), "got {response:?}");
    assert!(actions.is_empty());

    let (response, actions) = answer_payload(b"eval hl.config({ cursor = { invisible = maybe } })", &desktop());
    assert!(response.contains("requires true or false"), "got {response:?}");
    assert!(actions.is_empty());
}

#[test]
fn spatial_aliases_toggles_and_inapplicable_messages_have_deliberate_answers() {
    let (_, actions) = answer_payload(b"/dispatch settiled", &desktop());
    assert!(matches!(
        actions.as_slice(),
        [Action::SetFloating {
            floating: Some(false),
            ..
        }]
    ));
    let snapshot = desktop();
    for command in [
        "/dispatch layoutmsg togglesplit",
        "/dispatch togglesplit",
        "/dispatch pseudo",
        "/dispatch hl.dsp.layout(\"togglesplit\")",
    ] {
        let (response, actions) = answer_payload(command.as_bytes(), &snapshot);
        assert_eq!(response, "ok");
        assert_eq!(actions, vec![Action::LayoutNoop]);
    }
    for (command, mode) in [
        ("/keyword workspace 1, layout:scrolling", "scrolling"),
        (
            "/eval hl.workspace_rule({ workspace = \"1\", layout = \"dwindle\" })",
            "dwindle",
        ),
    ] {
        let (response, actions) = answer_payload(command.as_bytes(), &snapshot);
        assert_eq!(response, "ok");
        assert_eq!(
            actions,
            vec![Action::SetWorkspaceLayout {
                workspace: 0,
                mode: mode.into()
            }]
        );
    }
}

#[test]
fn membership_events_and_workspace_json_report_the_authoritative_layout() {
    let mut snapshot = desktop();
    let mut differ = chonk_hyprland_ipc::event::Differ::new();
    differ.diff(&snapshot);
    snapshot.windows[0].floating = false;
    snapshot.workspaces[0].layout = "scrolling".into();
    let events = differ.diff(&snapshot);
    assert!(events.iter().any(|e| e.name() == "changefloatingmode"));
    let (reply, _) = answer_payload(b"j/activeworkspace", &snapshot);
    let workspace: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(workspace["tiledLayout"], "scrolling");
}

/// `binds` reports a ChonkStep binding with no Hyprland dispatcher as
/// `chonkstep <name>`, and Omarchy's keybindings menu hands that pair back
/// to `dispatch`. Only a label the snapshot reports replays, and a locked
/// session replays only a locked binding.
#[test]
fn a_reported_chonkstep_binding_replays_and_nothing_else_does() {
    use chonk_hyprland_ipc::state::Binding;
    let binding = |argument: &str, locked: bool| Binding {
        modifiers: 64,
        key: "up".into(),
        description: String::new(),
        dispatcher: "chonkstep".into(),
        argument: argument.into(),
        locked,
        repeating: false,
        release: false,
    };
    let desk = Snapshot { bindings: vec![binding("overview", false), binding("reload", true)], ..desktop() };
    assert_eq!(dispatch::parse("chonkstep overview", &desk), Outcome::Run(Action::Binding("overview".into())));
    assert!(
        matches!(dispatch::parse("chonkstep not-an-action", &desk), Outcome::Unsupported(why) if why.contains("not-an-action")),
        "an unreported name is refused by name"
    );
    let locked = Snapshot { locked: true, ..desk.clone() };
    assert!(!dispatch::parse("chonkstep overview", &locked).is_ok(), "an unlocked binding waits out the lock");
    assert_eq!(dispatch::parse("chonkstep reload", &locked), Outcome::Run(Action::Binding("reload".into())));
}

/// The report of a ChonkStep next/previous binding is a signed step by
/// index. Read as integers, `+1` named workspace 1 and `-1` a special
/// workspace. Omarchy's `e+1` / `e-1` are a different selector: the
/// next workspace that has windows, so from an empty workspace 2 with
/// only workspace 1 occupied both go to 1, and with 3 occupied as well
/// `e+1` goes on to 3.
#[test]
fn signed_workspace_selectors_are_relative_steps() {
    let mut desk = Snapshot { monitors: vec![monitor(0, "eDP-1", true, 1)], ..desktop() };
    for (wire, index) in [
        ("workspace +1", 2),
        ("workspace -1", 0),
        ("workspace e+1", 0),
        ("workspace e-1", 0),
        ("workspace 3", 2),
    ] {
        assert_eq!(dispatch::parse(wire, &desk), Outcome::Run(Action::FocusWorkspace(index)), "{wire}");
    }
    desk.workspaces[2].windows = 1;
    assert_eq!(dispatch::parse("workspace e+1", &desk), Outcome::Run(Action::FocusWorkspace(2)));
    assert_eq!(dispatch::parse("workspace e-1", &desk), Outcome::Run(Action::FocusWorkspace(0)));
}

#[test]
fn plain_devices_carries_the_active_keymap_line_scripts_grep_for() {
    let mut desk = desktop();
    desk.devices.keyboards.push(Keyboard {
        name: "at-translated-set-2-keyboard".into(),
        layout: "us,de".into(),
        active_keymap: "German".into(),
        active_layout_index: 1,
    });
    let plain = ask("devices", &desk);
    assert!(plain.contains("\t\t\tactive keymap: German\n"), "{plain}");
    assert!(!plain.trim_start().starts_with('{'), "plain devices is not JSON: {plain}");
    assert_eq!(ask_json("j/devices", &desk)["keyboards"][0]["active_keymap"], "German");
}

/// Omarchy's scaling script sends output, mode, position and scale
/// together. Every key is read or the request is refused by name before
/// anything changes, and a mode is checked against the output's own list.
#[test]
fn hl_monitor_reads_every_key_or_refuses_the_request_by_name() {
    let desk = desktop();
    let eval = |source: &str| dispatch::parse_eval(source, &desk);
    assert_eq!(
        eval(r#"hl.monitor({ output = "eDP-1", mode = "2560x1600@59.97", position = "auto", scale = 1.6 })"#),
        Outcome::Run(Action::ConfigureMonitor {
            output: "eDP-1".into(),
            scale_120: Some(192),
            mode: Some("2560x1600@59.97".into()),
            position: Some("auto".into()),
        })
    );
    assert_eq!(
        eval(r#"hl.monitor({ output = "eDP-1", scale = 1.5 })"#),
        Outcome::Run(Action::SetMonitorScale { output: "eDP-1".into(), scale_120: 180 })
    );
    assert_eq!(
        eval(r#"hl.monitor({ output = "eDP-1", position = "2560x0" })"#),
        Outcome::Run(Action::ConfigureMonitor {
            output: "eDP-1".into(),
            scale_120: None,
            mode: None,
            position: Some("2560x0".into()),
        })
    );
    for (source, named) in [
        (r#"hl.monitor({ output = "eDP-1", disabled = true, scale = 1.5 })"#, "disabled"),
        (r#"hl.monitor({ output = "eDP-1", mirror = "DP-1" })"#, "mirror"),
        (r#"hl.monitor({ output = "eDP-1", mode = "1920x1080@60", scale = 1.5 })"#, "1920x1080@60"),
        (r#"hl.monitor({ output = "eDP-1", mode = "2560x1600@90" })"#, "2560x1600@90"),
        (r#"hl.monitor({ output = "eDP-1", position = "left" })"#, "left"),
        (r#"hl.monitor({ output = "eDP-1" })"#, "needs a mode"),
    ] {
        assert!(matches!(eval(source), Outcome::Unsupported(why) if why.contains(named)), "{source}: {:?}", eval(source));
    }
}

/// A snapshot that lists no modes for an output (the nested backend has no
/// connector to enumerate) cannot refuse a well-formed mode at parse: the
/// compositor resolves it against the live output before answering. A
/// malformed mode is still refused here.
#[test]
fn hl_monitor_defers_the_mode_check_when_the_snapshot_lists_no_modes() {
    let mut desk = desktop();
    desk.monitors[0].modes.clear();
    let eval = |source: &str| dispatch::parse_eval(source, &desk);
    assert_eq!(
        eval(r#"hl.monitor({ output = "eDP-1", mode = "1280x800@60.00", position = "auto", scale = 1.25 })"#),
        Outcome::Run(Action::ConfigureMonitor {
            output: "eDP-1".into(),
            scale_120: Some(150),
            mode: Some("1280x800@60.00".into()),
            position: Some("auto".into()),
        })
    );
    assert!(matches!(eval(r#"hl.monitor({ output = "eDP-1", mode = "wide" })"#), Outcome::Unsupported(why) if why.contains("wide")));
}
