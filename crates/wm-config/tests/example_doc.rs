//! The examples in `docs/config.example.toml` have to be real.
//!
//! A config reference nobody can paste from is worse than none: the
//! reader trusts it, the parser does not, and the failure is a silent
//! warning in a log they are not reading. So the documented spellings
//! are exercised here, uncommented, exactly as written in the file.

use wm_config::{parse, Action};

/// Every `[commands]`, `terminal` and `autostart` example from the
/// reference, uncommented and parsed.
#[test]
fn the_documented_examples_parse_and_mean_what_they_say() {
    let config = parse(
        r#"
        show_dock = false
        terminal = ["ghostty", "--title", "my terminal"]

        autostart = [
          "wl-paste --watch cliphist store",
          "udiskie --automount --no-notify --no-tray",
        ]

        [commands]
        omarchy-menu = "omarchy-menu toggle"
        volume-up    = "omarchy-audio-output-volume up"
        volume-down  = "omarchy-audio-output-volume down"
        lock         = "omarchy-system-lock"
        notify       = ["notify-send", "hello world"]

        [keybindings]
        "super+space" = "run omarchy-menu"
        "volumeup"    = "run volume-up"
        "volumedown"  = "run volume-down"
        "super+l"     = "run lock"
        "super+d"     = "toggle-dock"
        "#,
    )
    .expect("the documented config must parse");

    // The array spelling keeps a spaced argument whole -- the whole
    // reason it is documented beside the string one.
    assert_eq!(
        config.terminal.as_deref(),
        Some(["ghostty", "--title", "my terminal"].map(String::from).as_slice())
    );
    assert_eq!(config.autostart.len(), 2);
    // The dockless configuration the reference documents, and the
    // binding it suggests beside it.
    assert!(!config.show_dock, "the documented `show_dock = false` must actually turn the Dock off");
    assert_eq!(
        config.keybindings.iter().filter(|(_, a)| *a == Action::ToggleDock).count(),
        1,
        "the documented `super+d` = `toggle-dock` example must survive parsing"
    );
    assert_eq!(config.commands.len(), 5);
    assert_eq!(
        config.commands.get("notify").map(Vec::as_slice),
        Some(["notify-send", "hello world"].map(String::from).as_slice())
    );

    // Every documented binding survived. A `run` binding that named a
    // missing command would have been dropped, so a non-empty result
    // here is also the proof that the names line up.
    let runs: Vec<&str> = config
        .keybindings
        .iter()
        .filter_map(|(_, a)| match a {
            Action::Run(name) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for expected in ["omarchy-menu", "volume-up", "volume-down", "lock"] {
        assert!(runs.contains(&expected), "documented binding for {expected} did not survive parsing");
    }
}

/// The media-key names the reference lists must all be real names.
/// This is the list a reader copies from; every entry has to resolve.
#[test]
fn every_documented_media_key_name_parses() {
    for name in [
        "volumeup",
        "volumedown",
        "volumemute",
        "mute",
        "micmute",
        "playpause",
        "audioplay",
        "audiopause",
        "audiostop",
        "audionext",
        "audioprev",
        "brightnessup",
        "brightnessdown",
        "kbdbrightnessup",
        "kbdbrightnessdown",
        "poweroff",
        "search",
    ] {
        assert!(
            wm_config::parse_key(name).is_some(),
            "docs/config.example.toml lists {name} as a key token, but the parser does not know it"
        );
        // And they take modifiers like any other key.
        assert!(wm_config::parse_key(&format!("super+{name}")).is_some(), "super+{name}");
    }
}
