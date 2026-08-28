//! Rendering for launcher-dock tiles: the face a pinned application
//! shows on the strip below the Clip. Built on the theme's common tile
//! ([`tile::draw_tile_base`]) like every other square surface, but
//! where an icon tile frames a captured window preview, a launcher
//! tile has nothing to capture — the app may not even be running — so
//! its identity is typographic: a large two-letter monogram derived
//! from the app's name, set in [`tile::tile_ink`] at a commanding
//! size. A monogram instead of a raster icon is deliberate: `.desktop`
//! `Icon` art comes in whatever palette the app shipped, while a
//! monogram in the tile's own ink keeps the whole strip in one
//! material at every theme, the way the rest of the tile family
//! insists on. The app's name sits in a small label strip along the
//! bottom (elided to fit — a tile is a tile, it never widens for a
//! long name), and a running app earns a small lit indicator lamp in
//! [`panel::panel_accent`] at the face's bottom-right corner — the one
//! deliberate accent emphasis the tile grammar allows (see
//! [`tile::tile_ink`]'s doc comment), spent exactly where WindowMaker
//! spends its own running marker.

use wm_theme_api::DecorationBuffer;

use crate::model::{Color, FontSpec, FontWeight, TextAlign, Theme};
use crate::{paint, panel, tile};

/// The tile's monogram: the app name's first two initials — first
/// letters of the first two words, or the first two characters of a
/// one-word name — uppercased. Empty in, empty out (the face simply
/// shows no monogram rather than inventing one).
fn monogram(name: &str) -> String {
    let mut words = name.split_whitespace();
    let initials: Vec<char> = match (words.next(), words.next()) {
        (Some(first), Some(second)) => first.chars().take(1).chain(second.chars().take(1)).collect(),
        (Some(only), None) => only.chars().take(2).collect(),
        _ => Vec::new(),
    };
    initials.into_iter().flat_map(char::to_uppercase).collect()
}

