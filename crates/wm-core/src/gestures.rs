//! Desktop swipe recognition in libinput's logical motion units. No timers,
//! event buffers or allocations: only the net displacement survives an update.

/// Native touchpad settings, independent of two-finger scrolling and output scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GestureConfig {
    pub enabled: bool,
    /// Zero accepts both three and four fingers; otherwise three or four.
    pub fingers: u32,
    /// Minimum net travel before lifting the fingers commits an action.
    pub distance: f64,
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fingers: 0,
            distance: 80.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopGesture {
    WorkspaceNext,
    WorkspacePrevious,
    OverviewOpen,
    OverviewClose,
}

/// One seat's swipe ownership. Once claimed, even a cancelled gesture consumes
/// its remaining events, so clients never receive half a desktop gesture.
#[derive(Default, Debug)]
pub enum SwipeTracker {
    #[default]
    Idle,
    Client,
    Suppressed,
    Desktop {
        x: f64,
        y: f64,
        horizontal: Option<bool>,
        distance: f64,
    },
}

impl SwipeTracker {
    /// Returns whether the compositor owns this stream. Settings are latched
    /// until end, so reloading configuration cannot change its recipient.
    pub fn begin(&mut self, fingers: u32, config: GestureConfig, available: bool) -> bool {
        let claimed = config.enabled
            && matches!(fingers, 3 | 4)
            && (config.fingers == 0 || config.fingers == fingers);
        *self = if !claimed {
            Self::Client
        } else if !available {
            Self::Suppressed
        } else {
            Self::Desktop {
                x: 0.0,
                y: 0.0,
                horizontal: None,
                distance: config.distance,
            }
        };
        claimed
    }

    pub fn update(&mut self, dx: f64, dy: f64, available: bool) -> bool {
        if let Self::Desktop {
            x, y, horizontal, ..
        } = self
        {
            if !available || !dx.is_finite() || !dy.is_finite() {
                *self = Self::Suppressed;
                return true;
            }
            *x += dx;
            *y += dy;
            if !x.is_finite() || !y.is_finite() {
                *self = Self::Suppressed;
                return true;
            }
            // Ignore initial jitter and ambiguous diagonals. Once an axis is
            // chosen, a curved stroke cannot turn into a different action.
            if horizontal.is_none() && x.abs().max(y.abs()) >= 12.0 {
                if x.abs() > y.abs() * 1.25 {
                    *horizontal = Some(true);
                } else if y.abs() > x.abs() * 1.25 {
                    *horizontal = Some(false);
                }
            }
        }
        !matches!(self, Self::Client)
    }

    /// Commit only on a successful lift. Net distance makes reversing back to
    /// the start a cancellation; a long stroke still advances exactly once.
    pub fn end(&mut self, cancelled: bool, available: bool) -> (bool, Option<DesktopGesture>) {
        let stream = std::mem::take(self);
        let consumed = !matches!(stream, Self::Client);
        let action = match stream {
            Self::Desktop {
                x,
                y,
                horizontal: Some(horizontal),
                distance,
            } if !cancelled && available => {
                let travel = if horizontal { x } else { y };
                if travel.abs() < distance {
                    None
                } else {
                    Some(match (horizontal, travel < 0.0) {
                        (true, true) => DesktopGesture::WorkspaceNext,
                        (true, false) => DesktopGesture::WorkspacePrevious,
                        (false, true) => DesktopGesture::OverviewOpen,
                        (false, false) => DesktopGesture::OverviewClose,
                    })
                }
            }
            _ => None,
        };
        (consumed, action)
    }

    /// Returns whether a forwarded client needs a cancellation event.
    pub fn cancel(&mut self) -> bool {
        let client = matches!(self, Self::Client);
        *self = Self::Suppressed;
        client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_directions_and_finger_counts() {
        for fingers in [3, 4] {
            for (dx, dy, action) in [
                (-90.0, 1.0, DesktopGesture::WorkspaceNext),
                (90.0, 1.0, DesktopGesture::WorkspacePrevious),
                (1.0, -90.0, DesktopGesture::OverviewOpen),
                (1.0, 90.0, DesktopGesture::OverviewClose),
            ] {
                let mut swipe = SwipeTracker::default();
                assert!(swipe.begin(fingers, GestureConfig::default(), true));
                assert!(swipe.update(dx, dy, true));
                assert_eq!(swipe.end(false, true), (true, Some(action)));
                assert_eq!(swipe.end(false, true), (true, None));
            }
        }
    }

    #[test]
    fn jitter_diagonals_reversals_and_cancellation_do_nothing() {
        for (updates, cancelled) in [
            (vec![(5.0, 2.0)], false),
            (vec![(100.0, 100.0)], false),
            (vec![(-120.0, 0.0), (110.0, 0.0)], false),
            (vec![(-120.0, 0.0)], true),
            (vec![(f64::NAN, 0.0)], false),
            (vec![(0.0, f64::INFINITY)], false),
        ] {
            let mut swipe = SwipeTracker::default();
            swipe.begin(4, GestureConfig::default(), true);
            for (x, y) in updates {
                swipe.update(x, y, true);
            }
            assert_eq!(swipe.end(cancelled, true), (true, None));
        }
    }

    #[test]
    fn client_streams_and_disabled_gestures_remain_whole() {
        for (fingers, config) in [
            (2, GestureConfig::default()),
            (5, GestureConfig::default()),
            (
                3,
                GestureConfig {
                    enabled: false,
                    ..GestureConfig::default()
                },
            ),
            (
                3,
                GestureConfig {
                    fingers: 4,
                    ..GestureConfig::default()
                },
            ),
        ] {
            let mut swipe = SwipeTracker::default();
            assert!(!swipe.begin(fingers, config, true));
            assert!(!swipe.update(-100.0, 0.0, true));
            assert_eq!(swipe.end(false, true), (false, None));
        }
    }

    #[test]
    fn lost_ownership_never_resumes_or_leaks_a_partial_stream() {
        let mut swipe = SwipeTracker::default();
        swipe.begin(3, GestureConfig::default(), true);
        swipe.update(-120.0, 0.0, false);
        swipe.update(-120.0, 0.0, true);
        assert_eq!(swipe.end(false, true), (true, None));
        swipe.begin(2, GestureConfig::default(), true);
        assert!(swipe.cancel());
        assert!(!swipe.cancel());
        assert!(swipe.update(100.0, 0.0, true));
        assert_eq!(swipe.end(false, true), (true, None));
    }

    #[test]
    fn axis_locks_and_high_rate_motion_stays_bounded() {
        let mut swipe = SwipeTracker::default();
        swipe.begin(4, GestureConfig::default(), true);
        swipe.update(-20.0, 0.0, true);
        for _ in 0..100_000 {
            swipe.update(-0.01, -0.1, true);
        }
        assert_eq!(
            swipe.end(false, true),
            (true, Some(DesktopGesture::WorkspaceNext))
        );
        assert!(std::mem::size_of::<SwipeTracker>() <= 40);
    }
}
