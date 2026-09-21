//! Real compositor acceptance: Warp 4 pixels, menu icon, controls and restyling.
use chonk_testkit::{poll_until, FrameInfo, Session, SessionOptions, WindowInfo};
use std::time::Duration;
use wm_theme::{DecorationStyle, RasterThemeEngine};
use wm_theme_api::{ButtonKind, DecorationRequest, Size, ThemeEngine};

fn pair(session: &mut Session, id: u64) -> (WindowInfo, FrameInfo) {
    let world = session.world().unwrap();
    (
        world.windows.into_iter().find(|w| w.id == id).unwrap(),
        world.frames.into_iter().find(|f| f.window == id).unwrap(),
    )
}
fn move_to(session: &mut Session, id: u64, x: i32, y: i32, scale: u32) {
    let (_, frame) = pair(session, id);
    let from = (
        f64::from(frame.x + 40 * scale as i32),
        f64::from(frame.y + 10 * scale as i32),
    );
    session
        .door()
        .drag_to(
            from,
            (
                from.0 + f64::from(x - frame.x),
                from.1 + f64::from(y - frame.y),
            ),
        )
        .unwrap();
    session.door().button("left", false).unwrap();
}

#[test]
#[ignore = "isolated nested compositor: scripts/e2e.sh --headless --test os2warp"]
fn os2warp_pixels_menu_buttons_and_live_restyle() {
    assert!(chonk_testkit::require_client("foot"));
    assert!(chonk_testkit::require_client("xterm"));
    for scale in [1, 2] {
        for x11 in [false, true] {
            let config=format!("scale = {scale}\ntheme = \"os2-warp-4\"\ndecoration_style = \"auto\"\nshow_dock = false\nomarchy_shell = false\nhyprland_config = false\n");
            let mut session = Session::boot(
                &format!("os2warp-{scale}-{x11}"),
                SessionOptions {
                    config_extra: config.replace("omarchy_shell = false\n", ""),
                    ..Default::default()
                },
            )
            .unwrap();
            session
                .launch_isolated(
                    "foot",
                    &[
                        "--app-id",
                        "os2warp-back",
                        "--title",
                        "Documents",
                        "--window-size-pixels",
                        "560x340",
                        "sleep",
                        "3600",
                    ],
                )
                .unwrap();
            let back = session.wait_for_window("os2warp-back").unwrap().id;
            move_to(&mut session, back, 60, 70, scale);
            session.door().motion(0.0, 0.0).unwrap();
            session.door().barrier().unwrap();
            session.screenshot("before-front").unwrap();
            let app = if x11 {
                session
                    .launch_x11_isolated(
                        "xterm",
                        &[
                            "-class",
                            "Os2WarpXTerm",
                            "-title",
                            "Terminal",
                            "-geometry",
                            "40x10",
                            "-e",
                            "sleep",
                            "3600",
                        ],
                    )
                    .unwrap();
                "Os2WarpXTerm"
            } else {
                session
                    .launch_isolated(
                        "foot",
                        &[
                            "--app-id",
                            "os2warp-front",
                            "--title",
                            "Terminal",
                            "--window-size-pixels",
                            "380x220",
                            "sleep",
                            "3600",
                        ],
                    )
                    .unwrap();
                "os2warp-front"
            };
            let id = session.wait_for_window(app).unwrap().id;
            move_to(&mut session, id, 100, 140, scale);
            session.door().motion(0.0, 0.0).unwrap();
            session.door().barrier().unwrap();
            let (window, frame) = pair(&mut session, id);
            let request = DecorationRequest {
                content_size: Size::new(window.w, window.h),
                title: window.title.clone(),
                focused: true,
                resizable: true,
                buttons: Vec::new(),
            };
            let engine =
                RasterThemeEngine::new(wm_theme::default_theme::theme_by_id("os2-warp-4").unwrap())
                    .with_style(DecorationStyle::Auto)
                    .unwrap();
            let layout = engine.layout_at(&request, scale as f32);
            assert_eq!(
                (frame.w, frame.h),
                (layout.frame_size.w, layout.frame_size.h)
            );
            assert_eq!(window.y - frame.y, 24 * scale as i32);
            let capture = session.screenshot("os2warp-chrome").unwrap();
            for part in engine
                .render_surface_at(&request, &layout, scale as f32)
                .parts
            {
                for y in 0..part.buffer.height {
                    for x in 0..part.buffer.width {
                        let rgba =
                            &part.buffer.pixels[((y * part.buffer.width + x) * 4) as usize..][..4];
                        let (gx, gy) = (
                            (frame.x + part.offset.x) as u32 + x,
                            (frame.y + part.offset.y) as u32 + y,
                        );
                        if gx >= capture.width || gy >= capture.height {
                            continue;
                        }
                        if rgba[3] != 0 {
                            assert_eq!(
                                capture.pixel(gx, gy).as_slice(),
                                rgba,
                                "{scale}x X11={x11} ({gx},{gy})"
                            );
                        }
                    }
                }
            }
            assert!(layout.input_exclusion.is_none());
            let menu = layout
                .button_hitboxes
                .iter()
                .find(|(kind, _)| *kind == ButtonKind::Menu)
                .unwrap()
                .1;
            session
                .door()
                .click(
                    f64::from(frame.x + menu.pos.x + 2),
                    f64::from(frame.y + menu.pos.y + 2),
                )
                .unwrap();
            session.screenshot("window-menu").unwrap();
            session.door().tap_key(1).unwrap();
            session
                .door()
                .chord(chonk_testkit::keys::LEFTALT, 15)
                .unwrap();
            session
                .door()
                .chord(chonk_testkit::keys::LEFTALT, 15)
                .unwrap();
            let before = pair(&mut session, id).0;
            for style in ["system7", "os2warp"] {
                session
                    .rewrite_config(&config.replace(
                        "decoration_style = \"auto\"",
                        &format!("decoration_style = \"{style}\""),
                    ))
                    .unwrap();
                session.request_reload().unwrap();
                poll_until(Duration::from_secs(10), "style reload", || {
                    (session.world().ok()?.theme.decoration_style == style).then_some(())
                })
                .unwrap();
                let after = pair(&mut session, id).0;
                assert_eq!(
                    (after.x, after.y, after.w, after.h),
                    (before.x, before.y, before.w, before.h)
                );
            }
            // Controls stay attached to the right edge after maximize.
            let zoom = layout
                .button_hitboxes
                .iter()
                .find(|(kind, _)| *kind == ButtonKind::Maximize)
                .unwrap()
                .1;
            let (_, frame) = pair(&mut session, id);
            session
                .door()
                .click(
                    f64::from(frame.x + zoom.pos.x + 2),
                    f64::from(frame.y + zoom.pos.y + 2),
                )
                .unwrap();
            poll_until(Duration::from_secs(10), "maximize", || {
                (pair(&mut session, id).0.w > before.w).then_some(())
            })
            .unwrap();
            let (window, frame) = pair(&mut session, id);
            let current = engine.layout_at(
                &DecorationRequest {
                    content_size: Size::new(window.w, window.h),
                    ..request.clone()
                },
                scale as f32,
            );
            let zoom = current
                .button_hitboxes
                .iter()
                .find(|(kind, _)| *kind == ButtonKind::Maximize)
                .unwrap()
                .1;
            session
                .door()
                .click(
                    f64::from(frame.x + zoom.pos.x + 2),
                    f64::from(frame.y + zoom.pos.y + 2),
                )
                .unwrap();
            poll_until(Duration::from_secs(10), "restore", || {
                (pair(&mut session, id).0.w == before.w).then_some(())
            })
            .unwrap();
            let (_, frame) = pair(&mut session, id);
            let minimize = layout
                .button_hitboxes
                .iter()
                .find(|(kind, _)| *kind == ButtonKind::Miniaturize)
                .unwrap()
                .1;
            session
                .door()
                .click(
                    f64::from(frame.x + minimize.pos.x + 2),
                    f64::from(frame.y + minimize.pos.y + 2),
                )
                .unwrap();
            poll_until(Duration::from_secs(10), "minimize hides client", || {
                (!pair(&mut session, id).1.mapped).then_some(())
            })
            .unwrap();
            session.screenshot("minimized-preview").unwrap();
            session
                .door()
                .chord(chonk_testkit::keys::LEFTALT, 15)
                .unwrap();
            poll_until(
                Duration::from_secs(10),
                "switcher restores minimized client",
                || pair(&mut session, id).1.mapped.then_some(()),
            )
            .unwrap();
            let (_, frame) = pair(&mut session, id);
            let close = layout
                .button_hitboxes
                .iter()
                .find(|(kind, _)| *kind == ButtonKind::Close)
                .unwrap()
                .1;
            session
                .door()
                .click(
                    f64::from(frame.x + close.pos.x + 2),
                    f64::from(frame.y + close.pos.y + 2),
                )
                .unwrap();
            poll_until(Duration::from_secs(10), "close removes frame", || {
                (!session.world().ok()?.windows.iter().any(|w| w.id == id)).then_some(())
            })
            .unwrap();
            session.door().motion(10.0, 10.0).unwrap();
            session.door().button("right", true).unwrap();
            session.door().button("right", false).unwrap();
            session.door().motion(0.0, 0.0).unwrap();
            session.screenshot("desktop-menu").unwrap();
            session.door().tap_key(1).unwrap(); // Escape dismisses the posted menu.
        }
    }
}
