//! Original System 7-inspired shell surfaces, using the frame's measured atlas
//! and resident Unicode fallback. These are not historical OS window goldens.
use tiny_skia::Pixmap;
use wm_theme_api::{DecorationBuffer, DecorationStyle, Point, Rect, Size};
use crate::{FontState, Theme, icon, menu, overview, paint, switcher};
use crate::model::{Color, TextAlign};
use super::{atlas, fallback::Mask, spans, Metrics, Roles};

/// A cheap session handle for window-derived shell chrome. Construct at
/// startup/look changes, share the existing font state, and retain rendered
/// captions until their text or layout changes. Legacy rendering APIs continue
/// to produce WindowMaker pixels without this handle.
#[derive(Clone)]
pub struct UiChrome {
    style: DecorationStyle,
    roles: Roles,
    metrics: Metrics,
    fonts: FontState,
}

impl UiChrome {
    pub fn new(theme: &Theme, fonts: FontState, style: DecorationStyle, scale: f32) -> Self {
        let light = (style == DecorationStyle::System7)
            .then(|| crate::default_theme::theme_variant(&theme.id, crate::Appearance::Light)).flatten();
        Self { style, roles: Roles::from_theme(light.as_ref().unwrap_or(theme)), metrics: Metrics::new(scale), fonts }
    }

    pub fn style(&self) -> DecorationStyle { self.style }

    /// Flat native Overview outline ink and one logical pixel, already scaled.
    /// `None` preserves the existing WindowMaker compositor treatment.
    pub fn overview_ink(&self) -> Option<([u8; 3], u32)> {
        (self.style == DecorationStyle::System7).then_some((
            [self.roles.ink[0], self.roles.ink[1], self.roles.ink[2]], self.metrics.line))
    }

    fn px(&self, value: u32) -> u32 { self.metrics.r(value) }
    fn ink(&self) -> Color { color(self.roles.ink) }
    fn paper(&self) -> Color { color(self.roles.paper) }

