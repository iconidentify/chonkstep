//! Built-in ChonkStep wallpapers and the small amount of image layout
//! needed to turn their source PNGs into screen-sized root background
//! images (see `Backend::paint_root_image` for how a backend shows
//! one).

use std::path::PathBuf;

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use wm_theme_api::{DecorationBuffer, Size};

use crate::desktop::DESKTOP_BG;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wallpaper {
    LavenderGrid,
    AmberTerminal,
    TealBlueprint,
    GraphiteFold,
    ClassicLavender,
}

impl Wallpaper {
    pub const DEFAULT: Self = Self::LavenderGrid;
    pub const ALL: [Self; 5] = [
        Self::LavenderGrid,
        Self::AmberTerminal,
        Self::TealBlueprint,
        Self::GraphiteFold,
        Self::ClassicLavender,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::LavenderGrid => "Lavender Grid",
            Self::AmberTerminal => "Amber Terminal",
            Self::TealBlueprint => "Teal Blueprint",
            Self::GraphiteFold => "Graphite Fold",
            Self::ClassicLavender => "Classic Lavender",
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::LavenderGrid => "lavender-grid",
            Self::AmberTerminal => "amber-terminal",
            Self::TealBlueprint => "teal-blueprint",
            Self::GraphiteFold => "graphite-fold",
            Self::ClassicLavender => "classic-lavender",
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

    /// The quiet color at the right edge of each artwork, used behind
    /// the dock so the sidebar belongs to the selected composition.
    pub const fn dock_color(self) -> (u8, u8, u8) {
        match self {
            Self::LavenderGrid => (129, 130, 153),
            Self::AmberTerminal => (12, 11, 9),
            Self::TealBlueprint => (5, 70, 73),
            Self::GraphiteFold => (24, 24, 24),
            Self::ClassicLavender => DESKTOP_BG,
        }
    }

    fn png(self) -> Option<&'static [u8]> {
        match self {
            Self::LavenderGrid => Some(include_bytes!("../assets/wallpapers/lavender-grid.png")),
            Self::AmberTerminal => Some(include_bytes!("../assets/wallpapers/amber-terminal.png")),
            Self::TealBlueprint => Some(include_bytes!("../assets/wallpapers/teal-blueprint.png")),
            Self::GraphiteFold => Some(include_bytes!("../assets/wallpapers/graphite-fold.png")),
            Self::ClassicLavender => None,
        }
    }

    /// Decodes and scales the selected image to cover `screen`, cropping
    /// equally from opposite edges when its aspect ratio differs.
    pub fn render(self, screen: Size) -> Option<DecorationBuffer> {
        let source = Pixmap::decode_png(self.png()?).ok()?;
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
    fn every_art_wallpaper_decodes_and_covers_the_requested_size() {
        for wallpaper in Wallpaper::ALL
            .into_iter()
            .filter(|w| *w != Wallpaper::ClassicLavender)
        {
            let rendered = wallpaper
                .render(Size::new(320, 180))
                .expect("embedded wallpaper should decode");
            assert_eq!((rendered.width, rendered.height), (320, 180));
            assert_eq!(rendered.pixels.len(), 320 * 180 * 4);
        }
    }

    #[test]
    fn classic_wallpaper_uses_the_solid_color_path() {
        assert!(Wallpaper::ClassicLavender
            .render(Size::new(320, 180))
            .is_none());
        assert_eq!(Wallpaper::ClassicLavender.dock_color(), DESKTOP_BG);
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
