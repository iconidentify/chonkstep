//! The network-traffic instrument: a from-scratch reenvisioning of the
//! up/down throughput dockapp, built on the theme-reactive
//! [`crate::panel`] SDK rather than porting one specific piece of
//! hardware the way [`crate::netload`] does — same layout grammar
//! (glass well up top, tile-face label strip below), but every color on
//! the glass follows the theme's [`crate::panel::PanelPalette`].
//!
//! The composition is one vertically symmetric instrument, direction
//! carried by *position*, never hue (the classic dockapp principle):
//! download is always the top story, upload always the bottom one, and
//! the horizontal seam between them is the graph's shared baseline.
//! The middle band — the largest share of the glass, because it is the
//! part that moves — is RECENT: the mirrored dot-matrix history
//! ([`crate::panel::draw_led_matrix`], each direction growing outward
//! from the baseline toward its own story) with, at its right edge, the
//! NOW meter: the newest reading writ large as a mirrored pair of
//! segment stacks — the playhead the scrolling history disappears
//! under. Each story is its lane's absolute NOW: a chevron that lights
//! while traffic flows (pointing down into the machine for download, up
//! and out for upload), a three-digit LED readout, and a unit letter.
//! The split of duties is deliberate: the history is normalized against
//! the interface's own decaying peak, so the matrix carries *shape*
//! ("how does now compare to the last half minute") while the digits
//! carry *magnitude* ("and how fast is that actually") — without the
//! digits, a saturated dial-up link and a saturated fiber link would
//! render identically.

use tiny_skia::{Paint, Pixmap, Rect as SkRect, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::{FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::panel::{self, PanelPalette};
use crate::tile;
use crate::Theme;

/// History columns the graph shows — the widget keeps a longer history
/// than this and hands over the newest this-many quantized samples per
/// direction, oldest first. Sixteen matches the netload port, and at
/// the 56px default scale it is the most columns whose dots stay
/// height-limited rather than width-starved.
pub const NET_TRAFFIC_COLUMNS: usize = 16;

/// Dot rows each direction gets in the history matrix, and segment
/// cells each direction gets in the NOW meter — kept equal on purpose
/// so the meter's cells align with the matrix's rows and a full meter
/// visibly means a full column.
pub const NET_TRAFFIC_HALF_ROWS: u32 = 4;

/// The unit a three-digit mantissa is expressed in — 1024-based, like
/// the classic instruments this one descends from. Each direction
/// carries its own unit letter beside its digits (a download in the
/// megabytes and an upload in the kilobytes is the *normal* case, so a
/// shared unit indicator would misread one of them constantly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateUnit {
    Kilo,
    Mega,
    Giga,
}

impl RateUnit {
    /// The single letter drawn beside the digits.
    pub fn letter(self) -> &'static str {
        match self {
            RateUnit::Kilo => "K",
            RateUnit::Mega => "M",
            RateUnit::Giga => "G",
        }
    }
}

/// One direction's absolute rate, pre-folded into drawable shape by
/// [`format_rate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateReadout {
    /// Three digit positions, `None` for a blanked leading zero — real
    /// LED instruments blank, they don't pad.
    pub digits: [Option<u8>; 3],
    pub unit: RateUnit,
}

/// Everything the renderer needs to know about one direction of
/// traffic, in plain values — the sampling that produces them lives
/// entirely in the widget.
#[derive(Clone, Copy, Debug)]
pub struct TrafficLane<'a> {
    /// The absolute current rate (see [`format_rate`]).
    pub readout: RateReadout,
    /// Current peak-normalized level for the NOW meter and the
    /// direction chevron, `0..=NET_TRAFFIC_HALF_ROWS` (see
    /// [`quantize_level`]).
    pub now: u32,
    /// Peak-normalized history, oldest first, each entry
    /// `0..=NET_TRAFFIC_HALF_ROWS`; send [`NET_TRAFFIC_COLUMNS`] of
    /// them for the intended density.
    pub history: &'a [u32],
}

