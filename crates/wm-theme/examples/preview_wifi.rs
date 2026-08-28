//! Preview sheet for the network-link instrument (`wm_theme::wifi`):
//! renders every builtin theme at tile sizes 56 and 112 in every
//! representative state — wifi-strong, wifi-weak, ethernet-1000,
//! link-down, and absent (the `panel::render_dead_tile` face the
//! widget shows when no interface exists) — using fixed sample data,
//! so the PNGs are reproducible and diffable across changes.
//!
//! Usage: `cargo run -p wm-theme --example preview_wifi -- <out-dir>`;
//! files land as `<theme-id>_<size>_<state>.png`.

use wm_theme::default_theme::all_themes;
use wm_theme::panel;
use wm_theme::wifi::{render_wifi_tile, LinkReading};

fn main() {
    let out = std::env::args().nth(1).expect("usage: preview_wifi <output-dir>");
    std::fs::create_dir_all(&out).expect("create output dir");
    let mut font_system = cosmic_text::FontSystem::new();
    let mut swash_cache = cosmic_text::SwashCache::new();

    // `None` is the absent state: no interface at all, rendered through
    // the SDK's dead-screen face exactly as the widget does it.
    let states: [(&str, Option<LinkReading>); 5] = [
        ("wifi-strong", Some(LinkReading::Wifi { ssid: "HOMEBASE", signal_pct: 87 })),
        ("wifi-weak", Some(LinkReading::Wifi { ssid: "ATTIC", signal_pct: 23 })),
        ("ethernet-1000", Some(LinkReading::Wired { interface: "enp0s1", speed_mbps: Some(1000) })),
        ("link-down", Some(LinkReading::Down { interface: "eth0" })),
        ("absent", None),
    ];

    for theme in all_themes() {
        for size in [56u32, 112] {
            for (state, reading) in &states {
                let buffer = match reading {
                    Some(reading) => render_wifi_tile(&theme, &mut font_system, &mut swash_cache, size, reading),
                    None => panel::render_dead_tile(&theme, &mut font_system, &mut swash_cache, size, "LNK"),
                };
                let pixmap = tiny_skia::Pixmap::from_vec(
                    buffer.pixels,
                    tiny_skia::IntSize::from_wh(buffer.width, buffer.height).expect("nonzero tile"),
                )
                .expect("buffer length matches dimensions");
                let path = format!("{out}/{}_{size}_{state}.png", theme.id);
                pixmap.save_png(&path).expect("save png");
                println!("{path}");
            }
        }
    }
}
