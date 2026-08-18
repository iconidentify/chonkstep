//! Pure geometry: clamps a candidate content size to a client's
//! `SizeHints` (`WM_NORMAL_HINTS`'s min/max size and resize increment) —
//! WindowMaker's `wWindowConstrainSize` in `moveres.c`. No I/O, no
//! backend dependency, trivially unit-tested — matches this crate's
//! `hittest`/`snap` modules in spirit.

use wm_theme_api::Size;

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
