//! A compositor-owned modal UI must withdraw client keyboard focus. Filtering
//! new presses alone leaves a toolkit repeating an earlier held key behind it.

use std::time::Duration;

use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(10);
const PROBE: &str = "chonk-input-probe";
const KEY_A: u32 = 30;
const KEY_DOWN: u32 = 108;
const KEY_TAB: u32 = 15;

fn boot(name: &str) -> Session {
    boot_with(name, "", &[])
}

fn boot_with(name: &str, hyprland: &str, probe_modes: &[&str]) -> Session {
    let mut session = Session::boot(
        name,
        SessionOptions {
            config_extra: format!(
                "omarchy_menu = false\nhyprland_config = {}\nshow_dock = false\n\
                [keybindings]\n\"super+2\" = \"workspace 2\"\n\"super+1\" = \"workspace 1\"\n\"super+up\" = \"overview\"\n",
                !hyprland.is_empty()
            ),
            config_root_files: if hyprland.is_empty() {
                vec![]
            } else {
                vec![(
                    "hypr/hyprland.conf".into(),
                    format!("{hyprland}\nbind = SUPER, F12, workspace, 1\n"),
                )]
            },
            ..Default::default()
        },
    )
    .unwrap();
    let probe = profile_binary(PROBE).unwrap();
    let mut args = vec!["1"];
    args.extend_from_slice(probe_modes);
    session.launch(probe.to_str().unwrap(), &args).unwrap();
    let window = session.wait_for_window("input-probe").unwrap();
    session
        .door()
        .click(f64::from(window.x + 80), f64::from(window.y + 70))
        .unwrap();
    wait_events(&session, "keyboard enter", 1);
    session
}

/// Boots a session whose Hyprland configuration gives probe windows a
/// body alpha or a dim, and opens two red probe windows in it. Red on
/// purpose: a focus cue that lives in the pixels has to be read from
/// the pixels, and a solid red body makes "opaque" one exact colour.
fn boot_two_red_windows(name: &str, hyprland: &str) -> (Session, chonk_testkit::WindowInfo, chonk_testkit::WindowInfo) {
    let mut session = Session::boot(
        name,
        SessionOptions {
            config_extra: "omarchy_menu = false\nhyprland_config = true\nshow_dock = false\n".into(),
            config_root_files: vec![("hypr/hyprland.conf".into(), hyprland.to_string())],
            ..Default::default()
        },
    )
    .unwrap();
    // Use the harness's committed solid buffer: foot's color section
    // changed in 1.28, and a rejected override maps an error terminal
    // instead of the color these assertions need to measure.
    let color = session.dir.join("opacity-rgb");
    std::fs::write(&color, [255, 0, 0]).unwrap();
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    let mut windows = Vec::new();
    for title in ["Opacity One", "Opacity Two"] {
        session
            .launch_isolated("env", &[&format!("CHONKSTEP_PROBE_COLOR_FILE={}", color.display()),
                probe.to_str().unwrap(), title, "opacity-probe"])
            .unwrap();
        windows.push(session.wait_for_window(title).unwrap());
    }
    let two = windows.pop().unwrap();
    let one = windows.pop().unwrap();
    (session, one, two)
}

/// The mean colour of the middle of a window's body, well away from
/// the surrounding frame.
fn body_rgb(shot: &chonk_testkit::Screenshot, window: &chonk_testkit::WindowInfo) -> [f64; 3] {
    let x = window.x as u32 + window.w / 2 - 8;
    let y = window.y as u32 + window.h / 2 - 8;
    shot.mean_rgb(x, y, 16, 16)
}

fn is_pure_red([r, g, b]: [f64; 3]) -> bool {
    r > 245.0 && g < 10.0 && b < 10.0
}

fn focus_by_click(session: &mut Session, window: &chonk_testkit::WindowInfo) {
    session.door().click(f64::from(window.x) + f64::from(window.w) / 2.0, f64::from(window.y) + f64::from(window.h) / 2.0).unwrap();
    session.door().barrier().unwrap();
    let id = window.id;
    poll_until(EVENT, "the clicked window to take focus", || {
        (session.world().ok()?.logical_focus == Some(id)).then_some(())
    })
    .unwrap();
}

