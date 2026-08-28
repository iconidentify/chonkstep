//! A small analog clock tile for the dock sidebar, in the spirit of the
//! classic analog-clock dockapp. The clock sits on the shared
//! [`crate::tile`] platform: face and relief come from
//! [`tile::draw_tile_base`], the dial is recessed into a
//! [`tile::draw_tile_well`] so it reads as an instrument set into the
//! panel, and markers and hands use the tile's luminance-picked ink
//! instead of per-widget grays. Deliberately minimalist/geometric —
//! this reads as "a clock" at dock-tile scale, not a detailed
//! illustration.

use std::f32::consts::PI;

use tiny_skia::{LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::{Color, Theme};
use crate::paint;
use crate::tile;

pub fn render_clock_tile(theme: &Theme, size: u32, hour: u32, minute: u32, second: u32) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero clock tile size");

    // The shared tile base replaces the old resize-bar-fill-plus-
    // titlebar-bevel approximation: every dock surface now starts from
    // the same face and relief, so the clock is a true sibling of the
    // Clip rather than a near-match assembled from window chrome.
    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    // The dial sits in a well — shaded down and sunken — replacing the
    // old flat dark square, so it reads recessed like an instrument
    // face. The inset clears the tile's own relief; the dial radius
    // additionally clears the well's bevel so ticks never land on the
    // relief lines.
    let bevel_t = theme.tile.bevel.width.max(1);
    let inset = (bevel_t as f32 + 2.0).max(3.0);
    let well_size = (size as f32 - inset * 2.0).max(1.0) as u32;
    tile::draw_tile_well(&mut pixmap, inset as i32, inset as i32, well_size, well_size, theme);

    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let radius = (size as f32 / 2.0 - inset - bevel_t as f32 - 2.0).max(1.0);

    // Hour markers in the receding ink, with the four cardinal points
    // stepped up to full ink — the same two-tone hierarchy the tile
    // family uses for label/sublabel text.
    let ink = tile::tile_ink(theme);
    let ink_dim = tile::tile_ink_dim(theme);
    for i in 0..12 {
        let angle = i as f32 * (PI / 6.0) - PI / 2.0;
        let (inner, color) = if i % 3 == 0 { (radius * 0.76, ink) } else { (radius * 0.88, ink_dim) };
        draw_line(&mut pixmap, cx + angle.cos() * inner, cy + angle.sin() * inner, cx + angle.cos() * radius, cy + angle.sin() * radius, color, 1.0);
    }

    let hour_angle = ((hour % 12) as f32 + minute as f32 / 60.0) * (PI / 6.0) - PI / 2.0;
    let minute_angle = (minute as f32 + second as f32 / 60.0) * (PI / 30.0) - PI / 2.0;
    let second_angle = second as f32 * (PI / 30.0) - PI / 2.0;

    // Hands in full ink; the second hand keeps its muted red — the one
    // deliberate accent the tile contract allows, and the traditional
    // color for an instrument's fast hand.
    draw_line(&mut pixmap, cx, cy, cx + hour_angle.cos() * radius * 0.5, cy + hour_angle.sin() * radius * 0.5, ink, 2.2);
    draw_line(&mut pixmap, cx, cy, cx + minute_angle.cos() * radius * 0.75, cy + minute_angle.sin() * radius * 0.75, ink, 1.4);
    draw_line(&mut pixmap, cx, cy, cx + second_angle.cos() * radius * 0.85, cy + second_angle.sin() * radius * 0.85, Color::rgb(0xB0, 0x30, 0x30), 0.8);

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// Anti-aliased stroked line for the dial. Hands and ticks are angled
/// f32 geometry with varying widths, so this keeps its own stroke path
/// rather than borrowing `tile::draw_line`, whose hard 1px integer
/// walk is meant for tile chrome, not rotating hands.
fn draw_line(pixmap: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, color: Color, width: f32) {
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color(paint::sk_color(color));
    paint.anti_alias = true;
    let stroke = Stroke { width, line_cap: LineCap::Round, ..Default::default() };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    #[test]
    fn render_clock_tile_produces_correctly_sized_buffers() {
        let theme = nextstep_classic();
        for size in [16u32, 56, 64] {
            let buffer = render_clock_tile(&theme, size, 10, 9, 30);
            assert_eq!(buffer.width, size);
            assert_eq!(buffer.height, size);
            assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn render_clock_tile_is_not_a_blank_buffer() {
        let theme = nextstep_classic();
        let buffer = render_clock_tile(&theme, 56, 3, 15, 0);
        assert!(buffer.pixels.iter().any(|&b| b != 0), "clock tile should have drawn something");
    }
}
