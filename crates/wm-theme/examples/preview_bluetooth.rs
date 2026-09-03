//! Preview sheet for the Bluetooth instrument (`wm_theme::bluetooth`):
//! renders every builtin theme at tile sizes 56 and 112 in every
//! representative state — connected-one, connected-many, idle, off,
//! and absent (the `panel::render_dead_tile` face the widget shows
//! when no adapter exists) — plus the unfolded panel in its
//! representative row mixes, using fixed sample data so the PNGs are
//! reproducible and diffable across changes.
//!
//! Usage: `cargo run -p wm-theme --example preview_bluetooth -- <out-dir>`;
//! tiles land as `<theme-id>_<size>_<state>.png`, panels as
//! `<theme-id>_panel_<state>.png`.

use wm_theme::bluetooth::{
    panel_content_height, panel_content_width, panel_row_height, render_bluetooth_tile, render_bt_panel, BtPanelRow,
    BtReading,
};
use wm_theme::default_theme::all_themes;
use wm_theme::panel;
use wm_theme_api::DecorationBuffer;

fn save(buffer: DecorationBuffer, path: &str) {
    let pixmap = tiny_skia::Pixmap::from_vec(
        buffer.pixels,
        tiny_skia::IntSize::from_wh(buffer.width, buffer.height).expect("nonzero preview"),
    )
    .expect("buffer length matches dimensions");
    pixmap.save_png(path).expect("save png");
    println!("{path}");
}

fn main() {
    let out = std::env::args().nth(1).expect("usage: preview_bluetooth <output-dir>");
    std::fs::create_dir_all(&out).expect("create output dir");
    let mut font_system = cosmic_text::FontSystem::new();
    let mut swash_cache = cosmic_text::SwashCache::new();

    // `None` is the absent state: no adapter at all, rendered through
    // the SDK's dead-screen face exactly as the widget does it.
    let states: [(&str, Option<BtReading>); 5] = [
        ("connected-one", Some(BtReading::Connected { count: 1, name: "MX KEYS" })),
        ("connected-many", Some(BtReading::Connected { count: 3, name: "HEADPHONES" })),
        ("idle", Some(BtReading::Idle)),
        ("off", Some(BtReading::Off)),
        ("absent", None),
    ];

    let panels: [(&str, Vec<BtPanelRow>); 4] = [
        (
            "full",
            vec![
                BtPanelRow::Power { on: true },
                BtPanelRow::Device { name: "MX Keys", connected: true, pending: false, armed: false },
                BtPanelRow::Device { name: "WH-1000XM4", connected: false, pending: true, armed: false },
                BtPanelRow::Device { name: "Trackball", connected: false, pending: false, armed: true },
                BtPanelRow::PairNew,
            ],
        ),
        ("idle", vec![BtPanelRow::Power { on: true }, BtPanelRow::PairNew]),
        ("off", vec![BtPanelRow::Power { on: false }, BtPanelRow::Device { name: "MX Keys", connected: false, pending: false, armed: false }, BtPanelRow::PairNew]),
        ("absent", vec![BtPanelRow::NoAdapter]),
    ];

    for theme in all_themes() {
        for size in [56u32, 112] {
            for (state, reading) in &states {
                let buffer = match reading {
                    Some(reading) => render_bluetooth_tile(&theme, &mut font_system, &mut swash_cache, size, reading),
                    None => panel::render_dead_tile(&theme, &mut font_system, &mut swash_cache, size, "BT"),
                };
                save(buffer, &format!("{out}/{}_{size}_{state}.png", theme.id));
            }
        }
        for (state, rows) in &panels {
            let row_h = panel_row_height(56);
            let buffer = render_bt_panel(
                &theme,
                &mut font_system,
                &mut swash_cache,
                panel_content_width(56),
                panel_content_height(row_h, rows.len()),
                row_h,
                rows,
            );
            save(buffer, &format!("{out}/{}_panel_{state}.png", theme.id));
        }
    }
}