/// An `opacity` rule is a focus cue: the focused window's body is
/// composited at the active alpha and every other window's at the
/// inactive one, and a focus change repaints both on the next frame
/// without anything else damaging the scene.
#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --test keyboard_focus"]
fn a_focus_change_updates_the_body_alpha_on_the_next_frame() {
    let (mut session, one, two) =
        boot_two_red_windows("keyboard-focus-opacity", "windowrule = opacity 1.0 0.5, match:class ^opacity-probe$\n");
    // The second window mapped last and holds focus: opaque red. The
    // first is unfocused at half alpha: red mixed with whatever lies
    // beneath it, which is not red.
    focus_by_click(&mut session, &two);
    let shot = session.screenshot("two-focused").unwrap();
    assert!(is_pure_red(body_rgb(&shot, &two)), "focused body is opaque: {:?} {}", body_rgb(&shot, &two), shot.path.display());
    assert!(!is_pure_red(body_rgb(&shot, &one)), "unfocused body is translucent: {:?} {}", body_rgb(&shot, &one), shot.path.display());

    focus_by_click(&mut session, &one);
    let shot = session.screenshot("one-focused").unwrap();
    assert!(is_pure_red(body_rgb(&shot, &one)), "the newly focused body is opaque: {:?} {}", body_rgb(&shot, &one), shot.path.display());
    assert!(!is_pure_red(body_rgb(&shot, &two)), "the window that lost focus is translucent: {:?} {}", body_rgb(&shot, &two), shot.path.display());
    assert!(session.compositor_alive());
}

/// `dim_inactive` darkens every unfocused window and only those: the
/// focused body stays exactly its own colour, the unfocused one is the
/// same red under a half-strength black quad — still red, with nothing
/// from beneath the window mixed in, because the window stays opaque.
#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --test keyboard_focus"]
fn dim_inactive_darkens_only_the_unfocused_windows() {
    let (mut session, one, two) = boot_two_red_windows(
        "keyboard-focus-dim",
        "decoration {\n    dim_inactive = true\n    dim_strength = 0.5\n}\n",
    );
    let dimmed = |[r, g, b]: [f64; 3]| (100.0..=160.0).contains(&r) && g < 10.0 && b < 10.0;
    focus_by_click(&mut session, &two);
    let shot = session.screenshot("dim-two-focused").unwrap();
    assert!(is_pure_red(body_rgb(&shot, &two)), "the focused body is untouched: {:?} {}", body_rgb(&shot, &two), shot.path.display());
    assert!(dimmed(body_rgb(&shot, &one)), "the unfocused body is dimmed red: {:?} {}", body_rgb(&shot, &one), shot.path.display());

    focus_by_click(&mut session, &one);
    let shot = session.screenshot("dim-one-focused").unwrap();
    assert!(is_pure_red(body_rgb(&shot, &one)), "{:?} {}", body_rgb(&shot, &one), shot.path.display());
    assert!(dimmed(body_rgb(&shot, &two)), "{:?} {}", body_rgb(&shot, &two), shot.path.display());
    assert!(session.compositor_alive());
}

fn event_count(session: &Session, event: &str) -> usize {
    session
        .client_log(PROBE)
        .lines()
        .filter(|line| *line == event)
        .count()
}

fn wait_events(session: &Session, event: &str, count: usize) {
    poll_until(EVENT, &format!("{count} occurrences of {event}"), || {
        (event_count(session, event) >= count).then_some(())
    })
    .unwrap_or_else(|error| panic!("{error}\n{}\n{}", session.client_log(PROBE), session.log()));
}

