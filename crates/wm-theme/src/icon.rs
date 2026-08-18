//! Rendering for small square "icon tiles" — the shape a miniaturized
//! window collapses to (classic WindowMaker/NeXTSTEP "miniaturize to
//! icon", not minimize-to-a-taskbar). A miniature titlebar strip (the
//! window's own inactive fill, a small title) over a preview of the
//! window's actual content, captured the instant it was miniaturized —
//! see `wm_core::Backend::capture_window_image`. Falls back to a plain
//! dark tile when no preview is available (capture failed, or this is
//! used for something that isn't a captured window at all — a themed
//! launcher/shelf icon, say).

use tiny_skia::{FilterQuality, PixmapPaint, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::{Color, FontSpec, TextAlign, Theme};
use crate::paint;

/// Rasterizes one icon tile: `size` x `size`. `preview`, if present, is
/// scaled to fit (letterboxed, never cropped) under the title strip.
pub fn render_icon_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    label: &str,
    preview: Option<&DecorationBuffer>,
) -> DecorationBuffer {
    let size = size.max(1);
    let Some(mut pixmap) = tiny_skia::Pixmap::new(size, size) else {
        return DecorationBuffer { width: 0, height: 0, pixels: Vec::new() };
    };

    paint::fill_area(&mut pixmap, 0, 0, size, size, &theme.resize_bar.fill);

    // A miniature version of the window's own (inactive — nothing
    // miniaturized is ever the focused window) titlebar, not a generic
    // icon-tile color: this is meant to still read as "that window,"
    // shrunk, not as a different kind of object.
    let title_h = ((size as f32) * 0.24).round().max(9.0) as u32;
    paint::fill_area(&mut pixmap, 0, 0, size, title_h, &theme.titlebar.inactive);

    let text = if label.is_empty() { "?" } else { label };
    let short: String = text.chars().take(24).collect();
    // Deliberately smaller than a real titlebar's own font — this is a
    // caption on a thumbnail, not a titlebar someone reads from across
    // the room.
    let title_font = FontSpec { size: (title_h as f32 * 0.56).max(6.0), ..theme.titlebar.font.clone() };
    let text_pad = 3i32;
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &short,
        &title_font,
        theme.titlebar.text_color_inactive,
        text_pad,
        0,
        size.saturating_sub(text_pad as u32 * 2),
        title_h,
        TextAlign::Center,
    );

    let preview_y = title_h;
    let preview_h = size.saturating_sub(title_h);
    paint::fill_rect(&mut pixmap, 0, preview_y as i32, size, preview_h, Color::rgb(0x0c, 0x0c, 0x0c));

    if let Some(src) = preview.filter(|b| b.width > 0 && b.height > 0) {
        draw_preview(&mut pixmap, src, 0, preview_y, size, preview_h);
    }

    paint::draw_bevel(&mut pixmap, 0, 0, size, size, &theme.titlebar.bevel);

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// Scales `src` to fit entirely within `(x, y, w, h)` — never cropped,
/// centered, letterboxed on whichever axis doesn't fill exactly — and
/// blits it in. A captured window is essentially never the same aspect
/// ratio as a square icon tile, and cropping would hide real content
/// (often the most identifying part, like a browser's tab bar) to gain
/// nothing but a filled corner.
fn draw_preview(dest: &mut tiny_skia::Pixmap, src: &DecorationBuffer, x: u32, y: u32, w: u32, h: u32) {
    if w == 0 || h == 0 || src.width == 0 || src.height == 0 {
        return;
    }
    let Some(size) = tiny_skia::IntSize::from_wh(src.width, src.height) else { return };
    let Some(src_pixmap) = tiny_skia::Pixmap::from_vec(src.pixels.clone(), size) else { return };

    let scale = (w as f32 / src.width as f32).min(h as f32 / src.height as f32);
    let dst_w = src.width as f32 * scale;
    let dst_h = src.height as f32 * scale;
    let dx = x as f32 + (w as f32 - dst_w) / 2.0;
    let dy = y as f32 + (h as f32 - dst_h) / 2.0;

    let paint = PixmapPaint { quality: FilterQuality::Bilinear, ..Default::default() };
    dest.draw_pixmap(0, 0, src_pixmap.as_ref(), &paint, Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    fn solid_preview(width: u32, height: u32, color: (u8, u8, u8)) -> DecorationBuffer {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            pixels.extend_from_slice(&[color.0, color.1, color.2, 0xFF]);
        }
        DecorationBuffer { width, height, pixels }
    }

    #[test]
    fn render_icon_tile_produces_a_correctly_sized_buffer() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();

        let buffer = render_icon_tile(&theme, &mut font_system, &mut swash_cache, 56, "xterm", None);

        assert_eq!(buffer.width, 56);
        assert_eq!(buffer.height, 56);
        assert_eq!(buffer.pixels.len(), 56 * 56 * 4);
    }

    #[test]
    fn render_icon_tile_handles_an_empty_label() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();

        let buffer = render_icon_tile(&theme, &mut font_system, &mut swash_cache, 40, "", None);

        assert_eq!(buffer.pixels.len(), 40 * 40 * 4);
    }

    #[test]
    fn a_preview_visibly_changes_the_tile_versus_no_preview() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();

        let without = render_icon_tile(&theme, &mut font_system, &mut swash_cache, 56, "xterm", None);
        let preview = solid_preview(800, 600, (0xe0, 0x40, 0x40));
        let with = render_icon_tile(&theme, &mut font_system, &mut swash_cache, 56, "xterm", Some(&preview));

        assert_ne!(without.pixels, with.pixels, "a real preview image should visibly change the tile");
    }

    /// Regression guard: a preview with a very different aspect ratio
    /// than the square tile must still fit entirely within the preview
    /// area (letterboxed), not overflow past its top edge into the
    /// title strip or past the tile's own bottom edge.
    #[test]
    fn wide_preview_stays_within_the_preview_area_not_the_title_strip() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        let size = 112u32;

        let preview = solid_preview(2000, 40, (0x40, 0xe0, 0x40)); // very wide, short
        let buffer = render_icon_tile(&theme, &mut font_system, &mut swash_cache, size, "wide", Some(&preview));
        let mut pixmap = tiny_skia::Pixmap::new(buffer.width, buffer.height).unwrap();
        pixmap.data_mut().copy_from_slice(&buffer.pixels);

        let title_h = ((size as f32) * 0.24).round() as u32;
        for y in 0..title_h.saturating_sub(1) {
            for x in 0..size {
                let p = pixmap.pixels()[(y * size + x) as usize];
                let is_green = p.green() > 180 && p.red() < 120 && p.blue() < 120;
                assert!(!is_green, "preview pixel bled into the title strip at ({x}, {y})");
            }
        }
    }
}
