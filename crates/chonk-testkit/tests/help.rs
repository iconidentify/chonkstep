//! Real menu entry, readable scaled pixels, modal input and lifecycle cleanup.
use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions, ShellInfo, World};
use std::time::Duration;

fn body_pixels(shot: &chonk_testkit::Screenshot, panel: &ShellInfo, scale: f32) -> Vec<[u8; 4]> {
    let top = panel.y as u32 + (158.0 * scale) as u32;
    let bottom = panel.y as u32 + panel.h - (48.0 * scale) as u32;
    (top..bottom)
        .step_by(4)
        .flat_map(|y| {
            (panel.x as u32..panel.x as u32 + panel.w)
                .step_by(4)
                .map(move |x| shot.pixel(x, y))
        })
        .collect()
}
fn config(scale: f32, theme: &str) -> String {
    format!("scale={scale}\ntheme='{theme}'\nhyprland_config=false\nomarchy_menu=false\ninteraction_mode='spaces'\nkeyboard_mode='desktop'\n[keybindings]\n'super+o'='overview'\n'super+f1'='help'\n'super+q'='close'\n")
}
fn help(world: &World) -> Option<&ShellInfo> {
    world
        .shells
        .iter()
        .find(|s| s.mapped && s.above && s.w > 600 && s.h > 400 && s.buffer_bytes > 0)
}
fn opened(session: &mut Session) -> ShellInfo {
    poll_until(
        Duration::from_secs(5),
        "Help is painted and visible",
        || help(&session.world().ok()?).cloned(),
    )
    .unwrap()
}
fn closed(session: &mut Session) {
    poll_until(Duration::from_secs(5), "Help released its surface", || {
        help(&session.world().ok()?).is_none().then_some(())
    })
    .unwrap();
}

#[test]
#[ignore = "real nested Wayland"]
fn help_opens_from_the_menu_scales_scrolls_searches_and_returns_keyboard_to_the_app() {
    for (scale, theme) in [(1.0, "obsidian"), (1.5, "omarchy"), (2.0, "obsidian")] {
        let mut session = Session::boot(
            &format!("help-{scale}"),
            SessionOptions {
                config_extra: config(scale, theme),
                ..Default::default()
            },
        )
        .unwrap();
        let binary = profile_binary("chonk-input-probe").unwrap();
        session
            .launch(binary.to_str().unwrap(), &[&scale.to_string()])
            .unwrap();
        let app = session.wait_for_window("input-probe").unwrap().id;
        let edge = session.world().unwrap().output_w as f64 - 5.0;
        session.door().right_click(edge, 5.0).unwrap();
        session.screenshot("root-menu").unwrap();
        // The native menu has Terminal, Applications, Theme, Wallpaper, Help,
        // Exit; navigate the actual entry rather than invoking Help directly.
        for _ in 0..4 {
            session.door().tap_key(108).unwrap();
        }
        session.screenshot("root-menu-help-selected").unwrap();
        session.door().tap_key(keys::ENTER).unwrap();
        session.screenshot("after-help-selection").unwrap();
        let panel = opened(&mut session);
        let world = session.world().unwrap();
        assert!(panel.x >= 0 && panel.y >= 0);
        assert!(
            panel.x as u32 + panel.w <= world.output_w
                && panel.y as u32 + panel.h <= world.output_h
        );
        let first = session.screenshot("everyday").unwrap();
        session.door().tap_key(109).unwrap(); // Page Down
        let scrolled = session.screenshot("everyday-scrolled").unwrap();
        assert_ne!(
            body_pixels(&first, &panel, scale),
            body_pixels(&scrolled, &panel, scale),
            "keyboard scrolling moves real guide content"
        );
        session.door().chord(keys::LEFTMETA, 16).unwrap(); // normally closes the app
        assert!(
            session.world().unwrap().windows.iter().any(|w| w.id == app),
            "Help owns desktop shortcuts while open"
        );
        // Text automatically searches All shortcuts; it must not reach the app.
        let before_keys = session
            .client_log("chonk-input-probe")
            .matches("keyboard key ")
            .count();
        for key in [24, 47, 18, 19] {
            session.door().tap_key(key).unwrap();
        } // over
        let filtered = session.screenshot("search-overview").unwrap();
        assert_ne!(
            body_pixels(&scrolled, &panel, scale),
            body_pixels(&filtered, &panel, scale)
        );
        assert_eq!(
            session
                .client_log("chonk-input-probe")
                .matches("keyboard key ")
                .count(),
            before_keys
        );
        session.door().tap_key(15).unwrap();
        session.screenshot("quick-reference").unwrap();
        session.door().tap_key(keys::ESC).unwrap();
        closed(&mut session);
        session.door().tap_key(30).unwrap();
        poll_until(
            Duration::from_secs(3),
            "keyboard restored after Help",
            || {
                session
                    .client_log("chonk-input-probe")
                    .contains("keyboard key 30 down")
                    .then_some(())
            },
        )
        .unwrap();
        // Reopening and the painted Close button use the same cleanup path.
        session.door().chord(keys::LEFTMETA, 59).unwrap();
        let panel = opened(&mut session);
        session
            .door()
            .click(
                panel.x as f64 + panel.w as f64 - 50.0 * f64::from(scale),
                panel.y as f64 + 37.0 * f64::from(scale),
            )
            .unwrap();
        closed(&mut session);
    }
}

#[test]
#[ignore = "real nested Wayland"]
fn help_releases_its_grab_on_reload_and_lock() {
    let mut session = Session::boot(
        "help-lifecycle",
        SessionOptions {
            config_extra: config(1.5, "obsidian"),
            ..Default::default()
        },
    )
    .unwrap();
    session.door().chord(keys::LEFTMETA, 59).unwrap();
    opened(&mut session);
    session
        .rewrite_config(&format!(
            "omarchy_shell=false\n{}",
            config(1.5, "obsidian").replace("'super+o'='overview'", "'super+y'='overview'")
        ))
        .unwrap();
    session.request_reload().unwrap();
    closed(&mut session);
    session.door().chord(keys::LEFTMETA, 59).unwrap();
    opened(&mut session);
    let binary = profile_binary("chonk-lock-probe").unwrap();
    session
        .launch(binary.to_str().unwrap(), &["--hold"])
        .unwrap();
    poll_until(
        Duration::from_secs(10),
        "lock presentation owns the screen",
        || {
            session
                .client_log("chonk-lock-probe")
                .contains("locked ")
                .then_some(())
        },
    )
    .unwrap();
    closed(&mut session);
}
