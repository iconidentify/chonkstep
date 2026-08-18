//! A dual-tone dock widget: a mirrored bar-graph history of recent
//! network throughput, download growing up from a center baseline and
//! upload growing down from it — the same idiom system-monitor network
//! mini-graphs use, drawn in this theme's flat hard-edged style. Used by
//! `sysmon`'s combined dashboard for an at-a-glance, all-interfaces-
//! summed read (see `netload` for the per-interface `wmnetload` port).

use tiny_skia::{Paint, Pixmap, Rect as SkRect, Transform};
use wm_theme_api::DecorationBuffer;

use crate::model::Color;
use crate::paint;
use crate::Theme;

/// `rx`/`tx` are each already-normalized (0.0..=1.0) recent-history
/// samples, oldest first, newest last — pure rendering, no sampling of
/// its own. Colors intentionally reuse this desktop's own terminal ANSI
/// blue/amber so the widget reads as part of the same visual system as
/// everything else rather than inventing new accent colors.
pub fn render_network_tile(theme: &Theme, size: u32, rx: &[f32], tx: &[f32]) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero network tile size");

    let (inset, face_w, face_h) = draw_frame(&mut pixmap, theme, size);
    draw_graph(&mut pixmap, inset, inset, face_w, face_h, rx, tx);

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// Bevel frame + dark face. Returns `(inset, face_w, face_h)` — the
/// face's inner rect, in caller coordinates.
fn draw_frame(pixmap: &mut Pixmap, theme: &Theme, size: u32) -> (i32, u32, u32) {
    let bevel = &theme.titlebar.bevel;
    paint::fill_area(pixmap, 0, 0, size, size, &theme.resize_bar.fill);
    paint::draw_bevel(pixmap, 0, 0, size, size, bevel);

    let inset = (bevel.width as f32 + 2.0).max(3.0) as i32;
    let face_w = (size as i32 - inset * 2).max(1) as u32;
    let face_h = (size as i32 - inset * 2).max(1) as u32;
    paint::fill_rect(pixmap, inset, inset, face_w, face_h, Color::rgb(0x10, 0x10, 0x10));
    (inset, face_w, face_h)
}

/// Draws the mirrored rx/tx bar history into the given face rect.
fn draw_graph(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, rx: &[f32], tx: &[f32]) {
    if w == 0 || h == 0 {
        return;
    }
    let down_color = Color::rgb(0x60, 0xa5, 0xfa);
    let up_color = Color::rgb(0xf5, 0x9e, 0x0b);
    let baseline = y as f32 + h as f32 / 2.0;
    paint::fill_rect(pixmap, x, baseline as i32, w, 1, Color::rgb(0x38, 0x38, 0x3c));
    let half_h = (h as f32 / 2.0 - 1.0).max(1.0);

    let n = rx.len().max(tx.len()).max(1);
    let bar_w = (w as f32 / n as f32).max(1.0);

    for i in 0..n {
        let bx = x as f32 + i as f32 * bar_w;
        let rx_h = bar_height(rx.get(i).copied().unwrap_or(0.0), half_h);
        if rx_h > 0.0 {
            draw_bar(pixmap, bx, baseline - rx_h, bar_w, rx_h, down_color);
        }
        let tx_h = bar_height(tx.get(i).copied().unwrap_or(0.0), half_h);
        if tx_h > 0.0 {
            draw_bar(pixmap, bx, baseline, bar_w, tx_h, up_color);
        }
    }
}

fn bar_height(v: f32, half_h: f32) -> f32 {
    if v <= 0.0 {
        0.0
    } else {
        (v.clamp(0.0, 1.0) * half_h).max(1.0)
    }
}

fn draw_bar(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, color: Color) {
    let Some(r) = SkRect::from_xywh(x, y, (w - 1.0).max(1.0), h) else { return };
    let mut p = Paint { anti_alias: false, ..Default::default() };
    p.set_color(paint::sk_color(color));
    pixmap.fill_rect(r, &p, Transform::identity(), None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    #[test]
    fn render_network_tile_produces_correctly_sized_buffers() {
        let theme = nextstep_classic();
        for size in [16u32, 56, 64] {
            let buffer = render_network_tile(&theme, size, &[0.5; 10], &[0.3; 10]);
            assert_eq!(buffer.width, size);
            assert_eq!(buffer.height, size);
            assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn idle_and_busy_history_render_differently() {
        let theme = nextstep_classic();
        let idle = render_network_tile(&theme, 56, &[0.0; 10], &[0.0; 10]);
        let busy = render_network_tile(&theme, 56, &[1.0; 10], &[1.0; 10]);
        assert_ne!(idle.pixels, busy.pixels, "idle and saturated history must render differently");
    }

    #[test]
    fn empty_history_does_not_panic() {
        let theme = nextstep_classic();
        let _ = render_network_tile(&theme, 56, &[], &[]);
    }
}
