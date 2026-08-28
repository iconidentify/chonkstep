//! The workspace Clip: the classic desktop's top-left corner tile,
//! reproduced recipe-for-recipe and scaled. This widget's look —
//! diagonal gradient face, double raised relief, luminance-picked ink —
//! is where the shared [`crate::tile`] platform came from, and the Clip
//! now renders on that platform instead of keeping its own copy of the
//! recipe. The tile's two "clipped" corners are diagonal crease lines
//! (a hard black cut with dark/light shading on either side, the exact
//! stock line triplet) drawn over the tile base, each corner carrying a
//! small right-angle arrow: top-right advances a workspace, bottom-left
//! goes back. The current workspace number sits large in the middle
//! with a `Desk N` label beneath, matching how the stock Clip presents
//! the workspace name.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::{Color, TextAlign, Theme};
use crate::paint;
use crate::tile;

/// Fraction math from the stock constants: the corner button is 23 on a
/// 64px tile, and the arrow edge is that minus 15. Both are scaled off
/// the actual tile size so the Clip keeps its stock proportions at any
/// `CHONKSTEP_SCALE`.
fn clip_metrics(size: u32) -> (i32, i32, i32) {
    let s = size as i32;
    let pt = (23 * s) / 64;
    let tp = s - 1 - pt;
    let arrow = ((pt - (15 * s) / 64).max(3)) as i32;
    (pt, tp, arrow)
}

/// Which Clip zone a tile-local point falls in — the classic diagonal
/// corner test, verbatim but scaled: the top-right triangle advances,
/// the bottom-left one rewinds, the rest of the tile is inert (the
/// stock Clip's body is for dragging and menus, not switching).
pub fn clip_hit(size: u32, x: i32, y: i32) -> ClipZone {
    let s = size as i32;
    if x < 0 || y < 0 || x >= s || y >= s {
        return ClipZone::Body;
    }
    let pt = ((23 * s) / 64) + (2 * s) / 64;
    if y <= pt - (s - 1 - x) {
        ClipZone::Forward
    } else if x <= pt - (s - 1 - y) {
        ClipZone::Rewind
    } else {
        ClipZone::Body
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipZone {
    Forward,
    Rewind,
    Body,
}

/// Renders the Clip tile. `current` is 0-based; the number drawn is
/// 1-based, the way workspaces are classically named.
pub fn render_clip_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    current: usize,
    count: usize,
) -> DecorationBuffer {
    let size = size.max(16);
    let Some(mut pixmap) = Pixmap::new(size, size) else {
        return DecorationBuffer { width: 0, height: 0, pixels: Vec::new() };
    };

    // The tile base is the platform this widget inspired: drawing
    // through it (rather than a private fill + bevel copy) means a
    // theme's tile style restyles the Clip and every other dock tile
    // together, and the ink below is guaranteed to match its siblings.
    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    let (pt, tp, arrow) = clip_metrics(size);
    let s = size as i32;
    let t = ((size as f32) / 64.0).round().max(1.0) as i32;

    // The clipped corners: the exact stock line triplets — shade
    // below the cut, hard black cut, light above — repeated `t` thick
    // so the crease scales like every other piece of chrome. Drawn
    // straight over the tile base, exactly like the stock Clip carves
    // its corners out of the finished icon tile.
    let ink = tile::tile_ink(theme);
    for i in 0..t {
        // Top-right crease.
        tile::op_line(&mut pixmap, tp + i, 0, s - 2 + i, pt - 1, -60);
        tile::draw_line(&mut pixmap, tp - 1 + i, 0, s - 1 + i, pt + 1, Color::rgb(0, 0, 0));
        tile::op_line(&mut pixmap, tp + 1 + i, 2, s - 3 + i, pt, 80);
        // Bottom-left crease (mirrored).
        tile::op_line(&mut pixmap, 2, tp + 2 + i, pt - 2, s - 3 + i, -60);
        tile::draw_line(&mut pixmap, 0, tp - 1 + i, pt + 1, s - 1 + i, Color::rgb(0, 0, 0));
        tile::op_line(&mut pixmap, 0, tp - 2 + i, pt + 1, s - 2 + i, 80);
    }

    // Corner arrows: right-angle triangles hugging each clipped
    // corner, in the tile's ink color.
    let m5 = (5 * s) / 64;
    let m6 = (6 * s) / 64;
    fill_triangle(
        &mut pixmap,
        [(s - m5 - arrow, m5), (s - m6, m5), (s - m6, m5 - 1 + arrow)],
        ink,
    );
    fill_triangle(
        &mut pixmap,
        [(m5, s - m5 - arrow), (m5, s - m6), (m5 - 1 + arrow, s - m6)],
        ink,
    );

    // The workspace number, large and centered — then the Desk label
    // beneath, like the stock Clip's workspace name strip.
    // Type sizes are tuned against the clipped corners, not just the
    // tile: at the earlier 0.40/0.16 ratios the label ran nearly the
    // full tile width and its first glyph sat on top of the rewind
    // arrow (confirmed at 400 percent zoom). 0.30/0.115 keeps the
    // number readable at a glance while the label clears both crease
    // lines with margin (0.115 still grazed the bottom-left crease;
    // 0.10 clears it); the label floor keeps it legible at
    // CHONKSTEP_SCALE 1's 56px tile.
    let mut number_font = theme.titlebar.font.clone();
    number_font.size = (size as f32) * 0.30;
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &format!("{}", current + 1),
        &number_font,
        ink,
        0,
        (size as f32 * 0.17) as i32,
        size,
        (size as f32 * 0.46) as u32,
        TextAlign::Center,
    );
    let mut label_font = theme.menu.item_font.clone();
    label_font.size = ((size as f32) * 0.10).max(8.0);
    // Long labels drop the "Desk" word rather than growing back into
    // the crease: the guard estimates the rendered width from an
    // average glyph advance, erring toward the shorter form.
    let full = format!("Desk {} / {}", current + 1, count.max(1));
    let label = if (full.chars().count() as f32) * label_font.size * 0.55 > (size as f32) * 0.68 {
        format!("{} / {}", current + 1, count.max(1))
    } else {
        full
    };
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &label,
        &label_font,
        ink,
        0,
        (size as f32 * 0.65) as i32,
        size,
        (size as f32 * 0.24) as u32,
        TextAlign::Center,
    );

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

