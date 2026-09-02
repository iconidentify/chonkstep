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
//! theme: `colors.toml` and `backgrounds/<wallpaper>.png`. No
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
        std::fs::write(dir.join("backgrounds").join(file), png)?;
        written.push(dir);
    }
    Ok(written)
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
            assert_eq!(entries.len(), 2, "{}: colors.toml and backgrounds/, nothing that runs code: {entries:?}", theme.id);
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
