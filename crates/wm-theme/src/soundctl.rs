//! The sound instrument dock app renderer: system volume on a
//! [`crate::panel`] LED screen. Built to read as [`crate::netload`]'s
//! sibling — the same layout grammar (glass well in the upper region
//! of the tile, a strip of tile-face lettering below it, the same
//! margin/gasket recipe) with the theme-reactive panel palette instead
//! of netload's fixed hardware sage.
//!
//! On the glass, top to bottom:
//! - a three-digit LED percent readout (blank-padded like a real LCD,
//!   so `55` shows as ghost-8 / `5` / `5`);
//! - a stack of full-width LED slats — [`SOUND_BAR_SEGMENTS`] of them —
//!   filling upward from the glass base, the hi-fi volume-ladder form
//!   of [`crate::panel::draw_led_bar`]. Upward-filling on purpose: the
//!   strip's vertical axis *is* the control axis (see the zone map).
//!
//! On the tile face below the well: the instrument label on the left
//! and a blocky speaker mark on the right. The speaker is the mute
//! control's landmark, so it lives exactly inside the mute zone.
//!
//! Muted is a designed state, not a caption: every element on the
//! glass drops to ghost (digits blanked, no slats lit — a powered-up
//! screen with nothing to say), while the speaker mark sharpens from
//! its usual dim affordance to full tile ink under an accent strike.
//! The one bright thing on a muted tile is the crossed-out speaker.
//!
//! Control zones ([`zone_at`]; tile-local coordinates, y measured from
//! the tile's top edge):
//!
//! ```text
//! +----------------+
//! |  [8][5][5]     |   Louder: everything above the slat stack's
//! |  ============  |   vertical midpoint — "click high on the strip".
//! |  ============  |
//! |----------------|   Softer: the lower half of the stack down to
//! |  ============  |   the glass base.
//! +----------------+
//! | VOL      spkr> |   MuteToggle: the label strip band at the tile
//! +----------------+   base, anchored by the speaker mark.
//! ```
//!
//! The boundaries derive from the same geometry the renderer draws
//! (computed with the builtin themes' 1px bevel), so the zones track
//! what the user actually sees rather than arbitrary thirds.

use tiny_skia::Pixmap;
use wm_theme_api::{DecorationBuffer, Point};

use crate::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::panel;
use crate::tile;
use crate::Theme;

/// How many slats the level stack shows at full volume. Eight keeps
/// every slat at least two pixels tall on the default 56px tile
/// (draw_led_bar needs that to fit a gap between slats), and 12.5%
/// steps are plenty when the exact percent is on the digits.
pub const SOUND_BAR_SEGMENTS: u32 = 8;

/// The three control zones the tile face carves into — see the module
/// doc comment for the map. The widget translates these to mixer
/// commands; the renderer owns the enum because zone boundaries are a
/// function of the drawn layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundZone {
    Louder,
    Softer,
    MuteToggle,
}

/// The vertical bands the tile splits into — shared by the renderer
/// (which places the well and strip with the theme's real bevel) and
/// [`zone_at`] (which uses the builtin 1px bevel, since click handlers
/// get no theme). Same formulas as netload's layout, so the two
/// instruments line up when docked next to each other.
#[derive(Clone, Copy, Debug)]
struct TileRegions {
    margin: i32,
    well_h: i32,
    strip_h: i32,
    /// Glass interior, vertical extent only — the horizontal extent
    /// always mirrors it, and the zones only band on y.
    glass_y: i32,
    glass_h: i32,
}

fn tile_regions(size: u32, bevel_width: u8) -> TileRegions {
    let t = bevel_width.max(1) as i32;
    let size_i = size as i32;
    let margin = t + (size_i / 28).max(1);
    let strip_h = ((size as f32) * 0.20).round().max(9.0) as i32;
    let well_h = (size_i - margin * 2 - strip_h).max(0);
    let glass_inset = t + 1;
    TileRegions {
        margin,
        well_h,
        strip_h,
        glass_y: margin + glass_inset,
        glass_h: (well_h - glass_inset * 2).max(0),
    }
}

/// How much of the glass the digit row takes; the slat stack gets the
/// rest. Slightly less than netload's 0.48 — the stack is the primary
/// reading here and earns the extra rows.
fn digit_row_h(glass_h: i32) -> i32 {
    (glass_h as f32 * 0.44).round() as i32
}

/// Maps a tile-local click to its control zone. Pure geometry so the
/// widget's `on_click` and the unit tests share one source of truth.
/// Uses the builtin themes' 1px bevel (the trait's click handler has
/// no theme in scope); a theme with a wider bevel shifts the drawn
/// bands by only a pixel or two, well under a click target's slack.
pub fn zone_at(local: Point, tile: u32) -> SoundZone {
    let r = tile_regions(tile.max(8), 1);
    let strip_top = r.margin + r.well_h;
    let bar_top = r.glass_y + digit_row_h(r.glass_h);
    let bar_bottom = r.glass_y + r.glass_h;
    let split = (bar_top + bar_bottom) / 2;
    if local.y >= strip_top {
        SoundZone::MuteToggle
    } else if local.y < split {
        SoundZone::Louder
    } else {
        SoundZone::Softer
    }
}

