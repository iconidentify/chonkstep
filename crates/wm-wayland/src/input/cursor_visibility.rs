//! Getting the pointer out of the way: hidden while the user types or
//! touches, or after a stretch without pointer input, and shown again by
//! the next visible pointer input.
//!
//! This is a reason of the compositor's own and lives apart from the
//! IPC-owned `cursor_hidden` flag. A screensaver owns that flag, and moving
//! the mouse must never reveal a cursor the screensaver hid; nor may the
//! screensaver's `invisible false` reveal a cursor hidden for typing. The
//! renderer draws no cursor while either says so.
//!
//! Only transitions matter to the scene: every method reports whether the
//! pointer's visibility changed, so a held key or steady typing marks no
//! further damage.

use std::time::{Duration, Instant};

use smithay::input::keyboard::Keysym;
use wm_core::CursorBehaviour;

/// Why the compositor is hiding the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoHide {
    Typing,
    Touch,
    Idle,
}

/// The shortest and longest idle timeout honoured, in seconds. A value from
/// a config file is untrusted: zero, negative and non-finite values mean
/// "never".
const IDLE_SECONDS: std::ops::RangeInclusive<f64> = 1.0..=3600.0;

#[derive(Debug, Default)]
pub(crate) struct CursorVisibility {
    hidden: Option<AutoHide>,
    idle_deadline: Option<Instant>,
}

impl CursorVisibility {
    pub(crate) fn hidden(&self) -> bool {
        self.hidden.is_some()
    }

    /// When the loop must wake to hide an idle pointer, if ever.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.idle_deadline
    }

    /// Hides the pointer for `reason`. Returns whether it was visible.
    pub(crate) fn hide(&mut self, reason: AutoHide) -> bool {
        if self.hidden.is_some() {
            return false;
        }
        self.hidden = Some(reason);
        true
    }

    /// Visible pointer input: shows the pointer again and restarts the idle
    /// clock. `now` is consulted only when a timeout is configured, so the
    /// per-motion cost on an ordinary desk is two field reads.
    pub(crate) fn reveal(&mut self, behaviour: &CursorBehaviour, now: impl FnOnce() -> Instant) -> bool {
        if behaviour.inactive_timeout.is_some() {
            self.idle_deadline = idle_deadline(behaviour, now());
        }
        self.hidden.take().is_some()
    }

    /// Hides the pointer once its idle deadline has passed.
    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        if !self.idle_deadline.is_some_and(|deadline| now >= deadline) {
            return false;
        }
        self.idle_deadline = None;
        self.hide(AutoHide::Idle)
    }

    /// A new configuration: restart the idle clock under it, and release a
    /// hide whose setting was just turned off.
    pub(crate) fn configure(&mut self, behaviour: &CursorBehaviour, now: Instant) -> bool {
        self.idle_deadline = idle_deadline(behaviour, now);
        let still_wanted = match self.hidden {
            Some(AutoHide::Typing) => behaviour.hide_on_key_press == Some(true),
            Some(AutoHide::Touch) => behaviour.hide_on_touch == Some(true),
            Some(AutoHide::Idle) => self.idle_deadline.is_some(),
            None => return false,
        };
        if still_wanted {
            return false;
        }
        self.hidden = None;
        true
    }
}

/// Whether a key press delivered to a client hides the pointer. Modifiers
/// never do: a Super-drag or Alt-drag is about to use the pointer, and a
/// press the compositor consumed as a binding never reaches this question.
pub(crate) fn hides_on_key(behaviour: &CursorBehaviour, keysym: u32) -> bool {
    behaviour.hide_on_key_press == Some(true) && !Keysym::new(keysym).is_modifier_key()
}

