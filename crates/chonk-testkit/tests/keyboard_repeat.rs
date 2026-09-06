//! Real seat delivery and reload boundaries. Native Wayland clients implement
//! their own repeat timer; these assertions observe the parameters and physical
//! transitions, not invented server-side wl_keyboard repeats.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, session_dir, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(10);
const PROBE: &str = "chonk-input-probe";
const KEY_A: u32 = 30;
const KEY_R: u32 = 19;
const KEY_META: u32 = 125;

fn map_x11_window(connection: &x11rb::rust_connection::RustConnection, screen: usize) -> u32 {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass};
    use x11rb::wrapper::ConnectionExt as _;
    let root = connection.setup().roots[screen].root;
    let xid = connection.generate_id().unwrap();
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            xid,
            root,
            0,
            0,
            400,
            300,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(0x406080)
                .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )
        .unwrap();
    connection
        .change_property8(PropMode::REPLACE, xid, AtomEnum::WM_NAME, AtomEnum::STRING, b"keyboard-repeat-x11")
        .unwrap();
    connection.map_window(xid).unwrap();
    connection.flush().unwrap();
    xid
}

fn usable_config(input: &str) -> String {
    // Keep reload/timing tests independent of the separate input-only loading
    // regression below, so they also reach their intended assertion on an old
    // compositor that incorrectly discards configurations without bindings.
    format!("{input}\nbind = SUPER, F12, workspace, 1\n")
}

fn options(input: &str) -> SessionOptions {
    SessionOptions {
        config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
        config_root_files: vec![("hypr/hyprland.conf".into(), usable_config(input))],
        ..SessionOptions::default()
    }
}

fn probe(session: &mut Session) {
    let binary = profile_binary(PROBE).expect("input probe built");
    session.launch(binary.to_str().unwrap(), &["1"]).expect("probe launches");
    let window = session.wait_for_window("input-probe").expect("probe maps");
    session.door().click(f64::from(window.x + 80), f64::from(window.y + 70)).unwrap();
    wait_line(session, "keyboard enter");
}

fn wait_line(session: &Session, line: &str) {
    poll_until(EVENT, line, || session.client_log(PROBE).lines().any(|value| value == line).then_some(()))
        .unwrap_or_else(|error| panic!("{error}\n{}", session.client_log(PROBE)));
}

fn keymap_count(session: &Session) -> usize {
    session.client_log(PROBE).lines().filter(|line| line.starts_with("keyboard keymap ")).count()
}

