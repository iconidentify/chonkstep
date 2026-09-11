//! Throwaway debug tool: renders a `DecorationBuffer` for the flagship
//! theme and dumps it as a raw RGBA8 file plus a `.dims` sidecar, so it
//! can be converted to a PNG and visually diffed against a live
//! screenshot without going anywhere near X11 or a screenshot tool.
use std::io::Write;

use wm_theme::{DecorationStyle, FontState, RasterThemeEngine};
use wm_theme_api::{ButtonKind, ButtonRuntimeState, DecorationRequest, Size, ThemeEngine};

fn main() {
    let mut positional = Vec::new();
    let mut args = std::env::args().skip(1);
    let mut style = DecorationStyle::WindowMaker;
    while let Some(argument) = args.next() {
        if argument == "--style" {
            let name = args.next().unwrap_or_default();
            let Some(selected) = DecorationStyle::from_name(&name) else {
                eprintln!("unknown decoration style {name:?}");
                std::process::exit(2);
            };
            style = selected;
        } else {
            positional.push(argument);
        }
    }
    if positional.len() > 4 {
        eprintln!("usage: dump_decoration [--style NAME] [SCALE [TITLE [OUTPUT [unfocused]]]]");
        std::process::exit(2);
    }
    let scale: f32 = positional.first().and_then(|s| s.parse().ok()).unwrap_or(3.0);
    if !scale.is_finite() || scale <= 0.0 {
        eprintln!("scale must be positive and finite");
        std::process::exit(2);
    }
    let title = positional.get(1).cloned().unwrap_or_else(|| "xterm".to_string());
    let out = positional.get(2).cloned().unwrap_or_else(|| "/tmp/decoration_dump".to_string());
    let focused = positional.get(3).map(|s| s != "unfocused").unwrap_or(true);

    let theme = wm_theme::default_theme::nextstep_classic().scaled(scale);
    let engine = match RasterThemeEngine::with_fonts_at_scale(theme, FontState::new(), scale).with_style(style) {
        Ok(engine) => engine,
        Err(error) => { eprintln!("{error}"); std::process::exit(2); }
    };

    let request = DecorationRequest {
        content_size: Size::new(600, 300),
        title,
        focused,
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
