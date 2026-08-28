//! The instrument panel: the theme-reactive LED screen SDK that every
//! "instrument" dock app (network traffic, system load, sound, power)
//! builds on, the way everything square builds on [`crate::tile`].
//!
//! The look is the classic WindowMaker dockapp instrument — a dark
//! glass screen recessed into the tile behind a gasket (see
//! [`crate::netload`], the wmnetload port this generalizes from) — but
//! where wmnetload's LCD keeps a fixed sage palette because it depicts
//! one specific piece of hardware, *this* panel is a family of screens
//! that belong to the theme: every color on the glass derives from one
//! accent picked out of the theme's own terminal palette
//! ([`panel_accent`]), so the Amber Phosphor theme gets amber LEDs,
//! Teal Blueprint gets teal ones, and a new theme gets its own glow
//! with zero per-app work.
//!
//! What's here:
//! - [`panel_palette`] / [`panel_accent`]: the theme-derived glass and
//!   LED ink colors.
//! - [`draw_panel_glass`]: well + gasket + glass — the screen every
//!   instrument draws its readouts on.
//! - [`draw_led_digits`]: an N-digit seven-segment readout with the
//!   real-LCD "ghost 8" under every digit (reuses
//!   [`crate::digitalclock`]'s segment geometry).
//! - [`draw_led_bar`]: a segmented meter (volume, capacity, signal)
//!   with ghost segments past the lit level.
//! - [`draw_led_columns`]: a one-sided column-history graph (load over
//!   time).
//! - [`draw_led_matrix`]: the mirrored two-direction dot matrix
//!   (download filling down from the top edge, upload up from the
//!   bottom — direction by position, not hue, as wmnetload put it).
//! - [`render_dead_tile`]: the powered-off instrument — a blank glass
//!   with a dim label — for empty states (no battery, no sink, a
//!   widget not yet live).
//!
//! Readouts draw hard-edged and unantialiased on purpose: these are
//! discrete LEDs behind glass, not vector art.

use tiny_skia::{Paint, Pixmap, Rect as SkRect, Transform};
use wm_theme_api::DecorationBuffer;

use crate::digitalclock::{segment_rects, DIGIT_SEGMENTS};
use crate::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::tile;
use crate::Theme;

