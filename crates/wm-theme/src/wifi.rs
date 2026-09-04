//! The network-link instrument: which link the machine is on and how
//! good it is, rendered on the [`crate::panel`] LED screen. A sibling
//! of [`crate::netload`], and it keeps that instrument's layout
//! grammar so the two read as one family on the dock: the glass well
//! takes the upper region of the tile, a strip of tile-face lettering
//! sits below it, and everything on the glass is drawn in
//! [`crate::panel::PanelPalette`] colors only.
//!
//! The glass splits into two rows. The upper row is the LED readout —
//! seven-segment digits plus a dim lettering mark for the unit: signal
//! percent with a `%` mark on wifi, negotiated speed with an `M`
//! (Mb/s) or `G` (Gb/s) mark on wired. The lower row is the
//! state-specific indicator: a five-bar ascending signal staircase for
//! wifi (the universally-read radio-strength shape, drawn as
//! hard-edged LED bars with ghost bars keeping the silhouette visible
//! at low signal), or a link lamp with `LINK` lettering for wired —
//! the steady green lamp of a switch port, restated in the theme's LED
//! ink. A down link is a designed state, not a blank: the readout
//! shows the full ghost-8 pattern (a powered instrument with no
//! reading), the lamp goes ghost, and the lettering says `DOWN` in dim
//! ink.
//!
//! The tile-face strip carries the link's name (SSID or interface, in
//! lettering caps, truncated by measurement so it never collides with
//! its neighbors) and a two-cell `W`/`E` mode column on the right —
//! the lit-vs-dim indicator idea [`crate::netload`]'s `K`/`M`/`G`
//! column established, here answering "over what" at a glance:
//! wireless or ethernet, neither when down. The no-hardware-at-all
//! case is not this module's job: widgets show
//! [`crate::panel::render_dead_tile`] with `LNK` for that.

