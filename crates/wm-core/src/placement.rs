//! Initial window placement: where a freshly mapped frame goes when
//! the client itself expressed no real preference. Pure geometry —
//! policy in, position out — so every policy is unit-testable against
//! literal rectangles, in the same spirit as [`crate::snap`].
//!
//! Real WindowMaker's `src/placement.c` is the reference, ported here
//! rather than reinvented:
//!
//! - `Smart` is `smartPlaceWindow`: scan candidate frame origins across
//!   the workarea, score each by the summed intersection area against
//!   every existing frame, and keep the minimum. The scan runs in
//!   reading order (top row first, left to right) and only a *strictly*
//!   lower score displaces the incumbent, so ties resolve toward the
//!   top-left and windows fill the screen the way text fills a page.
//!   WindowMaker scans an 8-pixel grid (`PLACETEST_HSTEP`/`_VSTEP`)
//!   and then refines around the coarse winner at 1-pixel granularity;
//!   we keep the grid but replace the refinement pass with exact
//!   candidates flush against each existing frame's right and bottom
//!   edges — the only off-grid positions the refinement ever usefully
//!   found, since a gap between frames is always bounded on the left/
//!   top by another frame's edge or by the workarea itself. That keeps
//!   the scan O(grid) while still packing windows perfectly tight.
//! - `Cascade` is `cascadeWindow`: the classic staircase, each index
//!   one `cascade_step` down and right from the last. Where WindowMaker
//!   resets its counter to zero when the staircase would leave the
//!   screen (burying the first window again), we wrap into a fresh,
//!   horizontally shifted column so long sessions keep producing
//!   distinguishable positions.
//! - `Center` is `center_place_window`: exact centering.
//!
//! The window manager consults this only for clients that requested no
//! meaningful position — an explicit client position (a terminal
//! launched with `-geometry +x+y`) is always honored upstream.

use wm_theme_api::{Point, Rect, Size};

/// Which placement policy `place_frame` applies — configured by the
/// user (`placement` in the config file), `Smart` by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementPolicy {
    Smart,
    Cascade,
    Center,
}

/// Candidate-grid pitch for smart placement, straight from
/// WindowMaker's `PLACETEST_HSTEP`/`PLACETEST_VSTEP` (`wconfig.h`).
/// Coarse enough to keep the scan cheap on large screens, fine enough
/// that the grid winner is within a few pixels of optimal — and the
/// edge-flush candidates (see the module doc) recover the exact packing
/// positions the grid misses.
const PLACETEST_HSTEP: i32 = 8;
const PLACETEST_VSTEP: i32 = 8;

/// Picks the frame origin for a newly mapped window.
///
/// - `workarea`: the usable screen region (screen minus dock/Clip
///   reservations) — never place outside it.
/// - `frame`: the full frame size being placed (chrome included).
/// - `existing`: every visible frame rect on the destination workspace,
///   the ones placement should avoid covering.
/// - `cascade_index`: a monotonically increasing counter the caller
///   keeps; `Cascade` (and `Smart`'s full-screen fallback) derive the
///   staircase offset from it.
/// - `cascade_step`: staircase advance per index, in pixels — callers
///   pass the theme's titlebar height so each cascaded titlebar stays
///   visible under the next, at any scale.
///
/// The result is always clamped so the frame's top-left stays inside
/// `workarea` (a frame larger than the workarea pins to its origin —
/// the titlebar must stay reachable).
pub fn place_frame(
    policy: PlacementPolicy,
    workarea: Rect,
    frame: Size,
    existing: &[Rect],
    cascade_index: usize,
    cascade_step: u32,
) -> Point {
    let pos = match policy {
        PlacementPolicy::Center => center_of(workarea, frame),
        PlacementPolicy::Cascade => cascade_origin(workarea, frame, cascade_index, cascade_step),
        PlacementPolicy::Smart => {
            smart_origin(workarea, frame, existing, cascade_index, cascade_step)
        }
    };
    clamp_to(workarea, frame, pos)
}

