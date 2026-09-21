//! Independent 1996 capture oracle: complete chrome, including bitmap text.
use wm_theme::{Appearance, DecorationStyle, FontState, RasterThemeEngine, UiChrome};
use wm_theme_api::{
    ButtonKind, ButtonRuntimeState, DecorationBuffer, DecorationRequest, Point, Size, ThemeEngine,
};
fn request() -> DecorationRequest {
    DecorationRequest {
        content_size: Size::new(490, 292),
        title: "OS/2 System Editor - C:\\IBMVESA\\TPADVESA.DOC".into(),
        focused: true,
        resizable: true,
        buttons: Vec::new(),
    }
}
fn engine() -> RasterThemeEngine {
    RasterThemeEngine::new(wm_theme::default_theme::theme_by_id("os2-warp-4").unwrap())
        .with_style(DecorationStyle::Auto)
        .unwrap()
}
fn pixel(image: &DecorationBuffer, x: u32, y: u32) -> &[u8] {
    &image.pixels[((y * image.width + x) * 4) as usize..][..4]
}

#[test]
fn complete_active_chrome_and_title_match_the_original_editor_capture() {
    let reference = tiny_skia::Pixmap::decode_png(include_bytes!(
        "../../../docs/decoration-styles/os2warp/reference/editor.png"
    ))
    .unwrap();
    let engine = engine();
    for scale in [1, 2, 3] {
        let mut req = request();
        req.content_size = Size::new(490 * scale, 292 * scale);
        let layout = engine.layout_at(&req, scale as f32);
        assert_eq!(
            layout.client_offset,
            Point::new(4 * scale as i32, 24 * scale as i32)
        );
        let surface = engine.render_surface_at(&req, &layout, scale as f32);
        for part in surface.parts {
            for y in 0..part.buffer.height {
                for x in 0..part.buffer.width {
                    let rx = (part.offset.x as u32 + x) / scale;
                    let ry = (part.offset.y as u32 + y) / scale;
                    // The per-application icon is deliberately replaced with a
                    // generic document; every other chrome pixel is historical.
                    if (4..22).contains(&rx) && (5..23).contains(&ry) {
                        continue;
                    }
                    let expected =
                        &reference.data()[((ry * reference.width() + rx) * 4) as usize..][..4];
                    assert_eq!(pixel(&part.buffer, x, y), expected, "{scale}x ({rx},{ry})");
                }
            }
        }
    }
}

#[test]
fn inactive_title_texture_and_text_match_the_original_desktop_capture() {
    let reference = tiny_skia::Pixmap::decode_png(include_bytes!(
        "../../../docs/decoration-styles/os2warp/reference/desktop.png"
    ))
    .unwrap();
    let engine = engine();
    let req = DecorationRequest {
        content_size: Size::new(397, 98),
        title: "Voice Manager-Midnite".into(),
        focused: false,
        resizable: false,
        ..request()
    };
    let image = engine.render(&req, &engine.layout(&req));
    for y in 0..24 {
        for x in 0..405 {
            if (4..22).contains(&x) && (5..23).contains(&y) {
                continue;
            }
            let expected =
                &reference.data()[(((344 + y) * reference.width() + 12 + x) * 4) as usize..][..4];
            assert_eq!(pixel(&image, x, y), expected, "inactive ({x},{y})");
        }
    }
}

#[test]
fn integer_scales_replicate_menus_and_both_frame_states() {
    let engine = engine();
    for focused in [true, false] {
        let req = DecorationRequest {
            focused,
            ..request()
        };
        let one = engine.render(&req, &engine.layout(&req));
        for scale in [2, 3] {
            let mut req = req.clone();
            req.content_size = Size::new(req.content_size.w * scale, req.content_size.h * scale);
            let layout = engine.layout_at(&req, scale as f32);
            for part in engine.render_surface_at(&req, &layout, scale as f32).parts {
                for y in 0..part.buffer.height {
                    for x in 0..part.buffer.width {
                        assert_eq!(
                            pixel(&part.buffer, x, y),
                            pixel(
                                &one,
                                (part.offset.x as u32 + x) / scale,
                                (part.offset.y as u32 + y) / scale
                            )
                        );
                    }
                }
            }
        }
    }
    let fonts = FontState::new();
    let theme = wm_theme::default_theme::theme_by_id("os2-warp-4").unwrap();
    let items = [
        wm_theme::menu::MenuItem::Action {
            label: "Terminal".into(),
            action: 1,
        },
        wm_theme::menu::MenuItem::Submenu {
            label: "Programs".into(),
            items: Vec::new(),
        },
    ];
    let render = |scale: f32| {
        let chrome = UiChrome::new(&theme, fonts.clone(), DecorationStyle::Auto, scale);
        chrome
            .menu(
                &theme,
                &mut fonts.system(),
                "Desktop",
                &items,
                Some(1),
                true,
            )
            .buffer
    };
    let one = render(1.0);
    let two = render(2.0);
    assert_eq!((two.width, two.height), (one.width * 2, one.height * 2));
    for y in 0..two.height {
        for x in 0..two.width {
            assert_eq!(pixel(&two, x, y), pixel(&one, x / 2, y / 2));
        }
    }
}

