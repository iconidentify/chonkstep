//! An exact-as-practical port of the classic WindowMaker dockapp
//! `wmnetload`: a monochrome LCD-style panel — sage-gray "glass," a
//! silver chiseled bevel, a three-digit seven-segment throughput
//! readout with K/M/G unit indicators, the interface name, and a
//! mirrored dot-matrix history graph (download filling down from the
//! top edge, upload filling up from the bottom edge — "without
//! resorting to colors," exactly as the original's own description
//! puts it: direction is shown by *position*, not hue). Colors here are
//! deliberately fixed rather than theme-derived — this widget recreates
//! a specific piece of hardware-LCD chrome, not this desktop's own
//! NeXTSTEP palette.

use tiny_skia::{Paint, Pixmap, Rect as SkRect, Transform};
use wm_theme_api::DecorationBuffer;

use crate::digitalclock::{segment_rects, DIGIT_SEGMENTS};
use crate::model::{Bevel, BevelStyle, Color, FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::Theme;

/// Which unit the three-digit readout is currently expressed in — the
/// original lights up one of a fixed `K`/`M`/`G` column of labels rather
/// than drawing a unit suffix next to the number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetloadUnit {
    Kilo,
    Mega,
    Giga,
}

/// How many history columns the dot-matrix graph shows.
pub const NET_LOAD_COLUMNS: usize = 16;
/// How many dot-rows each direction (rx above center, tx below) gets —
/// the matrix is `NET_LOAD_HALF_ROWS * 2` rows tall in total.
pub const NET_LOAD_HALF_ROWS: u32 = 5;

const PANEL: Color = Color { r: 0x68, g: 0x74, b: 0x68, a: 0xff };
const PANEL_SHADOW: Color = Color { r: 0x58, g: 0x64, b: 0x58, a: 0xff };
const GHOST: Color = Color { r: 0x58, g: 0x64, b: 0x58, a: 0xff };
const INK: Color = Color { r: 0x10, g: 0x10, b: 0x10, a: 0xff };
const BEZEL_LIGHT: Color = Color { r: 0xe8, g: 0xf0, b: 0xf8, a: 0xff };
const BEZEL_DARK: Color = Color { r: 0x10, g: 0x10, b: 0x10, a: 0xff };

/// `rate_digits` is the three-digit readout — `None` for a leading-zero
/// blanked position (the original left-pads with blanks, not `0`s).
/// `rx_levels`/`tx_levels` are per-column dot-rows-lit, already
/// quantized to `0..=NET_LOAD_HALF_ROWS` by the caller, oldest first.
#[allow(clippy::too_many_arguments)]
pub fn render_netload_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    interface_name: &str,
    rate_digits: [Option<u8>; 3],
    unit: NetloadUnit,
    rx_levels: &[u32],
    tx_levels: &[u32],
) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero netload tile size");
    // Deliberately theme-independent for color/bevel treatment — see the
    // module doc comment — but text still needs a real, registered font
    // family for `cosmic-text` to resolve, so this borrows the theme's
    // own (confirmed-available) one rather than guessing a generic name
    // like "sans-serif", which isn't a real face and silently renders
    // nothing.
    let label_family = theme.menu.item_font.family.clone();

    let bevel = Bevel { style: BevelStyle::Raised, width: (size as f32 * 0.05).max(2.0) as u8, light: BEZEL_LIGHT, dark: BEZEL_DARK };
    paint::fill_rect(&mut pixmap, 0, 0, size, size, PANEL);
    paint::draw_bevel(&mut pixmap, 0, 0, size, size, &bevel);

    let inset = (bevel.width as f32 + 1.0) as i32;
    let face_w = (size as i32 - inset * 2).max(1) as u32;
    let face_h = (size as i32 - inset * 2).max(1) as u32;
    paint::fill_rect(&mut pixmap, inset, inset, face_w, face_h, PANEL);

    let digit_row_h = (face_h as f32 * 0.36).round() as u32;
    let name_row_h = (face_h as f32 * 0.16).round() as u32;
    let matrix_h = face_h.saturating_sub(digit_row_h).saturating_sub(name_row_h);

    draw_digit_row(&mut pixmap, inset, inset, face_w, digit_row_h, font_system, swash_cache, &label_family, rate_digits, unit);

    let name_y = inset + digit_row_h as i32;
    draw_name_row(&mut pixmap, inset, name_y, face_w, name_row_h, font_system, swash_cache, &label_family, interface_name);

    let matrix_y = name_y + name_row_h as i32;
    draw_dot_matrix(&mut pixmap, inset, matrix_y, face_w, matrix_h, rx_levels, tx_levels);

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

