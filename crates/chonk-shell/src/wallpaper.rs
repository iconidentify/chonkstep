//! Built-in ChonkStep wallpapers and the small amount of image layout
//! needed to turn their source PNGs into screen-sized root background
//! images (see `Backend::paint_root_image` for how a backend shows
//! one).

use std::path::PathBuf;

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use wm_theme::Appearance;
use wm_theme_api::{DecorationBuffer, Size};

use crate::desktop::{DESKTOP_BG, DESKTOP_BG_LIGHT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wallpaper {
    LavenderGrid,
    AmberTerminal,
    TealBlueprint,
    GraphiteFold,
    ClassicLavender,
    JadeTerrace,
    IvoryOrb,
    IndigoWaves,
}

impl Wallpaper {
    pub const DEFAULT: Self = Self::LavenderGrid;
    pub const ALL: [Self; 8] = [
        Self::LavenderGrid,
        Self::AmberTerminal,
        Self::TealBlueprint,
        Self::GraphiteFold,
        Self::ClassicLavender,
        Self::JadeTerrace,
        Self::IvoryOrb,
        Self::IndigoWaves,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::LavenderGrid => "Lavender Grid",
            Self::AmberTerminal => "Amber Terminal",
            Self::TealBlueprint => "Teal Blueprint",
            Self::GraphiteFold => "Graphite Fold",
            Self::ClassicLavender => "Classic Lavender",
            Self::JadeTerrace => "Jade Terrace",
            Self::IvoryOrb => "Ivory Orb",
            Self::IndigoWaves => "Indigo Waves",
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::LavenderGrid => "lavender-grid",
            Self::AmberTerminal => "amber-terminal",
            Self::TealBlueprint => "teal-blueprint",
            Self::GraphiteFold => "graphite-fold",
            Self::ClassicLavender => "classic-lavender",
            Self::JadeTerrace => "jade-terrace",
            Self::IvoryOrb => "ivory-orb",
            Self::IndigoWaves => "indigo-waves",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|wallpaper| wallpaper.id() == id)
    }

    /// Restores the last menu selection, falling back cleanly when this
    /// is the first launch or the state file came from a newer version.
    pub fn load() -> Self {
        state_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|id| Self::from_id(id.trim()))
            .unwrap_or(Self::DEFAULT)
    }

    pub fn persist(self) -> std::io::Result<()> {
        let Some(path) = state_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.id())
    }

    /// The quiet color at the right edge of each artwork's rendition,
    /// used behind the dock so the sidebar belongs to the selected
    /// composition in the selected mood.
    pub const fn dock_color(self, appearance: Appearance) -> (u8, u8, u8) {
        match (self, appearance) {
            (Self::LavenderGrid, Appearance::Dark) => (129, 130, 153),
            (Self::LavenderGrid, Appearance::Light) => (198, 199, 216),
            (Self::AmberTerminal, Appearance::Dark) => (12, 11, 9),
            (Self::AmberTerminal, Appearance::Light) => (246, 236, 211),
            (Self::TealBlueprint, Appearance::Dark) => (5, 70, 73),
            (Self::TealBlueprint, Appearance::Light) => (222, 240, 238),
            (Self::GraphiteFold, Appearance::Dark) => (24, 24, 24),
            (Self::GraphiteFold, Appearance::Light) => (235, 235, 233),
            (Self::ClassicLavender, Appearance::Dark) => DESKTOP_BG,
            (Self::ClassicLavender, Appearance::Light) => DESKTOP_BG_LIGHT,
            (Self::JadeTerrace, Appearance::Dark) => (30, 60, 45),
            (Self::JadeTerrace, Appearance::Light) => (166, 196, 172),
            (Self::IvoryOrb, Appearance::Light) => (250, 247, 234),
            (Self::IvoryOrb, Appearance::Dark) => (18, 17, 16),
            (Self::IndigoWaves, Appearance::Dark) => (29, 32, 45),
            (Self::IndigoWaves, Appearance::Light) => (226, 228, 236),
        }
    }

    /// The embedded artwork for one appearance. Every artwork ships a
    /// rendition per side of the axis — the counterparts are derived
    /// from the originals by `scripts/gen-wallpaper-renditions.py`, see
    /// `assets/wallpapers/SOURCES.md` — so a theme's wallpaper id never
    /// forks across the axis: the same composition changes mood.
    pub(crate) fn png(self, appearance: Appearance) -> Option<&'static [u8]> {
        match (self, appearance) {
            (Self::LavenderGrid, Appearance::Dark) => Some(include_bytes!("../assets/wallpapers/lavender-grid.png")),
            (Self::LavenderGrid, Appearance::Light) => Some(include_bytes!("../assets/wallpapers/lavender-grid-light.png")),
            (Self::AmberTerminal, Appearance::Dark) => Some(include_bytes!("../assets/wallpapers/amber-terminal.png")),
            (Self::AmberTerminal, Appearance::Light) => Some(include_bytes!("../assets/wallpapers/amber-terminal-light.png")),
            (Self::TealBlueprint, Appearance::Dark) => Some(include_bytes!("../assets/wallpapers/teal-blueprint.png")),
            (Self::TealBlueprint, Appearance::Light) => Some(include_bytes!("../assets/wallpapers/teal-blueprint-light.png")),
            (Self::GraphiteFold, Appearance::Dark) => Some(include_bytes!("../assets/wallpapers/graphite-fold.png")),
            (Self::GraphiteFold, Appearance::Light) => Some(include_bytes!("../assets/wallpapers/graphite-fold-light.png")),
            (Self::ClassicLavender, _) => None,
            (Self::JadeTerrace, Appearance::Dark) => Some(include_bytes!("../assets/wallpapers/jade-terrace.png")),
            (Self::JadeTerrace, Appearance::Light) => Some(include_bytes!("../assets/wallpapers/jade-terrace-light.png")),
            (Self::IvoryOrb, Appearance::Light) => Some(include_bytes!("../assets/wallpapers/ivory-orb.png")),
            (Self::IvoryOrb, Appearance::Dark) => Some(include_bytes!("../assets/wallpapers/ivory-orb-dark.png")),
            (Self::IndigoWaves, Appearance::Dark) => Some(include_bytes!("../assets/wallpapers/indigo-waves.png")),
            (Self::IndigoWaves, Appearance::Light) => Some(include_bytes!("../assets/wallpapers/indigo-waves-light.png")),
        }
    }

    /// Decodes and scales the selected image's rendition for
    /// `appearance` to cover `screen`, cropping equally from opposite
    /// edges when its aspect ratio differs.
    pub fn render(self, screen: Size, appearance: Appearance) -> Option<DecorationBuffer> {
        let source = Pixmap::decode_png(self.png(appearance)?).ok()?;
        let mut dest = Pixmap::new(screen.w.max(1), screen.h.max(1))?;
        let scale =
            (screen.w as f32 / source.width() as f32).max(screen.h as f32 / source.height() as f32);
        let rendered_w = source.width() as f32 * scale;
        let rendered_h = source.height() as f32 * scale;
        let offset_x = (screen.w as f32 - rendered_w) * 0.5;
        let offset_y = (screen.h as f32 - rendered_h) * 0.5;

        let image_paint = PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..PixmapPaint::default()
        };
        dest.draw_pixmap(
            0,
            0,
            source.as_ref(),
            &image_paint,
            Transform::from_row(scale, 0.0, 0.0, scale, offset_x, offset_y),
            None,
        );

        Some(DecorationBuffer {
            width: dest.width(),
            height: dest.height(),
            pixels: dest.data().to_vec(),
        })
    }
}

