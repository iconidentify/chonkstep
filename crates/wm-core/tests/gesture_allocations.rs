use chonk_test_support::{measure, AllocationCounter};
use wm_core::{GestureConfig, Point, Rect, Size, SwipeTracker};

#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;

#[test]
fn high_rate_swipes_and_snap_queries_allocate_nothing() {
    let mut swipe = SwipeTracker::default();
    let targets = [Rect::new(Point::new(0, 40), Size::new(1920, 1040)); 32];
    let (action, allocations) = measure(|| {
        swipe.begin(4, GestureConfig::default(), true, 0);
        for time in 1..=100_000 {
            swipe.update(std::hint::black_box(-0.01), 0.0, true, time);
            std::hint::black_box(swipe.motion(time));
            std::hint::black_box(wm_core::snap_position(
                Rect::new(Point::new(3, 43), Size::new(640, 480)),
                std::hint::black_box(&targets),
                8,
            ));
        }
        swipe.end(false, true, 100_000)
    });
    assert!(action.0);
    assert!(action.1.unwrap().progress > 6.0);
    assert_eq!(allocations.calls, 0);
    assert_eq!(allocations.requested_bytes, 0);
}

#[test]
fn timed_motion_projection_resistance_and_settling_allocate_nothing() {
    use wm_core::gesture_physics::{projected, resisted, Spring};
    let mut swipe = SwipeTracker::default();
    let started = std::time::Instant::now();
    let (_, allocations) = measure(|| {
        swipe.begin(3, GestureConfig::default(), true, 0);
        for time in 1..=100_000 {
            swipe.update(
                std::hint::black_box(if time % 400 < 200 { -0.5 } else { 0.5 }),
                0.0,
                true,
                time,
            );
            if let Some(motion) = swipe.motion(time) {
                let (p, derivative) = resisted(motion.progress, 0.0, 1.0);
                let v = motion.velocity * derivative;
                let mut spring = Spring::new(p, v, if projected(p, v) > 0.5 { 1.0 } else { 0.0 });
                spring.advance(std::hint::black_box(1.0 / 240.0));
                std::hint::black_box(spring);
            }
        }
        swipe.end(false, true, 100_000)
    });
    eprintln!(
        "100000 timed gesture/physics updates: {:?}; tracker {} bytes; allocations={}",
        started.elapsed(),
        std::mem::size_of::<SwipeTracker>(),
        allocations.calls
    );
    assert_eq!((allocations.calls, allocations.requested_bytes), (0, 0));
}
