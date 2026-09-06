//! Per-test-thread allocation accounting; disabled outside an explicit scope.
//! Records allocator requests, not RSS, live memory, latency or CPU performance.
//!
//! Depend on this crate only from `[dev-dependencies]`, install
//! [`AllocationCounter`] as the test executable's `#[global_allocator]`, and
//! use [`measure`] around the exact operation under test. Fixture setup belongs
//! outside that scope. This crate never installs or changes an allocator itself.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

fn record(bytes: usize) {
    if ACTIVE.try_with(Cell::get).unwrap_or(false) {
        let _ = CALLS.try_with(|value| value.set(value.get() + 1));
        let _ = BYTES.try_with(|value| value.set(value.get() + bytes));
    }
}

/// System allocator with allocation-free, opt-in per-thread request counters.
pub struct AllocationCounter;

// SAFETY: Every allocation and deallocation forwards the unchanged pointer,
// layout and size to System. The const-initialized counters never allocate.
unsafe impl GlobalAlloc for AllocationCounter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: The caller supplies the GlobalAlloc contract unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: The caller supplies the GlobalAlloc contract unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        // SAFETY: All arguments retain the caller's GlobalAlloc guarantees.
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: This allocation came from System with the supplied layout.
        unsafe { System.dealloc(pointer, layout) }
    }
}

struct Scope;
impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.set(false);
    }
}

/// Allocator requests inside a single scope, not live or resident memory.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AllocationStats {
    /// Calls to allocate, allocate-zeroed, or reallocate.
    pub calls: usize,
    /// Sum of requested allocation sizes, including full reallocation sizes.
    pub requested_bytes: usize,
}

/// Count this thread's allocator requests while running `operation`.
///
/// Requires [`AllocationCounter`] as the caller's global allocator. Counters
/// stop even if the closure panics. Other test threads are independent.
///
/// # Panics
///
/// Panics if scopes nest on the same thread, since inner resets would silently
/// invalidate the outer measurement. The closure's panic propagates normally.
pub fn measure<T>(operation: impl FnOnce() -> T) -> (T, AllocationStats) {
    assert!(!ACTIVE.get(), "allocation measurement scopes cannot nest");
    CALLS.set(0);
    BYTES.set(0);
    ACTIVE.set(true);
    let scope = Scope;
    let result = operation();
    drop(scope);
    (
        result,
        AllocationStats {
            calls: CALLS.get(),
            requested_bytes: BYTES.get(),
        },
    )
}

#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: AllocationCounter = AllocationCounter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_allocation_is_counted_without_counting_its_fixture() {
        let fixture = vec![0u8; 4096];
        let (copy, stats) = measure(|| std::hint::black_box(fixture.clone()));
        assert_eq!(copy, fixture);
        assert_eq!(
            stats,
            AllocationStats {
                calls: 1,
                requested_bytes: 4096
            }
        );
        assert_eq!(measure(|| ()).1, AllocationStats::default());
    }

    #[test]
    fn panic_stops_the_scope_and_later_measurements_start_clean() {
        assert!(std::panic::catch_unwind(|| measure(|| panic!("test closure"))).is_err());
        assert!(!ACTIVE.get());
        assert_eq!(measure(|| ()).1, AllocationStats::default());
    }

    #[test]
    fn concurrent_thread_allocations_do_not_pollute_the_scope() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let other = barrier.clone();
        let worker = std::thread::spawn(move || {
            other.wait();
            let _allocation = std::hint::black_box(vec![0u8; 65536]);
            other.wait();
        });
        let (_, stats) = measure(|| {
            barrier.wait();
            barrier.wait();
        });
        worker.join().unwrap();
        assert_eq!(stats, AllocationStats::default());
    }

    #[test]
    fn nested_scopes_are_rejected_and_leave_no_active_counter() {
        assert!(std::panic::catch_unwind(|| measure(|| measure(|| ()))).is_err());
        assert!(!ACTIVE.get());
    }
}
