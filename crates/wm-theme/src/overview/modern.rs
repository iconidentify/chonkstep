//! Workspace cards built from small labels, corner texels and uniform solids.
//! Window content remains a live transform supplied by the compositor.
use crate::{
    model::{Color, TextAlign},
    modern::Chrome,
    paint, Theme,
};
use wm_theme_api::{
    DecorationBuffer, DecorationPart, DecorationSolid, DecorationSurface, Point, Rect, Size,
};

pub fn layout(
    theme: &Theme,
    primary: Rect,
    sources: &[Rect],
    workspace: (usize, usize),
) -> super::OverviewLayout {
    layout_in(theme, primary, primary, sources, workspace)
}

/// The stage excludes external layer reservations; input and window source
/// coordinates still refer to the full output, preserving click alignment.
pub fn layout_in(
    theme: &Theme,
    primary: Rect,
    stage: Rect,
    sources: &[Rect],
    workspace: (usize, usize),
) -> super::OverviewLayout {
    let metrics = Chrome::from_theme(theme).overview;
    let stage = stage.intersection(primary).unwrap_or(primary);
    let mut grid = metrics.layout(stage.size, workspace.1);
    let offset = Point::new(
        stage.pos.x.saturating_sub(primary.pos.x),
        stage.pos.y.saturating_sub(primary.pos.y),
    );
    grid.bounds.pos.x = grid.bounds.pos.x.saturating_add(offset.x);
    grid.bounds.pos.y = grid.bounds.pos.y.saturating_add(offset.y);
    for card in &mut grid.cards {
        card.pos.x = card.pos.x.saturating_add(offset.x);
        card.pos.y = card.pos.y.saturating_add(offset.y);
    }
    let preview = grid
        .cards
        .get(workspace.0)
        .copied()
        .map(|card| metrics.preview(card))
        .unwrap_or_default();
    let source_bounds = wm_theme_api::overview_source_bounds(primary, sources.iter().copied());
    let cells = sources
        .iter()
        .map(|source| wm_theme_api::overview_thumbnail(*source, source_bounds, preview))
        .collect();
    super::OverviewLayout {
        panel: primary.size,
        header_h: u32::from(metrics.header),
        pad: u32::from(metrics.gap),
        cols: grid.cols,
        cells,
        strip: grid.cards,
        grid: grid.bounds,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn label(
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    cache: &mut cosmic_text::SwashCache,
    text: &str,
    width: u32,
    height: u32,
    accent: bool,
) -> DecorationBuffer {
    let width = width.clamp(1, 8192);
    let height = height.clamp(1, 256);
    let mut image = tiny_skia::Pixmap::new(width, height).unwrap();
    let chrome = Chrome::from_theme(theme);
    let mut font = theme.titlebar.font.clone();
    font.family = "IBM Plex Mono".into();
    let bounded = paint::elide(text, width, font.size);
    paint::draw_text_transparent(
        &mut image,
        fonts,
        cache,
        &bounded,
        &font,
        if accent { chrome.accent } else { chrome.muted },
        0,
        0,
        width,
        height,
        TextAlign::Left,
    );
    DecorationBuffer {
        width,
        height,
        pixels: image.take(),
    }
}

pub fn card_surface(theme: &Theme, size: Size) -> DecorationSurface {
    let mut out = DecorationSurface {
        frame_size: size,
        parts: Vec::new(),
        solids: Vec::new(),
        shadow: None,
        shape: None,
    };
    if size.w == 0 || size.h == 0 || size.w > 8192 || size.h > 8192 {
        return out;
    }
    let chrome = Chrome::from_theme(theme);
    let m = chrome.overview;
    let border = u32::from(m.border).min(size.w / 2).min(size.h / 2);
    let radius = u32::from(m.radius).min(size.w / 2).min(size.h / 2);
    let (w, h) = (size.w, size.h);
    let mut solid = |x, y, w, h, color: Color| {
        if w > 0 && h > 0 {
            out.solids.push(DecorationSolid {
                rect: Rect::new(Point::new(x as i32, y as i32), Size::new(w, h)),
                rgb: [color.r, color.g, color.b],
            });
        }
    };
    if radius == 0 {
        solid(0, 0, w, border, chrome.line);
        solid(0, h - border, w, border, chrome.line);
        solid(0, border, border, h - border * 2, chrome.line);
        solid(w - border, border, border, h - border * 2, chrome.line);
        solid(
            border,
            border,
            w - border * 2,
            h - border * 2,
            chrome.surface,
        );
        return out;
    }
    let r = radius.max(border);
    solid(0, r, border, h - r * 2, chrome.line);
    solid(w - border, r, border, h - r * 2, chrome.line);
    solid(border, r, w - border * 2, h - r * 2, chrome.surface);
    solid(r, 0, w - r * 2, border, chrome.line);
    solid(r, border, w - r * 2, r - border, chrome.surface);
    solid(r, h - r, w - r * 2, r - border, chrome.surface);
    solid(r, h - border, w - r * 2, border, chrome.line);
    for (x, y) in [(0, 0), (w - r, 0), (0, h - r), (w - r, h - r)] {
        let mut image = tiny_skia::Pixmap::new(r, r).unwrap();
        crate::modern::rounded_fill(
            &mut image,
            Rect::new(Point::new(-(x as i32), -(y as i32)), size),
            radius,
            chrome.line,
        );
        crate::modern::rounded_fill(
            &mut image,
            Rect::new(
                Point::new(border as i32 - x as i32, border as i32 - y as i32),
                Size::new(w - border * 2, h - border * 2),
            ),
            radius.saturating_sub(border),
            chrome.surface,
        );
        out.parts.push(DecorationPart {
            offset: Point::new(x as i32, y as i32),
            buffer: DecorationBuffer {
                width: r,
                height: r,
                pixels: image.take(),
            },
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_card_storage_tracks_corners_instead_of_preview_area() {
        for id in ["obsidian", "washi", "relay"] {
            let theme = crate::modern::theme(id, crate::Appearance::Dark).unwrap();
            let small = card_surface(&theme, Size::new(300, 180));
            let large = card_surface(&theme, Size::new(1200, 800));
            assert_eq!(small.retained_bytes(), large.retained_bytes());
            assert!(large.retained_bytes() <= 4 * 9 * 9 * 4);
            let size = large.frame_size;
            let area: u64 = large
                .parts
                .iter()
                .map(|part| u64::from(part.buffer.width) * u64::from(part.buffer.height))
                .chain(
                    large
                        .solids
                        .iter()
                        .map(|solid| u64::from(solid.rect.size.w) * u64::from(solid.rect.size.h)),
                )
                .sum();
            assert_eq!(
                area,
                u64::from(size.w) * u64::from(size.h),
                "patches and solids must tile exactly once"
            );
        }
    }

    #[test]
    fn virtual_flow_windows_remain_inside_the_active_workspace_preview() {
        let theme = crate::modern::theme("obsidian", crate::Appearance::Dark).unwrap();
        let primary = Rect::new(Point::new(800, 0), Size::new(1280, 800));
        let sources = [
            Rect::new(Point::new(850, 100), Size::new(400, 500)),
            Rect::new(Point::new(3000, 100), Size::new(600, 500)),
        ];
        let layout = layout(&theme, primary, &sources, (0, 4));
        let preview = theme.chrome.unwrap().overview.preview(layout.strip[0]);
        for cell in layout.cells {
            assert!(preview.contains(cell.pos));
            assert!(cell.pos.x + cell.size.w as i32 <= preview.pos.x + preview.size.w as i32 + 1);
            assert!(cell.pos.y + cell.size.h as i32 <= preview.pos.y + preview.size.h as i32 + 1);
        }
    }

    #[test]
    fn external_bar_reservation_offsets_cards_without_changing_output_coordinates() {
        let theme = crate::modern::theme("relay", crate::Appearance::Dark).unwrap();
        let primary = Rect::new(Point::new(800, 0), Size::new(1280, 800));
        let source = Rect::new(Point::new(900, 150), Size::new(400, 300));
        for top in [0, 33, 60] {
            let stage = Rect::new(Point::new(800, top), Size::new(1280, 800 - top as u32));
            let layout = layout_in(&theme, primary, stage, &[source], (0, 1));
            assert_eq!(layout.panel, primary.size);
            assert_eq!(layout.grid.pos.y, top + 66);
            let preview = theme.chrome.unwrap().overview.preview(layout.strip[0]);
            assert!(preview.contains(layout.cells[0].pos));
        }
    }
}
