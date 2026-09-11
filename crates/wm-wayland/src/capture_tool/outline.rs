//! Recording boundaries use retained, non-overlapping strips. No textures,
//! animation deadlines, or per-frame allocations are needed for this chrome.
use smithay::backend::renderer::element::{solid::SolidColorRenderElement, Id, Kind};
use smithay::backend::renderer::{utils::CommitCounter, Color32F};
use smithay::utils::{Physical, Rectangle};
use wm_theme_api::{Point, Rect, Size};

struct Band {
    rect: Option<Rect>,
    color: Color32F,
    id: Id,
}

pub(super) struct Outline {
    bands: [Band; 16],
}

impl Outline {
    pub fn new(region: Rect, monitor: Rect, scale: f64) -> Self {
        let bright = (1.5 * scale.clamp(1.0, 3.0)).round() as i64;
        let shadow = scale.clamp(1.0, 3.0).ceil() as i64;
        let x = i64::from(region.pos.x);
        let y = i64::from(region.pos.y);
        let w = i64::from(region.size.w);
        let h = i64::from(region.size.h);
        let bands = std::array::from_fn(|index| {
            let ring = index / 4;
            let (outset, thickness, color) = if ring == 0 {
                (0, bright, Color32F::new(0.95, 0.95, 0.95, 0.95))
            } else {
                (
                    ring as i64 * shadow,
                    shadow,
                    Color32F::new(0.0, 0.0, 0.0, [0.0, 0.45, 0.25, 0.10][ring]),
                )
            };
            let (x, y, w, h) = (x - outset, y - outset, w + outset * 2, h + outset * 2);
            let tx = thickness.min(w);
            let ty = thickness.min(h);
            let bottom_h = ty.min(h - ty);
            let right_w = tx.min(w - tx);
            // Sides exclude the corners, so translucent shadow bands do not
            // darken twice. Bright pixels remain inside the captured rectangle.
            let (x, y, w, h) = match index % 4 {
                0 => (x, y, w, ty),
                1 => (x, y + h - bottom_h, w, bottom_h),
                2 => (x, y + ty, tx, (h - ty * 2).max(0)),
                _ => (x + w - right_w, y + ty, right_w, (h - ty * 2).max(0)),
            };
            let left = x.max(i64::from(monitor.pos.x));
            let top = y.max(i64::from(monitor.pos.y));
            let right = (x + w).min(i64::from(monitor.pos.x) + i64::from(monitor.size.w));
            let bottom = (y + h).min(i64::from(monitor.pos.y) + i64::from(monitor.size.h));
            let rect = (right > left && bottom > top)
                .then(|| {
                    Some(Rect::new(
                        Point::new(i32::try_from(left).ok()?, i32::try_from(top).ok()?),
                        Size::new(
                            u32::try_from(right - left).ok()?,
                            u32::try_from(bottom - top).ok()?,
                        ),
                    ))
                })
                .flatten();
            Band {
                rect,
                color,
                id: Id::new(),
            }
        });
        Self { bands }
    }

    pub fn elements(&self, viewport: Rect) -> impl Iterator<Item = SolidColorRenderElement> + '_ {
        self.bands.iter().filter_map(move |band| {
            let rect = band.rect?.intersection(viewport)?;
            let geometry = Rectangle::<i32, Physical>::new(
                (
                    i32::try_from(i64::from(rect.pos.x) - i64::from(viewport.pos.x)).ok()?,
                    i32::try_from(i64::from(rect.pos.y) - i64::from(viewport.pos.y)).ok()?,
                )
                    .into(),
                (
                    i32::try_from(rect.size.w).ok()?,
                    i32::try_from(rect.size.h).ok()?,
                )
                    .into(),
            );
            Some(SolidColorRenderElement::new(
                band.id.clone(),
                geometry,
                CommitCounter::default(),
                band.color,
                Kind::Unspecified,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::element::Element;

    #[test]
    fn recording_boundary_is_retained_allocation_free_and_clipped_to_its_output() {
        let monitor = Rect::new(Point::new(800, 0), Size::new(800, 600));
        for scale in [1.0, 1.5, 2.0] {
            for region in [
                monitor,
                Rect::new(Point::new(800, 100), Size::new(320, 200)),
                Rect::new(Point::new(1000, 100), Size::new(1, 1)),
            ] {
                let outline = Outline::new(region, monitor, scale);
                assert_eq!(
                    outline
                        .elements(Rect::new(Point::new(0, 0), Size::new(800, 600)))
                        .count(),
                    0
                );
                let original: Vec<_> = outline
                    .elements(monitor)
                    .map(|e| (e.id().clone(), e.geometry(1.0.into())))
                    .collect();
                let (_, allocations) = chonk_test_support::measure(|| {
                    for _ in 0..10_000 {
                        for (element, (id, geometry)) in outline.elements(monitor).zip(&original) {
                            assert_eq!(element.id(), id);
                            assert_eq!(element.geometry(1.0.into()), *geometry);
                        }
                    }
                });
                assert_eq!((allocations.calls, allocations.requested_bytes), (0, 0));
                for band in &outline.bands[..4] {
                    if let Some(rect) = band.rect {
                        assert_eq!(rect.intersection(region), Some(rect));
                    }
                }
            }
        }
    }
}
