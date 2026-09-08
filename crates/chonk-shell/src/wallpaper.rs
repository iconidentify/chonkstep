//! Built-in ChonkStep wallpapers, plus the one that is not built in —
//! Omarchy's current background — and the small amount of image layout
//! needed to turn either into a screen-sized root background image
//! (see `Backend::paint_root_image` for how a backend shows one).

use std::path::{Path, PathBuf};

use tiny_skia::{FilterQuality, IntSize, Pixmap, PixmapPaint, Transform};
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
    /// The LCOS theme's ground. Original to this project, built from
    /// the two colours LCOS's own boot splash is made of; see
    /// `scripts/gen-lcos-wallpaper.py`.
    #[cfg(feature = "lcos")]
    LundukeNavy,
    /// Solid warm grounds for the three Lunduke desk themes, each the
    /// quiet colour of the picture its theme was drawn from. Like
    /// [`Self::ClassicLavender`] these are a colour rather than a file,
    /// so they cost the binary nothing.
    ///
    /// They exist because a built-in theme cannot name a host-discovered
    /// picture: `omarchy-export-themes` has to resolve and render a
    /// theme's wallpaper, and errors outright on one it cannot find. So
    /// each theme names its ground, and the matching photograph stays a
    /// separate choice in the Wallpaper menu — which is the arrangement
    /// the two menus already have, and the palettes come from those
    /// photographs either way.
    #[cfg(feature = "lcos")]
    WalnutGround,
    #[cfg(feature = "lcos")]
    DeskGround,
    #[cfg(feature = "lcos")]
    OakGround,
    /// Whatever Omarchy's `current/background` link points at right
    /// now — the theme's own picture, in Omarchy's own formats. Not in
    /// [`Self::ALL`]: the menu offers it only on a desk that has
    /// Omarchy, and the follow theme adopts it by id
    /// (`wm_theme::omarchy::WALLPAPER`). Read from disk at every
    /// render, so a repaint after the link moves shows the new image;
    /// when the link is missing or the file will not decode, Graphite
    /// Fold stands in — the neutral artwork, in the current mood.
    Omarchy,
    /// One of the host distribution's own backgrounds, discovered at
    /// runtime under `/usr/share/backgrounds/<ID>/` and addressed by
    /// slot. Not in [`Self::ALL`]: like [`Self::Omarchy`] it has no
    /// pixels of its own and is only offered where the files exist.
    ///
    /// Read rather than embedded, deliberately. LCOS ships four of
    /// these and they are Lunduke's artwork; shipping copies inside
    /// this binary would redistribute them, which is the same reason
    /// `lunduke-navy` is original work and the dock mark is read from
    /// `/usr/share/pixmaps`. Showing a picture already installed on the
    /// user's own machine carries no such question.
    HostArt(u8),
}

/// How many host backgrounds the menu will offer. A cap rather than a
/// limit anybody should hit: it keeps the Wallpaper submenu a menu
/// rather than a file browser, and keeps the action ids bounded.
pub const HOST_ART_SLOTS: usize = 6;

/// The host's own backgrounds: `(path, menu label)`, sorted by filename
/// so a slot means the same picture across restarts, deduplicated by
/// stem so a `.jpg`/`.png` pair of the same artwork is offered once.
///
/// Scanned once. Empty on a host that ships none, which is what keeps
/// these rows out of the menu everywhere they do not apply.
pub fn host_art() -> &'static [(PathBuf, &'static str, &'static str)] {
    static ART: std::sync::OnceLock<Vec<(PathBuf, &'static str, &'static str)>> = std::sync::OnceLock::new();
    ART.get_or_init(|| {
        let Some(id) = crate::desktop::os_release_field("ID") else { return Vec::new() };
        if id.is_empty() || id.contains('/') || id.contains("..") {
            return Vec::new();
        }
        let dir = PathBuf::from(format!("/usr/share/backgrounds/{id}"));
        let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.is_file()
                    && matches!(
                        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
                        Some("png" | "jpg" | "jpeg" | "webp")
                    )
            })
            .collect();
        files.sort();
        let mut seen: Vec<String> = Vec::new();
        let mut out: Vec<(PathBuf, &'static str, &'static str)> = Vec::new();
        for path in files {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            if seen.iter().any(|s| s == stem) {
                continue;
            }
            seen.push(stem.to_string());
            out.push((
                path.clone(),
                Box::leak(pretty_label(stem, &id).into_boxed_str()) as &'static str,
                // Named after the file, not the slot. A theme naming a
                // host background has to survive the host installing
                // another one ahead of it alphabetically, which a slot
                // index would not.
                Box::leak(format!("host:{stem}").into_boxed_str()) as &'static str,
            ));
            if out.len() == HOST_ART_SLOTS {
                break;
            }
        }
        out
    })
    .as_slice()
}

