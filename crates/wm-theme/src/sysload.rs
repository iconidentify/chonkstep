//! The sysload instrument dock app renderer: CPU and memory pressure
//! as one screen, built on the [`crate::panel`] SDK and laid out on
//! [`crate::netload`]'s grammar — a glass well in the tile's upper
//! region, a strip of tile-face lettering below it.
//!
//! Two readings share the glass without crowding each other because
//! they use two different LED idioms: CPU is *history* (a
//! [`panel::draw_led_columns`] graph filling the left of the glass —
//! the question it answers is "what has the machine been doing"), and
//! memory is *level* (one vertical [`panel::draw_led_bar`] on the
//! right — the question is "how full is it right now"). A column graph
//! and a single fat bar cannot be misread for each other even at 56px,
//! which is what lets both live on one screen with no divider.
//!
//! Across the room: an idle machine shows a dark glass with only ghost
//! dots and a short bar; a pegged one is a solid slab of lit ink. The
//! memory-pressure flag adds the screen's one alarm: a lit frame
//! around the memory bar, drawn in the same LED ink — a real
//! instrument flags trouble by lighting another lamp, not by changing
//! color, and staying inside [`panel::PanelPalette`] keeps every theme
//! honest.
//!
//! The `CPU`/`MEM` marks sit on the tile face under their readouts
//! (like netload's interface name), in [`tile::tile_ink`]: labels are
//! chrome, not readings, so they stay off the glass entirely.

use tiny_skia::Pixmap;
use wm_theme_api::DecorationBuffer;

