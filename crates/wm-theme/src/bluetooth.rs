//! The Bluetooth instrument: adapter power, connection count and the
//! known-device panel, rendered on the [`crate::panel`] LED screen. A
//! sibling of [`crate::wifi`], and it keeps that instrument's layout
//! grammar so the radios read as one family on the dock: the glass
//! well takes the upper region of the tile, a strip of tile-face
//! lettering sits below it, and everything on the glass is drawn in
//! [`crate::panel::PanelPalette`] colors only.
//!
//! The glass splits into two rows. The upper row is the LED readout —
//! the connected-device count as seven-segment digits when anything is
//! connected, the full ghost pattern otherwise (a powered instrument
//! with nothing to say). The lower row is the Bluetooth rune drawn as
//! a hard-edged LED dot matrix: full ink while the adapter is powered,
//! ghost when it is off — the radio's own mark doubling as its power
//! lamp, the way the wired instrument's link lamp does.
//!
//! The tile-face strip carries the state in lettering caps: the first
//! connected device's name when connected, `READY` when powered and
//! idle, `OFF` when the adapter is down — lit ink for a powered
//! adapter, dim for one that is off, so the whole tile recedes
//! together. The no-adapter-at-all case is not this module's job:
//! widgets show [`crate::panel::render_dead_tile`] with `BT` for that,
//! exactly as the link instrument does for a machine with no NIC.
//!
//! The panel renderer ([`render_bt_panel`]) draws the unfolded detail
//! view's *content*: one glass field of rows — adapter power, each
//! known device with its connection lamp and forget cell, the
//! pair-new action. Row semantics (which row is which device, what a
//! click means) belong to the widget's `bt_panel` module in
//! `chonk-instruments`; this side only turns [`BtPanelRow`] values
//! into pixels, so the same inputs always produce the same pixels and
//! the hit-test geometry ([`panel_row_height`], [`forget_cell_width`])
//! is defined once, here, beside the drawing it must match.

use tiny_skia::Pixmap;
use wm_theme_api::DecorationBuffer;

use crate::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::panel::{self, PanelPalette};
use crate::tile;
use crate::Theme;

/// One reading of the Bluetooth adapter, already reduced to plain
/// values by the sampling side: the renderer never touches the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtReading<'a> {
    /// Powered, with at least one device connected. `name` is the
    /// first connected device's name, for the lettering strip.
    Connected { count: u8, name: &'a str },
    /// Powered, nothing connected.
    Idle,
    /// The adapter exists but is powered down (rfkill-blocked or
    /// switched off) — or its daemon is not answering, which the
    /// widget deliberately folds to the same face: either way the
    /// radio is not on, and a click's only honest offer is to try
    /// turning it on.
    Off,
}

/// The Bluetooth rune as a hard-edged LED dot grid: the Hagall +
/// Berkanan bind rune — a vertical stem, the two right-hand loops, and
/// the two left arms crossing at the waist — authored on a cell grid
/// so it stays the same angular mark at every tile size instead of
/// picking up antialiased curves the LED idiom does not have.
///
/// # Why 7x9 and not the finer grid it started on
///
/// This was a 9x13 grid, and it was illegible at the size the tile
/// actually ships at. The arithmetic is the whole argument: at the
/// stock 56px tile the glass is about 36px tall, and a 13-row grid
/// gets 36/13 = 2.7px per cell, of which the dot is 78% — a 2px
/// speckle. Rendered, it read as a scatter of noise rather than as a
/// mark, and the *height* was always what bound it, since the rune is
/// taller than it is wide while the glass is wider than it is tall.
///
/// Nine rows gives 4px cells at the same tile, which is the same
/// order as the link instrument's signal stairs — five chunky
/// elements — and that is the legibility budget this glass has. The
/// mark is the same one: the loops still go out to the right and
/// return, the arms still cross at the waist, and it still mirrors
/// across that waist exactly.
/// The grid is trimmed to the mark's own extent — every column and
/// every row here lights somewhere. An empty border column costs real
/// size, because the cell is the smaller of `width/cols` and
/// `height/rows`: the two dead columns this grid used to carry made
/// every cell 29% narrower than the glass could have afforded.
const RUNE: [&str; 9] = [
    "..#..",
    "..##.",
    "#.#.#",
    ".###.",
    "..#..",
    ".###.",
    "#.#.#",
    "..##.",
    "..#..",
];

