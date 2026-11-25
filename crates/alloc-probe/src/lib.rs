//! A test-only heap-allocation probe for **constraint-driven memory tests**.
//!
//! It is a pass-through [`GlobalAlloc`] that forwards every operation to the System
//! allocator unchanged and, on the side, counts heap operations on the *current
//! thread*. That lets a test assert a hard constraint on a hot path, e.g. "a
//! steady-state audio block allocates nothing" or "this call allocates at most N
//! times", so an allocation regression fails the test instead of silently shipping.
//!
//! # Why a separate crate
//!
//! The player crates are `#![forbid(unsafe_code)]`, and a [`GlobalAlloc`] impl needs
//! `unsafe`. Confining the `unsafe` here lets those crates stay unsafe-free while still
//! *installing* this allocator — declaring a `#[global_allocator]` static is **safe**,
//! only implementing the trait is `unsafe`.
//!
//! # Usage
//!
//! Add it as a dev-dependency, install the allocator in the test build, and wrap the
//! code under test in [`measure`]:
//!
//! ```ignore
//! // in the crate's lib.rs (or an integration test):
//! #[cfg(test)]
//! #[global_allocator]
//! static PROBE: alloc_probe::Counting = alloc_probe::Counting;
//!
//! // in a test:
//! let (result, probe) = alloc_probe::measure(|| hot_path());
//! assert_eq!(probe.allocs, 0, "hot path must not allocate (made {})", probe.allocs);
//! ```
//!
//! ## Caveats
//!
//! - Counts are **per thread**: [`measure`] observes only allocations on the thread
//!   that runs the closure, so spawning work inside it is not counted.
//! - The thread-local counter is `const`-initialized, so reading/bumping it never
//!   allocates (it cannot re-enter the allocator) and does not perturb the count.
//! - `realloc` counts as one allocation with zero recorded bytes (the size delta is
//!   not tracked); `alloc` / `alloc_zeroed` record their requested `bytes`.

#![forbid(unsafe_op_in_unsafe_fn)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static STATE: Cell<Probe> = const { Cell::new(Probe::ZERO) };
}

/// A snapshot of heap activity: the number of allocation operations and the total
/// bytes requested by `alloc` / `alloc_zeroed`. Returned as a delta by [`measure`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    /// Count of `alloc` + `alloc_zeroed` + `realloc` calls.
    pub allocs: u64,
    /// Total bytes requested by `alloc` + `alloc_zeroed` (not the `realloc` delta).
    pub bytes: u64,
}

impl Probe {
    const ZERO: Probe = Probe {
        allocs: 0,
        bytes: 0,
    };
}

/// A pass-through global allocator that counts heap operations per thread. Install it
/// in a test build with `#[global_allocator] static X: Counting = Counting;`.
pub struct Counting;

// SAFETY: every method forwards to `System` unchanged (the allocator contract is
// System's); the only added work is bumping a `const`-initialized thread-local `Cell`,
// which never allocates and so cannot re-enter the allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump(layout.size() as u64);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump(layout.size() as u64);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump(0);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn bump(bytes: u64) {
    STATE.with(|s| {
        let mut p = s.get();
        p.allocs = p.allocs.wrapping_add(1);
        p.bytes = p.bytes.wrapping_add(bytes);
        s.set(p);
    });
}

/// Run `f` and return its result plus the heap operations it performed **on this
/// thread** (allocation count and bytes requested).
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, Probe) {
    let before = STATE.with(Cell::get);
    let r = f();
    let after = STATE.with(Cell::get);
    (
        r,
        Probe {
            allocs: after.allocs.wrapping_sub(before.allocs),
            bytes: after.bytes.wrapping_sub(before.bytes),
        },
    )
}

/// The number of heap allocations `f` performs on this thread (shorthand for
/// `measure(f).1.allocs` when the bytes are not needed).
pub fn count_allocs<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let (r, probe) = measure(f);
    (r, probe.allocs)
}

#[cfg(test)]
#[global_allocator]
static SELF_PROBE: Counting = Counting;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measure_counts_a_heap_allocation_and_its_bytes() {
        let (v, probe) = measure(|| Vec::<u8>::with_capacity(64));
        assert!(probe.allocs >= 1, "a heap Vec allocates at least once");
        assert!(
            probe.bytes >= 64,
            "and records at least the requested bytes"
        );
        drop(v);
    }

    #[test]
    fn measure_reports_zero_for_a_non_allocating_closure() {
        // Arithmetic on the stack must not touch the heap.
        let (sum, probe) = measure(|| (0u64..1000).sum::<u64>());
        assert_eq!(sum, 499_500);
        assert_eq!(probe.allocs, 0, "a stack-only closure allocates nothing");
        assert_eq!(probe.bytes, 0);
    }

    #[test]
    fn a_reused_buffer_within_capacity_does_not_reallocate() {
        let mut buf: Vec<u8> = Vec::with_capacity(128);
        // Warm the capacity, then clear+refill WITHIN capacity: no realloc, so zero ops.
        buf.extend(std::iter::repeat_n(0u8, 100));
        let (_, probe) = measure(|| {
            buf.clear();
            buf.extend(std::iter::repeat_n(7u8, 100));
        });
        assert_eq!(
            probe.allocs, 0,
            "clear + refill within capacity must not allocate (got {})",
            probe.allocs
        );
    }
}