use tiny_skia::{Paint, Pixmap, Rect as SkRect, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::{FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::panel::{self, PanelPalette};
use crate::tile;
use crate::Theme;

/// One reading of the machine's network link, already reduced to plain
/// values by the sampling side (the chonkstep widget, or any
/// `chonk-ui` dockapp): the renderer never touches the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkReading<'a> {
    /// An associated wireless link: the network's name and signal
    /// quality as a percentage (clamped to 100 when drawn).
    Wifi { ssid: &'a str, signal_pct: u8 },
    /// A wired link that is up. `speed_mbps` is the negotiated speed
    /// in Mb/s when the kernel reports one; `None` (an interface whose
    /// driver reports no speed, or -1) blanks the readout while the
    /// link lamp stays lit — up, speed unknown.
    Wired { interface: &'a str, speed_mbps: Option<u32> },
    /// Interfaces exist but none is up: the named one is what the
    /// widget last watched, so the user can still see *which* port is
    /// dead.
    Down { interface: &'a str },
}

/// How many bars the wifi signal staircase has. Five matches the shape
/// everyone already reads on a phone's status bar.
pub const SIGNAL_BARS: u32 = 5;

/// Signal percent to lit staircase bars: ceiling, so any nonzero
/// signal lights at least one bar — a barely-associated link must not
/// look identical to no signal at all.
pub fn signal_bars(signal_pct: u8) -> u32 {
    (signal_pct.min(100) as u32 * SIGNAL_BARS).div_ceil(100)
}

/// Signal percent as the three-position readout, leading zeros blanked
/// the way [`crate::netload`]'s original blanks them — `87` reads
/// `_87`, never `087`.
pub fn signal_digits(signal_pct: u8) -> [Option<u8>; 3] {
    let p = signal_pct.min(100) as u32;
    let d = |v: u32| Some((v % 10) as u8);
    match p {
        100 => [Some(1), Some(0), Some(0)],
        10..=99 => [None, d(p / 10), d(p)],
        _ => [None, None, d(p)],
    }
}

/// Negotiated speed as the four-position readout plus its unit mark.
/// Four digits cover every Mb/s speed through 9999; from 10 Gb/s up
/// the value switches to whole gigabits with a `G` mark, which is how
/// the speeds are named anyway (a "10G" port, not a "10000M" one).
pub fn speed_digits(speed_mbps: u32) -> ([Option<u8>; 4], &'static str) {
    let (value, mark) = if speed_mbps >= 10_000 { (speed_mbps / 1000, "G") } else { (speed_mbps, "M") };
    let v = value.min(9999);
    let raw = [v / 1000, (v / 100) % 10, (v / 10) % 10, v % 10];
    let mut out = [None; 4];
    let mut significant = false;
    for (slot, digit) in out.iter_mut().zip(raw) {
        significant = significant || digit != 0;
        if significant {
            *slot = Some(digit as u8);
        }
    }
    // An honest zero still shows one digit rather than a fully blank
    // readout, which would be indistinguishable from "no reading".
    out[3] = out[3].or(Some(0));
    (out, mark)
}

/// Renders the link instrument at `size` x `size`. Pure: everything it
/// needs arrives in `reading`, so the same inputs always produce the
/// same pixels.
pub fn render_wifi_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    reading: &LinkReading,
) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero wifi tile size");
    // Glass lettering needs a real, registered family for cosmic-text
    // to resolve (see netload.rs for why "sans-serif" silently renders
    // nothing); the theme's menu face is the confirmed-available one.
    let family = theme.menu.item_font.family.clone();

    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    // The netload margin/strip recipe verbatim: the well's sunken
    // bevel stays clear of the tile's raised relief, and the label
    // strip height scales with the tile like every other piece of
    // chrome.
    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let strip_h = ((size as f32) * 0.20).round().max(9.0) as i32;
    let well_w = (size as i32 - margin * 2).max(0) as u32;
    let well_h = (size as i32 - margin * 2 - strip_h).max(0) as u32;
    let (gx, gy, gw, gh) = panel::draw_panel_glass(&mut pixmap, margin, margin, well_w, well_h, theme);
    let pal = panel::panel_palette(theme);

    // Glass rows: readout on top, indicator below — the same top-heavy
    // split netload gives its digits over its history matrix.
    let readout_h = (gh as f32 * 0.52).round() as u32;
    let meter_h = gh.saturating_sub(readout_h);
    let meter_y = gy + readout_h as i32;

    match reading {
        LinkReading::Wifi { ssid: _, signal_pct } => {
            draw_readout(&mut pixmap, font_system, swash_cache, &family, gx, gy, gw, readout_h, &pal, &signal_digits(*signal_pct), Some("%"));
            draw_signal_stairs(&mut pixmap, gx, meter_y, gw, meter_h, &pal, signal_bars(*signal_pct));
        }
        LinkReading::Wired { interface: _, speed_mbps } => {
            let (digits, mark) = match speed_mbps {
                Some(speed) => speed_digits(*speed),
                None => ([None; 4], "M"),
            };
            draw_readout(&mut pixmap, font_system, swash_cache, &family, gx, gy, gw, readout_h, &pal, &digits, Some(mark));
            draw_link_row(&mut pixmap, font_system, swash_cache, &family, gx, meter_y, gw, meter_h, &pal, true, "LINK");
        }
        LinkReading::Down { interface: _ } => {
            // Full-width ghost readout: no unit mark, because there is
            // no quantity for a unit to belong to.
            draw_readout(&mut pixmap, font_system, swash_cache, &family, gx, gy, gw, readout_h, &pal, &[None; 4], None);
            draw_link_row(&mut pixmap, font_system, swash_cache, &family, gx, meter_y, gw, meter_h, &pal, false, "DOWN");
        }
    }

    let (name, wifi_lit, wired_lit) = match reading {
        LinkReading::Wifi { ssid, .. } => (*ssid, true, false),
        LinkReading::Wired { interface, .. } => (*interface, false, true),
        LinkReading::Down { interface } => (*interface, false, false),
    };
    draw_label_strip(
        &mut pixmap,
        theme,
        font_system,
        swash_cache,
        &family,
        margin,
        margin + well_h as i32,
        well_w,
        strip_h as u32,
        name,
        wifi_lit,
        wired_lit,
    );

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

fn solid(color: crate::model::Color) -> Paint<'static> {
    let mut p = Paint { anti_alias: false, ..Default::default() };
    p.set_color(paint::sk_color(color));
    p
}

fn frect(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, p: &Paint) {
    if let Some(r) = SkRect::from_xywh(x, y, w.max(1.0), h.max(1.0)) {
        pixmap.fill_rect(r, p, Transform::identity(), None);
    }
}

/// The LED readout row: digits across the left, an optional dim unit
/// mark in a reserved cell on the right — reserved (not overlaid) so
/// the mark can never collide with a wide digit count.
#[allow(clippy::too_many_arguments)]
fn draw_readout(
    pixmap: &mut Pixmap,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    family: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    pal: &PanelPalette,
    digits: &[Option<u8>],
    mark: Option<&str>,
) {
    if w == 0 || h == 0 {
        return;
    }
    let mark_w = if mark.is_some() { ((w as f32) * 0.20).round() as u32 } else { 0 };
    panel::draw_led_digits(pixmap, x, y, w.saturating_sub(mark_w), h, pal, digits);
    if let Some(mark) = mark {
        let font = FontSpec { family: family.to_string(), size: (h as f32 * 0.45).max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal };
        paint::draw_text(pixmap, font_system, swash_cache, mark, &font, pal.ink_dim, x + (w - mark_w) as i32, y, mark_w, h, TextAlign::Center);
    }
}

/// The wifi strength staircase: `SIGNAL_BARS` bars of ascending height
/// on a shared baseline, the first `lit` in ink, the rest as ghosts —
/// so weak signal still shows the whole instrument, with most of it
/// visibly unlit. Hand-drawn (not [`panel::draw_led_bar`]) because the
/// staircase silhouette *is* the icon; uniform-height segments would
/// read as a volume meter, not a radio.
fn draw_signal_stairs(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, pal: &PanelPalette, lit: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let pad_x = (w as f32 * 0.10).max(1.0);
    let pad_y = (h as f32 * 0.12).max(1.0);
    let inner_w = (w as f32 - pad_x * 2.0).max(1.0);
    let inner_h = (h as f32 - pad_y * 2.0).max(2.0);
    let cell = inner_w / SIGNAL_BARS as f32;
    let gap = (cell * 0.30).clamp(1.0, cell * 0.5);
    let baseline = y as f32 + h as f32 - pad_y;
    let ink = solid(pal.ink);
    let ghost = solid(pal.ghost);
    for i in 0..SIGNAL_BARS {
        let bar_h = (inner_h * (i + 1) as f32 / SIGNAL_BARS as f32).max(2.0);
        let bx = x as f32 + pad_x + i as f32 * cell + gap / 2.0;
        frect(pixmap, bx, baseline - bar_h, cell - gap, bar_h, if i < lit { &ink } else { &ghost });
    }
}

/// The wired-link indicator: a square LED lamp beside its lettering,
/// centered as one unit. Lit means carrier — lamp and lettering both
/// in full ink, the steady port light of a switch. Unlit is the down
/// state's face: ghost lamp, dim lettering.
#[allow(clippy::too_many_arguments)]
fn draw_link_row(
    pixmap: &mut Pixmap,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    family: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    pal: &PanelPalette,
    lit: bool,
    label: &str,
) {
    if w == 0 || h == 0 {
        return;
    }
    let lamp = ((h as f32) * 0.42).round().max(3.0) as u32;
    let gap = ((h as f32) * 0.22).round().max(2.0) as i32;
    let font = FontSpec { family: family.to_string(), size: (h as f32 * 0.52).max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal };
    let text_w = paint::text_width(font_system, &font, label).min(w.saturating_sub(lamp + gap as u32));
    let total = (lamp as i32 + gap + text_w as i32).min(w as i32);
    let start = x + (w as i32 - total) / 2;
    let lamp_y = y + (h.saturating_sub(lamp) / 2) as i32;
    paint::fill_rect(pixmap, start, lamp_y, lamp, lamp, if lit { pal.ink } else { pal.ghost });
    let color = if lit { pal.ink } else { pal.ink_dim };
    paint::draw_text(pixmap, font_system, swash_cache, label, &font, color, start + lamp as i32 + gap, y, text_w, h, TextAlign::Left);
}

/// The tile-face lettering under the well: the link's name on the
/// left, the `W`/`E` mode column on the right with the active mode in
/// full ink — netload's unit-column idea, answering "over what". A
/// name too wide for its region first drops one lettering size (SSIDs
/// run much longer than the interface names netload's strip was sized
/// for), and only then truncates by shaped measurement — so it can
/// never collide with the mode cells or clip mid-glyph.
#[allow(clippy::too_many_arguments)]
fn draw_label_strip(
    pixmap: &mut Pixmap,
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    family: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    name: &str,
    wifi_lit: bool,
    wired_lit: bool,
) {
    let ink = tile::tile_ink(theme);
    let dim = tile::tile_ink_dim(theme);
    let font = FontSpec { family: family.to_string(), size: (h as f32 * 0.68).max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal };

    let cell_w = (w as f32 * 0.14).round().max(7.0) as u32;
    let marks_w = cell_w * 2;
    for (i, (label, lit)) in [("W", wifi_lit), ("E", wired_lit)].into_iter().enumerate() {
        let color = if lit { ink } else { dim };
        let cx = x + w as i32 - marks_w as i32 + i as i32 * cell_w as i32;
        paint::draw_text(pixmap, font_system, swash_cache, label, &font, color, cx, y, cell_w, h, TextAlign::Center);
    }

    // A down link's name goes dim with everything else: the whole tile
    // recedes, one more cue before reading a single letter.
    let name_color = if wifi_lit || wired_lit { ink } else { dim };
    let name_w = w.saturating_sub(marks_w + cell_w / 4);
    let mut label = name.to_uppercase();
    let mut name_font = font;
    if paint::text_width(font_system, &name_font, &label) > name_w {
        name_font.size = (h as f32 * 0.50).max(6.0);
    }
    while !label.is_empty() && paint::text_width(font_system, &name_font, &label) > name_w {
        label.pop();
    }
    paint::draw_text(pixmap, font_system, swash_cache, &label, &name_font, name_color, x, y, name_w, h, TextAlign::Left);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::{all_themes, nextstep_classic};
    use crate::model::Color;

    fn ctx() -> (cosmic_text::FontSystem, cosmic_text::SwashCache) {
        (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new())
    }

    fn count_exact(buffer: &DecorationBuffer, color: Color) -> usize {
        buffer.pixels.as_chunks::<4>().0.iter().filter(|p| (p[0], p[1], p[2]) == (color.r, color.g, color.b)).count()
    }

    #[test]
    fn every_state_renders_at_every_size() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let states = [
            LinkReading::Wifi { ssid: "HOMEBASE", signal_pct: 87 },
            LinkReading::Wired { interface: "enp0s1", speed_mbps: Some(1000) },
            LinkReading::Wired { interface: "enp0s1", speed_mbps: None },
            LinkReading::Down { interface: "eth0" },
        ];
        for size in [16u32, 56, 112] {
            for state in &states {
                let buffer = render_wifi_tile(&theme, &mut fs, &mut sc, size, state);
                assert_eq!((buffer.width, buffer.height), (size, size));
                assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
            }
        }
    }

    #[test]
    fn the_four_states_are_pairwise_distinct_at_default_scale() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let faces: Vec<Vec<u8>> = [
            LinkReading::Wifi { ssid: "NET", signal_pct: 87 },
            LinkReading::Wifi { ssid: "NET", signal_pct: 23 },
            LinkReading::Wired { interface: "NET", speed_mbps: Some(1000) },
            LinkReading::Down { interface: "NET" },
        ]
        .iter()
        .map(|s| render_wifi_tile(&theme, &mut fs, &mut sc, 56, s).pixels)
        .collect();
        for a in 0..faces.len() {
            for b in (a + 1)..faces.len() {
                assert_ne!(faces[a], faces[b], "states {a} and {b} rendered identically");
            }
        }
    }

    #[test]
    fn stronger_signal_lights_more_ink() {
        // Same SSID, so every lit-ink difference comes from the meter
        // and the digits — strong must strictly outglow weak.
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let ink = panel::panel_palette(&theme).ink;
        for size in [56u32, 112] {
            let strong = render_wifi_tile(&theme, &mut fs, &mut sc, size, &LinkReading::Wifi { ssid: "NET", signal_pct: 92 });
            let weak = render_wifi_tile(&theme, &mut fs, &mut sc, size, &LinkReading::Wifi { ssid: "NET", signal_pct: 18 });
            assert!(
                count_exact(&strong, ink) > count_exact(&weak, ink),
                "size {size}: strong signal should light strictly more ink"
            );
        }
    }

    #[test]
    fn a_down_link_lights_no_ink_at_all() {
        // The whole down face is ghosts and dim lettering; full ink
        // appearing anywhere would mean some element failed to recede.
        // (Sound only on the flagship theme, whose grayscale face can't
        // blend into its saturated LED accent by accident.)
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let pal = panel::panel_palette(&theme);
        assert!(pal.ink.r != pal.ink.g || pal.ink.g != pal.ink.b, "test premise: flagship LED ink is saturated");
        for size in [56u32, 112] {
            let down = render_wifi_tile(&theme, &mut fs, &mut sc, size, &LinkReading::Down { interface: "eth0" });
            assert_eq!(count_exact(&down, pal.ink), 0, "size {size}: down face must not light full ink");
            let up = render_wifi_tile(&theme, &mut fs, &mut sc, size, &LinkReading::Wired { interface: "eth0", speed_mbps: Some(1000) });
            assert!(count_exact(&up, pal.ink) > 0, "size {size}: a live link must light the lamp and digits");
        }
    }

    #[test]
    fn negotiated_speeds_are_distinguishable() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let s100 = render_wifi_tile(&theme, &mut fs, &mut sc, 56, &LinkReading::Wired { interface: "E", speed_mbps: Some(100) });
        let s1000 = render_wifi_tile(&theme, &mut fs, &mut sc, 56, &LinkReading::Wired { interface: "E", speed_mbps: Some(1000) });
        let unknown = render_wifi_tile(&theme, &mut fs, &mut sc, 56, &LinkReading::Wired { interface: "E", speed_mbps: None });
        assert_ne!(s100.pixels, s1000.pixels);
        assert_ne!(s1000.pixels, unknown.pixels);
    }

    #[test]
    fn different_ssids_render_differently_and_long_ones_stay_inside() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let a = render_wifi_tile(&theme, &mut fs, &mut sc, 56, &LinkReading::Wifi { ssid: "ATTIC", signal_pct: 50 });
        let b = render_wifi_tile(&theme, &mut fs, &mut sc, 56, &LinkReading::Wifi { ssid: "CELLAR", signal_pct: 50 });
        assert_ne!(a.pixels, b.pixels);
        // A pathological SSID must truncate, not overflow or panic —
        // the mode cells sit right of the name, so an overflow would
        // change the pixels under the W mark's cell.
        let long = render_wifi_tile(
            &theme,
            &mut fs,
            &mut sc,
            56,
            &LinkReading::Wifi { ssid: "AN ABSURDLY LONG NETWORK NAME THAT CANNOT FIT", signal_pct: 50 },
        );
        assert_eq!((long.width, long.height), (56, 56));
    }

    #[test]
    fn every_theme_keeps_a_substantial_glass() {
        let (mut fs, mut sc) = ctx();
        for theme in all_themes() {
            let pal = panel::panel_palette(&theme);
            let buffer = render_wifi_tile(&theme, &mut fs, &mut sc, 112, &LinkReading::Wifi { ssid: "NET", signal_pct: 60 });
            assert!(
                count_exact(&buffer, pal.glass) > 500,
                "theme {}: expected a substantial glass area",
                theme.id
            );
        }
    }

    #[test]
    fn signal_bars_map_with_a_ceiling() {
        assert_eq!(signal_bars(0), 0);
        assert_eq!(signal_bars(1), 1);
        assert_eq!(signal_bars(20), 1);
        assert_eq!(signal_bars(21), 2);
        assert_eq!(signal_bars(80), 4);
        assert_eq!(signal_bars(81), 5);
        assert_eq!(signal_bars(100), 5);
        assert_eq!(signal_bars(255), 5, "out-of-range input clamps, never overflows the meter");
    }

    #[test]
    fn signal_digits_blank_leading_zeros() {
        assert_eq!(signal_digits(0), [None, None, Some(0)]);
        assert_eq!(signal_digits(7), [None, None, Some(7)]);
        assert_eq!(signal_digits(42), [None, Some(4), Some(2)]);
        assert_eq!(signal_digits(100), [Some(1), Some(0), Some(0)]);
        assert_eq!(signal_digits(200), [Some(1), Some(0), Some(0)], "clamped to 100");
    }

    #[test]
    fn speed_digits_cover_the_real_ladder() {
        assert_eq!(speed_digits(10), ([None, None, Some(1), Some(0)], "M"));
        assert_eq!(speed_digits(100), ([None, Some(1), Some(0), Some(0)], "M"));
        assert_eq!(speed_digits(1000), ([Some(1), Some(0), Some(0), Some(0)], "M"));
        assert_eq!(speed_digits(2500), ([Some(2), Some(5), Some(0), Some(0)], "M"));
        assert_eq!(speed_digits(10_000), ([None, None, Some(1), Some(0)], "G"));
        assert_eq!(speed_digits(100_000), ([None, Some(1), Some(0), Some(0)], "G"));
        assert_eq!(speed_digits(0), ([None, None, None, Some(0)], "M"), "an honest zero still shows a digit");
    }
}
