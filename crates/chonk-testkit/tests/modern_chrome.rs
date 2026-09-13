//! Real compositor coverage for modern decorations and window navigation.
//! Run with `scripts/e2e.sh --headless --release --test modern_chrome`.

use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions, World};
use std::{time::Duration};
use wm_theme::{default_theme::theme_variant, Appearance, Theme};

const WAIT: Duration = Duration::from_secs(15);
const THEMES: [&str; 3] = ["obsidian", "washi", "relay"];
const KEY_F: u32 = 33;

#[test]
#[ignore = "real Overview defaults: scripts/e2e.sh --headless --test modern_chrome"]
fn classic_overview_survives_modern_themes_and_switches_style_without_losing_clients() {
    let config = |theme: &str, style: Option<&str>| format!(
        "theme={theme:?}\nscale=1.5\ninteraction_mode='spaces'\nkeyboard_mode='desktop'\n\
         hyprland_config=false\n{}\n[keybindings]\n'super+o'='overview'\n\
         'super+1'='workspace 1'\n'super+2'='workspace 2'\n",
        style.map_or_else(String::new, |style| format!("overview_style={style:?}"))
    );
    let mut s = Session::boot("classic-overview-modern-theme", SessionOptions {
        config_extra: config("washi", None), ..Default::default()
    }).unwrap();
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    let mut ids = Vec::new();
    for title in ["Editor: workspace overview", "Browser: project notes"] {
        s.launch(probe.to_str().unwrap(), &[title, title]).unwrap();
        ids.push(s.wait_for_window(title).unwrap().id);
    }
    s.door().chord(keys::LEFTMETA, 3).unwrap(); // Desktop 2.
    poll_until(WAIT, "second desktop", || (s.world().ok()?.current_workspace == 1).then_some(())).unwrap();
    s.launch(probe.to_str().unwrap(), &["Files: Desktop 2", "classic-files"]).unwrap();
    ids.push(s.wait_for_window("Files: Desktop 2").unwrap().id);
    s.door().chord(keys::LEFTMETA, 2).unwrap(); // Desktop 1.
    poll_until(WAIT, "first desktop", || (s.world().ok()?.current_workspace == 0).then_some(())).unwrap();

    let open = |s: &mut Session| {
        s.door().chord(keys::LEFTMETA, 24).unwrap(); // Ordinary Super+O, no gesture or Mac translation.
        poll_until(WAIT, "Overview fully open", || {
            let world = s.world().ok()?;
            (world.overview.as_ref()?.progress == 1.0).then_some(world)
        }).unwrap()
    };
    let assert_classic = |world: &World| {
        assert_eq!(world.theme.decoration_style, "modern");
        assert_eq!(world.overview_spaces.len(), 2, "classic shows the actual desktop strip");
        let bottom = world.overview_spaces.iter().map(|space| {
            assert!(space.rect.size.h < world.output_h / 3, "desktop thumbnails stay at the top");
            space.rect.pos.y + space.rect.size.h as i32
        }).max().unwrap();
        assert!(world.overview_windows.iter().all(|window| window.rect.pos.y >= bottom),
            "large window previews sit below the desktop strip");
        assert_eq!(world.spatial.mode, "Freeform");
    };
    let world = open(&mut s);
    assert_classic(&world);
    s.screenshot("washi-classic-overview-1_5x").unwrap();
    let center = |rect: wm_theme_api::Rect| (
        f64::from(rect.pos.x) + f64::from(rect.size.w) / 2.0,
        f64::from(rect.pos.y) + f64::from(rect.size.h) / 2.0,
    );
    let source = world.overview_windows.iter().find(|window| window.id == ids[0]).unwrap().rect;
    let target = world.overview_spaces[1].rect;
    s.door().drag_to(center(source), center(target)).unwrap();
    s.door().button("left", false).unwrap();
    poll_until(WAIT, "drag moves window to Desktop 2 while retaining Overview", || {
        let world = s.world().ok()?;
        (world.overview.is_some() && world.overview_space_windows.contains(&(1, ids[0]))).then_some(())
    }).unwrap();
    let (x, y) = center(target);
    s.door().click(x, y).unwrap();
    let world = poll_until(WAIT, "click desktop switches without dismissing Classic", || {
        let world = s.world().ok()?;
        (world.current_workspace == 1 && world.overview.is_some()
            && world.overview_windows.iter().any(|w| w.id == ids[0])).then_some(world)
    }).unwrap();
    let (x, y) = center(world.overview_windows.iter().find(|w| w.id == ids[0]).unwrap().rect);
    s.door().click(x, y).unwrap();
    poll_until(WAIT, "window click activates and dismisses Overview", || {
        let world = s.world().ok()?;
        (world.overview.is_none() && world.logical_focus == Some(ids[0])).then_some(())
    }).unwrap();

    // Reloading only the layout must release the open view's input ownership.
    open(&mut s);
    s.rewrite_config(&config("washi", Some("cards"))).unwrap();
    s.request_reload().unwrap();
    poll_until(WAIT, "style reload dismisses the old scene", || s.world().ok()?.overview.is_none().then_some(())).unwrap();
    assert!(open(&mut s).overview_spaces.len() > 2, "numbered future cards remain available by opt-in");
    // Omitting the setting restores Classic; subsequent palette changes keep it.
    for theme in ["washi", "obsidian", "relay"] {
        s.rewrite_config(&config(theme, None)).unwrap();
        s.request_reload().unwrap();
        poll_until(WAIT, "theme reload completes", || {
            let world = s.world().ok()?;
            (world.theme.id == theme && world.overview.is_none()).then_some(())
        }).unwrap();
        let world = open(&mut s);
        assert_classic(&world);
        assert!(ids.iter().all(|id| world.windows.iter().any(|w| w.id == *id)));
        s.door().tap_key(1).unwrap();
        poll_until(WAIT, "Escape releases Overview", || s.world().ok()?.overview.is_none().then_some(())).unwrap();
    }
    assert!(s.compositor_alive());
}

