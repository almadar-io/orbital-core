//! The one counting allocator. A binary installs `CountingAlloc::thread_only(System)` and reads the current thread's
//! counts (a step's cost, EXEC-COST-1); a pin test installs `CountingAlloc::with_totals(System)` and also reads the
//! process totals (allocations, bytes, live and peak), which take shared atomics on every allocation.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::Cell;
use core::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

/// Wraps an allocator and counts every allocation it makes.
pub struct CountingAlloc<A> {
    inner: A,
    totals: bool,
}

impl<A> CountingAlloc<A> {
    /// This thread's counts only: no shared counter, so no contention between threads.
    pub const fn thread_only(inner: A) -> Self {
        Self {
            inner,
            totals: false,
        }
    }

    /// This thread's counts and the process totals.
    pub const fn with_totals(inner: A) -> Self {
        Self {
            inner,
            totals: true,
        }
    }
}

static INSTALLED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

std::thread_local! {
    static THREAD_COUNTS: Cell<AllocCounts> = const { Cell::new(AllocCounts { allocs: 0, bytes: 0 }) };
    static THREAD_HELD: Cell<ThreadHeld> = const { Cell::new(ThreadHeld { live: 0, peak: 0 }) };
}

/// The bytes this thread has allocated and not freed (what another thread frees of its blocks is not seen), and the
/// highest that has been since the last [`restart_thread_peak`] (P-049: a step's peak, EXEC-COST-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThreadHeld {
    pub live: isize,
    pub peak: isize,
}

pub fn thread_held() -> ThreadHeld {
    THREAD_HELD.try_with(Cell::get).unwrap_or_default()
}

/// Restart this thread's peak mark at `at`, so the next reading is the highest above it.
pub fn restart_thread_peak(at: isize) {
    let _ = THREAD_HELD.try_with(|h| h.set(ThreadHeld { live: h.get().live, peak: at }));
}

/// A span of this thread's work whose peak is read when it closes: the most the thread held above what it held when
/// the span opened. Windows nest: closing one gives the enclosing window back the higher of the two marks (P-049).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeakWindow {
    start: isize,
    outer: isize,
}

impl PeakWindow {
    pub fn open() -> Self {
        let held = thread_held();
        restart_thread_peak(held.live);
        Self { start: held.live, outer: held.peak }
    }

    /// The bytes the window peaked above its start.
    pub fn close(self) -> u64 {
        let peak = thread_held().peak;
        restart_thread_peak(self.outer.max(peak));
        u64::try_from(peak - self.start).unwrap_or(0)
    }
}

/// Whether this binary declared its counting allocator ([`declare_installed`]), so its readings are real.
pub fn installed() -> bool {
    INSTALLED.load(Ordering::SeqCst)
}

fn thread_grow(grow: isize) {
    let _ = THREAD_HELD.try_with(|h| {
        let was = h.get();
        let live = was.live + grow;
        h.set(ThreadHeld { live, peak: was.peak.max(live) });
    });
}

/// Process-wide counts since start: allocations (a realloc counts as one), bytes requested, bytes live and the
/// highest live mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocTotals {
    pub allocs: usize,
    pub bytes: usize,
    pub live: isize,
    pub peak: isize,
}

/// One thread's allocations and bytes requested since it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocCounts {
    pub allocs: usize,
    pub bytes: usize,
}

impl AllocCounts {
    /// What was allocated between `earlier` and `self` on the same thread.
    pub fn since(self, earlier: AllocCounts) -> AllocCounts {
        AllocCounts {
            allocs: self.allocs - earlier.allocs,
            bytes: self.bytes - earlier.bytes,
        }
    }
}

pub fn totals() -> AllocTotals {
    AllocTotals {
        allocs: ALLOCS.load(Ordering::SeqCst),
        bytes: BYTES.load(Ordering::SeqCst),
        live: LIVE.load(Ordering::SeqCst),
        peak: PEAK.load(Ordering::SeqCst),
    }
}

/// Restart the peak mark at `live`, so the next reading is the peak above it.
pub fn reset_peak(live: isize) {
    PEAK.store(live, Ordering::SeqCst);
}

pub fn thread_counts() -> AllocCounts {
    THREAD_COUNTS.try_with(Cell::get).unwrap_or_default()
}

/// Called once by a binary whose `#[global_allocator]` is a `CountingAlloc`, so its readings are real.
pub fn declare_installed() {
    INSTALLED.store(true, Ordering::SeqCst);
}

/// The native host's cost meter (EXEC-COST-1): the process monotonic clock and this thread's counts. `None` unless
/// the binary declared its counting allocator, so a host without one reports no cost rather than zeros.
#[cfg(not(target_arch = "wasm32"))]
pub fn native_meter() -> Option<std::sync::Arc<dyn crate::cost::CostMeter>> {
    INSTALLED
        .load(Ordering::SeqCst)
        .then(|| std::sync::Arc::new(NativeMeter) as std::sync::Arc<dyn crate::cost::CostMeter>)
}

#[cfg(not(target_arch = "wasm32"))]
struct NativeMeter;

#[cfg(not(target_arch = "wasm32"))]
impl crate::cost::CostMeter for NativeMeter {
    fn now_ns(&self) -> u64 {
        crate::time_util::monotonic_ns()
    }
    fn allocations(&self) -> (u64, u64) {
        let counts = thread_counts();
        (counts.allocs as u64, counts.bytes as u64)
    }
    fn held(&self) -> Option<(i64, i64)> {
        let held = thread_held();
        Some((held.live as i64, held.peak as i64))
    }
    fn restart_peak(&self, at: i64) {
        restart_thread_peak(at as isize);
    }
}

