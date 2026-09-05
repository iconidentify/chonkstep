//! chonkstep's built-in themes as Omarchy themes — the bridge in the
//! other direction. `omarchy-export-themes` materialises one Omarchy
//! theme directory per built-in (its native rendition) into a target
//! directory, normally `~/.config/omarchy/themes/`, where
//! `omarchy-theme-set "Amber Phosphor"` will find it and re-template
//! every Omarchy application in that palette.
//!
//! Generated, not checked in. A theme directory Omarchy will pick up
//! needs a `backgrounds/` with an image in it, and the eight backgrounds
//! weigh nearly ten megabytes together — a second copy in the tree for
//! the sake of a directory layout would be the wrong trade. More
//! importantly, `colors.toml` here is *derived* from the `Theme` by
//! [`wm_theme::omarchy::palette_from_theme`] at export time, so the
//! built-in stays the single source of truth: change a colour in
//! `default_theme.rs` and the next export carries it, with no second
//! file to remember to edit. The unit test below pins that every
//! export parses back through the same reader the following mode
//! uses, with the terminal palette intact.
//!
//! A directory holds exactly what Omarchy reads from a colours-only
//! theme: `colors.toml`, `backgrounds/<wallpaper>.png` and a rendered
//! `preview.png` for the picker. No
//! `hyprland.lua`, `neovim.lua` or terminal configs — those Omarchy
//! templates itself from the palette, and a theme that ships them is
//! treated as shipping code (see `omarchy-theme-set`'s deny list).

use std::path::{Path, PathBuf};

use wm_theme::Appearance;

use crate::wallpaper::Wallpaper;

/// The size of the flat background rendered for a theme whose
/// wallpaper is the procedural Classic Lavender ground and so has no
/// embedded artwork. Omarchy scales backgrounds to the output, so any
/// reasonable canvas serves; this one matches the shipped artworks.
const FLAT_BACKGROUND: (u32, u32) = (1920, 1200);

/// Writes every built-in theme's Omarchy rendition under `target`:
/// `target/<theme id>/colors.toml` and
/// `target/<theme id>/backgrounds/<wallpaper id>.png`. Existing files
/// are overwritten — an export is a refresh. Returns the theme
/// directories written.
pub fn export(target: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for theme in wm_theme::default_theme::all_themes() {
        let dir = target.join(&theme.id);
        std::fs::create_dir_all(dir.join("backgrounds"))?;
        let palette = wm_theme::omarchy::palette_from_theme(&theme);
        std::fs::write(dir.join("colors.toml"), colors_toml(&theme.name, &palette))?;
        let (file, png) = background(&theme.wallpaper, theme.appearance)?;
        std::fs::write(dir.join("backgrounds").join(&file), png.clone())?;
        // The picker's tile. A theme without one is a blank rectangle
        // in Omarchy's theme list, which is the whole reason a user
        // scrolls that list — see `preview`.
        std::fs::write(dir.join("preview.png"), preview(&theme, &png)?)?;
        written.push(dir);
    }
    Ok(written)
}

/// The width and height of a rendered preview. Omarchy's own previews
/// are full-resolution screenshots; a picker scales whatever it is
/// given, so this is sized to read clearly as a thumbnail without
/// carrying a megabyte per theme.
const PREVIEW: (u32, u32) = (1280, 800);

