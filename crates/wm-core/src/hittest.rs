use wm_theme_api::{ButtonKind, DecorationLayout, Point, ResizeEdge};

/// What a frame-local point landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTarget {
    Button(ButtonKind),
    TitlebarDrag,
    ResizeEdge(ResizeEdge),
    ClientArea,
}

/// Pure arithmetic against a cached `DecorationLayout` — no I/O,
/// trivially unit-tested, and the reason decoration hit-testing doesn't
/// need to know anything about pixels or an X server. Buttons and resize
/// regions are checked before the general titlebar-height band, since a
/// resize corner can sit within that band near the frame's top edge.
pub fn hit_test(layout: &DecorationLayout, point: Point) -> HitTarget {
    if let Some((kind, _)) = layout.button_hitboxes.iter().find(|(_, rect)| rect.contains(point)) {
        return HitTarget::Button(*kind);
    }
    if let Some((edge, _)) = layout.resize_hitboxes.iter().find(|(_, rect)| rect.contains(point)) {
        return HitTarget::ResizeEdge(*edge);
    }
    if point.y < layout.titlebar_height as i32 {
        return HitTarget::TitlebarDrag;
    }
    HitTarget::ClientArea
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme_api::{Rect, Size};

    fn sample_layout() -> DecorationLayout {
        DecorationLayout {
            frame_size: Size::new(200, 220),
            client_offset: Point::new(0, 20),
            titlebar_height: 20,
            button_hitboxes: vec![
                (ButtonKind::Close, Rect::new(Point::new(2, 2), Size::new(14, 14))),
                (ButtonKind::Miniaturize, Rect::new(Point::new(184, 2), Size::new(14, 14))),
            ],
            resize_hitboxes: vec![(
                ResizeEdge::SouthEast,
                Rect::new(Point::new(192, 212), Size::new(8, 8)),
            )],
            shaded_frame_height: 22,
        }
    }

    #[test]
    fn hits_close_button() {
        let layout = sample_layout();
        assert_eq!(hit_test(&layout, Point::new(5, 5)), HitTarget::Button(ButtonKind::Close));
    }

    #[test]
    fn hits_miniaturize_button() {
        let layout = sample_layout();
        assert_eq!(
            hit_test(&layout, Point::new(190, 5)),
            HitTarget::Button(ButtonKind::Miniaturize)
        );
    }

    #[test]
    fn hits_titlebar_drag_region() {
        let layout = sample_layout();
        assert_eq!(hit_test(&layout, Point::new(100, 10)), HitTarget::TitlebarDrag);
    }

    #[test]
    fn hits_resize_corner() {
        let layout = sample_layout();
        assert_eq!(
            hit_test(&layout, Point::new(195, 215)),
            HitTarget::ResizeEdge(ResizeEdge::SouthEast)
        );
    }

    #[test]
    fn hits_client_area() {
        let layout = sample_layout();
        assert_eq!(hit_test(&layout, Point::new(100, 100)), HitTarget::ClientArea);
    }
}
