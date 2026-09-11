//! Seat ownership around the allocation-free desktop swipe recognizer.

use smithay::input::pointer::GestureSwipeEndEvent;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use super::with_input;
use crate::state::Compositor;

pub(crate) fn inspect(
    state: &Compositor,
) -> (&'static str, (f64, f64), Option<wm_core::SwipeMotion>) {
    let time = state.start_time.elapsed().as_millis() as u32;
    with_input(&state.seat, |input| {
        (
            input.swipe.state_name(),
            input.swipe.displacement(),
            input.swipe.motion(time),
        )
    })
}

pub(crate) fn available(state: &Compositor) -> bool {
    !state.wm.monitors_ref().is_empty()
        && !state.wm.backend().locked
        && crate::session::input_active(&state.graphics)
        && !crate::capture_tool::modal(state.wm.backend())
        && state.layer_shell.exclusive_focus.is_none()
        && !state.focus_grab.is_active()
        && state.wm.backend().pointer_grab.is_none()
        && !state
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.is_grabbed())
        && with_input(&state.seat, |input| input.implicit_grab.is_none())
        && state.shell.desktop_gesture_available(&state.wm)
}

pub(super) fn begin(state: &mut Compositor, fingers: u32, time: u32) -> bool {
    // A replacement begin also retires a lost client end before choosing the
    // next owner. No client gets an update for a stream reserved by the shell.
    let config = state.shell.desktop_gesture_config();
    let available = available(state);
    let desktop = config.enabled
        && matches!(fingers, 3 | 4)
        && (config.fingers == 0 || config.fingers == fingers);
    if available && desktop && crate::gesture_scene::catch(state) {
        cancel_client(state, time);
    } else {
        cancel_at(state, time);
    }
    with_input(&state.seat, |input| {
        input.swipe.begin(fingers, config, available, time)
    })
}

pub(super) fn update(state: &mut Compositor, delta: Point<f64, Logical>, time: u32) -> bool {
    let available = available(state);
    let (consumed, motion, suppressed) = with_input(&state.seat, |input| {
        let consumed = input.swipe.update(delta.x, delta.y, available, time);
        (
            consumed,
            input.swipe.motion(time),
            matches!(input.swipe, wm_core::SwipeTracker::Suppressed),
        )
    });
    if suppressed || !available || !delta.x.is_finite() || !delta.y.is_finite() {
        crate::gesture_scene::cancel(state);
    } else if let Some(motion) = motion {
        crate::gesture_scene::update(state, motion);
    }
    consumed
}

pub(super) fn end(state: &mut Compositor, cancelled: bool, time: u32) -> bool {
    let available = available(state);
    // A cancelled end still supplies the measured velocity to its return
    // spring, but can never select a commit target.
    let (consumed, motion) =
        with_input(&state.seat, |input| input.swipe.end(false, available, time));
    if !available {
        crate::gesture_scene::cancel(state);
    } else if consumed {
        crate::gesture_scene::release(state, cancelled, motion);
    }
    consumed
}

pub(crate) fn cancel(state: &mut Compositor) {
    cancel_at(state, state.start_time.elapsed().as_millis() as u32);
}

fn cancel_at(state: &mut Compositor, time: u32) {
    crate::gesture_scene::cancel(state);
    cancel_client(state, time);
}
fn cancel_client(state: &mut Compositor, time: u32) {
    if with_input(&state.seat, |input| input.swipe.cancel()) {
        if let Some(pointer) = state.seat.get_pointer() {
            pointer.gesture_swipe_end(
                state,
                &GestureSwipeEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    cancelled: true,
                },
            );
        }
    }
}
