//! The POWER instrument dock app renderer: battery capacity and charge
//! state on a [`crate::panel`] LED screen, laid out on the grammar
//! [`crate::netload`] established — a glass well in the tile's upper
//! region carrying the readings, a strip of tile-face lettering below
//! it carrying the labels.
//!
//! The face is designed around one question asked at a glance: "am I
//! losing power, and how much is left?" Capacity is the hero — a
//! three-digit seven-segment readout (blank-padded like netload's rate,
//! so `60` reads as a number and not a code) over a ten-cell LED meter,
//! one cell per ten percent. Charge state lives in a fixed indicator
//! zone to the right of the digits, where a drawn, hard-edged mark
//! changes with the state: a segment-built bolt, lit while charging and
//! ghosted while discharging (the same lit-vs-ghost idiom every panel
//! primitive uses, so "not charging" is a visible unlit die rather than
//! an absence), and an AC plug when the battery is full and the machine
//! runs on line power.
//!
//! Low battery (at most [`LOW_CAPACITY`] percent while discharging)
//! escalates without leaving the family: the meter's ghost cells go
//! dark so only what remains glows on empty glass, a thin ink frame
//! boxes the meter like an alarm annunciator, the indicator zone swaps
//! the ghost bolt for a lit exclamation mark, and the strip's state
//! word turns to `LOW` in the panel accent — the face's one deliberate
//! tile-face emphasis.
//!
//! A machine with no battery at all (desktop, VM) is not an error, so
//! it does not get the dead screen: [`PowerFace::AcOnly`] renders a
//! deliberate "on line power" face — a large lit AC plug where the
//! digits would be, and the meter replaced by a continuous lit rail,
//! because line power is a steady bus, not a cell that empties.
//! [`panel::render_dead_tile`] (`PWR`) is reserved for
//! [`PowerFace::NoInfo`]: no power information at all.

use tiny_skia::Pixmap;
use wm_theme_api::DecorationBuffer;

use crate::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::panel::{self, PanelPalette};
use crate::tile;
use crate::Theme;

/// What the battery is doing right now. `Full` means "full and on line
/// power" — sysfs reports `Full` (and `Not charging`) only while AC is
/// attached, which is why the face shows it as a lit plug rather than
/// a battery reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeState {
    Charging,
    Discharging,
    Full,
}

/// Everything the renderer needs, as plain values — the widget samples
/// `/sys/class/power_supply` and boils it down to one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerFace {
    /// A battery is present. `capacity` is `None` when the kernel
    /// exposes the battery but not its charge percentage — the digits
    /// blank to ghosts and the meter shows an empty scale, degrading
    /// per-field instead of pretending a number.
    Battery { capacity: Option<u8>, state: ChargeState },
    /// No battery, but a line-power supply exists: the deliberate
    /// desktop/VM face.
    AcOnly,
    /// No power information at all: the SDK's dead screen.
    NoInfo,
}

/// At or below this percentage, a discharging battery is urgent.
pub const LOW_CAPACITY: u8 = 15;

/// The capacity meter's cell count — one cell per ten percent, so the
/// meter reads directly as tens without a scale printed next to it.
pub const METER_SEGMENTS: u32 = 10;

/// Cells lit for a capacity: ceiling, so any nonzero charge lights at
/// least one cell — a meter showing zero while the machine still runs
/// would be lying in the alarming direction.
fn meter_lit(capacity: u8) -> u32 {
    (capacity.min(100) as u32 + 9) / 10
}

/// The three digit positions, leading zeros blanked to ghosts exactly
/// like netload's rate readout — `100` is the only three-digit reading.
fn capacity_digits(capacity: Option<u8>) -> [Option<u8>; 3] {
    let Some(c) = capacity else { return [None; 3] };
    let c = c.min(100);
    [
        if c >= 100 { Some(1) } else { None },
        if c >= 10 { Some((c / 10) % 10) } else { None },
        Some(c % 10),
    ]
}

fn is_low(capacity: Option<u8>, state: ChargeState) -> bool {
    state == ChargeState::Discharging && capacity.is_some_and(|c| c <= LOW_CAPACITY)
}

// The indicator marks, as cell grids scaled to whatever box they land
// in — cells, not vector strokes, so the marks stay hard-edged LED
// clusters at any tile size instead of anti-aliased icon art. Each is
// drawn in a stroke two cells wide so it survives 1px cells.