fn state_path() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(root).join("chonkstep/wallpaper"));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/chonkstep/wallpaper"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_art_wallpaper_decodes_and_covers_the_requested_size_in_both_moods() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            for wallpaper in Wallpaper::ALL
                .into_iter()
                .filter(|w| *w != Wallpaper::ClassicLavender)
            {
                let rendered = wallpaper
                    .render(Size::new(320, 180), appearance)
                    .expect("embedded wallpaper should decode");
                assert_eq!((rendered.width, rendered.height), (320, 180));
                assert_eq!(rendered.pixels.len(), 320 * 180 * 4);
            }
        }
    }

    /// The two renditions of an artwork actually carry their moods: on
    /// every artwork, the light rendition's average luminance clearly
    /// exceeds the dark one's. This is what "the wallpaper's art works
    /// in both moods" means as a testable claim.
    #[test]
    fn light_renditions_are_actually_lighter_than_dark_ones() {
        let mean = |buffer: &wm_theme_api::DecorationBuffer| {
            let sum: u64 = buffer.pixels.chunks_exact(4).map(|px| (px[0] as u64 + px[1] as u64 + px[2] as u64) / 3).sum();
            (sum / (u64::from(buffer.width) * u64::from(buffer.height))) as i64
        };
        for wallpaper in Wallpaper::ALL.into_iter().filter(|w| *w != Wallpaper::ClassicLavender) {
            let light = wallpaper.render(Size::new(160, 90), Appearance::Light).unwrap();
            let dark = wallpaper.render(Size::new(160, 90), Appearance::Dark).unwrap();
            assert!(
                mean(&light) - mean(&dark) > 40,
                "{}: light rendition (mean {}) must be clearly lighter than dark ({})",
                wallpaper.id(),
                mean(&light),
                mean(&dark)
            );
        }
        let lum = |(r, g, b): (u8, u8, u8)| (r as i64 + g as i64 + b as i64) / 3;
        for wallpaper in Wallpaper::ALL {
            assert!(
                lum(wallpaper.dock_color(Appearance::Light)) > lum(wallpaper.dock_color(Appearance::Dark)),
                "{}: dock colors must follow the artwork's moods",
                wallpaper.id()
            );
        }
    }

    #[test]
    fn classic_wallpaper_uses_the_solid_color_path() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            assert!(Wallpaper::ClassicLavender
                .render(Size::new(320, 180), appearance)
                .is_none());
        }
        assert_eq!(Wallpaper::ClassicLavender.dock_color(Appearance::Dark), DESKTOP_BG);
        assert_eq!(Wallpaper::ClassicLavender.dock_color(Appearance::Light), DESKTOP_BG_LIGHT);
    }

    #[test]
    fn wallpaper_ids_round_trip() {
        for wallpaper in Wallpaper::ALL {
            assert_eq!(Wallpaper::from_id(wallpaper.id()), Some(wallpaper));
        }
        assert_eq!(Wallpaper::from_id("not-a-wallpaper"), None);
    }

    /// Every built-in theme names a wallpaper this shell actually has —
    /// the theme registry lives in `wm-theme`, which can't see these
    /// embedded artworks, so the referential integrity check lives here.
    #[test]
    fn every_theme_wallpaper_id_resolves() {
        for theme in wm_theme::default_theme::all_themes() {
            assert!(
                Wallpaper::from_id(&theme.wallpaper).is_some(),
                "theme {} references unknown wallpaper {}",
                theme.id,
                theme.wallpaper
            );
        }
    }
}
