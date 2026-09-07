//! A constant-sized translucent plane with a transparent selection hole.
//!
//! Moving four solid rectangles makes the damage tracker repaint their entire
//! old and new bounds. Keeping the plane's geometry fixed lets us report just
//! the pixels whose dimming changed, without allocating a fullscreen texture.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use smithay::backend::renderer::{
    element::{Element, Id, RenderElement},
    utils::{CommitCounter, DamageSet},
    Color32F, Frame, Renderer,
};
use smithay::utils::{Buffer, Physical, Rectangle, Scale};
use wm_theme_api::Rect;

type PixelRect = Rectangle<i32, Physical>;
const HISTORY: usize = 64;
const COLOR: Color32F = Color32F::new(0.0, 0.0, 0.0, 0.42);

#[derive(Debug)]
struct History {
    commit: CommitCounter,
    holes: VecDeque<(CommitCounter, Option<Rect>)>,
}

#[derive(Debug)]
pub(super) struct Dimming {
    id: Id,
    history: Rc<RefCell<History>>,
}

impl Default for Dimming {
    fn default() -> Self {
        Self {
            id: Id::new(),
            history: Rc::new(RefCell::new(History {
                commit: CommitCounter::default(),
                holes: VecDeque::with_capacity(HISTORY),
            })),
        }
    }
}

impl Dimming {
    pub(super) fn element(&self, hole: Option<Rect>, viewport: Rect) -> DimmingElement {
        let hole = hole.filter(|r| r.size.w > 0 && r.size.h > 0);
        let mut history = self.history.borrow_mut();
        if history
            .holes
            .back()
            .is_none_or(|(_, previous)| *previous != hole)
        {
            history.commit.increment();
            if history.holes.len() == HISTORY {
                history.holes.pop_front();
            }
            let commit = history.commit;
            history.holes.push_back((commit, hole));
        }
        DimmingElement {
            id: self.id.clone(),
            history: self.history.clone(),
            commit: history.commit,
            hole: local_hole(hole, viewport),
            viewport,
        }
    }
}

/// Snapshots retain their own hole and commit while other outputs advance.
/// The small shared history survives scene reuse and falls back to full damage
/// when an output has not presented within the bounded history window.
#[derive(Debug, Clone)]
pub(crate) struct DimmingElement {
    id: Id,
    history: Rc<RefCell<History>>,
    commit: CommitCounter,
    hole: Option<PixelRect>,
    viewport: Rect,
}

fn bounds(viewport: Rect) -> PixelRect {
    Rectangle::from_size((viewport.size.w as i32, viewport.size.h as i32).into())
}

fn local_hole(hole: Option<Rect>, viewport: Rect) -> Option<PixelRect> {
    let hole = hole?;
    // Widen before translating: outputs can have negative origins, and a hole
    // may lie on a different output entirely.
    let x1 =
        (i64::from(hole.pos.x) - i64::from(viewport.pos.x)).clamp(0, i64::from(viewport.size.w));
    let y1 =
        (i64::from(hole.pos.y) - i64::from(viewport.pos.y)).clamp(0, i64::from(viewport.size.h));
    let x2 = (i64::from(hole.pos.x) + i64::from(hole.size.w) - i64::from(viewport.pos.x))
        .clamp(x1, i64::from(viewport.size.w));
    let y2 = (i64::from(hole.pos.y) + i64::from(hole.size.h) - i64::from(viewport.pos.y))
        .clamp(y1, i64::from(viewport.size.h));
    (x2 > x1 && y2 > y1).then(|| {
        Rectangle::new(
            (x1 as i32, y1 as i32).into(),
            ((x2 - x1) as i32, (y2 - y1) as i32).into(),
        )
    })
}

/// Disjoint pieces of `rect` outside `hole`, with no heap allocation.
fn subtract(rect: PixelRect, hole: Option<PixelRect>, mut emit: impl FnMut(PixelRect)) {
    let Some(hole) = hole.and_then(|hole| rect.intersection(hole)) else {
        emit(rect);
        return;
    };
    let right = rect.loc.x + rect.size.w;
    let bottom = rect.loc.y + rect.size.h;
    let hole_right = hole.loc.x + hole.size.w;
    let hole_bottom = hole.loc.y + hole.size.h;
    for (x, y, w, h) in [
        (rect.loc.x, rect.loc.y, rect.size.w, hole.loc.y - rect.loc.y),
        (rect.loc.x, hole_bottom, rect.size.w, bottom - hole_bottom),
        (rect.loc.x, hole.loc.y, hole.loc.x - rect.loc.x, hole.size.h),
        (hole_right, hole.loc.y, right - hole_right, hole.size.h),
    ] {
        if w > 0 && h > 0 {
            emit(Rectangle::new((x, y).into(), (w, h).into()));
        }
    }
}