/// Every color an instrument needs to draw on its glass. Derived from
/// the theme by [`panel_palette`]; apps should not invent their own
/// glass colors, or the instruments stop reading as one family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelPalette {
    /// The screen's unlit background — near-black with the accent's
    /// hue in it.
    pub glass: Color,
    /// Unlit elements (ghost segments, unlit matrix dots): just barely
    /// off the glass, the way a real LED's die is faintly visible when
    /// dark.
    pub ghost: Color,
    /// Lit elements: the theme's accent, brightness-floored so it
    /// always glows against the glass.
    pub ink: Color,
    /// Secondary lit level — labels on the glass, de-emphasized
    /// readings.
    pub ink_dim: Color,
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color::rgb(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// Hue angle in degrees (`0..360`), HSV convention. Only meaningful on
/// a color with some chroma; callers gate on that.
fn hue_degrees(c: &Color) -> f32 {
    let (r, g, b) = (c.r as f32, c.g as f32, c.b as f32);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    if d <= 0.0 {
        return 0.0;
    }
    let sector = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    sector * 60.0
}

/// Shortest angular distance between two hues on the wheel.
fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// The theme's signature LED color. The terminal cursor is the
/// theme's own statement of its accent (Amber Phosphor's `#FFB000`,
/// Teal Blueprint's `#8FE3D2`), so a cursor with real saturation wins
/// outright — hunting the ANSI palette for the most vivid color
/// instead handed Teal Blueprint its *error red* and every teal
/// instrument glowed salmon (caught in design review). A theme whose
/// cursor can't glow falls through to its terminal palette, and even
/// there the cursor gets a vote: a washed-out cursor that still leans
/// somewhere (NeXT Lavender's cool near-white) picks the vivid ANSI
/// color nearest its own hue — its periwinkle — where taking the
/// loudest slot outright handed it an amber twin of Amber Phosphor's
/// glow, collapsing two themes into one on glass. Only a truly
/// neutral cursor (the flagship's NeXT-gray, Graphite's white) has no
/// lean to honor and takes the most vivid bright terminal color,
/// where vividness is saturation times brightness. Dark accents get
/// lifted toward white until they can actually glow.
pub fn panel_accent(theme: &Theme) -> Color {
    let saturation = |c: &Color| (c.r.max(c.g).max(c.b) - c.r.min(c.g).min(c.b)) as u32;
    let score = |c: &Color| saturation(c) * c.r.max(c.g).max(c.b) as u32;
    let cursor = theme.terminal.cursor;
    let mut best = cursor;
    if saturation(&cursor) < 48 {
        let loudest = theme.terminal.ansi.iter().copied().max_by_key(score).unwrap_or(cursor);
        best = if saturation(&cursor) >= 8 {
            // Candidates must still be vivid enough to be the glow —
            // within half the loudest slot's vividness — so hue
            // matching can't land on a barely-tinted pastel.
            let target = hue_degrees(&cursor);
            theme
                .terminal
                .ansi
                .iter()
                .filter(|c| score(c) * 2 >= score(&loudest))
                .min_by(|a, b| hue_distance(hue_degrees(a), target).total_cmp(&hue_distance(hue_degrees(b), target)))
                .copied()
                .unwrap_or(loudest)
        } else {
            loudest
        };
    }
    // Brightness floor: an accent that can't outshine the glass isn't
    // an LED. 400 of 765 total channel sum keeps hue while guaranteeing
    // glow.
    let total = best.r as u32 + best.g as u32 + best.b as u32;
    if total < 400 {
        let t = (400 - total) as f32 / 765.0;
        best = mix(best, Color::rgb(0xFF, 0xFF, 0xFF), t);
    }
    best
}

/// See [`PanelPalette`]. All four colors derive from [`panel_accent`],
/// so the whole screen shifts hue together when the theme changes.
pub fn panel_palette(theme: &Theme) -> PanelPalette {
    let accent = panel_accent(theme);
    let glass = mix(accent, Color::rgb(0x05, 0x06, 0x05), 0.90);
    PanelPalette {
        glass,
        ghost: mix(accent, glass, 0.80),
        ink: accent,
        ink_dim: mix(accent, glass, 0.48),
    }
}

/// The recessed screen: a [`tile::draw_tile_well`] with the glass fill
/// set one pixel inside its sunken bevel, leaving the well's shaded
/// face visible as the gasket a real instrument's screen sits behind
/// (the recipe [`crate::netload`] established). Returns the glass
/// interior `(x, y, w, h)` — draw readouts inside that, nothing else.
pub fn draw_panel_glass(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, theme: &Theme) -> (i32, i32, u32, u32) {
    let pal = panel_palette(theme);
    tile::draw_tile_well(pixmap, x, y, w, h, theme);
    let t = theme.tile.bevel.width.max(1) as i32;
    let inset = t + 1;
    let gx = x + inset;
    let gy = y + inset;
    let gw = (w as i32 - inset * 2).max(0) as u32;
    let gh = (h as i32 - inset * 2).max(0) as u32;
    paint::fill_rect(pixmap, gx, gy, gw, gh, pal.glass);
    (gx, gy, gw, gh)
}

fn solid(color: Color) -> Paint<'static> {
    let mut p = Paint { anti_alias: false, ..Default::default() };
    p.set_color(paint::sk_color(color));
    p
}

fn frect(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, p: &Paint) {
    if let Some(r) = SkRect::from_xywh(x, y, w.max(1.0), h.max(1.0)) {
        pixmap.fill_rect(r, p, Transform::identity(), None);
    }
}

/// A row of seven-segment digits spread across `(x, y, w, h)`, ghost-8
/// pattern under every position, lit segments in `pal.ink` over it.
/// `None` positions show the ghost only (a blanked leading digit).
pub fn draw_led_digits(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, pal: &PanelPalette, digits: &[Option<u8>]) {
    if digits.is_empty() || w == 0 || h == 0 {
        return;
    }
    let n = digits.len() as f32;
    let pad = (w as f32 * 0.06).max(1.0);
    let inner_w = (w as f32 - pad * 2.0).max(3.0);
    let margin = (inner_w * 0.18 / n).max(1.0);
    let digit_w = (inner_w / n - margin).max(1.0);
    let digit_h = h as f32 * 0.80;
    let digit_y = y as f32 + (h as f32 - digit_h) / 2.0;

    let ghost = solid(pal.ghost);
    let ink = solid(pal.ink);
    let mut dx = x as f32 + pad + margin / 2.0;
    for digit in digits {
        let rects = segment_rects(dx, digit_y, digit_w, digit_h);
        for (rx, ry, rw, rh) in rects {
            frect(pixmap, rx, ry, rw, rh, &ghost);
        }
        if let Some(d) = digit {
            let segs = DIGIT_SEGMENTS[*d as usize % 10];
            for (lit, (rx, ry, rw, rh)) in segs.into_iter().zip(rects) {
                if lit {
                    frect(pixmap, rx, ry, rw, rh, &ink);
                }
            }
        }
        dx += digit_w + margin;
    }
}

