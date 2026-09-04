//! An exact-as-practical port of the classic network-load dockapp,
//! sitting on this theme system's common tile: the widget is a
//! [`tile::draw_tile_base`] like every other dock item, and the
//! LCD — the monochrome screen carrying the three-digit seven-segment
//! throughput readout and the mirrored dot-matrix history graph
//! (download filling down from the top edge, upload filling up from
//! the bottom edge — "without resorting to colors," exactly as the
//! original's own description puts it: direction is shown by
//! *position*, not hue) — is recessed into a [`tile::draw_tile_well`],
//! the classic dockapp instrument look. The LCD's own sage-gray
//! monochrome palette stays deliberately fixed rather than
//! theme-derived: it depicts a specific piece of hardware — a screen —
//! not tile chrome. Everything *off* the glass (the interface name,
//! the K/M/G unit indicators) is lettering on the tile face, so it
//! uses [`tile::tile_ink`]/[`tile::tile_ink_dim`] like the rest of the
//! family.

use tiny_skia::{Paint, Pixmap, Rect as SkRect, Transform};
use wm_theme_api::DecorationBuffer;

use crate::digitalclock::{segment_rects, DIGIT_SEGMENTS};
use crate::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::tile;
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

// The LCD glass's own fixed palette — see the module doc comment for
// why these are not theme-derived. `INK` here is the LCD's segment
// ink, a different thing from `tile::tile_ink` (which is the tile
// face's lettering color).
const PANEL: Color = Color { r: 0x68, g: 0x74, b: 0x68, a: 0xff };
const PANEL_SHADOW: Color = Color { r: 0x58, g: 0x64, b: 0x58, a: 0xff };
const GHOST: Color = Color { r: 0x58, g: 0x64, b: 0x58, a: 0xff };
const INK: Color = Color { r: 0x10, g: 0x10, b: 0x10, a: 0xff };

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
    // The LCD keeps fixed colors, but text still needs a real,
    // registered font family for `cosmic-text` to resolve, so this
    // borrows the theme's own (confirmed-available) one rather than
    // guessing a generic name like "sans-serif", which isn't a real
    // face and silently renders nothing.
    let label_family = theme.menu.item_font.family.clone();

    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    // Geometry: the LCD well takes the top of the tile; a strip of
    // tile-face lettering (interface name, unit indicators) sits below
    // it. `margin` keeps the well's sunken bevel clear of the tile's
    // own raised relief so the two recipes never merge into mud, and
    // it scales with the tile like every other piece of chrome.
    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let strip_h = ((size as f32) * 0.20).round().max(9.0) as i32;
    let well_x = margin;
    let well_y = margin;
    let well_w = (size as i32 - margin * 2).max(0);
    let well_h = (size as i32 - margin * 2 - strip_h).max(0);
    tile::draw_tile_well(&mut pixmap, well_x, well_y, well_w as u32, well_h as u32, theme);

    // The glass sits one pixel inside the well's sunken bevel, leaving
    // a ring of the well's shaded face visible around it — the rubber
    // gasket a real instrument's LCD sits behind, and what keeps the
    // sage glass reading as *in* the well rather than pasted over it.
    let glass_inset = t + 1;
    let glass_x = well_x + glass_inset;
    let glass_y = well_y + glass_inset;
    let glass_w = (well_w - glass_inset * 2).max(0) as u32;
    let glass_h = (well_h - glass_inset * 2).max(0) as u32;
    paint::fill_rect(&mut pixmap, glass_x, glass_y, glass_w, glass_h, PANEL);

    // On the glass: digits above, history matrix below. The interface
    // name row the original wedged between them moved off the screen
    // entirely (it is a label, not a reading), which buys the digits
    // and the matrix more height than the original's 56px budget gave.
    let digit_row_h = (glass_h as f32 * 0.48).round() as u32;
    let matrix_h = glass_h.saturating_sub(digit_row_h);
    draw_digit_row(&mut pixmap, glass_x, glass_y, glass_w, digit_row_h, rate_digits);
    draw_dot_matrix(&mut pixmap, glass_x, glass_y + digit_row_h as i32, glass_w, matrix_h, rx_levels, tx_levels);

    draw_label_strip(
        &mut pixmap,
        theme,
        font_system,
        swash_cache,
        &label_family,
        well_x,
        well_y + well_h,
        well_w as u32,
        strip_h as u32,
        interface_name,
        unit,
    );

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// The three seven-segment digits, spread across the glass width. The
/// original reserved a unit column on the right of this row; that
/// column now lives on the tile face (see [`draw_label_strip`]), so
/// the digits get the full width.
fn draw_digit_row(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, digits: [Option<u8>; 3]) {
    let pad = (w as f32 * 0.06).max(1.0);
    let inner_w = (w as f32 - pad * 2.0).max(3.0);
    let digit_margin = (inner_w * 0.06).max(1.0);
    let digit_w = (inner_w / 3.0 - digit_margin).max(1.0);
    let digit_h = h as f32 * 0.80;
    let digit_y = y as f32 + (h as f32 - digit_h) / 2.0;

    let mut dx = x as f32 + pad + digit_margin / 2.0;
    for digit in digits {
        draw_lcd_digit(pixmap, dx, digit_y, digit_w, digit_h, digit);
        dx += digit_w + digit_margin;
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

/// The tile-face lettering under the well: interface name on the
/// left, the `K`/`M`/`G` unit column (now a row) on the right with the
/// active unit in full ink and the inactive ones dimmed — the same
/// lit-vs-ghost indicator idea the original drew on the LCD, restated
/// in the tile's own ink so it reads as part of the tile family.
#[allow(clippy::too_many_arguments)]
fn draw_label_strip(
    pixmap: &mut Pixmap,
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    label_family: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    interface_name: &str,
    unit: NetloadUnit,
) {
    let ink = tile::tile_ink(theme);
    let dim = tile::tile_ink_dim(theme);
    let font = FontSpec { family: label_family.to_string(), size: (h as f32 * 0.68).max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal };

    let cell_w = (w as f32 * 0.14).round().max(7.0) as u32;
    let units_w = cell_w * 3;
    let labels = [("K", NetloadUnit::Kilo), ("M", NetloadUnit::Mega), ("G", NetloadUnit::Giga)];
    for (i, (label, kind)) in labels.into_iter().enumerate() {
        let color = if kind == unit { ink } else { dim };
        let cx = x + w as i32 - units_w as i32 + i as i32 * cell_w as i32;
        paint::draw_text(pixmap, font_system, swash_cache, label, &font, color, cx, y, cell_w, h, TextAlign::Center);
    }

    let name = interface_name.to_uppercase();
    let name_w = w.saturating_sub(units_w);
    paint::draw_text(pixmap, font_system, swash_cache, &name, &font, ink, x, y, name_w, h, TextAlign::Left);
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

    /// The restyle's structural claims: the widget's face is the
    /// theme's tile gradient (not the old flat sage panel edge to
    /// edge), and the fixed LCD glass color still exists inside the
    /// well — a screen set into a tile, not a tile that *is* a screen.
    #[test]
    fn tile_face_shows_through_and_the_lcd_glass_survives() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        let size = 64u32;
        let buffer = render_netload_tile(&theme, &mut font_system, &mut swash_cache, size, "eth0", [None; 3], NetloadUnit::Kilo, &[0; 16], &[0; 16]);
        let px = |x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            (buffer.pixels[i], buffer.pixels[i + 1], buffer.pixels[i + 2])
        };
        // Two face pixels far apart along the gradient direction, both
        // inside the tile relief and outside the well/strip: a diagonal
        // gradient must make them differ (the old design filled both
        // with the same flat PANEL color).
        assert_ne!(px(2, 2), px(size - 3, size - 3), "tile gradient should show on the face");
        // The glass keeps its exact fixed color somewhere (interiors
        // between segments and dots; buffers are premultiplied-opaque,
        // so channel values survive verbatim) — enough pixels that it
        // is clearly a panel, not an artifact.
        let glass = buffer
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| (p[0], p[1], p[2]) == (PANEL.r, PANEL.g, PANEL.b))
            .count();
        assert!(glass > 200, "expected a substantial LCD glass area, found {glass} PANEL pixels");
    }
}