/// The moment an idle pointer hides, measured from `now`.
fn idle_deadline(behaviour: &CursorBehaviour, now: Instant) -> Option<Instant> {
    let seconds = behaviour.inactive_timeout.filter(|seconds| seconds.is_finite() && *seconds > 0.0)?;
    Some(now + Duration::from_secs_f64(seconds.clamp(*IDLE_SECONDS.start(), *IDLE_SECONDS.end())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::input::keyboard::keysyms;

    fn behaviour(hide_on_key_press: bool, hide_on_touch: bool, inactive_timeout: Option<f64>) -> CursorBehaviour {
        CursorBehaviour {
            hide_on_key_press: Some(hide_on_key_press),
            hide_on_touch: Some(hide_on_touch),
            inactive_timeout,
        }
    }

    #[test]
    fn typing_hides_once_and_the_next_pointer_input_shows_it() {
        let config = behaviour(true, false, None);
        let mut cursor = CursorVisibility::default();
        assert!(hides_on_key(&config, keysyms::KEY_a));
        assert!(cursor.hide(AutoHide::Typing), "the first key is a transition");
        assert!(!cursor.hide(AutoHide::Typing), "further typing marks no damage");
        assert!(cursor.hidden());
        assert!(cursor.reveal(&config, || panic!("no clock without a timeout")));
        assert!(!cursor.hidden());
        assert!(!cursor.reveal(&config, Instant::now), "motion while visible changes nothing");
    }

    #[test]
    fn modifiers_and_a_disabled_setting_never_hide() {
        let config = behaviour(true, false, None);
        for modifier in [keysyms::KEY_Shift_L, keysyms::KEY_Control_R, keysyms::KEY_Super_L, keysyms::KEY_Alt_L, keysyms::KEY_ISO_Level3_Shift] {
            assert!(!hides_on_key(&config, modifier), "{modifier:#x}");
        }
        assert!(!hides_on_key(&behaviour(false, false, None), keysyms::KEY_a));
        assert!(!hides_on_key(&CursorBehaviour::default(), keysyms::KEY_a));
    }

    #[test]
    fn an_idle_timeout_hides_after_its_deadline_and_input_restarts_it() {
        let config = behaviour(false, false, Some(2.0));
        let start = Instant::now();
        let mut cursor = CursorVisibility::default();
        assert!(!cursor.configure(&config, start));
        assert_eq!(cursor.deadline(), Some(start + Duration::from_secs(2)));
        assert!(!cursor.tick(start + Duration::from_millis(1999)));
        assert!(cursor.tick(start + Duration::from_secs(2)));
        assert!(cursor.hidden());
        assert_eq!(cursor.deadline(), None, "an expired deadline is not re-armed by the loop");
        let moved = start + Duration::from_secs(5);
        assert!(cursor.reveal(&config, || moved));
        assert_eq!(cursor.deadline(), Some(moved + Duration::from_secs(2)));
    }

    #[test]
    fn untrusted_timeouts_are_bounded_or_mean_never() {
        let now = Instant::now();
        for never in [None, Some(0.0), Some(-3.0), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_eq!(idle_deadline(&behaviour(false, false, never), now), None, "{never:?}");
        }
        assert_eq!(idle_deadline(&behaviour(false, false, Some(0.2)), now), Some(now + Duration::from_secs(1)));
        assert_eq!(idle_deadline(&behaviour(false, false, Some(1e9)), now), Some(now + Duration::from_secs(3600)));
    }

    #[test]
    fn turning_a_setting_off_releases_only_the_hide_it_caused() {
        let now = Instant::now();
        let mut cursor = CursorVisibility::default();
        cursor.hide(AutoHide::Typing);
        assert!(!cursor.configure(&behaviour(true, false, None), now), "still wanted");
        assert!(cursor.configure(&behaviour(false, true, None), now), "typing hide released");

        cursor.hide(AutoHide::Touch);
        assert!(!cursor.configure(&behaviour(false, true, None), now));
        assert!(cursor.configure(&behaviour(true, false, None), now));

        cursor.hide(AutoHide::Idle);
        assert!(!cursor.configure(&behaviour(false, false, Some(5.0)), now), "a timeout is still configured");
        assert!(cursor.configure(&behaviour(false, false, None), now));
    }
}
