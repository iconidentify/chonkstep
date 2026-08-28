//! The Alt-Tab switch panel (real WindowMaker's `switchpanel.c`, in
//! this theme's own visual language): a centered strip of live window
//! thumbnails — the same tiles miniaturized windows get — with the
//! selected one sitting on a highlight backdrop and its full title
//! written underneath. Pure rasterization; the desktop shell owns the
//! popup window this is blitted onto.

use tiny_skia::Pixmap;
use wm_theme_api::DecorationBuffer;

use crate::model::{Fill, Theme};
use crate::{icon, paint};

/// One switcher candidate: the window's title and, when the capture
/// succeeded, a live thumbnail of its content.
pub struct SwitcherEntry {
    pub title: String,
    pub preview: Option<DecorationBuffer>,
}

/// Lays out and rasterizes the whole panel. `tile` is the thumbnail
/// square's edge in pixels (the dock's tile size, so the panel scales
/// with `CHONKSTEP_SCALE` like everything else). `selected` is clamped
/// rather than trusted.
pub fn render_switcher(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    entries: &[SwitcherEntry],
    selected: usize,
    tile: u32,
) -> DecorationBuffer {
    let count = entries.len().max(1) as u32;
    let selected = selected.min(entries.len().saturating_sub(1));
    let pad = (tile / 7).max(6);
    // One line of the titlebar font plus breathing room — long titles
    // are elided rather than wrapped (see `elide`).
    let title_h = (theme.titlebar.font.size * 1.7).round().max(18.0) as u32;
    let width = count * tile + (count + 1) * pad;
    let height = pad + tile + title_h + pad;

    let Some(mut pixmap) = Pixmap::new(width, height) else {
        return DecorationBuffer { width: 0, height: 0, pixels: Vec::new() };
    };

    paint::fill_area(&mut pixmap, 0, 0, width, height, &theme.menu.background);

    // The selected entry's backdrop: the menu highlight fill, inflated
    // half a pad beyond the tile on every side so it reads as a plate
    // the thumbnail sits on, with the same relief the rest of the
    // chrome uses.
    let bevel_t = theme.menu.bevel.width.max(1) as u32;
    let ring = pad / 2;
    for (index, entry) in entries.iter().enumerate() {
        let x = (pad + index as u32 * (tile + pad)) as i32;
        let y = pad as i32;
        if index == selected {
            let hx = x - ring as i32;
            let hy = y - ring as i32;
            let hs = tile + ring * 2;
            paint::fill_area(&mut pixmap, hx, hy, hs, hs, &theme.menu.highlight_background);
            paint::draw_raised2_bevel(&mut pixmap, hx, hy, hs, hs, bevel_t);
        }
        let tile_buffer = icon::render_icon_tile(theme, font_system, swash_cache, tile, &entry.title, entry.preview.as_ref());
        blit_buffer(&mut pixmap, &tile_buffer, x, y);
    }

    // The whole panel gets the raised relief last so its edge reads
    // above the highlight plate, exactly like a menu's own frame.
    paint::draw_raised2_bevel(&mut pixmap, 0, 0, width, height, bevel_t);

    if let Some(entry) = entries.get(selected) {
        let label = elide(&entry.title, width.saturating_sub(pad * 2), theme.titlebar.font.size);
        paint::draw_text(
            &mut pixmap,
            font_system,
            swash_cache,
            &label,
            &theme.titlebar.font,
            theme.menu.text_color,
            pad as i32,
            (pad + tile) as i32,
            width.saturating_sub(pad * 2),
            title_h,
            crate::model::TextAlign::Center,
        );
    }

    DecorationBuffer { width, height, pixels: pixmap.data().to_vec() }
}