/// Lightning bolt: a down-left diagonal with two offset bars at the
/// waist — the flash's kink drawn as mass, not as a detached crossbar,
/// which at 2px cells kept reading as a letter S. Kept 4 cells wide so
/// the 56px zone still gets 2px cells (a 5-wide grid drops to 1px there
/// and dissolves).
#[rustfmt::skip]
const BOLT_MARK: [&[u8]; 6] = [
    &[0, 0, 1, 1],
    &[0, 1, 1, 0],
    &[0, 1, 1, 1],
    &[1, 1, 1, 0],
    &[0, 1, 1, 0],
    &[1, 1, 0, 0],
];

/// AC plug, prongs up, cord trailing down — the AC-only face's
/// landmark, where the whole glass gives it room to be 5 cells wide.
#[rustfmt::skip]
const PLUG_MARK: [&[u8]; 7] = [
    &[0, 1, 0, 1, 0],
    &[0, 1, 0, 1, 0],
    &[1, 1, 1, 1, 1],
    &[1, 1, 1, 1, 1],
    &[0, 1, 1, 1, 0],
    &[0, 0, 1, 0, 0],
    &[0, 0, 1, 0, 0],
];

/// The plug restated 3 cells wide for the battery face's indicator
/// zone: the 5-wide landmark grid only fits that box at 1px cells on a
/// 56px tile, where the narrow cut keeps 2px cells and stays a plug.
#[rustfmt::skip]
const PLUG_SMALL_MARK: [&[u8]; 6] = [
    &[1, 0, 1],
    &[1, 0, 1],
    &[1, 1, 1],
    &[1, 1, 1],
    &[0, 1, 0],
    &[0, 1, 0],
];

/// Exclamation mark: the low-battery alarm in the indicator zone. Two
/// cells wide — a 3-wide bar at 2px cells was a square blob, not a `!`.
#[rustfmt::skip]
const BANG_MARK: [&[u8]; 7] = [
    &[1, 1],
    &[1, 1],
    &[1, 1],
    &[1, 1],
    &[1, 1],
    &[0, 0],
    &[1, 1],
];

/// Draws a cell-grid mark centered in `(x, y, w, h)`. The cell edge is
/// the largest whole pixel count that fits the grid in the box — whole
/// pixels so cells never land on fractional boundaries and blur.
fn draw_mark(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, color: Color, grid: &[&[u8]]) {
    let rows = grid.len() as u32;
    let cols = grid.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
    if rows == 0 || cols == 0 || w == 0 || h == 0 {
        return;
    }
    let cell = (w / cols).min(h / rows).max(1) as i32;
    let ox = x + (w as i32 - cell * cols as i32) / 2;
    let oy = y + (h as i32 - cell * rows as i32) / 2;
    for (row, cells) in grid.iter().enumerate() {
        for (col, &on) in cells.iter().enumerate() {
            if on != 0 {
                paint::fill_rect(pixmap, ox + col as i32 * cell, oy + row as i32 * cell, cell as u32, cell as u32, color);
            }
        }
    }
}

/// The netload frame recipe restated: the well fills the tile above a
/// label strip, both inside a margin that keeps the well's sunken bevel
/// clear of the tile's raised relief. Returns `(well, strip)` rects.
type RectI = (i32, i32, u32, u32);

fn frame_layout(size: u32, theme: &Theme) -> (RectI, RectI) {
    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let strip_h = ((size as f32) * 0.20).round().max(9.0) as i32;
    let well_w = (size as i32 - margin * 2).max(0);
    let well_h = (size as i32 - margin * 2 - strip_h).max(0);
    ((margin, margin, well_w as u32, well_h as u32), (margin, margin + well_h, well_w as u32, strip_h as u32))
}

/// The glass interior carved into the battery face's three regions:
/// digits left, indicator zone right of them (beside the lit digits,
/// which right-fill because leading positions blank), meter across the
/// bottom.
struct Regions {
    digits: RectI,
    zone: RectI,
    meter: RectI,
}

fn regions(gx: i32, gy: i32, gw: u32, gh: u32) -> Regions {
    let digit_h = (gh as f32 * 0.55).round() as u32;
    let meter_region_h = gh.saturating_sub(digit_h);
    let zone_w = (gw as f32 * 0.26).round() as u32;
    let pad = (gw as f32 * 0.06).round().max(1.0) as u32;
    let bar_h = ((meter_region_h as f32 * 0.52).round().max(3.0) as u32).min(meter_region_h);
    let bar_y = gy + digit_h as i32 + (meter_region_h.saturating_sub(bar_h) / 2) as i32;
    Regions {
        digits: (gx, gy, gw.saturating_sub(zone_w), digit_h),
        zone: (gx + gw.saturating_sub(zone_w) as i32, gy, zone_w.min(gw), digit_h),
        meter: (gx + pad as i32, bar_y, gw.saturating_sub(pad * 2), bar_h),
    }
}