/// Connected-device count as a two-position readout, with leading
/// zeros blanked the way every other instrument blanks them: `3` reads
/// `_3`, never `03`. Counts past 99 clamp.
///
/// Two positions rather than the link tile's three, and that is a
/// difference in the *reading*, not a departure from the family: a
/// signal percentage genuinely needs three digits to say 100, while a
/// controller that grants seven simultaneous connections will never
/// need a hundreds column. The position bought back is what lets the
/// rune beside it be drawn at a size someone can recognize.
pub fn count_digits(count: u8) -> [Option<u8>; 2] {
    let c = count.min(99);
    if c >= 10 {
        [Some(c / 10), Some(c % 10)]
    } else {
        [None, Some(c)]
    }
}

/// Draws the rune into `(x, y, w, h)`, centered, every lit cell as a
/// square dot in `color`. Public so the pairing dialog can wear the
/// same mark; the tile calls it with ink or ghost by power state.
pub fn draw_bt_rune(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, color: Color) {
    if w == 0 || h == 0 {
        return;
    }
    let cols = RUNE[0].len() as u32;
    let rows = RUNE.len() as u32;
    // The cell grid keeps the rune's aspect; the dot inside each cell
    // is sized like `panel::draw_led_columns`' dots so the rune reads
    // as the same LED hardware as every other readout.
    let cell = (w as f32 / cols as f32).min(h as f32 / rows as f32);
    // A fuller cell than the bar meters' dots, deliberately. A signal
    // stair is a row of *separate* readings and wants the gap between
    // them; this is one continuous glyph, and most of its cells are
    // diagonal neighbours — at the meters' 0.7 the strokes broke into
    // unrelated speckles and the mark stopped being recognizable.
    // Just short of touching keeps the LED grid visible in the glyph
    // without letting the strokes come apart.
    let dot = (cell * 0.92).max(1.0);
    let x0 = x as f32 + (w as f32 - cell * cols as f32) / 2.0;
    let y0 = y as f32 + (h as f32 - cell * rows as f32) / 2.0;
    for (row, line) in RUNE.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            if ch != b'#' {
                continue;
            }
            let cx = x0 + col as f32 * cell + (cell - dot) / 2.0;
            let cy = y0 + row as f32 * cell + (cell - dot) / 2.0;
            paint::fill_rect(pixmap, cx.round() as i32, cy.round() as i32, dot.round().max(1.0) as u32, dot.round().max(1.0) as u32, color);
        }
    }
}

