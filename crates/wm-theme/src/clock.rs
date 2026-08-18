//! A small analog clock tile for the dock sidebar, in the spirit of the
//! classic WindowMaker `wmclock` dockapp: a bevel-framed square with a
//! dark face and thin light hands. Deliberately minimalist/geometric —
//! this reads as "a clock" at dock-tile scale, not a detailed
//! illustration, in the same hard-edged, muted-grayscale language as the
//! rest of the theme.

use std::f32::consts::PI;

use tiny_skia::{LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::{Color, Theme};
use crate::paint;

pub fn render_clock_tile(theme: &Theme, size: u32, hour: u32, minute: u32, second: u32) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero clock tile size");

    // Reuses the resize-bar fill and the titlebar bevel as the tile's
    // frame/face treatment — dockapp tiles don't get their own style in
    // this milestone, but this keeps them visually consistent with the
    // rest of the theme rather than inventing unstyled colors.
    let bevel = &theme.titlebar.bevel;
    paint::fill_area(&mut pixmap, 0, 0, size, size, &theme.resize_bar.fill);
    paint::draw_bevel(&mut pixmap, 0, 0, size, size, bevel);

    let inset = (bevel.width as f32 + 2.0).max(3.0);
    let face_size = (size as f32 - inset * 2.0).max(1.0) as u32;
    paint::fill_rect(&mut pixmap, inset as i32, inset as i32, face_size, face_size, Color::rgb(0x10, 0x10, 0x10));

    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let radius = (size as f32 / 2.0 - inset - 2.0).max(1.0);

    let tick_color = Color::rgb(0xC0, 0xC0, 0xC0);
    for i in 0..12 {
        let angle = i as f32 * (PI / 6.0) - PI / 2.0;
        let inner = if i % 3 == 0 { radius * 0.76 } else { radius * 0.88 };
        draw_line(&mut pixmap, cx + angle.cos() * inner, cy + angle.sin() * inner, cx + angle.cos() * radius, cy + angle.sin() * radius, tick_color, 1.0);
    }

    let hour_angle = ((hour % 12) as f32 + minute as f32 / 60.0) * (PI / 6.0) - PI / 2.0;
    let minute_angle = (minute as f32 + second as f32 / 60.0) * (PI / 30.0) - PI / 2.0;
    let second_angle = second as f32 * (PI / 30.0) - PI / 2.0;

    let hand_color = Color::rgb(0xF0, 0xF0, 0xF0);
    draw_line(&mut pixmap, cx, cy, cx + hour_angle.cos() * radius * 0.5, cy + hour_angle.sin() * radius * 0.5, hand_color, 2.2);
    draw_line(&mut pixmap, cx, cy, cx + minute_angle.cos() * radius * 0.75, cy + minute_angle.sin() * radius * 0.75, hand_color, 1.4);
    draw_line(&mut pixmap, cx, cy, cx + second_angle.cos() * radius * 0.85, cy + second_angle.sin() * radius * 0.85, Color::rgb(0xB0, 0x30, 0x30), 0.8);

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

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
