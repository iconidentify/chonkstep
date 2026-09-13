//! Theme-owned workspace-card geometry. Layout is resolved on semantic changes;
//! frame presentation consumes these rectangles without allocating or packing.
use crate::{Point, Rect, Size};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverviewMetrics {
    pub left: u16,
    pub right: u16,
    pub top: u16,
    pub bottom: u16,
    pub gap: u16,
    pub padding: u16,
    pub header: u16,
    pub footer: u16,
    pub radius: u16,
    pub border: u16,
    pub minimum_slots: u16,
}

impl Default for OverviewMetrics {
    fn default() -> Self {
        Self {
            left: 24,
            right: 82,
            top: 66,
            bottom: 75,
            gap: 14,
            padding: 12,
            header: 26,
            footer: 26,
            radius: 3,
            border: 1,
            minimum_slots: 4,
        }
    }
}

impl OverviewMetrics {
    pub fn normalized(mut self) -> Self {
        self.left = self.left.min(1024);
        self.right = self.right.min(1024);
        self.top = self.top.min(1024);
        self.bottom = self.bottom.min(1024);
        self.gap = self.gap.min(256);
        self.padding = self.padding.min(128);
        self.header = self.header.clamp(1, 256);
        self.footer = self.footer.clamp(1, 256);
        self.radius = self.radius.min(256);
        self.border = self.border.min(16);
        self.minimum_slots = self.minimum_slots.clamp(1, 16);
        self
    }

    pub fn scaled(self, scale: f32) -> Self {
        let s = |value: u16| crate::chrome::scaled(value, scale);
        Self {
            left: s(self.left),
            right: s(self.right),
            top: s(self.top),
            bottom: s(self.bottom),
            gap: s(self.gap),
            padding: s(self.padding),
            header: s(self.header),
            footer: s(self.footer),
            radius: s(self.radius),
            border: s(self.border),
            minimum_slots: self.minimum_slots,
        }
        .normalized()
    }

    pub fn layout(self, panel: Size, count: usize) -> OverviewGrid {
        let m = self.normalized();
        let panel = Size::new(panel.w.min(8192), panel.h.min(8192));
        let left = u32::from(m.left).min(panel.w / 4);
        let right = u32::from(m.right).min(panel.w / 4);
        let top = u32::from(m.top).min(panel.h / 4);
        let bottom = u32::from(m.bottom).min(panel.h / 4);
        let bounds = Rect::new(
            Point::new(left as i32, top as i32),
            Size::new(
                panel.w.saturating_sub(left + right),
                panel.h.saturating_sub(top + bottom),
            ),
        );
        let count = count.max(usize::from(m.minimum_slots)).min(128);
        let cols = count.isqrt().max(2).min(count);
        let rows = count.div_ceil(cols);
        let gap = u32::from(m.gap)
            .min(bounds.size.w / (cols as u32 * 2))
            .min(bounds.size.h / (rows as u32 * 2));
        let width = bounds.size.w.saturating_sub(gap * (cols as u32 - 1)) / cols as u32;
        let height = bounds.size.h.saturating_sub(gap * (rows as u32 - 1)) / rows as u32;
        let cards = (0..count)
            .map(|index| {
                Rect::new(
                    Point::new(
                        bounds.pos.x + (index % cols) as i32 * (width + gap) as i32,
                        bounds.pos.y + (index / cols) as i32 * (height + gap) as i32,
                    ),
                    Size::new(width, height),
                )
            })
            .collect();
        OverviewGrid {
            bounds,
            cards,
            cols,
        }
    }

    pub fn preview(self, card: Rect) -> Rect {
        let pad = u32::from(self.padding)
            .min(card.size.w / 4)
            .min(card.size.h / 4);
        let header = u32::from(self.header).min(card.size.h.saturating_sub(pad * 2) / 3);
        let footer = u32::from(self.footer).min(card.size.h.saturating_sub(pad * 2) / 3);
        Rect::new(
            Point::new(
                card.pos.x.saturating_add(pad as i32),
                card.pos.y.saturating_add((pad + header) as i32),
            ),
            Size::new(
                card.size.w.saturating_sub(pad * 2),
                card.size.h.saturating_sub(pad * 2 + header + footer),
            ),
        )
    }
}