/// Folds a raw bytes-per-second rate into the three-digit-plus-unit
/// shape the readout draws: mantissa `0..=999`, unit escalating
/// whenever the *rounded* mantissa would need a fourth digit (999.7K
/// must become 1M, not a phantom 1000K). Kilo is the floor — a
/// sub-kilobyte trickle rounds inside it rather than earning a bytes
/// unit nobody could read at dock scale — and Giga is the cap, pinned
/// at 999 because a rate that overflows it is a measurement error, not
/// a network.
pub fn format_rate(bytes_per_sec: f32) -> RateReadout {
    let mut value = bytes_per_sec.max(0.0) / 1024.0;
    let mut unit = RateUnit::Kilo;
    if value.round() > 999.0 {
        value /= 1024.0;
        unit = RateUnit::Mega;
    }
    if value.round() > 999.0 {
        value /= 1024.0;
        unit = RateUnit::Giga;
    }
    let n = (value.round() as u32).min(999);
    let (h, t) = (n / 100, n / 10 % 10);
    let digits = [
        (h > 0).then_some(h as u8),
        (h > 0 || t > 0).then_some(t as u8),
        Some((n % 10) as u8),
    ];
    RateReadout { digits, unit }
}

/// Maps a normalized `0.0..=1.0` reading onto `0..=max_level` LED
/// cells with the instrument rule that any traffic at all lights at
/// least one cell — a trickle must flicker the meter, not vanish in
/// rounding — while only a true zero goes dark.
pub fn quantize_level(value: f32, max_level: u32) -> u32 {
    if max_level == 0 || value <= 0.0 {
        return 0;
    }
    ((value * max_level as f32).ceil() as u32).clamp(1, max_level)
}

/// The tile's fixed geometry at a given size — one source of truth
/// shared by the renderer and its pixel-position tests (the tests
/// assert *where* light appears, and would rot instantly if they
/// re-derived this by hand). The margin/strip recipe mirrors
/// [`crate::netload`] so the two instruments read as siblings on the
/// dock; the glass inset mirrors [`panel::draw_panel_glass`], and the
/// renderer `debug_assert`s that the two stay in agreement.
struct Frame {
    well_x: i32,
    well_y: i32,
    well_w: u32,
    well_h: u32,
    strip_y: i32,
    strip_h: u32,
    glass_x: i32,
    glass_y: i32,
    glass_w: u32,
    glass_h: u32,
    /// Height of each story (the download readout band at the glass
    /// top, the upload one at the bottom); the graph band is whatever
    /// glass height remains between them. A quarter each leaves the
    /// graph half the glass — the moving part earns the most area.
    story_h: i32,
}

fn frame(theme: &Theme, size: u32) -> Frame {
    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let strip_h = ((size as f32) * 0.20).round().max(9.0) as i32;
    let well_w = (size as i32 - margin * 2).max(0);
    let well_h = (size as i32 - margin * 2 - strip_h).max(0);
    let inset = t + 1;
    Frame {
        well_x: margin,
        well_y: margin,
        well_w: well_w as u32,
        well_h: well_h as u32,
        strip_y: margin + well_h,
        strip_h: strip_h as u32,
        glass_x: margin + inset,
        glass_y: margin + inset,
        glass_w: (well_w - inset * 2).max(0) as u32,
        glass_h: (well_h - inset * 2).max(0) as u32,
        story_h: ((well_h - inset * 2).max(0) as f32 * 0.25).round().max(6.0) as i32,
    }
}

