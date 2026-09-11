//! Shared, explicit fixture generator. The checked-in oracle is read-only in tests.
use wm_theme::{FontState, RasterThemeEngine};
use wm_theme_api::{ButtonKind, ButtonRuntimeState, DecorationLayout, DecorationRequest, DecorationSurface, ResizeEdge, Size, ThemeEngine};

pub fn cases(mut visit: impl FnMut(&str, &DecorationLayout, &DecorationSurface)) {
    let fonts = FontState::new();
    let mut database = cosmic_text::fontdb::Database::new();
    database.load_font_data(include_bytes!("../fixtures/windowmaker/DejaVuSans-Bold.ttf").to_vec());
    database.set_sans_serif_family("DejaVu Sans");
    *fonts.system() = cosmic_text::FontSystem::new_with_locale_and_db("en-US".into(), database);
    for button_set in 0..3 {
        let mut theme = wm_theme::default_theme::nextstep_classic();
        match button_set {
            1 => theme.titlebar.buttons.retain(|button| button.kind == ButtonKind::Close),
            2 => {
                let mut zoom = theme.titlebar.buttons.iter().find(|button| button.kind == ButtonKind::Close).unwrap().clone();
                zoom.kind = ButtonKind::Maximize;
                theme.titlebar.buttons.push(zoom);
            }
            _ => {}
        }
        let engine = RasterThemeEngine::with_fonts(theme, fonts.clone());
        for scale in [1.0, 1.5, 2.0] {
            for focused in [false, true] {
                for resizable in [false, true] {
                    for shaded in [false, true] {
                        for (title_case, title) in ["", "A deliberately long title that reaches beyond the available chrome width"].into_iter().enumerate() {
                            for interaction in 0..5 {
                                let buttons = [ButtonKind::Close, ButtonKind::Miniaturize, ButtonKind::Maximize]
                                    .into_iter().enumerate().map(|(index, kind)| ButtonRuntimeState {
                                        kind, hovered: interaction == 1,
                                        pressed: interaction == index + 2,
                                    }).collect();
                                let mut request = DecorationRequest {
                                    content_size: Size::new(240, 120), title: title.into(),
                                    focused, resizable, buttons,
                                };
                                let mut layout = engine.layout_at(&request, scale);
                                if shaded {
                                    request.content_size.h = 0;
                                    request.resizable = false;
                                    layout.frame_size.h = layout.shaded_frame_height;
                                    layout.resize_hitboxes.clear();
                                }
                                let surface = engine.render_surface_at(&request, &layout, scale);
                                let name = format!("buttons-{button_set}_scale-{scale}_focused-{focused}_resizable-{resizable}_shaded-{shaded}_title-{title_case}_interaction-{interaction}");
                                visit(&name, &layout, &surface);
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn encoded(layout: &DecorationLayout, surface: &DecorationSurface) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [layout.frame_size.w, layout.frame_size.h,
        layout.client_offset.x as u32, layout.client_offset.y as u32,
        layout.titlebar_height, layout.shaded_frame_height, layout.button_hitboxes.len() as u32]
    {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for (kind, rect) in &layout.button_hitboxes {
        let kind = match kind { ButtonKind::Close => 0, ButtonKind::Miniaturize => 1, ButtonKind::Maximize => 2 };
        for value in [kind, rect.pos.x as u32, rect.pos.y as u32, rect.size.w, rect.size.h] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&(layout.resize_hitboxes.len() as u32).to_le_bytes());
    for (edge, rect) in &layout.resize_hitboxes {
        let edge = match edge {
            ResizeEdge::North => 0, ResizeEdge::South => 1, ResizeEdge::East => 2, ResizeEdge::West => 3,
            ResizeEdge::NorthEast => 4, ResizeEdge::NorthWest => 5, ResizeEdge::SouthEast => 6, ResizeEdge::SouthWest => 7,
        };
        for value in [edge, rect.pos.x as u32, rect.pos.y as u32, rect.size.w, rect.size.h] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    for value in [surface.frame_size.w, surface.frame_size.h, surface.parts.len() as u32, surface.retained_bytes() as u32] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for part in &surface.parts {
        for value in [part.offset.x as u32, part.offset.y as u32, part.buffer.width, part.buffer.height, part.buffer.pixels.len() as u32] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&part.buffer.pixels);
    }
    bytes
}