#[inline(always)]
fn count(totals: bool, requested: usize, grow: isize) {
    if totals {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(requested, Ordering::Relaxed);
        let live = LIVE.fetch_add(grow, Ordering::Relaxed) + grow;
        PEAK.fetch_max(live, Ordering::Relaxed);
    }
    thread_grow(grow);
    let _ = THREAD_COUNTS.try_with(|c| {
        let was = c.get();
        c.set(AllocCounts {
            allocs: was.allocs + 1,
            bytes: was.bytes + requested,
        });
    });
}

// SAFETY: every call forwards to the wrapped allocator with the caller's own arguments; counting touches only atomics
// and const-initialized thread-locals, which never allocate.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAlloc<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(self.totals, layout.size(), layout.size() as isize);
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        thread_grow(-(layout.size() as isize));
        if self.totals {
            LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        }
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(
            self.totals,
            new_size,
            new_size as isize - layout.size() as isize,
        );
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn alloc_meter_counts_allocations_bytes_live_and_peak_and_a_realloc_as_one() {
        let _serial = SERIAL.lock().unwrap();
        let meter = CountingAlloc::with_totals(System);
        let before = totals();
        let thread_before = thread_counts();
        let layout = Layout::from_size_align(16, 8).unwrap();
        unsafe {
            let ptr = meter.alloc(layout);
            let grown = meter.realloc(ptr, layout, 48);
            meter.dealloc(grown, Layout::from_size_align(48, 8).unwrap());
        }
        let after = totals();
        assert_eq!(
            (after.allocs - before.allocs, after.bytes - before.bytes),
            (2, 64)
        );
        assert_eq!(after.live, before.live, "everything allocated was freed");
        assert!(
            after.peak >= before.live + 48,
            "the peak saw the grown block"
        );
        assert_eq!(
            thread_counts().since(thread_before),
            AllocCounts {
                allocs: 2,
                bytes: 64
            }
        );
    }

    #[test]
    fn a_threads_peak_sees_a_block_freed_inside_the_window() {
        let _serial = SERIAL.lock().unwrap();
        let meter = CountingAlloc::thread_only(System);
        let start = thread_held().live;
        restart_thread_peak(start);
        let layout = Layout::from_size_align(4096, 8).unwrap();
        unsafe { meter.dealloc(meter.alloc(layout), layout) };
        let held = thread_held();
        assert_eq!(held.live, start, "what was allocated was freed");
        assert!(held.peak >= start + 4096, "the peak saw the block: {held:?} from {start}");
    }

    #[test]
    fn a_peak_window_reports_a_block_freed_inside_it_and_an_inner_window_keeps_the_outer_peak() {
        let _serial = SERIAL.lock().unwrap();
        let meter = CountingAlloc::thread_only(System);
        let outer = PeakWindow::open();
        unsafe {
            let big = Layout::from_size_align(8192, 8).unwrap();
            meter.dealloc(meter.alloc(big), big);
        }
        let inner = PeakWindow::open();
        unsafe {
            let small = Layout::from_size_align(512, 8).unwrap();
            meter.dealloc(meter.alloc(small), small);
        }
        let inner_peak = inner.close();
        assert!((512..8192).contains(&inner_peak), "the inner window saw only its own block: {inner_peak}");
        assert!(outer.close() >= 8192, "the outer window kept the block it saw before the inner one opened");
    }

    #[test]
    fn control_another_threads_block_does_not_move_my_peak() {
        let _serial = SERIAL.lock().unwrap();
        let start = thread_held().live;
        restart_thread_peak(start);
        std::thread::spawn(|| {
            let meter = CountingAlloc::thread_only(System);
            let layout = Layout::from_size_align(1 << 20, 8).unwrap();
            unsafe { meter.dealloc(meter.alloc(layout), layout) };
        })
        .join()
        .unwrap();
        assert!(thread_held().peak < start + (1 << 20));
    }

    #[test]
    fn alloc_meter_thread_counts_see_only_their_own_thread() {
        let _serial = SERIAL.lock().unwrap();
        let mine = thread_counts();
        let other = std::thread::spawn(|| {
            let meter = CountingAlloc::with_totals(System);
            let layout = Layout::from_size_align(32, 8).unwrap();
            unsafe { meter.dealloc(meter.alloc(layout), layout) };
            thread_counts()
        })
        .join()
        .unwrap();
        assert_eq!(
            other,
            AllocCounts {
                allocs: 1,
                bytes: 32
            }
        );
        assert_eq!(
            thread_counts().since(mine),
            AllocCounts::default(),
            "another thread's allocations are not mine"
        );
    }

    #[test]
    fn alloc_meter_thread_only_counts_the_thread_and_leaves_the_totals_alone() {
        let _serial = SERIAL.lock().unwrap();
        let meter = CountingAlloc::thread_only(System);
        let before = totals();
        let mine = thread_counts();
        let layout = Layout::from_size_align(24, 8).unwrap();
        unsafe { meter.dealloc(meter.alloc(layout), layout) };
        assert_eq!(
            thread_counts().since(mine),
            AllocCounts {
                allocs: 1,
                bytes: 24
            }
        );
        assert_eq!(totals(), before);
    }

    #[test]
    fn alloc_meter_reset_peak_restarts_the_mark() {
        let _serial = SERIAL.lock().unwrap();
        let meter = CountingAlloc::with_totals(System);
        let base = totals().live;
        reset_peak(base);
        let layout = Layout::from_size_align(1000, 8).unwrap();
        unsafe { meter.dealloc(meter.alloc(layout), layout) };
        assert_eq!(totals().peak - base, 1000);
    }
}