#[test]
fn fractional_tiny_unicode_edge_and_shaded_layouts_keep_controls_inside_chrome() {
    let engine = engine();
    for scale in [0.5, 1.0, 1.25, 1.5, 2.0, 3.0] {
        for width in [1, 12, 32, 100, 640, 4096] {
            let req = DecorationRequest {
                content_size: Size::new(width, 120),
                title: "Editor · 日本語 E\u{301} 🦀".into(),
                ..request()
            };
            let layout = engine.layout_at(&req, scale);
            assert!(layout.input_exclusion.is_none());
            for (i, (_, button)) in layout.button_hitboxes.iter().enumerate() {
                assert!(button.pos.x >= 0 && button.pos.y >= 0);
                assert!(button.pos.x as u32 + button.size.w <= layout.frame_size.w);
                assert!(button.pos.y as u32 + button.size.h <= layout.titlebar_height);
                assert!(!layout
                    .resize_hitboxes
                    .iter()
                    .any(|(_, r)| r.contains(button.pos)));
                assert!(!layout.button_hitboxes[..i]
                    .iter()
                    .any(|(_, r)| r.contains(button.pos)));
            }
            let surface = engine.render_surface_at(&req, &layout, scale);
            assert!(!surface.parts.is_empty());
            let edge = engine.edges_layout_at(&req, scale);
            assert_eq!(edge.titlebar_height, 0);
            assert!(edge.button_hitboxes.is_empty());
            assert!(!engine.render_edges_at(&req, &edge, scale).parts.is_empty());
            let mut shaded = layout.clone();
            shaded.frame_size.h = shaded.shaded_frame_height;
            assert_eq!(
                engine.render_surface_at(&req, &shaded, scale).parts[0],
                surface.parts[0]
            );
        }
    }
}

#[test]
fn hover_is_inert_pressed_controls_are_visible_and_appearance_preserves_chrome() {
    let engine = engine();
    let req = request();
    let layout = engine.layout(&req);
    let normal = engine.render(&req, &layout);
    for kind in [
        ButtonKind::Close,
        ButtonKind::Miniaturize,
        ButtonKind::Maximize,
        ButtonKind::Menu,
    ] {
        let mut held = req.clone();
        held.buttons.push(ButtonRuntimeState {
            kind,
            hovered: true,
            pressed: false,
        });
        assert_eq!(normal, engine.render(&held, &layout));
        held.buttons[0].pressed = true;
        assert_ne!(normal, engine.render(&held, &layout));
    }
    for appearance in [Appearance::Light, Appearance::Dark] {
        let theme = wm_theme::default_theme::theme_variant("os2-warp-4", appearance).unwrap();
        let restored: wm_theme::Theme = toml::from_str(&toml::to_string(&theme).unwrap()).unwrap();
        assert_eq!(
            restored.resolve_style(DecorationStyle::Auto),
            DecorationStyle::OS2Warp
        );
        let variant = RasterThemeEngine::new(theme)
            .with_style(DecorationStyle::Auto)
            .unwrap();
        assert_eq!(normal, variant.render(&req, &variant.layout(&req)));
    }
}

