//! The tile: this theme system's common UI platform, extracted from
//! the workspace Clip once it became clear its look — a diagonal
//! gradient face under the double raised relief, with luminance-picked
//! ink — was the style everything square should share. Every dock
//! item, miniaturized-window icon, and the Clip itself renders on one
//! of these, and a third-party `chonk-ui` app can too: this module
//! plus [`crate::paint`] is the SDK surface for building tiles that
//! belong on this desktop.
//!
//! The face comes from [`crate::model::TileStyle`] (per-theme; the
//! flagship uses the classic stock icon-background gradient), the
//! relief is the same relative raised recipe as window chrome, and
//! recessed content areas (an LCD readout, a window preview) sit in a
//! [`draw_tile_well`] — shaded and sunken, the classic instrument-
//! panel inset.

use tiny_skia::Pixmap;

use crate::model::{Color, Fill, Theme};
use crate::paint;

/// Paints a tile face (fill + relief) over `size` x `size` at
/// `(x, y)`. The base every tile-shaped surface starts from.
pub fn draw_tile_base(pixmap: &mut Pixmap, x: i32, y: i32, size: u32, theme: &Theme) {
    paint::fill_area(pixmap, x, y, size, size, &theme.tile.fill);
    let t = theme.tile.bevel.width.max(1) as u32;
    paint::draw_raised2_bevel(pixmap, x, y, size, size, t);
}

/// Convenience: a standalone tile-face pixmap of `size`.
pub fn render_tile_base(theme: &Theme, size: u32) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(size.max(1), size.max(1))?;
    draw_tile_base(&mut pixmap, 0, 0, size.max(1), theme);
    Some(pixmap)
}

/// A recessed content area within a tile: the region is shaded down
/// and given the sunken relief, so whatever is drawn inside (an LCD
/// panel, a live window preview, a graph) reads as set into the tile
/// rather than stickered onto it.
pub fn draw_tile_well(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, theme: &Theme) {
    let t = theme.tile.bevel.width.max(1) as u32;
    paint::op_rect(pixmap, x, y, w, h, -24);
    paint::draw_sunken_bevel(pixmap, x, y, w, h, t);
}

/// Ink that stays legible on this theme's tile face — picked from the
/// face's average luminance (gradients average their endpoints), same
/// reasoning as `paint::pressed_delta`. One ink for every tile keeps
/// the family consistent; widgets needing emphasis can still reach
/// for theme accent colors deliberately.
pub fn tile_ink(theme: &Theme) -> Color {
    let c = match &theme.tile.fill {
        Fill::Solid(c) => *c,
        Fill::Gradient(g) => Color::rgb(
            ((g.from.r as u16 + g.to.r as u16) / 2) as u8,
            ((g.from.g as u16 + g.to.g as u16) / 2) as u8,
            ((g.from.b as u16 + g.to.b as u16) / 2) as u8,
        ),
    };
    let luminance = (c.r as u16 + c.g as u16 + c.b as u16) / 3;
    if luminance < 128 {
        Color::rgb(0xE8, 0xE8, 0xE8)
    } else {
        Color::rgb(0x10, 0x10, 0x10)
    }
}

/// A secondary, receding ink — for sublabels and inactive marks —
/// derived from [`tile_ink`] by pulling it toward the face.
pub fn tile_ink_dim(theme: &Theme) -> Color {
    let ink = tile_ink(theme);
    if ink.r > 0x80 {
        Color::rgb(0xA0, 0xA0, 0xA0)
    } else {
        Color::rgb(0x50, 0x50, 0x50)
    }
}

/// Clamped add/subtract along a line — the diagonal sibling of
/// `paint::op_rect` (which only does rects). Integer line walk on
/// purpose: tile details are hard-edged.
pub fn op_line(pixmap: &mut Pixmap, x0: i32, y0: i32, x1: i32, y1: i32, delta: i16) {
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
    let pixels = pixmap.pixels_mut();
    for i in 0..=steps {
        let x = x0 + ((x1 - x0) * i) / steps;
        let y = y0 + ((y1 - y0) * i) / steps;
        if x < 0 || y < 0 || x >= w || y >= h {
            continue;
        }
        let idx = (y * w + x) as usize;
        let e = pixels[idx];
        let op = |c: u8| (c as i16 + delta).clamp(0, 255) as u8;
        if let Some(p) = tiny_skia::PremultipliedColorU8::from_rgba(op(e.red()), op(e.green()), op(e.blue()), 255) {
            pixels[idx] = p;
        }
    }
}

/// Hard 1px line in an absolute color — [`op_line`]'s counterpart for
/// details that must not take their tone from what is underneath.
pub fn draw_line(pixmap: &mut Pixmap, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    for i in 0..=steps {
        let x = x0 + ((x1 - x0) * i) / steps;
        let y = y0 + ((y1 - y0) * i) / steps;
        paint::fill_rect(pixmap, x, y, 1, 1, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_base_is_a_gradient_with_relief_not_flat() {
        let theme = crate::default_theme::nextstep_classic();
        let pixmap = render_tile_base(&theme, 64).unwrap();
        let px = |x: u32, y: u32| {
            let p = pixmap.pixels()[(y * 64 + x) as usize];
            (p.red(), p.green(), p.blue())
        };
        assert_ne!(px(8, 8), px(56, 56), "diagonal gradient: corners must differ");
        assert_ne!(px(32, 0), px(32, 4), "top relief line must differ from the face");
    }

    #[test]
    fn every_builtin_theme_ink_contrasts_with_its_tile() {
        for theme in crate::default_theme::all_themes() {
            let ink = tile_ink(&theme);
            let ink_l = (ink.r as u16 + ink.g as u16 + ink.b as u16) / 3;
            let face_l = match &theme.tile.fill {
                Fill::Solid(c) => (c.r as u16 + c.g as u16 + c.b as u16) / 3,
                Fill::Gradient(g) => {
                    ((g.from.r as u16 + g.from.g as u16 + g.from.b as u16) / 3
                        + (g.to.r as u16 + g.to.g as u16 + g.to.b as u16) / 3)
                        / 2
                }
            };
            let contrast = (ink_l as i32 - face_l as i32).abs();
            assert!(contrast > 60, "theme {} ink/face contrast too low: {contrast}", theme.id);
        }
    }

    #[test]
    fn tile_well_recesses_the_region() {
        let theme = crate::default_theme::nextstep_classic();
        let mut pixmap = render_tile_base(&theme, 64).unwrap();
        let before = pixmap.pixels()[(32 * 64 + 32) as usize].red();
        draw_tile_well(&mut pixmap, 16, 16, 32, 32, &theme);
        let after = pixmap.pixels()[(32 * 64 + 32) as usize].red();
        assert!(after < before, "well interior should be shaded down");
    }
}
