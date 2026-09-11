//! Preserve Copy/Cut -> switch -> Paste ordering without granting an unfocused
//! client clipboard ownership. Only keyboard commands after a Mac copy wait;
//! client dispatch, GPU work and pointer motion continue normally.
use super::KeyboardFocus;
use crate::state::Compositor;
use smithay::backend::input::KeyState;
use smithay::input::keyboard::Keycode;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const MAX_WAIT: Duration = Duration::from_millis(250);
const MAX_KEYS: usize = 256;

#[derive(Clone, Copy)]
pub(crate) struct Key {
    pub code: Keycode,
    pub state: KeyState,
    pub time: u32,
}

#[derive(Default)]
pub(crate) struct CopyOrder {
    pending: Option<(KeyboardFocus, Instant)>,
    keys: VecDeque<Key>,
}

impl CopyOrder {
    pub fn copied(&mut self, focus: KeyboardFocus) {
        self.pending = Some((focus, Instant::now() + MAX_WAIT));
    }
    pub fn offered(&mut self) {
        self.pending = None;
    }
    pub fn reset(&mut self) {
        self.pending = None;
        self.keys.clear();
    }
    pub fn queued(&self) -> bool {
        !self.keys.is_empty()
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.queued()
            .then(|| self.pending.as_ref().map_or_else(Instant::now, |(_, at)| *at))
    }
    fn expire(&mut self, focus: Option<KeyboardFocus>, now: Instant) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(owner, at)| Some(owner) != focus.as_ref() || now >= *at)
        {
            self.pending = None;
        }
    }
}

/// Releases preceding the next press still reach the source immediately, so a
/// toolkit that copies on key release can publish its offer without a timeout.
pub(crate) fn defer(comp: &mut Compositor, key: Key) -> bool {
    if comp.mac_copy_order.pending.is_none() && !comp.mac_copy_order.queued() {
        return false;
    }
    if comp.wm.backend().locked {
        if comp.mac_copy_order.queued() {
            crate::input::resynchronise_input_after_resume(comp);
        }
        comp.mac_copy_order.reset();
        return false;
    }
    let focus = comp.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus());
    let order = &mut comp.mac_copy_order;
    order.expire(focus, Instant::now());
    if !order.queued() && (order.pending.is_none() || key.state == KeyState::Released) {
        return false;
    }
    if order.keys.len() == MAX_KEYS {
        // A synthetic input flood must not grow storage or strand modifiers.
        // Cancel its pending sequence and balance every already-delivered key.
        tracing::warn!("Mac clipboard input queue exhausted; resetting held input");
        crate::input::resynchronise_input_after_resume(comp);
        return true;
    }
    order.keys.push_back(key);
    true
}

/// Replay one physical transition per dispatch. WM actions and focus changes
/// settle in the ordinary dispatch pass before the following key is forwarded.
pub(crate) fn service(comp: &mut Compositor) {
    if !comp.mac_copy_order.queued() {
        return;
    }
    if comp.wm.backend().locked {
        crate::input::resynchronise_input_after_resume(comp);
        return;
    }
    let focus = comp.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus());
    comp.mac_copy_order.expire(focus, Instant::now());
    if comp.mac_copy_order.pending.is_some() {
        return;
    }
    if let Some(key) = comp.mac_copy_order.keys.pop_front() {
        crate::input::deliver_keyboard_key(comp, key.code, key.state, key.time);
    }
}