#[test]
fn menus_paint_their_hit_rows_and_bound_oversized_models() {
    let fonts = FontState::new();
    let theme = wm_theme::default_theme::theme_by_id("os2-warp-4").unwrap();
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
            [0, 0, 170, 255]
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
    let theme = wm_theme::default_theme::theme_by_id("os2-warp-4").unwrap();
    let render = |scale| {
        UiChrome::new(&theme, fonts.clone(), DecorationStyle::Auto, scale)
            .menu(
                &theme,
                &mut fonts.system(),
                "Desktop",
                &[wm_theme::menu::MenuItem::Submenu {
                    label: "Programs".into(),
                    items: Vec::new(),
                }],
                None,
                false,
            )
            .buffer
    };
    let source = render(1.0);
    for scale in [1.25, 1.5, 2.0, 3.0] {
        let image = render(scale);
        let px = |n: u32| (n as f32 * scale + 0.5).floor() as u32;
        for y in 0..16 {
            for x in 0..16 {
                let expected = pixel(&source, 5 + x, 4 + y);
                for dy in px(y)..px(y + 1) {
                    for dx in px(x)..px(x + 1) {
                        assert_eq!(
                            pixel(&image, px(5) + dx, px(2) + px(2) + dy),
                            expected,
                            "{scale}x folder cell ({x},{y}), pixel ({dx},{dy})"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn menu_text_matches_warpcenters_helv_bitmap_without_a_host_font_dependency() {
    let reference = tiny_skia::Pixmap::decode_png(include_bytes!(
        "../../../docs/decoration-styles/os2warp/reference/menus.png"
    ))
    .unwrap();
    let fonts = FontState::new();
    let theme = wm_theme::default_theme::theme_by_id("os2-warp-4").unwrap();
    let chrome = UiChrome::new(&theme, fonts.clone(), DecorationStyle::Auto, 1.0);
    let menu = chrome.menu(
        &theme,
        &mut fonts.system(),
        "Desktop",
        &[wm_theme::menu::MenuItem::Action {
            label: "Command Prompts".into(),
            action: 1,
        }],
        None,
        false,
    );
    assert_eq!(menu.close_rect, None);
    for y in 0..13 {
        for x in 0..88 {
            let reference =
                &reference.data()[(((6 + y) * reference.width() + 149 + x) * 4) as usize..][..4];
            // ToastyTech's capture is quantized (8/206), unlike GUIdebook's
            // 0/207 palette. Compare the binary glyph, not those two encodings.
            assert_eq!(
                pixel(&menu.buffer, 26 + x, 5 + y)[0] == 0,
                reference[0] == 8,
                "menu ({x},{y})"
            );
        }
    }
}

#[test]
fn menu_folder_matches_the_original_workplace_shell_pixels() {
    let reference = tiny_skia::Pixmap::decode_png(include_bytes!(
        "../../../docs/decoration-styles/os2warp/reference/menus.png"
    ))
    .unwrap();
    let fonts = FontState::new();
    let theme = wm_theme::default_theme::theme_by_id("os2-warp-4").unwrap();
    let chrome = UiChrome::new(&theme, fonts.clone(), DecorationStyle::Auto, 1.0);
    let menu = chrome.menu(
        &theme,
        &mut fonts.system(),
        "Desktop",
        &[wm_theme::menu::MenuItem::Submenu {
            label: "Programs".into(),
            items: Vec::new(),
        }],
        Some(0),
        false,
    );
    for y in 0..16 {
        for x in 0..16 {
            let original =
                &reference.data()[(((3 + y) * reference.width() + 276 + x) * 4) as usize..][..4];
            let canonical = match original {
                [0, 0, 173, 255] => [0, 0, 170, 255],
                [8, 8, 8, 255] => [0, 0, 0, 255],
                [206, 206, 206, 255] => [207, 207, 207, 255],
                [132, 132, 132, 255] => [130, 130, 130, 255],
                other => other.try_into().unwrap(),
            };
            assert_eq!(
                pixel(&menu.buffer, 5 + x, 4 + y),
                canonical,
                "folder ({x},{y})"
            );
        }
    }
}

#[test]
fn cached_narrow_titles_distinguish_the_visible_control_set() {
    let fonts = FontState::new();
    let theme = wm_theme::default_theme::theme_by_id("os2-warp-4").unwrap();
    let cached = RasterThemeEngine::with_fonts(theme.clone(), fonts.clone())
        .with_style(DecorationStyle::Auto)
        .unwrap();
    for resizable in [true, false, true] {
        let req = DecorationRequest {
            content_size: Size::new(56, 120),
            resizable,
            ..request()
        };
        let layout = cached.layout(&req);
        let cold = RasterThemeEngine::with_fonts(theme.clone(), fonts.clone())
            .with_style(DecorationStyle::Auto)
            .unwrap();
        assert_eq!(cached.render(&req, &layout), cold.render(&req, &layout));
    }
}
