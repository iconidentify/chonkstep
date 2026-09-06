//! Compositor-owned repeat, separate from native clients' repeat-info timers.
//! A hold belongs either to a configured binding or a currently active modal
//! UI. Revalidate that owner before emitting, and never replay an unbounded
//! backlog after a delayed dispatch.

use std::time::{Duration, Instant};

use smithay::input::keyboard::{keysyms, Keycode};
use smithay::input::Seat;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat;
use wm_core::{KeyCombo, Modifiers};

use super::super::{with_input, WmEvent};
use crate::state::Compositor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepeatOwner {
    Binding,
    Modal,
}

#[derive(Clone, Copy)]
pub(crate) struct HeldPress {
    pub keycode: Keycode,
    pub combo: KeyCombo,
}

#[derive(Clone, Copy)]
pub(crate) struct RepeatingKey {
    pub owner: RepeatOwner,
    pub keycode: Keycode,
    pub combo: KeyCombo,
    pub next: Instant,
    pub interval: Duration,
    /// Test-door observation independent of external child startup time.
    pub emitted: u64,
}

impl RepeatingKey {
    pub(crate) fn new(
        owner: RepeatOwner,
        held: HeldPress,
        rate: u32,
        delay: Duration,
    ) -> Option<Self> {
        (rate != 0).then(|| Self {
            owner,
            keycode: held.keycode,
            combo: held.combo,
            next: Instant::now() + delay,
            interval: Duration::from_secs_f64(1.0 / f64::from(rate)),
            emitted: 0,
        })
    }
}

/// Repeat navigation, never commit/cancel or the Super+Up opener itself.
/// Tab may carry Alt/Shift for the switcher; neither flag is a new action.
fn modal_navigation(combo: KeyCombo) -> bool {
    let allowed = Modifiers::ALT | Modifiers::SHIFT;
    if !(combo.modifiers & !allowed).is_empty() {
        return false;
    }
    matches!(
        combo.keysym,
        keysyms::KEY_Tab
            | keysyms::KEY_ISO_Left_Tab
            | keysyms::KEY_Left
            | keysyms::KEY_Right
            | keysyms::KEY_Up
            | keysyms::KEY_Down
            | keysyms::KEY_Home
            | keysyms::KEY_End
            | keysyms::KEY_Page_Up
            | keysyms::KEY_Page_Down
    )
}

pub(crate) fn owner_for_press(
    modal: bool,
    combo: KeyCombo,
    configured: bool,
) -> Option<RepeatOwner> {
    if modal {
        modal_navigation(combo).then_some(RepeatOwner::Modal)
    } else {
        configured.then_some(RepeatOwner::Binding)
    }
}

/// The first Alt+Tab press opens the modal only after input dispatch. Arm that
/// still-held navigation key when its owner becomes active, not by marking a
/// user's ordinary binding as repeatable or guessing from a command string.
pub(crate) fn modal_changed(comp: &Compositor) {
    let backend = comp.wm.backend();
    let owns = super::modal_owns_keyboard(comp);
    with_input(&comp.seat, |input| {
        input.repeating = if owns {
            input
                .last_pressed
                .filter(|held| modal_navigation(held.combo))
                .and_then(|held| {
                    RepeatingKey::new(
                        RepeatOwner::Modal,
                        held,
                        backend.repeat_rate,
                        backend.repeat_delay,
                    )
                })
        } else {
            None
        };
    });
}

pub(crate) fn tick_repeating_binding(comp: &mut Compositor) {
    let seat = comp.seat.clone();
    let Some(held) = with_input(&seat, |input| input.repeating) else {
        return;
    };
    let backend = comp.wm.backend();
    let owner_valid = match held.owner {
        RepeatOwner::Binding => {
            backend.repeating_combos.contains(&held.combo)
                && backend.grabbed_combos.contains(&held.combo)
                && (!backend.locked || backend.locked_combos.contains(&held.combo))
                && !super::modal_owns_keyboard(comp)
        }
        RepeatOwner::Modal => super::modal_owns_keyboard(comp),
    };
    if !owner_valid
        || backend.repeat_rate == 0
        || seat.keyboard_shortcuts_inhibited()
        || backend
            .xwayland_keyboard_grab
            .as_ref()
            .is_some_and(Resource::is_alive)
    {
        with_input(&seat, |input| input.repeating = None);
        return;
    }
    let now = Instant::now();
    let due = with_input(&seat, |input| {
        let repeat = input.repeating.as_mut()?;
        let mut count = 0u8;
        for _ in 0..4 {
            if repeat.next > now {
                break;
            }
            count += 1;
            repeat.next += repeat.interval;
        }
        if repeat.next <= now {
            repeat.next = now + repeat.interval;
        }
        repeat.emitted = repeat.emitted.saturating_add(u64::from(count));
        Some((repeat.combo, count))
    });
    if let Some((combo, count)) = due {
        for _ in 0..count {
            comp.wm.backend_mut().queue(WmEvent::KeyPress(combo));
        }
    }
}

pub(super) fn reconfigure(
    seat: &Seat<Compositor>,
    rate: u32,
    delay: Duration,
    keymap_changed: bool,
) {
    with_input(seat, |input| {
        if keymap_changed {
            input.last_pressed = None;
        }
        if rate == 0 || keymap_changed {
            input.repeating = None;
        } else if let Some(repeat) = input.repeating.as_mut() {
            repeat.interval = Duration::from_secs_f64(1.0 / f64::from(rate));
            repeat.next = Instant::now() + delay;
        }
    });
}

/// Diagnostic state for one live hold. This opt-in test-door query performs no
/// work in an ordinary production session.
pub(crate) fn repeating_binding_status(comp: &Compositor) -> Option<(u64, Duration)> {
    with_input(&comp.seat, |input| {
        input
            .repeating
            .map(|repeat| (repeat.emitted, repeat.interval))
    })
}

/// The existing event loop sleeps until this deadline; no extra repeat thread.
pub(crate) fn repeating_binding_deadline(comp: &Compositor) -> Option<Instant> {
    with_input(&comp.seat, |input| {
        input.repeating.map(|repeat| repeat.next)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_navigation_and_configured_bindings_have_distinct_owners() {
        let tab = KeyCombo {
            keysym: keysyms::KEY_Tab,
            modifiers: Modifiers::ALT,
        };
        let opener = KeyCombo {
            keysym: keysyms::KEY_Up,
            modifiers: Modifiers::SUPER,
        };
        assert_eq!(owner_for_press(false, tab, false), None);
        assert_eq!(
            owner_for_press(false, tab, true),
            Some(RepeatOwner::Binding)
        );
        assert_eq!(owner_for_press(true, tab, false), Some(RepeatOwner::Modal));
        assert_eq!(owner_for_press(true, opener, true), None);
        for keysym in [keysyms::KEY_Return, keysyms::KEY_Escape, keysyms::KEY_a] {
            let combo = KeyCombo {
                keysym,
                modifiers: Modifiers::empty(),
            };
            assert_eq!(owner_for_press(true, combo, true), None);
        }
    }

    #[test]
    fn zero_rate_cannot_create_a_repeat_of_either_kind() {
        let held = HeldPress {
            keycode: Keycode::new(23),
            combo: KeyCombo {
                keysym: keysyms::KEY_Tab,
                modifiers: Modifiers::ALT,
            },
        };
        for owner in [RepeatOwner::Binding, RepeatOwner::Modal] {
            assert!(RepeatingKey::new(owner, held, 0, Duration::ZERO).is_none());
        }
    }
}
