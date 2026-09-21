//! Workplace Shell vocabulary applied to ChonkStep's live shell surfaces.
use super::{caption, control, document, empty, fill, sprite, text, Metrics};
use crate::{
    icon, menu,
    model::Color,
    os2warp::{BLUE, INK, PANEL, SHADE, WHITE},
    overview, switcher, FontState,
};
use wm_theme_api::{ButtonKind, DecorationBuffer, Point, Rect, Size};

#[derive(Clone)]
pub(crate) struct Os2Ui {
    fonts: FontState,
    m: Metrics,
}

impl Os2Ui {
    pub fn new(fonts: FontState, scale: f32) -> Self {
        Self {
            fonts,
            m: Metrics::new(scale),
        }
    }
    fn px(&self, n: u32) -> u32 {
        self.m.px(n)
    }
    pub fn overview_ink(&self) -> ([u8; 3], u32) {
        ([0, 0, 170], self.px(2).max(1))
    }
    fn paint(&self, image: &mut DecorationBuffer, rect: Rect, color: Color) {
        fill(
            image,
            rect.pos.x.max(0) as u32,
            rect.pos.y.max(0) as u32,
            rect.size.w,
            rect.size.h,
            color,
        );
    }
    fn panel(&self, image: &mut DecorationBuffer, rect: Rect, sunken: bool) {
        self.paint(image, rect, PANEL);
        let line = self.px(1).max(1).min(rect.size.w / 2).min(rect.size.h / 2);
        if line == 0 {
            return;
        }
        let (x, y, w, h) = (
            rect.pos.x.max(0) as u32,
            rect.pos.y.max(0) as u32,
            rect.size.w,
            rect.size.h,
        );
        let (light, dark) = if sunken {
            (SHADE, WHITE)
        } else {
            (WHITE, SHADE)
        };
        fill(image, x, y, w, line, light);
        fill(image, x, y, line, h, light);
        fill(image, x, y + h - line, w, line, dark);
        fill(image, x + w - line, y, line, h, dark);
    }
    fn label_at(
        &self,
        image: &mut DecorationBuffer,
        label: &str,
        rect: Rect,
        color: Color,
        bold: bool,
    ) {
        text::draw(image, &self.fonts, label, rect, color, bold, self.m);
    }
    fn centered(
        &self,
        image: &mut DecorationBuffer,
        label: &str,
        rect: Rect,
        color: Color,
        bold: bool,
    ) {
        let width = self.px(text::width(label, bold)).min(rect.size.w);
        // Center on the source grid so integer scaling reproduces every pixel.
        let unit = if self.m.scale.fract() == 0.0 {
            self.m.scale as u32
        } else {
            1
        };
        let inset = (rect.size.w - width) / (2 * unit) * unit;
        self.label_at(
            image,
            label,
            Rect::new(
                Point::new(rect.pos.x + inset as i32, rect.pos.y),
                Size::new(width, rect.size.h),
            ),
            color,
            bold,
        );
    }
    pub fn menu(
        &self,
        title: &str,
        items: &[menu::MenuItem],
        highlighted: Option<usize>,
        closable: bool,
    ) -> menu::MenuRender {
        let (line, row, header) = (
            self.px(1).max(1),
            self.px(20).max(1),
            if closable { self.px(26) } else { self.px(2) },
        );
        if items.len() > ((8192 - header - line * 2) / row) as usize {
            return menu::MenuRender {
                buffer: empty(0, 0),
                item_rects: Vec::new(),
                close_rect: None,
            };
        }
        let text_width = items
            .iter()
            .map(|item| text::width(item.label(), false))
            .max()
            .unwrap_or(0)
            .max(if closable {
                text::width(title, true)
            } else {
                0
            });
        let w = self
            .px(text_width.min(2048) + 54)
            .max(self.px(150))
            .min(4096);
        let h = header + row * items.len() as u32 + line * 2;
        if u64::from(w) * u64::from(h) > 16_777_216 {
            return menu::MenuRender {
                buffer: empty(0, 0),
                item_rects: Vec::new(),
                close_rect: None,
            };
        }
        let mut image = empty(w, h);
        self.panel(
            &mut image,
            Rect::new(Point::new(0, 0), Size::new(w, h)),
            false,
        );
        if closable {
            caption(
                &mut image,
                Rect::new(
                    Point::new(self.px(4) as i32, self.px(4) as i32),
                    Size::new(
                        w.saturating_sub(self.px(if closable { 28 } else { 8 })),
                        self.px(18),
                    ),
                ),
                true,
                self.m,
            );
            self.label_at(
                &mut image,
                title,
                Rect::new(
                    Point::new(self.px(12) as i32, self.px(4) as i32),
                    Size::new(w.saturating_sub(self.px(42)), self.px(16)),
                ),
                WHITE,
                true,
            );
        }
        let close_rect = closable.then(|| {
            Rect::new(
                Point::new(w.saturating_sub(self.px(20)) as i32, self.px(6) as i32),
                Size::new(self.px(14), self.px(14)),
            )
        });
        if let Some(rect) = close_rect {
            control(&mut image, rect, ButtonKind::Close, false, self.m);
        }
        let mut item_rects = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let y = header + index as u32 * row;
            let rect = Rect::new(
                Point::new(line as i32, y as i32),
                Size::new(w - line * 2, row),
            );
            let selected = highlighted == Some(index);
            if selected {
                self.paint(&mut image, rect, BLUE);
            }
            let color = if selected { WHITE } else { INK };
            if item.is_submenu() {
                self.folder(&mut image, self.px(5), y + self.px(2), selected);
            } else {
                document(&mut image, self.px(3), y + self.px(2), self.m);
            }
            let inset = self.px(26);
            self.label_at(
                &mut image,
                item.label(),
                Rect::new(
                    Point::new(inset as i32, y as i32),
                    Size::new(w.saturating_sub(inset + self.px(24)), row),
                ),
                color,
                false,
            );
            if item.is_submenu() {
                // Solid, seven-row Workplace Shell cascade pointer.
                for n in 0..4 {
                    fill(
                        &mut image,
                        w - self.px(10) + self.px(n),
                        y + self.px(6 + n),
                        self.px(n + 1) - self.px(n),
                        self.px(13 - n) - self.px(6 + n),
                        color,
                    );
                }
            }
            item_rects.push(rect);
        }
        menu::MenuRender {
            buffer: image,
            item_rects,
            close_rect,
        }
    }
    fn folder(&self, image: &mut DecorationBuffer, x: u32, y: u32, _selected: bool) {
        // Original 16px Workplace Shell folder; reference menus.png (276,3).
        let rows = [
            "................",
            "................",
            "...gk...........",
            "...kykk.........",
            "..kykgykk..kk...",
            "..kwywkgykkpykg.",
            "..kywywykgpypykg",
            "..kwywywykgpypyk",
            "..kywywywypgkgpk",
            "..kwywywywywykyk",
            "..kywywywywywkpk",
            "..kppwywywywykyk",
            "...kkppywywywkpk",
            ".....kkppwywykyk",
            ".......kkppywkpk",
            ".........kkppkyk",
        ];
        sprite(image, x, y, &rows, self.m);
    }
    fn card(
        &self,
        image: &mut DecorationBuffer,
        rect: Rect,
        title: &str,
        preview: Option<&DecorationBuffer>,
        selected: bool,
    ) {
        if rect.size.w == 0 || rect.size.h == 0 {
            return;
        }
        self.panel(image, rect, false);
        let line = self.px(2).min(rect.size.w / 2).min(rect.size.h / 2);
        let header = self.px(19).min(rect.size.h.saturating_sub(line * 2));
        let inside = rect.size.w.saturating_sub(line * 2);
        let tab = Rect::new(
            Point::new(rect.pos.x + line as i32, rect.pos.y + line as i32),
            Size::new(inside, header),
        );
        caption(image, tab, selected, self.m);
        self.centered(
            image,
            title,
            tab,
            if selected { WHITE } else { PANEL },
            true,
        );
        let content = Rect::new(
            Point::new(
                rect.pos.x + line as i32,
                rect.pos.y + (line + header) as i32,
            ),
            Size::new(inside, rect.size.h.saturating_sub(header + line * 2)),
        );
        self.paint(image, content, WHITE);
        if content.size.w > 0 && content.size.h > 0 {
            if let Some(preview) = preview {
                // Reuse the bounded, aspect-preserving live-preview primitive.
                if let Some(mut p) = tiny_skia::Pixmap::from_vec(
                    std::mem::take(&mut image.pixels),
                    tiny_skia::IntSize::from_wh(image.width, image.height).unwrap(),
                ) {
                    icon::draw_preview(
                        &mut p,
                        preview,
                        content.pos.x.max(0) as u32,
                        content.pos.y.max(0) as u32,
                        content.size.w,
                        content.size.h,
                    );
                    image.pixels = p.take();
                }
            } else if content.size.w >= self.px(24) && content.size.h >= self.px(18) {
                self.folder(
                    image,
                    content.pos.x.max(0) as u32 + self.px(4),
                    content.pos.y.max(0) as u32 + self.px(3),
                    false,
                );
            }
        }
    }
    pub fn icon(
        &self,
        size: u32,
        title: &str,
        preview: Option<&DecorationBuffer>,
    ) -> DecorationBuffer {
        let size = size.clamp(1, 4096);
        let mut image = empty(size, size);
        self.card(
            &mut image,
            Rect::new(Point::new(0, 0), Size::new(size, size)),
            title,
            preview,
            false,
        );
        image
    }
    pub fn switcher(
        &self,
        entries: &[switcher::SwitcherEntry],
        selected: usize,
        tile: u32,
    ) -> DecorationBuffer {
        let tile = tile.clamp(1, 2048);
        let pad = self.px(8).max(2);
        let height = tile + pad * 2 + self.px(22);
        let max_width = (16_777_216 / height).min(8192);
        let count = entries
            .len()
            .max(1)
            .min(((max_width - pad) / (tile + pad)).max(1) as usize);
        let selected = selected.min(entries.len().saturating_sub(1));
        let first = selected
            .saturating_sub(count / 2)
            .min(entries.len().saturating_sub(count));
        let size = Size::new(
            count as u32 * (tile + pad) + pad,
            tile + pad * 2 + self.px(22),
        );
        let mut image = empty(size.w, size.h);
        self.panel(&mut image, Rect::new(Point::new(0, 0), size), false);
        for (i, entry) in entries.iter().enumerate().skip(first).take(count) {
            let rect = Rect::new(
                Point::new((pad + (i - first) as u32 * (tile + pad)) as i32, pad as i32),
                Size::new(tile, tile),
            );
            if i == selected {
                self.paint(
                    &mut image,
                    Rect::new(
                        Point::new(
                            rect.pos.x - self.px(2) as i32,
                            rect.pos.y - self.px(2) as i32,
                        ),
                        Size::new(tile + self.px(4), tile + self.px(4)),
                    ),
                    BLUE,
                );
            }
            self.card(
                &mut image,
                rect,
                &entry.title,
                entry.preview.as_ref(),
                i == selected,
            );
        }
        if let Some(entry) = entries.get(selected) {
            self.centered(
                &mut image,
                &entry.title,
                Rect::new(
                    Point::new(pad as i32, (pad + tile) as i32),
                    Size::new(size.w - pad * 2, self.px(22)),
                ),
                INK,
                false,
            );
        }
        image
    }
    pub fn label(&self, label: &str, width: u32, height: u32, inverted: bool) -> DecorationBuffer {
        let width = (self.px(text::width(label, false)) + self.px(12))
            .min(width.clamp(1, 8192))
            .max(1);
        let height = height.clamp(1, 4096).min(16_777_216 / width);
        let mut image = empty(width, height);
        fill(
            &mut image,
            0,
            0,
            width,
            height,
            if inverted { BLUE } else { PANEL },
        );
        self.centered(
            &mut image,
            label,
            Rect::new(Point::new(0, 0), Size::new(width, height)),
            if inverted { WHITE } else { INK },
            false,
        );
        image
    }
    pub fn overview(
        &self,
        entries: &[overview::OverviewEntry<'_>],
        workspace: (usize, usize),
        layout: &overview::OverviewLayout,
    ) -> DecorationBuffer {
        if layout.panel.w > 8192
            || layout.panel.h > 8192
            || u64::from(layout.panel.w) * u64::from(layout.panel.h) > 16_777_216
        {
            return empty(0, 0);
        }
        let mut image = empty(layout.panel.w, layout.panel.h);
        self.panel(&mut image, Rect::new(Point::new(0, 0), layout.panel), false);
        self.centered(
            &mut image,
            "Workspaces",
            Rect::new(Point::new(0, 0), Size::new(layout.panel.w, layout.header_h)),
            INK,
            true,
        );
        for (entry, cell) in entries.iter().zip(&layout.cells) {
            self.card(&mut image, *cell, entry.title, entry.preview, false);
        }
        if entries.is_empty() {
            self.centered(
                &mut image,
                "No windows on this workspace",
                layout.grid,
                INK,
                false,
            );
        }
        for (i, rect) in layout.strip.iter().enumerate() {
            self.panel(&mut image, *rect, i == workspace.0);
            if i == workspace.0 {
                let mut inner = *rect;
                inner.pos.x += self.px(2) as i32;
                inner.pos.y += self.px(2) as i32;
                inner.size.w = inner.size.w.saturating_sub(self.px(4));
                inner.size.h = inner.size.h.saturating_sub(self.px(4));
                self.paint(&mut image, inner, BLUE);
            }
            self.centered(
                &mut image,
                &(i + 1).to_string(),
                *rect,
                if i == workspace.0 { WHITE } else { INK },
                false,
            );
            if let Some(close) = layout.workspace_close_rect(i) {
                control(
                    &mut image,
                    close,
                    ButtonKind::Close,
                    false,
                    Metrics::new(close.size.w as f32 / 14.0),
                );
            }
        }
        image
    }
    pub fn workspace_close(&self, edge: u32) -> DecorationBuffer {
        let edge = edge.clamp(1, 4096);
        let mut image = empty(edge, edge);
        control(
            &mut image,
            Rect::new(Point::new(0, 0), Size::new(edge, edge)),
            ButtonKind::Close,
            false,
            Metrics {
                scale: edge as f32 / 14.0,
            },
        );
        image
    }
    pub fn selection(
        &self,
        entry: &overview::OverviewEntry<'_>,
        cell: Size,
        pad: u32,
    ) -> DecorationBuffer {
        let ring = overview::plate_ring(pad);
        let size = Size::new(
            cell.w.saturating_add(ring * 2),
            cell.h.saturating_add(ring * 2),
        );
        if size.w > 8192 || size.h > 8192 || u64::from(size.w) * u64::from(size.h) > 16_777_216 {
            return empty(0, 0);
        }
        let mut image = empty(size.w, size.h);
        fill(&mut image, 0, 0, size.w, size.h, BLUE);
        self.card(
            &mut image,
            Rect::new(Point::new(ring as i32, ring as i32), cell),
            entry.title,
            entry.preview,
            true,
        );
        image
    }
}
