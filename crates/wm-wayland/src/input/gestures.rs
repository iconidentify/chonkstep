//! Seat ownership around the allocation-free desktop swipe recognizer.

use smithay::input::pointer::GestureSwipeEndEvent;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use super::with_input;
use crate::state::Compositor;

fn available(state: &Compositor) -> bool {
    !state.wm.backend().locked
        && state.layer_shell.exclusive_focus.is_none()
        && !state.focus_grab.is_active()
        && state.wm.backend().pointer_grab.is_none()
        && state.shell.desktop_gesture_available(&state.wm)
}

pub(super) fn begin(state: &mut Compositor, fingers: u32, time: u32) -> bool {
    // A replacement begin also retires a lost client end before choosing the
    // next owner. No client gets an update for a stream reserved by the shell.
    cancel_at(state, time);
    let config = state.shell.desktop_gesture_config();
    let available = available(state);
    with_input(&state.seat, |input| {
        input.swipe.begin(fingers, config, available)
    })
}

pub(super) fn update(state: &mut Compositor, delta: Point<f64, Logical>) -> bool {
    let available = available(state);
    with_input(&state.seat, |input| {
        input.swipe.update(delta.x, delta.y, available)
    })
}

pub(super) fn end(state: &mut Compositor, cancelled: bool) -> bool {
    let available = available(state);
    let (consumed, action) = with_input(&state.seat, |input| input.swipe.end(cancelled, available));
    if let Some(action) = action {
        state.shell.on_desktop_gesture(&mut state.wm, action);
    }
    consumed
}

pub(super) fn cancel(state: &mut Compositor) {
    cancel_at(state, state.start_time.elapsed().as_millis() as u32);
}

fn cancel_at(state: &mut Compositor, time: u32) {
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