/// Renders the full instrument tile: download lane on top, upload lane
/// on the bottom, mirrored graph between them, interface name on the
/// tile-face strip. For the no-interface case callers use
/// [`panel::render_dead_tile`] instead — a dead screen is the SDK's
/// empty state, not this renderer's.
pub fn render_nettraffic_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    interface_name: &str,
    down: &TrafficLane,
    up: &TrafficLane,
) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero nettraffic tile size");
    // Text needs a real, registered family for cosmic-text to resolve;
    // the theme's own menu face is the one family guaranteed present.
    let label_family = theme.menu.item_font.family.clone();

    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    let f = frame(theme, size);
    let (gx, gy, gw, gh) = panel::draw_panel_glass(&mut pixmap, f.well_x, f.well_y, f.well_w, f.well_h, theme);
    debug_assert_eq!((gx, gy, gw, gh), (f.glass_x, f.glass_y, f.glass_w, f.glass_h), "frame() must mirror draw_panel_glass's inset");
    let pal = panel::panel_palette(theme);

    let graph_h = (gh as i32 - f.story_h * 2).max(0);
    let glass_w = gw as i32;
    let mut text = TextCtx { font_system: &mut *font_system, swash_cache: &mut *swash_cache, family: &label_family };
    draw_story(&mut pixmap, &mut text, Band { x: gx, y: gy, w: glass_w, h: f.story_h }, &pal, down, true);
    draw_graph(&mut pixmap, Band { x: gx, y: gy + f.story_h, w: glass_w, h: graph_h }, &pal, down, up);
    draw_story(&mut pixmap, &mut text, Band { x: gx, y: gy + f.story_h + graph_h, w: glass_w, h: f.story_h }, &pal, up, false);

    // Off the glass: the interface name is a label, not a reading, so
    // it letters the tile face in the family's shared ink. Modern
    // predictable names (enp0s31f6, wlp0s20f3) run 8-9 characters
    // where the classics ran 4-5, and `paint::draw_text` wraps at its
    // box edge — a wrapped tail would land below the strip — so the
    // fit is earned deliberately: shrink the face toward a floor until
    // the measured line fits, then clip whole characters off the tail.
    // Interface names differentiate at the front (enp0s31f6 versus
    // wlp0s20f3), so a clipped head stays identifiable where a wrapped
    // or overflowing one reads as a rendering bug.
    let mut name = interface_name.to_uppercase();
    let mut name_font = FontSpec {
        family: label_family,
        size: (f.strip_h as f32 * 0.68).max(6.0),
        weight: FontWeight::Bold,
        style: FontStyle::Normal,
    };
    let floor = (f.strip_h as f32 * 0.52).max(6.0).min(name_font.size);
    while paint::text_width(font_system, &name_font, &name) > f.well_w && name_font.size > floor {
        name_font.size = (name_font.size - 0.5).max(floor);
    }
    while paint::text_width(font_system, &name_font, &name) > f.well_w && !name.is_empty() {
        name.pop();
    }
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &name,
        &name_font,
        tile::tile_ink(theme),
        f.well_x,
        f.strip_y,
        f.well_w,
        f.strip_h,
        TextAlign::Left,
    );

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// A rectangular slice of the glass in device pixels. Every band
/// helper takes one by value: the geometry travels as a single word,
/// the loose x/y/w/h quartets that tripped clippy's argument budget on
/// every helper go away, and "which region may this helper touch" is
/// one visible value at each call site instead of four.
#[derive(Clone, Copy, Debug)]
struct Band {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// The cosmic-text machinery plus the one registered family every
/// label uses, bundled for the same signature-budget reason as
/// [`Band`] — and so the two story calls cannot drift apart on which
/// face they letter in.
struct TextCtx<'a> {
    font_system: &'a mut cosmic_text::FontSystem,
    swash_cache: &'a mut cosmic_text::SwashCache,
    family: &'a str,
}

/// One lane's readout band: `[chevron][digits][unit]`, with the digits
/// right-aligned against the unit letter so the cluster reads as one
/// meter and the chevron holds the left edge. The same band serves
/// both stories — only the chevron's direction differs — which is what
/// keeps the instrument's top/bottom symmetry exact.
fn draw_story(pixmap: &mut Pixmap, text: &mut TextCtx, band: Band, pal: &PanelPalette, lane: &TrafficLane, down: bool) {
    if band.w <= 0 || band.h <= 0 {
        return;
    }
    let pad = ((band.w as f32) * 0.04).max(1.0) as i32;
    let chev_w = ((band.w as f32) * 0.12).max(4.0) as i32;
    let unit_w = ((band.w as f32) * 0.18).max(7.0) as i32;
    // Cap the digit cluster's width against the row height: LED digits
    // wider than ~three quarters of their height stop reading as
    // seven-segment and start reading as blobs, and on a wide glass
    // the uncapped remainder would produce exactly that.
    let avail = (band.w - pad * 2 - chev_w - unit_w).max(1);
    let digits_w = avail.min((band.h as f32 * 2.4) as i32).max(1);
    let digits_x = band.x + band.w - pad - unit_w - digits_w;

    draw_chevron(pixmap, Band { x: band.x + pad, y: band.y, w: chev_w, h: band.h }, pal, lane.now > 0, down);
    panel::draw_led_digits(pixmap, digits_x, band.y, digits_w as u32, band.h as u32, pal, &lane.readout.digits);

    let font = FontSpec {
        family: text.family.to_string(),
        size: (band.h as f32 * 0.85).max(6.0),
        weight: FontWeight::Bold,
        style: FontStyle::Normal,
    };
    // The unit is a label on the glass, so it takes dim ink — the
    // digits are the reading and keep full brightness.
    paint::draw_text(
        pixmap,
        text.font_system,
        text.swash_cache,
        lane.readout.unit.letter(),
        &font,
        pal.ink_dim,
        band.x + band.w - pad - unit_w,
        band.y,
        unit_w as u32,
        band.h as u32,
        TextAlign::Center,
    );
}