/// A 1px hard outline — the low-battery alarm box around the meter.
fn frame_rect(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, color: Color) {
    if w == 0 || h == 0 {
        return;
    }
    paint::fill_rect(pixmap, x, y, w, 1, color);
    paint::fill_rect(pixmap, x, y + h as i32 - 1, w, 1, color);
    paint::fill_rect(pixmap, x, y, 1, h, color);
    paint::fill_rect(pixmap, x + w as i32 - 1, y, 1, h, color);
}

fn draw_battery_glass(
    pixmap: &mut Pixmap,
    pal: &PanelPalette,
    gx: i32,
    gy: i32,
    gw: u32,
    gh: u32,
    capacity: Option<u8>,
    state: ChargeState,
) {
    let r = regions(gx, gy, gw, gh);
    let low = is_low(capacity, state);

    let (dx, dy, dw, dh) = r.digits;
    panel::draw_led_digits(pixmap, dx, dy, dw, dh, pal, &capacity_digits(capacity));

    // The mark box: the zone inset slightly so no mark ever kisses the
    // digits or the gasket.
    let (zx, zy, zw, zh) = r.zone;
    let (ix, iy) = ((zw / 8) as i32, (zh / 8) as i32);
    let (bx, by) = (zx + ix, zy + iy);
    let (bw, bh) = (zw.saturating_sub(zw / 4), zh.saturating_sub(zh / 4));
    match (state, low) {
        (ChargeState::Charging, _) => draw_mark(pixmap, bx, by, bw, bh, pal.ink, &BOLT_MARK),
        (ChargeState::Full, _) => draw_mark(pixmap, bx, by, bw, bh, pal.ink, &PLUG_SMALL_MARK),
        (ChargeState::Discharging, true) => draw_mark(pixmap, bx, by, bw, bh, pal.ink, &BANG_MARK),
        // The unlit die: "on battery" is a state worth showing, not an
        // absence — the ghost bolt is the meter's ghosts applied to the
        // indicator.
        (ChargeState::Discharging, false) => draw_mark(pixmap, bx, by, bw, bh, pal.ghost, &BOLT_MARK),
    }

    let lit = capacity.map_or(0, meter_lit);
    let (mx, my, mw, mh) = r.meter;
    if low {
        // Ghost suppression via the palette rather than a second bar
        // implementation: painting ghosts glass-on-glass keeps the lit
        // cells in exactly the geometry the normal meter uses, so the
        // remaining charge doesn't jump position when the alarm state
        // kicks in.
        let alarm = PanelPalette { ghost: pal.glass, ..*pal };
        panel::draw_led_bar(pixmap, mx, my, mw, mh, &alarm, METER_SEGMENTS, lit, false);
        frame_rect(pixmap, mx - 2, my - 2, mw + 4, mh + 4, pal.ink);
    } else {
        panel::draw_led_bar(pixmap, mx, my, mw, mh, pal, METER_SEGMENTS, lit, false);
    }
}

fn draw_ac_glass(pixmap: &mut Pixmap, pal: &PanelPalette, gx: i32, gy: i32, gw: u32, gh: u32) {
    // No capacity exists to meter, so the bottom region carries a
    // continuous lit rail instead of cells — line power drawn honestly
    // as a steady bus — and the freed digit space goes to a large lit
    // plug, unmistakable across the room.
    let rail_region_h = (gh as f32 * 0.25).round() as u32;
    let plug_region_h = gh.saturating_sub(rail_region_h);
    let pad = (gw as f32 * 0.06).round().max(1.0) as u32;
    let inset = (plug_region_h / 10) as i32;
    draw_mark(
        pixmap,
        gx + pad as i32,
        gy + inset,
        gw.saturating_sub(pad * 2),
        plug_region_h.saturating_sub((inset * 2).max(0) as u32),
        pal.ink,
        &PLUG_MARK,
    );
    let rail_h = ((rail_region_h as f32 * 0.30).round().max(2.0) as u32).min(rail_region_h);
    let rail_y = gy + plug_region_h as i32 + (rail_region_h.saturating_sub(rail_h) / 2) as i32;
    paint::fill_rect(pixmap, gx + pad as i32, rail_y, gw.saturating_sub(pad * 2), rail_h, pal.ink);
}

