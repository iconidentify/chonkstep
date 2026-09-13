//! Modern menus, previews, captions and switchers. No system queries or
//! scheduling: callers retain these buffers until their inputs change.
use crate::model::TextAlign;
use crate::{
    menu,
    modern::{self, Chrome},
    overview, paint, switcher, FontState, Theme,
};
use tiny_skia::Pixmap;
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

#[derive(Clone)]
pub(crate) struct ModernUi {
    chrome: Chrome,
    pub(crate) fonts: FontState,
    scale: f32,
}

fn empty() -> DecorationBuffer {
    DecorationBuffer {
        width: 0,
        height: 0,
        pixels: Vec::new(),
    }
}
fn buffer(p: Pixmap) -> DecorationBuffer {
    DecorationBuffer {
        width: p.width(),
        height: p.height(),
        pixels: p.take(),
    }
}
fn image(size: Size) -> Option<Pixmap> {
    (size.w <= 8192 && size.h <= 8192 && u64::from(size.w) * u64::from(size.h) <= 16_777_216)
        .then(|| Pixmap::new(size.w.max(1), size.h.max(1)))
        .flatten()
}

impl ModernUi {
    pub fn new(theme: &Theme, fonts: FontState, scale: f32) -> Self {
        Self {
            chrome: Chrome::from_theme_at_scale(theme, scale),
            fonts,
            scale: if scale.is_finite() {
                scale.clamp(0.125, 8.0)
            } else {
                1.0
            },
        }
    }
    fn px(&self, n: u32) -> u32 {
        (n as f32 * self.scale).round().max(1.0) as u32
    }
    pub fn overview_ink(&self) -> ([u8; 3], u32) {
        let c = self.chrome.accent;
        ([c.r, c.g, c.b], self.px(1))
    }
    fn panel(&self, p: &mut Pixmap, r: Rect, selected: bool) {
        modern::surface(
            p,
            r,
            u32::from(self.chrome.frame.radius),
            self.chrome.surface,
            if selected {
                self.chrome.accent
            } else {
                self.chrome.line
            },
            self.px(1),
        );
    }
    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        p: &mut Pixmap,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        text: &str,
        r: Rect,
        selected: bool,
        align: TextAlign,
    ) {
        let text = paint::elide(text, r.size.w, theme.titlebar.font.size);
        paint::draw_text(
            p,
            fonts,
            cache,
            &text,
            &theme.titlebar.font,
            if selected {
                self.chrome.accent
            } else {
                self.chrome.text
            },
            r.pos.x,
            r.pos.y,
            r.size.w,
            r.size.h,
            align,
        );
    }
    #[allow(clippy::too_many_arguments)]
    pub fn menu(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        title: &str,
        items: &[menu::MenuItem],
        highlighted: Option<usize>,
        closable: bool,
    ) -> menu::MenuRender {
        let pad = self.px(12);
        let row = u32::from(theme.menu.item_height).max(1);
        let header = self.px(32);
        if items.len() > ((8192 - header - pad * 2) / row) as usize {
            return menu::MenuRender {
                buffer: empty(),
                item_rects: Vec::new(),
                close_rect: None,
            };
        }
        let title_width = paint::text_width(
            fonts,
            &theme.menu.title_font,
            &paint::elide(title, self.px(480), theme.menu.title_font.size),
        );
        let width = items
            .iter()
            .map(|item| {
                paint::text_width(
                    fonts,
                    &theme.menu.item_font,
                    &paint::elide(item.label(), self.px(480), theme.menu.item_font.size),
                )
            })
            .chain([title_width])
            .max()
            .unwrap_or(0)
            .saturating_add(pad * 3)
            .max(self.px(180))
            .min(4096);
        let height = header + items.len() as u32 * row + pad;
        let Some(mut p) = image(Size::new(width, height)) else {
            return menu::MenuRender {
                buffer: empty(),
                item_rects: Vec::new(),
                close_rect: None,
            };
        };
        self.panel(
            &mut p,
            Rect::new(Point::new(0, 0), Size::new(width, height)),
            false,
        );
        let mut cache = self.fonts.modern_swash();
        let close_rect = closable.then(|| {
            Rect::new(
                Point::new((width - pad * 2) as i32, self.px(5) as i32),
                Size::new(pad, self.px(22)),
            )
        });
        self.text(
            &mut p,
            theme,
            fonts,
            &mut cache,
            title,
            Rect::new(
                Point::new(pad as i32, 0),
                Size::new(width - pad * 3, header),
            ),
            false,
            TextAlign::Left,
        );
        if let Some(r) = close_rect {
            self.text(
                &mut p,
                theme,
                fonts,
                &mut cache,
                "×",
                r,
                false,
                TextAlign::Center,
            );
        }
        let mut item_rects = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let r = Rect::new(
                Point::new(self.px(5) as i32, (header + i as u32 * row) as i32),
                Size::new(width - self.px(10), row),
            );
            let selected = highlighted == Some(i);
            if selected {
                paint::fill_rect(
                    &mut p,
                    r.pos.x,
                    r.pos.y,
                    r.size.w,
                    r.size.h,
                    self.chrome.selection,
                );
            }
            self.text(
                &mut p,
                theme,
                fonts,
                &mut cache,
                item.label(),
                Rect::new(
                    Point::new(pad as i32, r.pos.y),
                    Size::new(width - pad * 3, row),
                ),
                selected,
                TextAlign::Left,
            );
            if item.is_submenu() {
                self.text(
                    &mut p,
                    theme,
                    fonts,
                    &mut cache,
                    "›",
                    Rect::new(
                        Point::new((width - pad * 2) as i32, r.pos.y),
                        Size::new(pad, row),
                    ),
                    selected,
                    TextAlign::Center,
                );
            }
            item_rects.push(r);
        }
        menu::MenuRender {
            buffer: buffer(p),
            item_rects,
            close_rect,
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn card(
        &self,
        p: &mut Pixmap,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        r: Rect,
        title: &str,
        preview: Option<&DecorationBuffer>,
        selected: bool,
    ) {
        self.panel(p, r, selected);
        let pad = self.px(6).min(r.size.w / 3).min(r.size.h / 3);
        let caption = self.px(22).min(r.size.h.saturating_sub(pad * 2));
        if let Some(preview) = preview {
            crate::icon::draw_preview(
                p,
                preview,
                (r.pos.x + pad as i32).max(0) as u32,
                (r.pos.y + pad as i32).max(0) as u32,
                r.size.w.saturating_sub(pad * 2),
                r.size.h.saturating_sub(pad * 2 + caption),
            );
        }
        self.text(
            p,
            theme,
            fonts,
            cache,
            title,
            Rect::new(
                Point::new(
                    r.pos.x + pad as i32,
                    r.pos.y + r.size.h.saturating_sub(pad + caption) as i32,
                ),
                Size::new(r.size.w.saturating_sub(pad * 2), caption),
            ),
            selected,
            TextAlign::Left,
        );
    }
    #[allow(clippy::too_many_arguments)]
    pub fn icon(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        size: u32,
        title: &str,
        preview: Option<&DecorationBuffer>,
    ) -> DecorationBuffer {
        let size = size.clamp(1, 4096);
        let Some(mut p) = image(Size::new(size, size)) else {
            return empty();
        };
        self.card(
            &mut p,
            theme,
            fonts,
            cache,
            Rect::new(Point::new(0, 0), Size::new(size, size)),
            title,
            preview,
            false,
        );
        buffer(p)
    }
    pub fn switcher(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        entries: &[switcher::SwitcherEntry],
        selected: usize,
        tile: u32,
    ) -> DecorationBuffer {
        let tile = tile.clamp(1, 4096);
        let pad = self.px(10);
        let n = entries
            .len()
            .max(1)
            .min(((8192 - pad) / (tile + pad)).max(1) as usize);
        let selected = selected.min(entries.len().saturating_sub(1));
        let first = selected
            .saturating_sub(n / 2)
            .min(entries.len().saturating_sub(n));
        let card_h = (tile * 3 / 4).max(self.px(64));
        let title_h = self.px(26);
        let size = Size::new(n as u32 * (tile + pad) + pad, card_h + pad * 2 + title_h);
        let Some(mut p) = image(size) else {
            return empty();
        };
        self.panel(&mut p, Rect::new(Point::new(0, 0), size), false);
        for (i, entry) in entries.iter().enumerate().skip(first).take(n) {
            self.card(
                &mut p,
                theme,
                fonts,
                cache,
                Rect::new(
                    Point::new((pad + (i - first) as u32 * (tile + pad)) as i32, pad as i32),
                    Size::new(tile, card_h),
                ),
                &entry.title,
                entry.preview.as_ref(),
                i == selected,
            );
        }
        if let Some(entry) = entries.get(selected) {
            self.text(&mut p, theme, fonts, cache, &entry.title,
                Rect::new(Point::new(pad as i32, (card_h + pad * 2) as i32),
                    Size::new(size.w.saturating_sub(pad * 2), title_h)),
                false, TextAlign::Center);
        }
        buffer(p)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn label(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        text: &str,
        width: u32,
        height: u32,
        inverted: bool,
    ) -> DecorationBuffer {
        let width = width.clamp(1, 8192);
        let pad = self.px(8);
        let text = paint::elide(
            text,
            width.saturating_sub(pad * 2),
            theme.titlebar.font.size,
        );
        let width = paint::text_width(fonts, &theme.titlebar.font, &text)
            .saturating_add(pad * 2)
            .min(width)
            .max(1);
        let height = height.clamp(1, 4096);
        let Some(mut p) = image(Size::new(width, height)) else {
            return empty();
        };
        self.panel(
            &mut p,
            Rect::new(Point::new(0, 0), Size::new(width, height)),
            inverted,
        );
        self.text(
            &mut p,
            theme,
            fonts,
            cache,
            &text,
            Rect::new(
                Point::new(pad.min(width / 2) as i32, 0),
                Size::new(width.saturating_sub(pad * 2), height),
            ),
            inverted,
            TextAlign::Center,
        );
        buffer(p)
    }
    pub fn overview(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        entries: &[overview::OverviewEntry<'_>],
        workspace: (usize, usize),
        layout: &overview::OverviewLayout,
    ) -> DecorationBuffer {
        if theme.chrome.is_none() {
            let Some(mut p) = image(layout.panel) else {
                return empty();
            };
            self.panel(&mut p, Rect::new(Point::new(0, 0), layout.panel), false);
            self.text(
                &mut p,
                theme,
                fonts,
                cache,
                "OVERVIEW",
                Rect::new(Point::new(0, 0), Size::new(layout.panel.w, layout.header_h)),
                false,
                TextAlign::Center,
            );
            for (entry, cell) in entries.iter().zip(&layout.cells) {
                self.card(
                    &mut p,
                    theme,
                    fonts,
                    cache,
                    *cell,
                    entry.title,
                    entry.preview,
                    false,
                );
            }
            for (i, r) in layout.strip.iter().enumerate() {
                self.panel(&mut p, *r, i == workspace.0);
                self.text(
                    &mut p,
                    theme,
                    fonts,
                    cache,
                    &(i + 1).to_string(),
                    *r,
                    i == workspace.0,
                    TextAlign::Center,
                );
                if let Some(r) = layout.workspace_close_rect(i) {
                    switcher::blit_buffer(
                        &mut p,
                        &self.workspace_close(r.size.w),
                        r.pos.x,
                        r.pos.y,
                    );
                }
            }
            return buffer(p);
        }
        let Some(mut p) = image(layout.panel) else {
            return empty();
        };
        p.fill(tiny_skia::Color::from_rgba8(
            self.chrome.background.r,
            self.chrome.background.g,
            self.chrome.background.b,
            255,
        ));
        let metrics = self.chrome.overview;
        for (index, rect) in layout.strip.iter().enumerate() {
            let pad = u32::from(metrics.padding)
                .min(rect.size.w / 4)
                .min(rect.size.h / 4);
            let border = u32::from(metrics.border)
                .min(rect.size.w / 2)
                .min(rect.size.h / 2);
            modern::rounded_fill(&mut p, *rect, u32::from(metrics.radius), self.chrome.line);
            modern::rounded_fill(
                &mut p,
                Rect::new(
                    Point::new(rect.pos.x + border as i32, rect.pos.y + border as i32),
                    Size::new(
                        rect.size.w.saturating_sub(border * 2),
                        rect.size.h.saturating_sub(border * 2),
                    ),
                ),
                u32::from(metrics.radius).saturating_sub(border),
                self.chrome.surface,
            );
            let width = rect.size.w.saturating_sub(pad * 2);
            let title = overview::modern::label(
                theme,
                fonts,
                cache,
                &format!("{:02} / Workspace {}", index + 1, index + 1),
                width,
                u32::from(metrics.header),
                true,
            );
            switcher::blit_buffer(
                &mut p,
                &title,
                rect.pos.x + pad as i32,
                rect.pos.y + pad as i32,
            );
            let status = if index >= workspace.1 {
                "Create workspace".into()
            } else if index == workspace.0 {
                format!(
                    "{} window{}",
                    entries.len(),
                    if entries.len() == 1 { "" } else { "s" }
                )
            } else {
                "Select workspace".into()
            };
            let status = overview::modern::label(
                theme,
                fonts,
                cache,
                &status,
                width,
                u32::from(metrics.footer),
                false,
            );
            switcher::blit_buffer(
                &mut p,
                &status,
                rect.pos.x + pad as i32,
                rect.pos.y + rect.size.h.saturating_sub(pad + status.height) as i32,
            );
        }
        for (entry, cell) in entries.iter().zip(&layout.cells) {
            self.card(
                &mut p,
                theme,
                fonts,
                cache,
                *cell,
                entry.title,
                entry.preview,
                false,
            );
        }
        buffer(p)
    }
    pub fn workspace_close(&self, edge: u32) -> DecorationBuffer {
        let edge = edge.clamp(1, 4096);
        let Some(mut p) = image(Size::new(edge, edge)) else {
            return empty();
        };
        let pad = (edge / 3).max(1);
        let t = self.px(1);
        for i in pad..edge.saturating_sub(pad) {
            paint::fill_rect(&mut p, i as i32, i as i32, t, t, self.chrome.muted);
            paint::fill_rect(
                &mut p,
                i as i32,
                (edge - 1 - i) as i32,
                t,
                t,
                self.chrome.muted,
            );
        }
        buffer(p)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn selection(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        entry: &overview::OverviewEntry<'_>,
        cell: Size,
        pad: u32,
    ) -> DecorationBuffer {
        let ring = overview::plate_ring(pad);
        let size = Size::new(
            cell.w.saturating_add(ring * 2),
            cell.h.saturating_add(ring * 2),
        );
        let Some(mut p) = image(size) else {
            return empty();
        };
        self.card(
            &mut p,
            theme,
            fonts,
            cache,
            Rect::new(Point::new(0, 0), size),
            entry.title,
            entry.preview,
            true,
        );
        buffer(p)
    }
}