#[test]
#[ignore = "real preview navigation: scripts/e2e.sh --headless --test modern_chrome"]
fn readable_switcher_and_minimized_previews_fit_and_restore_at_high_dpi() {
    let mut s = Session::boot("readable-previews", SessionOptions {
        config_extra: "theme='washi'\nscale=1.5\ninteraction_mode='spaces'\nkeyboard_mode='desktop'\nhyprland_config=false\n[keybindings]\n'super+m'='miniaturize'\n'super+d'='show-desktop'\n".into(),
        ..Default::default()
    }).unwrap();
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    let mut ids = Vec::new();
    for i in 0..9 {
        let title = format!("Project {i}: window contents and readable titles");
        s.launch(probe.to_str().unwrap(), &[&title, &format!("preview-{i}")]).unwrap();
        ids.push(s.wait_for_window(&title).unwrap().id);
    }
    // Every selection, including those beyond the first visible page, fits.
    s.door().key(keys::LEFTALT, true).unwrap();
    for i in 0..9 {
        s.door().tap_key(15).unwrap();
        s.door().barrier().unwrap();
        let world = s.world().unwrap();
        let panel = world.shells.iter().find(|s| s.above && s.mapped && s.buffer_bytes > 0).unwrap();
        let shot = s.screenshot(&format!("washi-switcher-{i}")).unwrap();
        assert!(panel.x >= 0 && panel.y >= 0);
        assert!(panel.x as u32 + panel.w <= shot.width && panel.y as u32 + panel.h <= shot.height);
        assert!(panel.h >= 260, "large preview cards and a separate selected title");
    }
    s.door().key(keys::LEFTALT, false).unwrap();
    s.door().barrier().unwrap();
    let focused = s.world().unwrap().logical_focus.unwrap();
    assert!(ids.contains(&focused));
    s.door().chord(keys::LEFTMETA, 50).unwrap();
    let tile = poll_until(WAIT, "large minimized preview", || {
        s.world().ok()?.shells.into_iter().find(|s| s.mapped && !s.above && s.w == 240 && s.h == 240)
    }).unwrap();
    assert!(!s.world().unwrap().frame_of(focused).unwrap().mapped);
    s.door().chord(keys::LEFTMETA, 32).unwrap(); // Show Desktop exposes tiles below ordinary windows.
    let shot = s.screenshot("washi-minimized-preview").unwrap();
    assert_eq!(shot.pixel(tile.x as u32 + tile.w / 2, tile.y as u32 + tile.h / 2), [32, 64, 128, 255]);
    let start = ((tile.x + 30) as f64, (tile.y + 30) as f64);
    s.door().drag_to(start, (start.0 + 300.0, start.1 - 120.0)).unwrap();
    s.door().button("left", false).unwrap();
    s.door().barrier().unwrap();
    let tile = s.world().unwrap().shells.into_iter().find(|s| s.mapped && !s.above).unwrap();
    assert_eq!((tile.x, tile.y), (start.0 as i32 - 30 + 300, start.1 as i32 - 30 - 120));
    assert!(!s.world().unwrap().frame_of(focused).unwrap().mapped, "dragging must not restore");
    s.door().click((tile.x + 30) as f64, (tile.y + 30) as f64).unwrap();
    poll_until(WAIT, "clicking the preview restores its window", || {
        let world = s.world().ok()?;
        (world.frame_of(focused)?.mapped && world.shells.iter().all(|s| s.above || !s.mapped)).then_some(())
    }).unwrap();
}

