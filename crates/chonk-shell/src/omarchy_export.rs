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
//! A directory holds what Omarchy reads from a data-only
//! theme: `colors.toml`, `backgrounds/<wallpaper>.png` and a rendered
//! `preview.png` for the picker. Modern themes also supply `shell.toml`
//! surface colors through Omarchy's own API; user overrides remain last.
//! Their `chonkstep.toml` retains the complete public Theme at 1x for a
//! Chonkstep session following the theme through Omarchy's own picker.
//! No
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
        export_theme(&theme, &dir)?;
        written.push(dir);
    }
    Ok(written)
}

/// Register newly shipped themes for this user without replacing existing
/// themes or their customizations. Build each missing theme beside the theme
/// directory, then publish the complete directory so the picker never caches
/// an incomplete preview. Called before Omarchy's shell starts, including after
/// a package upgrade or a compositor hot restart.
pub fn install_missing(target: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for theme in wm_theme::default_theme::all_themes() {
        let dir = target.join(&theme.id);
        match dir.symlink_metadata() {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir_all(target)?;
        let parent = target.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let staging = tempfile::Builder::new().prefix(".chonkstep-theme-").tempdir_in(parent)?;
        let staged = staging.path().join(&theme.id);
        export_theme(&theme, &staged)?;
        match std::fs::rename(&staged, &dir) {
            Ok(()) => written.push(dir),
            Err(_) if dir.symlink_metadata().is_ok() => {}, // Another session installed it first.
            Err(error) => return Err(error),
        }
    }
    Ok(written)
}

/// The per-user directory read by Omarchy's theme picker.
pub fn default_target() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config_home.join("omarchy/themes"))
}

