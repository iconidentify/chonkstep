//! A compact horizontal memory-pressure gauge — `btop`'s header-style
//! percentage bars distilled to dock-tile scale: a single filled bar,
//! green/amber/red by how full it is.

use tiny_skia::Pixmap;
use wm_theme_api::DecorationBuffer;

use crate::model::Color;
use crate::paint;
use crate::Theme;

/// `used_fraction` is 0.0..=1.0 — pure rendering, no sampling of its
/// own; the caller owns reading `/proc/meminfo`.
pub fn render_memory_bar(theme: &Theme, width: u32, height: u32, used_fraction: f32) -> DecorationBuffer {
    let width = width.max(8);
    let height = height.max(4);
    let mut pixmap = Pixmap::new(width, height).expect("nonzero memory bar size");
    let _ = theme; // reserved: a future theme could vary the frame treatment

    paint::fill_rect(&mut pixmap, 0, 0, width, height, Color::rgb(0x10, 0x10, 0x10));

    let used = used_fraction.clamp(0.0, 1.0);
    let inset = 1u32.min(width / 2).min(height / 2);
    let inner_w = width.saturating_sub(inset * 2);
    let inner_h = height.saturating_sub(inset * 2);
    let filled_w = ((inner_w as f32) * used).round() as u32;

    if filled_w > 0 && inner_h > 0 {
        paint::fill_rect(&mut pixmap, inset as i32, inset as i32, filled_w, inner_h, zone_color(used));
    }

    DecorationBuffer { width, height, pixels: pixmap.data().to_vec() }
}

/// Same green/amber/red pressure semantics the CPU meter uses, so
/// "resource under pressure" reads the same color across both widgets.
fn zone_color(t: f32) -> Color {
    if t < 0.55 {
        Color::rgb(0x3d, 0xdc, 0x65)
    } else if t < 0.8 {
        Color::rgb(0xf2, 0xc1, 0x4e)
    } else {
        Color::rgb(0xe6, 0x48, 0x3f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    #[test]
    fn render_memory_bar_produces_correctly_sized_buffers() {
        let theme = nextstep_classic();
        for (w, h) in [(56u32, 10u32), (168, 20), (10, 4)] {
            let buffer = render_memory_bar(&theme, w, h, 0.5);
            assert_eq!(buffer.width, w);
            assert_eq!(buffer.height, h);
            assert_eq!(buffer.pixels.len(), (w * h * 4) as usize);
        }
    }

    #[test]
    fn empty_and_full_bars_render_differently() {
        let theme = nextstep_classic();
        let empty = render_memory_bar(&theme, 100, 12, 0.0);
        let full = render_memory_bar(&theme, 100, 12, 1.0);
        assert_ne!(empty.pixels, full.pixels);
    }

    #[test]
    fn out_of_range_fraction_is_clamped_not_panicking() {
        let theme = nextstep_classic();
        let _ = render_memory_bar(&theme, 56, 10, -1.0);
        let _ = render_memory_bar(&theme, 56, 10, 5.0);
    }
}