/// WindowMaker's `smartPlaceWindow`, restated over our types: minimize
/// the summed overlap with `existing` across a candidate scan of the
/// workarea, reading-order-biased (see the module doc for how the
/// candidate set differs from the original's grid-plus-refinement).
///
/// Two deliberate departures from a naive minimum:
///
/// - A zero-overlap candidate ends the scan immediately. The scan runs
///   in reading order and nothing scores below zero, so the first free
///   position found is already the tie-bias winner — and the common
///   case (sparse workspace) stays O(existing) instead of O(grid).
/// - When even the *best* candidate is fully buried (its summed
///   overlap reaches the frame's own area), minimization has
///   degenerated to a constant: the tie bias would drop every new
///   window on the exact same top-left spot, silently stacking them.
///   WindowMaker accepts that burial; we fall back to the cascade
///   staircase instead so consecutive placements on a packed workspace
///   remain individually grabbable. (The sum double-counts stacked
///   overlaps, so a pathological pile-up could trip this while a
///   sliver of the frame would still have been visible — an acceptable
///   trade for keeping the test a cheap sum rather than a union.)
fn smart_origin(
    workarea: Rect,
    frame: Size,
    existing: &[Rect],
    cascade_index: usize,
    cascade_step: u32,
) -> Point {
    let xs = axis_candidates(
        workarea.pos.x,
        workarea.pos.x + workarea.size.w.saturating_sub(frame.w) as i32,
        PLACETEST_HSTEP,
        existing.iter().map(|r| r.pos.x + r.size.w as i32),
    );
    let ys = axis_candidates(
        workarea.pos.y,
        workarea.pos.y + workarea.size.h.saturating_sub(frame.h) as i32,
        PLACETEST_VSTEP,
        existing.iter().map(|r| r.pos.y + r.size.h as i32),
    );

    let frame_area = frame.w as u64 * frame.h as u64;
    let mut best: Option<(u64, Point)> = None;
    'scan: for &y in &ys {
        for &x in &xs {
            let candidate = Rect::new(Point::new(x, y), frame);
            let score = existing
                .iter()
                .map(|r| overlap_area(candidate, *r))
                .fold(0u64, u64::saturating_add);
            if best.is_none_or(|(incumbent, _)| score < incumbent) {
                best = Some((score, Point::new(x, y)));
                if score == 0 {
                    break 'scan;
                }
            }
        }
    }

    match best {
        Some((score, pos)) if score < frame_area || score == 0 => pos,
        // Everything is buried (or, vacuously, the axis scans were
        // empty, which cannot happen since both always contain their
        // origin) — cascade for inevitability instead.
        _ => cascade_origin(workarea, frame, cascade_index, cascade_step),
    }
}

