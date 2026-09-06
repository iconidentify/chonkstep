use chonk_test_support::{measure, AllocationCounter};
use wm_core::{DesktopGesture, GestureConfig, Point, Rect, Size, SwipeTracker};

#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;

#[test]
fn high_rate_swipes_and_snap_queries_allocate_nothing() {
    let mut swipe = SwipeTracker::default();
    let targets = [Rect::new(Point::new(0, 40), Size::new(1920, 1040)); 32];
    let (action, allocations) = measure(|| {
        swipe.begin(4, GestureConfig::default(), true);
        for _ in 0..100_000 {
            swipe.update(std::hint::black_box(-0.01), 0.0, true);
            std::hint::black_box(wm_core::snap_position(
                Rect::new(Point::new(3, 43), Size::new(640, 480)),
                std::hint::black_box(&targets),
                8,
            ));
        }
        swipe.end(false, true)
    });
    assert_eq!(action, (true, Some(DesktopGesture::WorkspaceNext)));
    assert_eq!(allocations.calls, 0);
    assert_eq!(allocations.requested_bytes, 0);
}
