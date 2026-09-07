//! Shared, analytically integrated gesture physics. All tuning lives here.
pub const PROJECTION_SECONDS: f64 = 0.18;
pub const MAX_VELOCITY: f64 = 8.0;
pub const SPRING_OMEGA: f64 = 22.0;
pub const POSITION_EPSILON: f64 = 0.0001;
pub const VELOCITY_EPSILON: f64 = 0.003;
pub const EDGE_LIMIT: f64 = 0.18;
pub const COMMIT_MIDPOINT: f64 = 0.5;
pub fn projected(position: f64, velocity: f64) -> f64 {
    position + velocity.clamp(-MAX_VELOCITY, MAX_VELOCITY) * PROJECTION_SECONDS
}
/// A reversing flick can return home, but cannot skip across home to a
/// neighbor that the fingers have not yet exposed. Crossing home while held
/// changes sides immediately and makes the opposite neighbor eligible.
pub fn settle_target(position: f64, velocity: f64, minimum: f64, maximum: f64) -> f64 {
    let projected = projected(position, velocity);
    if maximum > 0.0 && position > 0.0 && projected > COMMIT_MIDPOINT {
        1.0
    } else if minimum < 0.0 && position < 0.0 && projected < -COMMIT_MIDPOINT {
        -1.0
    } else {
        0.0
    }
}
/// Position and derivative; the derivative maps finger velocity to visual
/// velocity so the spring starts with precisely the motion already on screen.
pub fn resisted(position: f64, minimum: f64, maximum: f64) -> (f64, f64) {
    let edge = position.clamp(minimum, maximum);
    let excess = position - edge;
    if excess == 0.0 {
        return (position, 1.0);
    }
    let denominator = EDGE_LIMIT + excess.abs();
    (
        edge + excess.signum() * EDGE_LIMIT * excess.abs() / denominator,
        (EDGE_LIMIT / denominator).powi(2),
    )
}
/// Inverse for catching a settling scene with a fresh gesture. Avoid a visual
/// jump when the finger first returns to a rubber-banded edge.
pub fn unresisted(position: f64, minimum: f64, maximum: f64) -> f64 {
    let edge = position.clamp(minimum, maximum);
    let excess = position - edge;
    edge + excess.signum() * EDGE_LIMIT * excess.abs() / (EDGE_LIMIT - excess.abs()).max(1e-6)
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub position: f64,
    pub velocity: f64,
    pub target: f64,
}
impl Spring {
    pub fn new(position: f64, velocity: f64, target: f64) -> Self {
        Self {
            position: finite_bound(position, 16.0),
            velocity: finite_bound(velocity, MAX_VELOCITY),
            target: finite_bound(target, 1.0),
        }
    }
    /// Exact critical-damping solution, stable even after a long frame.
    pub fn advance(&mut self, dt: f64) -> bool {
        self.position = finite_bound(self.position, 16.0);
        self.velocity = finite_bound(self.velocity, 1024.0);
        self.target = finite_bound(self.target, 1.0);
        if !dt.is_finite() || dt < 0.0 {
            return false;
        }
        if dt > 2.0 {
            self.position = self.target;
            self.velocity = 0.0;
            return true;
        }
        let offset = self.position - self.target;
        let c = self.velocity + SPRING_OMEGA * offset;
        let decay = (-SPRING_OMEGA * dt).exp();
        self.position = self.target + (offset + c * dt) * decay;
        self.velocity = (self.velocity - SPRING_OMEGA * c * dt) * decay;
        let settled = (self.position - self.target).abs() < POSITION_EPSILON
            && self.velocity.abs() < VELOCITY_EPSILON;
        if settled {
            self.position = self.target;
            self.velocity = 0.0;
        }
        settled
    }
}
fn finite_bound(value: f64, limit: f64) -> f64 {
    if value.is_finite() {
        value.clamp(-limit, limit)
    } else {
        0.0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_handles_slow_flick_and_reverse_releases() {
        assert!(projected(0.4, 0.0) < 0.5);
        assert!(projected(0.6, 0.0) > 0.5);
        assert!(projected(0.2, 2.0) > 0.5);
        assert!(projected(0.7, -2.0) < 0.5);
        assert_eq!(settle_target(0.7, -8.0, -1.0, 1.0), 0.0);
        assert_eq!(settle_target(-0.2, -2.0, -1.0, 1.0), -1.0);
    }
    #[test]
    fn rubber_band_is_monotonic_bounded_and_differentiable() {
        let mut previous = 0.0;
        for p in [0.1, 0.2, 0.4, 0.8, 1.6, 1000.0] {
            let (x, derivative) = resisted(p, -1.0, 0.0);
            assert!(x > previous && x < EDGE_LIMIT);
            assert!(derivative > 0.0 && derivative < 1.0);
            let numeric = (resisted(p + 1e-6, -1.0, 0.0).0 - x) / 1e-6;
            assert!((numeric - derivative).abs() < 1e-5);
            previous = x;
        }
    }
    #[test]
    fn handoff_preserves_motion_and_settles_at_all_refresh_rates() {
        for hz in [30, 60, 120, 144, 240, 360] {
            for target in [-1.0, 0.0, 1.0] {
                for velocity in [-4.0, 0.0, 4.0] {
                    let mut s = Spring::new(0.37, velocity, target);
                    assert_eq!((s.position, s.velocity), (0.37, velocity));
                    assert!(!s.advance(0.0));
                    for _ in 0..hz * 2 {
                        if s.advance(1.0 / hz as f64) {
                            break;
                        }
                    }
                    assert_eq!((s.position, s.velocity), (target, 0.0));
                }
            }
        }
    }
    #[test]
    fn exact_solution_is_partition_independent_and_handles_bad_time() {
        let mut a = Spring::new(0.3, 2.0, 1.0);
        let mut b = a;
        a.advance(0.08);
        for _ in 0..8 {
            b.advance(0.01);
        }
        assert!((a.position - b.position).abs() < 1e-12);
        assert!((a.velocity - b.velocity).abs() < 1e-12);
        for dt in [f64::NAN, f64::INFINITY, -1.0, 1e100] {
            a.advance(dt);
            assert!(a.position.is_finite() && a.velocity.is_finite());
        }
        assert_eq!((a.position, a.velocity), (1.0, 0.0));
    }
    #[test]
    fn malformed_state_cannot_poison_the_renderer() {
        for value in [f64::NAN, f64::INFINITY, -f64::INFINITY, f64::MAX, -f64::MAX] {
            let mut spring = Spring {
                position: value,
                velocity: value,
                target: value,
            };
            for dt in [0.0, 0.008, 0.016, f64::MAX] {
                spring.advance(dt);
                assert!(
                    spring.position.is_finite()
                        && spring.velocity.is_finite()
                        && spring.target.is_finite()
                );
            }
        }
    }
    #[test]
    fn catching_elastic_edges_preserves_the_presented_position() {
        for raw in [-10.0, -1.1, -0.5, 0.0, 0.8, 1.1, 10.0] {
            let visual = resisted(raw, -1.0, 1.0).0;
            assert!((unresisted(visual, -1.0, 1.0) - raw).abs() < 1e-9);
        }
    }
}