fn difference(old: Option<PixelRect>, new: Option<PixelRect>) -> DamageSet<i32, Physical> {
    let mut damage = [PixelRect::default(); 8];
    let mut len = 0;
    let mut emit = |rect| {
        damage[len] = rect;
        len += 1;
    };
    if let Some(old) = old {
        subtract(old, new, &mut emit);
    }
    if let Some(new) = new {
        subtract(new, old, &mut emit);
    }
    DamageSet::from_slice(&damage[..len])
}

impl Element for DimmingElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        // This unbacked element represents a virtual plane in global desktop
        // coordinates. Including its viewport origin makes the damage tracker
        // invalidate an output that moved without changing size or selection.
        // `draw` uses physical output-local pixels and does not sample a texture.
        Rectangle::new(
            (
                f64::from(self.viewport.pos.x),
                f64::from(self.viewport.pos.y),
            )
                .into(),
            (
                f64::from(self.viewport.size.w),
                f64::from(self.viewport.size.h),
            )
                .into(),
        )
    }

    fn geometry(&self, _scale: Scale<f64>) -> PixelRect {
        bounds(self.viewport)
    }

    fn alpha(&self) -> f32 {
        COLOR.a()
    }

    fn damage_since(
        &self,
        _scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            return DamageSet::default();
        }
        if self.commit.distance(commit).is_some() {
            let history = self.history.borrow();
            if let Some((_, old)) = history
                .holes
                .iter()
                .rev()
                .find(|(at, _)| Some(*at) == commit)
            {
                return difference(local_hole(*old, self.viewport), self.hole);
            }
        }
        DamageSet::from_slice(&[bounds(self.viewport)])
    }
}

