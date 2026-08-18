//! The "high tech" combined system-monitor dock widget: digital clock,
//! CPU load, memory pressure, and network throughput stacked into one
//! taller instrument. In spirit this is closer to `btop`'s dense, live
//! dashboards than to any single WindowMaker/AfterStep dockapp — a nod
//! to their dockapp scale and chiseled framing, not a recreation of
//! their look, and it composites the same [`cpu`] and [`netgraph`]
//! tiles the standalone analog-clock dock already used elsewhere, so
//! there's exactly one implementation of each gauge.

use tiny_skia::Pixmap;
use wm_theme_api::DecorationBuffer;

use crate::{cpu, digitalclock, membar, netgraph, paint};
use crate::Theme;

/// `tile` is the same square unit every other dock instrument uses —
/// the panel is that wide, but several `tile`-tall sections stacked, so
/// it's markedly taller than a single widget slot.
#[allow(clippy::too_many_arguments)]
pub fn render_sysmon_panel(
    theme: &Theme,
    tile: u32,
    hour: u32,
    minute: u32,
    cpu_load: f32,
    mem_fraction: f32,
    rx_history: &[f32],
    tx_history: &[f32],
) -> DecorationBuffer {
    let width = tile.max(8);
    let clock_h = ((width as f32) * 0.34).round() as u32;
    let mem_h = ((width as f32) * 0.16).round() as u32;
    let gap = ((width as f32) * 0.05).max(1.0).round() as u32;
    let height = clock_h + gap + width + gap + mem_h + gap + width;

    let mut pixmap = Pixmap::new(width, height.max(1)).expect("nonzero sysmon panel size");

    let bevel = &theme.titlebar.bevel;
    paint::fill_area(&mut pixmap, 0, 0, width, height, &theme.resize_bar.fill);
    paint::draw_bevel(&mut pixmap, 0, 0, width, height, bevel);

    let inset = (bevel.width as u32 + 2).max(3).min(width / 2);
    let inner_w = width.saturating_sub(inset * 2);

    let mut y = inset;
    let clock_buf = digitalclock::render_digital_clock(theme, inner_w, clock_h, hour, minute);
    blit(&mut pixmap, inset, y, &clock_buf);
    y += clock_h + gap;

    let cpu_size = tile.min(inner_w).max(1);
    let cpu_buf = cpu::render_cpu_tile(theme, cpu_size, cpu_load);
    blit(&mut pixmap, inset + (inner_w.saturating_sub(cpu_buf.width)) / 2, y, &cpu_buf);
    y += width + gap;

    let mem_buf = membar::render_memory_bar(theme, inner_w, mem_h, mem_fraction);
    blit(&mut pixmap, inset, y, &mem_buf);
    y += mem_h + gap;

    let net_size = tile.min(inner_w).max(1);
    let net_buf = netgraph::render_network_tile(theme, net_size, rx_history, tx_history);
    blit(&mut pixmap, inset + (inner_w.saturating_sub(net_buf.width)) / 2, y, &net_buf);

    DecorationBuffer { width, height, pixels: pixmap.data().to_vec() }
}

/// Same alpha-aware compositing `chonkstep::desktop`'s own dock painter
/// uses — duplicated rather than shared, since it's a three-line pixel
/// loop and pulling in a cross-crate dependency just for it isn't worth
/// it (this crate has no dependency on `chonkstep` at all, deliberately).
fn blit(dest: &mut Pixmap, x: u32, y: u32, src: &DecorationBuffer) {
    let (dest_w, dest_h) = (dest.width(), dest.height());
    for row in 0..src.height {
        let dy = y + row;
        if dy >= dest_h {
            break;
        }
        for col in 0..src.width {
            let dx = x + col;
            if dx >= dest_w {
                continue;
            }
            let idx = ((row * src.width + col) * 4) as usize;
            if idx + 4 > src.pixels.len() {
                continue;
            }
            let (r, g, b, a) = (src.pixels[idx], src.pixels[idx + 1], src.pixels[idx + 2], src.pixels[idx + 3]);
            if let Some(px) = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, a) {
                let pidx = (dy * dest_w + dx) as usize;
                dest.pixels_mut()[pidx] = px;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    #[test]
    fn render_sysmon_panel_is_wider_than_tall_by_a_predictable_multiple() {
        let theme = nextstep_classic();
        let buffer = render_sysmon_panel(&theme, 56, 9, 41, 0.4, 0.5, &[0.2; 10], &[0.1; 10]);
        assert_eq!(buffer.width, 56);
        assert!(buffer.height > 56 * 2, "panel should be noticeably taller than a single tile");
        assert_eq!(buffer.pixels.len(), (buffer.width * buffer.height * 4) as usize);
    }

    #[test]
    fn different_readings_render_differently() {
        let theme = nextstep_classic();
        let idle = render_sysmon_panel(&theme, 56, 0, 0, 0.0, 0.0, &[0.0; 10], &[0.0; 10]);
        let busy = render_sysmon_panel(&theme, 56, 23, 59, 1.0, 1.0, &[1.0; 10], &[1.0; 10]);
        assert_ne!(idle.pixels, busy.pixels);
    }

    #[test]
    fn tiny_tile_size_does_not_panic() {
        let theme = nextstep_classic();
        let _ = render_sysmon_panel(&theme, 1, 0, 0, 0.5, 0.5, &[], &[]);
    }
}