/// The candidate origins along one axis: the coarse grid from `min` in
/// `step` increments, the far bound `max` itself (the grid rarely lands
/// on it, and flush-with-the-workarea-edge must be reachable), and each
/// existing frame's trailing edge (`packed_edges`) so a new frame can
/// sit exactly against a neighbor. Sorted ascending — the scan order
/// *is* the tie-break policy — and deduplicated so no candidate is
/// scored twice.
fn axis_candidates(
    min: i32,
    max: i32,
    step: i32,
    packed_edges: impl Iterator<Item = i32>,
) -> Vec<i32> {
    let max = max.max(min);
    let mut candidates: Vec<i32> = (min..=max).step_by(step as usize).collect();
    candidates.push(max);
    candidates.extend(packed_edges.filter(|&edge| edge > min && edge < max));
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

/// Intersection area of two frame rects — WindowMaker's
/// `calcIntersectionArea`. Widened to i64 before any addition so a
/// pathological `u32`-sized frame cannot overflow the coordinate math.
fn overlap_area(a: Rect, b: Rect) -> u64 {
    overlap_len(a.pos.x, a.size.w, b.pos.x, b.size.w) * overlap_len(a.pos.y, a.size.h, b.pos.y, b.size.h)
}

/// Intersection length of two line segments (`calcIntersectionLength`),
/// zero when they do not touch.
fn overlap_len(p1: i32, l1: u32, p2: i32, l2: u32) -> u64 {
    let start = (p1 as i64).max(p2 as i64);
    let end = (p1 as i64 + l1 as i64).min(p2 as i64 + l2 as i64);
    (end - start).max(0) as u64
}

/// The staircase origin for `cascade_index`. Each index advances one
/// `cascade_step` down and right (WindowMaker's `cascadeWindow`, where
/// both axes advance by the titlebar height so every buried titlebar
/// peeks out under its successor). A run ends just before the next
/// step would push the frame past the workarea on either axis; the
/// following index starts a fresh column — back at the top, shifted
/// right by two steps per completed run, wrapping that shift so an
/// arbitrarily long session keeps cycling through distinguishable
/// positions instead of resetting onto the very first window the way
/// the original's `cascade_index = 0` reset does.
fn cascade_origin(workarea: Rect, frame: Size, cascade_index: usize, cascade_step: u32) -> Point {
    let step = cascade_step.max(1) as i64;
    let usable_w = workarea.size.w.saturating_sub(frame.w) as i64;
    let usable_h = workarea.size.h.saturating_sub(frame.h) as i64;
    // How many indices a diagonal run holds before the *next* offset
    // would exceed the tighter axis. Always at least one, so a frame
    // with no slack simply pins to the origin every index.
    let per_run = (usable_w.min(usable_h) / step + 1).max(1);
    let run = cascade_index as i64 / per_run;
    let diag = cascade_index as i64 % per_run;
    let column_shift = if usable_w > 0 { (run * step * 2) % (usable_w + 1) } else { 0 };
    Point::new(
        workarea.pos.x + (column_shift + diag * step) as i32,
        workarea.pos.y + (diag * step) as i32,
    )
}

fn center_of(workarea: Rect, frame: Size) -> Point {
    Point::new(
        workarea.pos.x + (workarea.size.w.saturating_sub(frame.w) / 2) as i32,
        workarea.pos.y + (workarea.size.h.saturating_sub(frame.h) / 2) as i32,
    )
}

fn clamp_to(workarea: Rect, frame: Size, pos: Point) -> Point {
    let max_x = workarea.pos.x + (workarea.size.w.saturating_sub(frame.w)) as i32;
    let max_y = workarea.pos.y + (workarea.size.h.saturating_sub(frame.h)) as i32;
    Point::new(pos.x.clamp(workarea.pos.x, max_x.max(workarea.pos.x)), pos.y.clamp(workarea.pos.y, max_y.max(workarea.pos.y)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { pos: Point { x: 0, y: 56 }, size: Size { w: 1600, h: 1000 } };

    /// Total overlap between a placed frame and the rects it was asked
    /// to avoid — what the smart tests assert on, so they check the
    /// property (no overlap) rather than blessing one coordinate.
    fn placed_overlap(pos: Point, frame: Size, existing: &[Rect]) -> u64 {
        let placed = Rect::new(pos, frame);
        existing.iter().map(|r| overlap_area(placed, *r)).sum()
    }

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect::new(Point::new(x, y), Size::new(w, h))
    }

    #[test]
    fn center_policy_centers_within_the_workarea() {
        let pos = place_frame(PlacementPolicy::Center, AREA, Size::new(400, 300), &[], 0, 23);
        assert_eq!(pos, Point::new(600, 56 + 350));
    }

    #[test]
    fn placement_never_leaves_the_workarea() {
        for policy in [PlacementPolicy::Smart, PlacementPolicy::Cascade, PlacementPolicy::Center] {
            for index in 0..200 {
                let pos = place_frame(policy, AREA, Size::new(500, 400), &[], index, 23);
                assert!(pos.x >= AREA.pos.x && pos.y >= AREA.pos.y, "{policy:?} index {index} escaped top-left: {pos:?}");
                assert!(
                    pos.x + 500 <= AREA.pos.x + AREA.size.w as i32 && pos.y + 400 <= AREA.pos.y + AREA.size.h as i32,
                    "{policy:?} index {index} escaped bottom-right: {pos:?}"
                );
            }
        }
    }

    #[test]
    fn an_oversized_frame_pins_to_the_workarea_origin() {
        let pos = place_frame(PlacementPolicy::Smart, AREA, Size::new(2000, 2000), &[], 3, 23);
        assert_eq!(pos, AREA.pos, "the titlebar must stay reachable");
    }

    #[test]
    fn consecutive_cascade_indices_do_not_coincide() {
        let a = place_frame(PlacementPolicy::Cascade, AREA, Size::new(400, 300), &[], 0, 23);
        let b = place_frame(PlacementPolicy::Cascade, AREA, Size::new(400, 300), &[], 1, 23);
        assert_ne!(a, b);
    }

    // ------------------------------------------------------------------
    // Smart placement.

    #[test]
    fn smart_places_an_empty_workspace_at_the_workarea_origin() {
        // Reading-order bias: with the whole workarea free, the very
        // first candidate (the workarea's own top-left) scores zero and
        // wins — not the center, not a cascade offset.
        let pos = place_frame(PlacementPolicy::Smart, AREA, Size::new(400, 300), &[], 0, 23);
        assert_eq!(pos, AREA.pos);
    }

    #[test]
    fn smart_places_a_second_window_clear_of_the_first() {
        let first = rect(AREA.pos.x, AREA.pos.y, 400, 300);
        let pos = place_frame(PlacementPolicy::Smart, AREA, Size::new(400, 300), &[first], 1, 23);
        assert_eq!(placed_overlap(pos, Size::new(400, 300), &[first]), 0, "free space existed, none was used: {pos:?}");
        // And not merely clear but packed: the first free spot in
        // reading order is flush against the first window's right edge.
        assert_eq!(pos, Point::new(400, 56));
    }

    #[test]
    fn smart_reading_order_fills_the_top_row_before_dropping_down() {
        // Both (100, 0) and (0, 100) are completely free; reading order
        // means the top-row spot wins.
        let area = rect(0, 0, 300, 300);
        let existing = [rect(0, 0, 100, 100)];
        let pos = place_frame(PlacementPolicy::Smart, area, Size::new(100, 100), &existing, 0, 20);
        assert_eq!(pos, Point::new(100, 0));
    }

    #[test]
    fn smart_fills_an_exact_hole_between_two_frames() {
        // A 98-wide hole between two frames, its left lip at x=100 —
        // off the 8-pixel grid on both sides, so only the edge-flush
        // candidate derived from the left frame's right edge finds it.
        let area = rect(0, 0, 300, 100);
        let existing = [rect(0, 0, 100, 100), rect(198, 0, 102, 100)];
        let pos = place_frame(PlacementPolicy::Smart, area, Size::new(98, 100), &existing, 0, 20);
        assert_eq!(pos, Point::new(100, 0), "the hole is the only zero-overlap position");
        assert_eq!(placed_overlap(pos, Size::new(98, 100), &existing), 0);
    }

    #[test]
    fn smart_free_space_beats_any_overlap() {
        // The top strip is (almost fully) covered; the earliest
        // candidates in reading order all overlap it. The first truly
        // free position sits below the strip and must win over every
        // slightly-overlapping spot that precedes it in scan order.
        let area = rect(0, 0, 300, 300);
        let existing = [rect(0, 0, 296, 100)];
        let pos = place_frame(PlacementPolicy::Smart, area, Size::new(100, 100), &existing, 0, 20);
        assert_eq!(pos, Point::new(0, 100), "flush under the strip via its bottom-edge candidate");
        assert_eq!(placed_overlap(pos, Size::new(100, 100), &existing), 0);
    }

    #[test]
    fn smart_prefers_the_position_with_least_overlap() {
        // Nowhere is free: the frame is 120 wide, the workarea 200, and
        // a 100-wide window pins the left. Every candidate x in 0..=80
        // overlaps it by (100 - x) columns, so the minimum is the far
        // right — partial overlap chosen by score, not by scan order.
        let area = rect(0, 0, 200, 100);
        let existing = [rect(0, 0, 100, 100)];
        let frame = Size::new(120, 100);
        let pos = place_frame(PlacementPolicy::Smart, area, frame, &existing, 0, 20);
        assert_eq!(pos, Point::new(80, 0));
        assert_eq!(placed_overlap(pos, frame, &existing), 20 * 100, "the irreducible 20-column overlap and no more");
    }

    #[test]
    fn smart_overlap_minimum_is_global_not_first_found() {
        // Two obstructions of different depth: a full-height wall on
        // the left and a shallow one on the right. The frame fits
        // nowhere freely; the scoring must walk past the heavy-overlap
        // region and settle where the summed overlap is smallest.
        let area = rect(0, 0, 300, 100);
        let existing = [rect(0, 0, 200, 100), rect(200, 0, 100, 30)];
        let frame = Size::new(150, 100);
        let pos = place_frame(PlacementPolicy::Smart, area, frame, &existing, 0, 20);
        // At x=150 (the rightmost candidate): 50 columns deep in the
        // wall (50*100) plus the full shallow window (100*30) = 8000,
        // the global minimum (x=144 on the grid scores 8420).
        assert_eq!(pos, Point::new(150, 0));
        assert_eq!(placed_overlap(pos, frame, &existing), 8000);
    }

    #[test]
    fn smart_avoids_a_window_hanging_off_the_workarea() {
        // Frames dragged partly off-screen still occupy their on-screen
        // part; overlap math must handle their negative origin.
        let area = rect(0, 0, 200, 200);
        let existing = [rect(-50, -50, 100, 100)];
        let pos = place_frame(PlacementPolicy::Smart, area, Size::new(100, 100), &existing, 0, 20);
        assert_eq!(pos, Point::new(50, 0), "flush against the on-screen part's right edge");
        assert_eq!(placed_overlap(pos, Size::new(100, 100), &existing), 0);
    }

    #[test]
    fn smart_full_workspace_falls_back_to_cascading() {
        // One frame covers the entire workarea: every candidate is
        // fully buried, minimization is a constant, and smart must
        // degrade to the cascade staircase — identical to the Cascade
        // policy for the same index, so successive windows stack in a
        // grabbable staircase instead of piling on one corner.
        let existing = [AREA];
        let frame = Size::new(400, 300);
        let mut positions = Vec::new();
        for index in 0..40 {
            let smart = place_frame(PlacementPolicy::Smart, AREA, frame, &existing, index, 23);
            let cascade = place_frame(PlacementPolicy::Cascade, AREA, frame, &[], index, 23);
            assert_eq!(smart, cascade, "index {index}");
            assert!(smart.x >= AREA.pos.x && smart.y >= AREA.pos.y);
            assert!(smart.x + 400 <= AREA.pos.x + AREA.size.w as i32);
            assert!(smart.y + 300 <= AREA.pos.y + AREA.size.h as i32);
            positions.push(smart);
        }
        assert_ne!(positions[0], positions[1], "the fallback must still advance per index");
    }

    #[test]
    fn smart_near_full_workspace_still_minimizes_instead_of_cascading() {
        // Same wall-to-wall coverage minus a sliver: the minimum is
        // below the frame's area, so scoring (not the cascade fallback)
        // decides, and it finds the least-buried column.
        let area = rect(0, 0, 200, 100);
        let existing = [rect(0, 0, 180, 100)];
        let frame = Size::new(100, 100);
        let pos = place_frame(PlacementPolicy::Smart, area, frame, &existing, 7, 20);
        assert_eq!(pos, Point::new(100, 0), "20 free columns on the right minimize the burial");
        assert_eq!(placed_overlap(pos, frame, &existing), 80 * 100);
    }

    // ------------------------------------------------------------------
    // Cascade placement.

    #[test]
    fn cascade_advances_down_and_right_by_one_step_per_index() {
        for index in 0..5usize {
            let pos = place_frame(PlacementPolicy::Cascade, AREA, Size::new(400, 300), &[], index, 20);
            let offset = 20 * index as i32;
            assert_eq!(pos, Point::new(AREA.pos.x + offset, AREA.pos.y + offset));
        }
    }

    #[test]
    fn cascade_wraps_into_a_fresh_column_before_leaving_the_workarea() {
        // 600x400 workarea, 300x300 frame, step 50: 100 px of vertical
        // slack holds a three-index run (offsets 0, 50, 100). Index 3
        // must restart at the top — not at the exact origin, but in a
        // column shifted right so it does not bury index 0.
        let area = rect(0, 0, 600, 400);
        let frame = Size::new(300, 300);
        let sequence: Vec<Point> =
            (0..6).map(|i| place_frame(PlacementPolicy::Cascade, area, frame, &[], i, 50)).collect();
        assert_eq!(
            sequence,
            vec![
                Point::new(0, 0),
                Point::new(50, 50),
                Point::new(100, 100),
                Point::new(100, 0),
                Point::new(150, 50),
                Point::new(200, 100),
            ]
        );
    }

    #[test]
    fn cascade_with_no_slack_pins_every_index_to_the_origin() {
        // A frame exactly the workarea size has nowhere to staircase:
        // WindowMaker resets to the origin in this situation and so do
        // we, rather than clamping a runaway diagonal into a corner.
        let area = rect(10, 20, 500, 400);
        for index in 0..10 {
            let pos = place_frame(PlacementPolicy::Cascade, area, Size::new(500, 400), &[], index, 24);
            assert_eq!(pos, area.pos);
        }
    }

    #[test]
    fn cascade_step_zero_still_advances() {
        // A zero step would freeze the staircase (and divide by zero in
        // the run length); it is clamped to one pixel instead.
        let a = place_frame(PlacementPolicy::Cascade, AREA, Size::new(400, 300), &[], 0, 0);
        let b = place_frame(PlacementPolicy::Cascade, AREA, Size::new(400, 300), &[], 1, 0);
        assert_ne!(a, b);
    }

    // ------------------------------------------------------------------
    // Invariants across every policy: containment, degenerate inputs.

    #[test]
    fn placement_containment_sweep_across_sizes_and_indices() {
        // Property-style: for hundreds of policy/size/index combos, a
        // frame that fits stays fully inside; an axis with no slack
        // pins to the workarea's origin on that axis.
        let existing = [rect(0, 56, 300, 200), rect(300, 56, 300, 200)];
        for policy in [PlacementPolicy::Smart, PlacementPolicy::Cascade, PlacementPolicy::Center] {
            for w in [1u32, 37, 250, 799, 1599, 1600, 2400] {
                for h in [1u32, 23, 300, 999, 1000, 3000] {
                    for index in [0usize, 1, 7, 31, 97, 400] {
                        let frame = Size::new(w, h);
                        let pos = place_frame(policy, AREA, frame, &existing, index, 23);
                        let label = format!("{policy:?} {w}x{h} index {index} -> {pos:?}");
                        if w <= AREA.size.w {
                            assert!(
                                pos.x >= AREA.pos.x && pos.x + w as i32 <= AREA.pos.x + AREA.size.w as i32,
                                "x containment: {label}"
                            );
                        } else {
                            assert_eq!(pos.x, AREA.pos.x, "oversized width pins x: {label}");
                        }
                        if h <= AREA.size.h {
                            assert!(
                                pos.y >= AREA.pos.y && pos.y + h as i32 <= AREA.pos.y + AREA.size.h as i32,
                                "y containment: {label}"
                            );
                        } else {
                            assert_eq!(pos.y, AREA.pos.y, "oversized height pins y: {label}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn degenerate_geometry_does_not_panic() {
        // 1x1 and 0x0 workareas, zero-sized frames, zero-area existing
        // rects: every combination must resolve (pinned to the origin
        // where nothing fits) without panicking or dividing by zero.
        let tiny_areas = [rect(0, 0, 1, 1), rect(5, 7, 0, 0)];
        let frames = [Size::new(0, 0), Size::new(1, 1), Size::new(5, 5)];
        let existing = [rect(0, 0, 0, 0), rect(0, 0, 1, 1)];
        for policy in [PlacementPolicy::Smart, PlacementPolicy::Cascade, PlacementPolicy::Center] {
            for area in tiny_areas {
                for frame in frames {
                    for index in [0usize, 1, 13] {
                        let pos = place_frame(policy, area, frame, &existing, index, 23);
                        assert_eq!(pos, area.pos, "{policy:?} {frame:?} in {area:?} index {index}");
                    }
                }
            }
        }
    }

    #[test]
    fn zero_sized_frame_in_a_real_workarea_lands_at_the_origin() {
        // A zero-area frame overlaps nothing by definition, so smart's
        // first candidate is free — and must not trip the "everything
        // is buried" cascade fallback (0 >= 0 * 0).
        let existing = [AREA];
        let pos = place_frame(PlacementPolicy::Smart, AREA, Size::new(0, 0), &existing, 5, 23);
        assert_eq!(pos, AREA.pos);
    }

    // ------------------------------------------------------------------
    // The scoring primitive itself.

    #[test]
    fn overlap_area_matches_hand_computed_rectangles() {
        let a = rect(0, 0, 100, 100);
        assert_eq!(overlap_area(a, rect(100, 0, 50, 50)), 0, "flush edges do not overlap");
        assert_eq!(overlap_area(a, rect(200, 200, 10, 10)), 0, "disjoint");
        assert_eq!(overlap_area(a, rect(50, 50, 100, 100)), 50 * 50, "corner overlap");
        assert_eq!(overlap_area(a, rect(25, 25, 50, 50)), 50 * 50, "containment counts the inner area");
        assert_eq!(overlap_area(a, rect(-25, -25, 50, 50)), 25 * 25, "negative origins");
        assert_eq!(overlap_area(a, rect(10, 10, 0, 50)), 0, "zero width is zero area");
        assert_eq!(overlap_area(a, a), 100 * 100, "identity");
    }
}