/// The tile-face lettering under the well, netload's grammar: the
/// instrument's name on the left, the state word on the right where
/// netload lights its unit — `word_color` is tile ink except for the
/// low-battery `LOW`, the face's single deliberate accent use.
#[allow(clippy::too_many_arguments)]
fn draw_label_strip(
    pixmap: &mut Pixmap,
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    strip: RectI,
    word: &str,
    word_color: Color,
) {
    let (x, y, w, h) = strip;
    let ink = tile::tile_ink(theme);
    let font = FontSpec {
        family: theme.menu.item_font.family.clone(),
        size: (h as f32 * 0.68).max(6.0),
        weight: FontWeight::Bold,
        style: FontStyle::Normal,
    };
    let name_w = w / 2;
    paint::draw_text(pixmap, font_system, swash_cache, "PWR", &font, ink, x, y, name_w, h, TextAlign::Left);
    paint::draw_text(
        pixmap,
        font_system,
        swash_cache,
        word,
        &font,
        word_color,
        x + name_w as i32,
        y,
        w.saturating_sub(name_w),
        h,
        TextAlign::Right,
    );
}

/// Renders the POWER tile for `face` at `size`. Pure over its inputs:
/// the widget owns all sampling and hands plain values in.
pub fn render_power_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    face: PowerFace,
) -> DecorationBuffer {
    if face == PowerFace::NoInfo {
        return panel::render_dead_tile(theme, font_system, swash_cache, size, "PWR");
    }

    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero power tile size");
    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    let (well, strip) = frame_layout(size, theme);
    let (gx, gy, gw, gh) = panel::draw_panel_glass(&mut pixmap, well.0, well.1, well.2, well.3, theme);
    let pal = panel::panel_palette(theme);

    let ink = tile::tile_ink(theme);
    let (word, word_color) = match face {
        PowerFace::Battery { capacity, state } => {
            draw_battery_glass(&mut pixmap, &pal, gx, gy, gw, gh, capacity, state);
            match state {
                ChargeState::Charging => ("CHG", ink),
                ChargeState::Discharging if is_low(capacity, state) => ("LOW", panel::panel_accent(theme)),
                ChargeState::Discharging => ("BAT", ink),
                ChargeState::Full => ("AC", ink),
            }
        }
        PowerFace::AcOnly => {
            draw_ac_glass(&mut pixmap, &pal, gx, gy, gw, gh);
            ("AC", ink)
        }
        // Handled by the early return; keeping the arm total keeps the
        // compiler checking this match if faces are ever added.
        PowerFace::NoInfo => unreachable!("NoInfo renders the dead tile above"),
    };
    draw_label_strip(&mut pixmap, theme, font_system, swash_cache, strip, word, word_color);

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::{all_themes, nextstep_classic};

    fn render(theme: &Theme, size: u32, face: PowerFace) -> DecorationBuffer {
        let mut fs = cosmic_text::FontSystem::new();
        let mut sc = cosmic_text::SwashCache::new();
        render_power_tile(theme, &mut fs, &mut sc, size, face)
    }

    fn px(buf: &DecorationBuffer, x: i32, y: i32) -> (u8, u8, u8) {
        let i = ((y as u32 * buf.width + x as u32) * 4) as usize;
        (buf.pixels[i], buf.pixels[i + 1], buf.pixels[i + 2])
    }

    fn rgb(c: Color) -> (u8, u8, u8) {
        (c.r, c.g, c.b)
    }

    /// Ink-colored pixel count within a rect — buffers are opaque
    /// premultiplied, so exact channel comparisons are sound.
    fn count_in_rect(buf: &DecorationBuffer, rect: RectI, color: Color) -> usize {
        let (x, y, w, h) = rect;
        let mut n = 0;
        for py in y..y + h as i32 {
            for px_ in x..x + w as i32 {
                if px(buf, px_, py) == rgb(color) {
                    n += 1;
                }
            }
        }
        n
    }

    /// The glass rect exactly as the renderer gets it: by running the
    /// same `draw_panel_glass` on a scratch pixmap, so this never
    /// drifts from panel.rs's inset recipe.
    fn glass_rect(theme: &Theme, size: u32) -> RectI {
        let (well, _) = frame_layout(size, theme);
        let mut scratch = Pixmap::new(size, size).unwrap();
        panel::draw_panel_glass(&mut scratch, well.0, well.1, well.2, well.3, theme)
    }

    const ALL_FACES: [PowerFace; 6] = [
        PowerFace::Battery { capacity: Some(100), state: ChargeState::Full },
        PowerFace::Battery { capacity: Some(60), state: ChargeState::Charging },
        PowerFace::Battery { capacity: Some(80), state: ChargeState::Discharging },
        PowerFace::Battery { capacity: Some(10), state: ChargeState::Discharging },
        PowerFace::AcOnly,
        PowerFace::NoInfo,
    ];

    #[test]
    fn every_face_renders_at_every_size_for_every_theme() {
        for theme in all_themes() {
            let mut fs = cosmic_text::FontSystem::new();
            let mut sc = cosmic_text::SwashCache::new();
            for size in [16u32, 56, 112] {
                for face in ALL_FACES {
                    let buf = render_power_tile(&theme, &mut fs, &mut sc, size, face);
                    assert_eq!((buf.width, buf.height), (size, size), "theme {} face {face:?}", theme.id);
                    assert_eq!(buf.pixels.len(), (size * size * 4) as usize);
                }
            }
        }
    }

    #[test]
    fn all_six_states_render_pairwise_distinctly() {
        let theme = nextstep_classic();
        let rendered: Vec<Vec<u8>> = ALL_FACES.iter().map(|f| render(&theme, 56, *f).pixels).collect();
        for a in 0..rendered.len() {
            for b in a + 1..rendered.len() {
                assert_ne!(rendered[a], rendered[b], "faces {:?} and {:?} render identically", ALL_FACES[a], ALL_FACES[b]);
            }
        }
    }

    #[test]
    fn meter_cells_light_with_capacity_at_exact_pixels() {
        let theme = nextstep_classic();
        let size = 112;
        let pal = panel::panel_palette(&theme);
        let (gx, gy, gw, gh) = glass_rect(&theme, size);
        let (mx, my, mw, mh) = regions(gx, gy, gw, gh).meter;
        let buf = render(&theme, size, PowerFace::Battery { capacity: Some(60), state: ChargeState::Discharging });
        let cell = mw as f32 / METER_SEGMENTS as f32;
        for i in 0..METER_SEGMENTS {
            let cx = mx + (i as f32 * cell + cell / 2.0) as i32;
            let cy = my + mh as i32 / 2;
            let expected = if i < 6 { pal.ink } else { pal.ghost };
            assert_eq!(px(&buf, cx, cy), rgb(expected), "cell {i} at 60 percent");
        }
    }

    #[test]
    fn low_battery_kills_the_ghosts_and_frames_the_meter() {
        let theme = nextstep_classic();
        let size = 112;
        let pal = panel::panel_palette(&theme);
        let (gx, gy, gw, gh) = glass_rect(&theme, size);
        let (mx, my, mw, mh) = regions(gx, gy, gw, gh).meter;
        let cell = mw as f32 / METER_SEGMENTS as f32;
        let mid = |i: u32| (mx + (i as f32 * cell + cell / 2.0) as i32, my + mh as i32 / 2);

        let low = render(&theme, size, PowerFace::Battery { capacity: Some(10), state: ChargeState::Discharging });
        let (x0, y0) = mid(0);
        assert_eq!(px(&low, x0, y0), rgb(pal.ink), "the remaining cell still glows");
        let (x5, y5) = mid(5);
        assert_eq!(px(&low, x5, y5), rgb(pal.glass), "ghost cells go dark, not ghost");
        assert_eq!(px(&low, mx - 2, my - 2), rgb(pal.ink), "the alarm frame corner is inked");

        // The same capacity while charging is not an alarm: ghosts stay.
        let charging = render(&theme, size, PowerFace::Battery { capacity: Some(10), state: ChargeState::Charging });
        assert_eq!(px(&charging, x5, y5), rgb(pal.ghost));
    }

    #[test]
    fn the_indicator_zone_tells_the_states_apart() {
        let theme = nextstep_classic();
        let size = 112;
        let pal = panel::panel_palette(&theme);
        let (gx, gy, gw, gh) = glass_rect(&theme, size);
        let zone = regions(gx, gy, gw, gh).zone;

        let face = |capacity, state| render(&theme, size, PowerFace::Battery { capacity, state });
        let charging = face(Some(80), ChargeState::Charging);
        let discharging = face(Some(80), ChargeState::Discharging);
        let full = face(Some(100), ChargeState::Full);
        let low = face(Some(10), ChargeState::Discharging);

        assert!(count_in_rect(&charging, zone, pal.ink) > 0, "charging bolt is lit");
        assert_eq!(count_in_rect(&discharging, zone, pal.ink), 0, "discharging shows no lit mark");
        assert!(count_in_rect(&discharging, zone, pal.ghost) > 0, "discharging shows the ghost bolt die");
        assert!(count_in_rect(&full, zone, pal.ink) > 0, "full-on-ac plug is lit");
        assert!(count_in_rect(&low, zone, pal.ink) > 0, "low alarm mark is lit");

        // Bolt and plug are different shapes, not just different spots
        // for the same blob.
        let crop = |buf: &DecorationBuffer| {
            let (x, y, w, h) = zone;
            let mut out = Vec::new();
            for py in y..y + h as i32 {
                for px_ in x..x + w as i32 {
                    out.push(px(buf, px_, py));
                }
            }
            out
        };
        assert_ne!(crop(&charging), crop(&full));
        assert_ne!(crop(&charging), crop(&low));
    }

    #[test]
    fn unknown_capacity_degrades_to_ghost_digits_and_an_empty_scale() {
        let theme = nextstep_classic();
        let size = 112;
        let pal = panel::panel_palette(&theme);
        let (gx, gy, gw, gh) = glass_rect(&theme, size);
        let r = regions(gx, gy, gw, gh);
        let buf = render(&theme, size, PowerFace::Battery { capacity: None, state: ChargeState::Discharging });
        assert_eq!(count_in_rect(&buf, r.digits, pal.ink), 0, "no digit segment may claim a reading");
        assert_eq!(count_in_rect(&buf, r.meter, pal.ink), 0, "no meter cell may claim a level");
        assert!(count_in_rect(&buf, r.digits, pal.ghost) > 0, "the ghost 8s show the instrument is alive");
    }

    #[test]
    fn ac_only_face_is_live_and_railed_not_dead() {
        let theme = nextstep_classic();
        let size = 112;
        let pal = panel::panel_palette(&theme);
        let (gx, gy, gw, gh) = glass_rect(&theme, size);
        let buf = render(&theme, size, PowerFace::AcOnly);
        assert_ne!(buf.pixels, render(&theme, size, PowerFace::NoInfo).pixels);

        // The rail: continuous ink at the meter region's center line.
        let rail_region_h = (gh as f32 * 0.25).round() as u32;
        let rail_cy = gy + (gh - rail_region_h) as i32 + rail_region_h as i32 / 2;
        assert_eq!(px(&buf, gx + gw as i32 / 2, rail_cy), rgb(pal.ink));
        // No ghost anywhere: this face has no unlit readings to show.
        assert_eq!(count_in_rect(&buf, (gx, gy, gw, gh), pal.ghost), 0);
        // The plug is substantial — a landmark, not an icon in a corner.
        assert!(count_in_rect(&buf, (gx, gy, gw, gh), pal.ink) > 400, "the AC face should read across the room");
    }

    #[test]
    fn no_info_is_exactly_the_family_dead_screen() {
        let theme = nextstep_classic();
        let mut fs = cosmic_text::FontSystem::new();
        let mut sc = cosmic_text::SwashCache::new();
        let ours = render_power_tile(&theme, &mut fs, &mut sc, 56, PowerFace::NoInfo);
        let dead = panel::render_dead_tile(&theme, &mut fs, &mut sc, 56, "PWR");
        assert_eq!(ours.pixels, dead.pixels);
    }

    #[test]
    fn capacity_digits_blank_leading_positions() {
        assert_eq!(capacity_digits(Some(100)), [Some(1), Some(0), Some(0)]);
        assert_eq!(capacity_digits(Some(60)), [None, Some(6), Some(0)]);
        assert_eq!(capacity_digits(Some(5)), [None, None, Some(5)]);
        assert_eq!(capacity_digits(Some(0)), [None, None, Some(0)]);
        assert_eq!(capacity_digits(None), [None, None, None]);
        assert_eq!(capacity_digits(Some(255)), [Some(1), Some(0), Some(0)], "garbage clamps to 100");
    }

    #[test]
    fn meter_lit_rounds_up_so_a_live_battery_never_shows_empty() {
        assert_eq!(meter_lit(0), 0);
        assert_eq!(meter_lit(1), 1);
        assert_eq!(meter_lit(9), 1);
        assert_eq!(meter_lit(10), 1);
        assert_eq!(meter_lit(11), 2);
        assert_eq!(meter_lit(15), 2);
        assert_eq!(meter_lit(100), 10);
        assert_eq!(meter_lit(200), 10);
    }
}
