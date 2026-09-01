//! Pure geometry: clamps a candidate content size to a client's
//! `SizeHints` (`WM_NORMAL_HINTS`'s min/max size and resize increment).
//! No I/O, no backend dependency, trivially unit-tested — matches this
//! crate's `hittest`/`snap` modules in spirit.

use wm_theme_api::{Point, ResizeEdge, Size};

use crate::types::SizeHints;

/// Clamps `candidate` to `hints`: never smaller than `min_size`
/// (defaulting to 1x1 if unset), never larger than `max_size` (if
/// set), and snapped to `resize_increment` steps counted from
/// `min_size` — matches a terminal's "always a whole number of rows/
/// columns" resize increment hint exactly.
pub fn constrain_size(candidate: Size, hints: SizeHints) -> Size {
    let min = hints.min_size.unwrap_or(Size::new(1, 1));
    let mut w = candidate.w.max(min.w.max(1));
    let mut h = candidate.h.max(min.h.max(1));

    if let Some(inc) = hints.resize_increment {
        if inc.w > 1 {
            w = min.w + ((w - min.w) / inc.w) * inc.w;
        }
        if inc.h > 1 {
            h = min.h + ((h - min.h) / inc.h) * inc.h;
        }
    }

    if let Some(max) = hints.max_size {
        if max.w > 0 {
            w = w.min(max.w.max(min.w));
        }
        if max.h > 0 {
            h = h.min(max.h.max(min.h));
        }
    }

    Size::new(w.max(1), h.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_shrinks_below_the_minimum_size() {
        let hints = SizeHints { min_size: Some(Size::new(100, 50)), max_size: None, resize_increment: None };
        let result = constrain_size(Size::new(20, 10), hints);
        assert_eq!(result, Size::new(100, 50));
    }

    #[test]
    fn never_grows_past_the_maximum_size() {
        let hints = SizeHints { min_size: None, max_size: Some(Size::new(400, 300)), resize_increment: None };
        let result = constrain_size(Size::new(900, 900), hints);
        assert_eq!(result, Size::new(400, 300));
    }

    #[test]
    fn snaps_to_resize_increment_steps_from_the_minimum() {
        // A terminal-style hint: 80x24 minimum, 8x16 cell increments.
        let hints = SizeHints { min_size: Some(Size::new(80, 24)), max_size: None, resize_increment: Some(Size::new(8, 16)) };

        let result = constrain_size(Size::new(103, 55), hints);

        // (103-80)/8 = 2 (floored) -> 80+16=96; (55-24)/16 = 1 (floored) -> 24+16=40
        assert_eq!(result, Size::new(96, 40));
    }

    #[test]
    fn with_no_hints_at_all_just_floors_at_one_pixel() {
        let result = constrain_size(Size::new(0, 0), SizeHints::default());
        assert_eq!(result, Size::new(1, 1));
    }

    #[test]
    fn max_smaller_than_min_never_shrinks_below_min() {
        // A pathological/misconfigured hint set (max < min) must not
        // produce a size smaller than min — min always wins.
        let hints = SizeHints { min_size: Some(Size::new(200, 200)), max_size: Some(Size::new(100, 100)), resize_increment: None };
        let result = constrain_size(Size::new(500, 500), hints);
        assert_eq!(result, Size::new(200, 200));
    }
}

/// Which edge or corner a modifier-drag resize should pull, given where
/// in a frame of `size` the press landed.
///
/// Thirds on each axis, so the frame is a noughts-and-crosses board:
/// the four corner cells pull their corner, the four edge cells pull
/// their edge, and the middle cell — the one place with no nearest edge
/// worth guessing at — pulls the bottom-right corner, where this
/// theme's resize bar lives and where a user reaching for "just resize
/// it" is already aiming.
///
/// Window Maker and KWin both quantise this way rather than by true
/// nearest edge, and the reason is that it makes the gesture aimable
/// without being precise: the target for "the right edge" is a third of
/// the window, not a few pixels of border.
pub fn resize_edge_for_point(size: Size, at: Point) -> ResizeEdge {
    // A degenerate frame has no thirds to speak of; the bottom-right
    // fallback is as good an answer as any and cannot divide by zero.
    let (w, h) = (size.w as i32, size.h as i32);
    if w <= 0 || h <= 0 {
        return ResizeEdge::SouthEast;
    }
    // -1 west/north, 0 middle, 1 east/south.
    let third = |v: i32, extent: i32| -> i32 {
        if v * 3 < extent {
            -1
        } else if v * 3 >= extent * 2 {
            1
        } else {
            0
        }
    };
    match (third(at.x, w), third(at.y, h)) {
        (-1, -1) => ResizeEdge::NorthWest,
        (0, -1) => ResizeEdge::North,
        (1, -1) => ResizeEdge::NorthEast,
        (-1, 0) => ResizeEdge::West,
        (1, 0) => ResizeEdge::East,
        (-1, 1) => ResizeEdge::SouthWest,
        (0, 1) => ResizeEdge::South,
        (1, 1) => ResizeEdge::SouthEast,
        // The middle cell, and the only arm the matrix above does not
        // name: no edge is nearest, so take the classic one.
        _ => ResizeEdge::SouthEast,
    }
}

#[cfg(test)]
mod resize_edge_tests {
    use super::*;

    #[test]
    fn each_ninth_pulls_its_own_edge() {
        let size = Size::new(300, 300);
        assert_eq!(resize_edge_for_point(size, Point::new(10, 10)), ResizeEdge::NorthWest);
        assert_eq!(resize_edge_for_point(size, Point::new(150, 10)), ResizeEdge::North);
        assert_eq!(resize_edge_for_point(size, Point::new(290, 10)), ResizeEdge::NorthEast);
        assert_eq!(resize_edge_for_point(size, Point::new(10, 150)), ResizeEdge::West);
        assert_eq!(resize_edge_for_point(size, Point::new(290, 150)), ResizeEdge::East);
        assert_eq!(resize_edge_for_point(size, Point::new(10, 290)), ResizeEdge::SouthWest);
        assert_eq!(resize_edge_for_point(size, Point::new(150, 290)), ResizeEdge::South);
        assert_eq!(resize_edge_for_point(size, Point::new(290, 290)), ResizeEdge::SouthEast);
    }

    /// The middle has no nearest edge, and refusing to resize there
    /// would make the gesture fail in the largest part of the window.
    #[test]
    fn the_middle_pulls_the_corner_the_resize_bar_lives_in() {
        assert_eq!(resize_edge_for_point(Size::new(300, 300), Point::new(150, 150)), ResizeEdge::SouthEast);
    }

    /// A press outside the frame (a grab that began before a
    /// configure landed) must still name an edge, not panic.
    #[test]
    fn a_degenerate_frame_or_an_outside_point_still_answers() {
        assert_eq!(resize_edge_for_point(Size::new(0, 0), Point::new(5, 5)), ResizeEdge::SouthEast);
        assert_eq!(resize_edge_for_point(Size::new(100, 100), Point::new(-50, -50)), ResizeEdge::NorthWest);
        assert_eq!(resize_edge_for_point(Size::new(100, 100), Point::new(500, 500)), ResizeEdge::SouthEast);
    }
}