/// Renders the Bluetooth instrument at `size` x `size`. Pure:
/// everything it needs arrives in `reading`, so the same inputs always
/// produce the same pixels.
pub fn render_bluetooth_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    reading: &BtReading,
) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero bluetooth tile size");
    // Glass lettering needs a real, registered family for cosmic-text
    // to resolve (see netload.rs); the theme's menu face is the
    // confirmed-available one.
    let family = theme.menu.item_font.family.clone();

    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    // The netload/wifi margin and strip recipe verbatim, so the radios
    // line up shelf-for-shelf in the dock's column.
    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let strip_h = ((size as f32) * 0.20).round().max(9.0) as i32;
    let well_w = (size as i32 - margin * 2).max(0) as u32;
    let well_h = (size as i32 - margin * 2 - strip_h).max(0) as u32;
    let (gx, gy, gw, gh) = panel::draw_panel_glass(&mut pixmap, margin, margin, well_w, well_h, theme);
    let pal = panel::panel_palette(theme);

    // Digits left, rune right, both the full height of the glass —
    // the link tile's "reading, then its unit mark" composition, with
    // the radio's own mark standing in for the percent sign.
    //
    // Side by side rather than stacked, and that is the fix for a real
    // defect rather than a preference: stacked, the rune got a wide,
    // short band, and a mark that is taller than it is wide can only
    // ever be as big as that band's *height* allows. Half the glass
    // width and all of its height is the shape the rune actually
    // wants, and it is the difference between a 4px cell and a 2px one
    // at the tile size this ships at. See [`RUNE`].
    let digits_w = gw / 2;
    let rune_w = gw - digits_w;
    let rune_x = gx + digits_w as i32;

    let digits = match reading {
        BtReading::Connected { count, .. } => count_digits(*count),
        BtReading::Idle | BtReading::Off => [None, None],
    };
    panel::draw_led_digits(&mut pixmap, gx, gy, digits_w, gh, &pal, &digits);

    let powered = !matches!(reading, BtReading::Off);
    draw_bt_rune(&mut pixmap, rune_x, gy, rune_w, gh, if powered { pal.ink } else { pal.ghost });

    // The lettering strip: state in caps, lit with the radio.
    let (label, lit) = match reading {
        BtReading::Connected { name, .. } => (*name, true),
        BtReading::Idle => ("READY", true),
        BtReading::Off => ("OFF", false),
    };
    draw_label_strip(&mut pixmap, theme, font_system, swash_cache, &family, margin, margin + well_h as i32, well_w, strip_h as u32, label, lit);

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// The tile-face lettering under the well: the state word or device
/// name, lit ink for a powered adapter, dim for one that is off. A
/// name too wide for the strip first drops one lettering size (device
/// names run long, like SSIDs), then truncates by shaped measurement —
/// the wifi strip's recipe without the mode column.
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
    lit: bool,
) {
    let ink = tile::tile_ink(theme);
    let dim = tile::tile_ink_dim(theme);
    let color = if lit { ink } else { dim };
    let mut font = FontSpec { family: family.to_string(), size: (h as f32 * 0.68).max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal };
    let mut label = name.to_uppercase();
    if paint::text_width(font_system, &font, &label) > w {
        font.size = (h as f32 * 0.50).max(6.0);
    }
    while !label.is_empty() && paint::text_width(font_system, &font, &label) > w {
        label.pop();
    }
    paint::draw_text(pixmap, font_system, swash_cache, &label, &font, color, x, y, w, h, TextAlign::Left);
}

// ---------------------------------------------------------------------
// The unfolded panel's content.

/// One row of the Bluetooth panel, already reduced to plain values.
/// The widget's `bt_panel` module owns which rows exist and what a
/// click on each means; this enum is only what a row *looks like*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtPanelRow<'a> {
    /// The adapter power line: lamp, `BLUETOOTH` lettering, `ON`/`OFF`
    /// readout on the right.
    Power { on: bool },
    /// One known device. `connected` lights its lamp; `pending` dims
    /// the name and appends an ellipsis (a connect or disconnect in
    /// flight — a request, not a fact); `armed` inverts the forget
    /// cell, which is the two-click confirm's visible half.
    Device { name: &'a str, connected: bool, pending: bool, armed: bool },
    /// The action row that spawns the pairing dialog.
    PairNew,
    /// The whole panel's answer on a machine with no adapter.
    NoAdapter,
}

/// Panel content width for a dock whose tiles are `tile` pixels —
/// five tiles wide, which fits a device name, its lamp and the forget
/// cell without crowding, and scales with the dock like every other
/// chrome metric.
pub fn panel_content_width(tile: u32) -> u32 {
    tile * 5
}

/// Panel row height for a dock whose tiles are `tile` pixels: 24px at
/// the stock 56px tile.
pub fn panel_row_height(tile: u32) -> u32 {
    ((tile * 3) / 7).max(12)
}

