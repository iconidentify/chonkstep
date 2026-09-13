//! Bluetooth glyph shared with the pairing application.
use tiny_skia::Pixmap;
use crate::{model::Color, paint};

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

pub fn draw_bt_rune(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, color: Color) {
    if w == 0 || h == 0 {
        return;
    }
    let cols = RUNE[0].len() as u32;
    let rows = RUNE.len() as u32;
    // The cell grid keeps the rune's aspect; the dot inside each cell
    // is sized like `a regular grid`' dots so the rune reads
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
