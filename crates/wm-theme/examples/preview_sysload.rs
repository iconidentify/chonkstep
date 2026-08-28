//! Preview sheet generator for the sysload instrument
//! (`wm_theme::sysload`): renders the tile for every built-in theme,
//! at both dock scales (56 and 112), in each representative state,
//! with fixed sample data so reruns are pixel-identical and diffable.
//!
//! Usage: `cargo run -p wm-theme --example preview_sysload -- <out-dir>`
//!
//! Output files are named `<theme-id>_<size>px_<state>.png`, one per
//! combination. The states cover the display's full range: `idle`
//! (ghost dots, short bar), `half-loaded` (mid-range on both),
//! `cpu-pegged` (solid CPU slab, calm memory), `memory-pressure`
//! (quiet CPU, full bar plus the alarm frame), and `both-pegged`
//! (everything lit).

use wm_theme::default_theme::all_themes;
use wm_theme::sysload::{render_sysload_tile, SYS_LOAD_COLUMNS, SYS_LOAD_MEM_SEGMENTS, SYS_LOAD_ROWS};

/// One named display state: CPU history levels (oldest first, each
/// `0..=SYS_LOAD_ROWS`), lit memory segments, and the alarm flag.
struct PreviewState {
    name: &'static str,
    cpu_levels: [u32; SYS_LOAD_COLUMNS],
    mem_lit: u32,
    mem_alert: bool,
}

const STATES: [PreviewState; 5] = [
    PreviewState { name: "idle", cpu_levels: [0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0], mem_lit: 3, mem_alert: false },
    PreviewState { name: "half-loaded", cpu_levels: [3, 4, 5, 4, 6, 5, 4, 5, 6, 5, 4, 5], mem_lit: 5, mem_alert: false },
    PreviewState { name: "cpu-pegged", cpu_levels: [6, 8, 10, 10, 9, 10, 10, 10, 10, 10, 10, 10], mem_lit: 4, mem_alert: false },
    PreviewState { name: "memory-pressure", cpu_levels: [1, 2, 1, 2, 1, 1, 2, 1, 2, 1, 1, 2], mem_lit: SYS_LOAD_MEM_SEGMENTS, mem_alert: true },
    PreviewState {
        name: "both-pegged",
        cpu_levels: [SYS_LOAD_ROWS; SYS_LOAD_COLUMNS],
        mem_lit: SYS_LOAD_MEM_SEGMENTS,
        mem_alert: true,
    },
];

fn main() {
    let out_dir = std::env::args().nth(1).expect("usage: preview_sysload <out-dir>");
    std::fs::create_dir_all(&out_dir).expect("create output directory");

    let mut font_system = cosmic_text::FontSystem::new();
    let mut swash_cache = cosmic_text::SwashCache::new();

    for theme in all_themes() {
        for size in [56u32, 112] {
            for state in &STATES {
                let buffer = render_sysload_tile(
                    &theme,
                    &mut font_system,
                    &mut swash_cache,
                    size,
                    &state.cpu_levels,
                    state.mem_lit,
                    state.mem_alert,
                );
                let pixmap = tiny_skia::Pixmap::from_vec(
                    buffer.pixels,
                    tiny_skia::IntSize::from_wh(buffer.width, buffer.height).expect("nonzero preview size"),
                )
                .expect("pixel buffer matches its declared dimensions");
                let path = format!("{out_dir}/{}_{size}px_{}.png", theme.id, state.name);
                pixmap.save_png(&path).expect("write preview png");
                println!("{path}");
            }
        }
    }
}