/// Width of the forget cell at a row's right edge — the `[x]` hitbox.
/// Geometry lives here, beside the drawing, so the widget's hit-test
/// and the renderer cannot disagree about where the cell is.
pub fn forget_cell_width(row_h: u32) -> u32 {
    row_h
}

/// The panel content height that fits `rows` rows — what a widget
/// *requests*; the granted height may be smaller on a crowded screen,
/// which is why [`render_bt_panel`] takes the granted height
/// explicitly instead of deriving one.
pub fn panel_content_height(row_h: u32, rows: usize) -> u32 {
    (row_h * rows.max(1) as u32).max(row_h)
}

/// Renders the panel content: `rows` stacked top to bottom on one
/// glass field, each `row_h` tall, into exactly `width` x `height` —
/// the *granted* size, which on a crowded screen can be smaller than
/// the size the rows asked for. Rows that do not fit are clipped by
/// the pixmap edge rather than squeezed, and a grant taller than the
/// rows shows bare glass below them — an instrument with empty screen,
/// not a hole. Pure, like the tile.
pub fn render_bt_panel(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    width: u32,
    height: u32,
    row_h: u32,
    rows: &[BtPanelRow],
) -> DecorationBuffer {
    let width = width.max(8);
    let height = height.max(8);
    let row_h = row_h.max(8);
    let mut pixmap = Pixmap::new(width, height).expect("nonzero bluetooth panel size");
    let pal = panel::panel_palette(theme);
    let family = theme.menu.item_font.family.clone();
    paint::fill_rect(&mut pixmap, 0, 0, width, height, pal.glass);

    for (index, row) in rows.iter().enumerate() {
        let y = (index as u32 * row_h) as i32;
        if y >= height as i32 {
            break;
        }
        draw_panel_row(&mut pixmap, font_system, swash_cache, &family, &pal, y, width, row_h, row);
        // A hairline between rows, shaded down rather than inked, so
        // the rows read as one instrument's face and not a list box.
        if index + 1 < rows.len() {
            paint::op_rect(&mut pixmap, 0, y + row_h as i32 - 1, width, 1, -14);
        }
    }

    DecorationBuffer { width, height, pixels: pixmap.data().to_vec() }
}

#[allow(clippy::too_many_arguments)]
fn draw_panel_row(
    pixmap: &mut Pixmap,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    family: &str,
    pal: &PanelPalette,
    y: i32,
    width: u32,
    row_h: u32,
    row: &BtPanelRow,
) {
    let font = FontSpec { family: family.to_string(), size: (row_h as f32 * 0.50).max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal };
    let pad = (row_h / 4) as i32;
    let lamp = (row_h as f32 * 0.34).round().max(3.0) as u32;
    let lamp_y = y + (row_h.saturating_sub(lamp) / 2) as i32;
    let text_x = pad + lamp as i32 + pad;

    match row {
        BtPanelRow::Power { on } => {
            paint::fill_rect(pixmap, pad, lamp_y, lamp, lamp, if *on { pal.ink } else { pal.ghost });
            let color = if *on { pal.ink } else { pal.ink_dim };
            paint::draw_text(pixmap, font_system, swash_cache, "BLUETOOTH", &font, color, text_x, y, width.saturating_sub(text_x as u32), row_h, TextAlign::Left);
            let state_w = row_h * 2;
            let state = if *on { "ON" } else { "OFF" };
            paint::draw_text(pixmap, font_system, swash_cache, state, &font, color, (width - state_w) as i32 - pad, y, state_w, row_h, TextAlign::Right);
        }
        BtPanelRow::Device { name, connected, pending, armed } => {
            paint::fill_rect(pixmap, pad, lamp_y, lamp, lamp, if *connected { pal.ink } else { pal.ghost });
            let color = if *pending {
                pal.ink_dim
            } else if *connected {
                pal.ink
            } else {
                pal.ink_dim
            };
            let cell = forget_cell_width(row_h);
            let name_w = width.saturating_sub(text_x as u32 + cell + pad as u32);
            let mut label = name.to_uppercase();
            if *pending {
                label.push_str("...");
            }
            while !label.is_empty() && paint::text_width(font_system, &font, &label) > name_w {
                label.pop();
            }
            paint::draw_text(pixmap, font_system, swash_cache, &label, &font, color, text_x, y, name_w, row_h, TextAlign::Left);
            draw_forget_cell(pixmap, font_system, swash_cache, family, pal, (width - cell) as i32, y, cell, row_h, *armed);
        }
        BtPanelRow::PairNew => {
            paint::draw_text(pixmap, font_system, swash_cache, "+ PAIR NEW...", &font, pal.ink_dim, text_x, y, width.saturating_sub(text_x as u32), row_h, TextAlign::Left);
        }
        BtPanelRow::NoAdapter => {
            paint::draw_text(pixmap, font_system, swash_cache, "NO ADAPTER", &font, pal.ink_dim, 0, y, width, row_h, TextAlign::Center);
        }
    }
}

