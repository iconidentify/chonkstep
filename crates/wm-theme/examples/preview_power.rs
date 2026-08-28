//! Preview matrix for the POWER instrument (`wm_theme::power`): renders
//! every builtin theme at both dock scales (56 and 112) in every
//! representative state, with fixed sample data so reruns are
//! pixel-identical, and writes each as a PNG named
//! `power_<theme>_<size>_<state>.png` into the directory given as the
//! first CLI argument.
//!
//!     cargo run -p wm-theme --example preview_power -- /tmp/previews
//!
//! States covered: `full-on-ac` (100 percent, plug lit), `charging-60`
//! (bolt lit), `discharging-80` (ghost bolt), `discharging-low-10`
//! (the alarm face), `ac-only-no-battery` (the desktop/VM line-power
//! face), and `no-power-info` (the SDK dead screen).

use wm_theme::default_theme::all_themes;
use wm_theme::power::{render_power_tile, ChargeState, PowerFace};

fn main() {
    let out = std::env::args().nth(1).expect("usage: preview_power <output-dir>");
    std::fs::create_dir_all(&out).expect("create output dir");

    let states: [(&str, PowerFace); 6] = [
        ("full-on-ac", PowerFace::Battery { capacity: Some(100), state: ChargeState::Full }),
        ("charging-60", PowerFace::Battery { capacity: Some(60), state: ChargeState::Charging }),
        ("discharging-80", PowerFace::Battery { capacity: Some(80), state: ChargeState::Discharging }),
        ("discharging-low-10", PowerFace::Battery { capacity: Some(10), state: ChargeState::Discharging }),
        ("ac-only-no-battery", PowerFace::AcOnly),
        ("no-power-info", PowerFace::NoInfo),
    ];

    let mut font_system = cosmic_text::FontSystem::new();
    let mut swash_cache = cosmic_text::SwashCache::new();
    let mut written = 0usize;
    for theme in all_themes() {
        for size in [56u32, 112] {
            for (name, face) in states {
                let buffer = render_power_tile(&theme, &mut font_system, &mut swash_cache, size, face);
                let pixmap = tiny_skia::Pixmap::from_vec(
                    buffer.pixels,
                    tiny_skia::IntSize::from_wh(buffer.width, buffer.height).expect("nonzero preview size"),
                )
                .expect("buffer length matches its dimensions");
                let path = format!("{out}/power_{}_{size}_{name}.png", theme.id);
                pixmap.save_png(&path).expect("write preview png");
                written += 1;
            }
        }
    }
    println!("wrote {written} previews to {out}");
}
