//! Tests over **real captured fragments from a real machine**.
//!
//! Every fixture under `tests/fixtures/hyprland/` is a byte-for-byte
//! copy of a file that was on the development machine when this module
//! was written: `machine/` is an Omarchy 4 install (Lua defaults, a
//! user config carrying *both* syntaxes because an upgrade left the
//! old one behind), and `conf-machine/` is the same install's Omarchy
//! 3 configuration, recovered from the migration's own backup
//! directory.
//!
//! Nothing here is invented syntax. That is the point: a parser tested
//! against examples its author wrote is a parser tested against its
//! author's beliefs about the format. These files were written by
//! Omarchy and by a user, and they contain the awkward things a
//! synthetic fixture never does — a `for` loop generating twenty
//! bindings, a helper whose expansion is three shell-quoted arguments,
//! a URL with a `##` in it, a window rule that only works through a
//! tag defined in a different file, and a `source` line pointing at a
//! symlink into a compatibility shim.

use super::*;
use wm_core::FloatPolicy;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hyprland")
}

/// The Omarchy 4 machine.
fn machine() -> Roots {
    Roots::under(&fixtures().join("machine"))
}

/// The same machine's Omarchy 3 configuration.
fn conf_machine() -> Roots {
    Roots::under(&fixtures().join("conf-machine"))
}

/// A scratch home, for the tests that write config files.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chonk-hypr-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".config/hypr")).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

/// The action bound to a chonkstep key spec, if any.
fn action_for(reading: &Reading, spec: &str) -> Option<Action> {
    let combo = crate::parse_key(spec).expect("test spec must parse");
    reading
        .keybindings
        .iter()
        .find(|(existing, _)| *existing == combo)
        .map(|(_, action)| action.clone())
}

/// The argv a chord ends up running, following the `run` name into the
/// declared commands — which is the only way to check that a binding
/// and its command agree.
fn argv_for(reading: &Reading, spec: &str) -> Option<Vec<String>> {
    match action_for(reading, spec)? {
        Action::Run(name) => reading.commands.get(&name).cloned(),
        _ => None,
    }
}

fn skipped_why(reading: &Reading, needle: &str) -> Option<String> {
    reading
        .skipped
        .iter()
        .find(|s| s.what.contains(needle))
        .map(|s| s.why.clone())
}

// ---- the Omarchy 4 machine, end to end --------------------------------

/// The headline claim: pointed at a real Omarchy 4 install, this reads
/// the whole graph — the user's Lua entry point, the `require` chain
/// into Omarchy's defaults, and the `require_all` fan-out over
/// `bindings/` and `apps/` — and comes back with a working desktop's
/// worth of bindings.
#[test]
fn a_real_omarchy_4_machine_reads_end_to_end() {
    let reading = read(&machine());
    assert!(
        reading.files.len() >= 30,
        "expected the whole require graph, got {} files: {:?}",
        reading.files.len(),
        reading.files
    );
    // The entry file is the Lua one, not the `hyprland.conf` sitting
    // beside it — the migration leftover this machine actually had.
    assert!(
        reading.files[0].ends_with("hyprland.lua"),
        "entry was {:?}",
        reading.files[0]
    );
    assert!(
        !reading.files.iter().any(|f| f.ends_with("hyprland.conf")),
        "the conf entry point must not be read when the Lua one exists"
    );
    assert!(
        reading.keybindings.len() >= 80,
        "only {} bindings",
        reading.keybindings.len()
    );
    assert!(!reading.float_rules.is_empty(), "no float rules");
    assert!(!reading.autostart.is_empty(), "no autostart");
    assert!(!reading.env.is_empty(), "no environment");
}

#[test]
fn selection_layer_bindings_are_scoped_and_complete() {
    let reading = read(&machine());
    let bindings = reading
        .layer_bindings
        .get("selection")
        .expect("selection layer bindings");
    assert_eq!(
        bindings.len(),
        8,
        "Return variants, Tab variants, and four arrows"
    );
    for spec in [
        "return",
        "ctrl+return",
        "tab",
        "ctrl+tab",
        "left",
        "right",
        "up",
        "down",
    ] {
        let combo = crate::parse_key(spec).unwrap();
        assert!(
            bindings.iter().any(|binding| binding.combo == combo),
            "missing scoped {spec}"
        );
        assert!(
            !reading
                .keybindings
                .iter()
                .any(|(global, _)| *global == combo),
            "{spec} leaked into global bindings"
        );
    }
}

/// The three kinds of answer, one chord each, from the real files.
#[test]
fn the_three_answers_each_land_on_a_real_chord() {
    let reading = read(&machine());
    // 1. A window verb chonkstep also has. `o.bind("SUPER + W",
    //    "Close window", hl.dsp.window.close())`.
    assert_eq!(action_for(&reading, "super+w"), Some(Action::Close));
    assert_eq!(
        action_for(&reading, "super+f"),
        Some(Action::ToggleFullscreen)
    );
    assert_eq!(
        action_for(&reading, "super+alt+f"),
        Some(Action::ToggleMaximize)
    );
    for (spec, direction) in [
        ("super+left", crate::FocusDirection::Left),
        ("super+right", crate::FocusDirection::Right),
        ("super+up", crate::FocusDirection::Up),
        ("super+down", crate::FocusDirection::Down),
    ] {
        assert_eq!(action_for(&reading, spec), Some(Action::Focus(direction)), "{spec}");
    }
    // 2. An Omarchy command, run by name. `o.bind("SUPER + SPACE",
    //    "Omarchy menu", "omarchy-menu toggle")`.
    assert_eq!(
        argv_for(&reading, "super+space"),
        Some(vec!["omarchy-menu".into(), "toggle".into()])
    );
    // 3. A tiling verb, deliberately unbound with a reason.
    assert_eq!(
        action_for(&reading, "super+j"),
        Some(Action::LayoutNoop),
        "inapplicable split messages are deliberately quiet"
    );
    assert_eq!(
        skipped_why(&reading, "SUPER + J"),
        None,
        "and the reason has to be recorded, not implied"
    );
}

/// The monitor and workspace chords Omarchy binds beside the workspace
/// row: `CTRL + ALT + TAB` focuses the next monitor, `SUPER + SHIFT +
/// ALT + arrows` move the workspace to the monitor in that direction,
/// `SUPER + CTRL + TAB` goes back to the former workspace, and `SUPER +
/// TAB` is `e+1` — the next workspace that has windows, which is a
/// different verb from stepping by index.
#[test]
fn malformed_and_extreme_monitor_bindings_are_refused_without_panicking() {
    for selector in ["+-2147483648", "--2147483648", "++1", "-+1", "-2147483648", "+2147483647"] {
        let home = scratch("hostile-monitor-selector");
        write(&home.join(".config/hypr/hyprland.conf"), &format!("bind = SUPER, F1, focusmonitor, {selector}\n"));
        let reading = read(&Roots::under(&home));
        assert_eq!(action_for(&reading, "super+f1"), None, "{selector}");
    }
}

#[test]
fn the_monitor_and_former_workspace_chords_bind_to_real_actions() {
    use wm_core::{FocusDirection, OutputTarget};
    let reading = read(&machine());
    assert_eq!(action_for(&reading, "ctrl+alt+tab"), Some(Action::FocusMonitor(OutputTarget::Relative(1))));
    assert_eq!(
        action_for(&reading, "ctrl+alt+shift+tab"),
        Some(Action::FocusMonitor(OutputTarget::Relative(-1)))
    );
    for (spec, direction) in [
        ("super+shift+alt+left", FocusDirection::Left),
        ("super+shift+alt+right", FocusDirection::Right),
        ("super+shift+alt+up", FocusDirection::Up),
        ("super+shift+alt+down", FocusDirection::Down),
    ] {
        assert_eq!(
            action_for(&reading, spec),
            Some(Action::MoveWorkspaceToMonitor(OutputTarget::Direction(direction))),
            "{spec}"
        );
    }
    assert_eq!(action_for(&reading, "super+ctrl+tab"), Some(Action::WorkspacePrevious));
    assert_eq!(action_for(&reading, "super+tab"), Some(Action::WorkspaceNextOccupied));
    assert_eq!(action_for(&reading, "super+shift+tab"), Some(Action::WorkspacePrevOccupied));
    for chord in ["CTRL + ALT + TAB", "SUPER + CTRL + TAB", "SUPER + SHIFT + ALT + LEFT"] {
        assert_eq!(skipped_why(&reading, chord), None, "{chord} is bound, not skipped");
    }
    // A user's own Lua naming an output, and the classic spellings in
    // a conf-only home.
    let lua_home = scratch("monitor-chords-lua");
    write(
        &lua_home.join(".config/hypr/hyprland.lua"),
        concat!(
            "hl.bind(\"SUPER + F1\", hl.dsp.focus({ monitor = \"eDP-1\" }))\n",
            "hl.bind(\"SUPER + F2\", hl.dsp.workspace.move({ monitor = \"HDMI-A-1\" }))\n",
            "hl.bind(\"SUPER + F3\", hl.dsp.focus({ workspace = \"+1\" }))\n",
        ),
    );
    let reading = read(&Roots::under(&lua_home));
    assert_eq!(action_for(&reading, "super+f1"), Some(Action::FocusMonitor(OutputTarget::Name("eDP-1".into()))));
    assert_eq!(
        action_for(&reading, "super+f2"),
        Some(Action::MoveWorkspaceToMonitor(OutputTarget::Name("HDMI-A-1".into())))
    );
    assert_eq!(action_for(&reading, "super+f3"), Some(Action::WorkspaceNext), "a bare +1 steps by index");
    let conf_home = scratch("monitor-chords-conf");
    write(
        &conf_home.join(".config/hypr/hyprland.conf"),
        concat!(
            "bind = SUPER, F4, focusmonitor, l\n",
            "bind = SUPER, F5, movecurrentworkspacetomonitor, +1\n",
            "bind = SUPER, F6, workspace, previous\n",
            "bind = SUPER, F7, workspace, e-1\n",
        ),
    );
    let reading = read(&Roots::under(&conf_home));
    assert_eq!(action_for(&reading, "super+f4"), Some(Action::FocusMonitor(OutputTarget::Direction(FocusDirection::Left))));
    assert_eq!(action_for(&reading, "super+f5"), Some(Action::MoveWorkspaceToMonitor(OutputTarget::Relative(1))));
    assert_eq!(action_for(&reading, "super+f6"), Some(Action::WorkspacePrevious));
    assert_eq!(action_for(&reading, "super+f7"), Some(Action::WorkspacePrevOccupied));
}

/// The `for` loop, which is the reason this reader parses Lua at all
/// rather than pattern-matching it. Twenty of Omarchy's most-used
/// chords are generated by three loops, and each needs the loop
/// variable evaluated through `tostring(workspace + 9)` into a
/// keycode.
#[test]
fn the_generated_workspace_chords_are_expanded_from_the_loop() {
    let reading = read(&machine());
    // `for workspace = 1, 10 do … "SUPER + " .. "code:" ..
    //  tostring(workspace + 9) … hl.dsp.focus({ workspace = "1" })`
    for (spec, index) in [
        ("super+1", 0usize),
        ("super+5", 4),
        ("super+9", 8),
        ("super+0", 9),
    ] {
        assert_eq!(
            action_for(&reading, spec),
            Some(Action::Workspace(index)),
            "{spec}"
        );
    }
    for (spec, index) in [("super+shift+1", 0usize), ("super+shift+0", 9)] {
        assert_eq!(
            action_for(&reading, spec),
            Some(Action::WorkspaceCarry(index)),
            "{spec}"
        );
    }
    // The third loop: the bar's panels, by position.
    assert_eq!(
        argv_for(&reading, "super+ctrl+3"),
        Some(vec![
            "omarchy-shell".into(),
            "-q".into(),
            "shell".into(),
            "togglePanelAt".into(),
            "right".into(),
            "3".into()
        ])
    );
}

/// Omarchy's keyboard resize chords, all twelve, from `tiling.lua`: SUPER
/// with minus or equal, SHIFT for the other axis, and ALT and CTRL for
/// the small and the large step, each `hl.dsp.window.resize({ x = …,
/// y = …, relative = true })`. The deltas are carried as written, in the
/// logical pixels Omarchy writes them in.
#[test]
fn the_keyboard_resize_chords_carry_their_deltas() {
    let reading = read(&machine());
    for (spec, x, y) in [
        ("super+minus", -100, 0),
        ("super+equal", 100, 0),
        ("super+shift+minus", 0, -100),
        ("super+shift+equal", 0, 100),
        ("super+alt+minus", -25, 0),
        ("super+alt+equal", 25, 0),
        ("super+shift+alt+minus", 0, -25),
        ("super+shift+alt+equal", 0, 25),
        ("super+ctrl+minus", -300, 0),
        ("super+ctrl+equal", 300, 0),
        ("super+ctrl+shift+minus", 0, -300),
        ("super+ctrl+shift+equal", 0, 300),
    ] {
        assert_eq!(
            action_for(&reading, spec),
            Some(Action::Resize(wm_core::Point::new(x, y))),
            "{spec}"
        );
    }
}