fn config(id: &str, scale: f32) -> String {
    format!(
        "omarchy_shell = false\nomarchy_menu = false\nhyprland_config = false\n\
         restore_session = false\ntheme = {id:?}\nscale = {scale}\noverview_style = 'cards'\n\
         [keybindings]\n\"super+r\" = \"root-menu\"\n"
    )
}

/// The two pinned applications are genuine clients with fixture-owned desktop
/// entries, so app discovery and launch routing do not depend on installed apps.
fn applications(count: usize) -> tempfile::TempDir {
    let data = tempfile::tempdir().unwrap();
    let apps = data.path().join("applications");
    std::fs::create_dir(&apps).unwrap();
    let probe =
        profile_binary("chonk-fullscreen-probe").expect("build chonk-testkit binaries first");
    for index in 0..count {
        let name = if index == 0 { "Terminal" } else { "Files" };
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName={name}\n\
             Exec={} ModernPin{index} modern-pin-{index}\nStartupWMClass=modern-pin-{index}\n",
            probe.display()
        );
        std::fs::write(apps.join(format!("modern-pin-{index}.desktop")), entry).unwrap();
    }
    data
}

fn boot(name: &str, data: &tempfile::TempDir, _pins: usize) -> Session {
    let options = SessionOptions {
        config_extra: config("obsidian", 1.0).replace("omarchy_shell = false\n", ""),
        env: vec![
            (
                "XDG_DATA_HOME".into(),
                data.path().to_string_lossy().into_owned(),
            ),
            (
                "XDG_DATA_DIRS".into(),
                data.path().to_string_lossy().into_owned(),
            ),
            (
                "OMARCHY_PATH".into(),
                data.path().to_string_lossy().into_owned(),
            ),
        ],
        ..Default::default()
    };
    let mut session = Session::boot(name, options).expect("modern nested session boots");
    assert!(session.world().unwrap().shells.is_empty());
    session
}

fn theme(id: &str, appearance: Appearance, scale: f32) -> Theme {
    theme_variant(id, appearance)
        .expect("registered modern variant")
        .scaled(scale)
}

fn restyle(s: &mut Session, id: &str, appearance: Appearance, scale: f32) -> World {
    s.rewrite_config(&config(id, scale)).unwrap();
    s.request_reload().unwrap();
    poll_until(WAIT, "theme and scale to reach the live session", || {
        let world = s.world().ok()?;
        (world.theme.id == id && world.scale == scale).then_some(())
    })
    .unwrap_or_else(|e| panic!("{e}\n{}", s.log()));
    let mode = appearance.name();
    std::fs::write(s.state_file("appearance-request"), mode).unwrap();
    poll_until(WAIT, "appearance request to be applied", || {
        let world = s.world().ok()?;
        (world.theme.appearance == mode && !s.state_file("appearance-request").exists())
            .then_some(world)
    })
    .unwrap_or_else(|e| panic!("{e}\n{}", s.log()))
}

#[test]
#[ignore = "requires nested Wayland and the chonk-testkit probe binaries"]
fn modern_fullscreen_releases_old_frame_clipping_and_restores_its_chrome() {
    let data = applications(1);
    let mut s = boot("modern-chrome-fullscreen", &data, 1);
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    s.launch(
        &probe.to_string_lossy(),
        &["ModernFullscreen", "modern-fullscreen"],
    )
    .unwrap();
    let window = s.wait_for_window("ModernFullscreen").unwrap();
    for id in THEMES {
        let world = restyle(&mut s, id, Appearance::Dark, 1.5);
        let windowed = world.windows.iter().find(|w| w.id == window.id).unwrap();
        let original = (windowed.w, windowed.h);
        s.door()
            .click(
                f64::from(windowed.x) + f64::from(windowed.w) / 2.0,
                f64::from(windowed.y) + f64::from(windowed.h) / 2.0,
            )
            .unwrap();
        // Present an ordinary decorated frame first: initial-fullscreen map
        // never installs the stale shape that this transition must release.
        s.screenshot(&format!("{id}-windowed")).unwrap();
        s.door().tap_key(KEY_F).unwrap();
        let _full = poll_until(WAIT, "the real fullscreen buffer to be presented", || {
            let world = s.world().ok()?;
            let client = world.windows.iter().find(|w| w.id == window.id)?;
            ((client.x, client.y, client.w, client.h) == (0, 0, world.output_w, world.output_h)
                && (client.presented_w, client.presented_h) == (world.output_w, world.output_h))
                .then_some(world)
        })
        .unwrap();
        let shot = s.screenshot(&format!("{id}-fullscreen")).unwrap();
        for (x, y) in [
            (0, 0),
            (shot.width - 1, 0),
            (0, shot.height - 1),
            (shot.width - 1, shot.height - 1),
            (shot.width / 2, shot.height / 2),
        ] {
            assert_eq!(
                &shot.pixel(x, y)[..3],
                &[0x20, 0x40, 0x80],
                "{id}: fullscreen must fill its output beyond the former rounded frame at {x},{y}"
            );
        }
        s.door().tap_key(KEY_F).unwrap();
        let restored = poll_until(WAIT, "windowed client and decoration to return", || {
            let world = s.world().ok()?;
            let client = world.windows.iter().find(|w| w.id == window.id)?;
            let frame = world.frames.iter().find(|f| f.window == window.id)?;
            ((client.w, client.h) == original
                && (client.presented_w, client.presented_h) == original
                && frame.mapped
                && frame.input_margin > 0
                && frame.h > client.h)
                .then_some(world)
        })
        .unwrap();
        assert_eq!(restored.logical_focus, Some(window.id));
        s.screenshot(&format!("{id}-restored")).unwrap();
    }
}

