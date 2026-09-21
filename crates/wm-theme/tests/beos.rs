//! Historical pixels are checked against a real OS screenshot, independently
//! of the painter. The substituted font is deliberately outside that oracle.
use wm_theme::{Appearance, DecorationStyle, FontState, RasterThemeEngine, UiChrome};
use wm_theme_api::{
    ButtonKind, ButtonRuntimeState, DecorationBuffer, DecorationRequest, Point, Size, ThemeEngine,
};

fn request() -> DecorationRequest {
    DecorationRequest {
        content_size: Size::new(301, 316),
        title: "GLTeapot".into(),
        focused: true,
        resizable: true,
        buttons: Vec::new(),
    }
}
fn engine() -> RasterThemeEngine {
    RasterThemeEngine::new(wm_theme::default_theme::theme_by_id("beos").unwrap())
        .with_style(DecorationStyle::Auto)
        .unwrap()
}
fn pixel(image: &DecorationBuffer, x: u32, y: u32) -> &[u8] {
    &image.pixels[((y * image.width + x) * 4) as usize..][..4]
}

#[test]
fn captured_controls_and_frame_bands_match_r5_at_integer_scales() {
    let capture = tiny_skia::Pixmap::decode_png(include_bytes!(
        "../../../docs/decoration-styles/beos/reference/r5-desktop.png"
    ))
    .unwrap();
    let at = |x: u32, y: u32| &capture.data()[((y * capture.width() + x) * 4) as usize..][..4];
    let engine = engine();
    for scale in [1, 2, 3] {
        for focused in [false, true] {
            let mut req = request();
            req.focused = focused;
            req.content_size = Size::new(301 * scale, 316 * scale);
            let layout = engine.layout_at(&req, scale as f32);
            let surface = engine.render_surface_at(&req, &layout, scale as f32);
            let top = &surface.parts[0].buffer;
            for &(kind, rect) in &layout.button_hitboxes {
                let origin = match (focused, kind) {
                    (true, ButtonKind::Close) => (99, 80),
                    (true, ButtonKind::Maximize) => (205, 80),
                    (false, ButtonKind::Close) => (54, 432),
                    (false, ButtonKind::Maximize) => (132, 432),
                    _ => unreachable!(),
                };
                for y in 0..rect.size.h {
                    for x in 0..rect.size.w {
                        assert_eq!(
                            pixel(top, rect.pos.x as u32 + x, rect.pos.y as u32 + y),
                            at(origin.0 + x / scale, origin.1 + y / scale),
                            "{kind:?} {focused} {scale}x ({x},{y})"
                        );
                    }
                }
            }
            // Entire straight side bands, including all five distinct tones.
            for x in 0..5 * scale {
                let left = &surface.parts[2].buffer;
                assert_eq!(
                    pixel(left, x, 20 * scale),
                    at(
                        if focused { 95 } else { 50 } + x / scale,
                        if focused { 200 } else { 600 }
                    )
                );
            }
            if focused {
                let bottom = &surface.parts.last().unwrap().buffer;
                for y in 0..5 * scale {
                    assert_eq!(pixel(bottom, 70 * scale, y), at(160, 416 + y / scale));
                }
            }
        }
    }
}

#[test]
fn tab_holes_controls_and_sparse_storage_stay_correct_across_sizes_and_scales() {
    let engine = engine();
    for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
        for width in [1, 12, 32, 100, 640, 4096] {
            for title in [
                "",
                "Terminal",
                "An unusually long title that must stay inside its tab",
                "E\u{301}ditor · 日本語 🦀",
            ] {
                let req = DecorationRequest {
                    content_size: Size::new(width, 120),
                    title: title.into(),
                    ..request()
                };
                let layout = engine.layout_at(&req, scale);
                let surface = engine.render_surface_at(&req, &layout, scale);
                assert!(
                    surface.retained_bytes()
                        <= 4 * (layout.frame_size.w * layout.client_offset.y as u32
                            + layout.frame_size.w * layout.client_offset.x as u32
                            + layout.frame_size.h * 2 * layout.client_offset.x as u32)
                            as usize
                );
                for (_, button) in &layout.button_hitboxes {
                    assert!(button.pos.x >= 0 && button.pos.y >= 0);
                    assert!(button.pos.x as u32 + button.size.w <= layout.frame_size.w);
                    assert!(button.pos.y as u32 + button.size.h <= layout.titlebar_height);
                }
                for (index, (_, button)) in layout.button_hitboxes.iter().enumerate() {
                    assert!(!layout.button_hitboxes[..index]
                        .iter()
                        .any(|(_, other)| other.contains(button.pos)));
                    assert!(!layout
                        .resize_hitboxes
                        .iter()
                        .any(|(_, other)| other.contains(button.pos)));
                }
                if let Some(hole) = layout.input_exclusion {
                    for y in 0..hole.size.h {
                        for x in hole.pos.x as u32..layout.frame_size.w {
                            assert!(surface.parts.iter().all(|part| {
                                let local_x = i64::from(x) - i64::from(part.offset.x);
                                let local_y = i64::from(y) - i64::from(part.offset.y);
                                local_x < 0
                                    || local_y < 0
                                    || local_x >= i64::from(part.buffer.width)
                                    || local_y >= i64::from(part.buffer.height)
                                    || pixel(&part.buffer, local_x as u32, local_y as u32)[3] == 0
                            }));
                            assert!(!layout
                                .resize_hitboxes
                                .iter()
                                .any(|(_, rect)| rect.contains(Point::new(x as i32, y as i32))));
                        }
                    }
                }
                let edge = engine.edges_layout_at(&req, scale);
                assert_eq!(edge.titlebar_height, 0);
                assert!(edge.input_exclusion.is_none());
                assert!(edge.button_hitboxes.is_empty());
                assert!(engine.render_edges_at(&req, &edge, scale).retained_bytes() > 0);
            }
        }
    }
}

