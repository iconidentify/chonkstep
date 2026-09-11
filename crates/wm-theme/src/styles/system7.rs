//! Measured System 7.5 monochrome document chrome. The OS-captured reference
//! fixtures, rather than this painter, own its pixel specification.
mod atlas;
mod fallback;
pub(crate) use fallback::Fallback;

use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use wm_theme_api::{ButtonKind, DecorationBuffer, DecorationLayout, DecorationPart,
    DecorationRequest, DecorationSurface, Point, Rect, ResizeEdge, Size};
use crate::model::{Color, Theme};

#[derive(Clone, Copy)]
pub(crate) struct Roles { paper: [u8; 4], ink: [u8; 4] }

impl Roles {
    pub(crate) fn from_theme(theme: &Theme) -> Self {
        if theme.id == "nextstep-classic" {
            return Self { paper: [255; 4], ink: [0, 0, 0, 255] };
        }
        let (bg, fg) = (theme.terminal.bg, theme.terminal.fg);
        let (paper, ink) = if bg.relative_luminance() >= fg.relative_luminance() { (bg, fg) } else { (fg, bg) };
        let paper = paper.mix(Color::rgb(255, 255, 255), 0.875);
        let ink = ink.mix(Color::rgb(0, 0, 0), 0.5);
        Self { paper: [paper.r, paper.g, paper.b, 255], ink: [ink.r, ink.g, ink.b, 255] }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Metrics { scale: f32, integer_scale: u32, line: u32, title: u32, margin: u32 }

impl Metrics {
    pub(crate) fn new(scale: f32) -> Self {
        // Session outputs are already bounded; keep the offline/public theme
        // API safe when handed an absurd finite scale as well.
        let scale = super::super::raster::normalized_scale(scale).min(16.0);
        let integer_scale = if scale.fract() == 0.0 { scale as u32 } else { 0 };
        let r = |value| if integer_scale > 0 { value * integer_scale } else { rounded(value, scale) };
        Self { scale, integer_scale, line: r(1).max(1), title: r(18), margin: r(4).max(1) }
    }
    fn r(self, value: u32) -> u32 {
        if self.integer_scale > 0 { value * self.integer_scale } else { rounded(value, self.scale) }
    }
}

fn rounded(value: u32, scale: f32) -> u32 { (value as f32 * scale + 0.5).floor() as u32 }

pub(crate) fn layout(request: &DecorationRequest, m: Metrics) -> DecorationLayout {
    let content = wm_theme_api::clamp_client_size(request.content_size,
        Size::new(wm_theme_api::MAX_CLIENT_WINDOW_DIMENSION, wm_theme_api::MAX_CLIENT_WINDOW_DIMENSION));
    let margin = if request.resizable { m.margin } else { 0 };
    let size = Size::new(content.w.saturating_add(m.line * 3 + margin * 2),
        content.h.saturating_add(m.title + m.line * 3 + margin * 2));
    let mut result = DecorationLayout {
        frame_size: size,
        input_margin: margin,
        client_offset: Point::new((margin + m.line) as i32, (margin + m.line + m.title) as i32),
        titlebar_height: m.title,
        shaded_frame_height: m.title + m.line * 2 + margin * 2,
        button_hitboxes: Vec::with_capacity(2),
        resize_hitboxes: Vec::with_capacity(if request.resizable { 12 } else { 0 }),
    };
    let box_size = m.r(11).max(1);
    let visible_w = size.w.saturating_sub(margin * 2);
    let close_x = m.r(9);
    let zoom_x = visible_w.saturating_sub(m.line + m.r(20));
    // Keep every hitbox inside the visible bar, and never overlap controls on
    // a client whose constraints allow a narrower-than-classic frame.
    if close_x + box_size + m.line < visible_w {
        result.button_hitboxes.push((ButtonKind::Close, Rect::new(
            Point::new((margin + close_x) as i32, (margin + m.r(4)) as i32), Size::new(box_size, box_size))));
        if request.resizable && zoom_x >= close_x + box_size + m.r(2) {
            result.button_hitboxes.push((ButtonKind::Maximize, Rect::new(
                Point::new((margin + zoom_x) as i32, (margin + m.r(4)) as i32), Size::new(box_size, box_size))));
        }
    }
    if request.resizable {
        let w = size.w;
        let h = size.h;
        let corner_w = m.r(28).min(w / 2);
        let corner_h = m.r(28).min(h / 2);
        let left = (margin + m.line).min(corner_w);
        let right = (margin + m.line * 2).min(corner_w);
        let top = (margin + m.line).min(corner_h);
        let bottom = (margin + m.line * 2).min(corner_h);
        // L-shaped corners occupy only the ring/outline/shadow. A square
        // corner would steal the close button and part of the client surface.
        let hitboxes = [
            (ResizeEdge::NorthWest, 0, 0, corner_w, top),
            (ResizeEdge::NorthWest, 0, 0, left, corner_h),
            (ResizeEdge::NorthEast, w - corner_w, 0, corner_w, top),
            (ResizeEdge::NorthEast, w - right, 0, right, corner_h),
            (ResizeEdge::SouthWest, 0, h - bottom, corner_w, bottom),
            (ResizeEdge::SouthWest, 0, h - corner_h, left, corner_h),
            (ResizeEdge::SouthEast, w - corner_w, h - bottom, corner_w, bottom),
            (ResizeEdge::SouthEast, w - right, h - corner_h, right, corner_h),
            (ResizeEdge::North, corner_w, 0, w - corner_w * 2, top),
            (ResizeEdge::South, corner_w, h - bottom, w - corner_w * 2, bottom),
            (ResizeEdge::West, 0, corner_h, left, h - corner_h * 2),
            (ResizeEdge::East, w - right, corner_h, right, h - corner_h * 2),
        ].map(|(edge, x, y, cw, ch)| (edge, Rect::new(Point::new(x as i32, y as i32), Size::new(cw, ch))));
        if w > corner_w * 2 && h > corner_h * 2 {
            // The ordinary frame has all twelve rectangles. Copy the bounded
            // array directly, without twelve independent capacity branches.
            result.resize_hitboxes.extend_from_slice(&hitboxes);
        } else {
            result.resize_hitboxes.extend(hitboxes.into_iter().filter(|(_, rect)| rect.size.w > 0 && rect.size.h > 0));
        }
    }
    result
}

pub(crate) fn render_sparse(
    roles: Roles,
    scale: f32,
    title_cache: &mut VecDeque<(u32, DecorationRequest, DecorationBuffer)>,
    fallback: &mut Fallback,
    request: &DecorationRequest,
    layout: &DecorationLayout,
) -> DecorationSurface {
    let m = Metrics::new(scale);
    let visual = layout.visual_bounds();
    let w = visual.size.w;
    let top_h = (m.line + m.title).min(visual.size.h);
    let shaded = layout.frame_size.h == layout.shaded_frame_height;
    let bottom_h = (if shaded { m.line } else { m.line * 2 }).min(visual.size.h.saturating_sub(top_h));
    let mut parts = Vec::with_capacity(4);
    let mut title_key = request.clone();
    title_key.content_size.h = 0;
    // A shade retains the original title's zoom control. The render-only
    // request clears resizable to suppress resize bars in legacy recipes.
    title_key.resizable = layout.button_hitboxes.iter().any(|(kind, _)| *kind == ButtonKind::Maximize);
    for button in &mut title_key.buttons { button.hovered = false; }
    let cached = title_cache.iter().find(|(key, cached, _)| *key == scale.to_bits() && cached == &title_key);
    let top = if let Some((_, _, buffer)) = cached { buffer.clone() } else {
        let top = paint_title(roles, m, w, top_h, &title_key, layout, fallback);
        title_cache.push_back((scale.to_bits(), title_key, top.clone()));
        while title_cache.len() > 16 { title_cache.pop_front(); }
        top
    };
    parts.push(DecorationPart { offset: visual.pos, buffer: top });
    if bottom_h > 0 {
        let mut bottom = filled(w, bottom_h, roles.ink);
        // Drop shadow is displaced right by one outline width. Its bottom
        // left corner is outside the window shape, revealing the desktop.
        fill(&mut bottom, 0, bottom_h.saturating_sub(m.line), m.line, m.line, [0; 4]);
        parts.push(DecorationPart { offset: Point::new(visual.pos.x, visual.pos.y + (visual.size.h - bottom_h) as i32), buffer: bottom });
    }
    let content_h = visual.size.h.saturating_sub(top_h + bottom_h);
    if content_h > 0 {
        for (x, width) in [(0, m.line), (w.saturating_sub(m.line * 2), m.line * 2)] {
            parts.push(DecorationPart { offset: Point::new(visual.pos.x + x as i32, visual.pos.y + top_h as i32),
                buffer: filled(width.min(w), content_h, roles.ink) });
        }
    }
    DecorationSurface { frame_size: layout.frame_size, parts }
}

pub(crate) fn flatten(surface: DecorationSurface) -> DecorationBuffer {
    let mut result = filled(surface.frame_size.w, surface.frame_size.h, [0; 4]);
    for part in surface.parts {
        for y in 0..part.buffer.height {
            let from = (y * part.buffer.width * 4) as usize;
            let to = (((part.offset.y as u32 + y) * result.width + part.offset.x as u32) * 4) as usize;
            result.pixels[to..to + part.buffer.width as usize * 4].copy_from_slice(&part.buffer.pixels[from..from + part.buffer.width as usize * 4]);
        }
    }
    result
}

fn paint_title(roles: Roles, m: Metrics, w: u32, h: u32, request: &DecorationRequest, layout: &DecorationLayout, fallback: &mut Fallback) -> DecorationBuffer {
    let mut top = filled(w, h, roles.ink);
    fill(&mut top, m.line, m.line, w.saturating_sub(m.line * 3), h.saturating_sub(m.line * 2), roles.paper);
    fill(&mut top, w.saturating_sub(m.line), 0, m.line, m.line, [0; 4]);
    if request.focused {
        let pitch = m.r(2).max(m.line + 1);
        for stripe in 0..6 {
            fill(&mut top, m.r(2), m.r(4) + stripe * pitch, w.saturating_sub(m.r(2) + m.line * 3), m.line, roles.ink);
        }
        for &(kind, rect) in &layout.button_hitboxes {
            let x = (rect.pos.x - layout.input_margin as i32).max(0) as u32;
            let y = (rect.pos.y - layout.input_margin as i32).max(0) as u32;
            fill(&mut top, x.saturating_sub(m.line), y, rect.size.w + m.line * 2, rect.size.h, roles.paper);
            let pressed = request.buttons.iter().any(|button| button.kind == kind && button.pressed);
            for dy in 0..rect.size.h {
                for dx in 0..rect.size.w {
                    let sx = ((dx as f32 / m.scale) as u32).min(10);
                    let sy = ((dy as f32 / m.scale) as u32).min(10);
                    let ink = if pressed { PRESSED[sy as usize] & (1 << (10 - sx)) != 0 }
                        else { sx == 0 || sx == 10 || sy == 0 || sy == 10 ||
                            (kind == ButtonKind::Maximize && ((sx == 6 && sy <= 6) || (sy == 6 && sx <= 6))) };
                    if ink { pixel(&mut top, x + dx, y + dy, roles.ink); }
                }
            }
        }
    }
    let content_w = request.content_size.w.min(wm_theme_api::MAX_CLIENT_WINDOW_DIMENSION);
    let limit = content_w.saturating_sub(m.r(64));
    // Latin-1 takes the allocation-free atlas path. Consecutive uncovered
    // graphemes are shaped together, preserving joining within Unicode runs
    // and keeping a decomposed accent attached to its base character.
    let mut fallback_runs = Vec::new();
    let mut measured = 0u32;
    for (offset, text, covered) in spans(&request.title) {
        if measured >= limit { break; }
        let width = if covered {
            text.chars().fold(0u32, |width, ch| width.saturating_add(m.r(atlas::glyph(ch).unwrap().advance)))
        } else {
            let mask = fallback.render(text, ((limit - measured) as f32 / m.scale).ceil() as u32);
            let width = m.r(mask.width);
            fallback_runs.push((offset, mask));
            width
        };
        measured = measured.saturating_add(width);
    }
    let advance = measured.min(limit);
    if advance > 0 {
        // Center on the source pixel grid at integer scales: centering after
        // replication shifts odd-advance titles by half a source pixel.
        let remainder = content_w - advance;
        let centered = if m.scale.fract() == 0.0 {
            remainder / (2 * m.scale as u32) * m.scale as u32
        } else { remainder / 2 };
        let x = m.line + centered;
        let paper_left = x.saturating_sub(m.r(6));
        let reserve = m.r(if request.resizable { 29 } else { 28 });
        let paper_right = (x + advance + m.r(6)).min(w.saturating_sub(reserve));
        fill(&mut top, paper_left, m.line, paper_right.saturating_sub(paper_left), h.saturating_sub(m.line * 2), roles.paper);
        let mut pen = x;
        let cell_y = m.r(14).saturating_sub(m.r(12));
        for (offset, text, covered) in spans(&request.title) {
            if pen >= x + advance { break; }
            if covered {
                for ch in text.chars() {
                    if pen >= x + advance { break; }
                    let glyph = atlas::glyph(ch).unwrap();
                    let width = m.r(glyph.advance);
                    for dy in 0..m.r(15) {
                        let sy = ((dy as f32 / m.scale) as usize).min(14);
                        for dx in 0..width.min(x + advance - pen) {
                            let sx = ((dx as f32 / m.scale) as u32).min(glyph.advance - 1);
                            if glyph.rows[sy] & (1 << (glyph.advance - 1 - sx)) != 0 {
                                pixel(&mut top, pen + dx, cell_y + dy, roles.ink);
                            }
                        }
                    }
                    pen += width;
                }
            } else if let Some((_, mask)) = fallback_runs.iter().find(|(start, _)| *start == offset) {
                let width = m.r(mask.width);
                for dy in 0..m.r(15) {
                    let sy = ((dy as f32 / m.scale) as usize).min(14);
                    for dx in 0..width.min(x + advance - pen) {
                        let sx = ((dx as f32 / m.scale) as u32).min(mask.width - 1);
                        if mask.pixels[sy * mask.width as usize + sx as usize] {
                            pixel(&mut top, pen + dx, cell_y + dy, roles.ink);
                        }
                    }
                }
                pen += width;
            }
        }
    }
    top
}

const PRESSED: [u16; 11] = [0x7ff, 0x421, 0x525, 0x4a9, 0x401, 0x78f, 0x401, 0x4a9, 0x525, 0x421, 0x7ff];

fn spans(mut text: &str) -> impl Iterator<Item = (usize, &str, bool)> {
    let mut offset = 0;
    std::iter::from_fn(move || {
        let is_covered = |cluster: &str| cluster.chars().all(|ch| atlas::glyph(ch).is_some());
        let covered = is_covered(text.graphemes(true).next()?);
        let end = text.grapheme_indices(true).find(|(_, cluster)| is_covered(cluster) != covered).map_or(text.len(), |(i, _)| i);
        let result = (offset, &text[..end], covered);
        text = &text[end..];
        offset += end;
        Some(result)
    })
}

fn filled(width: u32, height: u32, color: [u8; 4]) -> DecorationBuffer {
    let mut pixels = vec![0; width as usize * height as usize * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 { *pixel = color; }
    DecorationBuffer { width, height, pixels }
}

fn fill(buffer: &mut DecorationBuffer, x: u32, y: u32, w: u32, h: u32, color: [u8; 4]) {
    let end_x = x.saturating_add(w).min(buffer.width);
    for y in y..y.saturating_add(h).min(buffer.height) {
        if x >= end_x { break; }
        let start = (y as usize * buffer.width as usize + x as usize) * 4;
        let end = (y as usize * buffer.width as usize + end_x as usize) * 4;
        for pixel in buffer.pixels[start..end].as_chunks_mut::<4>().0 { *pixel = color; }
    }
}

fn pixel(buffer: &mut DecorationBuffer, x: u32, y: u32, color: [u8; 4]) {
    if x < buffer.width && y < buffer.height {
        let start = (y as usize * buffer.width as usize + x as usize) * 4;
        buffer.pixels[start..start + 4].copy_from_slice(&color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DecorationStyle, FontState, RasterThemeEngine};
    use wm_theme_api::{ButtonRuntimeState, ThemeEngine};

    #[test]
    fn fallback_spans_preserve_decomposed_accents_and_emoji_clusters() {
        assert_eq!(spans("Cafe\u{301} — 🧑‍💻").collect::<Vec<_>>(), [
            (0, "Caf", true), (3, "e\u{301}", false), (6, " ", true), (7, "—", false),
            (10, " ", true), (11, "🧑‍💻", false),
        ]);
    }

    #[test]
    fn unicode_missing_fonts_and_hover_keep_one_bit_pixels_and_cached_titles() {
        let engine = RasterThemeEngine::with_fonts(crate::default_theme::nextstep_classic(), FontState::new())
            .with_style(DecorationStyle::System7).unwrap();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for title in ["Café Ångström", "Cafe\u{301} — Живет 中文 日本語 العربية 🧑‍💻 \u{10ffff}"] {
                let mut request = DecorationRequest { content_size: Size::new(800, 600), title: title.into(),
                    focused: true, resizable: true, buttons: vec![ButtonRuntimeState {
                        kind: ButtonKind::Close, hovered: false, pressed: false,
                    }] };
                let layout = engine.layout_at(&request, scale);
                let first = engine.render_surface_at(&request, &layout, scale);
                for part in &first.parts {
                    assert!(part.buffer.pixels.as_chunks::<4>().0.iter().all(|pixel|
                        *pixel == [0, 0, 0, 255] || *pixel == [255; 4] || *pixel == [0; 4]),
                        "atlas and Unicode fallback must remain binary, including fractional output scales");
                }
                request.buttons[0].hovered = true;
                assert_eq!(engine.render_surface_at(&request, &layout, scale), first);
                request.focused = false;
                let inactive = engine.render_surface_at(&request, &layout, scale);
                assert_ne!(inactive.parts[0], first.parts[0]);
                assert_eq!(inactive.parts[1..], first.parts[1..], "focus affects only the title band");
            }
        }
        let mut fallback = Fallback::from_db("en-US".into(), cosmic_text::fontdb::Database::new());
        let missing = fallback.render("中文", 100);
        assert_eq!(missing.width, 14);
        assert!(missing.pixels.iter().any(|pixel| *pixel), "fontless machines need a visible missing-glyph mark");
    }
}