/// The direction chevron: three stepped, centered slabs — hard-edged
/// LED drawing, not vector art — pointing down into the machine for
/// the download story, up and out for the upload one. It lights while
/// its direction carries traffic and falls back to a ghost when idle,
/// so at a glance the pair answers "which way is data moving right
/// now" before the digits are even read.
fn draw_chevron(pixmap: &mut Pixmap, band: Band, pal: &PanelPalette, active: bool, down: bool) {
    // Odd base width so every step centers on the same pixel column;
    // even step decrements keep that center as the steps narrow.
    let base = {
        let b = band.w.min(band.h).max(3);
        if b % 2 == 0 {
            b - 1
        } else {
            b
        }
    };
    let dec = (base / 4).max(1) * 2;
    let step_h = (band.h / 6).max(1);
    let top = band.y + (band.h - step_h * 3) / 2;
    let cx = band.x + band.w / 2;
    let color = if active { pal.ink } else { pal.ghost };
    for i in 0..3i32 {
        let bw = (base - dec * i).max(1);
        let sy = if down { top + step_h * i } else { top + step_h * (2 - i) };
        paint::fill_rect(pixmap, cx - bw / 2, sy, bw as u32, step_h as u32, color);
    }
}

/// The middle band: history matrix left, NOW meter on the right edge.
/// The meter deliberately sits where the newest history column scrolls
/// in, one glass-colored gap away — current value and history share a
/// baseline, a scale, and a row grid, so the eye reads the meter as
/// the live head of the graph rather than a separate gauge.
fn draw_graph(pixmap: &mut Pixmap, band: Band, pal: &PanelPalette, down: &TrafficLane, up: &TrafficLane) {
    if band.w <= 0 || band.h <= 0 {
        return;
    }
    let pad = ((band.w as f32) * 0.04).max(1.0) as i32;
    let now_w = ((band.w as f32) * 0.10).max(4.0) as i32;
    let now_gap = ((band.w as f32) * 0.05).max(2.0) as i32;
    let hist_w = (band.w - pad * 2 - now_w - now_gap).max(0);
    panel::draw_led_matrix(pixmap, band.x + pad, band.y, hist_w as u32, band.h as u32, pal, NET_TRAFFIC_HALF_ROWS, down.history, up.history);
    let meter = Band { x: band.x + pad + hist_w + now_gap, y: band.y, w: now_w, h: band.h };
    draw_now_meter(pixmap, meter, pal, down.now, up.now);
}