#[test]
fn integer_scaling_replicates_text_controls_and_transparency_exactly() {
    let engine = engine();
    let req = request();
    let one = engine.render(&req, &engine.layout(&req));
    for scale in [2, 3] {
        let req = DecorationRequest {
            content_size: Size::new(req.content_size.w * scale, req.content_size.h * scale),
            ..req.clone()
        };
        let layout = engine.layout_at(&req, scale as f32);
        let surface = engine.render_surface_at(&req, &layout, scale as f32);
        for part in surface.parts {
            for y in 0..part.buffer.height {
                for x in 0..part.buffer.width {
                    assert_eq!(
                        pixel(&part.buffer, x, y),
                        pixel(
                            &one,
                            (part.offset.x as u32 + x) / scale,
                            (part.offset.y as u32 + y) / scale
                        ),
                        "{scale}x ({x},{y}) in {:?}",
                        part.offset
                    );
                }
            }
        }
    }
}

#[test]
fn hover_does_not_repaint_and_press_and_shade_keep_the_same_tab() {
    let engine = engine();
    let req = request();
    let layout = engine.layout(&req);
    let normal = engine.render(&req, &layout);
    let mut held = req.clone();
    held.buttons.push(ButtonRuntimeState {
        kind: ButtonKind::Close,
        hovered: true,
        pressed: false,
    });
    assert_eq!(normal, engine.render(&held, &layout));
    held.buttons[0].pressed = true;
    assert_ne!(normal, engine.render(&held, &layout));
    let mut shaded = layout.clone();
    shaded.frame_size.h = shaded.shaded_frame_height;
    held.resizable = false;
    let surface = engine.render_surface(&held, &shaded);
    assert_eq!(
        surface.parts[0].buffer,
        engine.render_surface(&held, &layout).parts[0].buffer
    );
}

#[test]
fn theme_round_trip_and_appearance_preserve_the_historical_chrome() {
    let fonts = FontState::new();
    let req = request();
    let mut outputs = Vec::new();
    for appearance in [Appearance::Light, Appearance::Dark] {
        let theme = wm_theme::default_theme::theme_variant("beos", appearance).unwrap();
        let restored: wm_theme::Theme = toml::from_str(&toml::to_string(&theme).unwrap()).unwrap();
        assert_eq!(
            restored.resolve_style(DecorationStyle::Auto),
            DecorationStyle::BeOS
        );
        assert_eq!(theme.wallpaper, "beos-blue");
        let engine = RasterThemeEngine::with_fonts(theme, fonts.clone())
            .with_style(DecorationStyle::Auto)
            .unwrap();
        outputs.push(engine.render(&req, &engine.layout(&req)));
    }
    assert_eq!(outputs[0], outputs[1]);
}

#[test]
fn menus_paint_their_hit_rows_and_bound_oversized_models() {
    let fonts = FontState::new();
    let theme = wm_theme::default_theme::theme_by_id("beos").unwrap();
    let items = [
        wm_theme::menu::MenuItem::Action {
            label: "Terminal".into(),
            action: 1,
        },
        wm_theme::menu::MenuItem::Submenu {
            label: "Applications".into(),
            items: Vec::new(),
        },
    ];
    for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
        let chrome = UiChrome::new(&theme, fonts.clone(), DecorationStyle::Auto, scale);
        let menu = chrome.menu(
            &theme,
            &mut fonts.system(),
            "ChonkStep",
            &items,
            Some(1),
            true,
        );
        assert_eq!(menu.item_rects.len(), 2);
        let row = menu.item_rects[1];
        assert_eq!(
            pixel(&menu.buffer, row.pos.x as u32 + 1, row.pos.y as u32 + 1),
            [152, 152, 152, 255]
        );
        assert!(menu.close_rect.is_some());
        let too_many = vec![items[0].clone(); 8192];
        let rejected = chrome.menu(
            &theme,
            &mut fonts.system(),
            "Too many",
            &too_many,
            None,
            false,
        );
        assert!(rejected.buffer.pixels.is_empty());
        assert!(rejected.item_rects.is_empty());
        assert!(!chrome
            .label(
                &theme,
                &mut fonts.system(),
                &mut fonts.swash(),
                "日本語",
                200,
                30,
                false
            )
            .pixels
            .is_empty());
    }
}

#[test]
fn menu_folder_pixels_fill_fractional_cells_without_gaps() {
    let fonts = FontState::new();
    let theme = wm_theme::default_theme::theme_by_id("beos").unwrap();
    let render = |scale| {
        UiChrome::new(&theme, fonts.clone(), DecorationStyle::Auto, scale)
            .menu(&theme, &mut fonts.system(), "Desktop", &[], None, false)
            .buffer
    };
    let source = render(1.0);
    for scale in [1.25, 1.5, 2.0, 3.0] {
        let image = render(scale);
        let px = |n: u32| (n as f32 * scale + 0.5).floor() as u32;
        for y in 0..12 {
            for x in 0..15 {
                let expected = pixel(&source, 8 + x, 6 + y);
                for dy in px(y)..px(y + 1) {
                    for dx in px(x)..px(x + 1) {
                        assert_eq!(
                            pixel(&image, px(8) + dx, px(6) + dy),
                            expected,
                            "{scale}x folder cell ({x},{y}), pixel ({dx},{dy})"
                        );
                    }
                }
            }
        }
    }
}
