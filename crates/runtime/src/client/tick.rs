//! Tick scheduler — drives an orbital's resolved `ticks` on a wall clock.
//!
//! Hoisted from the egui game client (`rendering/game.rs`) so every shell
//! (egui, dioxus-native, …) shares the ONE scheduling loop. `new`/`rebuild`
//! read the active orbital's tick schedule from `ClientCore`; `poll_due`
//! returns the `(trait_name, tick_name)` pairs whose interval has elapsed
//! (a `None` interval = per-frame tick, always due). The caller drives each
//! due tick through `ClientCore::drive_tick_blocking` and requests a repaint
//! whenever the scheduler is non-empty — the client is otherwise
//! event-reactive, so without this the `set`-effect loops
//! (`gameflowTick every 33ms`) never advance.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::time::Duration;
use web_time::Instant;

const NS: &str = "almadar::client::tick";

/// One scheduled tick's runtime bookkeeping: which trait/tick to fire, its
/// interval (`None` = every frame), and when it last fired.
struct TickTimer {
    trait_name: String,
    tick_name: String,
    interval: Option<Duration>,
    last_fired: Instant,
}

/// Per-orbital tick schedule, rebuilt on init and on any orbital switch.
pub struct TickScheduler {
    timers: Vec<TickTimer>,
}

impl TickScheduler {
    /// Build the scheduler from the active orbital's resolved `ticks`.
    pub fn new(core: &crate::client::client_core::ClientCore) -> Self {
        let mut scheduler = Self { timers: Vec::new() };
        scheduler.rebuild(core);
        scheduler
    }

    /// (Re)build from the active orbital's resolved `ticks`. Called after init
    /// and on any orbital switch so the right traits' ticks fire.
    pub fn rebuild(&mut self, core: &crate::client::client_core::ClientCore) {
        let now = Instant::now();
        self.timers = core
            .tick_schedule()
            .into_iter()
            .map(|(trait_name, tick_name, interval_ms)| TickTimer {
                trait_name,
                tick_name,
                interval: interval_ms.map(Duration::from_millis),
                last_fired: now,
            })
            .collect();
        if !self.timers.is_empty() {
            tracing::info!(target: NS, count = self.timers.len(), "Scheduled game ticks");
        }
    }

    /// Whether any tick is scheduled — callers request continuous repaint then.
    pub fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }

    /// Drain the ticks whose interval has elapsed (per-frame ticks always due),
    /// stamping their `last_fired`. Returns `(trait_name, tick_name)` pairs.
    pub fn poll_due(&mut self) -> Vec<(String, String)> {
        let now = Instant::now();
        let mut due = Vec::new();
        for timer in &mut self.timers {
            let elapsed = match timer.interval {
                None => true,
                Some(interval) => now.duration_since(timer.last_fired) >= interval,
            };
            if elapsed {
                timer.last_fired = now;
                due.push((timer.trait_name.clone(), timer.tick_name.clone()));
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::client_core::ClientCore;
    use orbital_core_base::schema::types::OrbitalSchema;
    use serde_json::json;

    fn core(ticks: serde_json::Value) -> ClientCore {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "t",
            "version": "1.0.0",
            "typeContractVersion": 1, "orbitals": [{
                "name": "O",
                "entity": { "name": "E", "persistence": "runtime", "fields": [] },
                "traits": [{
                    "name": "T",
                    "linkedEntity": "E",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [],
                        "transitions": []
                    },
                    "ticks": ticks
                }],
                "pages": []
            }]
        }))
        .expect("schema");
        ClientCore::new(schema, None)
    }

    #[test]
    fn a_frame_tick_is_due_on_every_poll_and_the_scheduler_is_not_empty() {
        let c = core(json!([{ "name": "f", "interval": "frame", "effects": [] }]));
        let mut s = TickScheduler::new(&c);
        assert!(!s.is_empty());
        assert_eq!(s.poll_due(), [("T".to_string(), "f".to_string())]);
        assert_eq!(s.poll_due().len(), 1);
    }

    #[test]
    fn an_interval_tick_is_not_due_before_its_interval_elapses() {
        let c = core(json!([{ "name": "slow", "interval": "1h", "effects": [] }]));
        let mut s = TickScheduler::new(&c);
        assert!(s.poll_due().is_empty());
        assert!(s.poll_due().is_empty(), "control: still inside the hour");
    }

    #[test]
    fn a_tickless_orbital_has_an_empty_scheduler() {
        let c = core(json!([]));
        let mut s = TickScheduler::new(&c);
        assert!(s.is_empty());
        assert!(s.poll_due().is_empty());
    }
}