/// The host photograph a built-in theme would rather wear, when this
/// machine actually ships it.
///
/// The three Lunduke desk themes have their palettes sampled from these
/// photographs, so wearing the picture is the look they were drawn for;
/// the solid ground each theme *names* is the fallback for a machine
/// without them.
///
/// It is a preference resolved here rather than the theme's `wallpaper`
/// field naming the picture directly, and that is not a detail:
/// `omarchy-export-themes` has to resolve and render a theme's declared
/// wallpaper, and fails outright ("theme names an unknown wallpaper") on
/// one that only exists on somebody else's disk. Declaring the ground
/// and preferring the picture keeps both true — the theme is always
/// exportable, and on LCOS it still arrives wearing the photograph.
#[cfg(feature = "lcos")]
pub fn host_art_for_theme(theme_id: &str) -> Option<Wallpaper> {
    let stem = match theme_id {
        "lunduke-walnut" => "lcos-desktop-4k-wood",
        "lunduke-desk" => "lcos-desktop-desk-coffee",
        "lunduke-oak" => "lcos-desktop-desk-oak-map",
        _ => return None,
    };
    host_art()
        .iter()
        .position(|(path, _, _)| path.file_stem().and_then(|s| s.to_str()) == Some(stem))
        .map(|slot| Wallpaper::HostArt(slot as u8))
}

/// A filename stem turned into something worth reading in a menu:
/// `lcos-desktop-desk-oak-map` -> `Desk Oak Map`. The distro id and a
/// `desktop` filler word are dropped because every one of the files
/// repeats them, and a submenu of "Lcos Desktop ..." five times over
/// tells the reader nothing.
fn pretty_label(stem: &str, id: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for word in stem.split(['-', '_']).filter(|w| !w.is_empty()) {
        let lower = word.to_ascii_lowercase();
        if lower == id.to_ascii_lowercase() || lower == "desktop" || lower == "background" {
            continue;
        }
        // "4k" reads as an acronym, not a word to title-case.
        if lower.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            words.push(lower.to_ascii_uppercase());
        } else {
            let mut chars = lower.chars();
            let first = chars.next().unwrap_or('?').to_ascii_uppercase();
            words.push(format!("{first}{}", chars.as_str()));
        }
    }
    if words.is_empty() { "Background".to_string() } else { words.join(" ") }
}

impl Wallpaper {
    pub const DEFAULT: Self = Self::LavenderGrid;
    /// The embedded artworks, in menu order. [`Self::Omarchy`] is not
    /// one: it has no pixels of its own.
    #[cfg(not(feature = "lcos"))]
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

