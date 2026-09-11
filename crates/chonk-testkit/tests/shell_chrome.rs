//! Real desktop use in both styles: menus, switcher, cached Overview, minimized
//! icons, and reload while a menu owns input. All sessions are private/nested.
use chonk_testkit::{keys, poll_until, Session, SessionOptions, ShellInfo, Screenshot, World};
use std::time::Duration;
use wm_theme::{DecorationStyle, FontState, UiChrome, menu::MenuItem};

const WAIT: Duration = Duration::from_secs(10);
fn config(scale: u32, style: &str) -> String {
    format!("scale = {scale}\ntheme = 'nextstep-classic'\ndecoration_style = '{style}'\ninteraction_mode = 'desktop'\nshow_dock = false\nomarchy_menu = false\nhyprland_config = false\n[keybindings]\n'super+o' = 'overview'\n")
}
fn above(world: &World) -> Vec<ShellInfo> {
    world.shells.iter().filter(|s| s.mapped && s.above && s.buffer_bytes > 0).cloned().collect()
}
fn only_popup(session: &mut Session) -> ShellInfo {
    poll_until(WAIT, "one mapped popup", || {
        let popups = above(&session.world().ok()?);
        (popups.len() == 1).then(|| popups[0].clone())
    }).unwrap_or_else(|error| {
        let _ = session.screenshot("missing-popup");
        panic!("{error}; world={:?}", session.world());
    })
}
fn right_click(session: &mut Session, x: f64, y: f64) {
    session.door().motion(x, y).unwrap();
    session.door().button("right", true).unwrap();
    session.door().button("right", false).unwrap();
    session.door().barrier().unwrap();
}
fn title(session: &mut Session, id: u64) -> (f64, f64) {
    let world = session.world().unwrap();
    let window = world.windows.iter().find(|w| w.id == id).unwrap();
    let frame = world.frame_of(id).unwrap();
    ((frame.x + frame.w as i32 / 2) as f64, (frame.y + (window.y - frame.y) / 2) as f64)
}
fn popup_pixels(session: &mut Session, popup: &ShellInfo, style: &str, scale: u32, name: &str,
    background: Option<&Screenshot>) {
    session.door().motion(0.0, 0.0).unwrap();
    let shot = session.screenshot(name).unwrap();
    let inside = shot.pixel(popup.x as u32 + 2 * scale, popup.y as u32 + 2 * scale);
    if style == "system7" {
        assert_eq!(inside, [255; 4], "flat paper replaces WindowMaker relief");
        if let Some(background) = background {
            for (x, y) in [(popup.x as u32 + popup.w - 1, popup.y as u32),
                (popup.x as u32, popup.y as u32 + popup.h - 1)] {
                assert_eq!(shot.pixel(x, y), background.pixel(x, y), "shadow corners must remain transparent");
            }
        }
    } else {
        assert_ne!(inside, [255; 4], "legacy relief must return after toggling back");
    }
}