/// Draws a theme's `preview.png`: its background with two of its own
/// window frames on it.
///
/// Rendered rather than photographed, deliberately. Omarchy's previews
/// are screenshots, and a screenshot is truer — but producing one needs
/// a running compositor on a real display, which puts it out of reach
/// of an exporter that has to work over ssh, in a package build and in
/// CI. What is drawn here comes from the same `ThemeEngine` the
/// compositor decorates live windows with, so the chrome in the tile is
/// the chrome the user will get; only the arrangement is invented.
///
/// The two frames are the smallest arrangement that says something: one
/// focused and one not, because a theme's focused/unfocused distinction
/// is the thing a picker most needs to show and the thing a flat colour
/// swatch cannot.
fn preview(theme: &wm_theme::Theme, background_png: &[u8]) -> std::io::Result<Vec<u8>> {
    use tiny_skia::{Pixmap, PixmapPaint, Transform};
    use wm_theme_api::{DecorationRequest, Size, ThemeEngine};

    let (width, height) = PREVIEW;
    let mut canvas = Pixmap::new(width, height).ok_or_else(|| {
        std::io::Error::other("preview canvas could not be allocated")
    })?;

    // The background, scaled to cover. `background` already handed us
    // PNG bytes for every theme — the procedural grounds included,
    // which it renders flat — so there is always something to draw.
    if let Ok(art) = Pixmap::decode_png(background_png) {
        let scale = (width as f32 / art.width() as f32).max(height as f32 / art.height() as f32);
        canvas.draw_pixmap(
            ((width as f32 - art.width() as f32 * scale) / 2.0) as i32,
            ((height as f32 - art.height() as f32 * scale) / 2.0) as i32,
            art.as_ref(),
            &PixmapPaint::default(),
            Transform::from_scale(scale, scale),
            None,
        );
    }

    let engine = wm_theme::RasterThemeEngine::new(theme.clone());
    // Back window first, then the focused one over it, so the overlap
    // reads the way a stack of windows does.
    for (content, origin, title, focused) in [
        (Size::new(520, 320), (150_i32, 150_i32), "Documents".to_string(), false),
        (Size::new(560, 340), (330_i32, 300_i32), theme.name.clone(), true),
    ] {
        let request = DecorationRequest {
            content_size: content,
            title,
            focused,
            resizable: true,
            buttons: Vec::new(),
        };
        let layout = engine.layout(&request);
        let buffer = engine.render(&request, &layout);
        let Some(frame) = Pixmap::from_vec(
            buffer.pixels.clone(),
            tiny_skia::IntSize::from_wh(buffer.width, buffer.height)
                .ok_or_else(|| std::io::Error::other("decoration buffer has no size"))?,
        ) else {
            continue;
        };
        canvas.draw_pixmap(origin.0, origin.1, frame.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        // The content the frame is drawn around. `render` produces the
        // decoration only — a live window's middle is the client's
        // pixels — so without this the tile shows two black holes and
        // reads as a broken screenshot rather than a desktop. The
        // theme's own terminal background is the honest fill: it is the
        // colour this theme actually dresses a window's contents in,
        // and it is the one surface every one of these themes defines.
        let bg = theme.terminal.bg;
        canvas.fill_rect(
            tiny_skia::Rect::from_xywh(
                (origin.0 + layout.client_offset.x) as f32,
                (origin.1 + layout.client_offset.y) as f32,
                content.w as f32,
                content.h as f32,
            )
            .ok_or_else(|| std::io::Error::other("content rect has no size"))?,
            &tiny_skia::Paint {
                shader: tiny_skia::Shader::SolidColor(
                    tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, 255),
                ),
                ..Default::default()
            },
            Transform::identity(),
            None,
        );

    }

    canvas.encode_png().map_err(std::io::Error::other)
}

/// The palette file, headed with where it came from so nobody edits
/// the copy expecting the desk to follow.
fn colors_toml(theme_name: &str, palette: &wm_theme::omarchy::OmarchyPalette) -> String {
    format!(
        "# {theme_name} — a chonkstep built-in, exported as an Omarchy theme by\n\
         # `omarchy-export-themes`. Generated from the theme itself: edit the\n\
         # built-in (crates/wm-theme/src/default_theme.rs) and export again.\n\n{}",
        palette.to_toml()
    )
}