    /// The same artworks with the LCOS grounds appended. Spelled out
    /// rather than concatenated so each array's length is compile-time
    /// checked.
    #[cfg(feature = "lcos")]
    pub const ALL: [Self; 12] = [
        Self::LavenderGrid,
        Self::AmberTerminal,
        Self::TealBlueprint,
        Self::GraphiteFold,
        Self::ClassicLavender,
        Self::JadeTerrace,
        Self::IvoryOrb,
        Self::IndigoWaves,
        Self::LundukeNavy,
        Self::WalnutGround,
        Self::DeskGround,
        Self::OakGround,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::LavenderGrid => "Lavender Grid",
            Self::AmberTerminal => "Amber Terminal",
            Self::TealBlueprint => "Teal Blueprint",
            Self::GraphiteFold => "Graphite Fold",
            Self::ClassicLavender => "Classic Lavender",
            Self::JadeTerrace => "Jade Terrace",
            Self::IvoryOrb => "Ivory Orb",
            Self::IndigoWaves => "Indigo Waves",
            #[cfg(feature = "lcos")]
            Self::LundukeNavy => "Lunduke Navy",
            #[cfg(feature = "lcos")]
            Self::WalnutGround => "Walnut",
            #[cfg(feature = "lcos")]
            Self::DeskGround => "Desk",
            #[cfg(feature = "lcos")]
            Self::OakGround => "Oak",
            Self::Omarchy => "Omarchy's Background",
            Self::HostArt(slot) => host_art().get(slot as usize).map_or("Background", |(_, label, _)| *label),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::LavenderGrid => "lavender-grid",
            Self::AmberTerminal => "amber-terminal",
            Self::TealBlueprint => "teal-blueprint",
            Self::GraphiteFold => "graphite-fold",
            Self::ClassicLavender => "classic-lavender",
            Self::JadeTerrace => "jade-terrace",
            Self::IvoryOrb => "ivory-orb",
            Self::IndigoWaves => "indigo-waves",
            #[cfg(feature = "lcos")]
            Self::LundukeNavy => "lunduke-navy",
            #[cfg(feature = "lcos")]
            Self::WalnutGround => "walnut-ground",
            #[cfg(feature = "lcos")]
            Self::DeskGround => "desk-ground",
            #[cfg(feature = "lcos")]
            Self::OakGround => "oak-ground",
            Self::Omarchy => wm_theme::omarchy::WALLPAPER,
            Self::HostArt(slot) => host_art().get(slot as usize).map_or("host:none", |(_, _, id)| *id),
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .chain([Self::Omarchy])
            .chain((0..host_art().len() as u8).map(Self::HostArt))
            .find(|wallpaper| wallpaper.id() == id)
    }

    /// Restores the last menu selection; with none — a first launch, or
    /// a state file from a newer version — the wallpaper the session's
    /// theme names, so a desk configured to a theme (`theme = "omarchy"`
    /// in the config, say, with the menu never touched) wears that
    /// theme's whole look and not the flagship's paper under someone
    /// else's palette. A theme naming a wallpaper this build does not
    /// know falls through to the default.
    pub fn load_or(theme_wallpaper: &str) -> Self {
        state_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|id| Self::from_id(id.trim()))
            .or_else(|| Self::from_id(theme_wallpaper))
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