fn reload_hyprland(session: &mut Session, name: &str, config: &str) {
    std::fs::write(session_dir(name).join("config/hypr/hyprland.conf"), usable_config(config)).unwrap();
    let before = session.log().matches("reload requested").count();
    session.request_reload().unwrap();
    poll_until(EVENT, "the requested session reload", || {
        (session.log().matches("reload requested").count() > before).then_some(())
    })
    .unwrap();
    session.door().barrier().unwrap();
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn zero_rate_and_zero_delay_reach_native_clients_and_disable_binding_repeat() {
    let config = "input {\n repeat_rate = 0\n repeat_delay = 0\n}\nbinde = SUPER, R, workspace, 1\n";
    let mut session = Session::boot("keyboard-repeat-disabled", options(config)).unwrap();
    probe(&mut session);
    wait_line(&session, "keyboard repeat 0 0");
    session.door().key(KEY_META, true).unwrap();
    session.door().key(KEY_R, true).unwrap();
    session.door().barrier().unwrap();
    assert!(session.door().repeating_binding().unwrap().is_none(), "zero rate must not arm a timer");
    session.door().key(KEY_R, false).unwrap();
    session.door().key(KEY_META, false).unwrap();
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn unchanged_reload_does_not_replace_a_held_keys_map() {
    let name = "keyboard-repeat-unchanged-reload";
    let config = "input {\n repeat_rate = 30\n repeat_delay = 150\n}\n";
    let mut session = Session::boot(name, options(config)).unwrap();
    probe(&mut session);
    wait_line(&session, "keyboard repeat 30 150");
    session.door().key(KEY_A, true).unwrap();
    wait_line(&session, "keyboard key 30 down");
    assert_eq!(keymap_count(&session), 1);
    reload_hyprland(&mut session, name, config);
    session.door().key(KEY_A, false).unwrap();
    // A delivered release is a wire-order barrier: a preceding reload cannot
    // hide its keymap event behind a test's instantaneous file read.
    wait_line(&session, "keyboard key 30 up");
    assert_eq!(keymap_count(&session), 1, "an unrelated reload must not reset client repeat state");
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn repeat_only_reload_updates_timing_without_replacing_the_keymap() {
    let name = "keyboard-repeat-timing-reload";
    let mut session = Session::boot(name, options("input {\n repeat_rate = 30\n repeat_delay = 150\n}\n")).unwrap();
    probe(&mut session);
    wait_line(&session, "keyboard repeat 30 150");
    session.door().key(KEY_A, true).unwrap();
    wait_line(&session, "keyboard key 30 down");
    reload_hyprland(&mut session, name, "input {\n repeat_rate = 45\n repeat_delay = 0\n}\n");
    wait_line(&session, "keyboard repeat 45 0");
    session.door().key(KEY_A, false).unwrap();
    wait_line(&session, "keyboard key 30 up");
    assert_eq!(keymap_count(&session), 1, "repeat timing is independent of keymap compilation");
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn releasing_a_binding_modifier_stops_the_held_shortcut() {
    let config = "input {\n repeat_rate = 25\n repeat_delay = 120\n}\nbinde = SUPER, R, workspace, 1\n";
    let mut session = Session::boot("keyboard-repeat-modifier-release", options(config)).unwrap();
    session.door().key(KEY_META, true).unwrap();
    session.door().key(KEY_R, true).unwrap();
    session.door().barrier().unwrap();
    assert!(session.door().repeating_binding().unwrap().is_some(), "binding owns repeat while held");
    session.door().key(KEY_META, false).unwrap();
    session.door().barrier().unwrap();
    assert!(session.door().repeating_binding().unwrap().is_none(), "released modifier ends the shortcut");
    session.door().key(KEY_R, false).unwrap();
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn disabling_repeat_on_reload_cancels_an_existing_binding_timer() {
    let name = "keyboard-repeat-disable-reload";
    let mut session =
        Session::boot(name, options("input {\n repeat_rate = 25\n}\nbinde = SUPER, R, workspace, 1\n")).unwrap();
    probe(&mut session);
    session.door().key(KEY_META, true).unwrap();
    session.door().key(KEY_R, true).unwrap();
    session.door().barrier().unwrap();
    assert!(session.door().repeating_binding().unwrap().is_some());
    reload_hyprland(&mut session, name, "input {\n repeat_rate = 0\n}\nbinde = SUPER, R, workspace, 1\n");
    wait_line(&session, "keyboard repeat 0 200");
    session.door().barrier().unwrap();
    assert!(session.door().repeating_binding().unwrap().is_none(), "disabled timing cancels a held repeat");
    session.door().key(KEY_R, false).unwrap();
    session.door().key(KEY_META, false).unwrap();
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn a_real_wayland_terminal_repeats_a_held_physical_key() {
    let mut session =
        Session::boot("keyboard-repeat-foot", options("input {\n repeat_rate = 40\n repeat_delay = 120\n}\n")).unwrap();
    let sink = profile_binary("chonk-key-sink").expect("terminal child built");
    let output = session.dir.join("terminal-bytes");
    session
        .launch(
            "foot",
            &[
                "--config=/dev/null",
                "--title=keyboard-repeat-foot",
                sink.to_str().unwrap(),
                output.to_str().unwrap(),
                "8",
            ],
        )
        .unwrap();
    let window = session.wait_for_window("keyboard-repeat-foot").unwrap();
    poll_until(EVENT, "terminal child to accept individual bytes", || output.is_file().then_some(())).unwrap();
    session.door().click(f64::from(window.x + 80), f64::from(window.y + 70)).unwrap();
    session.door().key(KEY_A, true).unwrap();
    let typed = poll_until(EVENT, "real terminal to repeat the held key eight times", || {
        std::fs::read(&output).ok().filter(|bytes| bytes.len() >= 8)
    });
    session.door().key(KEY_A, false).unwrap();
    assert_eq!(typed.unwrap(), b"aaaaaaaa", "one physical press must drive the toolkit repeat timer");
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn a_rejected_keymap_preserves_timing_and_a_later_valid_edit_recovers() {
    let name = "keyboard-repeat-keymap-recovery";
    let mut session =
        Session::boot(name, options("input {\n kb_layout = us\n repeat_rate = 30\n repeat_delay = 150\n}\n")).unwrap();
    probe(&mut session);
    wait_line(&session, "keyboard repeat 30 150");
    session.door().key(KEY_A, true).unwrap();
    wait_line(&session, "keyboard key 30 down");
    reload_hyprland(&mut session, name, "input {\n kb_layout = chonk-test-invalid-layout\n repeat_rate = 45\n}\n");
    poll_until(EVENT, "rejected layout diagnostic", || {
        session.log().contains("keyboard configuration was rejected").then_some(())
    })
    .unwrap();
    session.door().key(KEY_A, false).unwrap();
    wait_line(&session, "keyboard key 30 up");
    assert_eq!(keymap_count(&session), 1);
    assert!(
        !session.client_log(PROBE).contains("keyboard repeat 45 "),
        "failed edit must not partially replace timing"
    );
    reload_hyprland(&mut session, name, "input {\n kb_layout = de\n repeat_rate = 45\n}\n");
    wait_line(&session, "keyboard repeat 45 200");
    assert_eq!(keymap_count(&session), 2, "the next valid layout must compile and reach the client");
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn an_xwayland_client_repeats_a_held_physical_key() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt;
    use x11rb::protocol::Event;

    let mut session =
        Session::boot("keyboard-repeat-xwayland", options("input {\n repeat_rate = 40\n repeat_delay = 120\n}\n"))
            .unwrap();
    let (connection, screen) = session.connect_x11().unwrap();
    let xid = map_x11_window(&connection, screen);
    let window = poll_until(EVENT, "the single X11 window to map", || {
        session.world().ok()?.windows.into_iter().find(|window| window.mapped)
    })
    .unwrap();
    session.door().click(f64::from(window.x + 80), f64::from(window.y + 70)).unwrap();
    poll_until(EVENT, "the X server to focus its test window", || {
        (connection.get_input_focus().ok()?.reply().ok()?.focus == xid).then_some(())
    })
    .unwrap();
    session.door().key(KEY_A, true).unwrap();
    let mut presses = 0;
    let result = poll_until(EVENT, "XWayland to repeat eight key presses", || {
        while let Some(event) = connection.poll_for_event().expect("X11 event stream") {
            if let Event::KeyPress(key) = event {
                assert_eq!(u32::from(key.detail), KEY_A + 8, "X11 uses XKB's eight-code offset");
                presses += 1;
            }
        }
        (presses >= 8).then_some(())
    });
    session.door().key(KEY_A, false).unwrap();
    result.unwrap_or_else(|error| panic!("{error}: observed {presses} press events\n{}", session.log()));
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn input_only_hyprland_settings_reach_the_running_seat() {
    let mut options = options("");
    options.config_root_files[0].1 = "input {\n repeat_rate = 45\n repeat_delay = 150\n}\n".into();
    let mut session = Session::boot("keyboard-repeat-input-only", options).unwrap();
    probe(&mut session);
    wait_line(&session, "keyboard repeat 45 150");
}

#[test]
#[ignore = "needs a session to nest in and wtype; run via scripts/e2e.sh"]
fn physical_input_restores_the_real_keymap_after_virtual_typing_and_a_noop_reload() {
    if !chonk_testkit::require_client("wtype") {
        return;
    }
    let name = "keyboard-repeat-virtual-restore";
    let config = "input {\n kb_layout = us\n}\n";
    let mut session = Session::boot(name, options(config)).unwrap();
    probe(&mut session);
    assert_eq!(keymap_count(&session), 1);
    session.launch("wtype", &["x"]).unwrap();
    poll_until(EVENT, "virtual typing to finish", || {
        session.client_status("wtype").ok().flatten().map(|status| assert!(status.success()))
    })
    .unwrap();
    poll_until(EVENT, "virtual keymap to reach the client", || (keymap_count(&session) >= 2).then_some(())).unwrap();
    reload_hyprland(&mut session, name, config);
    session.door().key(KEY_A, true).unwrap();
    wait_line(&session, "keyboard key 30 down");
    session.door().key(KEY_A, false).unwrap();
    wait_line(&session, "keyboard key 30 up");
    assert_eq!(keymap_count(&session), 3, "initial, virtual, and one physical-seat restoration keymap");
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn older_keyboard_clients_receive_a_nul_terminated_keymap_too() {
    let mut session = Session::boot("keyboard-repeat-legacy-map", options("")).unwrap();
    let binary = profile_binary(PROBE).unwrap();
    session.launch(binary.to_str().unwrap(), &["1", "legacy-keyboard"]).unwrap();
    session.wait_for_window("input-probe").unwrap();
    wait_line(&session, "keyboard keymap-nul true");
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn unchanged_and_timing_only_reload_preserve_caps_lock_and_the_selected_layout() {
    const ALT: u32 = 56;
    const SHIFT: u32 = 42;
    const CAPS_LOCK: u32 = 58;
    let name = "keyboard-repeat-locked-state";
    let config =
        "input {\n kb_layout = us,de\n kb_options = grp:alt_shift_toggle\n repeat_rate = 30\n repeat_delay = 150\n}\n";
    let mut session = Session::boot(name, options(config)).unwrap();
    probe(&mut session);
    let modifiers = |session: &Session| -> Option<(u32, u32)> {
        let log = session.client_log(PROBE);
        let line = log.lines().rev().find_map(|line| line.strip_prefix("keyboard modifiers "))?;
        let fields: Vec<u32> = line.split_whitespace().map(str::parse).collect::<Result<_, _>>().ok()?;
        Some((*fields.get(2)?, *fields.get(3)?))
    };
    session.door().tap_key(CAPS_LOCK).unwrap();
    session.door().key(ALT, true).unwrap();
    session.door().tap_key(SHIFT).unwrap();
    session.door().key(ALT, false).unwrap();
    let initial = poll_until(EVENT, "Caps Lock and the second configured layout", || {
        modifiers(&session).filter(|(locked, group)| *locked != 0 && *group == 1)
    })
    .unwrap();

    for (index, next) in [config.to_string(), config.replace("repeat_rate = 30", "repeat_rate = 45")].iter().enumerate()
    {
        reload_hyprland(&mut session, name, next);
        // Force a modifiers event and follow it with a distinct key release as
        // the ordering barrier. Identical keymap hashes can suppress a reload's
        // wire notification even when an old compositor reset its internal XKB
        // state; a keymap-event-count-only test cannot catch that reset.
        session.door().tap_key(SHIFT).unwrap();
        let code = KEY_A + index as u32;
        session.door().tap_key(code).unwrap();
        wait_line(&session, &format!("keyboard key {code} up"));
        assert_eq!(modifiers(&session), Some(initial), "reload {index} must retain locked modifiers and layout");
    }
    assert_eq!(keymap_count(&session), 1);
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn successive_x11_windows_receive_focus_and_their_first_key_without_a_reclick() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt;
    use x11rb::protocol::Event;

    let mut session = Session::boot("keyboard-repeat-x11-map-order", options("")).unwrap();
    let (connection, screen) = session.connect_x11().unwrap();
    // Every cycle is a required pass, not a retry of a failed assertion.
    // Association and first-commit ordering varies across these new surfaces.
    for cycle in 0..30 {
        let xid = map_x11_window(&connection, screen);
        poll_until(EVENT, "new X11 window's initial focus", || {
            (connection.get_input_focus().ok()?.reply().ok()?.focus == xid).then_some(())
        })
        .unwrap_or_else(|error| panic!("cycle {cycle}, xid {xid}: {error}\n{}", session.log()));
        session.door().key(KEY_A, true).unwrap();
        poll_until(EVENT, "the new X11 client's first key", || {
            while let Some(event) = connection.poll_for_event().unwrap() {
                if let Event::KeyPress(key) = event {
                    assert_eq!(key.event, xid);
                    assert_eq!(u32::from(key.detail), KEY_A + 8);
                    return Some(());
                }
            }
            None
        })
        .unwrap();
        session.door().key(KEY_A, false).unwrap();
        poll_until(EVENT, "matching X11 key release", || {
            while let Some(event) = connection.poll_for_event().unwrap() {
                if matches!(event, Event::KeyRelease(key) if key.event == xid && u32::from(key.detail) == KEY_A + 8) {
                    return Some(());
                }
            }
            None
        })
        .unwrap();
        connection.destroy_window(xid).unwrap();
        connection.flush().unwrap();
        poll_until(EVENT, "the X11 window to retire before the next map", || {
            session.world().ok()?.windows.is_empty().then_some(())
        })
        .unwrap_or_else(|error| panic!("cycle {cycle}, xid {xid}: {error}\n{:?}\n{}", session.world(), session.log()));
    }
}