#[test]
#[ignore = "requires nested Wayland and the chonk-testkit probe binaries"]
fn modern_variants_restyle_existing_frames_without_persistent_chrome() {
    let data = applications(2);
    let mut s = boot("modern-chrome-matrix", &data, 2);
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    s.launch(&probe.to_string_lossy(), &["ModernPin0", "modern-pin"]).unwrap();
    let window = s.wait_for_window("ModernPin0").unwrap();
    for scale in [1.0, 1.5, 2.0, 1.0] {
        for id in THEMES {
            for appearance in [Appearance::Dark, Appearance::Light] {
                let world = restyle(&mut s, id, appearance, scale);
                assert_eq!(
                    world.theme.decoration_style, "modern",
                    "Auto follows the selected modern theme"
                );
                let active = theme(id, appearance, scale);
                let chrome = active.chrome.unwrap();
                assert!(world.shells.is_empty(), "theme reload must not recreate retired chrome");
                let client = world
                    .windows
                    .iter()
                    .find(|c| c.id == window.id)
                    .expect("live restyling preserves client identity");
                let frame = world
                    .frame_of(window.id)
                    .expect("server-decorated client retains its frame");
                assert_eq!(frame.input_margin, u32::from(chrome.frame.input_margin));
                assert!(
                    frame.y + frame.input_margin as i32 >= 0,
                    "restyling must keep the frame's visible title controls on screen"
                );
                assert_eq!(
                    client.y - frame.y,
                    i32::from(chrome.frame.input_margin + chrome.frame.title_height)
                );
                assert_eq!(
                    client.x - frame.x,
                    i32::from(chrome.frame.input_margin + chrome.frame.border)
                );
                assert_eq!(
                    frame.w - client.w,
                    u32::from(chrome.frame.input_margin + chrome.frame.border) * 2
                );
                s.door().barrier().unwrap();
                let shot = s
                    .screenshot(&format!("{id}-{}-{scale}", appearance.name()))
                    .unwrap();
                let background = shot.pixel(1, shot.height - 1);
                assert_eq!(
                    &background[..3],
                    &[
                        chrome.background.r,
                        chrome.background.g,
                        chrome.background.b
                    ],
                    "theme-default artwork did not follow its palette: {}",
                    shot.path.display()
                );
            }
        }
    }
    assert!(s.compositor_alive());
}


