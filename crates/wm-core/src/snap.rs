//! Pure geometry: pulls a candidate frame rect snug against nearby edges
//! (the screen boundary, other windows' frames) when it lands within a
//! small pixel threshold of one — WindowMaker calls this "edge
//! resistance"/"attraction" (`src/moveres.c`). No I/O, no backend
//! dependency, trivially unit-tested — matches this crate's `hittest`
//! module in spirit.

use wm_theme_api::{Point, Rect};

/// Returns `candidate`'s position nudged so any edge (left/right/top/
/// bottom) within `threshold` pixels of a matching edge in `targets`
/// lands exactly flush with it. Horizontal and vertical snapping are
/// independent — a window can snap its left edge to one target's right
/// edge while its top snaps to a different target's bottom edge.
pub fn snap_position(candidate: Rect, targets: &[Rect], threshold: i32) -> Point {
    let (w, h) = (candidate.size.w as i32, candidate.size.h as i32);
    let (left, top) = (candidate.pos.x, candidate.pos.y);
    let (right, bottom) = (left + w, top + h);

    let mut best_dx: Option<i32> = None;
    let mut best_dy: Option<i32> = None;

    for target in targets {
        let (tl, tt) = (target.pos.x, target.pos.y);
        let (tr, tb) = (tl + target.size.w as i32, tt + target.size.h as i32);

        for edge in [left, right] {
            for target_edge in [tl, tr] {
                consider(&mut best_dx, target_edge - edge, threshold);
            }
        }
        for edge in [top, bottom] {
            for target_edge in [tt, tb] {
                consider(&mut best_dy, target_edge - edge, threshold);
            }
        }
    }

    Point::new(left + best_dx.unwrap_or(0), top + best_dy.unwrap_or(0))
}

/// Keeps the smallest-magnitude delta seen so far, provided it's within
/// `threshold` — multiple nearby edges can compete for the same axis.
fn consider(best: &mut Option<i32>, delta: i32, threshold: i32) {
    if delta.abs() > threshold {
        return;
    }
    if best.map(|b| delta.abs() < b.abs()).unwrap_or(true) {
        *best = Some(delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme_api::Size;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { pos: Point::new(x, y), size: Size::new(w, h) }
    }

    #[test]
    fn snaps_left_edge_to_screen_left_when_close() {
        let candidate = rect(4, 100, 300, 200);
        let screen = rect(0, 0, 1600, 1000);
        let pos = snap_position(candidate, &[screen], 8);
        assert_eq!(pos, Point::new(0, 100));
    }

    #[test]
    fn snaps_right_edge_to_screen_right_when_close() {
        let candidate = rect(1294, 100, 300, 200);
        let screen = rect(0, 0, 1600, 1000);
        let pos = snap_position(candidate, &[screen], 8);
        assert_eq!(pos, Point::new(1300, 100));
    }

    #[test]
    fn does_not_snap_when_far_from_every_edge() {
        let candidate = rect(400, 400, 300, 200);
        let screen = rect(0, 0, 1600, 1000);
        let pos = snap_position(candidate, &[screen], 8);
        assert_eq!(pos, candidate.pos);
    }

    #[test]
    fn snaps_flush_against_another_windows_right_edge() {
        let candidate = rect(506, 100, 300, 200);
        let other = rect(200, 50, 300, 400);
        let pos = snap_position(candidate, &[other], 8);
        assert_eq!(pos.x, 500, "candidate's left edge should snap to the other window's right edge (200+300)");
    }

    #[test]
    fn horizontal_and_vertical_snap_independently_to_different_targets() {
        let candidate = rect(4, 796, 300, 200);
        let screen = rect(0, 0, 1600, 1000);
        let pos = snap_position(candidate, &[screen], 8);
        assert_eq!(pos, Point::new(0, 800), "left snaps to screen x=0, bottom snaps to screen y=1000");
    }
}
