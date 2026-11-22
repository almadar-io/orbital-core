//! The cost meter port (EXEC-COST-1): a host gives the kernel its monotonic clock and its counting allocator's
//! reading for the current thread, and the kernel reports a step's or an effect's cost only when an ask names it.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::sync::Arc;

/// What a host answers the kernel's cost readings with.
pub trait CostMeter: Send + Sync {
    /// The host's monotonic clock in nanoseconds, on the origin its other timelines use.
    fn now_ns(&self) -> u64;
    /// This thread's allocation count and bytes requested since it started.
    fn allocations(&self) -> (u64, u64);
    /// This thread's bytes held and its peak since the last [`Self::restart_peak`], where the allocator tracks them.
    fn held(&self) -> Option<(i64, i64)> {
        None
    }
    /// Restart this thread's peak mark at `at`.
    fn restart_peak(&self, _at: i64) {}
}

/// One step's or one effect's cost.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepCost {
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub start_ns: u64,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub ns: u64,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub allocs: u64,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub alloc_bytes: u64,
    /// The most the step held above what its thread held when it started (P-049); 0 where the host reads no peak.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts-export", ts(type = "number", optional))]
    pub peak_bytes: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A reading taken when a measured step starts; `finish` reads again and answers the difference.
pub struct CostStart {
    meter: Arc<dyn CostMeter>,
    start_ns: u64,
    allocs: u64,
    bytes: u64,
    /// The thread's bytes held at the start and the peak mark it had before this step restarted it.
    held: Option<(i64, i64)>,
}

impl CostStart {
    pub fn begin(meter: Arc<dyn CostMeter>) -> Self {
        let (allocs, bytes) = meter.allocations();
        let held = meter.held();
        if let Some((live, _)) = held {
            meter.restart_peak(live);
        }
        let start_ns = meter.now_ns();
        Self { meter, start_ns, allocs, bytes, held }
    }

    pub fn finish(self) -> StepCost {
        let end_ns = self.meter.now_ns();
        let (allocs, bytes) = self.meter.allocations();
        let peak_bytes = match (self.held, self.meter.held()) {
            (Some((start_live, outer_peak)), Some((_, peak))) => {
                // An enclosing step keeps the higher mark of the two, so nesting does not hide its own peak.
                self.meter.restart_peak(outer_peak.max(peak));
                u64::try_from(peak - start_live).unwrap_or(0)
            }
            _ => 0,
        };
        StepCost {
            start_ns: self.start_ns,
            ns: end_ns - self.start_ns,
            allocs: allocs - self.allocs,
            alloc_bytes: bytes - self.bytes,
            peak_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scripted {
        clock: AtomicU64,
        allocs: AtomicU64,
    }

    impl CostMeter for Scripted {
        fn now_ns(&self) -> u64 {
            self.clock.fetch_add(250, Ordering::SeqCst)
        }
        fn allocations(&self) -> (u64, u64) {
            let n = self.allocs.fetch_add(3, Ordering::SeqCst);
            (n, n * 8)
        }
    }

    #[test]
    fn cost_start_answers_the_difference_between_its_two_readings() {
        let meter = Arc::new(Scripted { clock: AtomicU64::new(1_000), allocs: AtomicU64::new(10) });
        let cost = CostStart::begin(meter).finish();
        assert_eq!(cost, StepCost { start_ns: 1_000, ns: 250, allocs: 3, alloc_bytes: 24, peak_bytes: 0 });
    }

    /// A meter whose live bytes the test moves and whose peak follows them, as a counting allocator's does.
    struct Held {
        clock: AtomicU64,
        live: std::sync::Mutex<(i64, i64)>,
    }

    impl Held {
        fn grow(&self, by: i64) {
            let mut held = self.live.lock().unwrap();
            held.0 += by;
            held.1 = held.1.max(held.0);
        }
    }

    impl CostMeter for Held {
        fn now_ns(&self) -> u64 {
            self.clock.fetch_add(1, Ordering::SeqCst)
        }
        fn allocations(&self) -> (u64, u64) {
            (0, 0)
        }
        fn held(&self) -> Option<(i64, i64)> {
            Some(*self.live.lock().unwrap())
        }
        fn restart_peak(&self, at: i64) {
            self.live.lock().unwrap().1 = at;
        }
    }

    #[test]
    fn a_step_reports_the_most_it_held_above_where_it_started() {
        let meter = Arc::new(Held { clock: AtomicU64::new(0), live: std::sync::Mutex::new((1_000, 5_000)) });
        let step = CostStart::begin(meter.clone());
        meter.grow(300);
        meter.grow(-300);
        assert_eq!(step.finish().peak_bytes, 300, "the step peaked 300 above its start, not at the earlier 5000");
    }

    #[test]
    fn control_an_inner_step_does_not_hide_the_outer_steps_peak() {
        let meter = Arc::new(Held { clock: AtomicU64::new(0), live: std::sync::Mutex::new((0, 0)) });
        let outer = CostStart::begin(meter.clone());
        meter.grow(800);
        meter.grow(-800);
        let inner = CostStart::begin(meter.clone());
        meter.grow(100);
        meter.grow(-100);
        assert_eq!(inner.finish().peak_bytes, 100);
        assert_eq!(outer.finish().peak_bytes, 800);
    }

    #[test]
    fn control_a_meter_without_a_held_reading_reports_no_peak() {
        let meter = Arc::new(Scripted { clock: AtomicU64::new(1_000), allocs: AtomicU64::new(10) });
        assert_eq!(CostStart::begin(meter).finish().peak_bytes, 0);
    }

    #[test]
    fn step_cost_serializes_in_the_wire_names() {
        let cost = StepCost { start_ns: 5, ns: 7, allocs: 2, alloc_bytes: 64, peak_bytes: 0 };
        assert_eq!(
            serde_json::to_value(cost).unwrap(),
            serde_json::json!({ "startNs": 5, "ns": 7, "allocs": 2, "allocBytes": 64 })
        );
    }
}
