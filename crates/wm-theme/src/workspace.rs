//! The dock's workspace-indicator tile, in the spirit of WindowMaker's
//! Clip: a square tile whose face is dominated by the current workspace
//! number (1-based — nobody thinks of their first workspace as "0"),
//! with a small position row underneath showing where that workspace
//! sits among the full set. Unlike the Clip this doesn't collect app
//! icons — it exists to answer "which workspace am I on?" at a glance
//! and to give the dock a click target for cycling — but it borrows the
//! Clip's visual role: the one dock tile whose face is *about*
//! workspaces. Drawn in the theme's own language (the resize-bar fill,
//! the titlebar font, the RAISED2 relief every other tile carries)
//! rather than inventing indicator-specific chrome.

use tiny_skia::Pixmap;
use wm_theme_api::DecorationBuffer;

use crate::model::{Color, Fill, FontSpec, TextAlign, Theme};
use crate::paint;

/// Rasterizes one workspace tile: `size` x `size`, showing `current`
/// (0-based, drawn 1-based) among `count` workspaces. Out-of-range
/// input is clamped rather than trusted — the desktop shell hands over
/// whatever the WM last reported, and a momentarily stale pair (a
/// workspace was just removed, say) should render *something* sane, not
/// panic the dock.
pub fn render_workspace_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    current: usize,
    count: usize,
) -> DecorationBuffer {
    let size = size.max(8);
    let count = count.max(1);
    let current = current.min(count - 1);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero workspace tile size");

    // Same frame treatment as the other dock tiles (see
    // `clock::render_clock_tile`): the resize-bar fill as the face,
    // since dockapp tiles don't get their own style in this milestone.
    paint::fill_area(&mut pixmap, 0, 0, size, size, &theme.resize_bar.fill);

    let (ink, ghost) = ink_colors(&theme.resize_bar.fill);
    let t = theme.titlebar.bevel.width.max(1) as u32;

    // The workspace number, large and centered — the titlebar font
    // scaled up to tile proportions rather than a new face, so the tile
    // reads as this theme's chrome. The band leaves the lower ~third of
    // the tile for the position row.
    let number_band_h = ((size as f32) * 0.66).round() as u32;
    let number_font = FontSpec { size: (size as f32 * 0.42).max(8.0), ..theme.titlebar.font.clone() };
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &(current + 1).to_string(),
        &number_font,
        ink,
        0,
        t as i32,
        size,
        number_band_h,
        TextAlign::Center,
    );

    // Position row: one dot per workspace when they fit (the current
    // one in full ink, the rest ghosted into the fill), falling back to
    // a compact "N / M" readout once the count outgrows the tile —
    // fifteen dots crammed into a 56px tile would read as noise, not
    // position.
    let row_y = number_band_h as i32;
    let row_h = size.saturating_sub(number_band_h).saturating_sub(t * 2);
    let dot = (size / 16).max(2);
    let gap = (dot / 2).max(1);
    let dots_w = count as u32 * dot + (count as u32 - 1) * gap;
    let avail = size.saturating_sub((t + 2) * 2);
    if dots_w <= avail {
        // Hard-edged squares, not circles — matching the rest of the
        // theme's non-anti-aliased pixel language.
        let mut x = (size as i32 - dots_w as i32) / 2;
        let y = row_y + (row_h as i32 - dot as i32) / 2;
        for index in 0..count {
            let color = if index == current { ink } else { ghost };
            paint::fill_rect(&mut pixmap, x, y, dot, dot, color);
            x += (dot + gap) as i32;
        }
    } else {
        let row_font = FontSpec { size: (size as f32 * 0.16).max(6.0), ..theme.titlebar.font.clone() };
        paint::draw_text(
            &mut pixmap,
            font_system,
            swash_cache,
            &format!("{} / {}", current + 1, count),
            &row_font,
            ink,
            0,
            row_y,
            size,
            row_h,
            TextAlign::Center,
        );
    }

    paint::draw_raised2_bevel(&mut pixmap, 0, 0, size, size, t);

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// The number/dot ink for a given tile fill, plus its ghosted variant
/// for the not-current dots: light ink on a dark fill, dark ink on a
/// light one — the same relative-to-the-fill reasoning as
/// `paint::pressed_delta`, since themes are free to make the resize bar
/// (and therefore this tile's face) any brightness they like. The ghost
/// is the ink mixed most-of-the-way back into the fill, so empty dots
/// read as marks on the same surface rather than a second accent color.
fn ink_colors(fill: &Fill) -> (Color, Color) {
    let base = match fill {
        Fill::Solid(c) => *c,
        Fill::Gradient(g) => Color::rgb(
            ((g.from.r as u16 + g.to.r as u16) / 2) as u8,
            ((g.from.g as u16 + g.to.g as u16) / 2) as u8,
            ((g.from.b as u16 + g.to.b as u16) / 2) as u8,
        ),
    };
    let luminance = (base.r as u16 + base.g as u16 + base.b as u16) / 3;
    let ink = if luminance < 128 { Color::rgb(0xF0, 0xF0, 0xF0) } else { Color::rgb(0x10, 0x10, 0x10) };
    let mix = |i: u8, b: u8| ((i as u16 * 2 + b as u16 * 3) / 5) as u8;
    let ghost = Color::rgb(mix(ink.r, base.r), mix(ink.g, base.g), mix(ink.b, base.b));
    (ink, ghost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    fn render(size: u32, current: usize, count: usize) -> DecorationBuffer {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        render_workspace_tile(&theme, &mut font_system, &mut swash_cache, size, current, count)
    }

    #[test]
    fn render_workspace_tile_produces_correctly_sized_buffers() {
        for size in [16u32, 56, 64] {
            let buffer = render(size, 0, 4);
            assert_eq!(buffer.width, size);
            assert_eq!(buffer.height, size);
            assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
        }
    }

    /// A flat tile (every pixel the fill color) would mean the number,
    /// position row, and relief all silently failed to draw — checked
    /// for both the trivial single-workspace case and a mid-set one,
    /// since they take different position-row branches at some sizes.
    #[test]
    fn first_of_one_and_fourth_of_nine_both_render_non_flat_tiles() {
        for (current, count) in [(0usize, 1usize), (3, 9)] {
            let buffer = render(56, current, count);
            let first = &buffer.pixels[0..4];
            assert!(
                buffer.pixels.chunks_exact(4).any(|px| px != first),
                "workspace {current} of {count} rendered a flat tile"
            );
        }
    }

    #[test]
    fn the_tile_changes_when_the_current_workspace_changes() {
        let on_first = render(56, 0, 9);
        let on_fourth = render(56, 3, 9);
        assert_ne!(on_first.pixels, on_fourth.pixels, "switching workspaces should visibly change the tile");
    }
}