pub struct OverviewGrid {
    pub bounds: Rect,
    pub cards: Vec<Rect>,
    pub cols: usize,
}

/// Include offscreen managed windows so Flow's virtual arrangement remains
/// reachable in the same preview that depicts ordinary desktop windows.
pub fn overview_source_bounds(desktop: Rect, sources: impl IntoIterator<Item = Rect>) -> Rect {
    let (mut left, mut top) = (i64::from(desktop.pos.x), i64::from(desktop.pos.y));
    let (mut right, mut bottom) = (
        left + i64::from(desktop.size.w),
        top + i64::from(desktop.size.h),
    );
    for source in sources {
        left = left.min(i64::from(source.pos.x));
        top = top.min(i64::from(source.pos.y));
        right = right.max(i64::from(source.pos.x) + i64::from(source.size.w));
        bottom = bottom.max(i64::from(source.pos.y) + i64::from(source.size.h));
    }
    Rect::new(
        Point::new(left as i32, top as i32),
        Size::new(
            (right - left).min(i64::from(u32::MAX)) as u32,
            (bottom - top).min(i64::from(u32::MAX)) as u32,
        ),
    )
}

/// Preserve a desktop's window arrangement inside a card without reading pixels.
pub fn overview_thumbnail(source: Rect, desktop: Rect, preview: Rect) -> Rect {
    let scale = (preview.size.w as f64 / desktop.size.w.max(1) as f64)
        .min(preview.size.h as f64 / desktop.size.h.max(1) as f64);
    Rect::new(
        Point::new(
            preview.pos.x.saturating_add(
                ((i64::from(source.pos.x) - i64::from(desktop.pos.x)) as f64 * scale).round()
                    as i32,
            ),
            preview.pos.y.saturating_add(
                ((i64::from(source.pos.y) - i64::from(desktop.pos.y)) as f64 * scale).round()
                    as i32,
            ),
        ),
        Size::new(
            (source.size.w as f64 * scale).round().max(1.0) as u32,
            (source.size.h as f64 * scale).round().max(1.0) as u32,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_grid_and_preview_hit_bounds_scale_together() {
        for scale in [1.0, 1.5, 2.0] {
            let metrics = OverviewMetrics::default().scaled(scale);
            let panel = Size::new((1280.0 * scale) as u32, (720.0 * scale) as u32);
            let grid = metrics.layout(panel, 1);
            assert_eq!((grid.cards.len(), grid.cols), (4, 2));
            assert_eq!(grid.bounds.pos.x, i32::from(metrics.left));
            assert_eq!(grid.bounds.pos.y, i32::from(metrics.top));
            for card in grid.cards {
                let preview = metrics.preview(card);
                assert!(card.contains(preview.pos));
                assert!(preview.pos.x + preview.size.w as i32 <= card.pos.x + card.size.w as i32);
                assert!(preview.pos.y + preview.size.h as i32 <= card.pos.y + card.size.h as i32);
            }
        }
    }
    #[test]
    fn degenerate_or_large_requests_keep_grid_work_and_storage_bounded() {
        for panel in [
            Size::new(0, 0),
            Size::new(1, 1),
            Size::new(320, 200),
            Size::new(u32::MAX, u32::MAX),
        ] {
            let grid = OverviewMetrics::default()
                .scaled(f32::MAX)
                .layout(panel, usize::MAX);
            assert_eq!(grid.cards.len(), 128);
            for card in grid.cards {
                assert!(card.pos.x >= 0 && card.pos.y >= 0);
                assert!(card.pos.x as u32 + card.size.w <= panel.w.min(8192));
                assert!(card.pos.y as u32 + card.size.h <= panel.h.min(8192));
            }
        }
    }
}
