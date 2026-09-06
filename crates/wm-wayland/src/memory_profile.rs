//! Opt-in allocation diagnostics, never enabled in ordinary builds.
//!
//! The binary must install [`CountingAllocator`] as its global allocator;
//! merely enabling this library feature does not replace a caller's allocator.
//! No stack traces, allocation payloads or pointers are recorded. Relaxed
//! counters are approximate concurrent snapshots of requested Rust allocation
//! sizes, not RSS or allocator usable sizes. C libraries and direct mappings
//! bypass these Rust counters. Profiling builds must not supply timing claims.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
struct Counters {
    live: AtomicU64,
    peak: AtomicU64,
    operations: AtomicU64,
    allocated: AtomicU64,
}

impl Counters {
    const fn new() -> Self {
        Self {
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            operations: AtomicU64::new(0),
            allocated: AtomicU64::new(0),
        }
    }

    fn allocated(&self, bytes: usize) {
        self.operations.fetch_add(1, Relaxed);
        self.allocated.fetch_add(bytes as u64, Relaxed);
        let live = self.live.fetch_add(bytes as u64, Relaxed) + bytes as u64;
        self.peak.fetch_max(live, Relaxed);
    }

    fn freed(&self, bytes: usize) {
        self.live.fetch_sub(bytes as u64, Relaxed);
    }

    fn reallocated(&self, old: usize, new: usize) {
        self.operations.fetch_add(1, Relaxed);
        self.allocated.fetch_add(new as u64, Relaxed);
        if new >= old {
            let added = (new - old) as u64;
            let live = self.live.fetch_add(added, Relaxed) + added;
            self.peak.fetch_max(live, Relaxed);
        } else {
            self.freed(old - new);
        }
    }

    fn snapshot(&self) -> RustAllocations {
        RustAllocations {
            live_bytes: self.live.load(Relaxed),
            peak_bytes: self.peak.load(Relaxed),
            operations: self.operations.load(Relaxed),
            allocated_bytes: self.allocated.load(Relaxed),
        }
    }
}

static COUNTERS: Counters = Counters::new();

/// System allocator with allocation-free, process-wide Rust size accounting.
pub struct CountingAllocator;

// SAFETY: System receives the caller's original pointers, layouts and sizes.
// Bookkeeping touches only statically initialized atomics and cannot recurse
// into allocation, lock a mutex, unwind, or expose allocation contents.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc's caller supplies the valid, nonzero layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            COUNTERS.allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The original valid layout is forwarded; System zeroes it.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            COUNTERS.allocated(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: The caller supplies a live System pointer and its layout.
        unsafe { System.dealloc(pointer, layout) };
        COUNTERS.freed(layout.size());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: The original pointer/layout and valid nonzero replacement
        // size are forwarded unchanged. Null leaves the old allocation live.
        let result = unsafe { System.realloc(pointer, layout, new_size) };
        if !result.is_null() {
            COUNTERS.reallocated(layout.size(), new_size);
        }
        result
    }
}

/// Requested Rust sizes; individual fields are not one atomic transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RustAllocations {
    /// Currently outstanding requested bytes, excluding allocator overhead.
    pub live_bytes: u64,
    /// Highest observed outstanding byte count, not resident-set peak.
    pub peak_bytes: u64,
    /// Successful alloc/alloc_zeroed/realloc calls since process start.
    pub operations: u64,
    /// Sum of successful requested allocation sizes, including realloc sizes.
    pub allocated_bytes: u64,
}

/// Read the counters without allocation, resetting, or allocator modification.
pub fn rust_allocations() -> RustAllocations {
    COUNTERS.snapshot()
}

/// glibc's own accounting, separate from requested Rust sizes and `/proc` PSS.
///
/// These are the reported mallinfo2 fields, not an all-library ownership
/// census: arena/thread-cache/version limitations preclude subtracting Rust
/// live bytes and labelling the result exact C-library allocations.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AllocatorStatistics {
    pub supported: bool,
    pub arena_bytes: usize,
    pub in_use_bytes: usize,
    pub free_bytes: usize,
    pub mapped_bytes: usize,
    pub top_releasable_bytes: usize,
}

pub(crate) fn allocator_statistics() -> AllocatorStatistics {
    #[cfg(target_env = "gnu")]
    {
        // SAFETY: mallinfo2 takes no pointers and returns counters by value.
        // Called from the opt-in door during normal execution, never from a
        // signal handler or concurrently with startup's mallopt policy setup.
        let info = unsafe { libc::mallinfo2() };
        AllocatorStatistics {
            supported: true,
            arena_bytes: info.arena,
            in_use_bytes: info.uordblks,
            free_bytes: info.fordblks,
            mapped_bytes: info.hblkhd,
            top_releasable_bytes: info.keepcost,
        }
    }
    #[cfg(not(target_env = "gnu"))]
    AllocatorStatistics::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_balance_alloc_zeroed_growth_shrink_and_free_without_resetting_peak() {
        let counters = Counters::new();
        counters.allocated(64);
        counters.allocated(128);
        counters.reallocated(64, 256);
        counters.reallocated(128, 32);
        assert_eq!(
            counters.snapshot(),
            RustAllocations {
                live_bytes: 288,
                peak_bytes: 384,
                operations: 4,
                allocated_bytes: 480,
            }
        );
        counters.freed(256);
        counters.freed(32);
        assert_eq!(counters.snapshot().live_bytes, 0);
        assert_eq!(counters.snapshot().peak_bytes, 384);
    }

    #[test]
    fn counting_allocator_preserves_alignment_zeroing_and_reallocation_contents() {
        let allocator = CountingAllocator;
        let layout = Layout::from_size_align(128, 64).unwrap();
        // SAFETY: Nonzero valid layout; each successful pointer is used only
        // within its allocation and released once with the current layout.
        unsafe {
            let pointer = allocator.alloc_zeroed(layout);
            assert!(!pointer.is_null());
            assert_eq!(pointer as usize % 64, 0);
            assert!(std::slice::from_raw_parts(pointer, 128)
                .iter()
                .all(|byte| *byte == 0));
            pointer.write_bytes(0xA5, 128);
            let grown = allocator.realloc(pointer, layout, 512);
            assert!(!grown.is_null());
            assert_eq!(grown as usize % 64, 0);
            assert!(std::slice::from_raw_parts(grown, 128)
                .iter()
                .all(|byte| *byte == 0xA5));
            allocator.dealloc(grown, Layout::from_size_align(512, 64).unwrap());
        }
    }
}