/// The forget cell: a small `x` mark in its own square at the row's
/// right edge. Unarmed it is a ghost-outline whisper; armed (first
/// click landed, confirm pending) it inverts — full ink cell, glass
/// mark — which is the panel grammar's honest "are you sure".
#[allow(clippy::too_many_arguments)]
fn draw_forget_cell(
    pixmap: &mut Pixmap,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    family: &str,
    pal: &PanelPalette,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    armed: bool,
) {
    let inset = (h / 5) as i32;
    let bw = w.saturating_sub(inset as u32 * 2);
    let bh = h.saturating_sub(inset as u32 * 2);
    let font = FontSpec { family: family.to_string(), size: (h as f32 * 0.42).max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal };
    if armed {
        paint::fill_rect(pixmap, x + inset, y + inset, bw, bh, pal.ink);
        paint::draw_text(pixmap, font_system, swash_cache, "X", &font, pal.glass, x, y, w, h, TextAlign::Center);
    } else {
        // Outline only: four one-pixel ghost edges.
        paint::fill_rect(pixmap, x + inset, y + inset, bw, 1, pal.ghost);
        paint::fill_rect(pixmap, x + inset, y + inset + bh as i32 - 1, bw, 1, pal.ghost);
        paint::fill_rect(pixmap, x + inset, y + inset, 1, bh, pal.ghost);
        paint::fill_rect(pixmap, x + inset + bw as i32 - 1, y + inset, 1, bh, pal.ghost);
        paint::draw_text(pixmap, font_system, swash_cache, "X", &font, pal.ink_dim, x, y, w, h, TextAlign::Center);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::{all_themes, nextstep_classic};

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
            BtReading::Connected { count: 1, name: "MX KEYS" },
            BtReading::Connected { count: 12, name: "HEADPHONES" },
            BtReading::Idle,
            BtReading::Off,
        ];
        for size in [16u32, 56, 112] {
            for state in &states {
                let buffer = render_bluetooth_tile(&theme, &mut fs, &mut sc, size, state);
                assert_eq!((buffer.width, buffer.height), (size, size));
                assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
            }
        }
    }

    #[test]
    fn the_states_are_pairwise_distinct_at_default_scale() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let faces: Vec<Vec<u8>> = [
            BtReading::Connected { count: 1, name: "BUDS" },
            BtReading::Connected { count: 2, name: "BUDS" },
            BtReading::Idle,
            BtReading::Off,
        ]
        .iter()
        .map(|s| render_bluetooth_tile(&theme, &mut fs, &mut sc, 56, s).pixels)
        .collect();
        for a in 0..faces.len() {
            for b in (a + 1)..faces.len() {
                assert_ne!(faces[a], faces[b], "states {a} and {b} rendered identically");
            }
        }
    }

    /// The off face is the whole point of the power-lamp rune: ghosts
    /// and dim lettering only, no full ink anywhere. (Sound only on
    /// the flagship theme, whose grayscale face can't blend into its
    /// saturated LED accent by accident — the wifi test's caveat.)
    #[test]
    fn the_off_face_lights_no_ink_at_all() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let pal = panel::panel_palette(&theme);
        assert!(pal.ink.r != pal.ink.g || pal.ink.g != pal.ink.b, "test premise: flagship LED ink is saturated");
        for size in [56u32, 112] {
            let off = render_bluetooth_tile(&theme, &mut fs, &mut sc, size, &BtReading::Off);
            assert_eq!(count_exact(&off, pal.ink), 0, "size {size}: off face must not light full ink");
            let idle = render_bluetooth_tile(&theme, &mut fs, &mut sc, size, &BtReading::Idle);
            assert!(count_exact(&idle, pal.ink) > 0, "size {size}: a powered radio must light its rune");
        }
    }

    #[test]
    fn a_connection_lights_more_ink_than_idle() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let ink = panel::panel_palette(&theme).ink;
        let connected = render_bluetooth_tile(&theme, &mut fs, &mut sc, 56, &BtReading::Connected { count: 2, name: "X" });
        let idle = render_bluetooth_tile(&theme, &mut fs, &mut sc, 56, &BtReading::Idle);
        assert!(count_exact(&connected, ink) > count_exact(&idle, ink), "the count digits must add lit ink over the bare rune");
    }

    #[test]
    fn every_theme_keeps_a_substantial_glass() {
        let (mut fs, mut sc) = ctx();
        for theme in all_themes() {
            let pal = panel::panel_palette(&theme);
            let buffer = render_bluetooth_tile(&theme, &mut fs, &mut sc, 112, &BtReading::Idle);
            assert!(count_exact(&buffer, pal.glass) > 500, "theme {}: expected a substantial glass area", theme.id);
        }
    }

    #[test]
    fn count_digits_blank_leading_zeros_and_clamp() {
        assert_eq!(count_digits(0), [None, Some(0)]);
        assert_eq!(count_digits(3), [None, Some(3)]);
        assert_eq!(count_digits(10), [Some(1), Some(0)]);
        assert_eq!(count_digits(42), [Some(4), Some(2)]);
        assert_eq!(count_digits(200), [Some(9), Some(9)], "clamps to two digits");
    }

    /// The legibility floor the 7x9 grid exists to clear: at the stock
    /// 56px tile the rune's cell must stay thick enough to read as a
    /// mark rather than as speckle. This is the assertion that would
    /// have caught the 9x13 grid, whose cells came out at 2.7px.
    #[test]
    fn the_rune_cell_stays_legible_at_the_stock_tile() {
        let theme = nextstep_classic();
        let size = 56u32;
        // The same arithmetic `render_bluetooth_tile` does.
        let t = theme.tile.bevel.width.max(1) as i32;
        let margin = t + (size as i32 / 28).max(1);
        let strip_h = ((size as f32) * 0.20).round().max(9.0) as i32;
        let well_h = (size as i32 - margin * 2 - strip_h).max(0) as u32;
        let inset = t + 1;
        let gh = (well_h as i32 - inset * 2).max(0) as u32;
        let gw = ((size as i32 - margin * 2) - inset * 2).max(0) as u32;

        let rune_w = gw - gw / 2;
        let cell = (rune_w as f32 / RUNE[0].len() as f32).min(gh as f32 / RUNE.len() as f32);
        assert!(cell >= 3.0, "a {cell:.1}px rune cell reads as noise, not as the Bluetooth mark");
    }

    #[test]
    fn long_device_names_truncate_rather_than_overflow() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let long = render_bluetooth_tile(
            &theme,
            &mut fs,
            &mut sc,
            56,
            &BtReading::Connected { count: 1, name: "AN ABSURDLY LONG DEVICE NAME THAT CANNOT FIT" },
        );
        assert_eq!((long.width, long.height), (56, 56));
    }

    /// The rune is deliberately *not* left-right symmetric — the
    /// Berkanan loops point right — but it must mirror across its
    /// waist, and every grid row must agree on width or the drawing
    /// loop shears it.
    #[test]
    fn the_rune_grid_is_rectangular_and_mirrors_across_its_waist() {
        for line in RUNE {
            assert_eq!(line.len(), RUNE[0].len(), "grid rows must agree on width");
        }
        for (a, b) in RUNE.iter().zip(RUNE.iter().rev()) {
            assert_eq!(a, b, "the rune mirrors across its waist");
        }
    }

    // -- panel ---------------------------------------------------------

    #[test]
    fn panel_renders_rows_at_the_declared_geometry() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let rows = [
            BtPanelRow::Power { on: true },
            BtPanelRow::Device { name: "MX KEYS", connected: true, pending: false, armed: false },
            BtPanelRow::Device { name: "BUDS", connected: false, pending: false, armed: false },
            BtPanelRow::PairNew,
        ];
        let (w, row_h) = (panel_content_width(56), panel_row_height(56));
        let h = panel_content_height(row_h, rows.len());
        let buffer = render_bt_panel(&theme, &mut fs, &mut sc, w, h, row_h, &rows);
        assert_eq!((buffer.width, buffer.height), (w, row_h * rows.len() as u32));
        // A clamped grant renders at the granted size, clipped not
        // squeezed; a generous one keeps the rows at the top.
        let clamped = render_bt_panel(&theme, &mut fs, &mut sc, w, h - row_h, row_h, &rows);
        assert_eq!(clamped.height, h - row_h);
        let generous = render_bt_panel(&theme, &mut fs, &mut sc, w, h + row_h, row_h, &rows);
        assert_eq!(generous.height, h + row_h);
    }

    #[test]
    fn an_armed_forget_cell_is_visibly_different() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let (w, row_h) = (panel_content_width(56), panel_row_height(56));
        let calm = [BtPanelRow::Device { name: "BUDS", connected: true, pending: false, armed: false }];
        let armed = [BtPanelRow::Device { name: "BUDS", connected: true, pending: false, armed: true }];
        let a = render_bt_panel(&theme, &mut fs, &mut sc, w, row_h, row_h, &calm);
        let b = render_bt_panel(&theme, &mut fs, &mut sc, w, row_h, row_h, &armed);
        assert_ne!(a.pixels, b.pixels);
        // The difference is confined to the forget cell's columns: the
        // name and lamp must not change out from under the pointer.
        let cell = forget_cell_width(row_h);
        let boundary = ((w - cell) * 4) as usize;
        for y in 0..row_h as usize {
            let row_a = &a.pixels[y * w as usize * 4..][..boundary];
            let row_b = &b.pixels[y * w as usize * 4..][..boundary];
            assert_eq!(row_a, row_b, "row {y}: arming must only repaint the cell");
        }
    }

    #[test]
    fn pending_and_settled_devices_render_differently() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let (w, row_h) = (panel_content_width(56), panel_row_height(56));
        let settled = [BtPanelRow::Device { name: "BUDS", connected: false, pending: false, armed: false }];
        let pending = [BtPanelRow::Device { name: "BUDS", connected: false, pending: true, armed: false }];
        let a = render_bt_panel(&theme, &mut fs, &mut sc, w, row_h, row_h, &settled);
        let b = render_bt_panel(&theme, &mut fs, &mut sc, w, row_h, row_h, &pending);
        assert_ne!(a.pixels, b.pixels);
    }

    #[test]
    fn the_no_adapter_panel_still_renders() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let row_h = panel_row_height(56);
        let buffer = render_bt_panel(&theme, &mut fs, &mut sc, panel_content_width(56), row_h, row_h, &[BtPanelRow::NoAdapter]);
        assert_eq!(buffer.height, row_h);
    }
}