/// Hard character-by-character elision to `max_width` shaped pixels:
/// the longest prefix of `text` that fits with a trailing ellipsis,
/// or `text` itself when the whole name fits. Measured with
/// [`paint::text_width`] (the real shaped width, not an estimate) so
/// "fits" means fits. The candidate is pre-capped at 40 characters —
/// no tile-width strip can show more — so a pathological name doesn't
/// buy a measurement per character of its whole length.
fn elide_to_width(font_system: &mut cosmic_text::FontSystem, font: &FontSpec, text: &str, max_width: u32) -> String {
    if paint::text_width(font_system, font, text) <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().take(40).collect();
    for keep in (0..chars.len()).rev() {
        let mut candidate: String = chars[..keep].iter().collect();
        candidate.push('\u{2026}');
        if paint::text_width(font_system, font, &candidate) <= max_width {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

/// The label strip's top edge (tile-local y) for a tile of `size` —
/// the same margin/proportion recipe as `icon::render_icon_tile`'s
/// caption, shared between the renderer and its overflow test so the
/// two can never disagree about where the face ends.
fn label_top(theme: &Theme, size: u32) -> i32 {
    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let label_h = ((size as f32) * 0.22).round().max(9.0) as i32;
    size as i32 - margin - label_h
}

/// Rasterizes one launcher tile: `size` x `size`, monogram on the
/// face, name in the bottom label strip, and — when `running` — the
/// lit indicator lamp at the face's bottom-right.
pub fn render_launcher_tile(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    size: u32,
    name: &str,
    running: bool,
) -> DecorationBuffer {
    let size = size.max(1);
    let Some(mut pixmap) = tiny_skia::Pixmap::new(size, size) else {
        return DecorationBuffer { width: 0, height: 0, pixels: Vec::new() };
    };

    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);

    // Geometry mirrors the icon tile's: the face down to a label strip
    // along the bottom, with `margin` keeping content clear of the
    // tile's own relief. Where the icon tile spends the face on a
    // sunken preview well, the launcher spends it on the monogram —
    // an open face, no well: the tile reads "press me" rather than
    // "look through me".
    let t = theme.tile.bevel.width.max(1) as i32;
    let margin = t + (size as i32 / 28).max(1);
    let label_y = label_top(theme, size);
    let face_w = (size as i32 - margin * 2).max(1) as u32;
    let face_h = (label_y - margin).max(1) as u32;
    let ink = tile::tile_ink(theme);

    // The monogram: two initials at a commanding size, centered in the
    // face. Bold on purpose — at dock scale this is the tile's whole
    // identity, and a lightweight monogram reads as a watermark.
    let initials = monogram(name);
    if !initials.is_empty() {
        let monogram_font = FontSpec {
            size: (face_h as f32 * 0.60).max(8.0),
            weight: FontWeight::Bold,
            ..theme.titlebar.font.clone()
        };
        paint::draw_text(
            &mut pixmap,
            font_system,
            swash_cache,
            &initials,
            &monogram_font,
            ink,
            margin,
            margin,
            face_w,
            face_h,
            TextAlign::Center,
        );
    }

    // The label: the app's name, small and hard-elided to the strip —
    // a caption under the monogram, same role as the icon tile's
    // title caption.
    let label_h = (size as i32 - margin - label_y).max(0) as u32;
    let label_font = FontSpec { size: (label_h as f32 * 0.60).max(6.0), ..theme.titlebar.font.clone() };
    let label = elide_to_width(font_system, &label_font, name, face_w.saturating_sub(2 * t as u32).max(1));
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &label,
        &label_font,
        ink,
        margin,
        label_y,
        face_w,
        label_h,
        TextAlign::Center,
    );

    // The running lamp: a hard-edged square of the theme's LED accent
    // with a thin darker inset ring, at the face's bottom-right just
    // above the label strip. Hard edges and the panel accent on
    // purpose — it is a lit instrument lamp, kin to the LED screens,
    // not vector decoration; drawn only when lit, since an unlit ghost
    // on every tile would dilute the one accent the face is allowed.
    if running {
        let lamp = (((size as f32) * 0.14).round() as i32).max(5);
        let ring = (size as i32 / 56).max(1);
        let lamp_x = size as i32 - margin - t - lamp;
        let lamp_y = label_y - t - lamp;
        let accent = panel::panel_accent(theme);
        let rim = Color::rgb((accent.r as u16 * 2 / 5) as u8, (accent.g as u16 * 2 / 5) as u8, (accent.b as u16 * 2 / 5) as u8);
        paint::fill_rect(&mut pixmap, lamp_x, lamp_y, lamp as u32, lamp as u32, accent);
        draw_square_ring(&mut pixmap, lamp_x + ring, lamp_y + ring, (lamp - 2 * ring).max(1) as u32, ring as u32, rim);
    }

    DecorationBuffer { width: size, height: size, pixels: pixmap.data().to_vec() }
}

/// A hard-edged square ring outline: `thickness`-thick sides of an
/// `edge` x `edge` square at `(x, y)`. Four flat fills, no
/// anti-aliasing — lamp bezels are hardware, not vector art.
fn draw_square_ring(pixmap: &mut tiny_skia::Pixmap, x: i32, y: i32, edge: u32, thickness: u32, color: Color) {
    let e = edge as i32;
    let th = thickness.min(edge);
    paint::fill_rect(pixmap, x, y, edge, th, color);
    paint::fill_rect(pixmap, x, y + e - th as i32, edge, th, color);
    paint::fill_rect(pixmap, x, y, th, edge, color);
    paint::fill_rect(pixmap, x + e - th as i32, y, th, edge, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    fn render(size: u32, name: &str, running: bool) -> DecorationBuffer {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        render_launcher_tile(&theme, &mut font_system, &mut swash_cache, size, name, running)
    }

    #[test]
    fn renders_at_dock_sizes() {
        for size in [56u32, 112] {
            let buffer = render(size, "Firefox", false);
            assert_eq!((buffer.width, buffer.height), (size, size));
            assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn running_lights_the_lamp() {
        let idle = render(56, "Firefox", false);
        let running = render(56, "Firefox", true);
        assert_ne!(idle.pixels, running.pixels, "the running lamp must visibly change the tile");
    }

    #[test]
    fn monogram_takes_the_first_two_words_initials() {
        assert_eq!(monogram("Web Browser"), "WB");
        assert_eq!(monogram("GNU Image Manipulation Program"), "GI", "only the first two words vote");
    }

    #[test]
    fn monogram_takes_a_one_word_names_first_two_characters() {
        assert_eq!(monogram("firefox"), "FI");
    }

    #[test]
    fn monogram_of_a_single_character_name_is_that_character() {
        assert_eq!(monogram("x"), "X");
    }

    #[test]
    fn monogram_of_an_empty_name_is_empty() {
        assert_eq!(monogram(""), "");
        assert_eq!(monogram("   "), "", "whitespace-only is as empty as empty");
    }

    /// Elision, pinned at the pixel: a long name must be confined to
    /// the label strip, never spilling onto the face. Two names with
    /// the same monogram (both one-word, both starting "Fi") are
    /// rendered and every pixel above the label strip — minus a
    /// two-row halo for glyph rasterization slack, the same tolerance
    /// family `icon.rs`'s overflow test grants — must be identical:
    /// the only thing allowed to change with name length is the strip
    /// itself.
    #[test]
    fn long_names_elide_into_the_label_strip_leaving_the_face_untouched() {
        let size = 56u32;
        let short = render(size, "Fireweasel", false);
        let long = render(size, "Fireweaselbrowserwithanextremelyoverlongname", false);
        assert_ne!(short.pixels, long.pixels, "the label strip itself should show different (elided) text");

        let theme = nextstep_classic();
        let face_bottom = (label_top(&theme, size) - 2).max(0) as u32;
        let row_bytes = (size * 4) as usize;
        for y in 0..face_bottom {
            let range = y as usize * row_bytes..(y as usize + 1) * row_bytes;
            assert_eq!(
                short.pixels[range.clone()],
                long.pixels[range],
                "face row {y} changed with name length: the label must elide, not overflow"
            );
        }
    }
}