/// Without `relative = true`, Hyprland's Lua resize sets an exact size,
/// as the classic `resizeactive exact` does. This desktop has no
/// exact-size verb, and an exact size read as a delta would grow the
/// window by the size it asked for, so both spellings are refused by
/// name while a relative resize beside them still binds.
#[test]
fn an_exact_resize_is_refused_rather_than_read_as_a_delta() {
    let root = scratch("exact-resize");
    write(
        &root.join(".config/hypr/hyprland.lua"),
        concat!(
            "hl.bind(\"SUPER + R\", hl.dsp.window.resize({ x = 1300, y = 900 }))\n",
            "hl.bind(\"SUPER + T\", hl.dsp.window.resize({ x = -40, y = 0, relative = true }))\n",
        ),
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(action_for(&reading, "super+r"), None);
    assert!(skipped_why(&reading, "SUPER + R").is_some(), "{:?}", reading.skipped);
    assert_eq!(
        action_for(&reading, "super+t"),
        Some(Action::Resize(wm_core::Point::new(-40, 0)))
    );
    let _ = std::fs::remove_file(root.join(".config/hypr/hyprland.lua"));
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "bind = SUPER, R, resizeactive, exact 1300 900\nbind = SUPER, T, resizeactive, -40 0\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(action_for(&reading, "super+r"), None, "the classic spelling of an exact size");
    assert!(skipped_why(&reading, "SUPER R").is_some(), "{:?}", reading.skipped);
    assert_eq!(
        action_for(&reading, "super+t"),
        Some(Action::Resize(wm_core::Point::new(-40, 0)))
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Hyprland's `movetoworkspacesilent` moves a window *without*
/// following it. The translated action preserves that distinction from
/// `workspace-carry`, including Omarchy's tenth workspace on zero.
#[test]
fn moving_a_window_without_following_it_is_native() {
    let reading = read(&machine());
    assert_eq!(action_for(&reading, "super+shift+alt+1"), Some(Action::WorkspaceSend(0)));
    assert_eq!(action_for(&reading, "super+shift+alt+0"), Some(Action::WorkspaceSend(9)));
    // The scratchpad send is a silent move too, onto the special
    // workspace the toggle chord shows. Omarchy's own `SUPER + ALT + S`.
    assert_eq!(
        action_for(&reading, "super+alt+s"),
        Some(Action::SendToSpecial { name: "scratchpad".into(), follow: false })
    );
}

/// Omarchy's scratchpad chords. `SUPER + S` toggles the special
/// workspace and `SUPER + ALT + S` sends the focused window there
/// without following; the grave spellings do the same. None of them is
/// `miniaturize`, which the send used to be mapped to, and none is
/// unbound, which the toggle used to be.
#[test]
fn the_scratchpad_chords_bind_to_special_workspace_verbs() {
    let reading = read(&machine());
    assert_eq!(action_for(&reading, "super+s"), Some(Action::ToggleSpecial("scratchpad".into())));
    assert_eq!(
        action_for(&reading, "super+alt+s"),
        Some(Action::SendToSpecial { name: "scratchpad".into(), follow: false })
    );
    assert!(!reading.skipped.iter().any(|skip| skip.what == "SUPER + S"), "{:?}", reading.skipped);
    assert!(!reading.keybindings.iter().any(|(_, action)| *action == Action::Miniaturize));

    let root = scratch("scratchpad-grave");
    write(
        &root.join(".config/hypr/hyprland.lua"),
        r#"
hl.bind("SUPER + grave", hl.dsp.workspace.toggle_special("scratchpad"))
hl.bind("SUPER + SHIFT + grave", hl.dsp.window.move({ workspace = "special:scratchpad", follow = false }))
hl.bind("SUPER + ALT + grave", hl.dsp.window.move({ workspace = "special:scratchpad" }))
hl.bind("SUPER + CTRL + grave", hl.dsp.workspace.toggle_special())
hl.bind("SUPER + CTRL + S", hl.dsp.focus({ workspace = "special:notes" }))
hl.bind("SUPER + CTRL + N", hl.dsp.window.move({ workspace = "name:notes", follow = false }))
"#,
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(action_for(&reading, "super+grave"), Some(Action::ToggleSpecial("scratchpad".into())));
    assert_eq!(
        action_for(&reading, "super+shift+grave"),
        Some(Action::SendToSpecial { name: "scratchpad".into(), follow: false })
    );
    assert_eq!(
        action_for(&reading, "super+alt+grave"),
        Some(Action::SendToSpecial { name: "scratchpad".into(), follow: true }),
        "the following form shows the special"
    );
    assert_eq!(
        action_for(&reading, "super+ctrl+grave"),
        Some(Action::ToggleSpecial(wm_core::DEFAULT_SPECIAL_NAME.into())),
        "no name is the default special workspace"
    );
    assert_eq!(action_for(&reading, "super+ctrl+s"), Some(Action::ToggleSpecial("notes".into())));
    assert_eq!(action_for(&reading, "super+ctrl+n"), None, "named workspaces are refused by name");
    assert!(skipped_why(&reading, "SUPER + CTRL + N").is_some(), "{:?}", reading.skipped);
    let _ = std::fs::remove_file(root.join(".config/hypr/hyprland.lua"));

    write(
        &root.join(".config/hypr/hyprland.conf"),
        "bind = SUPER, grave, togglespecialworkspace, scratchpad\n\
         bind = SUPER SHIFT, grave, movetoworkspacesilent, special:scratchpad\n\
         bind = SUPER ALT, grave, movetoworkspace, special\n\
         bind = SUPER CTRL, grave, togglespecialworkspace\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(action_for(&reading, "super+grave"), Some(Action::ToggleSpecial("scratchpad".into())));
    assert_eq!(
        action_for(&reading, "super+shift+grave"),
        Some(Action::SendToSpecial { name: "scratchpad".into(), follow: false })
    );
    assert_eq!(
        action_for(&reading, "super+alt+grave"),
        Some(Action::SendToSpecial { name: wm_core::DEFAULT_SPECIAL_NAME.into(), follow: true })
    );
    assert_eq!(
        action_for(&reading, "super+ctrl+grave"),
        Some(Action::ToggleSpecial(wm_core::DEFAULT_SPECIAL_NAME.into()))
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `binds.disable_keybind_grabbing`, Hyprland's spelling of
/// `allow_shortcut_inhibit = false`: read from the `binds` table, off
/// by default as in Hyprland, and the config file's own key still has
/// the last word over it.
#[test]
fn disable_keybind_grabbing_maps_onto_allow_shortcut_inhibit() {
    let root = scratch("binds-grabbing");
    write(&root.join(".config/hypr/hyprland.conf"), "binds {\n    disable_keybind_grabbing = true\n}\n");
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.disable_keybind_grabbing, Some(true), "{:?}", reading.skipped);
    assert!(skipped_why(&reading, "disable_keybind_grabbing").is_none(), "carried, not reported");
    let live = || Some(read(&Roots::under(&root)));
    assert!(!crate::parse_with("desktop = \"omarchy\"", &live).unwrap().allow_shortcut_inhibit);
    assert!(
        crate::parse_with("desktop = \"omarchy\"\nallow_shortcut_inhibit = true", &live).unwrap().allow_shortcut_inhibit,
        "config.toml has the last word"
    );
    assert!(
        crate::parse_with("desktop = \"omarchy\"", &|| None).unwrap().allow_shortcut_inhibit,
        "granted by default, as in Hyprland"
    );

    write(&root.join(".config/hypr/hyprland.conf"), "binds {\n    disable_keybind_grabbing = false\n}\n");
    assert_eq!(read(&Roots::under(&root)).disable_keybind_grabbing, Some(false));
    assert!(crate::parse_with("desktop = \"omarchy\"", &live).unwrap().allow_shortcut_inhibit);
    write(&root.join(".config/hypr/hyprland.conf"), "binds {\n    disable_keybind_grabbing = maybe\n}\n");
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.disable_keybind_grabbing, None);
    assert!(skipped_why(&reading, "disable_keybind_grabbing").is_some(), "a bad value is reported: {:?}", reading.skipped);
    let _ = std::fs::remove_dir_all(&root);
}

/// `binds.hide_special_on_workspace_change`, the one `binds` key with a
/// meaning here: Omarchy sets it, and a workspace switch then takes
/// the scratchpad down with it.
#[test]
fn hide_special_on_workspace_change_is_read_from_either_syntax() {
    let reading = read(&machine());
    assert_eq!(reading.hide_special_on_workspace_change, Some(true), "{:?}", reading.skipped);
    let config = crate::parse_with("desktop = \"omarchy\"", &|| Some(read(&machine()))).unwrap();
    assert!(config.hide_special_on_workspace_change);
    assert!(!crate::parse("").unwrap().hide_special_on_workspace_change, "off by default, as in Hyprland");

    let root = scratch("binds-conf");
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "binds {\n    hide_special_on_workspace_change = false\n    workspace_back_and_forth = true\n}\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.hide_special_on_workspace_change, Some(false));
    assert!(
        skipped_why(&reading, "workspace_back_and_forth").is_some(),
        "the rest of the table is reported: {:?}",
        reading.skipped
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `misc.focus_on_activate`, the one `misc` key with a meaning here:
/// Omarchy's shipped `looknfeel.lua` turns it on, so a desk reading
/// Omarchy's files keeps honouring every activation request; a file
/// that says `false`, or says nothing, leaves the default refusal.
#[test]
fn focus_on_activate_is_read_from_either_syntax() {
    let reading = read(&machine());
    assert_eq!(reading.focus_on_activate, Some(true), "{:?}", reading.skipped);
    let config = crate::parse_with("desktop = \"omarchy\"", &|| Some(read(&machine()))).unwrap();
    assert!(config.focus_on_activate);
    assert!(!crate::parse("").unwrap().focus_on_activate, "off by default, as in Hyprland");

    let root = scratch("misc-conf");
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "misc {\n    focus_on_activate = false\n    disable_hyprland_logo = true\n}\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.focus_on_activate, Some(false));
    assert!(
        skipped_why(&reading, "disable_hyprland_logo").is_some(),
        "the rest of the table is reported: {:?}",
        reading.skipped
    );

    // The colon spelling, and a later line winning.
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "misc:focus_on_activate = false\nmisc:focus_on_activate = yes\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.focus_on_activate, Some(true));

    // A value that is not a toggle is reported, not guessed at.
    write(&root.join(".config/hypr/hyprland.conf"), "misc {\n    focus_on_activate = sometimes\n}\n");
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.focus_on_activate, None);
    assert!(skipped_why(&reading, "focus_on_activate").is_some(), "{:?}", reading.skipped);
    let _ = std::fs::remove_dir_all(&root);
}

/// `misc.disable_autoreload`, the other `misc` key with a meaning here:
/// the configuration's own baseline for the switch Omarchy's upgrade
/// hooks throw live over the IPC. Off unless the file says so, as in
/// Hyprland, in every spelling the file can use.
#[test]
fn disable_autoreload_is_read_from_either_syntax() {
    let root = scratch("autoreload-conf");
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "misc {\n    disable_autoreload = true\n    disable_hyprland_logo = true\n}\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.disable_autoreload, Some(true), "{:?}", reading.skipped);
    assert!(skipped_why(&reading, "disable_autoreload").is_none(), "{:?}", reading.skipped);
    let config = crate::parse_with("desktop = \"omarchy\"", &|| Some(read(&Roots::under(&root)))).unwrap();
    assert!(config.disable_autoreload);
    assert!(!crate::parse("").unwrap().disable_autoreload, "off by default, as in Hyprland");

    // The colon spelling, and a later line winning.
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "misc:disable_autoreload = true\nmisc:disable_autoreload = 0\n",
    );
    assert_eq!(read(&Roots::under(&root)).disable_autoreload, Some(false));

    // Omarchy 4's Lua spelling.
    let lua = scratch("autoreload-lua");
    write(&lua.join(".config/hypr/hyprland.lua"), "hl.config({ misc = { disable_autoreload = true } })\n");
    let reading = read(&Roots::under(&lua));
    assert_eq!(reading.disable_autoreload, Some(true), "{:?}", reading.skipped);

    // A value that is not a toggle is reported, not guessed at.
    write(&root.join(".config/hypr/hyprland.conf"), "misc {\n    disable_autoreload = later\n}\n");
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.disable_autoreload, None);
    assert!(skipped_why(&reading, "disable_autoreload").is_some(), "{:?}", reading.skipped);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&lua);
}

/// The conditional Omarchy gates its preinstalled application chords
/// on is a file-system question, and answering it is a strict
/// improvement on the baked preset — which had to write off twenty-odd
/// chords as `Unbound::Conditional` because a table of constants
/// cannot make that test.
#[test]
fn the_preinstalled_gate_is_answered_from_the_file_system() {
    let root = fixtures().join("machine");
    let mut roots = Roots::under(&root);
    // The marker file is absent in the fixture, so the gate is open and
    // the webapp chords are bound.
    let open = read(&roots);
    assert_eq!(
        argv_for(&open, "super+shift+a"),
        Some(vec![
            "omarchy-launch-webapp".into(),
            "https://chatgpt.com".into()
        ]),
        "the ChatGPT webapp chord is inside `if o.preinstalled_bindings_enabled() then`"
    );
    // Now say the preinstalls were removed, the way Omarchy says it.
    let state = scratch("preinstalls");
    // A home whose state directory has the marker, but whose config
    // and Omarchy tree are the fixture's.
    roots.facts.home = Some(state.clone());
    roots.facts.state_home = Some(state.join(".local/state"));
    write(&state.join(".local/state/omarchy/preinstalls-removed"), "");
    let closed = read(&roots);
    assert_eq!(
        action_for(&closed, "super+shift+a"),
        None,
        "the gate is shut; the chord must not be bound"
    );
    // The ungated essentials in the same file are unaffected.
    assert!(
        action_for(&closed, "super+return").is_some(),
        "SUPER+RETURN is outside the gate"
    );
    let _ = std::fs::remove_dir_all(&state);
}

/// A global set in the user's own `hyprland.lua` reaches the file it
/// is read in, because the loader splices includes in place rather
/// than reading files in a fixed order.
#[test]
fn a_global_set_before_a_require_reaches_the_file_that_reads_it() {
    let root = scratch("globals");
    let fixture = fixtures().join("machine");
    // Symlink the Omarchy tree in rather than copying it: the read is
    // read-only, and this keeps the test about the one file it changes.
    std::os::unix::fs::symlink(fixture.join("omarchy"), root.join("omarchy")).unwrap();
    write(
        &root.join(".config/hypr/hyprland.lua"),
        "omarchy_default_bindings = false\nrequire(\"default.hypr.omarchy\")\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(
        action_for(&reading, "super+w"),
        None,
        "`omarchy_default_bindings = false` must silence the default bindings"
    );
    // ...and the same tree with the global left alone binds it.
    write(
        &root.join(".config/hypr/hyprland.lua"),
        "require(\"default.hypr.omarchy\")\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(action_for(&reading, "super+w"), Some(Action::Close));
    // Omarchy's gate is `_G.omarchy_default_bindings ~= false`, and
    // `nil ~= false` is true: setting the global back to `nil` restores
    // the defaults, and a `local` of the same name never touches `_G`.
    for entry in [
        "omarchy_default_bindings = false\nomarchy_default_bindings = nil\nrequire(\"default.hypr.omarchy\")\n",
        "local omarchy_default_bindings = false\nrequire(\"default.hypr.omarchy\")\n",
    ] {
        write(&root.join(".config/hypr/hyprland.lua"), entry);
        let reading = read(&Roots::under(&root));
        assert_eq!(action_for(&reading, "super+w"), Some(Action::Close), "{entry}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Omarchy's `helpers.lua` expands `{ omarchy = "browser" }` into
/// `omarchy-launch-browser` and `{ webapp = …, focus = true }` into a
/// three-argument shell-quoted command line. Getting these wrong would
/// not fail loudly — it would bind a chord to a command that almost
/// works — so each expansion is pinned against the helper it mirrors.
#[test]
fn the_bind_helper_forms_expand_exactly_as_omarchy_expands_them() {
    let reading = read(&machine());
    assert_eq!(
        argv_for(&reading, "super+shift+return"),
        Some(vec!["omarchy-launch-browser".into()])
    );
    assert_eq!(
        argv_for(&reading, "super+shift+alt+b"),
        Some(vec!["omarchy-launch-browser".into(), "--private".into()])
    );
    // `{ tui = "cliamp", focus = true }`
    assert_eq!(
        argv_for(&reading, "super+shift+alt+m"),
        Some(vec!["omarchy-launch-or-focus-tui".into(), "cliamp".into()])
    );
    // `{ launch = "obsidian", focus = "^obsidian$" }`. The expansion
    // carries a `$` — inside single quotes, where it is a regex anchor
    // rather than a variable — and a line with shell grammar anywhere
    // in it keeps its shell rather than being argv-split by a reader
    // that is not one. Hyprland's own `exec` runs through a shell too,
    // so this is the faithful answer as well as the safe one.
    assert_eq!(
        argv_for(&reading, "super+shift+o"),
        Some(vec![
            "bash".into(),
            "-lc".into(),
            "omarchy-launch-or-focus '^obsidian$' 'uwsm-app -- obsidian'".into()
        ])
    );
    // `o.bind_toggle("SUPER + CTRL + I", …, "idle")`
    assert_eq!(
        argv_for(&reading, "super+ctrl+i"),
        Some(vec!["omarchy-toggle-idle".into()])
    );
}

/// A chord that runs `hyprctl` or an `omarchy-hyprland-*` script whose
/// requests chonkstep does not serve is left unbound, with a reason
/// naming what the script needs — the same predicate
/// `chonk_shell::omarchy_menu` applies to menu rows. The scripts on
/// `dispatch::SERVED_OMARCHY_SCRIPTS` send only requests the IPC
/// applies, and bind like any other command; they were refused by a
/// name test for years after the IPC began serving them. `hyprpicker`
/// is deliberately *not* caught either: it is an ordinary layer-shell
/// client and works here.
#[test]
fn bindings_that_command_hyprland_stay_unbound_and_hyprpicker_does_not() {
    use crate::preset::Unbound;
    let reading = read(&machine());
    for (chord, argv) in [
        ("super+o", &["omarchy-hyprland-window-pop"][..]),
        ("super+alt+home", &["omarchy-hyprland-window-width", "save"]),
        ("super+home", &["omarchy-hyprland-window-width", "restore"]),
        ("super+slash", &["omarchy-hyprland-monitor-scaling", "up"]),
        ("super+alt+slash", &["omarchy-hyprland-monitor-scaling", "down"]),
        ("ctrl+alt+delete", &["omarchy-hyprland-window-close-all"]),
        ("super+ctrl+f", &["omarchy-hyprland-window-tiled-fullscreen-toggle"]),
    ] {
        let argv: Vec<String> = argv.iter().map(|word| word.to_string()).collect();
        assert_eq!(argv_for(&reading, chord), Some(argv), "{chord} runs its served script");
    }
    for (chord, what, reason) in [
        ("super+shift+backspace", "SUPER + SHIFT + BACKSPACE (Toggle window gaps)", Unbound::GAPS),
        (
            "super+ctrl+backspace",
            "SUPER + CTRL + BACKSPACE (Toggle single-window square aspect)",
            Unbound::LAYOUT_OPTION,
        ),
        ("super+ctrl+delete", "SUPER + CTRL + Delete (Toggle laptop display)", Unbound::OUTPUT_DISABLE),
        (
            "super+ctrl+alt+delete",
            "SUPER + CTRL + ALT + Delete (Toggle laptop display mirroring)",
            Unbound::OUTPUT_MIRROR,
        ),
    ] {
        assert_eq!(action_for(&reading, chord), None, "{chord} must stay unbound");
        assert_eq!(skipped_why(&reading, what), Some(reason.reason().to_string()), "{what}");
    }
    // The transparency toggle's script sends only requests the IPC
    // serves now, and its chord takes the native toggle directly.
    assert_eq!(
        action_for(&reading, "super+backspace"),
        Some(Action::ToggleOpaque),
        "SUPER + BACKSPACE toggles the focused window opaque"
    );
    // The clamshell half of Omarchy's lid bindings binds as a switch and
    // then meets the script filter: it disables outputs through requests
    // this desktop does not serve.
    assert!(
        skipped_why(&reading, "switch:off:Lid Switch").is_some_and(|why| !why.contains("pointer or switch")),
        "{:?}",
        reading.skipped
    );
    assert_eq!(
        argv_for(&reading, "super+print"),
        Some(vec![
            "bash".into(),
            "-lc".into(),
            "pkill hyprpicker || hyprpicker -a".into()
        ]),
        "hyprpicker is an ordinary client; its `||` needs a shell"
    );
}

/// Keys with pictures on them, which the whole preset exists partly to
/// reach: a laptop's volume and brightness keys arrive as `XF86…`
/// names and have to become this format's short ones.
#[test]
fn the_media_keys_survive_the_rename() {
    let reading = read(&machine());
    assert_eq!(
        argv_for(&reading, "volumeup"),
        Some(vec!["omarchy-audio-output-volume".into(), "raise".into()])
    );
    assert_eq!(
        argv_for(&reading, "brightnessdown"),
        Some(vec!["omarchy-brightness-display".into(), "5%-".into()])
    );
    assert_eq!(
        argv_for(&reading, "playpause"),
        Some(vec![
            "omarchy-shell".into(),
            "media".into(),
            "playPause".into()
        ])
    );
    assert_eq!(
        argv_for(&reading, "poweroff"),
        Some(vec![
            "omarchy-menu".into(),
            "toggle".into(),
            "system".into()
        ])
    );
}

#[test]
fn binding_flags_preserve_press_release_lock_and_repeat() {
    let root = scratch("binding-flags");
    write(
        &root.join(".config/hypr/hyprland.lua"),
        r#"
o.bind("F9", "Start dictation", "voxtype record start")
o.bind("F9", "Stop dictation", "voxtype record stop", { release = true })
o.bind("XF86AudioRaiseVolume", "Volume up", "volume up", { locked = true, repeating = true })
"#,
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(
        argv_for(&reading, "f9"),
        Some(vec!["voxtype".into(), "record".into(), "start".into()])
    );
    let f9 = crate::parse_key("f9").unwrap();
    let release = reading
        .bindings
        .iter()
        .find(|binding| binding.combo == f9 && binding.release)
        .unwrap();
    let Action::Run(name) = &release.action else {
        panic!("release should run the stop command")
    };
    assert_eq!(reading.commands[name], vec!["voxtype", "record", "stop"]);
    let volume = crate::parse_key("volumeup").unwrap();
    let volume = reading
        .bindings
        .iter()
        .find(|binding| binding.combo == volume)
        .unwrap();
    assert!(volume.locked && volume.repeating && !volume.release);
}

#[test]
fn keyboard_input_is_carried_but_hyprlands_focus_policy_is_not() {
    let root = scratch("input-table");
    write(
        &root.join(".config/hypr/hyprland.lua"),
        r#"
hl.config({ input = {
  kb_layout = "de", kb_variant = "nodeadkeys", kb_model = "pc105",
  kb_options = "compose:caps", repeat_rate = 40, repeat_delay = 250,
  follow_mouse = 1, sensitivity = -0.25, accel_profile = "flat",
  touchpad = {
    natural_scroll = true, tap_to_click = false, disable_while_typing = false,
    clickfinger_behavior = true, scroll_factor = 0.4,
  },
} })
o.bind("XF86TouchpadToggle", "Touchpad", "touchpad toggle")
o.bind("XF86TouchpadOn", "Touchpad on", "touchpad on")
o.bind("XF86TouchpadOff", "Touchpad off", "touchpad off")
o.bind("SUPER + SHIFT + code:201", "Menu", "omarchy-menu")
"#,
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.input.layout.as_deref(), Some("de"));
    assert_eq!(reading.input.variant.as_deref(), Some("nodeadkeys"));
    assert_eq!(reading.input.model.as_deref(), Some("pc105"));
    assert_eq!(reading.input.options.as_deref(), Some("compose:caps"));
    assert_eq!(reading.input.repeat_rate, Some(40));
    assert_eq!(reading.input.repeat_delay, Some(250));
    assert_eq!(reading.input.sensitivity, Some(-0.25));
    assert_eq!(reading.input.accel_profile.as_deref(), Some("flat"));
    assert_eq!(reading.input.touchpad_natural_scroll, Some(true));
    assert_eq!(reading.input.natural_scroll, None, "Omarchy's touchpad table must not reach mice");
    assert_eq!(reading.input.tap_to_click, Some(false));
    assert_eq!(reading.input.disable_while_typing, Some(false));
    assert_eq!(reading.input.clickfinger_behavior, Some(true));
    assert_eq!(reading.input.touchpad_scroll_factor, Some(0.4));
    assert_eq!(reading.input.scroll_factor, None);
    assert!(
        skipped_why(&reading, "follow_mouse")
            .is_some_and(|why| why.contains("focus policy belongs to chonkstep")),
        "Hyprland's whole-desktop pointer policy must be declined by name"
    );
    let config = crate::parse_with("desktop = \"omarchy\"", &|| {
        Some(read(&Roots::under(&root)))
    })
    .unwrap();
    assert!(
        !config.focus_follows_mouse,
        "stock Omarchy's follow_mouse = 1 must not override chonkstep's click-to-focus default"
    );
    for spec in [
        "touchpadtoggle",
        "touchpadon",
        "touchpadoff",
        "super+shift+f23",
    ] {
        assert!(action_for(&reading, spec).is_some(), "missing {spec}");
    }
}

/// Omarchy sets `numlock_by_default`, and its user template offers
/// `drag_3fg` beside the other documented touchpad keys. Each arrives from
/// either syntax as a typed value, and each keeps to its device class: a
/// touchpad's middle-button emulation is not a trackball's, and the
/// trackball's scroll method is not the touchpad's.
#[test]
fn the_documented_touchpad_and_pointer_keys_are_carried_by_class() {
    let lua = scratch("input-extras-lua");
    write(
        &lua.join(".config/hypr/hyprland.lua"),
        r#"
hl.config({ input = {
  numlock_by_default = true, scroll_method = "on_button_down", scroll_button = 274,
  touchpad = {
    tap_and_drag = false, drag_lock = 1, middle_button_emulation = true,
    tap_button_map = "lmr", drag_3fg = 2,
  },
} })
"#,
    );
    let conf = scratch("input-extras-conf");
    write(
        &conf.join(".config/hypr/hyprland.conf"),
        concat!(
            "input {\n",
            "    numlock_by_default = true\n",
            "    scroll_method = on_button_down\n",
            "    scroll_button = 274\n",
            "    touchpad {\n",
            "        tap-and-drag = false\n",
            "        drag_lock = 1\n",
            "        middle_button_emulation = true\n",
            "        tap_button_map = lmr\n",
            "        drag_3fg = 2\n",
            "    }\n",
            "}\n",
        ),
    );
    for root in [lua, conf] {
        let reading = read(&Roots::under(&root));
        let input = &reading.input;
        assert!(!reading.skipped.iter().any(|skip| skip.kind == "input"), "{root:?}: {:?}", reading.skipped);
        assert_eq!(input.numlock_by_default, Some(true), "{root:?}");
        assert_eq!(input.scroll_method, Some(wm_core::ScrollMethod::OnButtonDown));
        assert_eq!(input.scroll_button, Some(274));
        assert_eq!(input.touchpad_scroll_method, None, "a mouse's scroll method must not reach touchpads");
        assert_eq!(input.touchpad_middle_button_emulation, Some(true));
        assert_eq!(input.middle_button_emulation, None, "a touchpad's middle-button emulation must not reach mice");
        assert_eq!(input.tap_and_drag, Some(false));
        assert_eq!(input.drag_lock, Some(true));
        assert_eq!(input.tap_button_map, Some(wm_core::TapButtonMap::LeftMiddleRight));
        assert_eq!(input.drag_3fg, Some(wm_core::MultiFingerDrag::FourFingers));
        let config = crate::parse_with("desktop = \"omarchy\"", &|| Some(read(&Roots::under(&root)))).unwrap();
        assert_eq!(config.input.numlock_by_default, Some(true), "{root:?}");
        assert_eq!(config.input.drag_3fg, Some(wm_core::MultiFingerDrag::FourFingers));
    }
}

#[test]
fn out_of_range_touchpad_and_pointer_values_are_refused_by_name() {
    for (name, value, reason) in [
        ("touchpad:drag_lock", "2", "sticky"),
        ("touchpad:drag_lock", "maybe", "true or false"),
        ("touchpad:drag_3fg", "3", "four fingers"),
        ("touchpad:drag_3fg", "-1", "four fingers"),
        ("touchpad:tap_button_map", "rml", "lrm or lmr"),
        ("touchpad:tap-and-drag", "sometimes", "true or false"),
        ("touchpad:middle_button_emulation", "2", "true or false"),
        ("scroll_method", "natural", "on_button_down"),
        ("scroll_button", "301", "0 through 300"),
        ("scroll_button", "-1", "0 through 300"),
        ("scroll_button", "BTN_MIDDLE", "0 through 300"),
        ("numlock_by_default", "on-ish", "true or false"),
    ] {
        let mut reading = Reading::default();
        input(&mut reading, name, value);
        assert_eq!(reading.input, crate::InputConfig::default(), "{name} = {value}");
        assert_eq!(reading.skipped.len(), 1, "{name} = {value}");
        let skip = &reading.skipped[0];
        assert_eq!(skip.what, format!("{name} = {value}"), "the refusal names the key and the value");
        assert!(skip.why.contains(reason), "{name} = {value}: {}", skip.why);
    }
    let mut reading = Reading::default();
    input(&mut reading, "scroll_button", "0");
    input(&mut reading, "scroll_button", "300");
    input(&mut reading, "touchpad:drag_3fg", "0");
    input(&mut reading, "scroll_method", "NO_SCROLL");
    assert_eq!(reading.input.scroll_button, Some(300));
    assert_eq!(reading.input.drag_3fg, Some(wm_core::MultiFingerDrag::Disabled));
    assert_eq!(reading.input.scroll_method, Some(wm_core::ScrollMethod::NoScroll));
    assert!(reading.skipped.is_empty(), "{:?}", reading.skipped);
}

/// A device rule reaches one device by its exact name, written either way,
/// merges with a later rule for the same name, and keeps only the settings a
/// rule can carry.
#[test]
fn device_rules_are_read_by_exact_name_from_either_syntax() {
    let lua = scratch("device-rules-lua");
    write(
        &lua.join(".config/hypr/hyprland.lua"),
        r#"
hl.device({ name = "epic-mouse-v1", sensitivity = -0.5, enabled = false, scroll_factor = 2 })
hl.device({ name = "SynPS/2 Synaptics TouchPad", natural_scroll = true, tap_to_click = true })
hl.device({ name = "epic-mouse-v1", accel_profile = "flat" })
"#,
    );
    let conf = scratch("device-rules-conf");
    write(
        &conf.join(".config/hypr/hyprland.conf"),
        concat!(
            "device {\n    sensitivity = -0.5\n    name = epic-mouse-v1\n    enabled = false\n    scroll_factor = 2\n}\n",
            "device {\n    name = SynPS/2 Synaptics TouchPad\n    natural_scroll = true\n    tap-to-click = true\n}\n",
            "device {\n    name = epic-mouse-v1\n    accel_profile = flat\n}\n",
        ),
    );
    let mouse = wm_core::DeviceRule {
        name: "epic-mouse-v1".into(),
        enabled: Some(false),
        sensitivity: Some(-0.5),
        accel_profile: Some("flat".into()),
        ..Default::default()
    };
    let touchpad = wm_core::DeviceRule {
        name: "SynPS/2 Synaptics TouchPad".into(),
        natural_scroll: Some(true),
        tap_to_click: Some(true),
        ..Default::default()
    };
    for root in [lua, conf] {
        let reading = read(&Roots::under(&root));
        assert_eq!(reading.input.devices, [mouse.clone(), touchpad.clone()], "{root:?}: {:?}", reading.skipped);
        assert!(
            skipped_why(&reading, "scroll_factor").is_some_and(|why| why.contains("not implemented")),
            "{root:?}: a setting a rule cannot carry is named: {:?}",
            reading.skipped
        );
        let config = crate::parse_with("desktop = \"omarchy\"", &|| Some(read(&Roots::under(&root)))).unwrap();
        assert_eq!(config.input.devices, [mouse.clone(), touchpad.clone()], "{root:?}");
    }
}

#[test]
fn device_rules_without_a_plain_name_and_past_the_bound_are_refused_by_name() {
    let mut source = String::new();
    for index in 0..70 {
        source.push_str(&format!("hl.device({{ name = \"device {index}\", enabled = false }})\n"));
    }
    source.push_str(concat!(
        "hl.device({ name = \"\", enabled = false })\n",
        "hl.device({ enabled = false })\n",
        "hl.device({ name = device_name, enabled = false })\n",
        "hl.device({ name = \"late\", enabled = decided_later })\n",
        "hl.device({ name = \"bell\\n\", enabled = false })\n",
        "hl.device({ name = \"late\", enabled = sometimes })\n",
    ));
    source.push_str(&format!("hl.device({{ name = \"{}\", enabled = false }})\n", "x".repeat(300)));
    let root = scratch("device-rules-refused");
    write(&root.join(".config/hypr/hyprland.lua"), &source);
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.input.devices.len(), wm_core::DeviceRule::MAX_RULES);
    let why = |needle: &str| reading.skipped.iter().filter(|skip| skip.why.contains(needle)).count();
    let what = |needle: &str| reading.skipped.iter().filter(|skip| skip.what.contains(needle)).count();
    assert_eq!(why("more than 64 device rules"), 6, "{:?}", reading.skipped);
    assert_eq!(why("1 to 256 bytes"), 3, "an empty name, a control character and an oversized name");
    assert_eq!(what("needs the device's name"), 1);
    assert_eq!(what("is not a string in the file"), 1);
    assert_eq!(what("is computed at runtime"), 2);
}

/// Omarchy keeps a touchpad or touchscreen disable as one line naming the
/// device and re-applies it through `disabled_input_device`. That line came
/// from a USB descriptor: it is read as a name and never as Lua, and a line
/// that is not a name is refused.
#[test]
fn omarchys_persisted_device_disable_is_read_as_data_and_never_as_lua() {
    let root = scratch("persisted-device-disable");
    write(
        &root.join(".config/hypr/hyprland.lua"),
        concat!(
            "local disabled_input_device = require(\"default.hypr.disabled-input-device\")\n",
            "disabled_input_device(\"touchpad\")\n",
            "disabled_input_device(\"touchscreen\")\n",
            "disabled_input_device(\"keyboard\")\n",
        ),
    );
    // The module reads the file itself and calls `hl.device` at runtime; its
    // body is a function definition, never a rule of its own.
    write(
        &root.join("omarchy/default/hypr/disabled-input-device.lua"),
        "return function(kind)\n  hl.device({ name = \"never\", enabled = false })\nend\n",
    );
    let hostile = r#"Evil \" }) os.execute("touch pwned") hl.device({ name = ""#;
    let toggles = root.join(".local/state/omarchy/toggles/hypr");
    write(&toggles.join("touchpad-disabled-name"), &format!("{hostile}\n"));
    let reading = read(&Roots::under(&root));
    assert_eq!(
        reading.input.devices,
        [wm_core::DeviceRule { name: hostile.into(), enabled: Some(false), ..Default::default() }],
        "{:?}",
        reading.skipped
    );
    assert!(
        reading.skipped.iter().any(|skip| skip.what.contains("disabled_input_device(") && skip.what.contains("touchpad and touchscreen")),
        "a kind Omarchy never persists is named: {:?}",
        reading.skipped
    );
    assert!(!root.join("pwned").exists());

    write(&toggles.join("touchscreen-disabled-name"), "ELAN\u{7}Touch\n");
    write(&toggles.join("touchpad-disabled-name"), &"x".repeat(400));
    let reading = read(&Roots::under(&root));
    assert!(reading.input.devices.is_empty(), "{:?}", reading.input.devices);
    assert_eq!(
        reading.skipped.iter().filter(|skip| skip.kind == "device" && skip.why.contains("not a device name")).count(),
        2,
        "{:?}",
        reading.skipped
    );
}

/// Omarchy's look turns on `cursor:hide_on_key_press`. The keys that
/// decide when the pointer hides arrive from either syntax, the warp key
/// Omarchy sets beside it is declined by name, and the rest of the
/// section is still reported rather than dropped.
/// Omarchy's stock animation configuration, in both syntaxes: motion
/// stays on, the leaves this desktop has no transition for are named,
/// and the curves are declined as curves rather than as unknown calls.
#[test]
fn omarchys_default_animations_leave_motion_on_and_name_every_leaf_not_read() {
    for (root, curve) in [(machine(), "hl.curve(\"easeOutQuint\")"), (conf_machine(), "bezier = easeOutQuint")] {
        let reading = read(&root);
        assert_eq!(reading.motion, wm_core::MotionPolicy::default(), "{root:?}");
        for leaf in ["workspaces", "border", "layersIn"] {
            assert!(
                reading.skipped.iter().any(|skip| skip.kind == "animation" && skip.what.contains(&format!("leaf {leaf} "))),
                "{root:?}: {leaf} must be declined by name: {:?}",
                reading.skipped
            );
        }
        assert!(
            reading.skipped.iter().any(|skip| skip.kind == "animation" && skip.what.contains(curve)),
            "{root:?}: {:?}",
            reading.skipped
        );
        assert!(
            !reading.skipped.iter().any(|skip| skip.kind == "lua-call" && skip.what.contains("hl.animation")),
            "{root:?}: hl.animation is read, not an unplaced call: {:?}",
            reading.skipped
        );
    }
}

/// The switch Omarchy's own override template offers ("Disable all
/// animations"), uncommented, in every spelling a user can write it.
#[test]
fn animation_off_switches_reach_the_motion_policy_in_both_syntaxes() {
    let lua_cases: [(&str, wm_core::MotionPolicy); 6] = [
        ("hl.config({ animations = { enabled = false } })\n", wm_core::MotionPolicy { enabled: false, ..Default::default() }),
        ("hl.animation({ leaf = \"global\", enabled = false })\n", wm_core::MotionPolicy { enabled: false, ..Default::default() }),
        ("hl.animation({ leaf = \"windows\", enabled = false, speed = 3.79, bezier = \"easeOutQuint\" })\n", wm_core::MotionPolicy { layout: false, ..Default::default() }),
        ("hl.animation({ leaf = \"windowsMove\", enabled = false })\n", wm_core::MotionPolicy { layout: false, ..Default::default() }),
        // Later wins, the way a user's file lands on Omarchy's defaults.
        ("hl.animation({ leaf = \"global\", enabled = false })\nhl.config({ animations = { enabled = true } })\n", wm_core::MotionPolicy::default()),
        // A switch only running code could decide is refused, not guessed.
        ("x = y or false\nhl.animation({ leaf = \"global\", enabled = x })\nhl.config({ animations = { enabled = x } })\n", wm_core::MotionPolicy::default()),
    ];
    for (index, (source, expected)) in lua_cases.iter().enumerate() {
        let home = scratch(&format!("animation-lua-{index}"));
        write(&home.join(".config/hypr/hyprland.lua"), source);
        let reading = read(&Roots::under(&home));
        assert_eq!(reading.motion, *expected, "{source}: {:?}", reading.skipped);
        assert!(
            !reading.skipped.iter().any(|skip| skip.what.contains("outside input")),
            "{source}: animations is a read table: {:?}",
            reading.skipped
        );
    }
    let styled = read(&Roots::under(&{
        let home = scratch("animation-lua-styled");
        write(&home.join(".config/hypr/hyprland.lua"), lua_cases[2].0);
        home
    }));
    assert!(
        styled.skipped.iter().any(|skip| skip.kind == "animation" && skip.what.contains("\"windows\"") && skip.what.contains("speed, bezier")),
        "the speed and curve on a read leaf are declined by name: {:?}",
        styled.skipped
    );
    let runtime = read(&Roots::under(&{
        let home = scratch("animation-lua-runtime");
        write(&home.join(".config/hypr/hyprland.lua"), lua_cases[5].0);
        home
    }));
    assert_eq!(
        runtime.skipped.iter().filter(|skip| skip.what.contains("computed at runtime") && skip.kind == "animation").count(),
        2,
        "{:?}",
        runtime.skipped
    );

    let conf_cases: [(&str, wm_core::MotionPolicy); 4] = [
        ("animations {\n    enabled = no\n}\n", wm_core::MotionPolicy { enabled: false, ..Default::default() }),
        ("animations {\n    enabled = yes\n    animation = windows, 0, 3.79, easeOutQuint\n}\n", wm_core::MotionPolicy { layout: false, ..Default::default() }),
        ("animation = global, 0, 10, default\n", wm_core::MotionPolicy { enabled: false, ..Default::default() }),
        ("animations {\n    enabled = yes, please :)\n    animation = windows, maybe, 3.79, easeOutQuint\n}\n", wm_core::MotionPolicy::default()),
    ];
    for (index, (source, expected)) in conf_cases.iter().enumerate() {
        let home = scratch(&format!("animation-conf-{index}"));
        write(&home.join(".config/hypr/hyprland.conf"), source);
        let reading = read(&Roots::under(&home));
        assert_eq!(reading.motion, *expected, "{source}: {:?}", reading.skipped);
        assert!(
            !reading.skipped.iter().any(|skip| skip.kind == "block" && skip.what.contains("animations")),
            "{source}: the block is read line by line, not refused whole: {:?}",
            reading.skipped
        );
    }
    let unreadable = read(&Roots::under(&{
        let home = scratch("animation-conf-unreadable");
        write(&home.join(".config/hypr/hyprland.conf"), conf_cases[3].0);
        home
    }));
    assert!(unreadable.skipped.iter().any(|skip| skip.what.contains("yes, please")), "{:?}", unreadable.skipped);
    assert!(unreadable.skipped.iter().any(|skip| skip.what.contains("windows, maybe")), "{:?}", unreadable.skipped);

    // Through the whole precedence: the read lands under config.toml.
    let home = scratch("animation-precedence");
    write(&home.join(".config/hypr/hyprland.lua"), lua_cases[0].0);
    let live = || Some(read(&Roots::under(&home)));
    let config = crate::parse_with("desktop = \"omarchy\"", &live).unwrap();
    assert!(!config.motion.enabled);
    assert_eq!(config.provenance.get("motion").map(String::as_str), Some("live Hyprland config"));
    let config = crate::parse_with("desktop = \"omarchy\"\n[motion]\nenabled = true\nspeed = 2\n", &live).unwrap();
    assert_eq!(config.motion, wm_core::MotionPolicy { speed: 2.0, ..Default::default() });
    assert_eq!(config.provenance.get("motion").map(String::as_str), Some("config file"));
}

#[test]
fn the_cursor_table_carries_when_the_pointer_hides_and_declines_warps() {
    let lua = scratch("cursor-table-lua");
    write(
        &lua.join(".config/hypr/hyprland.lua"),
        r#"
hl.config({
  general = { gaps_in = 5 },
  cursor = {
    hide_on_key_press = true, hide_on_touch = true, inactive_timeout = 2.5,
    warp_on_change_workspace = 1, zoom_factor = 2,
  },
})
"#,
    );
    let conf = scratch("cursor-table-conf");
    write(
        &conf.join(".config/hypr/hyprland.conf"),
        concat!(
            "cursor {\n",
            "    hide_on_key_press = true\n",
            "    hide_on_touch = yes\n",
            "    inactive_timeout = 2.5\n",
            "    warp_on_change_workspace = 1\n",
            "    zoom_factor = 2\n",
            "}\n",
            "general {\n",
            "    gaps_in = 5\n",
            "}\n",
        ),
    );
    for root in [lua, conf] {
        let reading = read(&Roots::under(&root));
        assert_eq!(
            reading.input.cursor,
            wm_core::CursorBehaviour {
                hide_on_key_press: Some(true),
                hide_on_touch: Some(true),
                inactive_timeout: Some(2.5),
            },
            "{root:?}: {:?}",
            reading.skipped
        );
        assert!(
            skipped_why(&reading, "warp_on_change_workspace").is_some_and(|why| why.contains("never warps")),
            "{root:?}: a warp key must be declined by name: {:?}",
            reading.skipped
        );
        assert!(
            skipped_why(&reading, "zoom_factor").is_some_and(|why| why.contains("not implemented")),
            "{root:?}: {:?}",
            reading.skipped
        );
        assert!(
            reading.skipped.iter().any(|skip| skip.what.contains("general") || skip.what.contains("outside input, cursor, binds, animations, decoration")),
            "{root:?}: the rest of the configuration is still reported: {:?}",
            reading.skipped
        );
        let config = crate::parse_with("desktop = \"omarchy\"", &|| Some(read(&Roots::under(&root)))).unwrap();
        assert_eq!(config.input.cursor.hide_on_key_press, Some(true), "{root:?}");
    }
}

#[test]
fn cursor_values_are_validated_before_they_are_carried() {
    for (name, value) in [
        ("hide_on_key_press", "maybe"),
        ("inactive_timeout", "-1"),
        ("inactive_timeout", "NaN"),
        ("inactive_timeout", "inf"),
        ("inactive_timeout", "soon"),
    ] {
        let mut reading = Reading::default();
        cursor(&mut reading, name, value);
        assert_eq!(reading.input.cursor, wm_core::CursorBehaviour::default(), "{name} = {value}");
        assert_eq!(reading.skipped.len(), 1, "{name} = {value}");
    }
    let mut reading = Reading::default();
    cursor(&mut reading, "INACTIVE_TIMEOUT", "0");
    cursor(&mut reading, "hide_on_touch", "off");
    assert_eq!(reading.input.cursor.inactive_timeout, Some(0.0), "zero is a valid never");
    assert_eq!(reading.input.cursor.hide_on_touch, Some(false));
    assert!(reading.skipped.is_empty(), "{:?}", reading.skipped);
}

#[test]
fn keyboard_repeat_accepts_zero_and_rejects_out_of_range_values() {
    for value in [0, 1, 1000] {
        let mut reading = Reading::default();
        input(&mut reading, "repeat_rate", &value.to_string());
        assert_eq!(reading.input.repeat_rate, Some(value));
        assert!(reading.skipped.is_empty());
    }
    for value in [0, 1, 5000] {
        let mut reading = Reading::default();
        input(&mut reading, "repeat_delay", &value.to_string());
        assert_eq!(reading.input.repeat_delay, Some(value));
        assert!(reading.skipped.is_empty());
    }
    for (name, value) in [("repeat_rate", "-1"), ("repeat_rate", "1001"), ("repeat_delay", "-1"), ("repeat_delay", "5001")] {
        let mut reading = Reading::default();
        input(&mut reading, name, value);
        assert_eq!(reading.input.repeat_rate, None);
        assert_eq!(reading.input.repeat_delay, None);
        assert_eq!(reading.skipped.len(), 1);
    }
}

#[test]
fn keyboard_only_configuration_is_usable_without_replacing_default_bindings() {
    let root = scratch("keyboard-only");
    write(&root.join(".config/hypr/hyprland.conf"), "input {\n kb_layout = de\n repeat_rate = 0\n repeat_delay = 0\n}\n");
    let reading = read(&Roots::under(&root));
    assert!(!reading.is_empty(), "input settings must not be discarded because no bind was present");
    let mut config = crate::Config::default_config();
    let bindings = config.keybindings.clone();
    apply(&mut config, Some(&reading));
    assert_eq!(config.keybindings, bindings, "the default escape hatch stays available");
    assert_eq!(config.input.layout.as_deref(), Some("de"));
    assert_eq!(config.input.repeat_rate, Some(0));
    assert_eq!(config.input.repeat_delay, Some(0));
}

/// A value only running code could give is not a value this reader has,
/// and it must never reach xkb, a window rule or a monitor line as the
/// source text of the expression. Omarchy 4 computes its layout as
/// `vconsole.XKBLAYOUT or "us"`, and handing libxkbcommon those words
/// cost every stock session its keymap and its options.
#[test]
fn a_value_computed_at_runtime_is_refused_rather_than_rendered() {
    let out = lua_out(&[concat!(
        "x = y or \"z\"\n",
        "hl.config({ input = { kb_layout = x, touchpad = { natural_scroll = x } }, cursor = { hide_on_key_press = x } })\n",
        "o.window(\"foot\", { size = x })\n",
        "o.window(x, { float = true })\n",
        "hl.window_rule({ match = { class = x }, float = true })\n",
        "hl.monitor({ output = \"DP-1\", mode = x })\n",
        "hl.monitor({ output = \"DP-2\", mode = \"preferred\", transform = x })\n",
    )]);
    for directive in &out {
        assert!(
            !matches!(
                directive,
                Directive::Input { .. } | Directive::Cursor { .. } | Directive::WindowRule(_) | Directive::Monitor(_)
            ),
            "built from a value only running code could give: {directive:?}"
        );
    }
    for name in ["kb_layout", "natural_scroll", "hide_on_key_press", "size", "class", "mode", "transform"] {
        assert!(
            out.iter().any(|d| matches!(d, Directive::Ignored { detail, .. } if detail.contains(name))),
            "{name} was not refused by name: {out:?}"
        );
    }
}

/// Omarchy 4's stock `input.lua`, off the captured machine: the layout
/// and variant are computed at runtime, so they are left unset and the
/// read says why, while the literal model and options still arrive.
#[test]
fn omarchys_runtime_keyboard_layout_is_left_unset_rather_than_named_as_text() {
    let reading = read(&machine());
    assert_eq!(reading.input.layout, None, "{:?}", reading.input);
    assert_eq!(reading.input.variant, None);
    assert_eq!(reading.input.model.as_deref(), Some(""));
    assert_eq!(
        reading.input.options.as_deref(),
        Some("compose:caps,shift:both_capslock_cancel")
    );
    assert!(
        reading.skipped.iter().any(|skip| skip.what.contains("kb_layout")),
        "the unset layout must be explained: {:?}",
        reading.skipped
    );
}

/// ...and the system's own keyboard configuration stands in for what
/// the read leaves unset. `/etc/vconsole.conf` is the file `localectl`
/// writes and the one Omarchy's `input.lua` reads. The Lua is Omarchy
/// 4's `default/hypr/input.lua`, the lines that compute the keyboard,
/// verbatim.
#[test]
fn the_systems_keyboard_configuration_fills_what_the_read_leaves_unset() {
    const INPUT_LUA: &str = r##"
local function read_vconsole()
  local values = {}
  local file = io.open("/etc/vconsole.conf", "r")
  if not file then
    return values
  end

  for line in file:lines() do
    local key, value = line:match("^%s*([%w_]+)%s*=%s*(.-)%s*$")
    if key and value then
      value = value:gsub("%s+#.*$", "")
      value = value:gsub('^"(.*)"$', "%1")
      value = value:gsub("^'(.*)'$", "%1")
      values[key] = value
    end
  end

  file:close()
  return values
end

local non_latin_layouts =
  " af am ara bd bg by et ge gr il in iq ir kg kh kz la lk mk mm mn mv np rs ru sy th tj ua "

local vconsole = read_vconsole()

local kb_layout = vconsole.XKBLAYOUT or "us"
local kb_variant = vconsole.XKBVARIANT or ""
local kb_options = "compose:caps,shift:both_capslock_cancel"

if non_latin_layouts:find(" " .. kb_layout:match("^[^,]*") .. " ", 1, true) then
  kb_layout = "us," .. kb_layout
  kb_variant = "," .. kb_variant
  -- Reach the original layout with Left Alt + Right Alt.
  kb_options = kb_options .. ",grp:alts_toggle"
end

hl.config({
  input = {
    kb_layout = kb_layout,
    kb_variant = kb_variant,
    kb_model = "",
    kb_options = kb_options,
    kb_rules = "",
  },
})
"##;
    let root = scratch("vconsole");
    write(&root.join(".config/hypr/hyprland.lua"), INPUT_LUA);
    write(
        &root.join("etc/vconsole.conf"),
        "# Written by systemd-localed(8)\nKEYMAP=de\nXKBLAYOUT=\"de\"\nXKBVARIANT='nodeadkeys'\nXKBMODEL=pc105\nXKBOPTIONS=\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.input.layout.as_deref(), Some("de"), "{:?}", reading.input);
    assert_eq!(reading.input.variant.as_deref(), Some("nodeadkeys"));
    // Omarchy's own literals win over the system's values.
    assert_eq!(reading.input.model.as_deref(), Some(""));
    assert_eq!(
        reading.input.options.as_deref(),
        Some("compose:caps,shift:both_capslock_cancel")
    );
    assert!(
        reading.skipped.iter().any(|skip| skip.what.contains("vconsole.conf")),
        "the substitution must be said: {:?}",
        reading.skipped
    );

    // A layout the configuration names outright wins too.
    write(
        &root.join(".config/hypr/hyprland.lua"),
        &format!("{INPUT_LUA}\nhl.config({{ input = {{ kb_layout = \"fr\" }} }})\n"),
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.input.layout.as_deref(), Some("fr"));
    assert_eq!(reading.input.variant.as_deref(), Some("nodeadkeys"));

    // The system file alone does not make a desk with nothing to read
    // into one that replaces its keymap.
    let _ = std::fs::remove_file(root.join(".config/hypr/hyprland.lua"));
    assert!(read(&Roots::under(&root)).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn metadata_only_is_empty_but_release_bindings_and_monitors_are_not() {
    let mut reading = Reading::default();
    reading.files.push("hyprland.conf".into());
    reading.skipped.push(Skipped { kind: "input".into(), what: "unknown".into(), why: "unsupported".into() });
    assert!(reading.is_empty());
    let root = scratch("non-press-config");
    for fragment in ["bindr = SUPER, R, workspace, 1\n", "monitor = , preferred, auto, 1.5\n"] {
        write(&root.join(".config/hypr/hyprland.conf"), fragment);
        let reading = read(&Roots::under(&root));
        assert!(!reading.is_empty(), "usable configuration discarded: {fragment}");
    }
}

/// Pointer bindings — the mouse wheel, a mouse button — are not key
/// chords and are refused by name rather than mangled into some nearby
/// keysym. The lid switch binds as a switch.
#[test]
fn pointer_bindings_are_refused_by_name_and_the_lid_switch_binds() {
    let reading = read(&machine());
    assert!(skipped_why(&reading, "mouse_down").is_some_and(|w| w.contains("pointer or switch")));
    assert!(skipped_why(&reading, "mouse:272").is_some_and(|w| w.contains("pointer or switch")));
    let shape: Vec<_> = reading.switch_bindings.iter().map(|b| (b.device.as_str(), b.edge, b.locked)).collect();
    assert_eq!(shape, [("Lid Switch", crate::SwitchEdge::On, true)], "{:?}", reading.skipped);
    assert!(
        matches!(&reading.switch_bindings[0].action, Action::Run(name)
            if reading.commands.get(name).is_some_and(|argv| argv.iter().any(|arg| arg.contains("omarchy-system-lid-close")))),
        "closing the lid runs Omarchy's lock-on-close handler: {:?}",
        reading.switch_bindings
    );
}

/// Switch bindings read the same from conf and Lua: the edge, the exact
/// device name, the locked flag, and `unbind`. A nameless switch is
/// refused by name.
#[test]
fn switch_bindings_carry_edge_device_and_lock_from_either_syntax() {
    let lua = scratch("switch-bindings-lua");
    write(
        &lua.join(".config/hypr/hyprland.lua"),
        r#"
o.bind("switch:on:Lid Switch", nil, "lock-now", { locked = true })
hl.bind("switch:off:Lid Switch", "wake-panel")
hl.bind("switch:Tablet Mode Switch", "flip")
hl.bind("switch:on:", "nameless")
hl.bind("switch:on:Gone", "gone")
hl.unbind("switch:on:Gone")
"#,
    );
    let conf = scratch("switch-bindings-conf");
    write(
        &conf.join(".config/hypr/hyprland.conf"),
        concat!(
            "bindl = , switch:on:Lid Switch, exec, lock-now\n",
            "bind = , switch:off:Lid Switch, exec, wake-panel\n",
            "bind = , switch:Tablet Mode Switch, exec, flip\n",
            "bind = , switch:on:, exec, nameless\n",
            "bind = , switch:on:Gone, exec, gone\n",
            "unbind = , switch:on:Gone\n",
        ),
    );
    for root in [lua, conf] {
        let reading = read(&Roots::under(&root));
        let shape: Vec<_> = reading.switch_bindings.iter().map(|b| (b.device.as_str(), b.edge, b.locked)).collect();
        assert_eq!(
            shape,
            [
                ("Lid Switch", crate::SwitchEdge::On, true),
                ("Lid Switch", crate::SwitchEdge::Off, false),
                ("Tablet Mode Switch", crate::SwitchEdge::Any, false),
            ],
            "{root:?}: {:?}",
            reading.skipped
        );
        assert!(
            reading.skipped.iter().any(|skip| skip.what.contains("switch:on:") && skip.why.contains("device name")),
            "{root:?}: {:?}",
            reading.skipped
        );
        assert!(reading.keybindings.is_empty(), "{root:?}: a switch is never a key chord");
        let config = crate::parse_with("desktop = \"omarchy\"", &|| Some(read(&Roots::under(&root)))).unwrap();
        assert_eq!(config.switch_bindings, reading.switch_bindings, "{root:?}");
    }
}

#[test]
fn switch_bindings_are_bounded_and_run_by_exact_name_edge_and_lock() {
    let mut reading = Reading::default();
    let run = directive::Dispatcher::Exec("true".into());
    for n in 0..=crate::SwitchBinding::MAX {
        bind(&mut reading, &format!("switch:on:Switch {n}"), None, directive::BindFlags::default(), &run);
    }
    bind(&mut reading, "switch:on:Switch 0", None, directive::BindFlags::default(), &run);
    assert_eq!(reading.switch_bindings.len(), crate::SwitchBinding::MAX, "a rebinding replaces within the bound");
    assert!(
        skipped_why(&reading, &format!("Switch {}", crate::SwitchBinding::MAX)).is_some_and(|why| why.contains("more than")),
        "{:?}",
        reading.skipped
    );
    let mut reading = Reading::default();
    bind(&mut reading, &format!("switch:{}", "x".repeat(257)), None, directive::BindFlags::default(), &run);
    assert!(reading.switch_bindings.is_empty());
    assert_eq!(reading.skipped.len(), 1);

    let binding = crate::SwitchBinding {
        device: "Lid Switch".into(),
        edge: crate::SwitchEdge::On,
        action: Action::Run("x".into()),
        locked: false,
    };
    assert!(binding.runs("Lid Switch", true, false));
    assert!(!binding.runs("Lid Switch", false, false), "the other edge");
    assert!(!binding.runs("lid switch", true, false), "names match exactly");
    assert!(!binding.runs("Lid Switch", true, true), "an unlocked binding waits out the lock");
    let locked = crate::SwitchBinding { locked: true, edge: crate::SwitchEdge::Any, ..binding };
    assert!(locked.runs("Lid Switch", true, true) && locked.runs("Lid Switch", false, true));
    assert_eq!(keys::switch_for("SWITCH:OFF:Lid Switch"), Some(("Lid Switch", crate::SwitchEdge::Off)));
    assert_eq!(keys::switch_for("SUPER + L"), None);
}

// ---- window rules -----------------------------------------------------

/// The tag indirection, which is the whole difficulty of Omarchy's
/// window rules: nothing says `float` next to a class. One rule tags
/// fifteen classes `floating-window`, and three more rules float,
/// center and size anything carrying that tag.
#[test]
fn float_rules_resolve_through_omarchys_tags() {
    let reading = read(&machine());
    let policy = reading.float_rules;
    let decision = policy
        .decision_for("org.omarchy.btop", "", "")
        .expect("Omarchy's own terminals float");
    assert_eq!(
        decision.size,
        Some(wm_core::Size::new(875, 600)),
        "the size the hardcoded rule was a transcription of"
    );
    assert!(decision.center);
    // The `.*` rules that carry no float property must not float
    // everything on the desk.
    assert_eq!(policy.decision_for("some-ordinary-app", "A window", ""), None);
}

/// `apps/browser.lua` sends Chromium's "is sharing your screen" bar to
/// the hidden special workspace: `workspace = "special silent"`. The
/// rule compiles, and through the window manager the bar maps hidden
/// and unfocused instead of taking a tile.
#[test]
fn the_screen_sharing_bar_rule_maps_the_window_hidden_and_unfocused() {
    let reading = read(&machine());
    let policy = reading.float_rules;
    let decision = policy.window_decision_for("chromium", "example.com is sharing your screen.", "");
    assert_eq!(
        decision.workspace,
        Some(wm_core::RuleWorkspace {
            target: wm_core::RuleWorkspaceTarget::Special(wm_core::DEFAULT_SPECIAL_NAME.into()),
            silent: true,
        })
    );
    assert_eq!(policy.window_decision_for("chromium", "New Tab", "").workspace, None);
    assert!(
        policy.descriptions().iter().any(|line| line.contains("workspace special:special silently")),
        "{:?}",
        policy.descriptions()
    );

    use wm_core::fake_backend::{FakeBackend, FakeTheme};
    let mut wm = wm_core::WindowManager::new(FakeBackend::new(), Box::new(FakeTheme));
    wm.set_float_policy(policy.policy());
    let ordinary = wm.backend_mut().create_window();
    wm.backend_mut().set_title(ordinary, "New Tab");
    wm.dispatch(wm_core::BackendEvent::MapRequest(ordinary));
    let ordinary = wm.client_for_window(ordinary).unwrap();
    let bar = wm.backend_mut().create_window();
    wm.backend_mut().set_title(bar, "example.com is sharing your screen.");
    wm.dispatch(wm_core::BackendEvent::MapRequest(bar));
    let bar = wm.client_for_window(bar).unwrap();
    let frame = wm.client(bar).unwrap().frame.expect("decorated");
    assert!(!wm.backend().mapped_frames.contains(&frame), "the bar is parked on the hidden special");
    assert_eq!(wm.focused_client(), Some(ordinary), "and took no focus");
    assert_eq!(wm.client(bar).unwrap().special, Some(0));
    assert_eq!(wm.special_name(0), Some(wm_core::DEFAULT_SPECIAL_NAME));
    assert!(!wm.layout_order(0).contains(&bar), "it never took a tile");
}

/// The `workspace` rule's spellings: a number, `special`,
/// `special:NAME`, each with an optional `silent`; `name:…` and the
/// relative forms are refused by name.
#[test]
fn workspace_rules_read_numbers_and_specials_and_refuse_names() {
    let root = scratch("workspace-rules");
    write(
        &root.join(".config/hypr/hyprland.lua"),
        r#"
hl.window_rule({ match = { class = "^(numbered)$" }, workspace = "3 silent" })
hl.window_rule({ match = { class = "^(followed)$" }, workspace = "2" })
hl.window_rule({ match = { class = "^(notes)$" }, workspace = "special:notes" })
hl.window_rule({ match = { class = "^(named)$" }, workspace = "name:notes silent" })
hl.window_rule({ match = { class = "^(relative)$" }, workspace = "+1" })
hl.window_rule({ match = { class = "^(loud)$" }, workspace = "special quiet" })
"#,
    );
    let reading = read(&Roots::under(&root));
    let policy = &reading.float_rules;
    let target = |class: &str| policy.window_decision_for(class, "", "").workspace;
    assert_eq!(
        target("numbered"),
        Some(wm_core::RuleWorkspace { target: wm_core::RuleWorkspaceTarget::Numbered(2), silent: true })
    );
    assert_eq!(
        target("followed"),
        Some(wm_core::RuleWorkspace { target: wm_core::RuleWorkspaceTarget::Numbered(1), silent: false })
    );
    assert_eq!(
        target("notes"),
        Some(wm_core::RuleWorkspace { target: wm_core::RuleWorkspaceTarget::Special("notes".into()), silent: false })
    );
    for (class, why) in [("named", "numbered, not named"), ("relative", "must be 1 to"), ("loud", "\"quiet\"")] {
        assert_eq!(target(class), None, "{class}");
        assert!(
            reading.skipped.iter().any(|skip| skip.what.contains(class) && skip.what.contains(why)),
            "{class}: {:?}",
            reading.skipped
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// And the reason reading them properly is worth more than the one
/// number the hardcoded rule held: Omarchy floats several classes at
/// sizes that are not 875x600, and the hardcoded rule got every one of
/// them wrong.
#[test]
fn the_sizes_the_hardcoded_rule_could_not_express_come_through() {
    let reading = read(&machine());
    let policy = reading.float_rules;
    // `apps/steam.lua`: `o.window({ class = "steam", title = "Steam" },
    // { center = true, size = { 1100, 700 } })`
    assert_eq!(
        policy.decision_for("steam", "Steam", "").and_then(|d| d.size),
        Some(wm_core::Size::new(1100, 700))
    );
    // `apps/pip.lua`: picture-in-picture, matched on *title*.
    assert_eq!(
        policy
            .decision_for("firefox", "Picture-in-Picture", "")
            .and_then(|d| d.size),
        Some(wm_core::Size::new(600, 338)),
        "a title-matched rule, through the `pip` tag"
    );
    // `apps/system.lua`: the About box has its own size.
    assert_eq!(
        policy
            .decision_for("org.omarchy.about", "", "")
            .and_then(|d| d.size),
        Some(wm_core::Size::new(920, 480))
    );
    // Hyprland requires a full match even without explicit anchors.
    assert_eq!(
        policy
            .decision_for("localsend", "", "")
            .and_then(|d| d.size),
        Some(wm_core::Size::new(1100, 700))
    );
    assert!(policy.decision_for("localsend_app", "", "").is_none());
    for title in ["Steam Big Picture Mode", "Sign in to Steam"] {
        assert_eq!(policy.decision_for("steam", title, "").and_then(|d| d.size), None,
            "the desktop size rule must not resize {title}");
    }
    assert!(policy.decision_for("steam_app_123", "Steam", "").is_none(),
        "Steam's floating rule must not turn a game's window into the launcher");
}

/// A rule this reader only half-understands is dropped whole and says
/// so, rather than applied on the half it understood — which would
/// turn "float this one XWayland window" into "float every window".
#[test]
fn a_rule_with_an_unimplemented_matcher_is_refused_whole_and_loudly() {
    let rule = directive::WindowRule {
        matchers: vec![
            directive::Matcher::Class("^$".into()),
            directive::Matcher::Other {
                key: "xwayland".into(),
                value: "1".into(),
            },
        ],
        props: vec![("float".into(), "on".into())],
    };
    let (rules, notes) = rules::compile(&[rule]);
    assert!(rules.is_empty(), "half a rule must not be applied");
    assert!(
        notes.iter().any(|n| n.contains("match:xwayland 1")),
        "and it must say which matcher: {notes:?}"
    );
}

/// A float rule naming a tag nothing adds cannot be resolved, and says
/// which tag rather than silently matching nothing.
#[test]
fn a_float_rule_on_an_unknown_tag_names_the_tag() {
    let rule = directive::WindowRule {
        matchers: vec![directive::Matcher::Tag("ghost".into())],
        props: vec![("float".into(), "on".into())],
    };
    let (rules, notes) = rules::compile(&[rule]);
    assert!(rules.is_empty());
    assert!(notes.iter().any(|n| n.contains("ghost")), "{notes:?}");
}

// ---- layout expressions -----------------------------------------------

/// A monitor to evaluate against, and no window of note.
fn on_1080p() -> expr::Values {
    expr::Values { monitor_w: 1920.0, monitor_h: 1080.0, window_w: 600.0, window_h: 338.0 }
}

fn eval(source: &str) -> Option<f64> {
    expr::Expr::parse(source).unwrap_or_else(|why| panic!("{source:?}: {why}")).eval(&on_1080p())
}

#[test]
fn expressions_follow_arithmetic_precedence_and_associativity() {
    assert_eq!(eval("2+3*4"), Some(14.0));
    assert_eq!(eval("2*3+4"), Some(10.0));
    assert_eq!(eval("10-4-3"), Some(3.0), "subtraction is left-associative");
    assert_eq!(eval("8/2/2"), Some(2.0), "so is division");
    assert_eq!(eval("1+8/2*3"), Some(13.0));
    assert_eq!(eval("0.04*100"), Some(4.0));
}

#[test]
fn unary_minus_binds_tighter_than_the_binary_operators() {
    assert_eq!(eval("-5"), Some(-5.0));
    assert_eq!(eval("--5"), Some(5.0));
    assert_eq!(eval("-2*3"), Some(-6.0));
    assert_eq!(eval("2*-3"), Some(-6.0));
    assert_eq!(eval("-(2+3)"), Some(-5.0));
    assert_eq!(eval("10--5"), Some(15.0));
    assert_eq!(eval("-monitor_w+10"), Some(-1910.0));
}

#[test]
fn parentheses_group() {
    assert_eq!(eval("(2+3)*4"), Some(20.0));
    assert_eq!(eval("((2+3))*(4)"), Some(20.0));
    assert_eq!(eval("2*(3+(4*(5-1)))"), Some(38.0));
    assert!(expr::Expr::parse("(2+3").is_err(), "an unclosed parenthesis is refused");
    assert!(expr::Expr::parse("2+3)").is_err(), "and so is an unopened one");
}

#[test]
fn each_variable_reads_its_own_value() {
    assert_eq!(eval("monitor_w"), Some(1920.0));
    assert_eq!(eval("monitor_h"), Some(1080.0));
    assert_eq!(eval("window_w"), Some(600.0));
    assert_eq!(eval("window_h"), Some(338.0));
    // Omarchy's own expressions, verbatim.
    assert_eq!(eval("(monitor_w-window_w-40)"), Some(1280.0));
    assert_eq!(eval("(monitor_h*0.04)"), Some(43.2));
    assert_eq!(eval("(monitor_h-window_h-40)"), Some(702.0));
    assert_eq!(eval("(monitor_h*4/25)"), Some(172.8));
    assert_eq!(eval("(monitor_w-monitor_h*4/25-40)"), Some(1707.2));
    for unknown in ["monitor_x", "w", "cursor", "monitor_w2", "Monitor_W", "100%"] {
        assert!(expr::Expr::parse(unknown).is_err(), "{unknown:?} is not a variable");
    }
}

#[test]
fn a_result_that_is_not_a_number_is_no_result() {
    assert_eq!(eval("1/0"), None, "division by zero");
    assert_eq!(eval("-1/0"), None);
    assert_eq!(eval("0/0"), None);
    assert_eq!(eval("(1/0)*0"), None, "and it does not come back");
    assert_eq!(eval("monitor_w/(window_w-600)"), None);
    assert_eq!(eval("1/0.5"), Some(2.0), "a fraction is not zero");
}

#[test]
fn malformed_expressions_are_refused_with_a_reason() {
    for bad in ["", "2+", "+2", "*3", "2 3", "2..3", "(", ")", "monitor_w monitor_h", "3x", "1e3"] {
        assert!(expr::Expr::parse(bad).is_err(), "{bad:?} must not parse");
    }
}

#[test]
fn an_expression_over_the_length_bound_is_refused() {
    // `1+1+…+1`, negated and parenthesised to land exactly on the bound.
    let flat = |terms: usize| format!("1{}", "+1".repeat(terms));
    let at_bound = format!("-({})", flat(62));
    assert_eq!(at_bound.len(), expr::MAX_SOURCE_BYTES);
    assert_eq!(expr::Expr::parse(&at_bound).map(|e| e.eval(&on_1080p())), Ok(Some(-63.0)), "the bound is inclusive");
    let over = flat(64);
    assert_eq!(over.len(), expr::MAX_SOURCE_BYTES + 1);
    let why = expr::Expr::parse(&over).unwrap_err();
    assert!(why.contains("longer than"), "{why}");
}

#[test]
fn an_expression_nested_over_the_depth_bound_is_refused() {
    let nested = |depth: usize| format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
    assert_eq!(
        expr::Expr::parse(&nested(expr::MAX_DEPTH)).map(|e| e.eval(&on_1080p())),
        Ok(Some(1.0)),
        "the bound is inclusive"
    );
    let why = expr::Expr::parse(&nested(expr::MAX_DEPTH + 1)).unwrap_err();
    assert!(why.contains("deeper than"), "{why}");
    // A run of minus signs nests the same way and is bounded the same way.
    assert!(expr::Expr::parse(&format!("{}1", "-".repeat(expr::MAX_DEPTH))).is_ok());
    assert!(expr::Expr::parse(&format!("{}1", "-".repeat(expr::MAX_DEPTH + 1))).is_err());
    // And a long flat chain is not deep at all.
    assert!(expr::Expr::parse(&format!("1{}", "+1".repeat(50))).is_ok());
}

/// The monitor Omarchy's fixtures are written against, with a
/// client-drawn window that asked for a size the rules override.
fn metrics_1080p() -> wm_core::RuleMetrics {
    wm_core::RuleMetrics {
        monitor: wm_core::Size::new(1920, 1080),
        window: wm_core::Size::new(640, 360),
        chrome: wm_core::Size::new(0, 0),
    }
}

/// `size` given as a layout expression is evaluated when the window
/// maps, against the monitor it maps on.
#[test]
fn a_size_expression_is_evaluated_against_the_monitor() {
    let rule = directive::WindowRule {
        matchers: vec![directive::Matcher::Class("^WebcamOverlay-small$".into())],
        props: vec![("size".into(), "(monitor_h*4/25) (monitor_h*9/50)".into())],
    };
    let (rules, notes) = rules::compile(&[rule]);
    assert!(notes.is_empty(), "{notes:?}");
    // Without a monitor there is no size — and no guess.
    let decision = rules.decision_for("WebcamOverlay-small", "", "").expect("a sized window floats");
    assert_eq!(decision.size, None);
    let placement = rules.placement_for("WebcamOverlay-small", "", "", &metrics_1080p()).expect("the rule matches");
    assert_eq!(placement.size, Some(wm_core::Size::new(173, 194)), "round(172.8) x round(194.4)");
    assert_eq!(placement.position, None);
    assert!(
        rules.descriptions()[0].contains("size (monitor_h*4/25) (monitor_h*9/50)"),
        "{:?}",
        rules.descriptions()
    );
}

/// `move` is evaluated after `size`, with the size the rule resolved
/// as `window_w`/`window_h` — which is how Omarchy's picture-in-picture
/// rule reaches the corner.
#[test]
fn a_move_expression_sees_the_rules_own_size() {
    let rule = directive::WindowRule {
        matchers: vec![directive::Matcher::Title("^pip$".into())],
        props: vec![
            ("float".into(), "on".into()),
            ("size".into(), "600 338".into()),
            ("move".into(), "(monitor_w-window_w-40) (monitor_h*0.04)".into()),
        ],
    };
    let (rules, notes) = rules::compile(&[rule]);
    assert!(notes.is_empty(), "{notes:?}");
    let placement = rules.placement_for("firefox", "pip", "", &metrics_1080p()).unwrap();
    assert_eq!(placement.size, Some(wm_core::Size::new(600, 338)));
    assert_eq!(placement.position, Some(wm_core::Point::new(1280, 43)));
    // A framed window's visual size includes its chrome, so the same
    // rule keeps the frame, not the content, 40 from the edge.
    let framed = wm_core::RuleMetrics { chrome: wm_core::Size::new(2, 24), ..metrics_1080p() };
    let placement = rules.placement_for("firefox", "pip", "", &framed).unwrap();
    assert_eq!(placement.position, Some(wm_core::Point::new(1278, 43)));
    // The scale-2 desk is measured in the same logical pixels, so the
    // answer is the same; the window manager scales it back.
    assert_eq!(rules.decision_for("firefox", "pip", "").and_then(|d| d.size), Some(wm_core::Size::new(600, 338)));
}

/// A `move` with no `size` positions the client's own size, and an
/// expression that comes out non-finite drops only its own property.
#[test]
fn a_move_without_a_size_places_the_clients_own_size() {
    let rule = directive::WindowRule {
        matchers: vec![directive::Matcher::Class("^cam$".into())],
        props: vec![("float".into(), "on".into()), ("move".into(), "(monitor_w-window_w) (monitor_h-window_h)".into())],
    };
    let (rules, notes) = rules::compile(&[rule]);
    assert!(notes.is_empty(), "{notes:?}");
    let placement = rules.placement_for("cam", "", "", &metrics_1080p()).unwrap();
    assert_eq!(placement.size, None);
    assert_eq!(placement.position, Some(wm_core::Point::new(1280, 720)));

    let broken = directive::WindowRule {
        matchers: vec![directive::Matcher::Class("^cam$".into())],
        props: vec![
            ("float".into(), "on".into()),
            ("size".into(), "(monitor_w/0) 100".into()),
            ("move".into(), "(monitor_w/(window_w-window_w)) 0".into()),
        ],
    };
    let (rules, notes) = rules::compile(&[broken]);
    assert!(notes.is_empty(), "a division by zero is a map-time fact, not a parse error: {notes:?}");
    let placement = rules.placement_for("cam", "", "", &metrics_1080p()).unwrap();
    assert_eq!(placement, wm_core::RulePlacement { size: None, position: None }, "both drop, the float stays");
}

/// What this reader refuses to guess at: Hyprland's other `move`
/// spellings, a size that is not a size, and a third value.
#[test]
fn unreadable_sizes_and_moves_are_reported_by_property() {
    for (name, value, why) in [
        ("move", "cursor 0", "\"cursor\""),
        ("move", "onscreen", "exactly two values"),
        ("move", "cursor 0 0", "exactly two values"),
        ("move", "100%-w-40 0", "\"100%-w-40\""),
        ("size", "50% 50%", "\"50%\""),
        ("size", "0 0", "not a positive size"),
        ("size", "-600 338", "not a positive size"),
        ("size", "600 338 1", "exactly two values"),
    ] {
        let rule = directive::WindowRule {
            matchers: vec![directive::Matcher::Class("^x$".into())],
            props: vec![("float".into(), "on".into()), (name.into(), value.into())],
        };
        let (rules, notes) = rules::compile(&[rule]);
        assert!(
            notes.iter().any(|n| n.contains(&format!("window rule {name} {value:?}")) && n.contains(why)),
            "{name} {value:?}: {notes:?}"
        );
        assert!(notes.iter().all(|n| n.contains("property skipped")), "{notes:?}");
        let placement = rules.placement_for("x", "", "", &metrics_1080p()).expect("the float survives");
        assert_eq!(placement, wm_core::RulePlacement::default());
    }
    // Over-long and over-deep text is refused at compile time, with the reason.
    let rule = directive::WindowRule {
        matchers: vec![directive::Matcher::Class("^x$".into())],
        props: vec![("move".into(), format!("{}1{} 0", "(".repeat(17), ")".repeat(17)))],
    };
    let (_, notes) = rules::compile(&[rule]);
    assert!(notes.iter().any(|n| n.contains("deeper than 16 levels")), "{notes:?}");
}

/// `match:xdg_tag` reads the window's `xdg_toplevel_tag_v1` tag, the
/// third leg of the identity a rule sees at map time, in every
/// spelling Hyprland has used for it: `xdgTag:` in the v2 form and
/// `match:xdg_tag` in the 0.53 keyword form. A regular expression like
/// the class and title matchers, anchored the same way, so `probe-main`
/// does not float `probe-main-2`; and a tag rule can carry a tag for
/// another rule to read, like any other direct matcher.
#[test]
fn xdg_tag_rules_match_the_toplevel_tag_at_map_time() {
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        concat!(
            "windowrule = float on, match:xdg_tag ^probe-main$\n",
            "windowrule = size 640 480, match:class ^probe$, match:xdg_tag ^probe-(main|aux)$\n",
            "windowrulev2 = pin, xdgTag:^pinned$\n",
            "windowrule = tag +quake, match:xdg_tag ^quake$\n",
            "windowrule = workspace special, match:tag quake\n",
        ),
        &mut vars,
        &mut out,
    );
    let rules: Vec<directive::WindowRule> = out
        .into_iter()
        .filter_map(|d| match d {
            Directive::WindowRule(rule) => Some(rule),
            _ => None,
        })
        .collect();
    let (compiled, notes) = rules::compile(&rules);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        compiled.decision_for("probe", "", "probe-main"),
        Some(wm_core::FloatDecision { size: Some(wm_core::Size::new(640, 480)), center: true }),
        "both rules match a tagged probe window"
    );
    assert_eq!(
        compiled.decision_for("other", "", "probe-main").and_then(|d| d.size),
        None,
        "the class matcher beside the tag still constrains the size rule"
    );
    assert!(compiled.decision_for("probe", "", "probe-main-2").is_none(), "the tag is matched whole");
    assert!(compiled.decision_for("probe", "", "").is_none(), "an untagged window matches no tag rule");
    assert!(compiled.decision_for("probe", "probe-main", "").is_none(), "the tag is not the title");
    assert!(compiled.window_decision_for("any", "", "pinned").pin, "the v2 xdgTag: spelling");
    assert!(!compiled.window_decision_for("any", "", "pinned-2").pin);
    assert_eq!(
        compiled.window_decision_for("term", "", "quake").workspace,
        Some(wm_core::RuleWorkspace { target: wm_core::RuleWorkspaceTarget::Special("special".into()), silent: false }),
        "a tag carried by xdg tag resolves like one carried by class"
    );
    assert_eq!(compiled.window_decision_for("term", "", "").workspace, None);
    assert!(
        compiled.descriptions().iter().any(|line| line.starts_with("xdg_tag ^probe-main$ -> float")),
        "{:?}",
        compiled.descriptions()
    );
}

/// The same matcher from Lua: `match = { xdg_tag = … }`, as Hyprland's
/// `hl.window_rule` spells it.
#[test]
fn xdg_tag_rules_are_read_from_lua() {
    let root = scratch("xdg-tag-rules");
    write(
        &root.join(".config/hypr/hyprland.lua"),
        r#"
hl.window_rule({ match = { xdg_tag = "^probe-main$" }, float = true, size = { 875, 600 } })
hl.window_rule({ match = { class = "^probe$", xdg_tag = "^quake$" }, pin = true })
"#,
    );
    let reading = read(&Roots::under(&root));
    let policy = &reading.float_rules;
    assert_eq!(
        policy.decision_for("probe", "", "probe-main").and_then(|d| d.size),
        Some(wm_core::Size::new(875, 600))
    );
    assert!(policy.decision_for("probe", "", "other").is_none());
    assert!(policy.window_decision_for("probe", "", "quake").pin);
    assert!(!policy.window_decision_for("other", "", "quake").pin, "class and tag both constrain");
    assert!(!policy.window_decision_for("probe", "", "").pin);
    assert!(reading.skipped.is_empty(), "{:?}", reading.skipped);
    let _ = std::fs::remove_dir_all(&root);
}

/// A rule that floats and centres, with no expression anywhere, is
/// placed exactly as before: no size, no position, centred.
#[test]
fn a_plain_float_and_center_rule_still_centers() {
    let rule = directive::WindowRule {
        matchers: vec![directive::Matcher::Class("^about$".into())],
        props: vec![("float".into(), "on".into()), ("center".into(), "on".into())],
    };
    let (rules, notes) = rules::compile(&[rule]);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(rules.decision_for("about", "", ""), Some(wm_core::FloatDecision { size: None, center: true }));
    assert_eq!(rules.placement_for("about", "", "", &metrics_1080p()), Some(wm_core::RulePlacement::default()));
    // An explicit `center` is the stronger statement: a `move` beside it
    // does not win.
    let both = directive::WindowRule {
        matchers: vec![directive::Matcher::Class("^about$".into())],
        props: vec![("float".into(), "on".into()), ("move".into(), "0 0".into()), ("center".into(), "on".into())],
    };
    let (rules, _) = rules::compile(&[both]);
    assert_eq!(rules.placement_for("about", "", "", &metrics_1080p()), Some(wm_core::RulePlacement::default()));
    // And a policy that does not float says nothing.
    assert_eq!(rules.placement_for("other", "", "", &metrics_1080p()), None);
}

/// Omarchy's real `pip.lua` and `webcam-overlay.lua`, through the
/// whole machine read: every `size` and `move` in them is read, and the
/// windows land where the file says.
#[test]
fn omarchys_pip_and_webcam_overlay_rules_are_read_whole() {
    let reading = read(&machine());
    let notes: Vec<&str> = reading
        .skipped
        .iter()
        .filter(|s| s.kind == "window-rule")
        .map(|s| s.what.as_str())
        .collect();
    for forbidden in ["size ignored", "property move", "needs a monitor", "property size", "window rule size", "window rule move"] {
        assert!(
            !notes.iter().any(|n| n.contains(forbidden)),
            "{forbidden:?} must no longer be reported: {notes:?}"
        );
    }
    let policy = reading.float_rules;
    let metrics = metrics_1080p();

    // `apps/pip.lua`: a title-matched rule through the `pip` tag.
    let pip = policy.placement_for("firefox", "Picture-in-Picture", "", &metrics).expect("pip floats");
    assert_eq!(pip.size, Some(wm_core::Size::new(600, 338)));
    assert_eq!(pip.position, Some(wm_core::Point::new(1280, 43)), "top right, 40 in, 4% down");
    // Google Meet's variant, through the `chromium-based-browser` tag
    // plus its own title: bottom right.
    let meet = policy.placement_for("chromium", "Meet - abc-defg-hij", "", &metrics).expect("meet floats");
    assert_eq!(meet.size, Some(wm_core::Size::new(600, 338)));
    assert_eq!(meet.position, Some(wm_core::Point::new(1280, 702)));

    // `apps/webcam-overlay.lua`: sized off the monitor height.
    let small = policy.placement_for("WebcamOverlay-small", "WebcamOverlay", "", &metrics).expect("small floats");
    assert_eq!(small.size, Some(wm_core::Size::new(173, 194)));
    assert_eq!(small.position, Some(wm_core::Point::new(1707, 846)));
    let medium = policy.placement_for("WebcamOverlay-medium", "WebcamOverlay", "", &metrics).expect("medium floats");
    assert_eq!(medium.size, Some(wm_core::Size::new(240, 270)));
    assert_eq!(medium.position, Some(wm_core::Point::new(1640, 770)));
    let large = policy.placement_for("WebcamOverlay-large", "WebcamOverlay", "", &metrics).expect("large floats");
    assert_eq!(large.size, Some(wm_core::Size::new(324, 365)), "round(364.5) rounds away from zero");
    assert_eq!(large.position, Some(wm_core::Point::new(1556, 676)), "round(675.5)");
    // The overlay's own rule still says what it said.
    let decision = policy.window_decision_for("WebcamOverlay-small", "WebcamOverlay", "");
    assert!(decision.pin && decision.no_initial_focus);
}

// ---- autostart and environment ----------------------------------------

/// Omarchy's whole `autostart.lua` is one
/// `hl.on("hyprland.start", function() … end)`, so a reader that threw
/// function bodies away would find no autostart at all.
#[test]
fn autostart_comes_out_of_the_start_handler() {
    let reading = read(&machine());
    let flat: Vec<String> = reading
        .autostart
        .iter()
        .map(|argv| argv.join(" "))
        .collect();
    assert!(flat.iter().any(|c| c.contains("udiskie")), "{flat:?}");
    assert!(
        flat.iter()
            .any(|c| c.contains("omarchy-powerprofiles-init")),
        "{flat:?}"
    );
    // A line with shell grammar in it keeps its shell rather than
    // being mis-split into argv.
    assert!(
        flat.iter()
            .any(|c| c.starts_with("bash -lc") && c.contains("post-boot")),
        "{flat:?}"
    );
}

/// Two autostart entries must not be carried: one is Omarchy's monitor
/// watcher, whose display toggles need output disable, and one is a
/// second copy of the shell this desktop already starts.
#[test]
fn autostart_refuses_the_two_things_that_would_be_worse_than_nothing() {
    let reading = read(&machine());
    let flat: Vec<String> = reading
        .autostart
        .iter()
        .map(|argv| argv.join(" "))
        .collect();
    assert!(
        !flat
            .iter()
            .any(|c| c.contains("omarchy-hyprland-monitor-watch")),
        "commands Hyprland: {flat:?}"
    );
    assert_eq!(
        skipped_why(&reading, "omarchy-hyprland-monitor-watch"),
        Some(crate::preset::Unbound::OUTPUT_DISABLE.reason().to_string())
    );
    assert!(
        !flat.iter().any(|c| c.contains("omarchy-launch-shell")),
        "we start the shell ourselves: {flat:?}"
    );
    assert!(
        skipped_why(&reading, "omarchy-launch-shell").is_some_and(|w| w.contains("second copy"))
    );
}

#[test]
fn blanket_activation_environment_imports_are_never_autostarted() {
    let root = scratch("activation-import");
    write(
        &root.join(".config/hypr/hyprland.lua"),
        concat!(
            "hl.on(\"hyprland.start\", function()\n",
            "  hl.exec_cmd(\"systemctl --user import-environment $(env | cut -d'=' -f 1)\")\n",
            "  hl.exec_cmd(\"dbus-update-activation-environment --systemd --all\")\n",
            "  hl.exec_cmd(\"safe-program --flag\")\n",
            "end)\n",
        ),
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(
        reading.autostart,
        vec![vec!["safe-program".to_string(), "--flag".to_string()]]
    );
    assert_eq!(
        reading
            .skipped
            .iter()
            .filter(|skip| skip.why.contains("blanket activation-environment"))
            .count(),
        2
    );
}

/// The environment carries the guest desktop's own expectations, minus
/// the two variables that would be lies here.
#[test]
fn the_environment_is_carried_except_where_it_would_lie() {
    let reading = read(&machine());
    let names: Vec<&str> = reading.env.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"GDK_BACKEND"), "{names:?}");
    assert!(names.contains(&"MOZ_ENABLE_WAYLAND"), "{names:?}");
    assert!(names.contains(&"XCURSOR_SIZE"), "{names:?}");
    assert!(
        !names.contains(&"XDG_CURRENT_DESKTOP"),
        "naming Hyprland as the running desktop would break portals"
    );
    assert!(!names.contains(&"XDG_SESSION_DESKTOP"), "{names:?}");
    assert!(skipped_why(&reading, "XDG_CURRENT_DESKTOP")
        .is_some_and(|w| w.contains("under chonkstep it is not")));
}

/// `monitor =` lines are read and kept, so they can be reported —
/// and are not applied. See `Monitors` for the argument.
#[test]
fn monitor_lines_are_read_and_reported_rather_than_applied() {
    let reading = read(&conf_machine());
    assert!(
        reading
            .monitors
            .lines
            .iter()
            .any(|m| m.output == "DP-2" && m.scale == "1.5"),
        "{:?}",
        reading.monitors.lines
    );
    assert!(
        reading.monitors.lines.iter().any(|m| m.output.is_empty()),
        "the catch-all `monitor=,preferred,auto,auto` line too"
    );
}

/// `wm-wayland`'s `monitor_transform()` recognizes `extra == ["transform",
/// "N"]` — the shape the conf front end produces by splitting
/// `monitor = …, transform, N` on commas (`conf.rs`'s
/// `fields.iter().skip(4)`). The Lua front end has to produce the exact
/// same two-token shape for `hl.monitor({ …, transform = N })`, or every
/// Lua-configured rotation is silently refused as an unsupported field —
/// which is exactly what happened on real hardware before this fix.
#[test]
fn lua_monitor_transform_lowers_to_the_same_extra_shape_as_conf() {
    // Exercise both parsers, including unsupported extra fields: parity
    // must not accidentally make a partially supported monitor rule valid.
    for (lua_extra, conf_extra) in [
        ("transform = 0", "transform, 0"),
        ("transform = 1", "transform, 1"),
        ("transform = 2", "transform, 2"),
        ("transform = 3", "transform, 3"),
        ("transform = 4", "transform, 4"),
        ("transform = 3, bitdepth = 10", "transform, 3, bitdepth, 10"),
        ("mirror = 'DP-1'", "mirror, DP-1"),
    ] {
        let mut globals = lua::Globals::default();
        let mut lua_out = Vec::new();
        lua::read(
            &format!("hl.monitor({{ output = 'DP-2', mode = 'preferred', position = 'auto', scale = 2, {lua_extra} }})"),
            &lua::Facts { path: Vec::new(), home: None, state_home: None },
            &mut globals,
            &mut lua_out,
        );
        let mut conf_out = Vec::new();
        conf::read(
            &format!("monitor = DP-2, preferred, auto, 2, {conf_extra}"),
            &mut Default::default(),
            &mut conf_out,
        );
        let monitor = |out: Vec<Directive>| {
            out.into_iter().find_map(|directive| match directive {
                Directive::Monitor(monitor) => Some(monitor),
                _ => None,
            }).expect("each parser must emit a Monitor directive")
        };
        assert_eq!(monitor(lua_out), monitor(conf_out), "{lua_extra}");
    }
}

// ---- workspace layouts ------------------------------------------------

/// The Lua and conf spellings of the default layout and of a workspace
/// rule lower onto the same two directives, still in Hyprland's words.
#[test]
fn the_layout_name_lowers_to_the_same_directives_from_both_syntaxes() {
    let conf = |source: &str| {
        let mut out = Vec::new();
        conf::read(source, &mut Default::default(), &mut out);
        out
    };
    let default_layout = Directive::DefaultLayout { layout: "dwindle".into() };
    assert!(lua_out(&["hl.config({ general = { gaps_in = 5, layout = \"dwindle\" } })\n"]).contains(&default_layout));
    assert!(conf("general {\n  gaps_in = 5\n  layout = dwindle\n}\n").contains(&default_layout));
    assert!(conf("general:layout = dwindle\n").contains(&default_layout));
    let rule = Directive::WorkspaceLayout { workspace: 2, layout: "scrolling".into() };
    assert!(lua_out(&["hl.workspace_rule({ workspace = \"2\", layout = \"scrolling\" })\n"]).contains(&rule));
    assert!(lua_out(&["hl.workspace_rule({ workspace = 2, layout = \"scrolling\" })\n"]).contains(&rule));
    assert!(conf("workspace = 2, layout:scrolling\n").contains(&rule));
}

/// Omarchy's shipped `looknfeel` sets `general.layout = "dwindle"` in
/// both syntaxes, and that is the one layout setting that is not a
/// look: it is why an Omarchy desktop tiles. Both captured machines
/// read it as Mosaic, and it reaches the config.
#[test]
fn omarchys_default_layout_reads_as_mosaic_from_both_machines() {
    for roots in [machine(), conf_machine()] {
        let reading = read(&roots);
        assert_eq!(reading.default_layout, Some(wm_core::LayoutMode::Mosaic), "{:?}", reading.skipped);
        assert!(reading.workspace_layouts.is_empty(), "the captured machine saved no workspace layouts");
        let config = crate::parse_with("desktop = \"omarchy\"", &|| Some(read(&roots))).unwrap();
        assert_eq!(config.default_layout, Some(wm_core::LayoutMode::Mosaic));
        assert_eq!(config.provenance.get("default_layout").map(String::as_str), Some("live Hyprland config"));
    }
    let config = crate::parse_with("desktop = \"omarchy\"", &|| None).unwrap();
    assert_eq!(config.default_layout, None, "nothing to read leaves the built-in Freeform default");
}

/// The files Omarchy's own `omarchy-hyprland-workspace-layout-toggle`
/// saves — one `hl.workspace_rule` per workspace under
/// `~/.local/state/omarchy/workspace-layouts/` — are reached through
/// `toggles.lua`'s `require_all` and read as that workspace's style.
/// Every key but the two that matter, every selector that is not a
/// numbered workspace, and every layout this desktop has no style for
/// is reported on its own line; no call vanishes silently.
#[test]
fn saved_workspace_layouts_are_read_through_omarchys_own_include_chain() {
    let home = scratch("workspace-layouts");
    let layouts = home.join(".local/state/omarchy/workspace-layouts");
    write(&layouts.join("2.lua"), "hl.workspace_rule({ workspace = \"2\", layout = \"scrolling\" })\n");
    write(&layouts.join("3.lua"), "hl.workspace_rule({ workspace = \"3\", layout = \"dwindle\", gapsin = 0, monitor = \"DP-1\" })\n");
    write(&layouts.join("bare.lua"), "hl.workspace_rule({ workspace = \"5\" })\n");
    write(&layouts.join("far.lua"), "hl.workspace_rule({ workspace = \"150\", layout = \"scrolling\" })\n");
    write(&layouts.join("magic.lua"), "hl.workspace_rule({ workspace = \"special:magic\", layout = \"scrolling\" })\n");
    write(&layouts.join("master.lua"), "hl.workspace_rule({ workspace = \"4\", layout = \"master\" })\n");
    write(&layouts.join("nil.lua"), "hl.workspace_rule(nil)\n");
    let mut roots = machine();
    // The scratch state root stands in front of the captured one, so
    // `omarchy.workspace-layouts` resolves to the files above.
    roots.module_path.insert(0, home.join(".local/state"));
    let reading = read(&roots);

    assert_eq!(
        reading.workspace_layouts,
        BTreeMap::from([(1, wm_core::LayoutMode::Flow), (2, wm_core::LayoutMode::Mosaic)]),
        "workspace N is index N-1: {:?}",
        reading.skipped
    );
    assert_eq!(reading.default_layout, Some(wm_core::LayoutMode::Mosaic));
    let lines = |needle: &str| reading.skipped.iter().filter(|skip| skip.what.contains(needle)).count();
    assert_eq!(lines("hl.workspace_rule(…) gapsin = 0"), 1, "one line per extra key: {:?}", reading.skipped);
    assert_eq!(lines("hl.workspace_rule(…) monitor = \"DP-1\""), 1, "{:?}", reading.skipped);
    assert_eq!(lines("hl.workspace_rule(workspace = \"special:magic\"): a special workspace"), 1, "{:?}", reading.skipped);
    assert_eq!(lines("hl.workspace_rule(workspace = \"150\"): outside 1 to 99"), 1, "{:?}", reading.skipped);
    assert_eq!(lines("hl.workspace_rule(workspace = \"5\"): names no layout"), 1, "{:?}", reading.skipped);
    assert_eq!(lines("hl.workspace_rule(nil)"), 1, "{:?}", reading.skipped);
    assert!(
        reading.skipped.iter().any(|skip| skip.what == "workspace 4, layout:master" && skip.why.contains("not a style")),
        "{:?}",
        reading.skipped
    );
    assert_eq!(lines("\"2\""), 0, "the rule that was read earns no skip line: {:?}", reading.skipped);
    assert_eq!(lines("hl.workspace_rule"), 6, "every call this reader declined is on its own line: {:?}", reading.skipped);

    let config = crate::parse_with("desktop = \"omarchy\"", &|| Some(read(&roots))).unwrap();
    assert_eq!(config.workspace_layouts, reading.workspace_layouts);
}

/// The classic `workspace = N, rules…` line: the layout of a numbered
/// workspace is read, every other rule is named, and every selector
/// that is not a number from 1 to 99 says what it is instead.
#[test]
fn conf_workspace_rules_read_the_layout_and_report_the_rest_by_name() {
    let mut out = Vec::new();
    conf::read(
        concat!(
            "workspace = 2, layout:scrolling, gapsin:0\n",
            "workspace = special:magic, layout:dwindle\n",
            "workspace = 1, monitor:DP-1\n",
            "workspace = name:mail, layout:dwindle\n",
            "workspace = r[1-5], layout:dwindle\n",
            "workspace = 0, layout:dwindle\n",
            "general {\n  layout = master\n}\n",
        ),
        &mut Default::default(),
        &mut out,
    );
    assert_eq!(
        out.iter().filter(|d| matches!(d, Directive::WorkspaceLayout { .. })).count(),
        1,
        "{out:?}"
    );
    assert!(out.contains(&Directive::WorkspaceLayout { workspace: 2, layout: "scrolling".into() }));
    let ignored: Vec<&str> = out
        .iter()
        .filter_map(|d| match d {
            Directive::Ignored { kind: "workspace-rule", detail } => Some(detail.as_str()),
            _ => None,
        })
        .collect();
    for needle in [
        "workspace = 2, gapsin:0: only layout is read",
        "special:magic, layout:dwindle: a special workspace",
        "workspace = 1, monitor:DP-1: only layout is read",
        "name:mail, layout:dwindle: a named workspace",
        "r[1-5], layout:dwindle: a workspace selector",
        "workspace = 0, layout:dwindle: outside 1 to 99",
    ] {
        assert!(ignored.iter().any(|d| d.contains(needle)), "{needle}: {ignored:?}");
    }
    // A layout name this desktop has no style for is judged once, in
    // the lowering, with a reason — and changes nothing.
    let reading = lower(out, LoadReport { files: Vec::new(), skipped: Vec::new() });
    assert_eq!(reading.default_layout, None);
    assert!(
        reading.skipped.iter().any(|skip| skip.what == "general.layout = master" && skip.why.contains("not a style")),
        "{:?}",
        reading.skipped
    );
}

// ---- the classic conf syntax ------------------------------------------

/// The same machine's Omarchy 3 configuration, read through the other
/// front end — including the `~/.local/share/omarchy` symlink into the
/// compatibility shim that the user's own `hyprland.conf` sources
/// through.
#[test]
fn the_classic_conf_syntax_reads_the_same_desktop() {
    let reading = read(&conf_machine());
    assert!(reading.files.len() >= 15, "{:?}", reading.files);
    assert_eq!(action_for(&reading, "super+w"), Some(Action::Close));
    assert_eq!(action_for(&reading, "super+1"), Some(Action::Workspace(0)));
    assert_eq!(
        action_for(&reading, "super+shift+9"),
        Some(Action::WorkspaceCarry(8))
    );
    assert_eq!(
        action_for(&reading, "super+tab"),
        Some(Action::WorkspaceNextOccupied)
    );
    assert_eq!(
        argv_for(&reading, "super+space"),
        Some(vec!["omarchy-launch-walker".into()])
    );
    assert!(!reading.float_rules.is_empty());
}

/// The user's own file is read *after* the defaults it sources, so
/// their overrides win — which is only true because includes are
/// spliced in place.
#[test]
fn the_users_own_conf_overrides_the_default_it_sources() {
    let reading = read(&conf_machine());
    // `~/.config/hypr/bindings.conf` rebinds SUPER+SPACE's neighbours
    // and adds `SUPER SHIFT, S, Screenshot`; the default file has
    // `SUPER SHIFT, S` unbound and `SUPER SHIFT, SLASH` on 1password.
    assert_eq!(
        argv_for(&reading, "super+shift+s"),
        Some(vec!["omarchy-capture-screenshot".into()])
    );
    assert_eq!(
        argv_for(&reading, "super+shift+slash"),
        Some(vec!["uwsm-app".into(), "--".into(), "1password".into()]),
        "the user's own line, not the default's"
    );
}

/// Hyprland's `##` escape for a literal `#`, which Omarchy's own user
/// template calls out by name because web-app bindings carry URLs with
/// fragments in them. A comment stripper that ate half a URL would
/// silently rewrite a binding.
#[test]
fn a_doubled_hash_stays_a_hash_in_a_url() {
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        "bind = SUPER, A, exec, launch-webapp \"https://x.com/##anchor\" # trailing\n",
        &mut vars,
        &mut out,
    );
    let Directive::Bind {
        dispatcher: directive::Dispatcher::Exec(command),
        ..
    } = &out[0]
    else {
        panic!("expected a bind, got {out:?}");
    };
    assert_eq!(command, "launch-webapp \"https://x.com/#anchor\"");
}

#[test]
fn a_global_dispatcher_becomes_a_portal_shortcut_target() {
    let root = scratch("global-shortcut");
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "bind = SUPER, M, global, org.example.Player:mute\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(
        action_for(&reading, "super+m"),
        Some(Action::GlobalShortcut("org.example.Player:mute".into()))
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Hyprland's `$variables`, which every hand-written `hyprland.conf`
/// from the upstream wiki uses for its modifier.
#[test]
fn conf_variables_are_substituted_longest_name_first() {
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        "$mainMod = SUPER\n$mainModShift = SUPER SHIFT\nbind = $mainModShift, Q, killactive,\nbind = $mainMod, W, killactive,\n",
        &mut vars,
        &mut out,
    );
    let keys: Vec<&str> = out
        .iter()
        .filter_map(|d| match d {
            Directive::Bind { keys, .. } => Some(keys.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(keys, vec!["SUPER SHIFT Q", "SUPER W"]);
}

#[test]
fn conf_submap_bindings_never_leak_into_the_global_keymap() {
    let root = scratch("conf-submap");
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "bind = SUPER, R, submap, resize\nsubmap = resize\nbind = , 1, exec, notify-send ONE\nbind = , escape, submap, reset\nsubmap = reset\nbind = SUPER, Q, killactive,\n",
    );
    let reading = read(&Roots::under(&root));
    assert!(
        action_for(&reading, "1").is_none(),
        "a submap typing key became global"
    );
    assert!(
        action_for(&reading, "super+q").is_some(),
        "global parsing did not resume after reset"
    );
    assert!(reading.skipped.iter().any(|skip| skip.kind == "submap-bind"
        && skip.what.contains("1")
        && skip.what.contains("resize")));
}

#[test]
fn lua_submap_bindings_are_reported_without_becoming_global() {
    let root = scratch("lua-submap");
    write(
        &root.join(".config/hypr/hyprland.lua"),
        "hl.define_submap(\"resize\", function()\n  hl.bind(\"1\", hl.dsp.exec_cmd(\"notify-send ONE\"))\nend)\nhl.bind(\"SUPER + Q\", hl.dsp.window.close())\n",
    );
    let reading = read(&Roots::under(&root));
    assert!(action_for(&reading, "1").is_none());
    assert!(action_for(&reading, "super+q").is_some());
    assert!(reading
        .skipped
        .iter()
        .any(|skip| skip.kind == "submap-bind" && skip.what.contains("resize")));
}

/// All three window-rule syntaxes a real machine can carry.
#[test]
fn every_window_rule_syntax_hyprland_has_shipped_is_read() {
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        concat!(
            "windowrule = float, ^(v1app)$\n",
            "windowrulev2 = float, class:^(v2app)$, title:^(Dialog)$\n",
            "windowrule = float on, match:class modern\n",
            "windowrule = size 400 300, match:class modern\n",
        ),
        &mut vars,
        &mut out,
    );
    let rules: Vec<directive::WindowRule> = out
        .into_iter()
        .filter_map(|d| match d {
            Directive::WindowRule(rule) => Some(rule),
            _ => None,
        })
        .collect();
    let (compiled, notes) = rules::compile(&rules);
    assert!(notes.is_empty(), "{notes:?}");
    assert!(
        compiled.decision_for("v1app", "", "").is_some(),
        "v1 bare-pattern form"
    );
    assert!(
        compiled.decision_for("v2app", "Dialog", "").is_some(),
        "v2 colon form"
    );
    assert!(
        compiled.decision_for("v2app", "Other", "").is_none(),
        "v2 title matcher must actually constrain"
    );
    assert_eq!(
        compiled.decision_for("modern", "", "").and_then(|d| d.size),
        Some(wm_core::Size::new(400, 300)),
        "0.53+ match: form"
    );
}

#[test]
fn a_scroll_touchpad_rule_sets_that_windows_touchpad_factor() {
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        concat!(
            "windowrule = scroll_touchpad 1.5, match:class (Alacritty|kitty|foot)\n",
            "windowrule = scroll_touchpad 0.2, match:class com.mitchellh.ghostty\n",
            "windowrule = scroll_touchpad fast, match:class ^odd$\n",
        ),
        &mut vars,
        &mut out,
    );
    let parsed: Vec<_> = out
        .into_iter()
        .filter_map(|directive| match directive {
            Directive::WindowRule(rule) => Some(rule),
            _ => None,
        })
        .collect();
    let (rules, notes) = rules::compile(&parsed);
    assert_eq!(rules.window_decision_for("foot", "", "").touchpad_scroll_factor, Some(1.5));
    assert_eq!(rules.window_decision_for("com.mitchellh.ghostty", "", "").touchpad_scroll_factor, Some(0.2));
    assert_eq!(rules.window_decision_for("firefox", "", "").touchpad_scroll_factor, None);
    assert_eq!(rules.window_decision_for("odd", "", "").touchpad_scroll_factor, None);
    assert!(notes.iter().any(|note| note.contains("\"fast\"")), "{notes:?}");
    assert!(
        !notes.iter().any(|note| note.contains("scroll_touchpad is not implemented")),
        "the property is read now: {notes:?}"
    );
}

#[test]
fn idle_inhibit_modes_are_read_by_name_and_unknown_values_are_reported() {
    use wm_core::IdleInhibitRule::{Always, Focus, Fullscreen, None as Never};
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        concat!(
            "windowrule = idle_inhibit always, match:class ^always$\n",
            "windowrule = idle_inhibit on, match:class ^on$\n",
            "windowrule = idle_inhibit focus, match:class ^focus$\n",
            "windowrule = idle_inhibit fullscreen, match:class ^steam$\n",
            "windowrule = idle_inhibit none, match:class ^none$\n",
            "windowrule = idle_inhibit off, match:class ^off$\n",
            "windowrule = idle_inhibit always, match:class ^cleared$\n",
            "windowrule = idle_inhibit none, match:class ^cleared$\n",
            "windowrule = idle_inhibit sometimes, match:class ^odd$\n",
        ),
        &mut vars,
        &mut out,
    );
    let parsed: Vec<_> = out
        .into_iter()
        .filter_map(|directive| match directive {
            Directive::WindowRule(rule) => Some(rule),
            _ => None,
        })
        .collect();
    let (rules, notes) = rules::compile(&parsed);
    for (class, mode) in [
        ("always", Always),
        ("on", Always),
        ("focus", Focus),
        ("steam", Fullscreen),
        ("none", Never),
        ("off", Never),
        ("cleared", Never),
        ("odd", Never),
    ] {
        assert_eq!(rules.window_decision_for(class, "", "").idle_inhibit, mode, "{class}");
    }
    assert!(
        notes.iter().any(|note| note.contains("\"sometimes\"")),
        "an unknown mode is reported, not read as on: {notes:?}"
    );
}

/// Omarchy suppresses client maximize requests on every window. The
/// events are read one by one from a list, and one this desktop cannot
/// suppress is reported by name rather than taking the rule down.
#[test]
fn suppress_event_reads_each_event_and_reports_the_rest_by_name() {
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        concat!(
            "windowrule = suppress_event maximize, match:class .*\n",
            "windowrule = suppress_event fullscreen activate, match:class ^game$\n",
            "windowrule = suppress_event fullscreenoutput, match:class ^odd$\n",
        ),
        &mut vars,
        &mut out,
    );
    let parsed: Vec<_> = out
        .into_iter()
        .filter_map(|directive| match directive {
            Directive::WindowRule(rule) => Some(rule),
            _ => None,
        })
        .collect();
    let (rules, notes) = rules::compile(&parsed);
    let any = rules.window_decision_for("foot", "", "");
    assert!(any.suppress_maximize && !any.suppress_fullscreen, "{notes:?}");
    assert_eq!(any.focus_on_activate, None);
    let game = rules.window_decision_for("game", "", "");
    assert!(game.suppress_maximize && game.suppress_fullscreen, "{notes:?}");
    assert_eq!(game.focus_on_activate, Some(false), "activate is the activation refusal");
    assert!(!rules.window_decision_for("odd", "", "").suppress_fullscreen);
    assert!(notes.iter().any(|note| note.contains("\"fullscreenoutput\"")), "{notes:?}");
    assert!(
        !notes.iter().any(|note| note.contains("property suppress_event")),
        "the property itself is read: {notes:?}"
    );
}

#[test]
fn non_geometric_window_rules_are_combined_property_by_property() {
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        concat!(
            "windowrule = pin on, match:class ^player$\n",
            "windowrule = idle_inhibit always, match:class ^player$\n",
            "windowrule = no_focus on, match:class ^player$\n",
            "windowrule = no_focus off, match:class ^player$\n",
            "windowrule = no_initial_focus on, match:class ^player$\n",
            "windowrule = focus_on_activate off, match:class ^player$\n",
            "windowrule = maximize on, match:title ^Cinema$\n",
            "windowrule = fullscreen on, match:title ^Cinema$\n",
        ),
        &mut vars,
        &mut out,
    );
    let parsed: Vec<_> = out
        .into_iter()
        .filter_map(|directive| match directive {
            Directive::WindowRule(rule) => Some(rule),
            _ => None,
        })
        .collect();
    let (rules, notes) = rules::compile(&parsed);
    assert!(
        notes.is_empty(),
        "every property in this fixture is supported: {notes:?}"
    );

    let decision = rules.window_decision_for("player", "Cinema", "");
    assert!(decision.pin);
    assert_eq!(decision.idle_inhibit, wm_core::IdleInhibitRule::Always);
    assert!(
        !decision.no_focus,
        "the later property overrides only no_focus"
    );
    assert!(decision.no_initial_focus);
    assert_eq!(decision.focus_on_activate, Some(false));
    assert!(decision.maximize && decision.fullscreen);
}

/// Omarchy's opacity rules, as shipped, resolve to what Omarchy means
/// by them. The whole scheme rests on tag removal: every window is
/// tagged `default-opacity` first, each app file that wants an opaque
/// window removes the tag again, and the opacity for the tag comes
/// last — so a reader that ignored `-default-opacity` would make mpv
/// translucent and give Chromium the terminal's alpha.
#[test]
fn omarchys_opacity_rules_resolve_as_authored() {
    use wm_core::OpacityRule;
    let reading = read(&machine());
    let policy = reading.float_rules;
    let opacity = |class: &str, title: &str| policy.window_decision_for(class, title, "").opacity;
    // A terminal: the default, through the tag alone.
    assert_eq!(
        opacity("org.codeberg.dnkl.foot", ""),
        Some(OpacityRule { active: 0.985, inactive: 0.96, fullscreen: None }),
        "a terminal takes Omarchy's default opacity"
    );
    // Chromium: `apps/browser.lua` removes the default tag on the
    // strength of the `chromium-based-browser` tag and sets its own.
    assert_eq!(
        opacity("chromium", ""),
        Some(OpacityRule { active: 1.0, inactive: 0.985, fullscreen: None }),
        "a browser is opaque while focused"
    );
    assert_eq!(opacity("firefox", ""), Some(OpacityRule { active: 1.0, inactive: 0.985, fullscreen: None }));
    // Media, games and colour-critical work: `-default-opacity` and
    // an explicit `1 1`.
    for class in ["mpv", "steam", "steam_app_123", "resolve", "DaVinci Resolve"] {
        assert_eq!(
            opacity(class, ""),
            Some(OpacityRule { active: 1.0, inactive: 1.0, fullscreen: None }),
            "{class} is opaque in both focus states"
        );
    }
    // A YouTube web app: only the tag is removed. No rule names an
    // opacity for it, and no rule may, so it is drawn opaque.
    assert_eq!(
        opacity("chrome-youtube.com__-Default", "YouTube"),
        None,
        "tag removal alone takes the web app out of the default"
    );
    // The webcam overlay asks to be left alone by `dim_inactive`.
    let overlay = policy.window_decision_for("WebcamOverlay-small", "WebcamOverlay", "");
    assert!(overlay.no_dim, "the webcam overlay is never dimmed");
    assert_eq!(overlay.opacity, Some(OpacityRule { active: 1.0, inactive: 1.0, fullscreen: None }));
    assert!(
        !policy.window_decision_for("org.codeberg.dnkl.foot", "", "").no_dim,
        "no_dim is the overlay's, not everybody's"
    );
    assert!(
        !reading.skipped.iter().any(|skip| skip.what.contains("tag removal is not followed")
            || skip.what.contains("property opacity")
            || skip.what.contains("property no_dim")),
        "opacity, no_dim and tag removal are read now: {:?}",
        reading.skipped.iter().filter(|skip| skip.kind == "window-rule").map(|skip| &skip.what).collect::<Vec<_>>()
    );
}

/// The order Omarchy writes its tag rules in is not the order a
/// one-pass reader would need. `floating-window`'s consumers sit above
/// the lines that add the tag, while `default-opacity`'s removals sit
/// between the add and the consumer; both have to come out the way
/// Hyprland's repeated passes settle them.
#[test]
fn tag_removal_follows_file_order() {
    let compile = |text: &str| {
        let mut vars = BTreeMap::new();
        let mut out = Vec::new();
        conf::read(text, &mut vars, &mut out);
        let parsed: Vec<_> = out
            .into_iter()
            .filter_map(|directive| match directive {
                Directive::WindowRule(rule) => Some(rule),
                _ => None,
            })
            .collect();
        rules::compile(&parsed)
    };
    // Add, remove, consume: the removal between them holds.
    let (rules, notes) = compile(concat!(
        "windowrule = tag +translucent, match:class .*\n",
        "windowrule = tag -translucent, match:class ^mpv$\n",
        "windowrule = opacity 0.9, match:tag translucent\n",
    ));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(rules.window_decision_for("foot", "", "").opacity.map(|o| o.active), Some(0.9));
    assert_eq!(rules.window_decision_for("mpv", "", "").opacity, None, "removed before the rule");
    // Consume, add, remove: a rule above the add still sees the tag
    // (the second pass does), and the removal after it still holds.
    let (rules, notes) = compile(concat!(
        "windowrule = float on, match:tag floating\n",
        "windowrule = tag +floating, match:class ^(btop|mpv)$\n",
        "windowrule = tag -floating, match:class ^mpv$\n",
    ));
    assert!(notes.is_empty(), "{notes:?}");
    assert!(rules.decision_for("btop", "", "").is_some(), "tagged below the rule that reads the tag");
    assert!(rules.decision_for("mpv", "", "").is_none(), "removed below both");
    // Add, remove, add again: the last word wins.
    let (rules, _) = compile(concat!(
        "windowrule = tag +x, match:class ^a$\n",
        "windowrule = tag -x, match:class ^a$\n",
        "windowrule = tag +x, match:class ^a$\n",
        "windowrule = pin on, match:tag x\n",
    ));
    assert!(rules.window_decision_for("a", "", "").pin);
    // A removal on the strength of another tag is followed one level,
    // exactly as Omarchy's browser file writes it.
    let (rules, notes) = compile(concat!(
        "windowrule = tag +default-opacity, match:class .*\n",
        "windowrule = tag +browser, match:class ^(chromium|firefox)$\n",
        "windowrule = tag -default-opacity, match:tag browser\n",
        "windowrule = opacity 1.0 0.985, match:tag browser\n",
        "windowrule = opacity 0.985 0.96, match:tag default-opacity\n",
    ));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(rules.window_decision_for("chromium", "", "").opacity.map(|o| o.inactive), Some(0.985));
    assert_eq!(rules.window_decision_for("foot", "", "").opacity.map(|o| o.inactive), Some(0.96));
    // Two levels is where following stops, loudly.
    let (rules, notes) = compile(concat!(
        "windowrule = tag +a, match:class ^x$\n",
        "windowrule = tag +b, match:tag a\n",
        "windowrule = tag +c, match:tag b\n",
        "windowrule = pin on, match:tag c\n",
    ));
    assert!(!rules.window_decision_for("x", "", "").pin, "a chain of two tags is not followed");
    assert!(notes.iter().any(|note| note.contains("chained tags are not followed")), "{notes:?}");
}

/// `opacity` in its three shapes, clamped, with the unreadable
/// refused by name rather than read as something.
#[test]
fn opacity_values_are_read_clamped_and_refused_when_unreadable() {
    use wm_core::OpacityRule;
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        concat!(
            "windowrule = opacity 0.8, match:class ^one$\n",
            "windowrule = opacity 0.9 0.7, match:class ^two$\n",
            "windowrule = opacity 0.9 0.7 0.5, match:class ^three$\n",
            "windowrule = opacity 1.5 -2 override, match:class ^clamped$\n",
            "windowrule = opacity nan, match:class ^nan$\n",
            "windowrule = opacity inf 1, match:class ^inf$\n",
            "windowrule = opacity 1 2 3 4, match:class ^many$\n",
            "windowrule = opacity, match:class ^none$\n",
            "windowrule = no_dim on, match:class ^nodim$\n",
            "windowrule = no_dim off, match:class ^dimmed$\n",
        ),
        &mut vars,
        &mut out,
    );
    let parsed: Vec<_> = out
        .into_iter()
        .filter_map(|directive| match directive {
            Directive::WindowRule(rule) => Some(rule),
            _ => None,
        })
        .collect();
    let (rules, notes) = rules::compile(&parsed);
    let opacity = |class: &str| rules.window_decision_for(class, "", "").opacity;
    assert_eq!(opacity("one"), Some(OpacityRule { active: 0.8, inactive: 0.8, fullscreen: None }));
    assert_eq!(opacity("two"), Some(OpacityRule { active: 0.9, inactive: 0.7, fullscreen: None }));
    assert_eq!(opacity("three"), Some(OpacityRule { active: 0.9, inactive: 0.7, fullscreen: Some(0.5) }));
    assert_eq!(opacity("clamped"), Some(OpacityRule { active: 1.0, inactive: 0.0, fullscreen: None }));
    for class in ["nan", "inf", "many", "none"] {
        assert_eq!(opacity(class), None, "{class} is refused");
    }
    assert_eq!(
        notes.iter().filter(|note| note.contains("window rule opacity") && note.contains("property skipped")).count(),
        4,
        "{notes:?}"
    );
    assert!(rules.window_decision_for("nodim", "", "").no_dim);
    assert!(!rules.window_decision_for("dimmed", "", "").no_dim);
}

/// `no_screen_share` is read in both spellings, last rule winning, and
/// Omarchy's password-manager rules resolve to it: a 1Password or
/// Bitwarden window is hidden from every capture the compositor
/// renders, while nothing else is.
#[test]
fn no_screen_share_is_read_and_resolves_for_omarchys_password_managers() {
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        concat!(
            "windowrule = no_screen_share on, match:class ^vault$\n",
            "windowrule = noscreenshare 1, match:class ^legacy$\n",
            "windowrule = no_screen_share on, match:class ^shown$\n",
            "windowrule = no_screen_share off, match:class ^shown$\n",
        ),
        &mut vars,
        &mut out,
    );
    let parsed: Vec<_> = out
        .into_iter()
        .filter_map(|directive| match directive {
            Directive::WindowRule(rule) => Some(rule),
            _ => None,
        })
        .collect();
    let (rules, notes) = rules::compile(&parsed);
    assert!(notes.is_empty(), "no_screen_share is read, not skipped: {notes:?}");
    assert!(rules.window_decision_for("vault", "", "").no_screen_share);
    assert!(rules.window_decision_for("legacy", "", "").no_screen_share, "Hyprland's one-word spelling");
    assert!(!rules.window_decision_for("shown", "", "").no_screen_share, "the later property wins");
    assert!(!rules.window_decision_for("other", "", "").no_screen_share, "a window no rule names is captured");

    let reading = read(&machine());
    let policy = reading.float_rules;
    for class in ["1Password", "1password", "Bitwarden", "chrome-nngceckbapebfimnlniiiahkandclblb-Default"] {
        assert!(
            policy.window_decision_for(class, "", "").no_screen_share,
            "{class} is hidden from capture by Omarchy's rules"
        );
    }
    assert!(
        !policy.window_decision_for("org.codeberg.dnkl.foot", "", "").no_screen_share,
        "no_screen_share is the password managers', not everybody's"
    );
    assert!(
        !reading.skipped.iter().any(|skip| skip.what.contains("property no_screen_share")),
        "no_screen_share is read now: {:?}",
        reading.skipped.iter().filter(|skip| skip.kind == "window-rule").map(|skip| &skip.what).collect::<Vec<_>>()
    );
}

/// `decoration:dim_inactive` and `dim_strength`, in both syntaxes,
/// with the rest of the decoration table still declined by name.
#[test]
fn dim_inactive_is_read_from_the_decoration_table() {
    let home = scratch("dim-lua");
    write(
        &home.join(".config/hypr/hyprland.lua"),
        "hl.config({ decoration = { rounding = 8, dim_inactive = true, dim_strength = 0.15 } })\n",
    );
    let reading = read(&Roots::under(&home));
    assert_eq!(reading.dim_inactive, Some(0.15));
    assert!(
        reading.skipped.iter().any(|skip| skip.what.contains("decoration.rounding")),
        "the rest of the table is still declined by name: {:?}",
        reading.skipped
    );
    let mut config = crate::Config::default_config();
    apply(&mut config, Some(&reading));
    assert_eq!(config.decorations.dim_inactive, Some(0.15));

    let home = scratch("dim-conf");
    write(
        &home.join(".config/hypr/hyprland.conf"),
        "decoration {\n    rounding = 8\n    dim_inactive = true\n    dim_strength = 0.4\n    blur {\n        enabled = true\n    }\n}\n",
    );
    let reading = read(&Roots::under(&home));
    assert_eq!(reading.dim_inactive, Some(0.4));

    // Hyprland's default strength applies when only the switch is set;
    // an out-of-range strength is refused and the default kept.
    let home = scratch("dim-default");
    write(&home.join(".config/hypr/hyprland.lua"), "hl.config({ decoration = { dim_inactive = true, dim_strength = 7 } })\n");
    let reading = read(&Roots::under(&home));
    assert_eq!(reading.dim_inactive, Some(0.5));
    assert!(reading.skipped.iter().any(|skip| skip.what.contains("dim_strength = 7")), "{:?}", reading.skipped);

    let home = scratch("dim-off");
    write(&home.join(".config/hypr/hyprland.lua"), "hl.config({ decoration = { dim_strength = 0.3 } })\n");
    assert_eq!(read(&Roots::under(&home)).dim_inactive, None, "a strength without the switch dims nothing");
}

#[test]
fn unsupported_rule_properties_are_named_without_discarding_supported_siblings() {
    let mut vars = BTreeMap::new();
    let mut out = Vec::new();
    conf::read(
        "windowrule = pin on, match:class ^notes$\n\
         windowrule = mystery_value 7, match:class ^notes$\n\
         windowrule = pin on, match:xwayland 1, match:class ^xterm$\n",
        &mut vars,
        &mut out,
    );
    let parsed: Vec<_> = out
        .into_iter()
        .filter_map(|directive| match directive {
            Directive::WindowRule(rule) => Some(rule),
            _ => None,
        })
        .collect();
    let (rules, notes) = rules::compile(&parsed);
    assert!(
        rules.window_decision_for("notes", "", "").pin,
        "a supported sibling property remains effective"
    );
    assert!(
        !rules.window_decision_for("xterm", "", "").pin,
        "an unsupported matcher refuses the whole rule"
    );
    assert!(notes
        .iter()
        .any(|note| note.contains("property mystery_value") && note.contains("property skipped")));
    assert!(notes
        .iter()
        .any(|note| note.contains("match:xwayland 1") && note.contains("rule skipped")));
}

// ---- activation and layering ------------------------------------------

/// The activation rule: the posture decides, and the key overrides the
/// posture.
#[test]
fn the_posture_decides_whether_anybody_elses_config_is_read() {
    let mut config = crate::Config::default_config();
    assert!(
        !wanted(&config),
        "a plain chonkstep desk reads nobody else's files"
    );
    config.desktop = crate::preset::Desktop::Omarchy;
    assert!(
        wanted(&config),
        "`desktop = \"omarchy\"` has already asked for this"
    );
    config.hyprland_config = Some(false);
    assert!(!wanted(&config), "and the key is the escape hatch");
    let mut config = crate::Config::default_config();
    config.keymap = crate::preset::Keymap::Omarchy;
    assert!(
        wanted(&config),
        "wanting Hyprland chords means wanting *your* Hyprland chords"
    );
    config.hyprland_config = Some(true);
    assert!(wanted(&config));
    let mut config = crate::Config::default_config();
    config.hyprland_config = Some(true);
    assert!(
        wanted(&config),
        "and it can be turned on from a chonkstep posture too"
    );
}

/// Live bindings replace the preset, with native capture defaults filling only
/// unclaimed chords. No chord may have two answers.
#[test]
fn the_live_read_replaces_the_baked_preset() {
    let mut config = crate::Config::default_config();
    crate::preset::apply_keymap(&mut config, crate::preset::Keymap::Omarchy);
    let baked = config.keybindings.len();
    assert!(baked > 0);
    let reading = read(&machine());
    apply(&mut config, Some(&reading));
    assert_eq!(
        config.keybindings.len(),
        reading.keybindings.len() + 5,
        "four native capture shortcuts and Freeform augment the live read, not all {baked} entries"
    );
    assert!(config.float_policy.is_some());
    assert!(!config.session_env.is_empty());
}

#[test]
fn capture_defaults_honor_live_bind_and_unbind_even_without_other_bindings() {
    let capture = crate::parse_key("super+ctrl+shift+4").unwrap();
    for replacement in [None, Some(Action::Close)] {
        let mut config = crate::Config::default_config();
        crate::preset::apply_keymap(&mut config, crate::preset::Keymap::Omarchy);
        let mut reading = Reading { explicit_keys: vec![capture], ..Reading::default() };
        if let Some(action) = &replacement { reading.keybindings.push((capture, action.clone())); }
        assert!(!reading.is_empty(), "unbind is an explicit user choice");
        apply(&mut config, Some(&reading));
        assert_eq!(config.keybindings.iter().find(|(key, _)| *key == capture).map(|(_, a)| a.clone()), replacement);
    }
}

/// Nothing to read means the preset stands. That is what it is for.
#[test]
fn nothing_to_read_keeps_the_preset_untouched() {
    let mut config = crate::Config::default_config();
    crate::preset::apply_keymap(&mut config, crate::preset::Keymap::Omarchy);
    let before = config.keybindings.clone();
    apply(&mut config, None);
    assert_eq!(config.keybindings, before);
    // ...and an empty home is "nothing to read", not "read nothing".
    let empty = scratch("empty");
    let reading = read(&Roots::under(&empty));
    assert!(reading.is_empty());
    let _ = std::fs::remove_dir_all(&empty);
}

/// The user's own `config.toml` still has the last word on any chord,
/// which is the whole precedence claim in one test.
#[test]
fn chonksteps_own_config_still_wins_over_the_read() {
    let root = fixtures().join("machine");
    let mut config = crate::Config::default_config();
    config.desktop = crate::preset::Desktop::Omarchy;
    config.keymap = crate::preset::Keymap::Omarchy;
    apply(&mut config, Some(&read(&Roots::under(&root))));
    let from_their_file = config.keybindings.clone();
    assert!(from_their_file
        .iter()
        .any(|(c, a)| *c == crate::parse_key("super+w").unwrap() && *a == Action::Close));
    // Now the layer above: a `[keybindings]` entry, applied the way
    // `parse` applies it.
    let table: toml::Table = "\"super+w\" = \"overview\"\n\"super+f\" = \"none\"\n"
        .parse()
        .unwrap();
    crate::apply_keybindings(&mut config.keybindings, &table);
    let action = |spec: &str| {
        let combo = crate::parse_key(spec).unwrap();
        config
            .keybindings
            .iter()
            .find(|(c, _)| *c == combo)
            .map(|(_, a)| a.clone())
    };
    assert_eq!(
        action("super+w"),
        Some(Action::Overview),
        "the file's own key beats the read"
    );
    assert_eq!(action("super+f"), None, "and `none` still unbinds one");
}

// ---- the watch --------------------------------------------------------

#[global_allocator]
static ALLOCATOR: chonk_test_support::AllocationCounter = chonk_test_support::AllocationCounter;

#[test]
fn unchanged_watch_polls_do_not_allocate() {
    let root = scratch("watch-allocations");
    let roots = Roots::under(&root);
    let mut reading = Reading::default();
    for index in 0..256 {
        let path = roots.user.join(format!("source-{index}.conf"));
        write(&path, "# unchanged\n");
        reading.files.push(path);
    }
    let mut watch = Watch::new(&roots, &reading);
    let now = std::time::Instant::now();
    assert!(!watch.changed(now));
    let (changed, stats) = chonk_test_support::measure(|| {
        (1..=20).fold(false, |changed, seconds| {
            watch.changed(now + std::time::Duration::from_secs(seconds)) | changed
        })
    });
    std::fs::remove_dir_all(root).unwrap();
    assert!(!changed);
    eprintln!("256 files, 20 unchanged polls: {stats:?}");
    assert_eq!(stats, chonk_test_support::AllocationStats::default());
}

/// What the reload guard's pause rests on: a watch that is simply not
/// asked for a while keeps the baseline it had, so an edit made in the
/// meantime — an upgrade replacing the tree file by file — is seen
/// exactly once when asking resumes, and never while it is off.
#[test]
fn an_edit_made_while_the_watch_is_not_consulted_is_seen_once_afterwards() {
    let root = scratch("watch-withheld");
    let entry = root.join(".config/hypr/hyprland.conf");
    write(&entry, "bind = SUPER, W, killactive,\n");
    let roots = Roots::under(&root);
    let mut watch = Watch::new(&roots, &read(&roots));
    let t0 = std::time::Instant::now();
    assert!(!watch.changed(t0), "the first look is a baseline");

    // Replaced the way a package transaction replaces it, while nobody
    // is asking.
    let staged = root.join(".config/hypr/hyprland.conf.new");
    write(&staged, "bind = SUPER, Q, killactive,\n");
    std::fs::rename(&staged, &entry).unwrap();

    assert!(watch.changed(t0 + std::time::Duration::from_secs(9)), "seen on the first look afterwards");
    assert!(!watch.changed(t0 + std::time::Duration::from_secs(11)), "and only once");
    let _ = std::fs::remove_dir_all(&root);
}

/// The live-edit path: a change to a watched file is seen, once.
#[test]
fn an_edit_to_a_watched_file_is_noticed_once() {
    let root = scratch("watch");
    let entry = root.join(".config/hypr/hyprland.conf");
    write(&entry, "bind = SUPER, W, killactive,\n");
    let roots = Roots::under(&root);
    let reading = read(&roots);
    assert_eq!(action_for(&reading, "super+w"), Some(Action::Close));

    let mut watch = Watch::new(&roots, &reading);
    let t0 = std::time::Instant::now();
    assert!(!watch.changed(t0), "the first look is a baseline");
    assert!(
        !watch.changed(t0 + std::time::Duration::from_secs(2)),
        "nothing moved"
    );

    // The way Omarchy's menu writes: a temporary file renamed over the
    // original, so the inode changes and the mtime may not.
    let staged = root.join(".config/hypr/hyprland.conf.new");
    write(&staged, "bind = SUPER, Q, killactive,\n");
    std::fs::rename(&staged, &entry).unwrap();
    assert!(
        watch.changed(t0 + std::time::Duration::from_secs(4)),
        "a rename-over must be seen"
    );
    assert!(
        !watch.changed(t0 + std::time::Duration::from_secs(6)),
        "reported once"
    );

    let reread = read(&roots);
    assert_eq!(action_for(&reread, "super+w"), None);
    assert_eq!(action_for(&reread, "super+q"), Some(Action::Close));
    let _ = std::fs::remove_dir_all(&root);
}

/// A look inside the cadence window does not touch the disk.
#[test]
fn the_watch_rate_limits_itself_to_a_look_a_second() {
    let root = scratch("cadence");
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "bind = SUPER, W, killactive,\n",
    );
    let roots = Roots::under(&root);
    let reading = read(&roots);
    let mut watch = Watch::new(&roots, &reading);
    let t0 = std::time::Instant::now();
    assert!(!watch.changed(t0));
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "bind = SUPER, Q, killactive,\n",
    );
    assert!(
        !watch.changed(t0 + std::time::Duration::from_millis(500)),
        "too soon to look"
    );
    assert!(
        watch.changed(t0 + std::time::Duration::from_millis(1000)),
        "the second look sees it"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A *new* file appearing is a change, which a per-file signature
/// alone would miss — the directory's mtime is what catches it.
#[test]
fn a_new_config_file_appearing_is_a_change() {
    let root = scratch("newfile");
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "bind = SUPER, W, killactive,\n",
    );
    let roots = Roots::under(&root);
    let reading = read(&roots);
    let mut watch = Watch::new(&roots, &reading);
    let t0 = std::time::Instant::now();
    assert!(!watch.changed(t0));
    write(
        &root.join(".config/hypr/extra.conf"),
        "bind = SUPER, E, killactive,\n",
    );
    assert!(
        watch.changed(t0 + std::time::Duration::from_secs(2)),
        "the directory's mtime moved"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// After a re-read the watch follows the new file set, so a file
/// brought in by a fresh `source =` line is watched from then on.
#[test]
fn the_watch_follows_a_freshly_sourced_file() {
    let root = scratch("follow");
    let entry = root.join(".config/hypr/hyprland.conf");
    write(&entry, "bind = SUPER, W, killactive,\n");
    write(
        &root.join(".config/hypr/more.conf"),
        "bind = SUPER, E, killactive,\n",
    );
    let roots = Roots::under(&root);
    let mut watch = Watch::new(&roots, &read(&roots));
    let t0 = std::time::Instant::now();
    assert!(!watch.changed(t0));

    write(
        &entry,
        "source = ~/.config/hypr/more.conf\nbind = SUPER, W, killactive,\n",
    );
    assert!(watch.changed(t0 + std::time::Duration::from_secs(2)));
    let reread = read(&roots);
    assert_eq!(
        action_for(&reread, "super+e"),
        Some(Action::Close),
        "the sourced file was read"
    );
    watch.follow(&reread);

    // The newly sourced file is now watched in its own right.
    write(
        &root.join(".config/hypr/more.conf"),
        "bind = SUPER, R, killactive,\n",
    );
    assert!(
        watch.changed(t0 + std::time::Duration::from_secs(4)),
        "an edit to the sourced file must now count"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---- key translation --------------------------------------------------

#[test]
fn every_separator_hyprland_accepts_between_modifiers_is_accepted() {
    for spelling in [
        "SUPER SHIFT, RETURN",
        "SUPER + SHIFT + RETURN",
        "SUPER_SHIFT RETURN",
        "super shift return",
    ] {
        assert_eq!(
            keys::spec_for(spelling).as_deref(),
            Ok("super+shift+return"),
            "{spelling}"
        );
    }
}

#[test]
fn a_keycode_resolves_through_the_layout_the_numbers_were_chosen_against() {
    assert_eq!(keys::spec_for("SUPER + code:10").as_deref(), Ok("super+1"));
    assert_eq!(keys::spec_for("SUPER + code:19").as_deref(), Ok("super+0"));
    assert_eq!(
        keys::spec_for("SUPER + code:20").as_deref(),
        Ok("super+minus")
    );
    assert_eq!(
        keys::spec_for("SUPER + ALT + code:34").as_deref(),
        Ok("super+alt+bracketleft")
    );
    // Omarchy's Apple-keyboard menu position: evdev aliases it to F23.
    assert_eq!(
        keys::spec_for("SUPER + SHIFT + code:201").as_deref(),
        Ok("super+shift+f23")
    );
}

#[test]
fn underscore_separates_modifiers_without_splitting_a_key_name() {
    // `_` is one of the three separators Hyprland puts between
    // modifiers, and it is also inside the name of every key on the
    // numeric keypad. The splitter used to assume the first and take
    // the whole chunk apart, so `KP_Enter` arrived as `KP` + `Enter`
    // and the line was refused for a modifier nobody wrote.
    assert_eq!(keys::spec_for("SUPER_SHIFT RETURN").as_deref(), Ok("super+shift+return"));
    assert_eq!(keys::spec_for("SUPER, KP_Enter").as_deref(), Ok("super+kpenter"));
    // Modifiers and an underscored key name in one chunk: the leading
    // run of modifiers comes off and the rest stays whole.
    assert_eq!(keys::spec_for("SUPER_KP_Enter").as_deref(), Ok("super+kpenter"));
    assert_eq!(keys::spec_for("SUPER_SHIFT_KP_Add").as_deref(), Ok("super+shift+kpadd"));
    // A genuinely unknown key still reports itself, and reports the
    // whole name rather than the fragment after the last underscore.
    assert_eq!(
        keys::spec_for("SUPER, KP_Nonsense"),
        Err(keys::KeyTrouble::UnknownKey("KP_Nonsense".into()))
    );
    // The pointer bindings that also contain `_` are still refused as
    // pointer bindings, ahead of any of this.
    assert!(matches!(keys::spec_for("SUPER, mouse_up"), Err(keys::KeyTrouble::NotAKey(_))));
}

#[test]
fn kp_enter_is_its_own_key_and_does_not_steal_the_main_enter_binding() {
    // Two failures at once, which is why this asserts both halves.
    // On main `KP_Enter` did not resolve at all — the chunk splitter
    // tore it into `KP` + `Enter` and the line was refused as
    // `UnknownModifier("KP")`. Behind that stood a dead
    // `("kp_enter", "return")` alias, so fixing only the splitter
    // would have turned a refused binding into a silent collision:
    // `bind()` de-duplicates by combo, and a config binding both
    // `SUPER, Return` and `SUPER, KP_Enter` would have kept just the
    // second action, on the main Enter key. On an Omarchy machine the
    // binding that would have removed is the terminal's.
    assert_eq!(keys::spec_for("SUPER, Return").as_deref(), Ok("super+return"));
    assert_eq!(keys::spec_for("SUPER, KP_Enter").as_deref(), Ok("super+kpenter"));
    assert_ne!(
        keys::spec_for("SUPER, KP_Enter"),
        keys::spec_for("SUPER, Return"),
        "the numpad's Enter and the main Enter are different keys"
    );
    // And they really are two distinct keysyms downstream, not two
    // spellings the parser folds back together.
    let numpad = crate::parse_key("super+kpenter").expect("kpenter parses");
    let main = crate::parse_key("super+return").expect("return parses");
    assert_eq!(numpad.keysym, 0xff8d, "XK_KP_Enter");
    assert_eq!(main.keysym, 0xff0d, "XK_Return");
    assert_ne!(numpad, main);
}

#[test]
fn binding_both_enters_leaves_two_bindings_rather_than_one() {
    // The collision end-to-end, through the reader that de-duplicates
    // by combo: the shape of an Omarchy config that also binds the
    // numpad. On main the second line was refused outright, so the
    // numpad did nothing; with the splitter fixed but the `kp_enter`
    // alias left pointing at `return` both lines would collide on
    // `super+return` and the terminal binding would vanish. Only both
    // fixes together give the two bindings this asserts.
    let root = scratch("kp-enter-collision");
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "bind = SUPER, Return, exec, my-terminal\nbind = SUPER, KP_Enter, exec, my-calculator\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(
        argv_for(&reading, "super+return"),
        Some(vec!["my-terminal".into()]),
        "the main Enter must keep its own action"
    );
    assert_eq!(
        argv_for(&reading, "super+kpenter"),
        Some(vec!["my-calculator".into()]),
        "the numpad Enter must get its own binding"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_keypad_is_bindable_by_name_in_both_spellings() {
    // Every keypad key Hyprland can name, resolved. The digits and the
    // cursor-mode names are two spellings of one key, so they must
    // produce the same keysym — see `keysym_for`'s keypad block.
    for (hypr, spec) in [
        ("KP_1", "kp1"),
        ("KP_5", "kp5"),
        ("KP_0", "kp0"),
        ("KP_Add", "kpadd"),
        ("KP_Subtract", "kpsubtract"),
        ("KP_Multiply", "kpmultiply"),
        ("KP_Divide", "kpdivide"),
        ("KP_Decimal", "kpdecimal"),
        ("KP_End", "kpend"),
        ("KP_Begin", "kpbegin"),
        ("KP_Insert", "kpinsert"),
    ] {
        assert_eq!(
            keys::spec_for(&format!("SUPER, {hypr}")).as_deref(),
            Ok(format!("super+{spec}").as_str()),
            "{hypr}"
        );
    }
    for (digit, cursor) in
        [("kp1", "kpend"), ("kp5", "kpbegin"), ("kp0", "kpinsert"), ("kpdecimal", "kpdelete")]
    {
        assert_eq!(
            crate::parse_key(digit).map(|combo| combo.keysym),
            crate::parse_key(cursor).map(|combo| combo.keysym),
            "{digit} and {cursor} are the same key"
        );
    }
}

#[test]
fn a_keypad_binding_names_the_keysym_the_compositor_will_actually_see() {
    // The correction that makes this fix work rather than merely
    // parse. Bindings are matched against the keycode's LEVEL-0
    // keysym (`wm-wayland`'s `input.rs`), and on the stock US map the
    // keypad's digits are all at level 2, behind NumLock:
    //
    //   $ xkbcli how-to-type --keysym KP_1
    //   KEYCODE 87  KP1  LEVEL# 2  MODIFIERS [ Mod2 NumLock ]
    //   $ xkbcli how-to-type --keysym KP_End
    //   KEYCODE 87  KP1  LEVEL# 1  MODIFIERS [ ]
    //
    // So XK_KP_1 (0xffb1) is a keysym this compositor can never be
    // handed for a binding. Naming `kp1` after it would produce a
    // binding that parses, warns about nothing and never fires. Each
    // digit therefore resolves to its key's level-0 keysym, which also
    // makes the binding independent of how NumLock happens to be set.
    for (spec, keysym) in [
        ("kp7", 0xff95),
        ("kp4", 0xff96),
        ("kp8", 0xff97),
        ("kp6", 0xff98),
        ("kp2", 0xff99),
        ("kp9", 0xff9a),
        ("kp3", 0xff9b),
        ("kp1", 0xff9c),
        ("kp5", 0xff9d),
        ("kp0", 0xff9e),
        ("kpdecimal", 0xff9f),
    ] {
        let combo = crate::parse_key(spec).unwrap_or_else(|| panic!("{spec} parses"));
        assert_eq!(combo.keysym, keysym, "{spec}");
        assert!(
            !(0xffb0..=0xffb9).contains(&combo.keysym),
            "{spec} resolved to a NumLock-only keysym, which can never match"
        );
    }
    // The operators have no second level to hide behind and keep their
    // own keysyms.
    for (spec, keysym) in [
        ("kpenter", 0xff8d),
        ("kpmultiply", 0xffaa),
        ("kpadd", 0xffab),
        ("kpsubtract", 0xffad),
        ("kpdivide", 0xffaf),
        ("kpequal", 0xffbd),
    ] {
        assert_eq!(crate::parse_key(spec).map(|combo| combo.keysym), Some(keysym), "{spec}");
    }
}

#[test]
fn the_keypads_keycodes_resolve_too() {
    // `code:N` and the name must land on the same key, or a config
    // that mixes the two spellings gets two bindings where it meant
    // one. Note 63 and 125 sit outside the 79..=91 run the rest of the
    // keypad forms.
    for (code, spec) in [
        (63, "kpmultiply"),
        (79, "kp7"),
        (82, "kpsubtract"),
        (84, "kp5"),
        (86, "kpadd"),
        (87, "kp1"),
        (90, "kp0"),
        (91, "kpdecimal"),
        (104, "kpenter"),
        (106, "kpdivide"),
        (125, "kpequal"),
    ] {
        assert_eq!(
            keys::spec_for(&format!("SUPER + code:{code}")).as_deref(),
            Ok(format!("super+{spec}").as_str()),
            "code:{code}"
        );
    }
}

#[test]
fn a_chord_of_nothing_but_modifiers_is_not_a_binding() {
    assert_eq!(
        keys::spec_for("SUPER + SHIFT"),
        Err(keys::KeyTrouble::NoKey)
    );
    assert_eq!(keys::spec_for(""), Err(keys::KeyTrouble::NoKey));
    assert_eq!(keys::spec_for("   "), Err(keys::KeyTrouble::NoKey));
}

#[test]
fn command_names_are_a_function_of_the_argv_and_nothing_else() {
    let a = dispatch::command_name(&["omarchy-menu".into(), "toggle".into(), "apps".into()]);
    let b = dispatch::command_name(&["omarchy-menu".into(), "toggle".into(), "apps".into()]);
    assert_eq!(a, b, "the same argv must always name the same command");
    assert!(a.starts_with("hypr:omarchy-menu-toggle-apps"), "{a}");
    assert_ne!(
        a,
        dispatch::command_name(&["omarchy-menu".into(), "toggle".into()])
    );
    // Bounded, because a name is a log line and a docs-table row.
    let long = dispatch::command_name(&[(0..500).map(|_| 'x').collect::<String>()]);
    assert!(long.len() <= 64, "{}", long.len());
    // Two argvs that agree for the whole readable half still get
    // different names, because the fingerprint is taken over all of it.
    let a_long = dispatch::command_name(&["x".repeat(200) + "a"]);
    let b_long = dispatch::command_name(&["x".repeat(200) + "b"]);
    assert_ne!(
        a_long, b_long,
        "truncation must cost legibility, never uniqueness"
    );
}

/// Omarchy's `shell_quote` produces single-quoted arguments that must
/// survive as *one* argument each.
#[test]
fn quoted_arguments_survive_the_split() {
    assert_eq!(
        dispatch::split_command("omarchy-launch-or-focus '^obsidian$' 'uwsm-app -- obsidian'"),
        vec![
            "omarchy-launch-or-focus",
            "^obsidian$",
            "uwsm-app -- obsidian"
        ]
    );
    assert_eq!(dispatch::split_command("  "), Vec::<String>::new());
}

// ---- hostile input ----------------------------------------------------

/// The rule for this module is absolute: a malformed file is a logged
/// warning and a skipped line, never a panic and never a refusal to
/// start. These are the inputs that would break a reader written
/// without that rule in mind.
///
/// Each case is run through **both** front ends, because "this cannot
/// happen in Lua" is exactly the assumption that turns into a panic
/// when somebody names a `.conf` file `.lua`.
#[test]
fn hostile_input_never_panics_and_always_yields_something() {
    let deep_tables = format!("o.window({}{})", "{ match = ".repeat(400), "}".repeat(400));
    let deep_parens = format!("hl.bind({}\"a\"{})", "(".repeat(2000), ")".repeat(2000));
    let hostile: Vec<String> = vec![
        // Unterminated everything.
        "o.bind(\"SUPER + W\"".into(),
        "o.bind(\"unterminated string".into(),
        "--[[ unterminated long comment".into(),
        "o.window({ class = \"x\"".into(),
        "for i = 1, 10 do".into(),
        "if true then".into(),
        "function f(".into(),
        "[[".into(),
        // Nesting deep enough to blow a recursive parser's stack.
        deep_tables,
        deep_parens,
        format!("o.bind({}", "{".repeat(5000)),
        // Loops asking for the world.
        "for i = 1, 100000000 do o.bind(\"SUPER + \" .. i, nil, \"x\") end".into(),
        "for i = 1, 1e400 do o.bind(\"SUPER + W\", nil, \"x\") end".into(),
        "for i = 10, 1 do o.bind(\"SUPER + W\", nil, \"x\") end".into(),
        // Numbers and values that are not.
        "hl.monitor({ scale = 0/0, mode = 1e999 })".into(),
        "o.bind(nil, nil, nil)".into(),
        "o.bind(0x, 0xzz, 1.2.3.4)".into(),
        "windowrule = size 99999999999999999999 -0, match:class x".into(),
        "windowrule = size -1 -1, match:class x".into(),
        // Patterns that would hang a backtracking engine, and one no
        // engine should compile.
        "windowrule = float on, match:class (a+)+$".into(),
        "windowrule = float on, match:class (((((((((((a)))))))))))*[".into(),
        format!(
            "windowrule = float on, match:class {}",
            "a{100}{100}{100}".repeat(20)
        ),
        // Key specs from nowhere.
        "bind = SUPER, code:4294967295, killactive,".into(),
        "bind = SUPER, code:-1, killactive,".into(),
        "bind = , , ,".into(),
        "bind = ,,,,,,,,,,,,,,".into(),
        "bindddddd = SUPER, W, a, b, c, d, e".into(),
        // Structure that is not.
        "= = =".into(),
        "}}}}}}".into(),
        "\0\0\0\0".into(),
        "env = ".into(),
        "env = ,".into(),
        "$ = $".into(),
        "$a = $a".into(),
        "source = ".into(),
        "source = /".into(),
        "source = ~/../../../../../../etc/passwd".into(),
        "require(\"../../../../etc/passwd\")".into(),
        "require(\"\")".into(),
        // A very long line, and a very wide one.
        format!("bind = SUPER, W, exec, {}", "x".repeat(200_000)),
        "a".repeat(500_000),
        (0..5000)
            .map(|i| format!("bind = SUPER, W, exec, cmd{i}\n"))
            .collect(),
    ];
    for source in hostile {
        let mut globals = lua::Globals::default();
        let mut out = Vec::new();
        lua::read(
            &source,
            &lua::Facts {
                path: Vec::new(),
                home: None,
                state_home: None,
            },
            &mut globals,
            &mut out,
        );
        let mut vars = BTreeMap::new();
        let mut out2 = Vec::new();
        conf::read(&source, &mut vars, &mut out2);
        // Whatever came out, lowering it must also be total.
        for stream in [out, out2] {
            let reading = lower(
                stream,
                LoadReport {
                    files: Vec::new(),
                    skipped: Vec::new(),
                },
            );
            // Nothing bound to a chord this desktop cannot express.
            for (combo, _) in &reading.keybindings {
                assert!(
                    combo.keysym != 0,
                    "bound a null keysym from {:?}",
                    &source[..source.len().min(60)]
                );
            }
        }
    }

    // Shapes that recurse or multiply rather than nest: a name bound to
    // itself, operator chains longer than a stack, values that grow each
    // time they are rebound, and loops whose counts multiply. Each group
    // of files is read in order through one `Globals`, the way
    // `helpers.lua` and a user's file share them, and must end in a
    // skip that names the bound it hit.
    let facts = lua::Facts {
        path: Vec::new(),
        home: None,
        state_home: None,
    };
    let nested_loops = |body: &str| {
        format!(
            "{}{body}{}",
            "for i = 1, 64 do\n".repeat(5),
            "end\n".repeat(5)
        )
    };
    let bounded: Vec<(Vec<String>, &str)> = vec![
        (
            vec!["o = o or {}\n".into(), "if o then hl.env(\"A\", \"B\") end\n".into()],
            "cannot answer",
        ),
        (vec!["a = b\nb = a\nif b then hl.env(\"A\", \"B\") end\n".into()], "cannot answer"),
        (vec!["local x = x or false\nif x then hl.env(\"A\", \"B\") end\n".into()], "cannot answer"),
        (
            vec![format!("if {}true then hl.env(\"A\", \"B\") end", "not ".repeat(200_000))],
            "nested too deeply",
        ),
        (
            vec![format!("if {}1 then hl.env(\"A\", \"B\") end", "- ".repeat(200_000))],
            "nested too deeply",
        ),
        (
            vec![format!("local x = a{}\nif x then hl.env(\"A\", \"B\") end", " .. a".repeat(150_000))],
            "too long",
        ),
        (
            vec![format!(
                "local x = {}a{}\nif x then hl.env(\"A\", \"B\") end",
                "(".repeat(24),
                format!("){}", " .. a".repeat(16)).repeat(24)
            )],
            "too long",
        ),
        (
            vec![format!("local x = {{}}\n{}if x then hl.env(\"A\", \"B\") end", "x = { x }\n".repeat(19_000))],
            "too large",
        ),
        (
            vec![format!("local x = {{}}\n{}if x then hl.env(\"A\", \"B\") end", "x = { x, x }\n".repeat(64))],
            "too large",
        ),
        (
            vec![format!("local x = \"ab\"\n{}if x then hl.env(\"A\", \"B\") end", "x = x .. x\n".repeat(64))],
            "too large",
        ),
        (vec![nested_loops("hl.env(\"A\", \"B\")\n")], "directives"),
        (vec![nested_loops("")], "statements walked"),
        (vec![nested_loops("local y = 1\n")], "statements walked"),
        (vec!["local y = 1\n".repeat(20_001)], "per-file limit"),
    ];
    for (sources, needle) in bounded {
        let mut globals = lua::Globals::default();
        let mut out = Vec::new();
        for source in &sources {
            out.clear();
            lua::read(source, &facts, &mut globals, &mut out);
        }
        let shape = &sources.last().unwrap()[..sources.last().unwrap().len().min(60)];
        assert!(
            out.iter()
                .any(|d| matches!(d, Directive::Ignored { detail, .. } if detail.contains(needle))),
            "{shape:?} must be skipped naming its bound ({needle:?}): {:?}",
            &out[..out.len().min(4)]
        );
        assert!(
            out.len() <= lua::MAX_DIRECTIVES + 1,
            "{shape:?} produced {} directives",
            out.len()
        );
        if needle == "cannot answer" {
            assert!(
                !out.iter().any(|d| matches!(d, Directive::Env { .. })),
                "{shape:?} ran a block whose condition it could not answer"
            );
        }
    }
    // ...and every one of those bounds sits far above what a real
    // Omarchy tree spends, so none of them costs a real binding.
    let real = read(&machine());
    assert!(
        !real.skipped.iter().any(|skip| ["the rest is not read", "too large", "too long", "nested too deeply"]
            .iter()
            .any(|bound| skip.what.contains(bound))),
        "the captured machine hit a reader bound: {:?}",
        real.skipped
    );
    assert!(
        real.bindings.len() + real.skipped.len() < lua::MAX_DIRECTIVES / 16,
        "the directive bound is no longer far above a real tree's output"
    );
}

/// The same, over bytes that are not text at all.
#[test]
fn arbitrary_bytes_are_read_without_panicking() {
    let mut seed = 0x12345678u32;
    let mut next = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 16) as u8
    };
    for case in 0..400 {
        let len = (case * 7) % 900 + 1;
        let bytes: Vec<u8> = (0..len).map(|_| next()).collect();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut globals = lua::Globals::default();
        let mut out = Vec::new();
        lua::read(
            &text,
            &lua::Facts {
                path: Vec::new(),
                home: None,
                state_home: None,
            },
            &mut globals,
            &mut out,
        );
        let mut vars = BTreeMap::new();
        let mut out2 = Vec::new();
        conf::read(&text, &mut vars, &mut out2);
    }
}

/// Fragments of the machine's real files, cut at every byte boundary.
/// Truncation is what a reader actually meets in the wild — a config
/// written by a process that was killed halfway, or a file being
/// rewritten as this reads it — and it produces malformed input that
/// is *almost* valid, which is where parsers break.
#[test]
fn every_truncation_of_a_real_file_is_read_without_panicking() {
    for name in [
        "machine/omarchy/default/hypr/bindings/tiling.lua",
        "machine/omarchy/default/hypr/apps/pip.lua",
    ] {
        let text = std::fs::read_to_string(fixtures().join(name)).unwrap();
        let chars: Vec<char> = text.chars().collect();
        for cut in (0..chars.len()).step_by(3) {
            let fragment: String = chars[..cut].iter().collect();
            let mut globals = lua::Globals::default();
            let mut out = Vec::new();
            lua::read(
                &fragment,
                &lua::Facts {
                    path: Vec::new(),
                    home: None,
                    state_home: None,
                },
                &mut globals,
                &mut out,
            );
        }
    }
    for name in [
        "conf-machine/omarchy/default/hypr/bindings/tiling-v2.conf",
        "conf-machine/omarchy/default/hypr/apps/system.conf",
    ] {
        let text = std::fs::read_to_string(fixtures().join(name)).unwrap();
        let chars: Vec<char> = text.chars().collect();
        for cut in (0..chars.len()).step_by(3) {
            let fragment: String = chars[..cut].iter().collect();
            let mut vars = BTreeMap::new();
            let mut out = Vec::new();
            conf::read(&fragment, &mut vars, &mut out);
        }
    }
}

/// A config file must never be a code-execution path into the window
/// manager. The two conditions Omarchy branches on are file-system
/// questions and are answered as such; anything that would need a
/// shell is refused, and the refusal is visible.
#[test]
fn a_condition_that_would_need_a_shell_is_refused_rather_than_run() {
    let marker = scratch("noexec").join("PWNED");
    let source = format!(
        "if o.shell_succeeds(\"touch {}\") then o.bind(\"SUPER + W\", nil, \"x\") end\n",
        marker.display()
    );
    let mut globals = lua::Globals::default();
    let mut out = Vec::new();
    lua::read(
        &source,
        &lua::Facts {
            path: Vec::new(),
            home: None,
            state_home: None,
        },
        &mut globals,
        &mut out,
    );
    assert!(!marker.exists(), "a config file must never run anything");
    assert!(
        out.iter().any(
            |d| matches!(d, Directive::Ignored { detail, .. } if detail.contains("cannot answer"))
        ),
        "and the refusal must be visible: {out:?}"
    );
    let _ = std::fs::remove_dir_all(marker.parent().unwrap());
}

/// Reads Lua sources in order through one `Globals`, on a machine with
/// nothing on its `PATH`, and returns what the last one produced.
fn lua_out(sources: &[&str]) -> Vec<Directive> {
    let facts = lua::Facts {
        path: Vec::new(),
        home: None,
        state_home: None,
    };
    let mut globals = lua::Globals::default();
    let mut out = Vec::new();
    for source in sources {
        out.clear();
        lua::read(source, &facts, &mut globals, &mut out);
    }
    out
}

/// Which branch of a `RAN` probe ran, if either did.
fn branch(out: &[Directive]) -> Option<&str> {
    out.iter().find_map(|d| match d {
        Directive::Env { name, value } if name == "RAN" => Some(value.as_str()),
        _ => None,
    })
}

/// Branch conditions mean what Lua 5.4 says they mean. `and`, `or` and
/// `not` combine answers, `==` and `~=` compare values, and an unset
/// name is `nil`. Every expected answer in the table is Lua's own, for
/// `x` as a global and as a local.
///
/// A condition that depends on something only running code could know
/// decides nothing, unless the other operand decides it — and a name
/// that some construct the reader skipped could have set is not
/// confidently `nil`.
#[test]
fn conditions_are_answered_the_way_lua_answers_them() {
    let conditions = [
        "x and y",
        "x or z",
        "not x",
        "y and not x",
        "x == \"y\"",
        "x ~= nil",
        "x ~= false",
    ];
    let table: [(&str, [bool; 7]); 5] = [
        ("", [false, false, true, true, false, false, true]),
        ("x = nil", [false, false, true, true, false, false, true]),
        ("x = false", [false, false, true, true, false, true, false]),
        ("x = true", [true, true, false, false, false, true, true]),
        ("x = \"y\"", [true, true, false, false, true, true, true]),
    ];
    for (setting, answers) in table {
        for (condition, answer) in conditions.iter().zip(answers) {
            for scope in ["", "local "] {
                let setting = match setting {
                    "" => String::new(),
                    setting => format!("{scope}{setting}\n"),
                };
                let source = format!(
                    "y = true\nz = false\n{setting}if {condition} then hl.env(\"RAN\", \"then\") else hl.env(\"RAN\", \"else\") end\n"
                );
                assert_eq!(
                    branch(&lua_out(&[&source])),
                    Some(if answer { "then" } else { "else" }),
                    "{source}"
                );
            }
        }
    }

    // Three-valued: an unanswerable operand, and what decides it anyway.
    for (condition, expected) in [
        ("o.shell_succeeds(\"x\") and false", Some("else")),
        ("false and o.shell_succeeds(\"x\")", Some("else")),
        ("o.shell_succeeds(\"x\") or true", Some("then")),
        ("o.cmd_present(\"no-such-tool-xyz\") or true", Some("then")),
        ("o.cmd_present(\"no-such-tool-xyz\") and true", Some("else")),
        ("o.shell_succeeds(\"x\") and true", None),
        ("not o.shell_succeeds(\"x\")", None),
        ("o.shell_succeeds(\"x\") == nil", None),
        ("hl ~= nil", None),
        ("vconsole.XKBLAYOUT == nil", None),
    ] {
        let source = format!(
            "if {condition} then hl.env(\"RAN\", \"then\") else hl.env(\"RAN\", \"else\") end\n"
        );
        assert_eq!(branch(&lua_out(&[&source])), expected, "{source}");
    }

    // Possibly set: each of these could have assigned `w` without the
    // reader seeing it, so `w ~= nil` is not answered either way.
    let probe = "if w ~= nil then hl.env(\"RAN\", \"then\") else hl.env(\"RAN\", \"else\") end\n";
    for prelude in [
        "if o.shell_succeeds(\"x\") then w = 1 end\n",
        "if o.shell_succeeds(\"x\") then else w = 1 end\n",
        "while true do w = 1 end\n",
        "repeat w = 1 until true\n",
        "function set() w = 1 end\n",
        "local function set() _G.w = 1 end\n",
        "for _, v in pairs(os.environ()) do w = v end\n",
        "hl.timer(function() w = 1 end)\n",
    ] {
        let source = format!("{prelude}{probe}");
        let out = lua_out(&[&source]);
        assert_eq!(branch(&out), None, "{source}");
        assert!(
            out.iter().any(|d| matches!(d, Directive::Ignored { detail, .. } if detail.contains("w ~= nil"))),
            "the probe must be read and left unanswered, not swallowed: {source}\n{out:?}"
        );
    }
    assert_eq!(
        branch(&lua_out(&["while true do w = 1 end\n", probe])),
        None,
        "possibly set in one file is possibly set in the next"
    );
    assert_eq!(
        branch(&lua_out(&["function set() local w = 1 end\n", probe])),
        Some("else"),
        "a local inside a skipped function is not the global"
    );
    assert_eq!(
        branch(&lua_out(&["while true do w = 1 end\nw = 2\n", probe])),
        Some("then"),
        "a readable assignment after the skipped one is the value"
    );
}

/// `if` must be followed by a condition and then `then`. Anything else
/// is an `if` this reader has misread, and its body is skipped whole
/// rather than walked as though the rest of the line were a statement.
#[test]
fn an_if_without_then_is_skipped_whole() {
    for source in [
        "if true garbage then hl.env(\"A\", \"B\") end\nhl.env(\"AFTER\", \"1\")\n",
        "if false then elseif true garbage then hl.env(\"A\", \"B\") end\nhl.env(\"AFTER\", \"1\")\n",
    ] {
        let out = lua_out(&[source]);
        assert!(
            out.iter().any(|d| matches!(d, Directive::Ignored { detail, .. } if detail.contains("unreadable condition"))),
            "{source}: {out:?}"
        );
        assert!(
            !out.iter().any(|d| matches!(d, Directive::Env { name, .. } if name == "A")),
            "{source}: {out:?}"
        );
        assert!(
            out.iter().any(|d| matches!(d, Directive::Env { name, .. } if name == "AFTER")),
            "reading must resume after the skipped if: {out:?}"
        );
    }
}

/// The module promises that everything it meets and does not act on is
/// logged. Calls were the gap: Omarchy's animation curves, its
/// persisted touchpad disable and every runtime-only call vanished.
/// Of the sixteen `hl.animation` leaves, the switch on each is read;
/// fourteen carry a speed and curve, declined by leaf, and fourteen
/// name a transition this desktop does not have, declined by leaf.
#[test]
fn every_call_the_lua_reader_meets_is_recorded() {
    let reading = read(&machine());
    let count = |kind: &str, needle: &str| {
        reading
            .skipped
            .iter()
            .filter(|skip| skip.kind == kind && skip.what.contains(needle))
            .count()
    };
    assert_eq!(count("animation", "hl.curve("), 5, "{:?}", reading.skipped);
    assert_eq!(count("animation", "hl.animation leaf"), 14, "{:?}", reading.skipped);
    assert_eq!(count("animation", "(enabled = "), 14, "{:?}", reading.skipped);
    assert_eq!(count("animation", "hl.animation("), 0);
    // Omarchy's persisted touchpad and touchscreen disables are read as
    // data now, and the captured machine has none.
    assert_eq!(count("lua-call", "disabled_input_device("), 0);
    assert!(reading.input.devices.is_empty(), "{:?}", reading.input.devices);
    assert_eq!(count("include", "dofile("), 1);
    let out = lua_out(&[concat!(
        "cover(0)\n",
        "fit()\n",
        "hl.device(settings)\n",
        "hl.workspace_rule({ workspace = \"1\" })\n",
        "hl.dispatch(hl.dsp.window.close())\n",
        "hl.timer(function() end, { timeout = 10 })\n",
    )]);
    for call in ["cover(", "fit(", "hl.device(", "hl.workspace_rule(", "hl.dispatch(", "hl.timer("] {
        assert!(
            out.iter().any(|d| matches!(d, Directive::Ignored { detail, .. } if detail.starts_with(call))),
            "{call} was dropped silently: {out:?}"
        );
    }
    // ...and no arm of the call reader drops a call without a line.
    const SOURCE: &str = include_str!("lua.rs");
    let body = &SOURCE[SOURCE.find("fn emit_call(").expect("emit_call")..];
    let body = &body[..body.find("\n}\n").expect("the end of emit_call")];
    assert!(!body.contains("=> {}"), "emit_call has an arm that drops a call silently");
}

/// An include graph that points at itself terminates, whichever
/// syntax it is written in.
#[test]
fn a_cyclic_include_graph_terminates() {
    let root = scratch("cycle");
    write(
        &root.join(".config/hypr/hyprland.conf"),
        "source = ~/.config/hypr/b.conf\nbind = SUPER, W, killactive,\n",
    );
    write(
        &root.join(".config/hypr/b.conf"),
        "source = ~/.config/hypr/hyprland.conf\nbind = SUPER, E, killactive,\n",
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(action_for(&reading, "super+w"), Some(Action::Close));
    assert_eq!(action_for(&reading, "super+e"), Some(Action::Close));

    // ...and through a symlink loop, which canonicalization is what
    // actually catches.
    let _ = std::fs::remove_file(root.join(".config/hypr/b.conf"));
    std::os::unix::fs::symlink(
        root.join(".config/hypr/hyprland.conf"),
        root.join(".config/hypr/b.conf"),
    )
    .unwrap();
    let reading = read(&Roots::under(&root));
    assert_eq!(action_for(&reading, "super+w"), Some(Action::Close));
    let _ = std::fs::remove_dir_all(&root);
}

/// A `source =` line naming a path outside the config tree is followed
/// only where Hyprland itself would follow it — but a *module* name
/// with `..` in it is refused outright, because a Lua module name is
/// not a path and treating it as one is how a config file reads
/// `/etc/shadow`.
#[test]
fn a_module_name_cannot_climb_out_of_the_search_path() {
    assert_eq!(module_relative("../../etc/passwd"), None);
    assert_eq!(
        module_relative("a..b"),
        None,
        "an empty path segment is not a module name"
    );
    assert_eq!(module_relative("a/b"), None);
    assert_eq!(module_relative(""), None);
    assert_eq!(
        module_relative("default.hypr.omarchy"),
        Some(PathBuf::from("default/hypr/omarchy"))
    );
}

/// A file bigger than the per-file budget is skipped with a reason
/// rather than read into memory.
#[test]
fn an_enormous_file_is_skipped_with_a_reason() {
    let root = scratch("huge");
    let entry = root.join(".config/hypr/hyprland.conf");
    write(
        &entry,
        &"# comment\n".repeat((MAX_FILE_BYTES as usize / 10) + 100),
    );
    let reading = read(&Roots::under(&root));
    assert!(reading.files.is_empty());
    assert!(
        reading
            .skipped
            .iter()
            .any(|s| s.why.contains("larger than")),
        "{:?}",
        reading.skipped
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A maintainer's-eye summary of what this machine's configuration
/// produces. Not an assertion — the numbers move whenever Omarchy
/// ships — but the fastest way to see what a new Omarchy release did
/// to the read, and where `docs/hyprland-config.md`'s counts come from.
#[test]
#[ignore = "diagnostic: summarises what the captured machine produces"]
fn debug_summary() {
    let reading = read(&machine());
    println!("files read      {}", reading.files.len());
    println!("bindings        {}", reading.keybindings.len());
    println!(
        "  verbs         {}",
        reading
            .keybindings
            .iter()
            .filter(|(_, a)| !matches!(a, Action::Run(_)))
            .count()
    );
    println!(
        "  run           {}",
        reading
            .keybindings
            .iter()
            .filter(|(_, a)| matches!(a, Action::Run(_)))
            .count()
    );
    println!("commands        {}", reading.commands.len());
    println!("env             {}", reading.env.len());
    println!("autostart       {}", reading.autostart.len());
    println!("float rules     {}", reading.float_rules.len());
    println!("monitor lines   {}", reading.monitors.lines.len());
    println!("skipped         {}", reading.skipped.len());
    let mut by_kind: std::collections::BTreeMap<&str, usize> = Default::default();
    for skip in &reading.skipped {
        *by_kind.entry(skip.kind.as_str()).or_default() += 1;
    }
    for (kind, n) in &by_kind {
        println!("  {kind:<14}{n}");
    }
    println!("\nverbs:");
    for (combo, action) in &reading.keybindings {
        if !matches!(action, Action::Run(_)) {
            println!("  {:?} {:#x} -> {action:?}", combo.modifiers, combo.keysym);
        }
    }
    println!("\nfloat rules:");
    for d in reading.float_rules.descriptions() {
        println!("  {d}");
    }
    println!("\nmonitors:");
    for m in &reading.monitors.lines {
        println!("  {m:?}");
    }
    println!("\nautostart:");
    for a in &reading.autostart {
        println!("  {}", a.join(" "));
    }
    println!("\nenv:");
    for (n, v) in &reading.env {
        println!("  {n}={v}");
    }
}

// ---- the documentation, pinned ----------------------------------------

/// The reference and the guide have to describe the switch that
/// actually exists.
///
/// A config reference nobody can paste from is worse than none — the
/// same argument `example_doc.rs` makes about its own examples — and
/// this one gates a feature that reads somebody else's files, so
/// "how do I turn it off" has to be findable and correct.
#[test]
fn the_documented_switch_is_the_real_one() {
    const REFERENCE: &str = include_str!("../../../../docs/config.example.toml");
    const GUIDE: &str = include_str!("../../../../docs/hyprland-config.md");
    assert!(
        REFERENCE.contains("#hyprland_config = false"),
        "the reference must show the key, commented like its neighbours"
    );
    for spelling in ["hyprland_config = false", "hyprland_config = true"] {
        assert!(
            GUIDE.contains(spelling),
            "docs/hyprland-config.md must document `{spelling}`"
        );
    }
    // Both spellings parse, and each does what the documents claim.
    let off = crate::parse("desktop = \"omarchy\"\nhyprland_config = false\n").expect("documented");
    assert!(
        !wanted(&off),
        "`hyprland_config = false` must be the escape hatch from any posture"
    );
    let on = crate::parse("hyprland_config = true\n").expect("documented");
    assert!(
        wanted(&on),
        "`hyprland_config = true` must work on a plain chonkstep desk"
    );
    // ...and the posture-decides default the documents lead with.
    let posture = crate::parse("desktop = \"omarchy\"").expect("the one-liner");
    assert!(wanted(&posture));
    assert!(
        !wanted(&crate::parse("").expect("empty")),
        "a plain chonkstep desk reads nobody else's files"
    );
}

/// Every `Unbound` reason this reader can hand a user has to be one
/// the guide explains, so a log line is something they can look up
/// rather than a dead end.
#[test]
fn every_reason_a_binding_can_be_refused_for_is_explained_somewhere() {
    const GUIDE: &str = include_str!("../../../../docs/hyprland-config.md");
    const CARD: &str = include_str!("../../../../docs/keybindings.md");
    let named = [
        crate::preset::Unbound::TilingOnly,
        crate::preset::Unbound::HyprlandOnly,
        crate::preset::Unbound::NoVerb,
        crate::preset::Unbound::NotAKey,
        crate::preset::Unbound::Conditional,
        crate::preset::Unbound::Declined,
    ];
    let scripts = dispatch::UNSERVED_OMARCHY_SCRIPTS.iter().map(|(_, reason)| *reason);
    for reason in named.into_iter().chain(scripts) {
        let text = reason.reason();
        assert!(
            GUIDE.contains(text) || CARD.contains(text) || explained_in_prose(GUIDE, reason),
            "no document explains {text:?}"
        );
    }
}

/// The guide explains a reason in its own words rather than by
/// quoting the enum's one-liner; this is the phrase that stands in for
/// each.
fn explained_in_prose(guide: &str, reason: crate::preset::Unbound) -> bool {
    let phrase = match reason {
        crate::preset::Unbound::TilingOnly => "there is nothing to split",
        crate::preset::Unbound::HyprlandOnly => "only when every request it sends is proven served",
        // A script's reason is specific to it, so the guide has to quote it.
        crate::preset::Unbound::Unserved(text) => text,
        crate::preset::Unbound::NoVerb => "has no verb for",
        crate::preset::Unbound::NotAKey => "Not key chords; this config format cannot express one",
        crate::preset::Unbound::Conditional => "answered by asking the file system",
        crate::preset::Unbound::Declined => "declined on purpose",
    };
    guide.contains(phrase)
}

/// The counts the documents quote off this machine are the counts this
/// reader actually produces from it.
///
/// `docs/omarchy-mode.md` tells a reader what they gain by having a
/// real Omarchy configuration rather than the baked table — "167
/// bindings over 120 commands, against the baked table's 151 over 83",
/// and 38 float rules where the hardcoded one had a single prefix.
/// Those numbers are the argument for the whole module, and a number
/// in prose is the first thing to go stale. Pinned here against the
/// captured machine, so a fixture update or a mapping change has to
/// update the prose with it.
#[test]
fn the_numbers_the_documents_quote_are_the_numbers_this_machine_produces() {
    const MODE: &str = include_str!("../../../../docs/omarchy-mode.md");
    let reading = read(&machine());
    assert_eq!(
        reading.keybindings.len(),
        189,
        "bindings read from the captured machine"
    );
    assert_eq!(
        reading.commands.len(),
        121,
        "commands declared for global and scoped bindings"
    );
    assert_eq!(
        reading.float_rules.len(),
        43,
        "window behaviors resolved through Omarchy's tags"
    );
    // The skipped count is quoted too, in the guide's sample log line.
    // It is by far the largest number this module reports, and a reader
    // who has just been told "ignore loudly" needs to see that a big
    // number there is the normal case rather than a fault.
    assert_eq!(
        reading.skipped.len(),
        144,
        "directives this desktop has its own answer for"
    );
    const GUIDE: &str = include_str!("../../../../docs/hyprland-config.md");
    assert!(
        MODE.contains("189\nbindings over 121 commands") || MODE.contains("189 bindings over 121 commands"),
        "docs/omarchy-mode.md no longer quotes the 189 bindings over 121 commands this machine produces"
    );
    assert!(
        GUIDE.contains("files=42 bindings=189 commands=121 env=8 autostart=4")
            && GUIDE.contains("float_rules=43 monitors=1 skipped=144"),
        "the guide's sample log line no longer matches what this machine reports"
    );
}

#[test]
fn screensaver_defaults_survive_unrelated_rules_and_allow_explicit_opt_out() {
    let root = scratch("screensaver-defaults");
    let path = root.join(".config/hypr/hyprland.conf");
    write(&path, "windowrule = float on, match:class ^notes$\ninput {\n touchpad {\n  disable_while_typing = false\n }\n}\n");
    let reading = read(&Roots::under(&root));
    assert_eq!(reading.input.disable_while_typing, Some(false));
    assert!(reading.float_rules.window_decision_for("org.omarchy.screensaver", "foot", "").fullscreen);
    for other in ["foot", "org.omarchy.btop", "org.omarchy.screensaver.settings"] {
        assert!(!reading.float_rules.window_decision_for(other, "org.omarchy.screensaver", "").fullscreen);
    }
    write(&path, "windowrule = fullscreen off, match:class ^org\\.omarchy\\.screensaver$\n");
    let reading = read(&Roots::under(&root));
    assert!(!reading.float_rules.window_decision_for("org.omarchy.screensaver", "foot", "").fullscreen);
}

// ---- Omarchy's toggle directory ---------------------------------------

/// A scratch home wearing the fixture's Omarchy defaults, so the real
/// `toggles.lua` — `require_all.files(toggles_dir, nil, { exclude = … })`
/// over `paths.state_home .. "/omarchy/toggles/hypr"` — is what the
/// reader meets.
fn scratch_with_omarchy_defaults(tag: &str) -> PathBuf {
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).unwrap();
            }
        }
    }
    let root = scratch(tag);
    copy_tree(&fixtures().join("machine/omarchy"), &root.join("omarchy"));
    root
}

/// Omarchy's clamshell and laptop-display toggles each write one
/// `hl.monitor({ output = …, disabled = true })` line into the toggle
/// directory and reload. Those two files are read; the two legacy names
/// Omarchy's own loader excludes are not, whatever they contain.
#[test]
fn omarchys_toggle_directory_is_read_and_its_excluded_names_are_not() {
    let root = scratch_with_omarchy_defaults("toggles-dir");
    write(&root.join(".config/hypr/hyprland.lua"), "require(\"default.hypr.toggles\")\n");
    let toggles = root.join(".local/state/omarchy/toggles/hypr");
    write(
        &toggles.join("internal-monitor-clamshell.lua"),
        "hl.monitor({ output = \"eDP-1\", disabled = true })\n",
    );
    write(
        &toggles.join("internal-monitor-disable.lua"),
        "hl.monitor({ output = \"DP-3\", disabled = true })\n",
    );
    for legacy in ["touchpad-disabled", "touchscreen-disabled"] {
        write(
            &toggles.join(format!("{legacy}.lua")),
            "hl.monitor({ output = \"NEVER\", disabled = true })\no.bind(\"SUPER + F11\", nil, \"never\")\n",
        );
    }
    // Not a toggle: the loader takes `*.lua` only, as Omarchy's does.
    write(&toggles.join("notes.txt"), "hl.monitor({ output = \"NEVER\", disabled = true })\n");

    let reading = read(&Roots::under(&root));
    let disabled: Vec<(&str, &[String])> = reading
        .monitors
        .lines
        .iter()
        .map(|line| (line.output.as_str(), line.extra.as_slice()))
        .collect();
    assert_eq!(
        disabled,
        [("eDP-1", &["disabled".to_string(), "on".to_string()][..]), ("DP-3", &["disabled".to_string(), "on".to_string()][..])],
        "{:?}",
        reading.skipped
    );
    assert!(
        reading.files.iter().any(|file| file.ends_with("internal-monitor-clamshell.lua"))
            && reading.files.iter().any(|file| file.ends_with("internal-monitor-disable.lua")),
        "{:?}",
        reading.files
    );
    assert!(
        !reading.files.iter().any(|file| file.ends_with("touchpad-disabled.lua") || file.ends_with("touchscreen-disabled.lua")),
        "an excluded name must never be read: {:?}",
        reading.files
    );
    assert!(action_for(&reading, "super+f11").is_none());
    assert!(
        !reading.skipped.iter().any(|skip| skip.what.contains("no module prefix")),
        "the toggle fan-out is followed, not recorded as ignored: {:?}",
        reading.skipped
    );
}

/// Only the one shape Omarchy writes is followed. A nil-prefix fan-out
/// over any other directory expression is still recorded as ignored,
/// and a suffix that would climb out of the state home is refused.
#[test]
fn only_omarchys_toggle_fan_out_is_followed_without_a_module_prefix() {
    let root = scratch_with_omarchy_defaults("toggles-dir-shape");
    write(
        &root.join(".local/state/etc/marker.lua"),
        "o.bind(\"SUPER + F11\", nil, \"never\")\n",
    );
    write(
        &root.join(".config/hypr/hyprland.lua"),
        concat!(
            "local paths = require(\"default.hypr.paths\")\n",
            "local require_all = require(\"default.hypr.require_all\")\n",
            "local elsewhere = require(\"default.hypr.helpers\")\n",
            "require_all.files(paths.config_home .. \"/hypr\", nil, {})\n",
            "require_all.files(\"/etc\", nil)\n",
            "require_all.files(elsewhere.state_home .. \"/etc\", nil)\n",
            "require_all.files(paths.state_home .. \"/omarchy/../etc\", nil)\n",
            "require_all.files(paths.state_home .. \"etc\", nil)\n",
            "require_all.files(paths.state_home .. \"/etc\", nil, { exclude = { [\"marker\"] = true } })\n",
        ),
    );
    let reading = read(&Roots::under(&root));
    assert_eq!(
        reading.skipped.iter().filter(|skip| skip.what.contains("require_all.files with no module prefix")).count(),
        5,
        "{:?}",
        reading.skipped
    );
    assert!(action_for(&reading, "super+f11").is_none(), "an excluded file is skipped");
    assert!(!reading.files.iter().any(|file| file.ends_with("marker.lua")), "{:?}", reading.files);
}