    /// True for the artworks that are a colour rather than a picture:
    /// [`Self::render`] returns `None` for these and the caller paints
    /// [`Self::dock_color`] across the whole desk instead.
    ///
    /// A property rather than a list of names repeated at each use, so
    /// adding another solid ground does not mean hunting down every
    /// test that has to skip decoding one.
    pub const fn is_solid_colour(self) -> bool {
        #[cfg(feature = "lcos")]
        {
            matches!(self, Self::ClassicLavender | Self::WalnutGround | Self::DeskGround | Self::OakGround)
        }
        #[cfg(not(feature = "lcos"))]
        {
            matches!(self, Self::ClassicLavender)
        }
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
            #[cfg(feature = "lcos")]
            (Self::LundukeNavy, Appearance::Dark) => (10, 20, 35),
            #[cfg(feature = "lcos")]
            (Self::LundukeNavy, Appearance::Light) => (220, 226, 238),
            // Sampled from each photograph: the wood's own mid tone, and
            // a paper-side counterpart for the light rendition.
            #[cfg(feature = "lcos")]
            (Self::WalnutGround, Appearance::Dark) => (74, 46, 26),
            #[cfg(feature = "lcos")]
            (Self::WalnutGround, Appearance::Light) => (222, 206, 188),
            #[cfg(feature = "lcos")]
            (Self::DeskGround, Appearance::Dark) => (58, 40, 24),
            #[cfg(feature = "lcos")]
            (Self::DeskGround, Appearance::Light) => (232, 220, 202),
            #[cfg(feature = "lcos")]
            (Self::OakGround, Appearance::Dark) => (44, 30, 18),
            #[cfg(feature = "lcos")]
            (Self::OakGround, Appearance::Light) => (214, 198, 178),
            (Self::IndigoWaves, Appearance::Light) => (226, 228, 236),
            // The floor under Omarchy's picture is Graphite Fold's:
            // this colour is the ground the dock's X11 window shows
            // before its first paint and the root's colour when the
            // image cannot be shown, and both are the neutral artwork's.
            (Self::Omarchy, appearance) => Self::GraphiteFold.dock_color(appearance),
            // Same neutral floor as Omarchy's, and for the same reason:
            // the picture is the host's and its edge colour is unknown
            // until it is decoded.
            (Self::HostArt(_), appearance) => Self::GraphiteFold.dock_color(appearance),
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
            #[cfg(feature = "lcos")]
            (Self::LundukeNavy, Appearance::Dark) => Some(include_bytes!("../assets/wallpapers/lunduke-navy.png")),
            #[cfg(feature = "lcos")]
            (Self::LundukeNavy, Appearance::Light) => Some(include_bytes!("../assets/wallpapers/lunduke-navy-light.png")),
            #[cfg(feature = "lcos")]
            (Self::WalnutGround | Self::DeskGround | Self::OakGround, _) => None,
            (Self::Omarchy, _) => None,
            (Self::HostArt(_), _) => None,
        }
    }

    /// Decodes and scales the selected image's rendition for
    /// `appearance` to cover `screen`, cropping equally from opposite
    /// edges when its aspect ratio differs. `None` for the solid-colour
    /// artwork, and for an image that cannot be read — the caller
    /// paints [`Self::dock_color`] instead.
    pub fn render(self, screen: Size, appearance: Appearance) -> Option<DecorationBuffer> {
        if let Self::HostArt(slot) = self {
            let path = host_art().get(slot as usize).map(|(path, _, _)| path.clone());
            return match path.as_deref().and_then(load_image) {
                Some(source) => Some(cover(&source, screen)),
                // A picture that has been uninstalled since it was
                // chosen should not leave a bare desk.
                None => Self::GraphiteFold.render(screen, appearance),
            };
        }
        if self == Self::Omarchy {
            let link = wm_theme::omarchy::current_background_path();
            return Self::omarchy_background(link.as_deref(), screen, appearance);
        }
        // The image codec is already linked for Omarchy backgrounds.
        // Its newer PNG decoder and RGB-to-RGBA conversion avoid the
        // older tiny-skia path's extra per-channel work at each boot.
        let source = image::load_from_memory_with_format(self.png(appearance)?, image::ImageFormat::Png).ok()?;
        let source = rgba_to_pixmap(source.into_rgba8())?;
        Some(cover(&source, screen))
    }

    /// [`Self::Omarchy`]'s render against an explicit link path: the
    /// image it names laid over the screen, or Graphite Fold in
    /// `appearance` when there is no link, or no image behind it.
    fn omarchy_background(link: Option<&Path>, screen: Size, appearance: Appearance) -> Option<DecorationBuffer> {
        match link.and_then(load_image) {
            Some(source) => Some(cover(&source, screen)),
            None => Self::GraphiteFold.render(screen, appearance),
        }
    }
}