/// A segmented meter: `segments` cells along the axis, the first `lit`
/// of them in ink, the rest as ghosts. `vertical` fills bottom-up
/// (levels rise), horizontal fills left-to-right. The classic LED VU
/// strip — volume, battery capacity, signal strength.
pub fn draw_led_bar(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, pal: &PanelPalette, segments: u32, lit: u32, vertical: bool) {
    if segments == 0 || w == 0 || h == 0 {
        return;
    }
    let lit = lit.min(segments);
    let ink = solid(pal.ink);
    let ghost = solid(pal.ghost);
    let along = if vertical { h as f32 } else { w as f32 };
    let cell = along / segments as f32;
    let gap = (cell * 0.25).clamp(1.0, cell * 0.5);
    for i in 0..segments {
        let p = if i < lit { &ink } else { &ghost };
        if vertical {
            // Segment 0 is the bottom cell; the strip fills upward.
            let sy = y as f32 + h as f32 - (i + 1) as f32 * cell + gap / 2.0;
            frect(pixmap, x as f32, sy, w as f32, cell - gap, p);
        } else {
            let sx = x as f32 + i as f32 * cell + gap / 2.0;
            frect(pixmap, sx, y as f32, cell - gap, h as f32, p);
        }
    }
}

/// A one-sided column history: `levels` (oldest first, one per column,
/// each `0..=rows`) as columns of dots filling upward from the bottom
/// edge, unlit rows as ghosts. Load-over-time, in LED form.
pub fn draw_led_columns(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, pal: &PanelPalette, rows: u32, levels: &[u32]) {
    if levels.is_empty() || rows == 0 || w == 0 || h == 0 {
        return;
    }
    let cols = levels.len() as u32;
    let (cell_w, cell_h) = (w as f32 / cols as f32, h as f32 / rows as f32);
    let dot = (cell_w.min(cell_h) * 0.7).max(1.0);
    let ink = solid(pal.ink);
    let ghost = solid(pal.ghost);
    for (col, &level) in levels.iter().enumerate() {
        let level = level.min(rows);
        for row in 0..rows {
            // Row 0 is the top; rows light from the bottom edge up.
            let lit = (rows - 1 - row) < level;
            let cx = x as f32 + col as f32 * cell_w + (cell_w - dot) / 2.0;
            let cy = y as f32 + row as f32 * cell_h + (cell_h - dot) / 2.0;
            frect(pixmap, cx, cy, dot, dot, if lit { &ink } else { &ghost });
        }
    }
}

/// The shared row grid for mirrored two-direction readouts: the top
/// edge of the `k`-th element (an `element_h`-tall dot or slab, `k = 0`
/// nearest the seam) in the top half and in the bottom half of a band
/// at `y` with height `h` split into `2 * half_rows` cells. Both edges
/// round the *same* seam-relative distance, which is what makes the
/// halves pixel-exact mirrors — deriving each row's y independently
/// from the band top (the obvious `y + row * cell` loop) lets rounding
/// drift the two halves a pixel apart, and on an instrument whose
/// whole premise is vertical symmetry a one-pixel limp reads as a
/// defect, not noise.
pub(crate) fn mirrored_row_edges(y: i32, h: u32, half_rows: u32, k: u32, element_h: i32) -> (i32, i32) {
    let seam = y + h as i32 / 2;
    let cell_h = h as f32 / (half_rows * 2) as f32;
    let near = (k as f32 * cell_h + (cell_h - element_h as f32) / 2.0).round() as i32;
    (seam - near - element_h, seam + near)
}

