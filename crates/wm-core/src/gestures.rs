//! Allocation-free desktop gesture ownership and timed motion. Progress is
//! dimensionless: output pixels never enter the recognizer or shared physics.
pub mod physics;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GestureConfig {
    pub enabled: bool,
    /// Zero accepts both three and four fingers; otherwise three or four.
    pub fingers: u32,
    /// Slow-release midpoint in logical touchpad units. Full span is twice this.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwipeAxis {
    Horizontal,
    Vertical,
}
/// Positive progress means left/up. Velocity is progress units per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwipeMotion {
    pub axis: SwipeAxis,
    pub x: f64,
    pub y: f64,
    pub progress: f64,
    pub velocity: f64,
}
const AXIS_DISTANCE: f64 = 12.0;
const AXIS_DOMINANCE: f64 = 1.25;
const SAMPLE_COUNT: usize = 8;
const SAMPLE_INTERVAL_MS: u32 = 12;
const HISTORY_MS: u32 = 100;
const RECENCY_SECONDS: f64 = 0.035;
#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    time: u32,
    x: f64,
    y: f64,
}
#[derive(Debug)]
pub struct SwipeState {
    x: f64,
    y: f64,
    axis: Option<SwipeAxis>,
    span: f64,
    samples: [Sample; SAMPLE_COUNT],
    next: usize,
    count: usize,
}
impl SwipeState {
    fn new(distance: f64, time: u32) -> Self {
        let mut state = Self {
            x: 0.0,
            y: 0.0,
            axis: None,
            span: if distance.is_finite() {
                distance.clamp(24.0, 1000.0) * 2.0
            } else {
                160.0
            },
            samples: [Sample::default(); SAMPLE_COUNT],
            next: 0,
            count: 0,
        };
        state.sample(time);
        state
    }
    fn sample(&mut self, time: u32) {
        if self.count > 0 {
            let last = self.samples[(self.next + SAMPLE_COUNT - 1) % SAMPLE_COUNT];
            let elapsed = time.wrapping_sub(last.time);
            if elapsed > i32::MAX as u32 {
                self.count = 0;
            } else if elapsed < SAMPLE_INTERVAL_MS {
                return;
            }
        }
        self.samples[self.next] = Sample {
            time,
            x: self.x,
            y: self.y,
        };
        self.next = (self.next + 1) % SAMPLE_COUNT;
        self.count = (self.count + 1).min(SAMPLE_COUNT);
    }
    fn motion(&self, time: u32) -> Option<SwipeMotion> {
        let axis = self.axis?;
        let position = |sample: Sample| {
            -match axis {
                SwipeAxis::Horizontal => sample.x,
                SwipeAxis::Vertical => sample.y,
            } / self.span
        };
        let mut recent = Sample {
            time,
            x: self.x,
            y: self.y,
        };
        let mut weighted_motion = 0.0;
        let mut weighted_time = 0.0;
        let mut covered_ms = 0;
        for age in 0..self.count {
            let older = self.samples[(self.next + SAMPLE_COUNT - age - 1) % SAMPLE_COUNT];
            let age_ms = time.wrapping_sub(older.time);
            if age_ms > HISTORY_MS {
                break;
            }
            let dt = recent.time.wrapping_sub(older.time) as f64 / 1000.0;
            if dt > 0.0 {
                let weight = (-(age_ms as f64 / 1000.0) / RECENCY_SECONDS).exp();
                weighted_motion += (position(recent) - position(older)) * weight;
                weighted_time += dt * weight;
                covered_ms = age_ms;
                recent = older;
            }
        }
        Some(SwipeMotion {
            axis,
            x: self.x,
            y: self.y,
            progress: -match axis {
                SwipeAxis::Horizontal => self.x,
                SwipeAxis::Vertical => self.y,
            } / self.span,
            velocity: if weighted_time > 0.0 && covered_ms >= SAMPLE_INTERVAL_MS {
                (weighted_motion / weighted_time)
                    .clamp(-physics::MAX_VELOCITY, physics::MAX_VELOCITY)
            } else {
                0.0
            },
        })
    }
}
/// A claimed stream stays consumed through cancellation. Settings are latched.
#[derive(Default, Debug)]
// The fixed history deliberately stays on the seat: boxing it would add an
// allocation at every begin and indirection at every input sample.
#[allow(clippy::large_enum_variant)]
pub enum SwipeTracker {
    #[default]
    Idle,
    Client,
    Suppressed,
    Desktop(SwipeState),
}
impl SwipeTracker {
    /// Read-only test/debug state; no production logging or owned strings.
    pub fn state_name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Client => "client",
            Self::Suppressed => "suppressed",
            Self::Desktop(_) => "desktop",
        }
    }
    pub fn displacement(&self) -> (f64, f64) {
        match self {
            Self::Desktop(state) => (state.x, state.y),
            _ => (0.0, 0.0),
        }
    }
    pub fn begin(
        &mut self,
        fingers: u32,
        config: GestureConfig,
        available: bool,
        time: u32,
    ) -> bool {
        let claimed = config.enabled
            && matches!(fingers, 3 | 4)
            && (config.fingers == 0 || config.fingers == fingers);
        *self = if !claimed {
            Self::Client
        } else if !available {
            Self::Suppressed
        } else {
            Self::Desktop(SwipeState::new(config.distance, time))
        };
        claimed
    }
    pub fn update(&mut self, dx: f64, dy: f64, available: bool, time: u32) -> bool {
        if let Self::Desktop(state) = self {
            if !available || !dx.is_finite() || !dy.is_finite() {
                *self = Self::Suppressed;
                return true;
            }
            state.x += dx;
            state.y += dy;
            if !state.x.is_finite()
                || !state.y.is_finite()
                || state.x.abs().max(state.y.abs()) > 1e9
            {
                *self = Self::Suppressed;
                return true;
            }
            if state.axis.is_none() && state.x.abs().max(state.y.abs()) >= AXIS_DISTANCE {
                if state.x.abs() > state.y.abs() * AXIS_DOMINANCE {
                    state.axis = Some(SwipeAxis::Horizontal);
                } else if state.y.abs() > state.x.abs() * AXIS_DOMINANCE {
                    state.axis = Some(SwipeAxis::Vertical);
                }
            }
            state.sample(time);
        }
        !matches!(self, Self::Client)
    }
    pub fn motion(&self, time: u32) -> Option<SwipeMotion> {
        match self {
            Self::Desktop(state) => state.motion(time),
            _ => None,
        }
    }
    pub fn end(
        &mut self,
        cancelled: bool,
        available: bool,
        time: u32,
    ) -> (bool, Option<SwipeMotion>) {
        let stream = std::mem::take(self);
        let consumed = !matches!(stream, Self::Client);
        (
            consumed,
            if !cancelled && available {
                stream.motion(time)
            } else {
                None
            },
        )
    }
    /// Whether a forwarded client needs a cancelled end.
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
    fn fingers_axes_jitter_and_reversal() {
        for fingers in [3, 4] {
            let mut s = SwipeTracker::default();
            assert!(s.begin(fingers, GestureConfig::default(), true, 0));
            s.update(-5.0, -2.0, true, 16);
            assert!(s.motion(16).is_none());
            s.update(-35.0, 0.0, true, 32);
            assert_eq!(s.motion(32).unwrap().axis, SwipeAxis::Horizontal);
            assert_eq!(s.motion(32).unwrap().progress, 0.25);
            s.update(20.0, -100.0, true, 48);
            assert_eq!(s.motion(48).unwrap().progress, 0.125);
            s.update(60.0, 0.0, true, 64);
            assert_eq!(s.motion(64).unwrap().progress, -0.25);
            assert!(s.end(true, true, 80).1.is_none());
            assert!(matches!(s, SwipeTracker::Idle));
        }
    }
    #[test]
    fn ambiguous_invalid_and_lost_ownership_stay_consumed() {
        for (dx, dy, available) in [
            (100.0, 100.0, true),
            (f64::NAN, 0.0, true),
            (0.0, f64::INFINITY, true),
            (-100.0, 0.0, false),
        ] {
            let mut s = SwipeTracker::default();
            s.begin(3, GestureConfig::default(), true, 0);
            assert!(s.update(dx, dy, available, 16));
            assert_eq!(s.end(false, true, 32), (true, None));
        }
    }
    #[test]
    fn client_streams_are_whole_and_can_be_cancelled() {
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
            let mut s = SwipeTracker::default();
            assert!(!s.begin(fingers, config, true, 0));
            assert!(!s.update(-100.0, 0.0, true, 16));
            assert_eq!(s.end(false, true, 32), (false, None));
            s.begin(fingers, config, true, 40);
            assert!(s.cancel());
            assert!(!s.cancel());
            assert!(s.update(-100.0, 0.0, true, 48));
        }
    }
    #[test]
    fn velocity_uses_history_and_decays_when_the_hand_stops() {
        let mut s = SwipeTracker::default();
        s.begin(4, GestureConfig::default(), true, 1000);
        for time in (1016..=1096).step_by(16) {
            s.update(-8.0, 0.0, true, time);
        }
        assert!((s.motion(1096).unwrap().velocity - 3.125).abs() < 0.01);
        s.update(0.0, 0.0, true, 1097);
        assert!(s.motion(1097).unwrap().velocity > 2.5);
        for time in (1112..=1160).step_by(16) {
            s.update(12.0, 0.0, true, time);
        }
        assert!(s.motion(1160).unwrap().velocity < -2.0);
        assert_eq!(s.motion(1300).unwrap().velocity, 0.0);
    }
    #[test]
    fn high_rate_history_covers_time_not_just_eight_events() {
        let mut s = SwipeTracker::default();
        s.begin(3, GestureConfig::default(), true, u32::MAX - 40);
        for i in 1..=100 {
            s.update(-0.5, 0.0, true, (u32::MAX - 40).wrapping_add(i));
        }
        assert!((s.motion(59).unwrap().velocity - 3.125).abs() < 0.01);
        assert!(std::mem::size_of::<SwipeTracker>() < 320);
    }
    #[test]
    fn coalesced_same_timestamp_reversal_keeps_the_newest_position() {
        for time in 0..=40 {
            let mut swipe = SwipeTracker::default();
            swipe.begin(3, GestureConfig::default(), true, 0);
            swipe.update(-120.0, 0.0, true, time);
            swipe.update(115.0, 0.0, true, time);
            let motion = swipe.end(false, true, time).1.unwrap();
            assert_eq!(motion.progress, 0.03125);
            assert_eq!(physics::settle_target(motion.progress, motion.velocity, -1.0, 1.0), 0.0,
                "zero-duration samples cannot resurrect the pre-reversal position at {time}ms");
        }
    }
}
