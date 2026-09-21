//! WarpSans Bold 9pt captions and Helv 8pt menu text, both at 96 dpi. Source provenance and the capture oracle are in
//! docs/decoration-styles/os2warp.md. Bitmap pixels replicate at integer scales.
use super::{fill, Metrics};
use crate::{model::Color, FontState};
use wm_theme_api::{DecorationBuffer, Rect};

const REGULAR: &[u8; 46031] = include_bytes!("regular.atlas");
const BOLD: &[u8; 46031] = include_bytes!("bold.atlas");

fn glyph(ch: char, bold: bool) -> Option<&'static [u8]> {
    let index = match ch as u32 {
        32..=126 => ch as usize - 32,
        160..=255 => ch as usize - 160 + 95,
        _ => return None,
    };
    let atlas = if bold { BOLD } else { REGULAR };
    Some(&atlas[index * 241..(index + 1) * 241])
}

fn bounded(text: &str) -> &str {
    // Also bound a pathological single grapheme with thousands of marks.
    let end = text
        .char_indices()
        .nth(2048)
        .map_or(text.len(), |(end, _)| end);
    &text[..end]
}

fn spans(mut text: &str, bold: bool) -> impl Iterator<Item = (&str, bool)> {
    use unicode_segmentation::UnicodeSegmentation;
    std::iter::from_fn(move || {
        let covered = |cluster: &str| cluster.chars().all(|ch| glyph(ch, bold).is_some());
        let first = covered(text.graphemes(true).next()?);
        let end = text
            .grapheme_indices(true)
            .find(|(_, cluster)| covered(cluster) != first)
            .map_or(text.len(), |(end, _)| end);
        let result = (&text[..end], first);
        text = &text[end..];
        Some(result)
    })
}

// Match measurement and drawing for uncovered Unicode. The resident fallback
// paints the actual cluster into this bounded cell, never dropping its text.
pub(super) fn width(text: &str, bold: bool) -> u32 {
    use unicode_segmentation::UnicodeSegmentation;
    bounded(text)
        .graphemes(true)
        .map(|cluster| {
            if cluster.chars().all(|ch| glyph(ch, bold).is_some()) {
                cluster
                    .chars()
                    .map(|ch| u32::from(glyph(ch, bold).unwrap()[0]))
                    .sum()
            } else {
                16
            }
        })
        .sum::<u32>()
        .min(8192)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    image: &mut DecorationBuffer,
    fonts: &FontState,
    text: &str,
    rect: Rect,
    color: Color,
    bold: bool,
    m: Metrics,
) {
    use unicode_segmentation::UnicodeSegmentation;
    let mut pen = rect.pos.x;
    let right = rect.pos.x.saturating_add(rect.size.w as i32);
    let unit = if m.scale.fract() == 0.0 {
        m.scale as i32
    } else {
        1
    };
    let y = rect.pos.y + (rect.size.h as i32 - m.px(14) as i32).div_euclid(2 * unit) * unit;
    let mut advance = 0;
    for (cluster, covered) in spans(bounded(text), bold) {
        if pen >= right {
            break;
        }
        if covered {
            for ch in cluster.chars() {
                if pen >= right {
                    break;
                }
                let glyph = glyph(ch, bold).unwrap();
                for dy in 0..m.px(14) {
                    for dx in 0..m.px(u32::from(glyph[0])) {
                        let sx = (dx as f32 / m.scale) as usize;
                        let sy = ((dy as f32 / m.scale) as usize).min(14);
                        let a = glyph[1 + sy * 16 + sx.min(15)];
                        blend(image, pen + dx as i32, y + dy as i32, rect, color, a);
                    }
                }
                advance += u32::from(glyph[0]);
                pen = rect.pos.x + m.px(advance) as i32;
            }
        } else {
            // Shape adjacent uncovered clusters together, retaining Arabic
            // joining and combining marks in the already-resident font set.
            let cells = (cluster.graphemes(true).count() as u32 * 16).min(8192);
            let available = ((right - pen).max(0) as f32 / m.scale).ceil() as u32;
            let mask = fonts
                .system7_fallback()
                .render(cluster, cells.min(available).max(1));
            for dy in 0..m.px(14) {
                for dx in 0..m.px(mask.width) {
                    let sx = ((dx as f32 / m.scale) as u32).min(mask.width - 1);
                    let sy = ((dy as f32 / m.scale) as usize).min(14);
                    if mask.pixels[sy * mask.width as usize + sx as usize] {
                        blend(image, pen + dx as i32, y + dy as i32, rect, color, 255);
                    }
                }
            }
            advance += cells;
            pen = rect.pos.x + m.px(advance) as i32;
        }
    }
}

fn blend(image: &mut DecorationBuffer, x: i32, y: i32, clip: Rect, color: Color, alpha: u8) {
    if alpha == 0
        || x < 0
        || y < 0
        || x >= image.width as i32
        || y >= image.height as i32
        || !clip.contains(wm_theme_api::Point::new(x, y))
    {
        return;
    }
    let i = ((y as u32 * image.width + x as u32) * 4) as usize;
    let mix = |a: u8, b: u8| {
        ((u32::from(a) * u32::from(alpha) + u32::from(b) * (255 - u32::from(alpha)) + 127) / 255)
            as u8
    };
    let c = Color::rgb(
        mix(color.r, image.pixels[i]),
        mix(color.g, image.pixels[i + 1]),
        mix(color.b, image.pixels[i + 2]),
    );
    fill(image, x as u32, y as u32, 1, 1, c);
}
