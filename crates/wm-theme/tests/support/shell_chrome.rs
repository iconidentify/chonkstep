//! Deliberate implementation-golden matrix. The bundled DejaVu font makes the
//! legacy text deterministic; System 7 cases here use only the original atlas.
use wm_theme::{UiChrome, FontState, DecorationStyle, icon, menu, overview, switcher};
use wm_theme_api::{DecorationBuffer, Size};

pub fn cases(mut visit: impl FnMut(&str, &DecorationBuffer, &[u8])) {
    let fonts = FontState::new();
    let mut db = cosmic_text::fontdb::Database::new();
    db.load_font_data(include_bytes!("../fixtures/windowmaker/DejaVuSans-Bold.ttf").to_vec());
    db.set_sans_serif_family("DejaVu Sans");
    *fonts.system() = cosmic_text::FontSystem::new_with_locale_and_db("en-US".into(), db);
    let preview = preview();
    for style in [DecorationStyle::WindowMaker, DecorationStyle::System7] {
        for scale in [1.0, 1.5, 2.0] {
            for palette in ["nextstep-classic", "amber-phosphor"] {
                let theme = wm_theme::default_theme::theme_variant(palette, wm_theme::Appearance::Dark).unwrap().scaled(scale);
                let chrome = UiChrome::new(&theme, fonts.clone(), style, scale);
                let prefix = format!("{}-{palette}-{scale}", style.name());
                let (mut fs, mut sc) = (fonts.system(), fonts.swash());
                let items = [menu::MenuItem::Action { label: "Terminal".into(), action: 1 },
                    menu::MenuItem::Submenu { label: "Applications".into(), items: Vec::new() },
                    menu::MenuItem::Action { label: "Workspace 2".into(), action: 2 }];
                for (surface, closable, selected) in [("root-menu", true, Some(1)), ("window-menu", false, Some(0)),
                    ("cascade-menu", false, None)] {
                    let render = chrome.menu(&theme, &mut fs, "ChonkStep", &items, selected, closable);
                    if style == DecorationStyle::WindowMaker {
                        let legacy = menu::render_menu(&theme, &mut fs, "ChonkStep", &items, selected, closable);
                        assert_eq!(render.buffer, legacy.buffer);
                        assert_eq!(render.item_rects, legacy.item_rects);
                        assert_eq!(render.close_rect, legacy.close_rect);
                    }
                    let geometry: Vec<u8> = render.item_rects.iter().chain(render.close_rect.iter())
                        .flat_map(|r| [r.pos.x as u32, r.pos.y as u32, r.size.w, r.size.h])
                        .flat_map(u32::to_le_bytes).collect();
                    visit(&format!("{prefix}-{surface}"), &render.buffer, &geometry);
                }
                let tile = (56.0 * scale) as u32;
                let entries = [switcher::SwitcherEntry { title: "Terminal".into(), preview: Some(preview.clone()) },
                    switcher::SwitcherEntry { title: "Notes".into(), preview: None }];
                let render = chrome.switcher(&theme, &mut fs, &mut sc, &entries, 1, tile);
                if style == DecorationStyle::WindowMaker {
                    assert_eq!(render, switcher::render_switcher(&theme, &mut fs, &mut sc, &entries, 1, tile));
                }
                visit(&format!("{prefix}-switcher"), &render, &[]);
                let render = chrome.icon(&theme, &mut fs, &mut sc, tile, "Terminal", Some(&preview));
                if style == DecorationStyle::WindowMaker {
                    assert_eq!(render, icon::render_icon_tile(&theme, &mut fs, &mut sc, tile, "Terminal", Some(&preview)));
                }
                visit(&format!("{prefix}-icon"), &render, &[]);
                let items = [overview::OverviewEntry { title: "Terminal", preview: Some(&preview), miniaturized: false },
                    overview::OverviewEntry { title: "Notes", preview: None, miniaturized: true }];
                let layout = overview::layout(Size::new((640.0 * scale) as u32, (400.0 * scale) as u32),
                    tile, overview::header_height(&theme), items.len(), 2);
                let render = chrome.overview(&theme, &mut fs, &mut sc, &items, (0, 2), &layout);
                if style == DecorationStyle::WindowMaker {
                    assert_eq!(render, overview::render_overview(&theme, &mut fs, &mut sc, &items, (0, 2), &layout));
                }
                visit(&format!("{prefix}-overview"), &render, &[]);
                let render = chrome.selection(&theme, &mut fs, &mut sc, &items[1], layout.cells[1].size, layout.pad);
                if style == DecorationStyle::WindowMaker {
                    assert_eq!(render, overview::render_selection(&theme, &mut fs, &mut sc, &items[1], layout.cells[1].size, layout.pad));
                }
                visit(&format!("{prefix}-overview-selection"), &render, &[]);
                for selected in [false, true] {
                    let render = chrome.label(&theme, &mut fs, &mut sc, "Desktop 2 - Terminal", tile * 4, tile / 2, selected);
                    if style == DecorationStyle::WindowMaker {
                        assert_eq!(render, overview::live::label(&theme, &mut fs, &mut sc, "Desktop 2 - Terminal", tile * 4, tile / 2));
                    }
                    visit(&format!("{prefix}-caption-{selected}"), &render, &[]);
                }
                let render = chrome.workspace_close((18.0 * scale) as u32);
                if style == DecorationStyle::WindowMaker { assert_eq!(render, overview::workspace_close_glyph((18.0 * scale) as u32)); }
                visit(&format!("{prefix}-desktop-close"), &render, &[]);
            }
        }
    }
}

fn preview() -> DecorationBuffer {
    let (width, height) = (160, 90);
    let mut pixels = vec![0; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let color = if x > 8 && x < 100 && y % 12 >= 4 && y % 12 <= 6 { [210, 225, 235, 255] }
                else { [25, 45, 60, 255] };
            pixels[(y * width + x) * 4..(y * width + x) * 4 + 4].copy_from_slice(&color);
        }
    }
    DecorationBuffer { width: width as u32, height: height as u32, pixels }
}