#[allow(clippy::too_many_arguments)]
fn draw_digit_row(
    pixmap: &mut Pixmap,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    label_family: &str,
    digits: [Option<u8>; 3],
    unit: NetloadUnit,
) {
    let unit_col_w = (w as f32 * 0.18).round().max(10.0) as u32;
    let digits_w = w.saturating_sub(unit_col_w);
    let digit_margin = (digits_w as f32 * 0.03).max(1.0);
    let digit_w = (digits_w as f32 / 3.0 - digit_margin).max(1.0);
    let digit_h = h as f32 * 0.88;
    let digit_y = y as f32 + (h as f32 - digit_h) / 2.0;

    let mut dx = x as f32;
    for digit in digits {
        draw_lcd_digit(pixmap, dx, digit_y, digit_w, digit_h, digit);
        dx += digit_w + digit_margin;
    }

    let unit_x = x + digits_w as i32;
    let labels = [("K", NetloadUnit::Kilo), ("M", NetloadUnit::Mega), ("G", NetloadUnit::Giga)];
    let label_h = h / 3;
    let font = FontSpec { family: label_family.to_string(), size: (label_h as f32 * 0.72).max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal };
    for (i, (label, kind)) in labels.into_iter().enumerate() {
        let color = if kind == unit { INK } else { GHOST };
        paint::draw_text(pixmap, font_system, swash_cache, label, &font, color, unit_x, y + i as i32 * label_h as i32, unit_col_w, label_h, TextAlign::Center);
    }
}