/// The percent readout as blank-padded LED digits: `0.55` becomes
/// blank/5/5, `1.0` becomes 1/0/0. Values above 1.0 (PipeWire allows
/// overdrive, and other mixers may set it) display honestly — `1.5`
/// reads 150 — rather than lying at 100; three digits cap at 999.
pub fn percent_digits(volume: f32) -> [Option<u8>; 3] {
    let p = ((volume.max(0.0) * 100.0).round() as u32).min(999);
    [
        (p >= 100).then_some((p / 100) as u8),
        (p >= 10).then_some(((p / 10) % 10) as u8),
        Some((p % 10) as u8),
    ]
}

/// Lit slat count for a volume level: ceiling, so any audible volume
/// lights at least one slat — a 3% whisper showing a fully dark stack
/// would read as muted, which it is not.
pub fn lit_segments(volume: f32, segments: u32) -> u32 {
    if volume <= 0.0 || segments == 0 {
        return 0;
    }
    ((volume * segments as f32).ceil() as u32).clamp(1, segments)
}

/// Renders the sound tile. `volume` is the sink's level (`1.0` = 100%,
/// values above render as overdrive percent); `muted` blanks the glass
/// and strikes the speaker mark regardless of `volume`, mirroring how
/// PipeWire keeps the level while muted.
pub fn render_soundctl_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    volume: f32,
    muted: bool,
) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero soundctl tile size");
    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    let r = tile_regions(size, theme.tile.bevel.width);
    let well_w = (size as i32 - r.margin * 2).max(0) as u32;
    let (gx, gy, gw, gh) =
        panel::draw_panel_glass(&mut pixmap, r.margin, r.margin, well_w, r.well_h as u32, theme);
    let pal = panel::panel_palette(theme);

    // Digits above, slat stack below — the netload grammar. Muted
    // blanks the digits entirely (ghost 8s across the row): a dead
    // readout is a stronger "no sound" than any zero.
    let digit_h = digit_row_h(gh as i32).max(0) as u32;
    let digits = if muted { [None; 3] } else { percent_digits(volume) };
    panel::draw_led_digits(&mut pixmap, gx, gy, gw, digit_h, &pal, &digits);

    // The stack insets horizontally to match the digits' own side
    // padding and vertically to keep a gasket-like breath from the
    // digit row and the glass base.
    let pad = ((gw as f32) * 0.08).round().max(1.0) as i32;
    let vgap = (gh as i32 / 24).max(1);
    let bar_x = gx + pad;
    let bar_w = (gw as i32 - pad * 2).max(0) as u32;
    let bar_y = gy + digit_h as i32 + vgap;
    let bar_h = (gh as i32 - digit_h as i32 - vgap * 2).max(0) as u32;
    // draw_led_bar needs at least 2px per slat to fit its inter-slat
    // gap; on tiles too small for the full count, fewer, taller slats
    // beat a panic or a smear.
    let segments = SOUND_BAR_SEGMENTS.min(bar_h / 2);
    if segments > 0 {
        let lit = if muted { 0 } else { lit_segments(volume, segments) };
        panel::draw_led_bar(&mut pixmap, bar_x, bar_y, bar_w, bar_h, &pal, segments, lit, true);
    }

    draw_label_strip(
        &mut pixmap,
        theme,
        font_system,
        swash_cache,
        r.margin,
        r.margin + r.well_h,
        well_w,
        r.strip_h as u32,
        muted,
    );

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// The tile-face band under the well: instrument label on the left in
/// tile ink, the speaker mark right-aligned so it sits inside the mute
/// zone it triggers. Unmuted, the speaker is dim — an affordance, not
/// a reading; muted, it goes full ink under a [`panel::panel_accent`]
/// strike, the face's single deliberate accent emphasis.
#[allow(clippy::too_many_arguments)]
fn draw_label_strip(
    pixmap: &mut Pixmap,
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    muted: bool,
) {
    let ink = tile::tile_ink(theme);
    let dim = tile::tile_ink_dim(theme);
    // The theme's own registered family: a generic name like
    // "sans-serif" is not a real face and would silently render nothing.
    let font = FontSpec {
        family: theme.menu.item_font.family.clone(),
        size: (h as f32 * 0.68).max(6.0),
        weight: FontWeight::Bold,
        style: FontStyle::Normal,
    };
    let mark = h.min(w);
    let text_w = w.saturating_sub(mark + 2);
    paint::draw_text(pixmap, font_system, swash_cache, "VOL", &font, ink, x, y, text_w, h, TextAlign::Left);

    let mx = x + w as i32 - mark as i32;
    if muted {
        draw_speaker_mark(pixmap, mx, y, mark, ink, Some(panel::panel_accent(theme)));
    } else {
        draw_speaker_mark(pixmap, mx, y, mark, dim, None);
    }
}

