use wm_theme::{DecorationStyle, FontState, RasterThemeEngine};
use wm_theme_api::{ButtonKind, ButtonRuntimeState, DecorationRequest, Size, ThemeEngine};

pub fn cases(mut visit: impl FnMut(String, Vec<u8>)) {
    let engine = RasterThemeEngine::with_fonts(wm_theme::default_theme::nextstep_classic(), FontState::new())
        .with_style(DecorationStyle::System7).unwrap();
    for scale in [1.25, 1.5] {
        for focused in [false, true] {
            for resizable in [false, true] {
                for shaded in [false, true] {
                    for pressed in [None, Some(ButtonKind::Close), Some(ButtonKind::Maximize)] {
                        for title in ["", "Terminal — Café naïf Ångström", "A deliberately long bitmap title clipped at its final pixel"] {
                            let request = DecorationRequest { content_size: Size::new(400, if shaded { 0 } else { 120 }),
                                title: title.replace('—', "-"), focused, resizable,
                                buttons: pressed.map(|kind| ButtonRuntimeState { kind, hovered: true, pressed: true }).into_iter().collect() };
                            let mut layout = engine.layout_at(&request, scale);
                            if shaded { layout.frame_size.h = layout.shaded_frame_height; }
                            let surface = engine.render_surface_at(&request, &layout, scale);
                            let name = format!("{scale}/{focused}/{resizable}/{shaded}/{pressed:?}/{title}");
                            let mut bytes = format!("{layout:?}\n").into_bytes();
                            for part in surface.parts {
                                bytes.extend_from_slice(format!("{:?}/{}/{}\n", part.offset, part.buffer.width, part.buffer.height).as_bytes());
                                bytes.extend_from_slice(&part.buffer.pixels);
                            }
                            visit(name, bytes);
                        }
                    }
                }
            }
        }
    }
}