#[test]
#[ignore = "requires nested Wayland and the chonk-testkit probe binaries"]
fn modern_overlapping_previews_activate_the_visible_front_window() {
    let data = applications(2);
    let mut s = boot("modern-chrome-overlap", &data, 2);
    let active = theme("obsidian", Appearance::Dark, 1.0);
    let mut clients = Vec::new();
    for index in 0..2 {
        let probe = profile_binary("chonk-fullscreen-probe").unwrap();
        s.launch(&probe.to_string_lossy(), &[&format!("ModernPin{index}"), "modern-pin"]).unwrap();
        let client = s.wait_for_window(&format!("ModernPin{index}")).unwrap();
        let world = s.world().unwrap();
        let frame = world.frame_of(client.id).unwrap();
        let title_y = f64::from(frame.input_margin) + f64::from(active.titlebar.height) / 2.0;
        s.door()
            .drag_to(
                (
                    f64::from(frame.x) + f64::from(frame.w) / 2.0,
                    f64::from(frame.y) + title_y,
                ),
                (150.0 + f64::from(frame.w) / 2.0, 150.0 + title_y),
            )
            .unwrap();
        s.door().button("left", false).unwrap();
        s.door().barrier().unwrap();
        clients.push(client.id);
    }
    let world = s.world().unwrap();
    let front = world
        .windows
        .iter()
        .filter(|w| clients.contains(&w.id))
        .max_by_key(|w| w.stack_index)
        .unwrap()
        .id;
    s.door().chord(keys::LEFTMETA, keys::UP).unwrap();
    let opened = poll_until(WAIT, "overlapping live previews to settle", || {
        let world = s.world().ok()?;
        (world.overview.as_ref()?.progress == 1.0).then_some(world)
    })
    .unwrap();
    let first = opened
        .overview_windows
        .iter()
        .find(|w| w.id == clients[0])
        .unwrap();
    let second = opened
        .overview_windows
        .iter()
        .find(|w| w.id == clients[1])
        .unwrap();
    let overlap = first
        .rect
        .intersection(second.rect)
        .expect("posed previews overlap");
    let at = (
        f64::from(overlap.pos.x) + f64::from(overlap.size.w) / 2.0,
        f64::from(overlap.pos.y) + f64::from(overlap.size.h) / 2.0,
    );
    let index = opened
        .overview_windows
        .iter()
        .position(|w| w.id == front)
        .unwrap();
    s.door().motion(at.0, at.1).unwrap();
    s.door().barrier().unwrap();
    assert_eq!(
        s.world().unwrap().overview.unwrap().selected,
        index,
        "hover must select the texture actually visible above the overlap"
    );
    s.screenshot("obsidian-overlap-selection").unwrap();
    s.door().click(at.0, at.1).unwrap();
    poll_until(
        WAIT,
        "overlap click to activate the visible front window",
        || {
            let world = s.world().ok()?;
            (world.overview.is_none() && world.logical_focus == Some(front)).then_some(())
        },
    )
    .unwrap();
}

#[test]
#[ignore = "requires nested Wayland and the chonk-testkit probe binaries"]
fn modern_overview_drag_materializes_the_numbered_future_workspace() {
    let data = applications(1);
    let mut s = boot("modern-chrome-workspace-drag", &data, 1);
    restyle(&mut s, "relay", Appearance::Dark, 1.0);
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    s.launch(&probe.to_string_lossy(), &["ModernPin0", "modern-pin"]).unwrap();
    let client = s.wait_for_window("ModernPin0").unwrap();
    s.door().chord(keys::LEFTMETA, keys::UP).unwrap();
    let world = poll_until(WAIT, "native workspace cards to open", || {
        let world = s.world().ok()?;
        (world.overview.as_ref()?.progress == 1.0).then_some(world)
    })
    .unwrap();
    assert_eq!(world.workspace_count, 1);
    let window = world
        .overview_windows
        .iter()
        .find(|w| w.id == client.id)
        .unwrap();
    let source = window.rect;
    let target = world.overview_spaces[3].rect;
    let from = (
        f64::from(source.pos.x) + f64::from(source.size.w) / 2.0,
        f64::from(source.pos.y) + f64::from(source.size.h) / 2.0,
    );
    let to = (
        f64::from(target.pos.x) + f64::from(target.size.w) / 2.0,
        f64::from(target.pos.y) + f64::from(target.size.h) / 2.0,
    );
    s.door().drag_to(from, to).unwrap();
    s.screenshot("relay-future-workspace-drag").unwrap();
    s.door().button("left", false).unwrap();
    s.door().barrier().unwrap();
    poll_until(
        WAIT,
        "drop to create the fourth workspace and move the actual window",
        || {
            let world = s.world().ok()?;
            (world.overview.is_some()
                && world.workspace_count == 4
                && world.current_workspace == 0
                && world
                    .windows
                    .iter()
                    .any(|w| w.id == client.id && w.workspace == 3)
                && world.overview_space_windows.contains(&(3, client.id)))
            .then_some(())
        },
    )
    .unwrap();
    s.screenshot("relay-future-workspace-created").unwrap();
    s.door()
        .click(
            f64::from(target.pos.x) + 20.0,
            f64::from(target.pos.y) + 20.0,
        )
        .unwrap();
    poll_until(
        WAIT,
        "the new workspace card to focus its moved window",
        || {
            let world = s.world().ok()?;
            (world.overview.is_none()
                && world.current_workspace == 3
                && world.windows.iter().any(|w| w.id == client.id && w.mapped))
            .then_some(())
        },
    )
    .unwrap();
}
