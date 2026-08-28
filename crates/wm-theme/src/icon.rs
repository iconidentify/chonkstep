//! Rendering for small square "icon tiles" — the shape a miniaturized
//! window collapses to (the classic NeXTSTEP "miniaturize to icon",
//! not minimize-to-a-taskbar), also reused by the Alt-Tab
//! switcher for its thumbnails. Built on the theme's common tile
//! ([`tile::draw_tile_base`]): a preview of the window's actual
//! content, captured the instant it was miniaturized (see
//! `wm_core::Backend::capture_window_image`), sits letterboxed inside
//! a recessed [`tile::draw_tile_well`] covering most of the face —
//! a little framed viewport onto the window — with the title as a
//! small ink caption on the face beneath it. When no preview exists
//! (capture failed, or this is used for something that isn't a
//! captured window at all — a themed launcher/shelf icon, say) the
//! empty well plus caption still reads as a finished tile, not a
//! broken one.

use tiny_skia::{FilterQuality, PixmapPaint, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::{FontSpec, TextAlign, Theme};
use crate::{paint, tile};

/// Rasterizes one icon tile: `size` x `size`. `preview`, if present, is
/// scaled to fit (letterboxed, never cropped) inside the well.
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

    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    // Geometry: the preview well fills the tile down to a caption
    // strip along the bottom. `margin` keeps the well's sunken bevel
    // clear of the tile's own raised relief (the two recipes read as
    // mush when they touch) and scales with the tile like all chrome.
    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let caption_h = ((size as f32) * 0.22).round().max(9.0) as i32;
    let caption_y = size as i32 - margin - caption_h;
    let well_x = margin;
    let well_y = margin;
    let well_w = (size as i32 - margin * 2).max(0) as u32;
    let well_h = (caption_y - margin).max(0) as u32;
    tile::draw_tile_well(&mut pixmap, well_x, well_y, well_w, well_h, theme);

    if let Some(src) = preview.filter(|b| b.width > 0 && b.height > 0) {
        // The content stays inside the well's sunken bevel so the
        // recess lines keep framing it; letterbox bars show the well's
        // shaded floor, which reads far better than the old hard-black
        // backing — the tile stays one material throughout.
        let inner = t;
        let px = well_x + inner;
        let py = well_y + inner;
        let pw = (well_w as i32 - inner * 2).max(0) as u32;
        let ph = (well_h as i32 - inner * 2).max(0) as u32;
        draw_preview(&mut pixmap, src, px as u32, py as u32, pw, ph);
    }

    // The caption: the window's title in the tile's own ink, beneath
    // the well. Deliberately small — this is a caption on a thumbnail,
    // not a titlebar someone reads from across the room — and hard-
    // elided the same way the old strip was.
    let text = if label.is_empty() { "?" } else { label };
    let short: String = text.chars().take(24).collect();
    let caption_font = FontSpec { size: (caption_h as f32 * 0.60).max(6.0), ..theme.titlebar.font.clone() };
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &short,
        &caption_font,
        tile::tile_ink(theme),
        well_x,
        caption_y,
        well_w,
        caption_h.max(0) as u32,
        TextAlign::Center,
    );

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
    /// than the square tile must still fit entirely within the well
    /// interior (letterboxed), never spilling onto the tile face above
    /// the well or into the caption strip below it. Geometry here
    /// mirrors `render_icon_tile`'s, with one row/column of slack for
    /// edge filtering, matching the tolerance the old strip test gave.
    #[test]
    fn wide_preview_stays_within_the_well_not_the_face_or_caption() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        let size = 112u32;

        let preview = solid_preview(2000, 40, (0x40, 0xe0, 0x40)); // very wide, short
        let buffer = render_icon_tile(&theme, &mut font_system, &mut swash_cache, size, "wide", Some(&preview));
        let mut pixmap = tiny_skia::Pixmap::new(buffer.width, buffer.height).unwrap();
        pixmap.data_mut().copy_from_slice(&buffer.pixels);

        let t = theme.tile.bevel.width.max(1) as i32;
        let margin = t + (size as i32 / 28).max(1);
        let caption_h = ((size as f32) * 0.22).round().max(9.0) as i32;
        let caption_y = size as i32 - margin - caption_h;
        let interior_top = (margin + t) as u32;
        let interior_bottom = (caption_y - t) as u32;
        let interior_left = (margin + t) as u32;
        let interior_right = size - interior_left;
        for y in 0..size {
            for x in 0..size {
                let inside = x + 1 >= interior_left
                    && x < interior_right + 1
                    && y + 1 >= interior_top
                    && y < interior_bottom + 1;
                if inside {
                    continue;
                }
                let p = pixmap.pixels()[(y * size + x) as usize];
                let is_green = p.green() > 180 && p.red() < 120 && p.blue() < 120;
                assert!(!is_green, "preview pixel bled outside the well interior at ({x}, {y})");
            }
        }
    }
}
