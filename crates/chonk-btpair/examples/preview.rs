//! Rasterizes every phase of the pairing dialog to PNGs — the design
//! review this machine can actually run, since it has no Bluetooth
//! controller and can therefore never reach any phase but
//! `Unavailable` on real hardware.
//!
//! ```sh
//! cargo run -p chonk-btpair --example preview-btpair -- /tmp/btpair
//! ```
//!
//! One sheet per built-in theme and appearance, so the "does it match
//! the join dialog in every palette" question is answered by looking
//! rather than by hoping. `chonk-netjoin`'s own `preview` example is
//! the other half of that comparison.

use chonk_btpair::pair::{Found, Phase};
use chonk_btpair::render::{draw, Metrics};

fn found(address: &str, name: &str, paired: bool) -> Found {
    Found { address: address.to_string(), name: name.to_string(), paired }
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/btpair".to_string());
    let scale: f32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    std::fs::create_dir_all(&out).expect("create the output directory");

    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let devices = [
        found("38:18:4C:11:22:33", "WH-1000XM4", false),
        found("F8:4E:17:00:11:22", "MX Keys", true),
        found("A0:9F:10:AB:CD:EF", "Pixel 8", false),
        found("C8:21:58:99:00:01", "DualSense", false),
        found("D4:11:22:33:44:55", "D4:11:22:33:44:55", false),
    ];
    let refs: Vec<&Found> = devices.iter().collect();
    let phases: [(&str, Phase); 8] = [
        ("1-starting", Phase::Starting),
        ("2-scanning", Phase::Scanning),
        ("3-pairing", Phase::Pairing { address: "38:18:4C:11:22:33".into() }),
        ("4-confirm", Phase::Confirm { address: "38:18:4C:11:22:33".into(), passkey: "419 203".into() }),
        ("5-display", Phase::DisplayPasskey { address: "F8:4E:17:00:11:22".into(), passkey: "042 318".into() }),
        ("6-needs-keyboard", Phase::NeedsKeyboard { address: "F8:4E:17:00:11:22".into() }),
        ("7-paired", Phase::Paired { address: "38:18:4C:11:22:33".into() }),
        ("8-failed", Phase::Failed { address: "38:18:4C:11:22:33".into(), reason: "Authentication Failed".into() }),
    ];

    let themes = wm_theme::default_theme::all_themes()
        .into_iter()
        .map(|theme| (theme, "dark"))
        .chain(wm_theme::default_theme::all_themes_in(wm_theme::model::Appearance::Light).into_iter().map(|t| (t, "light")));

    for (theme, appearance) in themes {
        // The desk hands an app a theme that is *already* scaled
        // (`chonk_ui::scaled_theme`), so the preview must scale it too
        // or it reviews a window nobody will ever see.
        let theme = theme.scaled(scale);
        let m = Metrics::new(scale);
        for (name, phase) in &phases {
            let mut pixmap = tiny_skia::Pixmap::new(m.width, m.height).expect("nonzero window");
            draw(&mut pixmap, &theme, &mut fonts, &mut swash, &m, phase, &refs, None);
            let path = format!("{out}/{}-{appearance}-{name}.png", theme.id);
            pixmap.save_png(&path).expect("write the sheet");
            println!("{path}");
        }
    }
}
