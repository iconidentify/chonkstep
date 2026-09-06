//! Geometry and small labels for compositor-native Overview. No window pixels
//! are rasterized here. A common scale preserves relative window sizes.

use super::OverviewLayout;
use crate::{
    model::{Color, TextAlign},
    paint, Theme,
};
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

pub fn layout(panel: Size, tile: u32, sizes: &[Size], workspaces: usize) -> OverviewLayout {
    let pad = (tile / 4).max(8).min(panel.w / 8).min(panel.h / 8);
    let label_h = (tile / 2).max(16);
    let available = panel.w.saturating_sub(pad * 2);
    let space_gap = pad.min(available / (workspaces.max(1) as u32 * 2));
    let width = (tile * 3).min(
        available.saturating_sub(space_gap * workspaces.saturating_sub(1) as u32)
            / workspaces.max(1) as u32,
    );
    let height = ((width as f64 * panel.h as f64 / panel.w.max(1) as f64) as u32).min(panel.h / 5);
    let width = width.min((height as f64 * panel.w as f64 / panel.h.max(1) as f64) as u32);
    let total = width * workspaces as u32 + space_gap * workspaces.saturating_sub(1) as u32;
    let strip: Vec<_> = (0..workspaces)
        .map(|i| {
            Rect::new(
                Point::new(
                    ((panel.w - total) / 2 + i as u32 * (width + space_gap)) as i32,
                    pad as i32,
                ),
                Size::new(width, height),
            )
        })
        .collect();
    let header_h = if strip.is_empty() {
        pad
    } else {
        pad * 2 + height + label_h
    };
    let grid = Rect::new(
        Point::new(pad as i32, header_h.min(panel.h) as i32),
        Size::new(available, panel.h.saturating_sub(header_h + pad + label_h)),
    );
    // At most 32 linear passes, only when the entry set changes. No iterative
    // physics, per-frame packing, or combinatorial rectangle search.
    let mut best = (1, 0.0_f64);
    let gap = (pad * 2).min(grid.size.h / (sizes.len().max(1) as u32).isqrt().max(1) / 3);
    for cols in 1..=sizes.len().min(32) {
        let rows = sizes.len().div_ceil(cols);
        let mut scale = 0.8_f64;
        let mut height = 0.0_f64;
        for row in sizes.chunks(cols) {
            let width: f64 = row.iter().map(|s| s.w.max(1) as f64).sum();
            height += row.iter().map(|s| s.h.max(1)).max().unwrap_or(1) as f64;
            scale = scale.min(
                grid.size
                    .w
                    .saturating_sub(gap * row.len().saturating_sub(1) as u32)
                    as f64
                    / width,
            );
        }
        scale = scale.min(
            grid.size
                .h
                .saturating_sub(gap * rows.saturating_sub(1) as u32) as f64
                / height.max(1.0),
        );
        if scale > best.1 {
            best = (cols, scale);
        }
    }
    let (cols, scale) = best;
    let mut cells = Vec::with_capacity(sizes.len());
    let heights: Vec<u32> = sizes
        .chunks(cols)
        .map(|row| (row.iter().map(|s| s.h.max(1)).max().unwrap_or(1) as f64 * scale) as u32)
        .collect();
    let total_h = heights.iter().sum::<u32>() + gap * heights.len().saturating_sub(1) as u32;
    let mut y = grid.pos.y + (grid.size.h.saturating_sub(total_h) / 2) as i32;
    for (row, height) in sizes.chunks(cols).zip(heights) {
        let row_w = row
            .iter()
            .map(|s| (s.w.max(1) as f64 * scale) as u32)
            .sum::<u32>()
            + gap * row.len().saturating_sub(1) as u32;
        let mut x = grid.pos.x + (grid.size.w.saturating_sub(row_w) / 2) as i32;
        for size in row {
            let size = Size::new(
                (size.w.max(1) as f64 * scale) as u32,
                (size.h.max(1) as f64 * scale) as u32,
            );
            cells.push(Rect::new(
                Point::new(x, y + (height.saturating_sub(size.h) / 2) as i32),
                size,
            ));
            x += (size.w + gap) as i32;
        }
        y += (height + gap) as i32;
    }
    OverviewLayout {
        panel,
        header_h,
        pad,
        cols,
        cells,
        strip,
        grid,
    }
}

/// Compact translucent caption. Rasterized once per entry, then reused on
/// hover. Font size follows the desktop scale, not the thumbnail scale.
pub fn label(
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    cache: &mut cosmic_text::SwashCache,
    text: &str,
    max_width: u32,
    height: u32,
) -> DecorationBuffer {
    let font = &theme.menu.item_font;
    let width = ((text.chars().count() as f32 * font.size * 0.65) as u32 + height)
        .min(max_width)
        .max(1);
    let height = height.max(1);
    let mut pixmap = tiny_skia::Pixmap::new(width, height).expect("bounded label");
    let mut fill = tiny_skia::Paint::default();
    fill.set_color_rgba8(25, 27, 32, 220);
    pixmap.fill_rect(
        tiny_skia::Rect::from_xywh(0.0, 0.0, width as f32, height as f32).unwrap(),
        &fill,
        tiny_skia::Transform::identity(),
        None,
    );
    paint::draw_text(
        &mut pixmap,
        fonts,
        cache,
        &paint::elide(text, width.saturating_sub(height / 2), font.size),
        font,
        Color::rgb(255, 255, 255),
        (height / 4) as i32,
        0,
        width.saturating_sub(height / 2),
        height,
        TextAlign::Center,
    );
    DecorationBuffer {
        width,
        height,
        pixels: pixmap.take(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_windows_keep_shape_size_and_wallpaper_space() {
        for scale in [1, 2] {
            for n in [0, 1, 2, 7, 32, 128] {
                let sizes: Vec<_> = (0..n)
                    .map(|i| match i % 3 {
                        0 => Size::new(900 * scale, 500 * scale),
                        1 => Size::new(300 * scale, 700 * scale),
                        _ => Size::new(200 * scale, 100 * scale),
                    })
                    .collect();
                let l = layout(Size::new(1920 * scale, 1080 * scale), 56 * scale, &sizes, 3);
                assert_eq!(l.cells.len(), n);
                for (i, (cell, size)) in l.cells.iter().zip(&sizes).enumerate() {
                    assert!(cell.size.w <= size.w && cell.size.h <= size.h);
                    let ratio = cell.size.w as f64 / size.w as f64;
                    assert!((cell.size.h as f64 - size.h as f64 * ratio).abs() < 4.0);
                    assert!(cell.pos.y >= l.grid.pos.y);
                    assert!(cell.pos.x + cell.size.w as i32 <= (1920 * scale) as i32);
                    assert!(cell.pos.y + cell.size.h as i32 <= (1080 * scale) as i32);
                    assert_eq!(
                        l.cell_at(Point::new(
                            cell.pos.x + cell.size.w as i32 / 2,
                            cell.pos.y + cell.size.h as i32 / 2
                        )),
                        Some(i)
                    );
                    for other in &l.cells[i + 1..] {
                        assert!(
                            cell.pos.x + cell.size.w as i32 <= other.pos.x
                                || other.pos.x + other.size.w as i32 <= cell.pos.x
                                || cell.pos.y + cell.size.h as i32 <= other.pos.y
                                || other.pos.y + other.size.h as i32 <= cell.pos.y
                        );
                    }
                }
                assert!(l
                    .strip
                    .iter()
                    .all(|s| s.pos.y < l.grid.pos.y && s.size.w > s.size.h));
            }
        }
    }
}
