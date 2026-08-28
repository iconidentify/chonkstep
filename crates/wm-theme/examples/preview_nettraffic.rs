//! Design-review contact sheet for the network-traffic instrument
//! (`wm_theme::nettraffic`): renders every builtin theme at both dock
//! scales (56px default, 112px double) in each representative state,
//! with fixed sample data so successive runs diff cleanly, and writes
//! one PNG per combination into the directory given as the first CLI
//! argument, named `<theme-id>_<size>_<state>.png`.
//!
//! States:
//! - `idle`: zero rates, empty histories — the ghost structure alone.
//! - `download-heavy`: a fast download over a trickle of upload.
//! - `upload-heavy`: the mirror image, proving direction-by-position.
//! - `saturated-both`: both lanes pinned at their decayed peak.
//! - `long-name`: a modern predictable interface name (enp0s31f6,
//!   nine characters) exercising the strip's shrink-then-clip fitting.
//! - `no-interface`: the SDK dead screen the widget falls back to.

use tiny_skia::{IntSize, Pixmap};
use wm_theme::default_theme::all_themes;
use wm_theme::nettraffic::{format_rate, render_nettraffic_tile, TrafficLane, NET_TRAFFIC_COLUMNS, NET_TRAFFIC_HALF_ROWS};
use wm_theme::panel;
use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

const IDLE: [u32; NET_TRAFFIC_COLUMNS] = [0; NET_TRAFFIC_COLUMNS];
const FULL: [u32; NET_TRAFFIC_COLUMNS] = [NET_TRAFFIC_HALF_ROWS; NET_TRAFFIC_COLUMNS];
/// A lively but fixed main-lane history, oldest first.
const WAVE_A: [u32; NET_TRAFFIC_COLUMNS] = [1, 1, 2, 3, 4, 3, 2, 2, 3, 4, 4, 3, 2, 3, 4, 4];
/// A second wave so the upload-heavy sheet isn't a pixel mirror of the
/// download-heavy one.
const WAVE_B: [u32; NET_TRAFFIC_COLUMNS] = [2, 3, 4, 4, 3, 2, 1, 2, 3, 4, 3, 3, 4, 4, 3, 2];
/// The trickle the quiet lane shows opposite a busy one.
const TRICKLE: [u32; NET_TRAFFIC_COLUMNS] = [0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 0, 1, 1, 0, 0, 1];

fn lane(bps: f32, now: u32, history: &[u32]) -> TrafficLane<'_> {
    TrafficLane { readout: format_rate(bps), now, history }
}

fn render_states(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
) -> Vec<(&'static str, DecorationBuffer)> {
    let k = 1024.0f32;
    let m = k * k;
    let mut render = |name: &str, down: &TrafficLane, up: &TrafficLane| {
        render_nettraffic_tile(theme, font_system, swash_cache, size, name, down, up)
    };
    let idle = render("eth0", &lane(0.0, 0, &IDLE), &lane(0.0, 0, &IDLE));
    let dl = render("eth0", &lane(84.0 * m, 4, &WAVE_A), &lane(38.0 * k, 1, &TRICKLE));
    let ul = render("wlan0", &lane(96.0 * k, 1, &TRICKLE), &lane(13.0 * m, 4, &WAVE_B));
    let sat = render("eth0", &lane(118.0 * m, 4, &FULL), &lane(97.0 * m, 4, &FULL));
    let long = render("enp0s31f6", &lane(2.4 * m, 3, &WAVE_B), &lane(310.0 * k, 1, &TRICKLE));
    let dead = panel::render_dead_tile(theme, font_system, swash_cache, size, "NET");
    vec![
        ("idle", idle),
        ("download-heavy", dl),
        ("upload-heavy", ul),
        ("saturated-both", sat),
        ("long-name", long),
        ("no-interface", dead),
    ]
}

fn main() {
    let out_dir = std::env::args().nth(1).expect("usage: preview_nettraffic <output-dir>");
    std::fs::create_dir_all(&out_dir).expect("create output directory");
    let mut font_system = cosmic_text::FontSystem::new();
    let mut swash_cache = cosmic_text::SwashCache::new();

    for theme in all_themes() {
        for size in [56u32, 112] {
            for (state, buffer) in render_states(&theme, &mut font_system, &mut swash_cache, size) {
                let pixmap = Pixmap::from_vec(
                    buffer.pixels,
                    IntSize::from_wh(buffer.width, buffer.height).expect("nonzero preview size"),
                )
                .expect("pixel buffer matches its declared size");
                let path = format!("{out_dir}/{}_{size}_{state}.png", theme.id);
                pixmap.save_png(&path).expect("write preview PNG");
                println!("{path}");
            }
        }
    }
}