impl<R: Renderer> RenderElement<R> for DimmingElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: PixelRect,
        damage: &[PixelRect],
        _opaque: &[PixelRect],
    ) -> Result<(), R::Error> {
        // The capture plane is output-local physical pixels and is appended
        // after scene clipping. Batch clipped solid damage on the stack.
        let mut pieces = [PixelRect::default(); 32];
        let mut len = 0;
        for rect in damage {
            if len > pieces.len() - 4 {
                frame.draw_solid(dst, &pieces[..len], COLOR)?;
                len = 0;
            }
            if let Some(rect) = rect.intersection(bounds(self.viewport)) {
                subtract(rect, self.hole, |part| {
                    pieces[len] = part;
                    len += 1;
                });
            }
        }
        frame.draw_solid(dst, &pieces[..len], COLOR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme_api::{Point, Size};

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect::new(Point::new(x, y), Size::new(w, h))
    }

    #[test]
    fn damage_matches_pixel_oracle_and_never_overlaps() {
        let viewport = rect(-4, -3, 16, 12);
        let mut seed = 17u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        for _ in 0..4096 {
            let old = local_hole(
                Some(rect(
                    (next() % 28) as i32 - 12,
                    (next() % 24) as i32 - 10,
                    next() % 20,
                    next() % 16,
                )),
                viewport,
            );
            let new = local_hole(
                Some(rect(
                    (next() % 28) as i32 - 12,
                    (next() % 24) as i32 - 10,
                    next() % 20,
                    next() % 16,
                )),
                viewport,
            );
            let damage = difference(old, new);
            for y in 0..12 {
                for x in 0..16 {
                    let at = (x, y);
                    let expected =
                        old.is_some_and(|r| r.contains(at)) ^ new.is_some_and(|r| r.contains(at));
                    assert_eq!(
                        damage.iter().filter(|r| r.contains(at)).count(),
                        usize::from(expected)
                    );
                }
            }
        }
    }

    #[test]
    fn translation_damage_is_sparse_and_allocation_free() {
        let dimming = Dimming::default();
        let viewport = rect(0, 0, 3840, 2160);
        let mut previous = dimming.element(Some(rect(100, 100, 1000, 800)), viewport);
        let (_, allocations) = chonk_test_support::measure(|| {
            for x in 101..10_000 {
                let next = dimming.element(Some(rect(x, 100, 1000, 800)), viewport);
                assert_eq!(next.geometry(1.0.into()), previous.geometry(1.0.into()));
                let damage = next.damage_since(1.0.into(), Some(previous.current_commit()));
                assert!(damage.iter().map(|r| r.size.w * r.size.h).sum::<i32>() <= 1600);
                assert!(next
                    .damage_since(1.0.into(), Some(next.current_commit()))
                    .is_empty());
                previous = next;
            }
        });
        assert_eq!((allocations.calls, allocations.requested_bytes), (0, 0));
    }

    #[test]
    fn snapshots_outputs_buffer_age_and_history_expiry() {
        let dimming = Dimming::default();
        let left = rect(-100, 0, 100, 100);
        let right = rect(0, 0, 100, 100);
        let first = dimming.element(None, left);
        let old = dimming.element(Some(rect(-10, 10, 20, 20)), left);
        let other = dimming.element(Some(rect(-10, 10, 20, 20)), right);
        assert_eq!(old.current_commit(), other.current_commit());
        assert_eq!(
            old.hole,
            Some(Rectangle::new((90, 10).into(), (10, 20).into()))
        );
        assert_eq!(
            other.hole,
            Some(Rectangle::new((0, 10).into(), (10, 20).into()))
        );
        let newer = dimming.element(Some(rect(20, 10, 20, 20)), left);
        assert_eq!(
            old.damage_since(1.0.into(), Some(first.current_commit()))
                .iter()
                .map(|r| r.size.w * r.size.h)
                .sum::<i32>(),
            200
        );
        assert!(newer
            .damage_since(1.0.into(), Some(first.current_commit()))
            .is_empty());
        // A snapshot must keep its own hole when a newer output advances state.
        assert_eq!(old.hole.unwrap().size.w, 10);
        for x in 0..HISTORY {
            dimming.element(Some(rect(x as i32, 0, 1, 1)), left);
        }
        assert_eq!(
            &*old.damage_since(1.0.into(), Some(first.current_commit())),
            &[bounds(left)]
        );
        assert_eq!(
            &*old.damage_since(1.0.into(), Some(newer.current_commit())),
            &[bounds(left)]
        );
    }

    #[test]
    fn output_relocation_invalidates_its_plane_without_churning_other_outputs() {
        use smithay::backend::renderer::damage::OutputDamageTracker;
        use smithay::utils::Transform;

        let dimming = Dimming::default();
        let first = rect(-100, 0, 100, 100);
        let second = rect(0, 0, 100, 100);
        let hole = Some(rect(-20, 10, 40, 20));
        let mut trackers = [
            OutputDamageTracker::new((100, 100), 1.0, Transform::Normal),
            OutputDamageTracker::new((100, 100), 1.0, Transform::Normal),
        ];
        for (index, viewport) in [first, second].into_iter().enumerate() {
            let element = dimming.element(hole, viewport);
            assert!(trackers[index]
                .damage_output(0, &[element])
                .unwrap()
                .0
                .is_some());
        }
        for _ in 0..3 {
            for (index, viewport) in [first, second].into_iter().enumerate() {
                let element = dimming.element(hole, viewport);
                assert!(trackers[index]
                    .damage_output(1, &[element])
                    .unwrap()
                    .0
                    .is_none());
            }
        }
        let before = dimming.element(hole, first);
        let moved = dimming.element(hole, rect(-50, 0, 100, 100));
        assert_eq!(before.current_commit(), moved.current_commit());
        assert_ne!(before.hole, moved.hole);
        assert_eq!(before.geometry(1.0.into()), moved.geometry(1.0.into()));
        assert_eq!(
            trackers[0].damage_output(1, &[moved]).unwrap().0.unwrap(),
            &[bounds(first)]
        );
        assert!(trackers[1]
            .damage_output(1, &[dimming.element(hole, second)])
            .unwrap()
            .0
            .is_none());
    }

    #[test]
    fn retained_buffers_match_full_repaint_after_moves_resizes_and_clear() {
        use smithay::backend::renderer::damage::OutputDamageTracker;
        use smithay::utils::Transform;

        let viewport = rect(-8, -4, 64, 48);
        // Exercise reused buffers of different ages, including an output that
        // sleeps beyond our history. Repaint only the real tracker's damage,
        // then compare every pixel against an independent full-scene oracle.
        for age in [1, 2, 3, 5, 70] {
            let dimming = Dimming::default();
            let mut tracker = OutputDamageTracker::new((64, 48), 1.0, Transform::Normal);
            let mut buffers = vec![vec![false; 64 * 48]; age];
            for frame in 0..210 {
                let hole = if frame % 19 == 0 {
                    None
                } else {
                    Some(rect(
                        (frame % 90) as i32 - 28,
                        (frame % 62) as i32 - 18,
                        1 + (frame % 37) as u32,
                        1 + (frame % 29) as u32,
                    ))
                };
                let element = dimming.element(hole, viewport);
                let (damage, _) = tracker
                    .damage_output(if frame < age { 0 } else { age }, &[element])
                    .unwrap();
                let pixels = &mut buffers[frame % age];
                if let Some(damage) = damage {
                    for piece in damage {
                        for y in piece.loc.y..piece.loc.y + piece.size.h {
                            for x in piece.loc.x..piece.loc.x + piece.size.w {
                                let global = Point::new(x + viewport.pos.x, y + viewport.pos.y);
                                pixels[(y * 64 + x) as usize] =
                                    !hole.is_some_and(|r| r.contains(global));
                            }
                        }
                    }
                }
                for y in 0..48 {
                    for x in 0..64 {
                        let global = Point::new(x + viewport.pos.x, y + viewport.pos.y);
                        assert_eq!(
                            pixels[(y * 64 + x) as usize],
                            !hole.is_some_and(|r| r.contains(global)),
                            "age={age} frame={frame} pixel={x},{y}"
                        );
                    }
                }
            }
        }
    }
}
