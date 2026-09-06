//! Pointer constraint policy, shared by physical, nested and virtual input.
//!
//! Smithay owns protocol objects and double-buffered regions. This module owns
//! activation and enforcement against the scene's surface-local input geometry.
//! Never hit-test inside `with_pointer_constraint`: that callback holds the
//! surface-state mutex that hit-testing would need to acquire again.

use smithay::input::pointer::PointerHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::RegionAttributes;
use smithay::wayland::pointer_constraints::{
    with_pointer_constraint, PointerConstraint, PointerConstraintsHandler,
};

use super::{surface::SurfaceCoordinates, surface_focus_at};
use crate::state::Compositor;

type Position = Point<f64, Logical>;

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Constrained {
    pub position: Position,
    pub locked: bool,
}

fn in_region(
    coordinates: SurfaceCoordinates,
    point: Position,
    region: Option<&RegionAttributes>,
) -> bool {
    region.is_none_or(|region| {
        let local = coordinates.local(point);
        region.contains((local.x.floor() as i32, local.y.floor() as i32))
    })
}

/// Region and input-geometry changes are double-buffered, not mouse events.
/// Reconcile only the focused surface after Smithay has applied its commit;
/// ordinary background commits do not need a scene hit-test or constraint lock.
pub(crate) fn surface_committed(state: &mut Compositor, surface: &WlSurface) {
    let focused = state
        .seat
        .get_pointer()
        .and_then(|pointer| pointer.current_focus());
    if focused
        .as_ref()
        .is_none_or(|focused| focused.surface() != surface)
    {
        return;
    }
    let _ = apply(state, state.pointer_location);
    activate_for_current_focus(state);
}

pub(super) fn is_locked(state: &Compositor) -> bool {
    let Some(pointer) = state.seat.get_pointer() else {
        return false;
    };
    let Some(surface) = pointer.current_focus() else {
        return false;
    };
    with_pointer_constraint(surface.surface(), &pointer, |constraint| {
        constraint.is_some_and(|constraint| {
            constraint.is_active() && matches!(&*constraint, PointerConstraint::Locked(_))
        })
    })
}

/// Apply only an already-active constraint. Pending constraints activate after
/// motion reaches their region, so the client observes its entry coordinate
/// before being told the pointer is locked there.
pub(super) fn apply(state: &mut Compositor, proposed: Position) -> Constrained {
    let mut result = Constrained {
        position: proposed,
        locked: false,
    };
    let Some(pointer) = state.seat.get_pointer() else {
        return result;
    };
    let Some(surface) = pointer.current_focus() else {
        return result;
    };
    let active = with_pointer_constraint(surface.surface(), &pointer, |constraint| {
        constraint
            .filter(|constraint| constraint.is_active())
            .map(|constraint| matches!(&*constraint, PointerConstraint::Confined(_)))
    });
    let Some(confined) = active else {
        return result;
    };

    let anchor = state.pointer_location;
    let coordinates = surface_focus_at(state.wm.backend(), anchor, surface.surface())
        .map(|(target, _)| target.coordinates());
    // Intersect the explicit constraint region with the surface's actual
    // input region and visibility. Compute this before locking surface state.
    let proposed_on_surface =
        confined && surface_focus_at(state.wm.backend(), proposed, surface.surface()).is_some();
    with_pointer_constraint(surface.surface(), &pointer, |constraint| {
        let Some(constraint) = constraint else {
            return;
        };
        let Some(coordinates) = coordinates else {
            constraint.deactivate();
            return;
        };
        if !in_region(coordinates, anchor, constraint.region()) {
            // A committed region change may exclude the old position. The
            // protocol permits deactivation instead of a surprise warp.
            constraint.deactivate();
            return;
        }
        match &*constraint {
            PointerConstraint::Locked(_) => {
                result.position = anchor;
                result.locked = true;
            }
            PointerConstraint::Confined(confined) => {
                if !proposed_on_surface || !in_region(coordinates, proposed, confined.region()) {
                    result.position = anchor;
                }
            }
        }
    });
    result
}

/// Activate only at an already-delivered, visible surface-local position that
/// belongs to both input regions. No mouse warp or focus change is implied.
pub(super) fn activate_for_current_focus(state: &Compositor) {
    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };
    let Some(surface) = pointer.current_focus() else {
        return;
    };
    let pending = with_pointer_constraint(surface.surface(), &pointer, |constraint| {
        constraint.is_some_and(|constraint| !constraint.is_active())
    });
    if !pending {
        return;
    }
    let position = state.pointer_location;
    let Some((target, _)) = surface_focus_at(state.wm.backend(), position, surface.surface())
    else {
        return;
    };
    with_pointer_constraint(surface.surface(), &pointer, |constraint| {
        if let Some(constraint) = constraint {
            if in_region(target.coordinates(), position, constraint.region()) {
                constraint.activate();
            }
        }
    });
}

impl PointerConstraintsHandler for Compositor {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        if pointer
            .current_focus()
            .as_ref()
            .is_some_and(|target| target.surface() == surface)
        {
            activate_for_current_focus(self);
        }
    }

    // Smithay commits this surface-local hint for release_pointer_constraint.
    // Committing a hint must not itself move the currently locked pointer.
    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        location: Position,
    ) {
        tracing::debug!(surface = ?surface.id(), x = location.x, y = location.y,
            "client set a cursor-position hint for when its pointer lock ends");
    }
}