fn fill_triangle(pixmap: &mut Pixmap, points: [(i32, i32); 3], color: Color) {
    let mut p = Paint::default();
    p.set_color(paint::sk_color(color));
    p.anti_alias = false;
    let mut pb = PathBuilder::new();
    pb.move_to(points[0].0 as f32, points[0].1 as f32);
    pb.line_to(points[1].0 as f32, points[1].1 as f32);
    pb.line_to(points[2].0 as f32, points[2].1 as f32);
    pb.close();
    if let Some(path) = pb.finish() {
        pixmap.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(current: usize, count: usize, size: u32) -> DecorationBuffer {
        let theme = crate::default_theme::nextstep_classic();
        let mut fs = cosmic_text::FontSystem::new();
        let mut sc = cosmic_text::SwashCache::new();
        render_clip_tile(&theme, &mut fs, &mut sc, size, current, count)
    }

    #[test]
    fn clip_tile_renders_at_the_requested_size_and_is_not_flat() {
        for size in [56u32, 64, 112] {
            let tile = render(0, 2, size);
            assert_eq!((tile.width, tile.height), (size, size));
            let first = &tile.pixels[0..4];
            assert!(tile.pixels.chunks_exact(4).any(|px| px != first), "size {size} should not be flat");
        }
    }

    #[test]
    fn changing_the_workspace_changes_the_pixels() {
        assert_ne!(render(0, 3, 64).pixels, render(1, 3, 64).pixels);
    }

    /// The classic diagonal corner zones: the extreme corners resolve
    /// to the arrows, the middle of the tile to the body.
    #[test]
    fn hit_zones_match_the_classic_corner_geometry() {
        assert_eq!(clip_hit(64, 62, 2), ClipZone::Forward);
        assert_eq!(clip_hit(64, 2, 62), ClipZone::Rewind);
        assert_eq!(clip_hit(64, 32, 32), ClipZone::Body);
        assert_eq!(clip_hit(64, -1, 5), ClipZone::Body);
        // Scaled tile keeps the same proportional zones.
        assert_eq!(clip_hit(112, 108, 4), ClipZone::Forward);
        assert_eq!(clip_hit(112, 4, 108), ClipZone::Rewind);
    }
}