/// The mirrored two-direction matrix wmnetload made canonical: `top`
/// levels fill *downward* from the top edge, `bottom` levels fill
/// *upward* from the bottom edge, each `0..=half_rows` — direction is
/// shown by position, not hue. Columns come from the longer of the two
/// slices, oldest first. Rows sit on [`mirrored_row_edges`]'s integer
/// grid, so the two halves reflect each other exactly at any band
/// height.
pub fn draw_led_matrix(
    pixmap: &mut Pixmap,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    pal: &PanelPalette,
    half_rows: u32,
    top: &[u32],
    bottom: &[u32],
) {
    let cols = top.len().max(bottom.len()) as u32;
    if cols == 0 || half_rows == 0 || w == 0 || h == 0 {
        return;
    }
    let rows = half_rows * 2;
    let (cell_w, cell_h) = (w as f32 / cols as f32, h as f32 / rows as f32);
    let dot = (cell_w.min(cell_h) * 0.7).max(1.0);
    // Dot heights snap to whole pixels so a dot and its mirror are
    // always the same height; widths keep the float grid, since
    // columns carry no symmetry contract.
    let dot_h = (dot.round() as i32).max(1);
    let ink = solid(pal.ink);
    let ghost = solid(pal.ghost);
    for col in 0..cols {
        let top_lit = top.get(col as usize).copied().unwrap_or(0).min(half_rows);
        let bottom_lit = bottom.get(col as usize).copied().unwrap_or(0).min(half_rows);
        let cx = x as f32 + col as f32 * cell_w + (cell_w - dot) / 2.0;
        for k in 0..half_rows {
            let (ty, by) = mirrored_row_edges(y, h, half_rows, k, dot_h);
            frect(pixmap, cx, ty as f32, dot, dot_h as f32, if k < top_lit { &ink } else { &ghost });
            frect(pixmap, cx, by as f32, dot, dot_h as f32, if k < bottom_lit { &ink } else { &ghost });
        }
    }
}

