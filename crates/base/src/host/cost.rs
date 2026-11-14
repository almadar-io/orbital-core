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
}

/// A reading taken when a measured step starts; `finish` reads again and answers the difference.
pub struct CostStart {
    meter: Arc<dyn CostMeter>,
    start_ns: u64,
    allocs: u64,
    bytes: u64,
}

impl CostStart {
    pub fn begin(meter: Arc<dyn CostMeter>) -> Self {
        let (allocs, bytes) = meter.allocations();
        let start_ns = meter.now_ns();
        Self { meter, start_ns, allocs, bytes }
    }

    pub fn finish(self) -> StepCost {
        let end_ns = self.meter.now_ns();
        let (allocs, bytes) = self.meter.allocations();
        StepCost {
            start_ns: self.start_ns,
            ns: end_ns - self.start_ns,
            allocs: allocs - self.allocs,
            alloc_bytes: bytes - self.bytes,
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
        assert_eq!(cost, StepCost { start_ns: 1_000, ns: 250, allocs: 3, alloc_bytes: 24 });
    }

    #[test]
    fn step_cost_serializes_in_the_wire_names() {
        let cost = StepCost { start_ns: 5, ns: 7, allocs: 2, alloc_bytes: 64 };
        assert_eq!(
            serde_json::to_value(cost).unwrap(),
            serde_json::json!({ "startNs": 5, "ns": 7, "allocs": 2, "allocBytes": 64 })
        );
    }
}