fn open_overview(session: &mut Session) {
    session.door().chord(keys::LEFTMETA, keys::UP).unwrap();
    poll_until(EVENT, "the compositor's modal Overview", || {
        let world = session.world().ok()?;
        world
            .shells
            .iter()
            .any(|shell| {
                shell.mapped
                    && shell.above
                    && shell.w == world.output_w
                    && shell.h == world.output_h
            })
            .then_some(())
    })
    .unwrap();
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless"]
fn retiring_an_old_input_method_grab_preserves_its_replacement() {
    let mut session = boot_with("keyboard-ime-replacement", "", &["ime-replace-grab"]);
    wait_events(&session, "ime replacement ready", 1);
    session.door().tap_key(KEY_A).unwrap();
    session.door().barrier().unwrap();
    poll_until(EVENT, "replacement IME receives both key edges", || {
        let log = session.client_log(PROBE);
        (log.contains("ime grab 1 key 30 Value(Pressed)")
            && log.contains("ime grab 1 key 30 Value(Released)"))
            .then_some(())
    }).unwrap_or_else(|error| panic!("{error}\n{}", session.client_log(PROBE)));
    assert_eq!(event_count(&session, "keyboard key 30 down"), 0,
        "retiring an old grab must not route new keys directly to a client");
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --test keyboard_focus"]
fn a_translated_release_cannot_cross_focus_through_an_input_method() {
    let mut session = boot_with("keyboard-ime-release-focus", "", &["ime-replace-grab"]);
    wait_events(&session, "ime replacement ready", 1);
    let reloads = session.log().matches("reload requested").count();
    session.rewrite_config("interaction_mode='mac'\nhyprland_config=false\nshow_dock=false\nomarchy_shell=false\n").unwrap();
    session.request_reload().unwrap();
    poll_until(EVENT, "Mac mode applied", || {
        (session.log().matches("reload requested").count() > reloads).then_some(())
    }).unwrap();
    session.door().key(keys::LEFTMETA, true).unwrap();
    session.door().key(105, true).unwrap();
    wait_events(&session, "ime grab 1 key 102 Value(Pressed)", 1);
    session.launch_isolated("foot", &["--title=IME Other Focus", "sleep", "120"]).unwrap();
    let other = session.wait_for_window("IME Other Focus").unwrap();
    session.door().click(f64::from(other.x + 30), f64::from(other.y + 50)).unwrap();
    wait_events(&session, "keyboard leave", 1);
    session.door().key(105, false).unwrap();
    session.door().key(keys::LEFTMETA, false).unwrap();
    session.door().tap_key(KEY_A).unwrap();
    wait_events(&session, "ime grab 1 key 30 Value(Released)", 1);
    assert_eq!(event_count(&session, "ime grab 1 key 102 Value(Released)"), 0,
        "an IME must not reinject the previous focus's translated release into the new client");
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn overview_withdraws_a_held_keys_client_focus_and_restores_it_on_cancel() {
    let mut session = boot("keyboard-focus-overview");
    let enters = event_count(&session, "keyboard enter");
    let leaves = event_count(&session, "keyboard leave");
    session.door().key(KEY_A, true).unwrap();
    wait_events(&session, "keyboard key 30 down", 1);
    open_overview(&mut session);
    wait_events(&session, "keyboard leave", leaves + 1);
    session.door().key(KEY_A, false).unwrap();
    session.door().tap_key(KEY_DOWN).unwrap();
    session.door().tap_key(keys::ESC).unwrap();
    wait_events(&session, "keyboard enter", enters + 1);
    session.door().tap_key(KEY_A).unwrap();
    wait_events(&session, "keyboard key 30 up", 1);
    assert_eq!(event_count(&session, "keyboard key 30 down"), 2);
    assert_eq!(
        event_count(&session, "keyboard key 30 up"),
        1,
        "the release during modal focus must not reach the old client"
    );
    assert_eq!(
        event_count(&session, "keyboard key 108 down"),
        0,
        "Overview navigation belongs only to the modal UI"
    );
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn alt_tab_withdraws_client_focus_and_restores_it_even_when_cancel_keeps_the_same_window() {
    let mut session = boot("keyboard-focus-alt-tab");
    let enters = event_count(&session, "keyboard enter");
    let leaves = event_count(&session, "keyboard leave");
    session.door().key(keys::LEFTALT, true).unwrap();
    session.door().tap_key(KEY_TAB).unwrap();
    session.door().barrier().unwrap();
    wait_events(&session, "keyboard leave", leaves + 1);
    session.door().tap_key(keys::ESC).unwrap();
    session.door().key(keys::LEFTALT, false).unwrap();
    wait_events(&session, "keyboard enter", enters + 1);
    session.door().tap_key(KEY_A).unwrap();
    wait_events(&session, "keyboard key 30 up", 1);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn held_overview_navigation_repeats_and_stops_when_the_modal_closes() {
    let mut session = boot("keyboard-focus-modal-repeat");
    let enters = event_count(&session, "keyboard enter");
    open_overview(&mut session);
    session.door().key(KEY_DOWN, true).unwrap();
    poll_until(EVENT, "held Overview Down to emit modal repeats", || {
        session
            .door()
            .repeating_binding()
            .ok()?
            .filter(|(emitted, _)| *emitted >= 3)
    })
    .unwrap();
    session.door().tap_key(keys::ESC).unwrap();
    session.door().barrier().unwrap();
    assert!(
        session.door().repeating_binding().unwrap().is_none(),
        "a modal repeat must end with its owner, even while the key is held"
    );
    session.door().key(KEY_DOWN, false).unwrap();
    // The return animation retains modal input until client focus is restored.
    wait_events(&session, "keyboard enter", enters + 1);
    session.door().tap_key(KEY_A).unwrap();
    wait_events(&session, "keyboard key 30 up", 1);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn hiding_the_last_window_withdraws_text_input_focus_and_restores_both_together() {
    let mut session = boot_with("keyboard-focus-text-input", "", &["ime"]);
    wait_events(&session, "text-input enter", 1);
    session.door().chord(keys::LEFTMETA, keys::TWO).unwrap();
    wait_events(&session, "keyboard leave", 1);
    wait_events(&session, "text-input leave", 1);
    session.door().chord(keys::LEFTMETA, keys::ONE).unwrap();
    wait_events(&session, "keyboard enter", 2);
    wait_events(&session, "text-input enter", 2);
    session.door().tap_key(KEY_A).unwrap();
    wait_events(&session, "keyboard key 30 up", 1);
    assert_eq!(event_count(&session, "text-input leave"), 1);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn the_first_held_alt_tab_repeats_without_a_second_physical_press() {
    let mut session = boot("keyboard-focus-first-alt-tab");
    session.door().key(keys::LEFTALT, true).unwrap();
    session.door().key(KEY_TAB, true).unwrap();
    poll_until(EVENT, "the opening Alt+Tab hold to repeat", || {
        session
            .door()
            .repeating_binding()
            .ok()?
            .filter(|(emitted, _)| *emitted >= 3)
    })
    .unwrap();
    session.door().key(keys::LEFTALT, false).unwrap();
    session.door().barrier().unwrap();
    assert!(session.door().repeating_binding().unwrap().is_none());
    session.door().key(KEY_TAB, false).unwrap();
    session.door().tap_key(KEY_A).unwrap();
    wait_events(&session, "keyboard key 30 up", 1);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn zero_repeat_rate_applies_to_modal_navigation_too() {
    let mut session = boot_with(
        "keyboard-focus-modal-disabled",
        "input {\n repeat_rate = 0\n repeat_delay = 0\n}\n",
        &[],
    );
    wait_events(&session, "keyboard repeat 0 0", 1);
    let enters = event_count(&session, "keyboard enter");
    open_overview(&mut session);
    session.door().key(KEY_DOWN, true).unwrap();
    session.door().barrier().unwrap();
    assert!(session.door().repeating_binding().unwrap().is_none());
    session.door().key(KEY_DOWN, false).unwrap();
    session.door().tap_key(keys::ESC).unwrap();
    wait_events(&session, "keyboard enter", enters + 1);
    session.door().tap_key(KEY_A).unwrap();
    wait_events(&session, "keyboard key 30 up", 1);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn a_self_minimized_clients_shortcut_inhibitor_is_revoked_with_keyboard_focus() {
    let mut session = boot_with("keyboard-focus-inhibitor", "", &["inhibit"]);
    wait_events(&session, "shortcut-inhibitor active", 1);
    // The inhibited client receives F5 and requests its own minimization;
    // using a compositor shortcut here would deliberately be inhibited.
    session.door().tap_key(63).unwrap();
    wait_events(&session, "keyboard leave", 1);
    wait_events(&session, "shortcut-inhibitor inactive", 1);
}
