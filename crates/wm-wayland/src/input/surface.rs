//! The boundary between the pixel-based scene and surface-local seat events.
//!
//! Smithay 0.7 takes a surface and an origin, then subtracts that origin.
//! Subtraction alone cannot express a scaled surface. Worse, its pointer and
//! touch grabs retain the *press-time* origin. Recalculating a synthetic origin
//! on every hover therefore fixes hover but distorts every subsequent drag.
//!
//! Keep the complete affine transform in the focus target. Pointer/touch
//! delivery reconstructs the scene position before converting to local units,
//! even when Smithay retained the target and origin from an earlier event.
//! The synthetic origin is still supplied for Smithay's data-device drag code,
//! which bypasses PointerTarget/TouchTarget and subtracts the *current* origin
//! itself. All adaptation is here; no custom copy of Smithay's grab state
//! machine, shared mutable transform, allocation, or seat-lock re-entry.

use std::borrow::Cow;

use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, MotionEvent,
    PointerTarget, RelativeMotionEvent,
};
use smithay::input::{touch, Seat};
use smithay::reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource};
use smithay::utils::{IsAlive, Logical, Point, Serial};
use smithay::wayland::seat::WaylandFocus;

use crate::state::Compositor;

type Position = Point<f64, Logical>;

/// A surface's physical origin and device pixels per surface-local unit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SurfaceCoordinates {
    origin: Position,
    scale: f64,
}

impl SurfaceCoordinates {
    pub(crate) fn new(origin: Position, scale: f64) -> Self {
        let scale = valid_scale(scale);
        Self { origin, scale }
    }

    fn from_tree(anchor: Position, found: Position, scale: f64) -> Self {
        let root = Self::new(anchor, scale);
        Self::new(root.global(found - anchor), scale)
    }

    pub(crate) fn local(self, global: Position) -> Position {
        ((global.x - self.origin.x) / self.scale, (global.y - self.origin.y) / self.scale).into()
    }

    pub(crate) fn global(self, local: Position) -> Position {
        (self.origin.x + local.x * self.scale, self.origin.y + local.y * self.scale).into()
    }

    fn seat_origin(self, global: Position) -> Position {
        global - self.local(global)
    }
}

pub(super) fn valid_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale >= 0.125 {
        scale
    } else {
        1.0
    }
}

/// Surface identity plus the coordinate mapping retained by an implicit grab.
/// Identity deliberately excludes geometry: moving a surface is not a leave / enter.
#[derive(Clone, Debug)]
pub struct SurfaceTarget {
    surface: WlSurface,
    coordinates: SurfaceCoordinates,
    seat_origin: Position,
}

impl SurfaceTarget {
    /// `found` is Smithay's tree-walk origin: the physical root anchor plus
    /// surface-local subsurface offsets. Convert those offsets back to pixels.
    pub(super) fn from_tree(
        surface: WlSurface,
        anchor: Position,
        found: Position,
        scale: f64,
        global: Position,
    ) -> (Self, Position) {
        let coordinates = SurfaceCoordinates::from_tree(anchor, found, scale);
        let seat_origin = coordinates.seat_origin(global);
        (Self { surface, coordinates, seat_origin }, seat_origin)
    }

    pub(crate) fn surface(&self) -> &WlSurface {
        &self.surface
    }

    pub(super) fn coordinates(&self) -> SurfaceCoordinates {
        self.coordinates
    }

    /// Reuse the seat's exact retained focus/origin when a pointer lock keeps
    /// its absolute position unchanged. Relative events preserve their units.
    pub(super) fn into_focus_pair(self) -> (Self, Position) {
        let origin = self.seat_origin;
        (self, origin)
    }

    fn location(&self, seat_local: Position) -> Position {
        self.coordinates.local(seat_local + self.seat_origin)
    }

    fn motion_event(&self, event: &MotionEvent) -> MotionEvent {
        MotionEvent { location: self.location(event.location), ..*event }
    }
}

impl PartialEq for SurfaceTarget {
    fn eq(&self, other: &Self) -> bool {
        self.surface == other.surface
    }
}

impl IsAlive for SurfaceTarget {
    fn alive(&self) -> bool {
        self.surface.is_alive()
    }
}

impl WaylandFocus for SurfaceTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        Some(Cow::Borrowed(&self.surface))
    }
}

macro_rules! forward_pointer_events {
    ($($method:ident($event:ident: $event_type:ty)),* $(,)?) => {$(
        fn $method(&self, seat: &Seat<Compositor>, data: &mut Compositor, $event: $event_type) {
            PointerTarget::$method(&self.surface, seat, data, $event);
        }
    )*};
}

impl PointerTarget<Compositor> for SurfaceTarget {
    fn enter(&self, seat: &Seat<Compositor>, data: &mut Compositor, event: &MotionEvent) {
        PointerTarget::enter(&self.surface, seat, data, &self.motion_event(event));
    }

    fn motion(&self, seat: &Seat<Compositor>, data: &mut Compositor, event: &MotionEvent) {
        PointerTarget::motion(&self.surface, seat, data, &self.motion_event(event));
    }

    fn leave(&self, seat: &Seat<Compositor>, data: &mut Compositor, serial: Serial, time: u32) {
        PointerTarget::leave(&self.surface, seat, data, serial, time);
    }

