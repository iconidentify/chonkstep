//! Demo app for the `chonk-ui` SDK: a small themed panel proving that a
//! third-party app can draw its own content with the exact same
//! primitives (fills, bevels, themed text) chonkstep's own dock and
//! root menu use, so it visually matches the rest of the desktop
//! without reimplementing any of the look and feel itself.

use chonk_ui::model::{Color, Fill, TextAlign};
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

// Logical (1x, unscaled) size — `App::new` multiplies this by the active
// `CHONKSTEP_SCALE` for the real window; every position/size below that
// this app computes itself must be scaled the same way (`s(...)`) since
// there's no layout engine yet to do that automatically.
const WIDTH: u32 = 300;
const HEIGHT: u32 = 180;

fn main() {
    let theme = chonk_ui::scaled_theme();
    let app = chonk_ui::App::new("About chonkstep", WIDTH, HEIGHT);
    let scale = app.scale();
    let s = move |v: u32| ((v as f32) * scale).round() as u32;
    let width = s(WIDTH);
    let height = s(HEIGHT);
    let mut font_system = cosmic_text::FontSystem::new();
    let mut swash_cache = cosmic_text::SwashCache::new();

    let logo = Pixmap::decode_png(include_bytes!("../../chonkstep/assets/branding/chonkstep-logo-icon.png"))
        .expect("embedded ChonkStep logo should decode");

    app.run(
        move |pixmap| {
            chonk_ui::paint::fill_area(pixmap, 0, 0, width, height, &Fill::Solid(Color::rgb(0xA6, 0xA6, 0xA6)));
            chonk_ui::paint::draw_bevel(pixmap, 0, 0, width, height, &theme.titlebar.bevel);

            let logo_size = s(44);
            let logo_scale = logo_size as f32 / logo.width() as f32;
            pixmap.draw_pixmap(
                0,
                0,
                logo.as_ref(),
                &PixmapPaint { quality: FilterQuality::Bicubic, ..PixmapPaint::default() },
                Transform::from_row(logo_scale, 0.0, 0.0, logo_scale, ((width - logo_size) / 2) as f32, s(16) as f32),
                None,
            );

            chonk_ui::paint::draw_text(
                pixmap,
                &mut font_system,
                &mut swash_cache,
                "chonkstep",
                &theme.titlebar.font,
                theme.titlebar.text_color_active,
                0,
                s(66) as i32,
                width,
                s(26),
                TextAlign::Center,
            );
            chonk_ui::paint::draw_text(
                pixmap,
                &mut font_system,
                &mut swash_cache,
                "A quietly considered window manager.",
                &theme.menu.item_font,
                theme.titlebar.text_color_inactive,
                s(12) as i32,
                s(96) as i32,
                width - s(24),
                s(40),
                TextAlign::Center,
            );
            chonk_ui::paint::draw_text(
                pixmap,
                &mut font_system,
                &mut swash_cache,
                concat!("v", env!("CARGO_PKG_VERSION")),
                &theme.menu.item_font,
                theme.titlebar.text_color_inactive,
                0,
                (height - s(22)) as i32,
                width,
                s(18),
                TextAlign::Center,
            );
        },
        |_x, _y| {},
    );
}
