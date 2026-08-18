//! A small vector-drawn seven-segment digital clock readout (`HH:MM`),
//! in the spirit of a classic LED alarm-clock/calculator display —
//! blocky hard-edged segments, not shaped text, so it needs no font
//! system and stays legible at dock-tile scale.

use tiny_skia::{Paint, Pixmap, Rect as SkRect, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::Color;
use crate::paint;
use crate::Theme;

/// Segment order: a (top), b (top-right), c (bottom-right), d (bottom),
/// e (bottom-left), f (top-left), g (middle). `pub(crate)` — reused by
/// `netload` for its own seven-segment readout rather than duplicating
/// this lookup table.
#[rustfmt::skip]
pub(crate) const DIGIT_SEGMENTS: [[bool; 7]; 10] = [
    [true,  true,  true,  true,  true,  true,  false], // 0
    [false, true,  true,  false, false, false, false], // 1
    [true,  true,  false, true,  true,  false, true],  // 2
    [true,  true,  true,  true,  false, false, true],  // 3
    [false, true,  true,  false, false, true,  true],  // 4
    [true,  false, true,  true,  false, true,  true],  // 5
    [true,  false, true,  true,  true,  true,  true],  // 6
    [true,  true,  true,  false, false, false, false], // 7
    [true,  true,  true,  true,  true,  true,  true],  // 8
    [true,  true,  true,  true,  false, true,  true],  // 9
];

/// Renders `hour:minute` (seconds are deliberately not shown — cramming
/// six digits and two colons into a dock-tile-width readout would make
/// every digit too small to read; the widget's analog mode already
/// covers second-hand precision for anyone who wants it) against a dark
/// LED-style face.
pub fn render_digital_clock(theme: &Theme, width: u32, height: u32, hour: u32, minute: u32) -> DecorationBuffer {
    let width = width.max(8);
    let height = height.max(8);
    let mut pixmap = Pixmap::new(width, height).expect("nonzero digital clock size");
    let _ = theme; // reserved: a future theme could vary the LED tint

    paint::fill_rect(&mut pixmap, 0, 0, width, height, Color::rgb(0x08, 0x08, 0x0a));

    let colon_w = (width as f32 * 0.09).max(2.0);
    let digit_w = ((width as f32 - colon_w) / 4.0).min(height as f32 * 0.62);
    let digit_h = height as f32 * 0.8;
    let total_w = digit_w * 4.0 + colon_w;
    let start_x = ((width as f32 - total_w) / 2.0).max(0.0);
    let y = (height as f32 - digit_h) / 2.0;

    let lit = Color::rgb(0x4a, 0xf6, 0xc7);
    let digits = [hour / 10 % 10, hour % 10, minute / 10, minute % 10];

    let mut x = start_x;
    for (i, digit) in digits.into_iter().enumerate() {
        draw_digit(&mut pixmap, x, y, digit_w, digit_h, digit as usize % 10, lit);
        x += digit_w;
        if i == 1 {
            draw_colon(&mut pixmap, x, y, digit_h, colon_w, lit);
            x += colon_w;
        }
    }

    DecorationBuffer { width, height, pixels: pixmap.data().to_vec() }
}

/// The seven segment rects (a, b, c, d, e, f, g order) for a digit box
/// at `(x, y, w, h)`. `pub(crate)` alongside [`DIGIT_SEGMENTS`] — the
/// other half of the geometry `netload` needs to draw the same style of
/// digit with its own ghost/lit segment treatment.
pub(crate) fn segment_rects(x: f32, y: f32, w: f32, h: f32) -> [(f32, f32, f32, f32); 7] {
    let stroke = (w * 0.24).max(1.5);
    let half_h = h / 2.0;
    let arm = (half_h - stroke * 0.75).max(1.0);
    [
        (x + stroke, y, (w - 2.0 * stroke).max(1.0), stroke),                    // a: top
        (x + w - stroke, y + stroke * 0.5, stroke, arm),                         // b: top-right
        (x + w - stroke, y + half_h + stroke * 0.25, stroke, arm),               // c: bottom-right
        (x + stroke, y + h - stroke, (w - 2.0 * stroke).max(1.0), stroke),       // d: bottom
        (x, y + half_h + stroke * 0.25, stroke, arm),                           // e: bottom-left
        (x, y + stroke * 0.5, stroke, arm),                                      // f: top-left
        (x + stroke, y + half_h - stroke * 0.5, (w - 2.0 * stroke).max(1.0), stroke), // g: middle
    ]
}

fn draw_digit(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, digit: usize, color: Color) {
    let segs = DIGIT_SEGMENTS[digit];
    let mut paint = Paint { anti_alias: false, ..Default::default() };
    paint.set_color(paint::sk_color(color));
    for (lit, (rx, ry, rw, rh)) in segs.into_iter().zip(segment_rects(x, y, w, h)) {
        if !lit {
            continue;
        }
        if let Some(r) = SkRect::from_xywh(rx, ry, rw.max(1.0), rh.max(1.0)) {
            pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
    }
}

fn draw_colon(pixmap: &mut Pixmap, x: f32, y: f32, h: f32, slot_w: f32, color: Color) {
    let dot = slot_w.min(h * 0.14).max(1.5);
    let cx = x + (slot_w - dot).max(0.0) / 2.0;
    let mut paint = Paint { anti_alias: false, ..Default::default() };
    paint.set_color(paint::sk_color(color));
    for cy in [y + h * 0.28, y + h * 0.62] {
        if let Some(r) = SkRect::from_xywh(cx, cy, dot, dot) {
            pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    #[test]
    fn render_digital_clock_produces_correctly_sized_buffers() {
        let theme = nextstep_classic();
        for (w, h) in [(56u32, 20u32), (168, 60), (24, 10)] {
            let buffer = render_digital_clock(&theme, w, h, 9, 41);
            assert_eq!(buffer.width, w);
            assert_eq!(buffer.height, h);
            assert_eq!(buffer.pixels.len(), (w * h * 4) as usize);
        }
    }

    #[test]
    fn different_times_render_differently() {
        let theme = nextstep_classic();
        let a = render_digital_clock(&theme, 100, 40, 9, 41);
        let b = render_digital_clock(&theme, 100, 40, 14, 7);
        assert_ne!(a.pixels, b.pixels);
    }

    #[test]
    fn every_digit_draws_something_distinguishable_from_zero() {
        // Regression guard for the segment lookup table: every digit
        // 0-9 should actually differ from a blank/zero face, catching a
        // transposed or all-false row in `DIGIT_SEGMENTS`.
        let theme = nextstep_classic();
        let blank = render_digital_clock(&theme, 100, 40, 0, 0);
        for minute in 1..10 {
            let other = render_digital_clock(&theme, 100, 40, 0, minute);
            assert_ne!(blank.pixels, other.pixels, "digit {minute} should render differently from 0");
        }
    }
}