/// The powered-off instrument: a full-size glass with a dim label
/// centered on it. This is both the stub face a not-yet-implemented
/// widget shows and the SDK's empty-state answer (no battery present,
/// no audio sink, no interface) — a dead screen still belongs to the
/// family, where a blank tile or an error string would not.
pub fn render_dead_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    label: &str,
) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero panel tile size");
    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let well = (size as i32 - margin * 2).max(0) as u32;
    let (gx, gy, gw, gh) = draw_panel_glass(&mut pixmap, margin, margin, well, well, theme);

    let pal = panel_palette(theme);
    let font = FontSpec {
        family: theme.menu.item_font.family.clone(),
        size: (size as f32 * 0.14).max(6.0),
        weight: FontWeight::Bold,
        style: FontStyle::Normal,
    };
    paint::draw_text(&mut pixmap, font_system, swash_cache, label, &font, pal.ink_dim, gx, gy, gw, gh, TextAlign::Center);

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::all_themes;

    #[test]
    fn every_theme_gets_a_glowing_ink_on_a_dark_glass() {
        for theme in all_themes() {
            let pal = panel_palette(&theme);
            let lum = |c: Color| c.r as i32 + c.g as i32 + c.b as i32;
            assert!(
                lum(pal.ink) - lum(pal.glass) > 300,
                "theme {}: ink {:?} must clearly outshine glass {:?}",
                theme.id,
                pal.ink,
                pal.glass
            );
            assert!(lum(pal.ghost) > lum(pal.glass), "theme {}: ghosts sit just above the glass", theme.id);
            assert!(lum(pal.ink) > lum(pal.ink_dim), "theme {}: dim ink is dimmer than ink", theme.id);
        }
    }

    #[test]
    fn the_accent_follows_the_theme() {
        let accents: Vec<Color> = all_themes().iter().map(panel_accent).collect();
        let distinct: std::collections::HashSet<(u8, u8, u8)> = accents.iter().map(|c| (c.r, c.g, c.b)).collect();
        assert!(distinct.len() >= 4, "five themes should produce at least four distinct LED accents, got {distinct:?}");
    }

    #[test]
    fn amber_phosphor_leds_are_actually_amber() {
        let amber = crate::default_theme::amber_phosphor();
        let ink = panel_palette(&amber).ink;
        assert!(ink.r > ink.b, "amber ink should be warm, got {ink:?}");
        assert!(ink.r > 0xC0, "amber ink should glow, got {ink:?}");
    }

    /// Regression test for the salmon-teal bug design review caught:
    /// the accent hunt used to pick Teal Blueprint's most vivid ANSI
    /// color — its error red — over its own teal cursor. A saturated
    /// cursor is the theme's declared accent and must win.
    #[test]
    fn teal_blueprint_leds_are_actually_teal() {
        let teal = crate::default_theme::teal_blueprint();
        let ink = panel_palette(&teal).ink;
        assert_eq!((ink.r, ink.g, ink.b), (0x8F, 0xE3, 0xD2), "the teal cursor is the accent, verbatim");
    }

    /// The other half of the review's accent finding: NeXT Lavender's
    /// near-white cursor used to fall through to the loudest ANSI slot
    /// — an amber that made it indistinguishable from Amber Phosphor
    /// on glass. A washed-out cursor still names a temperature, so the
    /// accent must land on the cursor's cool side of the wheel.
    #[test]
    fn next_lavender_leds_stay_cool_rather_than_borrowing_amber() {
        let ink = panel_palette(&crate::default_theme::next_lavender()).ink;
        assert!(ink.b > ink.r, "lavender ink should lean blue, got {ink:?}");
        let amber = panel_palette(&crate::default_theme::amber_phosphor()).ink;
        assert_ne!((ink.r, ink.g, ink.b), (amber.r, amber.g, amber.b));
    }

    /// The mirrored matrix's premise, pinned to the pixel: with equal
    /// top and bottom levels, every row above the seam is the exact
    /// reflection of its partner below. The review caught the third
    /// upload row drifting a pixel low at the 56px tile's 38x17 band
    /// when each row's y was derived independently from the band top.
    #[test]
    fn matrix_halves_mirror_pixel_exactly() {
        let theme = crate::default_theme::nextstep_classic();
        let pal = panel_palette(&theme);
        let levels = [1u32, 4, 2, 3, 0, 4, 1, 2, 3, 4, 0, 1, 2, 3, 4, 2];
        for (w, h) in [(38u32, 17u32), (79, 38), (64, 40)] {
            let mut pm = Pixmap::new(w, h).unwrap();
            draw_led_matrix(&mut pm, 0, 0, w, h, &pal, 4, &levels, &levels);
            let seam = h as i32 / 2;
            let row = |y: i32| {
                let start = (y as u32 * w * 4) as usize;
                pm.data()[start..start + (w * 4) as usize].to_vec()
            };
            for dy in 0..seam {
                assert_eq!(row(seam - 1 - dy), row(seam + dy), "{w}x{h}: rows {dy} out from the seam must mirror");
            }
        }
    }

    #[test]
    fn led_primitives_light_up_with_their_levels() {
        let theme = crate::default_theme::nextstep_classic();
        let pal = panel_palette(&theme);
        let render = |f: &dyn Fn(&mut Pixmap)| {
            let mut pm = Pixmap::new(64, 64).unwrap();
            f(&mut pm);
            pm.data().to_vec()
        };
        let bar0 = render(&|pm| draw_led_bar(pm, 0, 0, 20, 60, &pal, 8, 0, true));
        let bar5 = render(&|pm| draw_led_bar(pm, 0, 0, 20, 60, &pal, 8, 5, true));
        assert_ne!(bar0, bar5);

        let cols_low = render(&|pm| draw_led_columns(pm, 0, 0, 64, 32, &pal, 6, &[1; 16]));
        let cols_high = render(&|pm| draw_led_columns(pm, 0, 0, 64, 32, &pal, 6, &[6; 16]));
        assert_ne!(cols_low, cols_high);

        let m_up = render(&|pm| draw_led_matrix(pm, 0, 0, 64, 40, &pal, 5, &[4; 16], &[0; 16]));
        let m_down = render(&|pm| draw_led_matrix(pm, 0, 0, 64, 40, &pal, 5, &[0; 16], &[4; 16]));
        assert_ne!(m_up, m_down, "direction must be visible by position");

        let d12 = render(&|pm| draw_led_digits(pm, 0, 0, 60, 30, &pal, &[Some(1), Some(2)]));
        let d34 = render(&|pm| draw_led_digits(pm, 0, 0, 60, 30, &pal, &[Some(3), Some(4)]));
        assert_ne!(d12, d34);
    }

    #[test]
    fn dead_tile_renders_at_size_with_the_glass_present() {
        let theme = crate::default_theme::teal_blueprint();
        let mut fs = cosmic_text::FontSystem::new();
        let mut sc = cosmic_text::SwashCache::new();
        let tile = render_dead_tile(&theme, &mut fs, &mut sc, 112, "PWR");
        assert_eq!((tile.width, tile.height), (112, 112));
        let pal = panel_palette(&theme);
        let glass = tile
            .pixels
            .chunks_exact(4)
            .filter(|p| (p[0], p[1], p[2]) == (pal.glass.r, pal.glass.g, pal.glass.b))
            .count();
        assert!(glass > 500, "expected a substantial glass area, found {glass} pixels");
    }
}