/// Shortens `title` to roughly what fits on one line of `font_size`
/// within `width`, eliding the middle — the start and end of a window
/// title are usually both meaningful (app name, document name), the
/// middle least so. The 0.75 average-advance estimate suits the bold
/// titlebar face; `draw_text` clips whatever still overflows.
fn elide(title: &str, width: u32, font_size: f32) -> String {
    let fits = ((width as f32) / (font_size * 0.75)).max(4.0) as usize;
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= fits {
        return title.to_string();
    }
    let keep = fits.saturating_sub(3);
    let head = keep / 2 + keep % 2;
    let tail = keep / 2;
    let mut out: String = chars[..head].iter().collect();
    out.push_str("...");
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// The panel's own window background color for the shell window that
/// hosts it — the menu background's solid color (gradients fall back
/// to their start color; only ever a pre-first-paint fallback anyway).
pub fn panel_background(theme: &Theme) -> (u8, u8, u8) {
    match &theme.menu.background {
        Fill::Solid(c) => (c.r, c.g, c.b),
        Fill::Gradient(g) => (g.from.r, g.from.g, g.from.b),
    }
}

/// Copies an opaque RGBA `DecorationBuffer` into `pixmap` at `(x, y)`,
/// clipping to the destination. Both sides are straight RGBA at full
/// alpha (see `paint::draw_text`'s doc comment for why that equals
/// tiny-skia's premultiplied storage here).
fn blit_buffer(pixmap: &mut Pixmap, buffer: &DecorationBuffer, x: i32, y: i32) {
    let (dw, dh) = (pixmap.width() as i32, pixmap.height() as i32);
    let pixels = pixmap.pixels_mut();
    for row in 0..buffer.height as i32 {
        let dy = y + row;
        if dy < 0 || dy >= dh {
            continue;
        }
        for col in 0..buffer.width as i32 {
            let dx = x + col;
            if dx < 0 || dx >= dw {
                continue;
            }
            let src = ((row as u32 * buffer.width + col as u32) * 4) as usize;
            let (r, g, b) = (buffer.pixels[src], buffer.pixels[src + 1], buffer.pixels[src + 2]);
            if let Some(px) = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, 255) {
                pixels[(dy * dw + dx) as usize] = px;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(n: usize) -> Vec<SwitcherEntry> {
        (0..n).map(|i| SwitcherEntry { title: format!("window {i}"), preview: None }).collect()
    }

    #[test]
    fn panel_sizes_to_the_entry_count() {
        let theme = crate::default_theme::nextstep_classic();
        let mut fs = cosmic_text::FontSystem::new();
        let mut sc = cosmic_text::SwashCache::new();
        let tile = 56;
        let pad = (tile / 7).max(6);

        let three = render_switcher(&theme, &mut fs, &mut sc, &entries(3), 0, tile);
        assert_eq!(three.width, 3 * tile + 4 * pad);
        assert_eq!(three.pixels.len(), (three.width * three.height * 4) as usize);

        let one = render_switcher(&theme, &mut fs, &mut sc, &entries(1), 0, tile);
        assert!(one.width < three.width);
    }

    #[test]
    fn selected_entry_gets_a_visibly_different_backdrop() {
        let theme = crate::default_theme::nextstep_classic();
        let mut fs = cosmic_text::FontSystem::new();
        let mut sc = cosmic_text::SwashCache::new();
        let tile = 56u32;
        let pad = (tile / 7).max(6);

        let panel = render_switcher(&theme, &mut fs, &mut sc, &entries(2), 0, tile);
        // Sample just outside each tile's top-left corner — inside the
        // selected entry's highlight ring, plain background for the
        // unselected one.
        let sample = |index: u32| {
            let x = pad + index * (tile + pad) - pad / 4;
            let y = pad - pad / 4;
            let i = ((y * panel.width + x) * 4) as usize;
            (panel.pixels[i], panel.pixels[i + 1], panel.pixels[i + 2])
        };
        assert_ne!(sample(0), sample(1), "selected ring must differ from the plain panel background");
    }

    #[test]
    fn out_of_range_selection_is_clamped_not_panicking() {
        let theme = crate::default_theme::nextstep_classic();
        let mut fs = cosmic_text::FontSystem::new();
        let mut sc = cosmic_text::SwashCache::new();
        let panel = render_switcher(&theme, &mut fs, &mut sc, &entries(2), 99, 56);
        assert!(panel.width > 0);
    }
}