/// The NOW meter: one wide segmented column, mirrored around the same
/// center baseline as the matrix. Its slabs sit on the exact
/// [`panel::mirrored_row_edges`] grid the matrix's dots do — same
/// seam, same cells, elements centered per cell — so the meter can
/// never disagree with the history about where a level's row lives,
/// and its halves mirror as exactly as the matrix's (deriving each
/// row's y independently let rounding drift the lower half a pixel,
/// the defect design review caught in the matrix). Full-width slabs
/// instead of centered dots because at 56px a single dot column would
/// read as a dotted line, not a gauge.
fn draw_now_meter(pixmap: &mut Pixmap, band: Band, pal: &PanelPalette, down_lit: u32, up_lit: u32) {
    if band.w <= 0 || band.h <= 0 {
        return;
    }
    let half = NET_TRAFFIC_HALF_ROWS;
    let cell = band.h as f32 / (half * 2) as f32;
    let gap = (cell * 0.3).clamp(1.0, cell * 0.5);
    let slab_h = ((cell - gap).round() as i32).max(1);
    let ink = solid(pal.ink);
    let ghost = solid(pal.ghost);
    let (down_lit, up_lit) = (down_lit.min(half), up_lit.min(half));
    for k in 0..half {
        let (ty, by) = panel::mirrored_row_edges(band.y, band.h as u32, half, k, slab_h);
        frect(pixmap, band.x as f32, ty as f32, band.w as f32, slab_h as f32, if k < down_lit { &ink } else { &ghost });
        frect(pixmap, band.x as f32, by as f32, band.w as f32, slab_h as f32, if k < up_lit { &ink } else { &ghost });
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::{all_themes, nextstep_classic};

    fn render(theme: &Theme, size: u32, name: &str, down: &TrafficLane, up: &TrafficLane) -> DecorationBuffer {
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        render_nettraffic_tile(theme, &mut font_system, &mut swash_cache, size, name, down, up)
    }

    fn lane<'a>(bps: f32, now: u32, history: &'a [u32]) -> TrafficLane<'a> {
        TrafficLane { readout: format_rate(bps), now, history }
    }

    const IDLE: [u32; NET_TRAFFIC_COLUMNS] = [0; NET_TRAFFIC_COLUMNS];
    const FULL: [u32; NET_TRAFFIC_COLUMNS] = [NET_TRAFFIC_HALF_ROWS; NET_TRAFFIC_COLUMNS];
    const WAVE: [u32; NET_TRAFFIC_COLUMNS] = [1, 1, 2, 3, 4, 3, 2, 2, 3, 4, 4, 3, 2, 3, 4, 4];

    #[test]
    fn buffers_come_out_at_the_requested_size() {
        let theme = nextstep_classic();
        for size in [16u32, 56, 112] {
            let buffer = render(&theme, size, "eth0", &lane(84.0 * 1024.0 * 1024.0, 4, &WAVE), &lane(38.0 * 1024.0, 1, &IDLE));
            assert_eq!((buffer.width, buffer.height), (size, size));
            assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn every_theme_renders_at_both_dock_scales() {
        for theme in all_themes() {
            for size in [56u32, 112] {
                let buffer = render(&theme, size, "wlan0", &lane(12.5 * 1024.0 * 1024.0, 3, &WAVE), &lane(420.0 * 1024.0, 2, &WAVE));
                assert_eq!((buffer.width, buffer.height), (size, size), "theme {}", theme.id);
            }
        }
    }

    #[test]
    fn the_representative_states_all_render_distinctly() {
        let theme = nextstep_classic();
        let idle = render(&theme, 56, "eth0", &lane(0.0, 0, &IDLE), &lane(0.0, 0, &IDLE));
        let dl = render(&theme, 56, "eth0", &lane(84.0 * 1024.0 * 1024.0, 4, &WAVE), &lane(38.0 * 1024.0, 1, &IDLE));
        let ul = render(&theme, 56, "eth0", &lane(38.0 * 1024.0, 1, &IDLE), &lane(84.0 * 1024.0 * 1024.0, 4, &WAVE));
        let sat = render(&theme, 56, "eth0", &lane(118.0 * 1024.0 * 1024.0, 4, &FULL), &lane(97.0 * 1024.0 * 1024.0, 4, &FULL));
        let states = [("idle", &idle), ("download", &dl), ("upload", &ul), ("saturated", &sat)];
        for (i, (name_a, a)) in states.iter().enumerate() {
            for (name_b, b) in states.iter().skip(i + 1) {
                assert_ne!(a.pixels, b.pixels, "{name_a} and {name_b} must not render identically");
            }
        }
    }

    /// The core design claim: direction is position. A download-only
    /// load lights full-brightness ink strictly in the top half of the
    /// glass and none in the bottom half, and an upload-only load
    /// mirrors that. Digits are blanked and rates chosen so the only
    /// full-ink sources are the matrix, the meter, and the chevron —
    /// all of which must respect the split. Exact channel matching is
    /// sound because the buffer is premultiplied-opaque.
    #[test]
    fn download_lights_only_the_top_half_and_upload_only_the_bottom() {
        let theme = nextstep_classic();
        let size = 112u32;
        let pal = panel::panel_palette(&theme);
        let blank = RateReadout { digits: [None; 3], unit: RateUnit::Kilo };
        let busy = TrafficLane { readout: blank, now: NET_TRAFFIC_HALF_ROWS, history: &FULL };
        let quiet = TrafficLane { readout: blank, now: 0, history: &IDLE };

        let f = frame(&theme, size);
        let mid = f.glass_y + f.glass_h as i32 / 2;
        let ink_in = |buffer: &DecorationBuffer, y0: i32, y1: i32| -> usize {
            let mut count = 0;
            for y in y0..y1 {
                for x in f.glass_x..f.glass_x + f.glass_w as i32 {
                    let i = ((y as u32 * size + x as u32) * 4) as usize;
                    if (buffer.pixels[i], buffer.pixels[i + 1], buffer.pixels[i + 2]) == (pal.ink.r, pal.ink.g, pal.ink.b) {
                        count += 1;
                    }
                }
            }
            count
        };

        let dl = render(&theme, size, "eth0", &busy, &quiet);
        assert!(ink_in(&dl, f.glass_y, mid) > 50, "download load must light the top half");
        assert_eq!(ink_in(&dl, mid, f.glass_y + f.glass_h as i32), 0, "download load must leave the bottom half dark");

        let ul = render(&theme, size, "eth0", &quiet, &busy);
        assert_eq!(ink_in(&ul, f.glass_y, mid), 0, "upload load must leave the top half dark");
        assert!(ink_in(&ul, mid, f.glass_y + f.glass_h as i32) > 50, "upload load must light the bottom half");
    }

    /// The instrument's core premise, pinned to the pixel at both dock
    /// scales: a download-only load and the equal upload-only load
    /// must render graph bands that are exact vertical reflections
    /// around the seam. Digits are blanked so the only asymmetric
    /// glass content (glyphs) stays out of the compared region; the
    /// unit letters match between the two renders and sit outside the
    /// graph band anyway.
    #[test]
    fn the_graph_band_mirrors_exactly_between_directions() {
        let theme = nextstep_classic();
        let blank = RateReadout { digits: [None; 3], unit: RateUnit::Kilo };
        let busy = TrafficLane { readout: blank, now: 3, history: &WAVE };
        let quiet = TrafficLane { readout: blank, now: 0, history: &IDLE };
        for size in [56u32, 112] {
            let dl = render(&theme, size, "eth0", &busy, &quiet);
            let ul = render(&theme, size, "eth0", &quiet, &busy);
            let f = frame(&theme, size);
            let graph_y = f.glass_y + f.story_h;
            let graph_h = (f.glass_h as i32 - f.story_h * 2).max(0);
            let seam = graph_y + graph_h / 2;
            let row = |buf: &DecorationBuffer, y: i32| -> Vec<u8> {
                let start = ((y as u32 * size + f.glass_x as u32) * 4) as usize;
                buf.pixels[start..start + (f.glass_w * 4) as usize].to_vec()
            };
            for dy in 0..graph_h / 2 {
                assert_eq!(
                    row(&dl, seam - 1 - dy),
                    row(&ul, seam + dy),
                    "size {size}: download row {dy} above the seam must mirror the equal upload row below it"
                );
            }
        }
    }

    /// Modern predictable names run 8-9 characters; the strip must
    /// absorb them by shrinking or clipping, never by wrapping a tail
    /// below the strip. Diffing against an empty-name render catches
    /// every touched pixel, anti-aliased edges included.
    #[test]
    fn long_modern_interface_names_stay_inside_the_strip() {
        let theme = nextstep_classic();
        let quiet = lane(0.0, 0, &IDLE);
        for size in [56u32, 112] {
            let named = render(&theme, size, "enp0s31f6", &quiet, &quiet);
            let blank = render(&theme, size, "", &quiet, &quiet);
            let f = frame(&theme, size);
            let mut touched = 0usize;
            for (i, (a, b)) in named.pixels.iter().zip(&blank.pixels).enumerate() {
                if a != b {
                    touched += 1;
                    let row = i as u32 / 4 / size;
                    assert!(
                        (f.strip_y..f.strip_y + f.strip_h as i32).contains(&(row as i32)),
                        "size {size}: the name may only letter the strip, but touched row {row}"
                    );
                }
            }
            assert!(touched > 0, "size {size}: the long name must actually be drawn");
        }
    }

    #[test]
    fn the_now_meter_alone_changes_the_face() {
        let theme = nextstep_classic();
        let blank = RateReadout { digits: [None; 3], unit: RateUnit::Kilo };
        // Chevron activity keys off `now` too, so this exercises both
        // live-now indicators at once; history and digits stay fixed.
        let a = TrafficLane { readout: blank, now: 0, history: &IDLE };
        let b = TrafficLane { readout: blank, now: NET_TRAFFIC_HALF_ROWS, history: &IDLE };
        let quiet = TrafficLane { readout: blank, now: 0, history: &IDLE };
        let off = render(&theme, 56, "eth0", &a, &quiet);
        let on = render(&theme, 56, "eth0", &b, &quiet);
        assert_ne!(off.pixels, on.pixels);
    }

    #[test]
    fn each_lane_shows_its_own_unit_letter() {
        let theme = nextstep_classic();
        let kilo = TrafficLane { readout: RateReadout { digits: [None, Some(4), Some(2)], unit: RateUnit::Kilo }, now: 1, history: &IDLE };
        let giga = TrafficLane { readout: RateReadout { digits: [None, Some(4), Some(2)], unit: RateUnit::Giga }, now: 1, history: &IDLE };
        let quiet = lane(0.0, 0, &IDLE);
        let a = render(&theme, 56, "eth0", &kilo, &quiet);
        let b = render(&theme, 56, "eth0", &giga, &quiet);
        assert_ne!(a.pixels, b.pixels, "the unit letter must be drawn per lane");
    }

    #[test]
    fn different_interface_names_render_differently() {
        let theme = nextstep_classic();
        let quiet = lane(0.0, 0, &IDLE);
        let a = render(&theme, 56, "eth0", &quiet, &quiet);
        let b = render(&theme, 56, "wlan0", &quiet, &quiet);
        assert_ne!(a.pixels, b.pixels);
    }

    #[test]
    fn format_rate_folds_rates_into_blank_padded_digits_and_escalating_units() {
        let k = 1024.0f32;
        assert_eq!(format_rate(0.0), RateReadout { digits: [None, None, Some(0)], unit: RateUnit::Kilo });
        assert_eq!(format_rate(300.0), RateReadout { digits: [None, None, Some(0)], unit: RateUnit::Kilo });
        assert_eq!(format_rate(42.0 * k), RateReadout { digits: [None, Some(4), Some(2)], unit: RateUnit::Kilo });
        assert_eq!(format_rate(300.0 * k), RateReadout { digits: [Some(3), Some(0), Some(0)], unit: RateUnit::Kilo });
        // 1023K rounds past 999, so it must escalate to 1M rather than
        // show a phantom fourth digit.
        assert_eq!(format_rate(1023.0 * k), RateReadout { digits: [None, None, Some(1)], unit: RateUnit::Mega });
        assert_eq!(format_rate(5.0 * k * k), RateReadout { digits: [None, None, Some(5)], unit: RateUnit::Mega });
        assert_eq!(format_rate(3.0 * k * k * k), RateReadout { digits: [None, None, Some(3)], unit: RateUnit::Giga });
        // Beyond the top unit the mantissa pins rather than overflows.
        assert_eq!(format_rate(1e15), RateReadout { digits: [Some(9), Some(9), Some(9)], unit: RateUnit::Giga });
    }

    #[test]
    fn quantize_level_keeps_trickles_visible_and_zero_dark() {
        assert_eq!(quantize_level(0.0, 4), 0);
        assert_eq!(quantize_level(-1.0, 4), 0);
        assert_eq!(quantize_level(1e-4, 4), 1, "any traffic at all must light one cell");
        assert_eq!(quantize_level(0.25, 4), 1);
        assert_eq!(quantize_level(0.26, 4), 2);
        assert_eq!(quantize_level(1.0, 4), 4);
        assert_eq!(quantize_level(5.0, 4), 4, "over-unity input clamps to the top cell");
        assert_eq!(quantize_level(0.5, 0), 0);
    }
}