/// A blocky speaker: driver box plus a three-step cone widening to the
/// right, all hard-edged rects in the fractional grid of its `s`-sided
/// cell so it survives 11px and 22px alike. `strike` lays a thick
/// rising slash over it — the crossed-out speaker every mixer UI has
/// taught people to read as "muted".
fn draw_speaker_mark(pixmap: &mut Pixmap, x: i32, y: i32, s: u32, body: Color, strike: Option<Color>) {
    let f = s as f32;
    let cell = |fx: f32, fy: f32, fw: f32, fh: f32| {
        (
            x + (fx * f).round() as i32,
            y + (fy * f).round() as i32,
            ((fw * f).round() as u32).max(1),
            ((fh * f).round() as u32).max(1),
        )
    };
    for (fx, fy, fw, fh) in [
        (0.08, 0.36, 0.22, 0.28),
        (0.30, 0.28, 0.16, 0.44),
        (0.46, 0.18, 0.16, 0.64),
        (0.62, 0.06, 0.16, 0.88),
    ] {
        let (rx, ry, rw, rh) = cell(fx, fy, fw, fh);
        paint::fill_rect(pixmap, rx, ry, rw, rh, body);
    }

    let Some(strike) = strike else { return };
    let thick = ((f / 8.0).round() as i32).max(2);
    let (x0, y0) = (x + (0.06 * f).round() as i32, y + (0.88 * f).round() as i32);
    let (x1, y1) = (x + (0.78 * f).round() as i32, y + (0.10 * f).round() as i32);
    for i in 0..thick {
        tile::draw_line(pixmap, x0 + i, y0, x1 + i, y1, strike);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::{all_themes, nextstep_classic};

    fn ctx() -> (cosmic_text::FontSystem, cosmic_text::SwashCache) {
        (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new())
    }

    #[test]
    fn renders_correctly_sized_buffers_without_panic() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        for size in [8u32, 16, 24, 40, 56, 112] {
            let buf = render_soundctl_tile(&theme, &mut fs, &mut sc, size, 0.55, false);
            assert_eq!((buf.width, buf.height), (size, size));
            assert_eq!(buf.pixels.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn distinct_states_render_distinctly_at_both_scales() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        for size in [56u32, 112] {
            let render = |fs: &mut _, sc: &mut _, vol: f32, muted: bool| {
                render_soundctl_tile(&theme, fs, sc, size, vol, muted).pixels
            };
            let low = render(&mut fs, &mut sc, 0.15, false);
            let mid = render(&mut fs, &mut sc, 0.55, false);
            let full = render(&mut fs, &mut sc, 1.0, false);
            let muted = render(&mut fs, &mut sc, 0.55, true);
            let silent = render(&mut fs, &mut sc, 0.0, false);
            assert_ne!(low, mid, "size {size}: 15% and 55% must differ");
            assert_ne!(mid, full, "size {size}: 55% and 100% must differ");
            assert_ne!(mid, muted, "size {size}: muted must not look like its own level");
            assert_ne!(muted, silent, "size {size}: muted and volume-zero are different states");
        }
    }

    #[test]
    fn digits_alone_distinguish_levels_within_one_slat() {
        // 55% and 56% light the same slat count, so only the percent
        // readout separates them — proves the digits carry real data.
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        assert_eq!(lit_segments(0.55, SOUND_BAR_SEGMENTS), lit_segments(0.56, SOUND_BAR_SEGMENTS));
        let a = render_soundctl_tile(&theme, &mut fs, &mut sc, 112, 0.55, false);
        let b = render_soundctl_tile(&theme, &mut fs, &mut sc, 112, 0.56, false);
        assert_ne!(a.pixels, b.pixels);
    }

    /// The glass in the muted state carries no lit ink at all — the
    /// whole screen drops to ghost. The strike on the face uses the
    /// accent (the same color as the panel ink), so the scan restricts
    /// itself to the glass interior.
    #[test]
    fn muted_glass_is_all_ghost_and_unmuted_glass_glows() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let pal = panel::panel_palette(&theme);
        let size = 56u32;
        let r = tile_regions(size, theme.tile.bevel.width);
        let glass_x = r.glass_y;
        let glass_w = size as i32 - r.glass_y * 2;
        let ink_on_glass = |buf: &DecorationBuffer| {
            let mut count = 0usize;
            for y in r.glass_y..(r.glass_y + r.glass_h) {
                for x in glass_x..(glass_x + glass_w) {
                    let i = ((y as u32 * size + x as u32) * 4) as usize;
                    if (buf.pixels[i], buf.pixels[i + 1], buf.pixels[i + 2]) == (pal.ink.r, pal.ink.g, pal.ink.b) {
                        count += 1;
                    }
                }
            }
            count
        };
        let muted = render_soundctl_tile(&theme, &mut fs, &mut sc, size, 0.55, true);
        let live = render_soundctl_tile(&theme, &mut fs, &mut sc, size, 0.55, false);
        assert_eq!(ink_on_glass(&muted), 0, "muted glass must not light a single LED");
        assert!(ink_on_glass(&live) > 50, "an unmuted 55% should light digits and slats");
    }

    #[test]
    fn louder_volume_lights_more_of_the_glass() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let pal = panel::panel_palette(&theme);
        let mut lit = |vol: f32| {
            render_soundctl_tile(&theme, &mut fs, &mut sc, 112, vol, false)
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| (p[0], p[1], p[2]) == (pal.ink.r, pal.ink.g, pal.ink.b))
                .count()
        };
        let low = lit(0.15);
        let full = lit(1.0);
        assert!(full > low, "100% ({full} ink px) should out-light 15% ({low} ink px)");
    }

    #[test]
    fn every_theme_renders_with_its_own_glass() {
        let (mut fs, mut sc) = ctx();
        for theme in all_themes() {
            let pal = panel::panel_palette(&theme);
            let buf = render_soundctl_tile(&theme, &mut fs, &mut sc, 56, 0.55, false);
            let glass = buf
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| (p[0], p[1], p[2]) == (pal.glass.r, pal.glass.g, pal.glass.b))
                .count();
            assert!(glass > 200, "theme {}: expected a substantial glass area, found {glass} px", theme.id);
        }
    }

    #[test]
    fn percent_digits_blank_pad_like_a_real_lcd() {
        assert_eq!(percent_digits(0.0), [None, None, Some(0)]);
        assert_eq!(percent_digits(0.05), [None, None, Some(5)]);
        assert_eq!(percent_digits(0.15), [None, Some(1), Some(5)]);
        assert_eq!(percent_digits(0.55), [None, Some(5), Some(5)]);
        assert_eq!(percent_digits(1.0), [Some(1), Some(0), Some(0)]);
        assert_eq!(percent_digits(1.5), [Some(1), Some(5), Some(0)], "overdrive shows honestly");
        assert_eq!(percent_digits(-0.2), [None, None, Some(0)], "negative clamps to zero");
        assert_eq!(percent_digits(99.0), [Some(9), Some(9), Some(9)], "three digits cap at 999");
    }

    #[test]
    fn lit_segments_ceil_so_a_whisper_still_shows() {
        assert_eq!(lit_segments(0.0, 8), 0);
        assert_eq!(lit_segments(0.001, 8), 1);
        assert_eq!(lit_segments(0.15, 8), 2);
        assert_eq!(lit_segments(0.55, 8), 5);
        assert_eq!(lit_segments(1.0, 8), 8);
        assert_eq!(lit_segments(2.0, 8), 8, "overdrive clamps to a full stack");
        assert_eq!(lit_segments(0.5, 0), 0);
    }

    #[test]
    fn zones_band_the_tile_top_to_bottom() {
        for tile in [56u32, 112] {
            let at = |y: i32| zone_at(Point::new(tile as i32 / 2, y), tile);
            assert_eq!(at(0), SoundZone::Louder, "tile {tile}: the top edge raises volume");
            assert_eq!(at(tile as i32 / 4), SoundZone::Louder);
            assert_eq!(at(tile as i32 - 1), SoundZone::MuteToggle, "tile {tile}: the base toggles mute");
        }
    }

    /// Boundary rows, pinned at both preview scales from the same
    /// formulas the renderer lays out with — a layout change that moves
    /// the drawn bands must move the click bands with it.
    #[test]
    fn zone_boundaries_track_the_drawn_layout() {
        let at = |y: i32, tile: u32| zone_at(Point::new(3, y), tile);
        // 56: glass rows 5..40, digit row 15 tall, slats 20..40, split
        // at 30; the label strip starts at 42.
        assert_eq!(at(29, 56), SoundZone::Louder);
        assert_eq!(at(30, 56), SoundZone::Softer);
        assert_eq!(at(41, 56), SoundZone::Softer);
        assert_eq!(at(42, 56), SoundZone::MuteToggle);
        // 112: glass rows 7..83, digit row 33 tall, split at 61; the
        // strip starts at 85.
        assert_eq!(at(60, 112), SoundZone::Louder);
        assert_eq!(at(61, 112), SoundZone::Softer);
        assert_eq!(at(84, 112), SoundZone::Softer);
        assert_eq!(at(85, 112), SoundZone::MuteToggle);
    }
}
