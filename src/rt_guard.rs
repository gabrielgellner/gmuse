//! **Allocation forbidden, and provable** — #211.
//!
//! The 2026-09-11 Decision Log entry that retired the three-thread count named
//! the debt it was taking on: *"three threads, not negotiable" was checkable by
//! reading the code, and a property is not.* This is where that comes due for
//! the one rule that cannot be inspected — the render step allocates nothing.
//!
//! ## Why a global allocator, and why only in tests
//!
//! Nothing else can see an allocation. A review can miss a `Vec` grown three
//! calls deep inside something that looks like arithmetic, and the symptom is
//! not a failing test but a click, months later, on somebody else's machine.
//!
//! It is `#[cfg(test)]`, so it exists in the test binary and nowhere else. That
//! matters here more than usual: `just ci` runs `cargo test --release`, so a
//! guard behind `debug_assertions` would compile into nothing and pass for ever
//! without running once.
//!
//! ## It counts rather than panics
//!
//! Panicking inside a global allocator is its own hazard — the unwinder
//! allocates — so a violation bumps a counter and the test reads it. The
//! counter is process-wide but the arming is per-thread, so an allocation on
//! another thread while a guarded block runs is not attributed to it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    /// Whether this thread is currently forbidden from allocating.
    ///
    /// `const`-initialised on purpose: a lazily initialised thread-local
    /// allocates on first touch, which is an allocation inside the allocator.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

static VIOLATIONS: AtomicUsize = AtomicUsize::new(0);

pub struct Guarded;

unsafe impl GlobalAlloc for Guarded {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc_zeroed(layout) }
    }
}

/// Record a violation if this thread is armed.
///
/// `try_with` rather than `with`: a thread-local is unavailable during its own
/// destruction, and panicking there would turn a tidy shutdown into an abort.
fn note() {
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            VIOLATIONS.fetch_add(1, Ordering::Relaxed);
        }
    });
}

/// Run `f` with allocation forbidden on this thread, and say how many times it
/// allocated anyway.
///
/// Nested arming is not supported and not needed: the guarded region is one
/// render step.
pub fn forbidden<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = VIOLATIONS.load(Ordering::Relaxed);
    ARMED.with(|armed| armed.set(true));
    let out = f();
    ARMED.with(|armed| armed.set(false));
    (out, VIOLATIONS.load(Ordering::Relaxed) - before)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard sees an allocation, which is the only way to trust a zero
    /// from it. A guard that never fires is indistinguishable from no guard.
    #[test]
    fn the_guard_notices_an_allocation() {
        let (v, count) = forbidden(|| vec![0u8; 64]);
        assert!(!v.is_empty());
        assert!(count > 0, "the guard did not see an obvious allocation");
    }

    /// And stays quiet otherwise, so a violation means something.
    #[test]
    fn arithmetic_is_not_an_allocation() {
        let (sum, count) = forbidden(|| (0..1000u64).sum::<u64>());
        assert_eq!(sum, 499_500);
        assert_eq!(count, 0);
    }
}
