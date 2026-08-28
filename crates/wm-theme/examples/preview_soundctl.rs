//! Renders the soundctl instrument tile in every built-in theme, at
//! both dock scales (56px = 1x, 112px = 2x), in every state the
//! widget can show — fixed sample data, so reruns are diffable:
//! - `low15`: 15% volume (one lonely slat pair, two-digit readout)
//! - `mid55`: 55% (the everyday face)
//! - `full100`: 100% (full stack, all three digits lit)
//! - `muted`: 55% behind a mute — all-ghost glass, struck speaker
//! - `nosink`: no `wpctl`/default sink — the SDK's dead screen
//!
//! Usage: `cargo run -p wm-theme --example preview_soundctl -- <out-dir>`
//! writes `soundctl_<theme>_<size>px_<state>.png` into `<out-dir>`.

use wm_theme::default_theme::all_themes;
use wm_theme::{panel, soundctl};
use wm_theme_api::DecorationBuffer;

fn save_png(buffer: DecorationBuffer, dir: &str, name: &str) {
    let size = tiny_skia::IntSize::from_wh(buffer.width, buffer.height).expect("nonzero tile");
    // DecorationBuffer pixels are premultiplied-opaque RGBA, which is
    // exactly Pixmap's own storage, so the buffer round-trips verbatim.
    let pixmap = tiny_skia::Pixmap::from_vec(buffer.pixels, size).expect("pixel count matches dimensions");
    let path = format!("{dir}/{name}.png");
    pixmap.save_png(&path).expect("writable output directory");
    println!("{path}");
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: preview_soundctl <output-dir>");
    let mut font_system = cosmic_text::FontSystem::new();
    let mut swash_cache = cosmic_text::SwashCache::new();

    let states: [(&str, f32, bool); 4] =
        [("low15", 0.15, false), ("mid55", 0.55, false), ("full100", 1.0, false), ("muted", 0.55, true)];

    for theme in all_themes() {
        for size in [56u32, 112] {
            for (state, volume, muted) in states {
                let buffer =
                    soundctl::render_soundctl_tile(&theme, &mut font_system, &mut swash_cache, size, volume, muted);
                save_png(buffer, &dir, &format!("soundctl_{}_{}px_{}", theme.id, size, state));
            }
            let dead = panel::render_dead_tile(&theme, &mut font_system, &mut swash_cache, size, "SND");
            save_png(dead, &dir, &format!("soundctl_{}_{}px_nosink", theme.id, size));
        }
    }
}
