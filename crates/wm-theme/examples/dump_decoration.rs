//! Throwaway debug tool: renders a `DecorationBuffer` for the flagship
//! theme and dumps it as a raw RGBA8 file plus a `.dims` sidecar, so it
//! can be converted to a PNG and visually diffed against a live
//! screenshot without going anywhere near X11 or a screenshot tool.
use std::io::Write;

use wm_theme::RasterThemeEngine;
use wm_theme_api::{ButtonKind, ButtonRuntimeState, DecorationRequest, Size, ThemeEngine};

fn main() {
    let scale: f32 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let title = std::env::args().nth(2).unwrap_or_else(|| "xterm".to_string());
    let out = std::env::args().nth(3).unwrap_or_else(|| "/tmp/decoration_dump".to_string());

    let theme = wm_theme::default_theme::nextstep_classic().scaled(scale);
    let engine = RasterThemeEngine::new(theme);

    let request = DecorationRequest {
        content_size: Size::new(600, 300),
        title,
        focused: true,
        resizable: true,
        buttons: vec![
            ButtonRuntimeState { kind: ButtonKind::Close, hovered: false, pressed: false },
            ButtonRuntimeState { kind: ButtonKind::Miniaturize, hovered: false, pressed: false },
        ],
    };
    let layout = engine.layout(&request);
    let buffer = engine.render(&request, &layout);

    let mut f = std::fs::File::create(format!("{out}.rgba")).unwrap();
    f.write_all(&buffer.pixels).unwrap();
    std::fs::write(format!("{out}.dims"), format!("{}x{}", buffer.width, buffer.height)).unwrap();
    println!("{}x{} -> {out}.rgba", buffer.width, buffer.height);
}