/// One LCD digit: every one of the 7 segments is always visible (the
/// classic "ghost 8" look of a real LCD readout — you can always make
/// out the full segment pattern, faintly, even when unlit), with
/// whichever segments are actually lit for `digit` drawn over them in
/// full-contrast ink. `None` draws the ghost pattern only — a fully
/// blanked position.
fn draw_lcd_digit(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, digit: Option<u8>) {
    let rects = segment_rects(x, y, w, h);
    let mut ghost_paint = Paint { anti_alias: false, ..Default::default() };
    ghost_paint.set_color(paint::sk_color(GHOST));
    for (rx, ry, rw, rh) in rects {
        if let Some(r) = SkRect::from_xywh(rx, ry, rw.max(1.0), rh.max(1.0)) {
            pixmap.fill_rect(r, &ghost_paint, Transform::identity(), None);
        }
    }

    let Some(digit) = digit else { return };
    let segs = DIGIT_SEGMENTS[digit as usize % 10];
    let mut ink_paint = Paint { anti_alias: false, ..Default::default() };
    ink_paint.set_color(paint::sk_color(INK));
    for (lit, (rx, ry, rw, rh)) in segs.into_iter().zip(rects) {
        if !lit {
            continue;
        }
        if let Some(r) = SkRect::from_xywh(rx, ry, rw.max(1.0), rh.max(1.0)) {
            pixmap.fill_rect(r, &ink_paint, Transform::identity(), None);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_name_row(
    pixmap: &mut Pixmap,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    label_family: &str,
    interface_name: &str,
) {
    let mid_y = y + h as i32 / 2;
    let dash_w = (w as f32 * 0.14).round().max(2.0) as u32;
    paint::fill_rect(pixmap, x, mid_y, dash_w, 2, INK);
    paint::fill_rect(pixmap, x + w as i32 - dash_w as i32, mid_y, dash_w, 2, INK);

    let name = interface_name.to_uppercase();
    let font = FontSpec { family: label_family.to_string(), size: (h as f32 * 0.72).max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal };
    paint::draw_text(pixmap, font_system, swash_cache, &name, &font, INK, x, y, w, h, TextAlign::Center);
}

fn draw_dot_matrix(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, rx_levels: &[u32], tx_levels: &[u32]) {
    if w == 0 || h == 0 {
        return;
    }
    let rows = NET_LOAD_HALF_ROWS * 2;
    let cell_w = w as f32 / NET_LOAD_COLUMNS as f32;
    let cell_h = h as f32 / rows as f32;
    let dot = (cell_w.min(cell_h) * 0.7).max(1.0);

    let mut lit_paint = Paint { anti_alias: false, ..Default::default() };
    lit_paint.set_color(paint::sk_color(INK));
    let mut unlit_paint = Paint { anti_alias: false, ..Default::default() };
    unlit_paint.set_color(paint::sk_color(PANEL_SHADOW));

    for col in 0..NET_LOAD_COLUMNS {
        let rx_lit = rx_levels.get(col).copied().unwrap_or(0).min(NET_LOAD_HALF_ROWS);
        let tx_lit = tx_levels.get(col).copied().unwrap_or(0).min(NET_LOAD_HALF_ROWS);
        for row in 0..rows {
            let is_top_half = row < NET_LOAD_HALF_ROWS;
            let lit = if is_top_half {
                // Row 0 is the panel's top edge; the row nearest that
                // edge lights first, so rx fills *downward* from the top.
                (NET_LOAD_HALF_ROWS - 1 - row) < rx_lit
            } else {
                // Rows just past center light first, so tx fills
                // *upward* from the bottom edge as it grows.
                (row - NET_LOAD_HALF_ROWS) < tx_lit
            };
            let cx = x as f32 + col as f32 * cell_w + (cell_w - dot) / 2.0;
            let cy = y as f32 + row as f32 * cell_h + (cell_h - dot) / 2.0;
            if let Some(r) = SkRect::from_xywh(cx, cy, dot, dot) {
                pixmap.fill_rect(r, if lit { &lit_paint } else { &unlit_paint }, Transform::identity(), None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    #[test]
    fn render_netload_tile_produces_correctly_sized_buffers() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        for size in [16u32, 56, 64] {
            let buffer = render_netload_tile(
                &theme,
                &mut font_system,
                &mut swash_cache,
                size,
                "eth0",
                [None, Some(9), Some(4)],
                NetloadUnit::Kilo,
                &[0, 1, 2, 3, 4],
                &[0, 0, 1, 2, 5],
            );
            assert_eq!(buffer.width, size);
            assert_eq!(buffer.height, size);
            assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn idle_and_busy_readings_render_differently() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        let idle = render_netload_tile(&theme, &mut font_system, &mut swash_cache, 56, "eth0", [None, None, None], NetloadUnit::Kilo, &[0; 16], &[0; 16]);
        let busy = render_netload_tile(
            &theme,
            &mut font_system,
            &mut swash_cache,
            56,
            "eth0",
            [Some(9), Some(9), Some(9)],
            NetloadUnit::Giga,
            &[5; 16],
            &[5; 16],
        );
        assert_ne!(idle.pixels, busy.pixels);
    }

    #[test]
    fn different_units_light_a_different_label() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        let kilo = render_netload_tile(&theme, &mut font_system, &mut swash_cache, 56, "eth0", [None, None, Some(1)], NetloadUnit::Kilo, &[0; 16], &[0; 16]);
        let giga = render_netload_tile(&theme, &mut font_system, &mut swash_cache, 56, "eth0", [None, None, Some(1)], NetloadUnit::Giga, &[0; 16], &[0; 16]);
        assert_ne!(kilo.pixels, giga.pixels, "the K/M/G column should visibly change which label is lit");
    }

    #[test]
    fn different_interface_names_render_differently() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        let a = render_netload_tile(&theme, &mut font_system, &mut swash_cache, 56, "eth0", [None; 3], NetloadUnit::Kilo, &[0; 16], &[0; 16]);
        let b = render_netload_tile(&theme, &mut font_system, &mut swash_cache, 56, "wlan0", [None; 3], NetloadUnit::Kilo, &[0; 16], &[0; 16]);
        assert_ne!(a.pixels, b.pixels);
    }
}
