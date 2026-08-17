//! An allocation-counting global allocator, for the `alloc_per_tick_steady_state`
//! gate.
//!
//! `03-conventions.md` bans allocation inside per-tick systems: scratch buffers
//! are preallocated in resources and reused. That rule is unenforceable by
//! review alone — an allocation can appear three call levels down inside a
//! `Vec::push` that looked harmless — so it is measured directly.
//!
//! Counting is process-wide and includes allocations made by any thread, which
//! is what we want: a tick that allocates on a worker thread has still violated
//! the rule.

// A `GlobalAlloc` implementation cannot be written in safe Rust. This is the
// only place in the repo that needs the exemption, and it is measurement code
// that never ships inside the simulation.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static BYTES_ALLOCATED: AtomicU64 = AtomicU64::new(0);

/// Wraps the system allocator and counts calls.
///
/// Install in a benchmark with:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOC: CountingAllocator = CountingAllocator;
/// ```
pub struct CountingAllocator;

// SAFETY: every method forwards directly to the system allocator with the
// caller's layout unchanged. The counters are atomic and touch no allocation
// state, so they cannot re-enter the allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES_ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A realloc that grows a buffer is exactly the failure this gate exists
        // to catch — a per-tick `Vec` that outgrew its capacity.
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES_ALLOCATED.fetch_add(
            new_size.saturating_sub(layout.size()) as u64,
            Ordering::Relaxed,
        );
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Allocation count and total bytes over a measured region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocationReport {
    pub allocations: u64,
    pub bytes: u64,
}

impl AllocationReport {
    /// Whether this region satisfies the zero-allocation rule.
    pub fn is_allocation_free(&self) -> bool {
        self.allocations == 0
    }
}

/// Runs `body` and reports what it allocated.
///
/// Call this only after the system under test has reached steady state. The
/// first few ticks legitimately allocate — chunk activation, archetype
/// reservation, scratch buffers sized on first use — and the gate is about the
/// steady state, not about startup.
pub fn measure<T>(body: impl FnOnce() -> T) -> (T, AllocationReport) {
    let allocations_before = ALLOCATIONS.load(Ordering::Acquire);
    let bytes_before = BYTES_ALLOCATED.load(Ordering::Acquire);

    let value = body();

    let report = AllocationReport {
        allocations: ALLOCATIONS
            .load(Ordering::Acquire)
            .saturating_sub(allocations_before),
        bytes: BYTES_ALLOCATED
            .load(Ordering::Acquire)
            .saturating_sub(bytes_before),
    };

    (value, report)
}
