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
        None,
        "toggle split must not be approximated"
    );
    assert_eq!(
        skipped_why(&reading, "SUPER + J"),
        Some(crate::preset::Unbound::TilingOnly.reason().to_string()),
        "and the reason has to be recorded, not implied"
    );
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

/// Hyprland's `movetoworkspacesilent` moves a window *without*
/// following it. The translated action preserves that distinction from
/// `workspace-carry`, including Omarchy's tenth workspace on zero.
#[test]
fn moving_a_window_without_following_it_is_native() {
    let reading = read(&machine());
    assert_eq!(action_for(&reading, "super+shift+alt+1"), Some(Action::WorkspaceSend(0)));
    assert_eq!(action_for(&reading, "super+shift+alt+0"), Some(Action::WorkspaceSend(9)));
    // ...except for the scratchpad, where "silent" is the whole point
    // and `miniaturize` is the honest match. Omarchy's own
    // `SUPER + ALT + S`.
    assert_eq!(
        action_for(&reading, "super+alt+s"),
        Some(Action::Miniaturize)
    );
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

/// A chord that runs `hyprctl` or an `omarchy-hyprland-*` script
/// commands a compositor that is not running, and is left unbound —
/// the same filter `chonk_shell::omarchy_menu` applies to menu rows.
/// `hyprpicker` is deliberately *not* caught by it: it is an ordinary
/// layer-shell client and works here.
#[test]
fn bindings_that_command_hyprland_stay_unbound_and_hyprpicker_does_not() {
    let reading = read(&machine());
    assert_eq!(
        action_for(&reading, "super+backspace"),
        None,
        "omarchy-hyprland-window-transparency-toggle"
    );
    assert_eq!(
        action_for(&reading, "super+slash"),
        None,
        "omarchy-hyprland-monitor-scaling"
    );
    assert_eq!(
        skipped_why(&reading, "SUPER + BACKSPACE"),
        Some(crate::preset::Unbound::HyprlandOnly.reason().to_string())
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
  follow_mouse = 1,
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

/// Bindings that are not key chords at all — the mouse wheel, a mouse
/// button, the lid switch — are refused by name rather than mangled
/// into some nearby keysym.
#[test]
fn pointer_and_switch_bindings_are_refused_by_name() {
    let reading = read(&machine());
    assert!(skipped_why(&reading, "mouse_down").is_some_and(|w| w.contains("pointer or switch")));
    assert!(skipped_why(&reading, "mouse:272").is_some_and(|w| w.contains("pointer or switch")));
    assert!(skipped_why(&reading, "Lid Switch").is_some_and(|w| w.contains("pointer or switch")));
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
        .decision_for("org.omarchy.btop", "")
        .expect("Omarchy's own terminals float");
    assert_eq!(
        decision.size,
        Some(wm_core::Size::new(875, 600)),
        "the size the hardcoded rule was a transcription of"
    );
    assert!(decision.center);
    // The `.*` rules that carry no float property must not float
    // everything on the desk.
    assert_eq!(policy.decision_for("some-ordinary-app", "A window"), None);
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
        policy.decision_for("steam", "Steam").and_then(|d| d.size),
        Some(wm_core::Size::new(1100, 700))
    );
    // `apps/pip.lua`: picture-in-picture, matched on *title*.
    assert_eq!(
        policy
            .decision_for("firefox", "Picture-in-Picture")
            .and_then(|d| d.size),
        Some(wm_core::Size::new(600, 338)),
        "a title-matched rule, through the `pip` tag"
    );
    // `apps/system.lua`: the About box has its own size.
    assert_eq!(
        policy
            .decision_for("org.omarchy.about", "")
            .and_then(|d| d.size),
        Some(wm_core::Size::new(920, 480))
    );
    // `apps/localsend.lua`, whose pattern has no anchors — Hyprland
    // matches by search, so it catches the real class `localsend_app`.
    assert_eq!(
        policy
            .decision_for("localsend_app", "")
            .and_then(|d| d.size),
        Some(wm_core::Size::new(1100, 700))
    );
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

/// `size` given as a Hyprland layout expression needs a monitor to
/// evaluate against, which a config reader does not have. The rule's
/// float still applies; the size is dropped, loudly.
#[test]
fn a_size_expression_is_dropped_with_its_own_reason() {
    let rule = directive::WindowRule {
        matchers: vec![directive::Matcher::Class("^WebcamOverlay-small$".into())],
        props: vec![("size".into(), "(monitor_h*4/25) (monitor_h*9/50)".into())],
    };
    let (rules, notes) = rules::compile(&[rule]);
    assert_eq!(
        rules
            .decision_for("WebcamOverlay-small", "")
            .and_then(|d| d.size),
        None
    );
    assert!(
        notes.iter().any(|n| n.contains("needs a monitor")),
        "{notes:?}"
    );
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

/// Two autostart entries must not be carried: one commands Hyprland,
/// and one is a second copy of the shell this desktop already starts.
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
        Some(Action::WorkspaceNext)
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
        compiled.decision_for("v1app", "").is_some(),
        "v1 bare-pattern form"
    );
    assert!(
        compiled.decision_for("v2app", "Dialog").is_some(),
        "v2 colon form"
    );
    assert!(
        compiled.decision_for("v2app", "Other").is_none(),
        "v2 title matcher must actually constrain"
    );
    assert_eq!(
        compiled.decision_for("modern", "").and_then(|d| d.size),
        Some(wm_core::Size::new(400, 300)),
        "0.53+ match: form"
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

    let decision = rules.window_decision_for("player", "Cinema");
    assert!(decision.pin && decision.idle_inhibit);
    assert!(
        !decision.no_focus,
        "the later property overrides only no_focus"
    );
    assert!(decision.no_initial_focus);
    assert_eq!(decision.focus_on_activate, Some(false));
    assert!(decision.maximize && decision.fullscreen);
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
        rules.window_decision_for("notes", "").pin,
        "a supported sibling property remains effective"
    );
    assert!(
        !rules.window_decision_for("xterm", "").pin,
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

/// The read replaces the baked preset rather than merging with it —
/// the same replace-don't-merge rule, and the same reason: a chord
/// with two answers has no documented winner.
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
        reading.keybindings.len(),
        "replaced, not merged with, the {baked} baked entries"
    );
    assert!(config.float_policy.is_some());
    assert!(!config.session_env.is_empty());
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
    for reason in [
        crate::preset::Unbound::TilingOnly,
        crate::preset::Unbound::HyprlandOnly,
        crate::preset::Unbound::NoVerb,
        crate::preset::Unbound::NotAKey,
        crate::preset::Unbound::Conditional,
        crate::preset::Unbound::Declined,
    ] {
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
        crate::preset::Unbound::HyprlandOnly => "talks to a compositor that is not running",
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
/// real Omarchy configuration rather than the baked table — "153
/// bindings over 113 commands, against the baked table's 127 over 77",
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
        153,
        "bindings read from the captured machine"
    );
    assert_eq!(
        reading.commands.len(),
        113,
        "commands declared for global and scoped bindings"
    );
    assert_eq!(
        reading.float_rules.len(),
        45,
        "window behaviors resolved through Omarchy's tags"
    );
    // The skipped count is quoted too, in the guide's sample log line.
    // It is by far the largest number this module reports, and a reader
    // who has just been told "ignore loudly" needs to see that a big
    // number there is the normal case rather than a fault.
    assert_eq!(
        reading.skipped.len(),
        175,
        "directives this desktop has its own answer for"
    );
    const GUIDE: &str = include_str!("../../../../docs/hyprland-config.md");
    assert!(
        MODE.contains("153\nbindings over 113 commands") || MODE.contains("153 bindings over 113 commands"),
        "docs/omarchy-mode.md no longer quotes the 153 bindings over 113 commands this machine produces"
    );
    assert!(
        GUIDE.contains("files=42 bindings=153 commands=113 env=8 autostart=4")
            && GUIDE.contains("float_rules=45 monitors=1 skipped=175"),
        "the guide's sample log line no longer matches what this machine reports"
    );
}