fn export_theme(theme: &wm_theme::Theme, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("backgrounds"))?;
    let palette = wm_theme::omarchy::palette_from_theme(theme);
    std::fs::write(dir.join("colors.toml"), colors_toml(&theme.name, &palette))?;
    if let Some(shell) = shell_toml(theme) {
        std::fs::write(dir.join("shell.toml"), shell)?;
    }
    if let Some(descriptor) = wm_theme::omarchy::descriptor_from_theme(theme).map_err(std::io::Error::other)? {
        std::fs::write(dir.join(wm_theme::omarchy::DESCRIPTOR_FILE), descriptor)?;
    }
    let (file, png) = background(&theme.wallpaper, theme.appearance)?;
    std::fs::write(dir.join("backgrounds").join(&file), png.clone())?;
    // The picker's tile. A theme without one is a blank rectangle
    // in Omarchy's theme list, which is the whole reason a user
    // scrolls that list — see `preview`.
    std::fs::write(dir.join("preview.png"), preview(theme, &png)?)?;
    Ok(())
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
    let pattern = Wallpaper::from_id(&theme.wallpaper).filter(|wallpaper| wallpaper.pattern_rows().is_some());
    let art = pattern.and_then(|wallpaper| wallpaper.render(Size::new(width, height), theme.appearance))
        .and_then(|buffer| Pixmap::from_vec(buffer.pixels, tiny_skia::IntSize::from_wh(width, height)?))
        .or_else(|| Pixmap::decode_png(background_png).ok());
    if let Some(art) = art {
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

    let engine = wm_theme::RasterThemeEngine::new(theme.clone())
        .with_style(wm_theme::DecorationStyle::Auto).expect("registered theme recipe");
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

/// Theme-level color roles understood by Omarchy's Color.qml. Omitted style,
/// typography and bar geometry keys retain Omarchy defaults and user settings.
/// Omarchy merges ~/.config/omarchy/shell.toml over this theme-owned file.
fn shell_toml(theme: &wm_theme::Theme) -> Option<String> {
    use std::fmt::Write;
    let beos = theme.resolve_style(wm_theme_api::DecorationStyle::Auto) == wm_theme_api::DecorationStyle::BeOS;
    let system7 = theme.resolve_style(wm_theme_api::DecorationStyle::Auto) == wm_theme_api::DecorationStyle::System7;
    let chrome = if beos {
        let mut chrome = wm_theme::modern::Chrome::from_theme_at_scale(theme, 1.0);
        chrome.background = wm_theme::beos::DESKTOP;
        chrome.surface = wm_theme::beos::PANEL;
        chrome.panel = wm_theme::beos::PANEL;
        chrome.text = wm_theme::beos::INK;
        chrome.line = wm_theme::model::Color::rgb(96,96,96);
        chrome.selection = wm_theme::beos::MENU_SELECTION;
        chrome.accent = wm_theme::beos::YELLOW;
        chrome.danger = wm_theme::beos::BLUE;
        chrome
    } else if system7 {
        let mut chrome = wm_theme::modern::Chrome::from_theme_at_scale(theme, 1.0);
        chrome.line = wm_theme::model::Color::rgb(0, 0, 0);
        chrome.selection = chrome.line;
        chrome.danger = chrome.line;
        chrome
    } else { theme.chrome? };
    let mut out = String::from("# Generated by chonkstep's omarchy-export-themes.\n# Surface colors only; Omarchy owns bar layout, fonts and user overrides.\n");
    let mut section = |name: &str, colors: &[(&str, wm_theme::model::Color)], alpha: &[(&str, f32)]| {
        writeln!(out,"\n[{name}]").unwrap();
        for (key,color) in colors {
            writeln!(out,"{key} = \"#{:02x}{:02x}{:02x}\"",color.r,color.g,color.b).unwrap();
        }
        for (key,value) in alpha { writeln!(out,"{key} = {value:.1}").unwrap(); }
    };
    section("bar",&[("background",chrome.surface),("text",chrome.text),("active",chrome.danger)],&[]);
    section("popups",&[("background",chrome.instrument.background.unwrap_or(chrome.surface)),
        ("text",chrome.text),("border",chrome.instrument.border.unwrap_or(chrome.line))],&[]);
    section("tooltip",&[("background",chrome.panel),("text",chrome.text),("border",chrome.line)],&[]);
    section("notifications",&[("background",chrome.panel),("text",chrome.text),("border",chrome.accent),("countdown",chrome.accent)],&[]);
    for name in ["menu","launcher"] {
        section(name,&[("background",chrome.panel),("text",chrome.text),("border",chrome.line),
            ("scrim",chrome.background),("selected-background",chrome.selection),("selected-text",if beos { wm_theme::beos::INK } else if system7 { theme.terminal.bg } else { chrome.text }),
            ("selected-border",chrome.line)],&[("background-alpha",1.0),("selected-background-alpha",1.0),("selected-border-alpha",0.0)]);
    }
    Some(out)
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
    if let Some(buffer) = wallpaper.render(wm_theme_api::Size::new(FLAT_BACKGROUND.0,FLAT_BACKGROUND.1),appearance) {
        let image = tiny_skia::Pixmap::from_vec(buffer.pixels,
            tiny_skia::IntSize::from_wh(buffer.width,buffer.height).ok_or_else(||std::io::Error::other("background has no size"))?)
            .ok_or_else(||std::io::Error::other("invalid background pixels"))?;
        return Ok((file,image.encode_png().map_err(std::io::Error::other)?));
    }
    let (r, g, b) = wallpaper.background_color(appearance);
    let mut pixmap = tiny_skia::Pixmap::new(FLAT_BACKGROUND.0, FLAT_BACKGROUND.1)
        .ok_or_else(|| std::io::Error::other("could not allocate the background canvas"))?;
    pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));
    let png = pixmap.encode_png().map_err(std::io::Error::other)?;
    Ok((file, png))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_registration_covers_fresh_installs_and_upgrades_without_overwriting_themes() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("themes");
        let written = install_missing(&target).unwrap();
        assert_eq!(written.len(), wm_theme::default_theme::all_themes().len());
        for id in ["obsidian", "washi", "relay"] {
            let dir = target.join(id);
            for file in ["colors.toml", "shell.toml", "chonkstep.toml"] {
                let text = std::fs::read_to_string(dir.join(file)).unwrap();
                toml::from_str::<toml::Table>(&text).unwrap();
            }
            let png = std::fs::read(dir.join("preview.png")).unwrap();
            tiny_skia::Pixmap::decode_png(&png).unwrap();
            assert_eq!(std::fs::read_dir(dir.join("backgrounds")).unwrap().count(), 1);
        }

        // An upgrade introduces missing themes but must leave authored user
        // overrides and their mtimes untouched, even on repeated logins.
        let custom = target.join("obsidian/colors.toml");
        std::fs::write(&custom, "# User's custom palette\n").unwrap();
        let modified = custom.metadata().unwrap().modified().unwrap();
        for id in ["washi", "relay"] {
            std::fs::remove_dir_all(target.join(id)).unwrap();
        }
        let mut added = install_missing(&target).unwrap();
        added.sort();
        assert_eq!(added, vec![target.join("relay"), target.join("washi")]);
        assert!(install_missing(&target).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&custom).unwrap(), "# User's custom palette\n");
        assert_eq!(custom.metadata().unwrap().modified().unwrap(), modified);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1, "temporary exports are cleaned up");
    }

    #[test]
    fn automatic_registration_preserves_user_theme_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("themes");
        std::fs::create_dir(&target).unwrap();
        for theme in wm_theme::default_theme::all_themes() {
            // A temporarily absent mount or checkout is still the user's
            // theme. Do not replace a dangling link with a built-in export.
            std::os::unix::fs::symlink(root.path().join("unmounted"), target.join(theme.id)).unwrap();
        }
        assert!(install_missing(&target).unwrap().is_empty());
        assert!(target.join("obsidian").is_symlink());
    }

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
        let root = scratch("every");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("themes");
        let user_override = "[bar]\nsize-horizontal = 42\n[font]\nbase-size = 18\n";
        std::fs::write(root.join("shell.toml"),user_override).unwrap();
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
            let decoded=tiny_skia::Pixmap::decode_png(&bytes).expect("a PNG Omarchy can set");
            if !Wallpaper::from_id(&theme.wallpaper).unwrap().is_solid_colour() {
                assert!(decoded.pixels().iter().any(|pixel|*pixel!=decoded.pixels()[0]),"{}: modern procedural artwork must not become a flat ground",theme.id);
            }
            let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
            assert_eq!(
                entries.len(),
                if theme.chrome.is_some() || theme.preferred_decoration_style.is_some() {5} else {3},
                "{}: palette, artwork, preview and optional shell/Chonkstep descriptors, nothing that runs code: {entries:?}",
                theme.id
            );
            assert_eq!(dir.join("shell.toml").exists(),(theme.chrome.is_some() || theme.preferred_decoration_style.is_some()));
            assert_eq!(dir.join(wm_theme::omarchy::DESCRIPTOR_FILE).exists(),(theme.chrome.is_some() || theme.preferred_decoration_style.is_some()));
            // Exercise the real follow path, including Omarchy's current/theme
            // indirection, rather than just parsing the exported palette.
            let current = root.join("current");
            std::fs::create_dir_all(&current).unwrap();
            let _ = std::fs::remove_file(current.join("theme"));
            std::os::unix::fs::symlink(&dir,current.join("theme")).unwrap();
            std::fs::write(current.join("theme.name"),&theme.id).unwrap();
            let loaded = wm_theme::omarchy::load_from_dir(&current).unwrap();
            assert_eq!(loaded.id,wm_theme::omarchy::ID);
            assert_eq!(loaded.wallpaper,wm_theme::omarchy::WALLPAPER);
            if theme.chrome.is_some() || theme.preferred_decoration_style.is_some() {
                let mut expected = theme.clone();
                expected.id=loaded.id.clone();expected.name=loaded.name.clone();expected.wallpaper=loaded.wallpaper.clone();
                assert_eq!(loaded,expected,"{}: full authored theme survives Omarchy selection",theme.id);
                assert_eq!(loaded.resolve_style(wm_theme_api::DecorationStyle::Auto),theme.resolve_style(wm_theme_api::DecorationStyle::Auto));
            } else {
                assert_eq!(loaded.chrome,None);
                assert_eq!(loaded.resolve_style(wm_theme_api::DecorationStyle::Auto),wm_theme_api::DecorationStyle::WindowMaker);
            }
        }
        assert_eq!(std::fs::read_to_string(root.join("shell.toml")).unwrap(),user_override,"machine-level Omarchy overrides remain owned by the user");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn modern_shell_exports_color_roles_without_bar_geometry_or_typography() {
        let hex=|color:wm_theme::model::Color|format!("#{:02x}{:02x}{:02x}",color.r,color.g,color.b);
        for id in ["obsidian","washi","relay"] {
            for appearance in [Appearance::Light,Appearance::Dark] {
                let mut theme=wm_theme::default_theme::theme_variant(id,appearance).unwrap();
                // Colors belong to the tokens, including future/custom theme
                // ids; the exporter must not duplicate named-palette branches.
                theme.id="custom-modern".into();
                theme.chrome.as_mut().unwrap().surface=wm_theme::model::Color::rgb(12,34,56);
                let chrome=theme.chrome.unwrap();
                let parsed:toml::Value=toml::from_str(&shell_toml(&theme).unwrap()).unwrap();
                let tables=parsed.as_table().unwrap();
                assert_eq!(tables.len(),6);
                assert_eq!(parsed["bar"]["background"].as_str(),Some(hex(chrome.surface).as_str()));
                assert_eq!(parsed["bar"]["active"].as_str(),Some(hex(chrome.danger).as_str()),"attention indicators keep their urgent meaning");
                assert_eq!(parsed["menu"]["selected-background"].as_str(),Some(hex(chrome.selection).as_str()));
                assert_eq!(parsed["menu"]["selected-background-alpha"].as_float(),Some(1.0));
                assert_eq!(parsed["popups"]["background"].as_str(),Some(hex(chrome.instrument.background.unwrap_or(chrome.surface)).as_str()));
                for table in tables.values() {
                    for (key,value) in table.as_table().unwrap() {
                        assert!(!key.contains("size") && !key.contains("width") && !key.contains("font") && !key.contains("spacing"));
                        assert!(value.as_str().is_some_and(|s|s.len()==7 && s.starts_with('#')) || value.as_float().is_some_and(|n|(0.0..=1.0).contains(&n)));
                    }
                }
            }
        }
        for theme in wm_theme::default_theme::all_themes().into_iter().filter(|theme|theme.chrome.is_none() && theme.preferred_decoration_style.is_none()) {
            assert!(shell_toml(&theme).is_none(),"classic exports retain Omarchy's own default surface mapping");
        }
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
            if theme.resolve_style(wm_theme_api::DecorationStyle::Auto) == wm_theme_api::DecorationStyle::BeOS {
                assert_eq!(at(355,311), (252,200,0), "BeOS must paint a short yellow tab");
                assert_eq!(titlebar, content, "space beside the tab reveals the back window");
            } else if theme.resolve_style(wm_theme_api::DecorationStyle::Auto) == wm_theme_api::DecorationStyle::System7 {
                // System 7 has white title paper and white content. Its six
                // continuous horizontal stripes distinguish a rendered frame
                // from the alternating pixels of the desktop behind it.
                let stripes = (304..330).filter(|y| (360..380).all(|x| at(x, *y) == (0, 0, 0))).count();
                assert!(stripes >= 6, "{}: missing System 7 title stripes", theme.id);
            } else {
                assert_ne!(
                    content, titlebar,
                    "{}: the focused window's titlebar and its content are the same colour, so no frame was drawn",
                    theme.id
                );
            }
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
        let (r, g, b) = Wallpaper::ClassicLavender.background_color(Appearance::Dark);
        let px = decoded.pixel(0, 0).unwrap();
        assert_eq!((px.red(), px.green(), px.blue()), (r, g, b));
    }
}
