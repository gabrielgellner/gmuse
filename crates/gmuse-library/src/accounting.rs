//! **What the fold costs in bytes, measured rather than guessed** — #255.
//!
//! A 7,271-row library and an 87 MB log, and nothing in the design says what folding
//! them should cost: the §"Measurement" targets are all about the audio path — CPU in
//! the callback, resampler throughput — and the librarian's footprint has never had a
//! number attached to it.
//!
//! What it turned out to be, measured against the real library: **121.8 MB live**, of
//! which **83.5 MB is the parsed records** and 38.3 MB the projection and the librarian
//! built from them. The records are held because `Projected::advance` rebuilds from all
//! of them on every fold rather than advancing — see #256.
//!
//! ## Why an allocator and not a profiler
//!
//! `size_of` times a count is a lower bound and misses the interesting half: every
//! `String` in a label, every `BTreeMap` node, every `Vec` grown past its contents. A
//! heap profiler answers properly and costs a dependency and a custom allocator anyway.
//!
//! So this is the allocator, and the pattern is `gmuse-engine`'s `rt_guard`: a
//! `GlobalAlloc` wrapper that accounts instead of forbidding. That one proves the render
//! step allocates nothing; this one says how much the fold allocates. Same mechanism,
//! opposite question.
//!
//! **`#[cfg(test)]`, so it exists in the test binary and nowhere else.** A counter on
//! every allocation is two atomics of overhead that a release build should not pay to
//! answer a question nobody is asking at runtime.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bytes currently held, and the high-water mark since the last reset.
///
/// **Relaxed ordering throughout.** These are statistics, not synchronisation: a
/// reader wants the right order of magnitude and no reader makes a decision that
/// another thread's allocation could invalidate. `SeqCst` here would put a fence on
/// every allocation in the test binary to make a number marginally less approximate.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

pub struct Accounting;

unsafe impl GlobalAlloc for Accounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // **Both halves, or a grown `Vec` reads as a leak.** A `realloc` that doubles a
        // buffer frees the old one, and accounting only the new size would make every
        // growth look like it kept both.
        let live = LIVE.load(Ordering::Relaxed) + new_size - layout.size().min(new_size);
        PEAK.fetch_max(live, Ordering::Relaxed);
        if new_size >= layout.size() {
            LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
}

/// Bytes held right now.
pub fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// The high-water mark, which is the number a peak-during-fold question wants.
pub fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// Start the high-water mark again from where things stand.
pub fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Bytes, for a person reading a table.
pub fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// What one step cost: what it left behind, and what it needed on the way.
///
/// **Both numbers, because they answer different questions.** A fold that ends holding
/// 300 MB after peaking at 500 is a different problem from one that ends at 300 having
/// never exceeded it — the first says the transient is worth attention and the second
/// says the structure is.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, usize, usize) {
    let before = live();
    reset_peak();
    let out = f();
    (
        out,
        live().saturating_sub(before),
        peak().saturating_sub(before),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The allocator counts, and the count comes back down** — #255.
    ///
    /// Asserted on a known allocation rather than on a fold, because a number that is
    /// merely plausible is what this whole file exists to replace. A megabyte of `u8`
    /// is a megabyte, and a test that could not see it would make every figure below
    /// meaningless.
    #[test]
    fn a_known_allocation_is_accounted_for() {
        let (_, kept, high) = measure(|| {
            let v: Vec<u8> = vec![7; 1024 * 1024];
            // Returned, so it is still live when `measure` reads the total.
            v
        });
        // **A window, not an equality, because the counter is global** — it is a
        // `GlobalAlloc`, so every other test thread in this binary is adding to and
        // subtracting from the same two atomics while this one runs. `measure` reads `live()`
        // before and after, and a neighbouring thread freeing in between makes the difference
        // *smaller*: seen failing at 1,048,552, which is twenty-four bytes short of the
        // megabyte and is a `Vec` somewhere else being dropped.
        //
        // The window is still three orders of magnitude tighter than "more than nothing",
        // which is the claim that would be worthless — an allocator that counted only the
        // `Vec`'s header would fail this, and so would one that counted nothing.
        let slack = 64 * 1024;
        assert!(
            kept >= 1024 * 1024 - slack,
            "a megabyte allocated and {kept} bytes counted"
        );
        assert!(high >= kept, "the peak cannot be below what was kept");

        // **And it is released.** Without this the allocator could be counting `alloc`
        // and ignoring `dealloc`, which would make every "live" figure a running total
        // of everything the test binary had ever touched.
        let (_, kept, _) = measure(|| {
            let v: Vec<u8> = vec![7; 1024 * 1024];
            v.len()
        });
        // The same noise, in the other direction: a neighbouring thread allocating between the
        // two reads makes this larger. The megabyte is the signal either way.
        assert!(
            kept < 1024 * 1024 - slack,
            "a megabyte was dropped and {kept} bytes stayed accounted for"
        );
    }

    /// **A grown `Vec` is not two buffers** — the `realloc` arm.
    ///
    /// Accounting only the new size would make every growth look like it kept the old
    /// one too, and a fold grows a great many vectors.
    #[test]
    fn growing_a_vector_does_not_double_count_it() {
        let (_, kept, _) = measure(|| {
            let mut v: Vec<u8> = Vec::new();
            for _ in 0..20 {
                v.extend(std::iter::repeat_n(0u8, 50_000));
            }
            v
        });
        let held = 20 * 50_000;
        assert!(
            kept < held * 2,
            "{kept} bytes accounted for {held} bytes held — the growth was counted twice"
        );
    }
}