/// Reads and decodes an image file in any format the decoder was built
/// with, sniffing the format from the bytes rather than the name:
/// Omarchy's `current/background` is a symlink whose own name says
/// nothing, and `omarchy theme bg set` takes a path it never checks.
/// Errors are logged, not returned — the one caller has a fallback and
/// a user with a broken background wants the desk up, with a note in
/// the log saying why it is not their picture.
///
/// The result is a premultiplied pixmap, the layout tiny-skia and
/// `DecorationBuffer` share — a no-op for the opaque images wallpapers
/// are, and correct for the odd PNG with a transparent corner.
fn load_image(path: &Path) -> Option<Pixmap> {
    let reader = image::ImageReader::open(path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|error| tracing::warn!(?error, path = %path.display(), "cannot open the background image"))
        .ok()?;
    let image = reader
        .decode()
        .map_err(|error| tracing::warn!(?error, path = %path.display(), "cannot decode the background image"))
        .ok()?
        .into_rgba8();
    rgba_to_pixmap(image)
}

fn rgba_to_pixmap(image: image::RgbaImage) -> Option<Pixmap> {
    let size = IntSize::from_wh(image.width(), image.height())?;
    let mut pixels = image.into_raw();
    for [r, g, b, a] in pixels.as_chunks_mut::<4>().0 {
        if *a != 255 {
            for channel in [r, g, b] {
                *channel = ((*channel as u16 * *a as u16 + 127) / 255) as u8;
            }
        }
    }
    Pixmap::from_vec(pixels, size)
}

/// Lays `source` over `screen` the way every wallpaper setter means by
/// "fill": scaled by the larger of the two axis ratios so nothing shows
/// through, centred so the crop comes equally off the two edges that
/// overflow, resampled bicubically straight into a screen-sized pixmap
/// — one pass over the destination's pixels, however large the source.
fn cover(source: &Pixmap, screen: Size) -> DecorationBuffer {
    let (screen_w, screen_h) = (screen.w.max(1), screen.h.max(1));
    let mut dest = Pixmap::new(screen_w, screen_h).expect("a non-zero pixmap");
    // A wallpaper is the bottom of the scene, so transparent source
    // pixels must resolve here rather than force every renderer to
    // blend the full output forever. Black is the neutral fallback and
    // keeps every pixel we publish honestly opaque.
    dest.fill(tiny_skia::Color::from_rgba8(0, 0, 0, 255));
    let scale =
        (screen_w as f32 / source.width() as f32).max(screen_h as f32 / source.height() as f32);
    let offset_x = (screen_w as f32 - source.width() as f32 * scale) * 0.5;
    let offset_y = (screen_h as f32 - source.height() as f32 * scale) * 0.5;
    let image_paint = PixmapPaint { quality: FilterQuality::Bicubic, ..PixmapPaint::default() };
    dest.draw_pixmap(0, 0, source.as_ref(), &image_paint, Transform::from_row(scale, 0.0, 0.0, scale, offset_x, offset_y), None);
    DecorationBuffer { width: dest.width(), height: dest.height(), pixels: dest.take() }
}

