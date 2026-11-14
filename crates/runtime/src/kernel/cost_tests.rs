//! EXEC-COST-1: a step's and each authored effect's cost is measured only inside a cost scope and only with a meter;
//! everything else about the step is unchanged.
use super::*;
use crate::entity_source::InMemoryEntityCache;
use orbital_core_base::cost::{CostMeter, StepCost};
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

fn scripted() -> Arc<dyn CostMeter> {
    Arc::new(Scripted { clock: AtomicU64::new(1_000), allocs: AtomicU64::new(0) })
}

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "Cost",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Page",
            "entity": { "name": "View", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Counter",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "BUMP",
                          "effects": [["set", "@entity.count", 1], ["set", "@entity.label", "one"]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn bump(kernel: &RuntimeKernel) -> ProcessEventResult {
    kernel
        .dispatch("Counter", None, "BUMP", Value::Null)
        .unwrap()
        .into_iter()
        .find(|(t, _)| t == "Counter")
        .expect("Counter ran")
        .1
}

#[test]
fn cost_a_step_in_a_cost_scope_carries_its_cost_and_each_authored_effects() {
    let kernel = kernel();
    kernel.set_cost_meter(Some(scripted()));
    let result = kernel.with_cost(|| bump(&kernel));
    assert!(result.executed);
    let effects: Vec<Option<StepCost>> = result.effect_runs.iter().map(|run| run.cost).collect();
    assert_eq!(
        effects,
        vec![
            Some(StepCost { start_ns: 1_250, ns: 250, allocs: 3, alloc_bytes: 24 }),
            Some(StepCost { start_ns: 1_750, ns: 250, allocs: 3, alloc_bytes: 24 }),
        ]
    );
    assert_eq!(result.cost, Some(StepCost { start_ns: 1_000, ns: 1_250, allocs: 15, alloc_bytes: 120 }));
}

#[test]
fn cost_control_outside_a_cost_scope_nothing_is_measured() {
    let kernel = kernel();
    kernel.set_cost_meter(Some(scripted()));
    let result = bump(&kernel);
    assert!(result.executed);
    assert_eq!(result.cost, None);
    assert!(result.effect_runs.iter().all(|run| run.cost.is_none()));
}

#[test]
fn cost_control_a_scope_without_a_meter_measures_nothing() {
    let kernel = kernel();
    let result = kernel.with_cost(|| bump(&kernel));
    assert!(result.executed);
    assert_eq!(result.cost, None);
    assert!(result.effect_runs.iter().all(|run| run.cost.is_none()));
}

#[test]
fn cost_the_traced_transition_has_no_cost_key_unless_measured() {
    let kernel = kernel();
    let plain = crate::dispatch_trace::transition_of("Counter", &bump(&kernel), None);
    let plain_json = serde_json::to_value(&plain).unwrap();
    assert!(plain_json.get("cost").is_none(), "{plain_json}");
    assert!(plain_json["effects"].as_array().unwrap().iter().all(|e| e.get("cost").is_none()));

    let measured_kernel = self::kernel();
    measured_kernel.set_cost_meter(Some(scripted()));
    let measured = measured_kernel.with_cost(|| bump(&measured_kernel));
    let traced = serde_json::to_value(crate::dispatch_trace::transition_of("Counter", &measured, None)).unwrap();
    assert_eq!(traced["cost"], serde_json::json!({ "startNs": 1_000, "ns": 1_250, "allocs": 15, "allocBytes": 120 }));
    assert_eq!(traced["effects"][1]["cost"]["startNs"], 1_750);
    assert_eq!((traced["effects"][0]["op"].as_str(), traced["effects"][1]["op"].as_str()), (Some("set"), Some("set")), "each effect names its operator");
    let mut stripped = traced.clone();
    stripped.as_object_mut().unwrap().remove("cost");
    for effect in stripped["effects"].as_array_mut().unwrap() {
        effect.as_object_mut().unwrap().remove("cost");
    }
    assert_eq!(stripped, plain_json, "every other field is unchanged");
}

#[test]
fn cost_a_trace_feed_measures_its_dispatches_only_while_cost_is_on() {
    let kernel = kernel();
    kernel.set_cost_meter(Some(scripted()));
    let feed = Arc::new(crate::dispatch_trace::TraceFeed::default());
    kernel.set_dispatch_observer(Some(Arc::new(feed.observer())));
    feed.set_on(true);
    bump(&kernel);
    feed.set_cost(true);
    bump(&kernel);
    let items = feed.take_views();
    assert_eq!(items.len(), 2);
    assert!(items[0].costed.is_none(), "cost off: nothing measured");
    let costed = items[1].costed.as_ref().expect("cost on: the dispatch was measured");
    assert_eq!(costed["dispatch"]["outcomes"][0]["cost"]["ns"], 1_250);
    assert_eq!(costed["dispatch"]["outcomes"][0]["effects"][0]["cost"]["ns"], 250);
    assert!(items[1].plain["dispatch"]["outcomes"][0].get("cost").is_none(), "a stream without cost sees none");
    assert_eq!(items[1].plain["dispatch"]["seq"], costed["dispatch"]["seq"]);
}

#[test]
fn cost_control_a_feed_that_is_off_turns_no_measuring_on() {
    let kernel = kernel();
    kernel.set_cost_meter(Some(scripted()));
    let feed = Arc::new(crate::dispatch_trace::TraceFeed::default());
    kernel.set_dispatch_observer(Some(Arc::new(feed.observer())));
    feed.set_cost(true);
    assert_eq!(bump(&kernel).cost, None, "a cost flag on a feed nobody reads measures nothing");
}