use crate::model::{FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::panel;
use crate::tile;
use crate::Theme;

/// How many history columns the CPU graph shows. Fewer than netload's
/// 16 on purpose: the graph shares the glass with the memory bar, so
/// wider cells keep the dots chunky enough to read at 56px.
pub const SYS_LOAD_COLUMNS: usize = 12;
/// Dot-rows in the CPU graph — one row per 10% of CPU.
pub const SYS_LOAD_ROWS: u32 = 10;
/// Segments in the memory bar — one per 10% used.
pub const SYS_LOAD_MEM_SEGMENTS: u32 = 10;

/// `cpu_levels` is the per-column history, oldest first, each already
/// quantized to `0..=SYS_LOAD_ROWS` by the caller (the widget owns
/// sampling policy; this stays a pure pixel function). `mem_lit` is
/// the bar's lit segment count, `0..=SYS_LOAD_MEM_SEGMENTS`.
/// `mem_alert` lights the alarm frame around the memory bar — the
/// caller decides what counts as pressure.
pub fn render_sysload_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    cpu_levels: &[u32],
    mem_lit: u32,
    mem_alert: bool,
) -> DecorationBuffer {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero sysload tile size");
    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    // Same face geometry as netload: the well fills the top of the
    // tile, the lettering strip takes the bottom, and `margin` keeps
    // the well's sunken bevel clear of the tile's raised relief.
    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let strip_h = ((size as f32) * 0.20).round().max(9.0) as i32;
    let well_x = margin;
    let well_y = margin;
    let well_w = (size as i32 - margin * 2).max(0) as u32;
    let well_h = (size as i32 - margin * 2 - strip_h).max(0) as u32;
    let (gx, gy, gw, gh) = panel::draw_panel_glass(&mut pixmap, well_x, well_y, well_w, well_h, theme);
    let pal = panel::panel_palette(theme);

    // Glass layout: the bar's width and the gap both derive from the
    // glass so 56 and 112 keep the same proportions; the padding keeps
    // the outer dots off the gasket. All widths clamp through zero so
    // absurdly small tiles degrade to an empty (but intact) glass
    // instead of wrapping arithmetic.
    let pad = ((gw.min(gh)) as f32 * 0.06).round().max(2.0) as i32;
    let inner_w = (gw as i32 - pad * 2).max(0);
    let inner_h = (gh as i32 - pad * 2).max(0);
    let bar_w = (((gw as f32) * 0.15).round().max(4.0) as i32).min(inner_w);
    let gap = ((gw as f32) * 0.08).round().max(2.0) as i32;
    let cols_w = (inner_w - bar_w - gap).max(0);

    panel::draw_led_columns(&mut pixmap, gx + pad, gy + pad, cols_w as u32, inner_h as u32, &pal, SYS_LOAD_ROWS, cpu_levels);

    // The bar needs at least 2px per segment or the SDK's segment-gap
    // arithmetic degenerates; below that (far under the 56px dock
    // minimum) the readout is unreadable anyway, so it goes dark
    // rather than glitching.
    if inner_h >= (SYS_LOAD_MEM_SEGMENTS * 2) as i32 {
        let bar_x = gx + pad + cols_w + gap;
        panel::draw_led_bar(&mut pixmap, bar_x, gy + pad, bar_w as u32, inner_h as u32, &pal, SYS_LOAD_MEM_SEGMENTS, mem_lit, true);

        // The alarm: a 1px lit frame around the memory bar's full
        // travel, one pixel clear of the segments so it reads as its
        // own lamp and not as an eleventh segment. Clamped to the
        // glass — at the smallest sizes the frame gives way rather
        // than climbing onto the gasket.
        if mem_alert {
            draw_alert_frame(&mut pixmap, bar_x - 2, gy + pad - 2, bar_w + 4, inner_h + 4, (gx, gy, gw, gh), &pal);
        }
    }

    draw_label_strip(
        &mut pixmap,
        theme,
        font_system,
        swash_cache,
        well_x,
        well_y + well_h as i32,
        well_w,
        strip_h as u32,
    );

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// A hard-edged 1px rectangle outline in LED ink, intersected with the
/// glass interior first so an alarm near the gasket clips instead of
/// painting over the well's bevel.
fn draw_alert_frame(pixmap: &mut Pixmap, x: i32, y: i32, w: i32, h: i32, glass: (i32, i32, u32, u32), pal: &panel::PanelPalette) {
    let (gx, gy, gw, gh) = glass;
    let x0 = x.max(gx);
    let y0 = y.max(gy);
    let x1 = (x + w).min(gx + gw as i32);
    let y1 = (y + h).min(gy + gh as i32);
    let (w, h) = (x1 - x0, y1 - y0);
    if w < 3 || h < 3 {
        return;
    }
    paint::fill_rect(pixmap, x0, y0, w as u32, 1, pal.ink);
    paint::fill_rect(pixmap, x0, y1 - 1, w as u32, 1, pal.ink);
    paint::fill_rect(pixmap, x0, y0, 1, h as u32, pal.ink);
    paint::fill_rect(pixmap, x1 - 1, y0, 1, h as u32, pal.ink);
}

/// The tile-face lettering under the well: `CPU` left-aligned under
/// the history graph, `MEM` right-aligned under the bar — each mark
/// sits under the readout it names, so the strip doubles as the
/// legend without any pointer chrome.
#[allow(clippy::too_many_arguments)] // Explicit raster bounds keep this leaf renderer allocation-free.
fn draw_label_strip(
    pixmap: &mut Pixmap,
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    let ink = tile::tile_ink(theme);
    let font = FontSpec {
        family: theme.menu.item_font.family.clone(),
        size: (h as f32 * 0.68).max(6.0),
        weight: FontWeight::Bold,
        style: FontStyle::Normal,
    };
    // Each label gets its own half so neither can run into the other
    // even if a theme's face renders the family wide.
    let half = w / 2;
    paint::draw_text(pixmap, font_system, swash_cache, "CPU", &font, ink, x, y, half, h, TextAlign::Left);
    paint::draw_text(pixmap, font_system, swash_cache, "MEM", &font, ink, x + half as i32, y, w - half, h, TextAlign::Right);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::{all_themes, nextstep_classic};
    use crate::model::Color;

    fn ctx() -> (cosmic_text::FontSystem, cosmic_text::SwashCache) {
        (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new())
    }

    fn render(theme: &Theme, size: u32, cpu: &[u32], mem: u32, alert: bool) -> DecorationBuffer {
        let (mut fs, mut sc) = ctx();
        render_sysload_tile(theme, &mut fs, &mut sc, size, cpu, mem, alert)
    }

    #[test]
    fn every_theme_renders_correctly_sized_buffers_at_both_scales() {
        for theme in all_themes() {
            for size in [56u32, 112] {
                let buffer = render(&theme, size, &[5; SYS_LOAD_COLUMNS], 5, false);
                assert_eq!((buffer.width, buffer.height), (size, size), "theme {}", theme.id);
                assert_eq!(buffer.pixels.len(), (size * size * 4) as usize, "theme {}", theme.id);
            }
        }
    }

    #[test]
    fn degenerate_sizes_and_empty_history_do_not_panic() {
        let theme = nextstep_classic();
        for size in [8u32, 16, 30] {
            let buffer = render(&theme, size, &[], 0, true);
            assert_eq!(buffer.pixels.len(), (buffer.width * buffer.height * 4) as usize);
        }
    }

    fn count_color(buffer: &DecorationBuffer, c: Color) -> usize {
        buffer.pixels.as_chunks::<4>().0.iter().filter(|p| (p[0], p[1], p[2]) == (c.r, c.g, c.b)).count()
    }

    #[test]
    fn the_glass_is_present_and_theme_derived() {
        for theme in all_themes() {
            let pal = panel::panel_palette(&theme);
            let buffer = render(&theme, 56, &[0; SYS_LOAD_COLUMNS], 0, false);
            // Idle glass: mostly unlit background plus ghost dots.
            // Buffers are premultiplied-opaque, so channel values
            // survive verbatim and exact matching is sound.
            assert!(count_color(&buffer, pal.glass) > 300, "theme {}: expected a substantial glass area", theme.id);
            assert!(count_color(&buffer, pal.ghost) > 50, "theme {}: expected visible ghost dots", theme.id);
        }
    }

    #[test]
    fn busy_lights_more_ink_than_idle() {
        let theme = nextstep_classic();
        let pal = panel::panel_palette(&theme);
        let idle = render(&theme, 56, &[0; SYS_LOAD_COLUMNS], 1, false);
        let pegged = render(&theme, 56, &[SYS_LOAD_ROWS; SYS_LOAD_COLUMNS], SYS_LOAD_MEM_SEGMENTS, false);
        assert!(
            count_color(&pegged, pal.ink) > count_color(&idle, pal.ink) + 100,
            "a pegged screen must carry substantially more lit ink than an idle one"
        );
    }

    #[test]
    fn cpu_and_memory_read_independently() {
        let theme = nextstep_classic();
        let cpu_only = render(&theme, 56, &[SYS_LOAD_ROWS; SYS_LOAD_COLUMNS], 0, false);
        let mem_only = render(&theme, 56, &[0; SYS_LOAD_COLUMNS], SYS_LOAD_MEM_SEGMENTS, false);
        assert_ne!(cpu_only.pixels, mem_only.pixels, "the two readings occupy different glass regions");

        // The memory bar lives on the right of the glass: filling it
        // must not touch the left half, and the CPU graph must not
        // touch the bar's own columns of pixels.
        let idle = render(&theme, 56, &[0; SYS_LOAD_COLUMNS], 0, false);
        let left_half = |b: &DecorationBuffer| {
            let mut out = Vec::new();
            for y in 0..56u32 {
                for x in 0..28u32 {
                    let i = ((y * 56 + x) * 4) as usize;
                    out.extend_from_slice(&b.pixels[i..i + 4]);
                }
            }
            out
        };
        assert_eq!(left_half(&mem_only), left_half(&idle), "memory bar must stay out of the CPU half");
    }

    #[test]
    fn memory_levels_fill_the_bar_bottom_up() {
        let theme = nextstep_classic();
        let pal = panel::panel_palette(&theme);
        let half = render(&theme, 112, &[0; SYS_LOAD_COLUMNS], 5, false);
        let full = render(&theme, 112, &[0; SYS_LOAD_COLUMNS], SYS_LOAD_MEM_SEGMENTS, false);
        // Scan the right third of the glass region for ink rows: the
        // half-full bar's ink must sit strictly below the full bar's
        // topmost ink.
        let top_ink_row = |b: &DecorationBuffer| {
            for y in 0..112u32 {
                for x in 74..112u32 {
                    let i = ((y * 112 + x) * 4) as usize;
                    if (b.pixels[i], b.pixels[i + 1], b.pixels[i + 2]) == (pal.ink.r, pal.ink.g, pal.ink.b) {
                        return Some(y);
                    }
                }
            }
            None
        };
        let (half_top, full_top) = (top_ink_row(&half).unwrap(), top_ink_row(&full).unwrap());
        assert!(half_top > full_top, "a fuller bar must be lit higher up (half {half_top} vs full {full_top})");
    }

    #[test]
    fn the_alert_frame_is_its_own_lamp() {
        let theme = nextstep_classic();
        let calm = render(&theme, 56, &[2; SYS_LOAD_COLUMNS], SYS_LOAD_MEM_SEGMENTS, false);
        let alarmed = render(&theme, 56, &[2; SYS_LOAD_COLUMNS], SYS_LOAD_MEM_SEGMENTS, true);
        assert_ne!(calm.pixels, alarmed.pixels, "the alert flag must be visible even with the bar already full");
        let pal = panel::panel_palette(&theme);
        assert!(
            count_color(&alarmed, pal.ink) > count_color(&calm, pal.ink),
            "the alarm adds lit ink (the frame), it does not recolor anything"
        );
    }

    #[test]
    fn all_five_preview_states_render_distinctly() {
        let theme = nextstep_classic();
        let states: [(&[u32], u32, bool); 5] = [
            (&[0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0], 3, false),
            (&[3, 4, 5, 4, 6, 5, 4, 5, 6, 5, 4, 5], 5, false),
            (&[6, 8, 10, 10, 9, 10, 10, 10, 10, 10, 10, 10], 4, false),
            (&[1, 2, 1, 2, 1, 1, 2, 1, 2, 1, 1, 2], 10, true),
            (&[10, 10, 10, 10, 9, 10, 10, 10, 10, 10, 10, 10], 10, true),
        ];
        for size in [56u32, 112] {
            let rendered: Vec<Vec<u8>> = states.iter().map(|(cpu, mem, alert)| render(&theme, size, cpu, *mem, *alert).pixels).collect();
            for i in 0..rendered.len() {
                for j in (i + 1)..rendered.len() {
                    assert_ne!(rendered[i], rendered[j], "states {i} and {j} must render distinctly at {size}px");
                }
            }
        }
    }
}