    // The signatures mirror the existing raster APIs, adding only the context.
    #[allow(clippy::too_many_arguments)]
    pub fn menu(&self, theme: &Theme, fonts: &mut cosmic_text::FontSystem,
        title: &str, items: &[menu::MenuItem], highlighted: Option<usize>, closable: bool) -> menu::MenuRender {
        if self.style == DecorationStyle::WindowMaker {
            return menu::render_menu(theme, fonts, title, items, highlighted, closable);
        }
        let (line, shadow, title_h, row_h) = (self.metrics.line, self.px(2), self.px(20), self.px(20));
        if items.len() > ((8192 - title_h - line * 2 - shadow) / row_h.max(1)) as usize {
            // Reject an unrepresentable popup as a whole. Truncating only the
            // pixels would leave keyboard navigation able to activate hidden
            // rows from the original menu model.
            return menu::MenuRender { buffer: empty(), item_rects: Vec::new(), close_rect: None };
        }
        let reserve = self.px(if closable { 44 } else { 12 });
        let title = self.text(title, 4096u32.saturating_sub(reserve));
        let labels: Vec<_> = items.iter()
            .map(|item| self.text(item.label(), 4096u32.saturating_sub(self.px(32)))).collect();
        let w = labels.iter().map(|text| text.width + self.px(32)).max().unwrap_or(0)
            .max(title.width + reserve).max(self.px(48)) + line * 2;
        let h = title_h + row_h * labels.len() as u32 + line * 2;
        let mut image = Pixmap::new(w + shadow, h + shadow).expect("bounded menu dimensions");
        self.panel(&mut image, Rect::new(Point::new(0, 0), Size::new(w, h)), true);
        let title_rect = Rect::new(Point::new(line as i32, line as i32), Size::new(w - line * 2, title_h));
        self.fill(&mut image, Rect::new(Point::new(line as i32, (line + title_h - line) as i32),
            Size::new(w - line * 2, line)), self.ink());
        let close_rect = closable.then(|| Rect::new(Point::new(self.px(6) as i32, self.px(5) as i32),
            Size::new(self.px(11), self.px(11))));
        if let Some(rect) = close_rect { self.outline(&mut image, rect, self.ink()); }
        let inset = if closable { self.px(22) } else { self.px(6) };
        self.draw_text(&mut image, &title, Rect::new(Point::new(inset as i32, title_rect.pos.y),
            Size::new(w.saturating_sub(inset * 2), title_h)), self.ink(), TextAlign::Center);
        let mut item_rects = Vec::with_capacity(items.len());
        for (i, text) in labels.iter().enumerate() {
            let row = Rect::new(Point::new(line as i32, (line + title_h + i as u32 * row_h) as i32),
                Size::new(w - line * 2, row_h));
            let selected = highlighted == Some(i);
            let ink = if selected { self.paper() } else { self.ink() };
            if selected { self.fill(&mut image, row, self.ink()); }
            self.draw_text(&mut image, text, Rect::new(Point::new(row.pos.x + self.px(8) as i32, row.pos.y),
                Size::new(row.size.w.saturating_sub(self.px(30)), row_h)), ink, TextAlign::Left);
            if items[i].is_submenu() {
                for column in 0..5 {
                    self.fill(&mut image, Rect::new(Point::new((w - self.px(12) + self.px(column)) as i32,
                        row.pos.y + ((row_h - self.px(9)) / 2 + self.px(column)) as i32),
                        Size::new(self.px(1), self.px(9 - column * 2))), ink);
                }
            }
            item_rects.push(row);
        }
        menu::MenuRender { buffer: buffer(image), item_rects, close_rect }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn icon(&self, theme: &Theme, fonts: &mut cosmic_text::FontSystem, cache: &mut cosmic_text::SwashCache,
        size: u32, title: &str, preview: Option<&DecorationBuffer>) -> DecorationBuffer {
        if self.style == DecorationStyle::WindowMaker {
            return icon::render_icon_tile(theme, fonts, cache, size, title, preview);
        }
        let size = size.clamp(1, 4096);
        let mut image = Pixmap::new(size, size).expect("bounded icon dimensions");
        self.card(&mut image, Rect::new(Point::new(0, 0), Size::new(size, size)), title, preview, false, true);
        buffer(image)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn switcher(&self, theme: &Theme, fonts: &mut cosmic_text::FontSystem, cache: &mut cosmic_text::SwashCache,
        entries: &[switcher::SwitcherEntry], selected: usize, tile: u32) -> DecorationBuffer {
        if self.style == DecorationStyle::WindowMaker {
            return switcher::render_switcher(theme, fonts, cache, entries, selected, tile);
        }
        let tile = tile.clamp(1, 4096);
        let pad = self.px(8).max(2);
        let shadow = self.px(2);
        let count = entries.len().max(1).min(((8192 - pad - shadow) / (tile + pad)).max(1) as usize);
        let selected = selected.min(entries.len().saturating_sub(1));
        let first = selected.saturating_sub(count / 2).min(entries.len().saturating_sub(count));
        let w = count as u32 * (tile + pad) + pad;
        let h = pad * 2 + tile + self.px(20);
        let mut image = Pixmap::new(w + shadow, h + shadow).expect("bounded switcher dimensions");
        self.panel(&mut image, Rect::new(Point::new(0, 0), Size::new(w, h)), true);
        for (index, entry) in entries.iter().enumerate().skip(first).take(count) {
            let x = pad + (index - first) as u32 * (tile + pad);
            let rect = Rect::new(Point::new(x as i32, pad as i32), Size::new(tile, tile));
            if index == selected {
                let ring = self.px(2).max(1);
                self.fill(&mut image, Rect::new(Point::new(rect.pos.x - ring as i32, rect.pos.y - ring as i32),
                    Size::new(tile + ring * 2, tile + ring * 2)), self.ink());
            }
            self.card(&mut image, rect, &entry.title, entry.preview.as_ref(), index == selected, true);
        }
        if let Some(entry) = entries.get(selected) {
            let text = self.text(&entry.title, w.saturating_sub(pad * 2));
            self.draw_text(&mut image, &text, Rect::new(Point::new(pad as i32, (pad + tile) as i32),
                Size::new(w - pad * 2, self.px(20))), self.ink(), TextAlign::Center);
        }
        buffer(image)
    }

    /// Build a caption only on entry/text/layout change; callers retain its
    /// pixels. The native compositor switches selection without calling here.
    #[allow(clippy::too_many_arguments)]
    pub fn label(&self, theme: &Theme, fonts: &mut cosmic_text::FontSystem, cache: &mut cosmic_text::SwashCache,
        text: &str, max_width: u32, height: u32, inverted: bool) -> DecorationBuffer {
        if self.style == DecorationStyle::WindowMaker {
            return overview::live::label(theme, fonts, cache, text, max_width, height);
        }
        let max_width = max_width.clamp(1, 8192);
        let pad = self.px(8);
        let text = self.text(text, max_width.saturating_sub(pad));
        let width = (text.width + pad).min(max_width).max(1);
        let height = height.clamp(1, 4096);
        let mut image = Pixmap::new(width, height).expect("bounded caption dimensions");
        let rect = Rect::new(Point::new(0, 0), Size::new(width, height));
        self.fill(&mut image, rect, if inverted { self.ink() } else { self.paper() });
        self.outline(&mut image, rect, self.ink());
        self.draw_text(&mut image, &text, rect, if inverted { self.paper() } else { self.ink() }, TextAlign::Center);
        buffer(image)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn overview(&self, theme: &Theme, fonts: &mut cosmic_text::FontSystem, cache: &mut cosmic_text::SwashCache,
        entries: &[overview::OverviewEntry<'_>], workspace: (usize, usize), layout: &overview::OverviewLayout) -> DecorationBuffer {
        if self.style == DecorationStyle::WindowMaker {
            return overview::render_overview(theme, fonts, cache, entries, workspace, layout);
        }
        let Some(mut image) = bounded_image(layout.panel) else { return empty(); };
        self.panel(&mut image, Rect::new(Point::new(0, 0), layout.panel), false);
        let title = self.text("Overview", layout.panel.w);
        self.draw_text(&mut image, &title, Rect::new(Point::new(0, 0), Size::new(layout.panel.w, layout.header_h)),
            self.ink(), TextAlign::Center);
        for (entry, cell) in entries.iter().zip(&layout.cells) {
            self.card(&mut image, *cell, entry.title, entry.preview, false, false);
        }
        if entries.is_empty() {
            let text = self.text("No windows on this desk", layout.grid.size.w);
            self.draw_text(&mut image, &text, layout.grid, self.ink(), TextAlign::Center);
        }
        for (index, rect) in layout.strip.iter().enumerate() {
            let selected = index == workspace.0;
            self.panel(&mut image, *rect, false);
            if selected { self.fill(&mut image, *rect, self.ink()); }
            let name = (index + 1).to_string();
            let text = self.text(&name, rect.size.w);
            self.draw_text(&mut image, &text, *rect, if selected { self.paper() } else { self.ink() }, TextAlign::Center);
            if let Some(close) = layout.workspace_close_rect(index) {
                let glyph = self.workspace_close(close.size.w);
                switcher::blit_buffer(&mut image, &glyph, close.pos.x, close.pos.y);
            }
        }
        buffer(image)
    }

    pub fn workspace_close(&self, edge: u32) -> DecorationBuffer {
        if self.style == DecorationStyle::WindowMaker { return overview::workspace_close_glyph(edge); }
        let edge = edge.clamp(1, 4096);
        let mut image = Pixmap::new(edge, edge).expect("bounded close control");
        self.panel(&mut image, Rect::new(Point::new(0, 0), Size::new(edge, edge)), false);
        let inset = self.px(4).min(edge / 3);
        let far = edge.saturating_sub(inset + self.metrics.line);
        for xy in (inset..=far).step_by(self.metrics.line as usize) {
            self.fill(&mut image, Rect::new(Point::new(xy as i32, xy as i32),
                Size::new(self.metrics.line, self.metrics.line)), self.ink());
            self.fill(&mut image, Rect::new(Point::new(xy as i32, (far + inset - xy) as i32),
                Size::new(self.metrics.line, self.metrics.line)), self.ink());
        }
        buffer(image)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn selection(&self, theme: &Theme, fonts: &mut cosmic_text::FontSystem, cache: &mut cosmic_text::SwashCache,
        entry: &overview::OverviewEntry<'_>, cell: Size, pad: u32) -> DecorationBuffer {
        if self.style == DecorationStyle::WindowMaker {
            return overview::render_selection(theme, fonts, cache, entry, cell, pad);
        }
        let ring = overview::plate_ring(pad);
        let size = Size::new(cell.w.saturating_add(ring * 2), cell.h.saturating_add(ring * 2));
        let Some(mut image) = bounded_image(size) else { return empty(); };
        self.fill(&mut image, Rect::new(Point::new(0, 0), size), self.ink());
        self.card(&mut image, Rect::new(Point::new(ring as i32, ring as i32), cell),
            entry.title, entry.preview, !entry.miniaturized, false);
        buffer(image)
    }

    fn fill(&self, image: &mut Pixmap, rect: Rect, color: Color) {
        paint::fill_rect(image, rect.pos.x, rect.pos.y, rect.size.w, rect.size.h, color);
    }

    fn outline(&self, image: &mut Pixmap, rect: Rect, color: Color) {
        let line = self.metrics.line.min(rect.size.w).min(rect.size.h);
        let (x, y, w, h) = (rect.pos.x, rect.pos.y, rect.size.w, rect.size.h);
        for rect in [Rect::new(Point::new(x, y), Size::new(w, line)),
            Rect::new(Point::new(x, y + h.saturating_sub(line) as i32), Size::new(w, line)),
            Rect::new(Point::new(x, y), Size::new(line, h)),
            Rect::new(Point::new(x + w.saturating_sub(line) as i32, y), Size::new(line, h))] {
            self.fill(image, rect, color);
        }
    }

    fn panel(&self, image: &mut Pixmap, rect: Rect, shadow: bool) {
        if shadow {
            self.fill(image, Rect::new(Point::new(rect.pos.x + self.px(2) as i32,
                rect.pos.y + self.px(2) as i32), rect.size), self.ink());
        }
        self.fill(image, rect, self.paper());
        self.outline(image, rect, self.ink());
    }

    fn card(&self, image: &mut Pixmap, rect: Rect, title: &str, preview: Option<&DecorationBuffer>, selected: bool, caption_below: bool) {
        let shadow = self.px(2).min(rect.size.w / 3).min(rect.size.h / 3);
        let rect = Rect::new(rect.pos, Size::new(rect.size.w - shadow, rect.size.h - shadow));
        self.panel(image, rect, shadow > 0);
        let line = self.metrics.line.min(rect.size.w / 2).min(rect.size.h / 2);
        let bar = self.px(18).min(rect.size.h.saturating_sub(line * 2));
        let inside_w = rect.size.w.saturating_sub(line * 2);
        let text_rect = Rect::new(Point::new(rect.pos.x + line as i32,
            rect.pos.y + if caption_below { rect.size.h.saturating_sub(bar + line) as i32 } else { line as i32 }),
            Size::new(inside_w, bar));
        let text = self.text(if title.is_empty() { "?" } else { title }, inside_w.saturating_sub(self.px(4)));
        if caption_below && selected { self.fill(image, text_rect, self.ink()); }
        if !caption_below {
            self.fill(image, Rect::new(Point::new(text_rect.pos.x, text_rect.pos.y + bar.saturating_sub(line) as i32),
                Size::new(inside_w, line)), self.ink());
            if selected {
                for stripe in 0..6 {
                    let y = text_rect.pos.y + self.px(3 + stripe * 2) as i32;
                    if y + (line as i32) < text_rect.pos.y + bar as i32 {
                        self.fill(image, Rect::new(Point::new(text_rect.pos.x, y), Size::new(inside_w, line)), self.ink());
                    }
                }
                let reserve = (text.width + self.px(8)).min(inside_w);
                self.fill(image, Rect::new(Point::new(text_rect.pos.x + ((inside_w - reserve) / 2) as i32, text_rect.pos.y),
                    Size::new(reserve, bar.saturating_sub(line))), self.paper());
            }
        }
        self.draw_text(image, &text, text_rect, if caption_below && selected { self.paper() } else { self.ink() }, TextAlign::Center);
        let preview_rect = Rect::new(Point::new(rect.pos.x + line as i32,
            rect.pos.y + if caption_below { line } else { line + bar } as i32),
            Size::new(inside_w, rect.size.h.saturating_sub(line * 2 + bar)));
        if let Some(preview) = preview {
            icon::draw_preview(image, preview, preview_rect.pos.x.max(0) as u32, preview_rect.pos.y.max(0) as u32,
                preview_rect.size.w, preview_rect.size.h);
        }
        self.outline(image, rect, self.ink());
    }

    fn text<'a>(&self, text: &'a str, max_width: u32) -> Text<'a> {
        // A malicious multi-megabyte title must not cause unbounded shaping.
        // Grapheme boundaries keep combining marks/emoji attached to their base.
        use unicode_segmentation::UnicodeSegmentation;
        let max_width = max_width.min(8192);
        let byte_limit = text.char_indices().find(|(index, _)| *index >= 16_384).map_or(text.len(), |(index, _)| index);
        let text = &text[..byte_limit];
        let end = text.grapheme_indices(true).nth(2048).map_or(text.len(), |(end, _)| end);
        let text = &text[..end];
        if text.chars().all(|ch| atlas::glyph(ch).is_some()) {
            let width: u32 = text.chars().map(|ch| self.px(atlas::glyph(ch).unwrap().advance)).sum();
            if width <= max_width { return Text { text, width, runs: Vec::new(), suffix: false }; }
            let dots = self.px(atlas::glyph('.').unwrap().advance) * 3;
            let mut width = 0;
            let mut end = 0;
            for (offset, ch) in text.char_indices() {
                let next = width + self.px(atlas::glyph(ch).unwrap().advance);
                if next > max_width.saturating_sub(dots) { break; }
                width = next;
                end = offset + ch.len_utf8();
            }
            return Text { text: &text[..end], width: (width + dots).min(max_width), runs: Vec::new(), suffix: true };
        }
        let mut runs = Vec::new();
        let mut width = 0u32;
        for (offset, span, covered) in spans(text) {
            if width >= max_width { break; }
            if covered {
                for ch in span.chars() {
                    width = width.saturating_add(self.px(atlas::glyph(ch).unwrap().advance));
                    if width >= max_width { break; }
                }
            } else {
                let mask = self.fonts.system7_fallback().render(span,
                    ((max_width - width) as f32 / self.metrics.scale).ceil() as u32);
                width = width.saturating_add(self.px(mask.width));
                runs.push((offset, mask));
            }
        }
        Text { text, width: width.min(max_width), runs, suffix: false }
    }

    fn draw_text(&self, image: &mut Pixmap, text: &Text<'_>, rect: Rect, color: Color, align: TextAlign) {
        let width = text.width.min(rect.size.w);
        let unit = self.metrics.integer_scale.max(1) as i32;
        let centered = |space: i32| space.div_euclid(unit * 2) * unit;
        let x = rect.pos.x + match align { TextAlign::Left => 0, TextAlign::Center => centered((rect.size.w - width) as i32),
            TextAlign::Right => (rect.size.w - width) as i32 };
        let y = rect.pos.y + centered(rect.size.h as i32 - self.px(15) as i32);
        let right = x.saturating_add(width as i32);
        let mut pen = x;
        let pixel_color = tiny_skia::PremultipliedColorU8::from_rgba(color.r, color.g, color.b, 255).unwrap();
        let mut draw = |pen: i32, advance: u32, ink: &dyn Fn(usize, u32) -> bool| {
            for dy in 0..self.px(15) {
                let py = y + dy as i32;
                if py < rect.pos.y || py >= rect.pos.y.saturating_add(rect.size.h as i32)
                    || py < 0 || py >= image.height() as i32 { continue; }
                let sy = ((dy as f32 / self.metrics.scale) as usize).min(14);
                for dx in 0..self.px(advance) {
                    let px = pen + dx as i32;
                    if px < 0 || px >= right || px >= image.width() as i32 { continue; }
                    let sx = ((dx as f32 / self.metrics.scale) as u32).min(advance - 1);
                    if ink(sy, sx) {
                        let offset = py as usize * image.width() as usize + px as usize;
                        image.pixels_mut()[offset] = pixel_color;
                    }
                }
            }
        };
        for (offset, span, covered) in spans(text.text) {
            if pen >= right { break; }
            if covered {
                for ch in span.chars() {
                    if pen >= right { break; }
                    let glyph = atlas::glyph(ch).unwrap();
                    draw(pen, glyph.advance, &|sy, sx| glyph.rows[sy] & (1 << (glyph.advance - 1 - sx)) != 0);
                    pen += self.px(glyph.advance) as i32;
                }
            } else if let Some((_, mask)) = text.runs.iter().find(|(start, _)| *start == offset) {
                draw(pen, mask.width, &|sy, sx| mask.pixels[sy * mask.width as usize + sx as usize]);
                pen += self.px(mask.width) as i32;
            }
        }
        if text.suffix {
            let glyph = atlas::glyph('.').unwrap();
            for _ in 0..3 {
                draw(pen, glyph.advance, &|sy, sx| glyph.rows[sy] & (1 << (glyph.advance - 1 - sx)) != 0);
                pen += self.px(glyph.advance) as i32;
            }
        }
    }
}

struct Text<'a> { text: &'a str, width: u32, runs: Vec<(usize, Mask)>, suffix: bool }
fn color(rgba: [u8; 4]) -> Color { Color::rgb(rgba[0], rgba[1], rgba[2]) }
fn buffer(image: Pixmap) -> DecorationBuffer {
    DecorationBuffer { width: image.width(), height: image.height(), pixels: image.take() }
}
fn empty() -> DecorationBuffer { DecorationBuffer { width: 0, height: 0, pixels: Vec::new() } }
fn bounded_image(size: Size) -> Option<Pixmap> {
    if size.w > 8192 || size.h > 8192 { return None; }
    Pixmap::new(size.w.max(1), size.h.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_chrome_has_only_binary_ink_paper_and_exact_shadow_holes_at_all_scales() {
        let fonts = FontState::new();
        for scale in [1.0, 1.5, 2.0] {
            let theme = crate::default_theme::nextstep_classic().scaled(scale);
            let chrome = UiChrome::new(&theme, fonts.clone(), DecorationStyle::System7, scale);
            let items = [menu::MenuItem::Action { label: "Terminal".into(), action: 0 },
                menu::MenuItem::Action { label: "Résumé".into(), action: 1 }];
            let menu = chrome.menu(&theme, &mut fonts.system(), "Applications", &items, Some(1), true);
            let buffer = &menu.buffer;
            let shadow = chrome.px(2);
            for y in 0..buffer.height {
                for x in 0..buffer.width {
                    let p = &buffer.pixels[((y * buffer.width + x) * 4) as usize..][..4];
                    let visible = (x < buffer.width - shadow && y < buffer.height - shadow) || (x >= shadow && y >= shadow);
                    assert_eq!(p[3] != 0, visible);
                    assert!(p == [0; 4] || p == [255; 4] || p == [0, 0, 0, 255]);
                }
            }
            let at = |x: i32, y: i32| &buffer.pixels[((y as u32 * buffer.width + x as u32) * 4) as usize..][..4];
            for (i, row) in menu.item_rects.iter().enumerate() {
                assert_eq!(at(row.pos.x + 1, row.pos.y + 1), if i == 1 { [0, 0, 0, 255] } else { [255; 4] });
            }
        }
    }

    #[test]
    fn unicode_and_tiny_widgets_are_bounded_and_share_the_resident_fallback_while_main_fonts_are_borrowed() {
        let theme = crate::default_theme::nextstep_classic();
        let fonts = FontState::new();
        let chrome = UiChrome::new(&theme, fonts.clone(), DecorationStyle::System7, 1.0);
        let (mut fs, mut cache) = (fonts.system(), fonts.swash());
        for title in ["", "Résumé", "E\u{301}ditor", "端末 · مرحبا", "👩\u{200d}💻"] {
            for width in [0, 1, 2, 5, 17, 160] {
                let label = chrome.label(&theme, &mut fs, &mut cache, title, width, 28, true);
                assert_eq!(label.pixels.len(), label.width as usize * label.height as usize * 4);
                assert!(label.width <= width.max(1));
                assert!(label.pixels.as_chunks::<4>().0.iter().all(|p| *p == [255; 4] || *p == [0, 0, 0, 255]));
                let icon = chrome.icon(&theme, &mut fs, &mut cache, width, title, None);
                assert_eq!(icon.pixels.len(), icon.width as usize * icon.height as usize * 4);
            }
        }
        let hostile = "E\u{301}".repeat(100_000);
        let text = chrome.text(&hostile, 160);
        assert!(text.text.len() <= 16_384);
        assert!(text.width <= 160);
        let atlas = chrome.text("Terminal", 48);
        assert!(atlas.suffix);
        assert!(atlas.text.len() < "Terminal".len());
    }

    #[test]
    fn atlas_captions_replicate_the_integer_grid_and_use_the_light_palette() {
        let fonts = FontState::new();
        let theme = crate::default_theme::nextstep_classic();
        let one = UiChrome::new(&theme, fonts.clone(), DecorationStyle::System7, 1.0)
            .label(&theme, &mut fonts.system(), &mut fonts.swash(), "Terminal", 120, 28, true);
        let two = UiChrome::new(&theme, fonts.clone(), DecorationStyle::System7, 2.0)
            .label(&theme, &mut fonts.system(), &mut fonts.swash(), "Terminal", 240, 56, true);
        assert_eq!((two.width, two.height), (one.width * 2, one.height * 2));
        for y in 0..two.height { for x in 0..two.width {
            let expected = ((y / 2 * one.width + x / 2) * 4) as usize;
            let actual = ((y * two.width + x) * 4) as usize;
            assert_eq!(&two.pixels[actual..actual + 4], &one.pixels[expected..expected + 4]);
        } }
        let dark = crate::default_theme::theme_variant("amber-phosphor", crate::Appearance::Dark).unwrap();
        let light = crate::default_theme::theme_variant("amber-phosphor", crate::Appearance::Light).unwrap();
        let a = UiChrome::new(&dark, fonts.clone(), DecorationStyle::System7, 1.0);
        let b = UiChrome::new(&light, fonts, DecorationStyle::System7, 1.0);
        assert_eq!(a.roles.ink, b.roles.ink);
        assert_eq!(a.roles.paper, b.roles.paper);
    }
}