/// The background image for a wallpaper id in `appearance`: the
/// embedded artwork's own PNG bytes where there is artwork, a flat
/// canvas in the ground colour for the procedural one.
fn background(wallpaper_id: &str, appearance: Appearance) -> std::io::Result<(String, Vec<u8>)> {
    let wallpaper = Wallpaper::from_id(wallpaper_id).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("theme names an unknown wallpaper {wallpaper_id:?}"))
    })?;
    let file = format!("{wallpaper_id}.png");
    if let Some(bytes) = wallpaper.png(appearance) {
        return Ok((file, bytes.to_vec()));
    }
    let (r, g, b) = wallpaper.dock_color(appearance);
    let mut pixmap = tiny_skia::Pixmap::new(FLAT_BACKGROUND.0, FLAT_BACKGROUND.1)
        .ok_or_else(|| std::io::Error::other("could not allocate the background canvas"))?;
    pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));
    let png = pixmap.encode_png().map_err(std::io::Error::other)?;
    Ok((file, png))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory per test: the tests run in parallel and each
    /// removes its own on the way out.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chonk-omarchy-export-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The export is a full theme per built-in, and every palette reads
    /// back through the following mode's parser as the theme it came
    /// from — mode, accent, and the terminal slot for slot.
    #[test]
    fn every_built_in_exports_to_a_theme_directory_omarchy_can_read() {
        let target = scratch("every");
        let written = export(&target).unwrap();
        let themes = wm_theme::default_theme::all_themes();
        assert_eq!(written.len(), themes.len());
        for theme in &themes {
            let dir = target.join(&theme.id);
            assert!(written.contains(&dir), "{}", theme.id);
            let text = std::fs::read_to_string(dir.join("colors.toml")).unwrap();
            let palette = wm_theme::omarchy::OmarchyPalette::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", theme.id));
            assert_eq!(palette, wm_theme::omarchy::palette_from_theme(theme), "{}: reads back as written", theme.id);
            assert_eq!(palette.mode, theme.appearance, "{}", theme.id);
            let terminal = palette.terminal();
            assert_eq!((terminal.fg, terminal.bg), (theme.terminal.fg, theme.terminal.bg), "{}", theme.id);
            assert_eq!(terminal.ansi[1..7], theme.terminal.ansi[1..7], "{}", theme.id);
            assert_eq!(terminal.ansi[8..16], theme.terminal.ansi[8..16], "{}", theme.id);
            // ...and dresses a desk again: the round trip through Omarchy
            // and back yields a theme in the same mood.
            let back = wm_theme::omarchy::theme_from_palette(&palette, &theme.name);
            assert_eq!(back.appearance, theme.appearance, "{}", theme.id);

            let background = dir.join("backgrounds").join(format!("{}.png", theme.wallpaper));
            let bytes = std::fs::read(&background).unwrap_or_else(|e| panic!("{}: {e}", background.display()));
            assert!(tiny_skia::Pixmap::decode_png(&bytes).is_ok(), "{}: a PNG Omarchy can set", theme.id);
            let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
            assert_eq!(
                entries.len(),
                3,
                "{}: colors.toml, backgrounds/ and preview.png, nothing that runs code: {entries:?}",
                theme.id
            );
        }
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn exporting_twice_refreshes_in_place() {
        let target = scratch("twice");
        export(&target).unwrap();
        std::fs::write(target.join("graphite/colors.toml"), "stale").unwrap();
        export(&target).unwrap();
        let text = std::fs::read_to_string(target.join("graphite/colors.toml")).unwrap();
        assert!(wm_theme::omarchy::OmarchyPalette::parse(&text).is_ok(), "overwritten with the real palette");
        let _ = std::fs::remove_dir_all(&target);
    }

    /// A preview is written for every theme, at the declared size, and
    /// it is a *picture* rather than a flat rectangle.
    ///
    /// The uniformity check is the load-bearing half. The first version
    /// of this renderer filled each window's content before compositing
    /// its frame, and the decoration buffer — which covers the whole
    /// frame, content region included, and is opaque there — painted
    /// straight over it. Every preview still had the right dimensions
    /// and a plausible file size; only looking at one showed two black
    /// holes where the windows should be. A test that counted bytes
    /// would have passed.
    #[test]
    fn every_theme_gets_a_preview_that_is_not_a_flat_rectangle() {
        let target = scratch("preview");
        export(&target).unwrap();
        for theme in wm_theme::default_theme::all_themes() {
            let png = target.join(&theme.id).join("preview.png");
            let pixmap = tiny_skia::Pixmap::decode_png(&std::fs::read(&png).unwrap())
                .unwrap_or_else(|error| panic!("{}: preview is not a PNG: {error}", theme.id));
            assert_eq!((pixmap.width(), pixmap.height()), PREVIEW, "{}", theme.id);

            let pixels = pixmap.pixels();
            let at = |x: u32, y: u32| {
                let p = pixels[(y * pixmap.width() + x) as usize];
                (p.red(), p.green(), p.blue())
            };
            // Inside the focused window, well clear of its chrome.
            let content = at(600, 430);
            // Its titlebar. A frame that drew puts something else here;
            // a frame that did not leaves the content colour.
            let titlebar = at(600, 311);
            assert_ne!(
                content, titlebar,
                "{}: the focused window's titlebar and its content are the same colour, so no frame was drawn",
                theme.id
            );
            // And the content is the theme's own background rather than
            // the opaque black the decoration buffer carries there.
            // Deliberately not a "corner differs from centre" check: on
            // a theme whose wallpaper and terminal share a colour —
            // ivory-halftone does — that compares equal while
            // everything is drawn correctly.
            let bg = theme.terminal.bg;
            assert_eq!(
                content,
                (bg.r, bg.g, bg.b),
                "{}: the focused window's content must be the theme's own background",
                theme.id
            );
        }
    }

    #[test]
    fn the_procedural_ground_becomes_a_flat_png_in_its_colour() {
        let (file, png) = background("classic-lavender", Appearance::Dark).unwrap();
        assert_eq!(file, "classic-lavender.png");
        let decoded = tiny_skia::Pixmap::decode_png(&png).unwrap();
        let (r, g, b) = Wallpaper::ClassicLavender.dock_color(Appearance::Dark);
        let px = decoded.pixel(0, 0).unwrap();
        assert_eq!((px.red(), px.green(), px.blue()), (r, g, b));
    }
}
