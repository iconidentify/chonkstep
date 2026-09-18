//! EWMH reservations owned by external client windows. No polling or dock code.
use super::*;

impl X11Backend {
    pub fn take_workarea_change(&mut self) -> bool {
        std::mem::take(&mut self.struts_dirty)
    }

    pub(super) fn refresh_strut(&mut self, window: Window, mapped: bool) {
        let read = |atom, count| -> Option<Vec<u32>> {
            let reply = self
                .conn
                .get_property(false, window, atom, AtomEnum::CARDINAL, 0, count)
                .ok()?
                .reply()
                .ok()?;
            if reply.bytes_after != 0 {
                return None;
            }
            let values: Vec<_> = reply.value32()?.collect();
            (values.len() == count as usize).then_some(values)
        };
        let next = mapped
            .then(|| {
                read(self.ewmh.net_wm_strut_partial, 12)
                    .map(|v| <[u32; 12]>::try_from(v).unwrap())
                    .or_else(|| {
                        read(self.ewmh.net_wm_strut, 4).map(|v| {
                            [
                                v[0],
                                v[1],
                                v[2],
                                v[3],
                                0,
                                u32::MAX,
                                0,
                                u32::MAX,
                                0,
                                u32::MAX,
                                0,
                                u32::MAX,
                            ]
                        })
                    })
                    .filter(|s| s[..4].iter().any(|v| *v != 0))
            })
            .flatten();
        if self.external_struts.get(&window).copied() != next {
            if let Some(strut) = next {
                self.external_struts.insert(window, strut);
            } else {
                self.external_struts.remove(&window);
            }
            self.struts_dirty = true;
        }
    }
}

pub(super) fn constrain(area: Rect, root: Size, struts: impl Iterator<Item = [u32; 12]>) -> Rect {
    let (x0, y0) = (area.pos.x as i64, area.pos.y as i64);
    let (x1, y1) = (x0 + area.size.w as i64, y0 + area.size.h as i64);
    let (mut left, mut top, mut right, mut bottom) = (x0, y0, x1, y1);
    let overlaps = |start: u32, end: u32, lo: i64, hi: i64| {
        start <= end && (start as i64) < hi && end as i64 >= lo
    };
    for s in struts {
        if s[0] != 0 && overlaps(s[4], s[5], y0, y1) {
            left = left.max(s[0].min(root.w) as i64);
        }
        if s[1] != 0 && overlaps(s[6], s[7], y0, y1) {
            right = right.min(root.w.saturating_sub(s[1]) as i64);
        }
        if s[2] != 0 && overlaps(s[8], s[9], x0, x1) {
            top = top.max(s[2].min(root.h) as i64);
        }
        if s[3] != 0 && overlaps(s[10], s[11], x0, x1) {
            bottom = bottom.min(root.h.saturating_sub(s[3]) as i64);
        }
    }
    // Malformed or opposing reservations never produce an empty/off-output workarea.
    left = left.clamp(x0, (x1 - 1).max(x0));
    top = top.clamp(y0, (y1 - 1).max(y0));
    right = right.clamp(left + 1, x1.max(left + 1));
    bottom = bottom.clamp(top + 1, y1.max(top + 1));
    Rect {
        pos: Point::new(left as i32, top as i32),
        size: Size::new((right - left) as u32, (bottom - top) as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_struts_only_constrain_outputs_in_their_span() {
        let upper = Rect {
            pos: Point::new(0, 0),
            size: Size::new(1280, 720),
        };
        let lower = Rect {
            pos: Point::new(0, 720),
            size: Size::new(1280, 720),
        };
        let root = Size::new(1280, 1440);
        let strut = [0, 56, 0, 0, 0, 0, 0, 719, 0, 0, 0, 0];
        assert_eq!(constrain(upper, root, [strut].into_iter()).size.w, 1224);
        assert_eq!(constrain(lower, root, [strut].into_iter()), lower);
        assert_eq!(constrain(upper, root, [].into_iter()), upper);
    }
    #[test]
    fn overlapping_panels_combine_by_maximum_and_keep_a_pixel_usable() {
        let area = Rect {
            pos: Point::new(0, 0),
            size: Size::new(1280, 720),
        };
        let first = [0, 56, 0, 0, 0, 0, 0, 719, 0, 0, 0, 0];
        let mut second = first;
        second[1] = 84;
        assert_eq!(
            constrain(area, area.size, [first, second].into_iter())
                .size
                .w,
            1196
        );
        assert_eq!(
            constrain(area, area.size, [[u32::MAX; 12]].into_iter()),
            area
        ); // invalid spans outside output
        let mut hostile = first;
        hostile[1] = u32::MAX;
        assert_eq!(constrain(area, area.size, [hostile].into_iter()).size.w, 1);
    }
}
