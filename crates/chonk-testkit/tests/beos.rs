//! Real compositor acceptance: tab transparency, input, controls and restyling.
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
#[ignore = "isolated nested compositor: scripts/e2e.sh --headless --test beos"]
fn beos_pixels_cutout_input_buttons_and_live_restyle() {
    assert!(chonk_testkit::require_client("foot"));
    assert!(chonk_testkit::require_client("xterm"));
    for scale in [1, 2] {
        for x11 in [false, true] {
            let config=format!("scale = {scale}\ntheme = \"beos\"\ndecoration_style = \"auto\"\nshow_dock = false\nomarchy_shell = false\nhyprland_config = false\n");
            let mut session = Session::boot(
                &format!("beos-{scale}-{x11}"),
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
                        "beos-back",
                        "--title",
                        "Documents",
                        "--window-size-pixels",
                        "560x340",
                        "sleep",
                        "3600",
                    ],
                )
                .unwrap();
            let back = session.wait_for_window("beos-back").unwrap().id;
            move_to(&mut session, back, 60, 70, scale);
            session.door().motion(0.0, 0.0).unwrap();
            session.door().barrier().unwrap();
            let behind = session.screenshot("before-front").unwrap();
            let app = if x11 {
                session
                    .launch_x11_isolated(
                        "xterm",
                        &[
                            "-class",
                            "BeosXTerm",
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
                "BeosXTerm"
            } else {
                session
                    .launch_isolated(
                        "foot",
                        &[
                            "--app-id",
                            "beos-front",
                            "--title",
                            "Terminal",
                            "--window-size-pixels",
                            "380x220",
                            "sleep",
                            "3600",
                        ],
                    )
                    .unwrap();
                "beos-front"
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
                RasterThemeEngine::new(wm_theme::default_theme::theme_by_id("beos").unwrap())
                    .with_style(DecorationStyle::Auto)
                    .unwrap();
            let layout = engine.layout_at(&request, scale as f32);
            assert_eq!(
                (frame.w, frame.h),
                (layout.frame_size.w, layout.frame_size.h)
            );
            assert_eq!(window.y - frame.y, 24 * scale as i32);
            let capture = session.screenshot("beos-chrome").unwrap();
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
            let hole = layout.input_exclusion.unwrap();
            let (hx, hy) = (frame.x + hole.pos.x + 8, frame.y + 10 * scale as i32);
            assert_eq!(
                capture.pixel(hx as u32, hy as u32),
                behind.pixel(hx as u32, hy as u32)
            );
            session.door().click(f64::from(hx), f64::from(hy)).unwrap();
            assert!(
                pair(&mut session, back).0.stack_index > pair(&mut session, id).0.stack_index,
                "the empty tab band must reach the window beneath it"
            );
            // The rear window now covers the short tab. Cycle back through the
            // real switcher before exercising controls on the frontmost frame.
            session
                .door()
                .chord(chonk_testkit::keys::LEFTALT, 15)
                .unwrap();
            assert!(pair(&mut session, id).0.stack_index > pair(&mut session, back).0.stack_index);
            let before = pair(&mut session, id).0;
            for style in ["system7", "beos"] {
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
            // Zoom is attached to the short tab, independent of frame width.
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
            poll_until(Duration::from_secs(10), "tab zoom maximizes", || {
                (pair(&mut session, id).0.w > before.w).then_some(())
            })
            .unwrap();
            let (_, frame) = pair(&mut session, id);
            session
                .door()
                .click(
                    f64::from(frame.x + zoom.pos.x + 2),
                    f64::from(frame.y + zoom.pos.y + 2),
                )
                .unwrap();
            poll_until(Duration::from_secs(10), "tab zoom restores", || {
                (pair(&mut session, id).0.w == before.w).then_some(())
            })
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