#[test]
#[ignore = "real nested desktop: scripts/e2e.sh --headless --test shell_chrome"]
fn menus_switcher_overview_icons_and_live_reload_follow_the_style_end_to_end() {
    assert!(chonk_testkit::require_client("foot"));
    let fonts = FontState::new();
    for scale in [1, 2] {
        let mut session = Session::boot(&format!("shell-chrome-{scale}"), SessionOptions {
            config_extra: config(scale, "windowmaker"), ..Default::default()
        }).unwrap();
        let mut ids = Vec::new();
        for (name, x, y) in [("Terminal", 70.0, 100.0), ("Notes", 470.0, 200.0)] {
            session.launch_isolated("foot", &["--title", name, "--app-id", name, "--window-size-pixels", "340x210",
                "--override", "locked-title=yes", "sh", "-c",
                r#"printf '\033[1;36mChonkStep desktop\033[0m\n\nWindow styles\n  WindowMaker  /  System 7\n\nMenus, Spaces and live windows\n\n'; exec sleep 3600"#]).unwrap();
            let id = session.wait_for_window(name).unwrap().id;
            let start = title(&mut session, id);
            let frame = session.world().unwrap().frame_of(id).unwrap().clone();
            session.door().drag_to(start, (start.0 + x - frame.x as f64, start.1 + y - frame.y as f64)).unwrap();
            session.door().button("left", false).unwrap();
            ids.push(id);
        }
        for (cycle, style) in ["windowmaker", "system7", "windowmaker", "system7"].into_iter().enumerate() {
            session.rewrite_config(&format!("omarchy_shell = false\n{}", config(scale, style))).unwrap();
            session.request_reload().unwrap();
            poll_until(WAIT, "style selected and previous menu/grabs released", || {
                let world = session.world().ok()?;
                (world.theme.decoration_style == style && above(&world).is_empty()).then_some(())
            }).unwrap();
            let name = |surface| format!("{cycle}-{style}-{surface}");
            session.door().motion(0.0, 0.0).unwrap();
            let background = session.screenshot(&name("desktop")).unwrap();
            right_click(&mut session, 900.0, 40.0);
            let root = only_popup(&mut session);
            popup_pixels(&mut session, &root, style, scale, &name("root-menu"), Some(&background));
            session.door().tap_key(keys::ESC).unwrap();
            assert!(above(&session.world().unwrap()).is_empty());

            // A real window menu activates the painted Miniaturize row.
            let at = title(&mut session, ids[1]);
            right_click(&mut session, at.0, at.1);
            let popup = only_popup(&mut session);
            popup_pixels(&mut session, &popup, style, scale, &name("window-menu"), None);
            let theme = wm_theme::default_theme::nextstep_classic().scaled(scale as f32);
            let chrome = UiChrome::new(&theme, fonts.clone(), DecorationStyle::from_name(style).unwrap(), scale as f32);
            let items: Vec<_> = ["Maximize", "Miniaturize", "Shade", "Fullscreen", "Move To", "Close", "Kill"]
                .into_iter().map(|label| if label == "Move To" { MenuItem::Submenu { label: label.into(), items: Vec::new() } }
                    else { MenuItem::Action { label: label.into(), action: 0 } }).collect();
            let expected = chrome.menu(&theme, &mut fonts.system(), "Notes", &items, Some(0), false);
            assert_eq!((popup.w, popup.h), (expected.buffer.width, expected.buffer.height));
            let row = expected.item_rects[1];
            session.door().click((popup.x + row.pos.x + row.size.w as i32 / 2) as f64,
                (popup.y + row.pos.y + row.size.h as i32 / 2) as f64).unwrap();
            let icon = poll_until(WAIT, "window minimizes to its desktop icon", || {
                let world = session.world().ok()?;
                if world.frame_of(ids[1])?.mapped { return None; }
                world.shells.iter().find(|s| s.mapped && !s.above && s.w == 56 * scale && s.h == s.w).cloned()
            }).unwrap();
            session.door().motion(0.0, 0.0).unwrap();
            let shot = session.screenshot(&name("minimized-icon")).unwrap();
            let expected_icon = chrome.icon(&theme, &mut fonts.system(), &mut fonts.swash(), icon.w, "Notes", None);
            for y in icon.h.saturating_sub(10 * scale)..icon.h { for x in 0..icon.w {
                let pixel = &expected_icon.pixels[((y * icon.w + x) * 4) as usize..][..4];
                let (gx, gy) = (icon.x as u32 + x, icon.y as u32 + y);
                if pixel[3] == 0 { assert_eq!(shot.pixel(gx, gy), background.pixel(gx, gy)); }
                else { assert_eq!(shot.pixel(gx, gy).as_slice(), pixel, "minimized caption/chrome ({x},{y})"); }
            } }
            session.door().click((icon.x + icon.w as i32 / 2) as f64, (icon.y + icon.h as i32 / 2) as f64).unwrap();
            poll_until(WAIT, "icon restores the same client", || {
                session.world().ok()?.frames.iter().find(|f| f.window == ids[1] && f.mapped).map(|_| ())
            }).unwrap();

            session.door().key(keys::LEFTALT, true).unwrap();
            session.door().tap_key(15).unwrap(); // Tab while holding the switch modifier.
            let switcher = only_popup(&mut session);
            popup_pixels(&mut session, &switcher, style, scale, &name("switcher"), None);
            session.door().key(keys::LEFTALT, false).unwrap();
            session.door().barrier().unwrap();
            assert!(above(&session.world().unwrap()).is_empty());

            session.door().chord(keys::LEFTMETA, 24).unwrap(); // configured Super+O
            let world = poll_until(WAIT, "native Overview with two live windows", || {
                let world = session.world().ok()?;
                (world.overview.is_some() && world.overview_windows.len() == 2).then_some(world)
            }).unwrap();
            let native = world.overview.unwrap();
            assert_eq!(native.preview_edge, 0, "style cannot enable capture/readback previews");
            assert!(world.shells.iter().filter(|s| s.mapped && s.above).all(|s| s.buffer_bytes == 0),
                "native Overview shell remains input-only");
            for card in &world.overview_windows {
                session.door().motion((card.rect.pos.x + card.rect.size.w as i32 / 2) as f64,
                    (card.rect.pos.y + card.rect.size.h as i32 / 2) as f64).unwrap();
                session.door().barrier().unwrap();
                assert_eq!(session.world().unwrap().overview.unwrap().label_bytes, native.label_bytes);
            }
            session.door().motion(0.0, 0.0).unwrap();
            session.screenshot(&name("overview")).unwrap();
            session.door().tap_key(keys::ENTER).unwrap();
            assert!(session.world().unwrap().overview.is_none());
            // The next loop switches styles with a root menu and its grabs live.
            right_click(&mut session, 900.0, 40.0);
            only_popup(&mut session);
        }
        session.door().tap_key(keys::ESC).unwrap();
        assert!(session.compositor_alive());
    }
}
