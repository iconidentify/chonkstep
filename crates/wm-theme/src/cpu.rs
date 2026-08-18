//! An LED bar-graph dock widget showing recent CPU load, styled after a
//! classic hardware VU meter — the same tile chrome `clock.rs` uses, so
//! it reads as one family of dockapp-style instruments.

use tiny_skia::{Paint, Pixmap, Rect as SkRect, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::Color;
use crate::paint;
use crate::Theme;

const SEGMENTS: u32 = 8;

/// `load` is 0.0..=1.0 — pure rendering, no time-based state of its own;
/// the caller (`chonkstep`'s widget SDK) owns sampling `/proc/stat` and
/// easing the displayed value toward it for the "animated" feel.
pub fn render_cpu_tile(theme: &Theme, size: u32, load: f32) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero cpu tile size");

    // Same frame/face treatment as the clock tile: dockapp tiles don't
    // get their own style in this milestone, but this keeps every
    // instrument in the dock visually consistent.
    let bevel = &theme.titlebar.bevel;
    paint::fill_area(&mut pixmap, 0, 0, size, size, &theme.resize_bar.fill);
    paint::draw_bevel(&mut pixmap, 0, 0, size, size, bevel);

    let inset = (bevel.width as f32 + 2.0).max(3.0) as i32;
    let face_w = (size as i32 - inset * 2).max(1) as u32;
    let face_h = (size as i32 - inset * 2).max(1) as u32;
    let dark = Color::rgb(0x10, 0x10, 0x10);
    paint::fill_rect(&mut pixmap, inset, inset, face_w, face_h, dark);

    let load = load.clamp(0.0, 1.0);
    let lit = (load * SEGMENTS as f32).round() as u32;

    let gap = (face_h as f32 * 0.06).max(1.0);
    let seg_h = ((face_h as f32 - gap * (SEGMENTS - 1) as f32) / SEGMENTS as f32).max(1.0);
    let seg_x = inset as f32 + 2.0;
    let seg_w = (face_w as f32 - 4.0).max(1.0);

    // Row 0 is the topmost segment; higher rows only light up as load
    // approaches 100%, matching a real VU meter's mostly-green,
    // rarely-red character.
    let mut y = inset as f32;
    for row in 0..SEGMENTS {
        let this_h = if row + 1 == SEGMENTS { (inset as f32 + face_h as f32) - y } else { seg_h };
        let lit_from_bottom = SEGMENTS - lit.min(SEGMENTS);
        let is_lit = row >= lit_from_bottom;
        let zone = segment_zone_color(row, SEGMENTS);
        let color = if is_lit { zone } else { mix(dark, zone, 0.16) };

        if let Some(r) = SkRect::from_xywh(seg_x, y, seg_w, this_h.max(1.0)) {
            let mut p = Paint { anti_alias: false, ..Default::default() };
            p.set_color(paint::sk_color(color));
            pixmap.fill_rect(r, &p, Transform::identity(), None);
        }
        y += this_h + gap;
    }

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// `row` 0 is the top of the stack. The top ~20% reads as red, the next
/// ~25% amber, the bottom (majority) green — classic VU-meter
/// proportions.
fn segment_zone_color(row: u32, total: u32) -> Color {
    let t = row as f32 / (total - 1).max(1) as f32;
    if t < 0.2 {
        Color::rgb(0xe6, 0x48, 0x3f)
    } else if t < 0.45 {
        Color::rgb(0xf2, 0xc1, 0x4e)
    } else {
        Color::rgb(0x3d, 0xdc, 0x65)
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let l = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Color::rgb(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    #[test]
    fn render_cpu_tile_produces_correctly_sized_buffers() {
        let theme = nextstep_classic();
        for size in [16u32, 56, 64] {
            let buffer = render_cpu_tile(&theme, size, 0.5);
            assert_eq!(buffer.width, size);
            assert_eq!(buffer.height, size);
            assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn higher_load_lights_more_of_the_meter() {
        let theme = nextstep_classic();
        let idle = render_cpu_tile(&theme, 56, 0.0);
        let busy = render_cpu_tile(&theme, 56, 1.0);
        assert_ne!(idle.pixels, busy.pixels, "idle and fully-loaded meters must render differently");
    }

    #[test]
    fn out_of_range_load_is_clamped_not_panicking() {
        let theme = nextstep_classic();
        let _ = render_cpu_tile(&theme, 56, -1.0);
        let _ = render_cpu_tile(&theme, 56, 5.0);
    }
}