    fn frame(&self, seat: &Seat<Compositor>, data: &mut Compositor) {
        PointerTarget::frame(&self.surface, seat, data);
    }

    // Relative, scroll and gesture streams have their own units. They are not
    // absolute scene positions; preserve their existing semantics and raw input.
    forward_pointer_events!(
        relative_motion(event: &RelativeMotionEvent),
        button(event: &ButtonEvent),
        axis(event: AxisFrame),
        gesture_swipe_begin(event: &GestureSwipeBeginEvent),
        gesture_swipe_update(event: &GestureSwipeUpdateEvent),
        gesture_swipe_end(event: &GestureSwipeEndEvent),
        gesture_pinch_begin(event: &GesturePinchBeginEvent),
        gesture_pinch_update(event: &GesturePinchUpdateEvent),
        gesture_pinch_end(event: &GesturePinchEndEvent),
        gesture_hold_begin(event: &GestureHoldBeginEvent),
        gesture_hold_end(event: &GestureHoldEndEvent),
    );
}

impl touch::TouchTarget<Compositor> for SurfaceTarget {
    fn down(&self, seat: &Seat<Compositor>, data: &mut Compositor, event: &touch::DownEvent, seq: Serial) {
        let event = touch::DownEvent { location: self.location(event.location), ..*event };
        touch::TouchTarget::down(&self.surface, seat, data, &event, seq);
    }

    fn motion(&self, seat: &Seat<Compositor>, data: &mut Compositor, event: &touch::MotionEvent, seq: Serial) {
        let event = touch::MotionEvent { location: self.location(event.location), ..*event };
        touch::TouchTarget::motion(&self.surface, seat, data, &event, seq);
    }

    fn up(&self, seat: &Seat<Compositor>, data: &mut Compositor, event: &touch::UpEvent, seq: Serial) {
        touch::TouchTarget::up(&self.surface, seat, data, event, seq);
    }

    fn frame(&self, seat: &Seat<Compositor>, data: &mut Compositor, seq: Serial) {
        touch::TouchTarget::frame(&self.surface, seat, data, seq);
    }

    fn cancel(&self, seat: &Seat<Compositor>, data: &mut Compositor, seq: Serial) {
        touch::TouchTarget::cancel(&self.surface, seat, data, seq);
    }

    fn shape(&self, seat: &Seat<Compositor>, data: &mut Compositor, event: &touch::ShapeEvent, seq: Serial) {
        touch::TouchTarget::shape(&self.surface, seat, data, event, seq);
    }

    fn orientation(
        &self,
        seat: &Seat<Compositor>,
        data: &mut Compositor,
        event: &touch::OrientationEvent,
        seq: Serial,
    ) {
        touch::TouchTarget::orientation(&self.surface, seat, data, event, seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_point(actual: Position, expected: Position) {
        assert!(
            (actual.x - expected.x).abs() < 1e-9 && (actual.y - expected.y).abs() < 1e-9,
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn local_global_roundtrip_including_negative_output_origins() {
        for scale in [0.125, 1.0, 1.25, 1.5, 2.0, 3.0] {
            let coordinates = SurfaceCoordinates::new((-1920.25, 137.5).into(), scale);
            for local in [(0.0, 0.0), (0.125, -0.25), (120.0, 90.0), (-900.0, 2048.0)] {
                assert_point(coordinates.local(coordinates.global(local.into())), local.into());
            }
        }
    }

    #[test]
    fn retained_seat_origin_does_not_rescale_drag_distance() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let coordinates = SurfaceCoordinates::new((147.25, -132.5).into(), scale);
            let origin = coordinates.seat_origin(coordinates.global((80.0, 70.0).into()));
            for local in [(120.0, 90.0), (210.0, 160.0), (-35.0, -45.0), (3000.0, 10.0)] {
                let global = coordinates.global(local.into());
                let smithay_event = global - origin;
                assert_point(coordinates.local(smithay_event + origin), local.into());
                // Data-device drags bypass the target, but subtract the fresh
                // hit's origin; their wire coordinates must stay correct too.
                assert_point(global - coordinates.seat_origin(global), local.into());
            }
        }
    }

    #[test]
    fn invalid_scales_are_an_identity_mapping() {
        for scale in [f64::NAN, f64::INFINITY, -2.0, 0.0, 0.01] {
            let coordinates = SurfaceCoordinates::new((10.0, 20.0).into(), scale);
            assert_point(coordinates.local((40.0, 60.0).into()), (30.0, 40.0).into());
        }
    }

    #[test]
    fn subsurface_offsets_are_scaled_from_the_root_not_the_output_origin() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let anchor: Position = (-137.5, 91.25).into();
            let offset: Position = (40.0, -30.0).into();
            let coordinates = SurfaceCoordinates::from_tree(anchor, anchor + offset, scale);
            let physical_origin: Position = (anchor.x + offset.x * scale, anchor.y + offset.y * scale).into();
            assert_point(coordinates.global((0.0, 0.0).into()), physical_origin);
            assert_point(coordinates.local(physical_origin), (0.0, 0.0).into());
            let delta: Position = (60.0 * scale, 25.0 * scale).into();
            assert_point(coordinates.local(physical_origin + delta), (60.0, 25.0).into());
        }
    }
}