/// `$XDG_STATE_HOME/chonkstep/wallpaper` (see `startup::state_file`).
fn state_path() -> Option<PathBuf> {
    crate::startup::state_file("wallpaper")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_faster_embedded_png_path_preserves_every_bundled_pixel() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            for wallpaper in Wallpaper::ALL {
                let Some(bytes) = wallpaper.png(appearance) else { continue };
                let original = Pixmap::decode_png(bytes).unwrap();
                let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).unwrap();
                let replacement = rgba_to_pixmap(decoded.into_rgba8()).unwrap();
                assert_eq!(replacement, original, "{} {appearance:?}", wallpaper.id());
            }
        }
    }

    #[test]
    fn every_art_wallpaper_decodes_and_covers_the_requested_size_in_both_moods() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            for wallpaper in Wallpaper::ALL.into_iter().filter(|w| !w.is_solid_colour()) {
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
            let sum: u64 = buffer.pixels.as_chunks::<4>().0.iter().map(|px| (px[0] as u64 + px[1] as u64 + px[2] as u64) / 3).sum();
            (sum / (u64::from(buffer.width) * u64::from(buffer.height))) as i64
        };
        for wallpaper in Wallpaper::ALL.into_iter().filter(|w| !w.is_solid_colour()) {
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
        for wallpaper in Wallpaper::ALL.into_iter().chain([Wallpaper::Omarchy]) {
            assert_eq!(Wallpaper::from_id(wallpaper.id()), Some(wallpaper));
        }
        assert_eq!(Wallpaper::from_id("not-a-wallpaper"), None);
        // The follow theme names its wallpaper by this id, and it must
        // land on the variant that reads Omarchy's link.
        assert_eq!(Wallpaper::from_id(wm_theme::omarchy::WALLPAPER), Some(Wallpaper::Omarchy));
        assert!(!Wallpaper::ALL.contains(&Wallpaper::Omarchy), "Omarchy's background is not an embedded artwork");
    }

    /// A scratch Omarchy `current` directory with a `background` link,
    /// made the way `omarchy-theme-bg-set` makes it: a file under the
    /// theme's `backgrounds/` and a symlink at `current/background`
    /// pointing at it.
    fn omarchy_current_with_background(tag: &str, file: &str, bytes: &[u8]) -> std::path::PathBuf {
        let current = std::env::temp_dir().join(format!("chonk-wallpaper-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&current);
        std::fs::create_dir_all(current.join("theme/backgrounds")).unwrap();
        let target = current.join("theme/backgrounds").join(file);
        std::fs::write(&target, bytes).unwrap();
        std::os::unix::fs::symlink(&target, current.join("background")).unwrap();
        current
    }

    /// One solid-colour image encoded in `format`, as a real Omarchy
    /// theme might ship it — Omarchy's own backgrounds are mostly WebP,
    /// with the odd JPEG, so the decoder has to speak those, not only
    /// the PNG the built-ins use.
    fn solid_image(width: u32, height: u32, rgb: [u8; 3], format: image::ImageFormat) -> Vec<u8> {
        // RGB, not RGBA: JPEG has no alpha channel to encode.
        let image = image::RgbImage::from_pixel(width, height, image::Rgb(rgb));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    /// A `width`×`height` pixmap, red on its left half and blue on its
    /// right, so where a crop landed is visible in the pixels.
    fn red_blue(width: u32, height: u32) -> Pixmap {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(if x < width / 2 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
            }
        }
        Pixmap::from_vec(pixels, IntSize::from_wh(width, height).unwrap()).unwrap()
    }

    /// `cover` scales by the larger ratio and crops the rest: a wide
    /// source on a square screen loses its sides, and a same-aspect one
    /// is only resampled.
    #[test]
    fn cover_fills_the_screen_and_crops_from_the_centre() {
        let source = red_blue(400, 100);
        // A square screen takes a 100×100 window from the middle of the
        // 400×100 strip: the seam sits at its centre.
        let square = cover(&source, Size::new(50, 50));
        assert_eq!((square.width, square.height), (50, 50));
        assert_eq!(square.pixels.len(), 50 * 50 * 4);
        let px = |b: &DecorationBuffer, x: u32, y: u32| {
            let i = ((y * b.width + x) * 4) as usize;
            [b.pixels[i], b.pixels[i + 1], b.pixels[i + 2], b.pixels[i + 3]]
        };
        assert_eq!(px(&square, 5, 25), [255, 0, 0, 255], "left of the seam is the red half");
        assert_eq!(px(&square, 45, 25), [0, 0, 255, 255], "right of the seam is the blue half");
        // A screen with the source's own aspect keeps everything.
        let same = cover(&source, Size::new(200, 50));
        assert_eq!(px(&same, 2, 25), [255, 0, 0, 255]);
        assert_eq!(px(&same, 197, 25), [0, 0, 255, 255]);
        // A degenerate screen never panics or divides by zero.
        let dot = cover(&source, Size::new(0, 0));
        assert_eq!((dot.width, dot.height), (1, 1));
    }

    #[test]
    fn cover_resolves_transparency_to_an_opaque_wallpaper() {
        let mut source = Pixmap::new(2, 2).unwrap();
        source.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 80));
        let covered = cover(&source, Size::new(8, 8));
        assert!(covered
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255));
    }

    /// A decoded image arrives straight-alpha and leaves premultiplied,
    /// the only layout the compositor's buffers speak.
    #[test]
    fn a_translucent_image_is_premultiplied_on_the_way_in() {
        let dir = std::env::temp_dir().join(format!("chonk-wallpaper-{}-alpha", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("glass.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 100, 0, 128])).save(&path).unwrap();
        let pixmap = load_image(&path).expect("a png decodes");
        // 200 × 128 / 255 ≈ 100, 100 × 128 / 255 ≈ 50; alpha untouched.
        assert_eq!(&pixmap.data()[..4], &[100, 50, 0, 128]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Omarchy variant reads whatever the link names, in Omarchy's
    /// formats, and re-reads it when the link moves — the whole point
    /// of following.
    #[test]
    fn omarchy_background_is_read_through_the_link_in_omarchys_formats() {
        let red_webp = solid_image(64, 32, [220, 20, 20], image::ImageFormat::WebP);
        let current = omarchy_current_with_background("formats", "red.webp", &red_webp);
        let link = current.join("background");
        let render = |appearance| Wallpaper::omarchy_background(Some(&link), Size::new(32, 32), appearance);

        let centre = |buffer: &DecorationBuffer| {
            let i = (((buffer.height / 2) * buffer.width + buffer.width / 2) * 4) as usize;
            [buffer.pixels[i], buffer.pixels[i + 1], buffer.pixels[i + 2]]
        };
        let first = render(Appearance::Dark).expect("a webp background renders");
        assert_eq!((first.width, first.height), (32, 32));
        let [r, g, b] = centre(&first);
        assert!(r > 180 && g < 60 && b < 60, "the webp's red survives decoding and resampling: {r},{g},{b}");

        // `omarchy-theme-bg-next`: a new file, the link repointed.
        let blue_jpg = solid_image(64, 32, [20, 20, 220], image::ImageFormat::Jpeg);
        std::fs::write(current.join("theme/backgrounds/blue.jpg"), blue_jpg).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(current.join("theme/backgrounds/blue.jpg"), &link).unwrap();
        let second = render(Appearance::Dark).expect("a jpeg background renders");
        let [r, g, b] = centre(&second);
        assert!(b > 180 && r < 60 && g < 60, "the next render is the new picture: {r},{g},{b}");

        // No link at all: the neutral artwork stands in, in the mood asked for.
        std::fs::remove_file(&link).unwrap();
        let floor = render(Appearance::Light).expect("the fallback renders");
        let expected = Wallpaper::GraphiteFold.render(Size::new(32, 32), Appearance::Light).unwrap();
        assert_eq!(floor.pixels, expected.pixels, "with no background set, Omarchy's wallpaper is Graphite Fold");
        // And so it does with no Omarchy state directory at all.
        let none = Wallpaper::omarchy_background(None, Size::new(32, 32), Appearance::Light).unwrap();
        assert_eq!(none.pixels, expected.pixels);

        let _ = std::fs::remove_dir_all(&current);
    }

    /// A link to a file that is not an image is a logged fallback, not
    /// a panic and not a black screen.
    #[test]
    fn an_unreadable_background_falls_back_to_the_neutral_artwork() {
        let current = omarchy_current_with_background("garbage", "notes.txt", b"this is not a picture");
        let rendered = Wallpaper::omarchy_background(Some(&current.join("background")), Size::new(16, 16), Appearance::Dark)
            .expect("falls back");
        let expected = Wallpaper::GraphiteFold.render(Size::new(16, 16), Appearance::Dark).unwrap();
        assert_eq!(rendered.pixels, expected.pixels);
        let _ = std::fs::remove_dir_all(&current);
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
