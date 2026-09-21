//! Render reviewable OS/2 Warp 4 chrome and shell specimens without a display server.
use tiny_skia::{Pixmap, PixmapPaint, Transform};
use wm_theme::{
    menu, overview, switcher, Appearance, DecorationStyle, FontState, RasterThemeEngine, UiChrome,
};
use wm_theme_api::{DecorationBuffer, DecorationRequest, Size, ThemeEngine};

fn blit(canvas: &mut Pixmap, buffer: &DecorationBuffer, x: i32, y: i32) {
    if let Some(image) =
        tiny_skia::PixmapRef::from_bytes(&buffer.pixels, buffer.width, buffer.height)
    {
        canvas.draw_pixmap(
            x,
            y,
            image,
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
}
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: os2warp_preview NEW_DIRECTORY");
    let path = std::path::Path::new(&path);
    std::fs::create_dir(path).expect("output must be a new directory");
    let fonts = FontState::new();
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let px = |n: u32| (n as f32 * scale).round() as u32;
        let theme = wm_theme::os2warp::theme("os2-warp-4", Appearance::Light)
            .unwrap()
            .scaled(scale);
        let engine = RasterThemeEngine::with_fonts_at_scale(theme.clone(), fonts.clone(), scale)
            .with_style(DecorationStyle::Auto)
            .unwrap();
        let chrome = UiChrome::new(&theme, fonts.clone(), DecorationStyle::Auto, scale);
        let mut screen = Pixmap::new(px(1024), px(700)).unwrap();
        let background = Pixmap::decode_png(include_bytes!(
            "../../chonk-shell/assets/wallpapers/os2-warp-4-1024.png"
        ))
        .unwrap();
        screen.draw_pixmap(
            0,
            0,
            background.as_ref(),
            &PixmapPaint {
                quality: tiny_skia::FilterQuality::Nearest,
                ..Default::default()
            },
            Transform::from_row(scale, 0.0, 0.0, scale, 0.0, -(px(34) as f32)),
            None,
        );
        for (title, x, y, w, h, focused) in [
            ("Welcome to ChonkStep", 48, 46, 505, 280, false),
            ("Terminal", 258, 158, 530, 278, true),
        ] {
            let req = DecorationRequest {
                content_size: Size::new(px(w), px(h)),
                title: title.into(),
                focused,
                resizable: true,
                buttons: Vec::new(),
            };
            let layout = engine.layout(&req);
            blit(
                &mut screen,
                &engine.render(&req, &layout),
                px(x) as i32,
                px(y) as i32,
            );
            wm_theme::paint::fill_rect(
                &mut screen,
                px(x) as i32 + layout.client_offset.x,
                px(y) as i32 + layout.client_offset.y,
                px(w),
                px(h),
                wm_theme::os2warp::WHITE,
            );
            let lines = if focused {
                vec![
                    "Welcome to ChonkStep.",
                    "",
                    "$ theme: OS/2 Warp 4",
                    "$ desktop: Warp Blue",
                    "",
                    "Purple titles. Crisp beveled frames.",
                    "Modern workspaces, familiar controls.",
                ]
            } else {
                vec![
                    "File     Edit     View",
                    "",
                    "Welcome to your desktop.",
                    "",
                    "Applications    Documents    Workspaces",
                ]
            };
            for (i, line) in lines.iter().enumerate() {
                // Example client content is illustrative; surrounding chrome
                // is rendered by the same engine as the live compositor.
                wm_theme::paint::draw_text(
                    &mut screen,
                    &mut fonts.system(),
                    &mut fonts.swash(),
                    line,
                    &theme.menu.item_font,
                    wm_theme::os2warp::INK,
                    px(x + 12) as i32,
                    px(y + 32 + i as u32 * 23) as i32,
                    px(w - 24),
                    px(20),
                    wm_theme::model::TextAlign::Left,
                );
            }
        }
        let items = vec![
            menu::MenuItem::Action {
                label: "About ChonkStep".into(),
                action: 1,
            },
            menu::MenuItem::Action {
                label: "Terminal".into(),
                action: 2,
            },
            menu::MenuItem::Submenu {
                label: "Applications".into(),
                items: Vec::new(),
            },
            menu::MenuItem::Submenu {
                label: "Workspaces".into(),
                items: Vec::new(),
            },
            menu::MenuItem::Submenu {
                label: "Preferences".into(),
                items: Vec::new(),
            },
            menu::MenuItem::Action {
                label: "Lock Screen".into(),
                action: 3,
            },
            menu::MenuItem::Action {
                label: "Log Out...".into(),
                action: 4,
            },
        ];
        let menu = chrome.menu(
            &theme,
            &mut fonts.system(),
            "ChonkStep",
            &items,
            Some(2),
            true,
        );
        blit(&mut screen, &menu.buffer, px(804) as i32, px(22) as i32);
        let entries = [
            switcher::SwitcherEntry {
                title: "Terminal".into(),
                preview: None,
            },
            switcher::SwitcherEntry {
                title: "Documents".into(),
                preview: None,
            },
            switcher::SwitcherEntry {
                title: "Browser".into(),
                preview: None,
            },
        ];
        let switcher = chrome.switcher(
            &theme,
            &mut fonts.system(),
            &mut fonts.swash(),
            &entries,
            0,
            px(86),
        );
        blit(&mut screen, &switcher, px(335) as i32, px(508) as i32);
        let icon = chrome.icon(
            &theme,
            &mut fonts.system(),
            &mut fonts.swash(),
            px(70),
            "Notes",
            None,
        );
        blit(&mut screen, &icon, px(32) as i32, px(587) as i32);
        screen
            .save_png(path.join(format!("desktop-{scale}x.png")))
            .unwrap();
        let entries = [
            overview::OverviewEntry {
                title: "Terminal",
                preview: None,
                miniaturized: false,
            },
            overview::OverviewEntry {
                title: "Documents",
                preview: None,
                miniaturized: false,
            },
        ];
        let layout = overview::layout(
            Size::new(px(640), px(400)),
            px(70),
            overview::header_height(&theme),
            entries.len(),
            3,
        );
        for (name, buffer) in [
            ("menu", menu.buffer),
            ("switcher", switcher),
            ("minimized", icon),
            (
                "overview",
                chrome.overview(
                    &theme,
                    &mut fonts.system(),
                    &mut fonts.swash(),
                    &entries,
                    (0, 3),
                    &layout,
                ),
            ),
        ] {
            tiny_skia::PixmapRef::from_bytes(&buffer.pixels, buffer.width, buffer.height)
                .unwrap()
                .save_png(path.join(format!("{name}-{scale}x.png")))
                .unwrap();
        }
    }
}
