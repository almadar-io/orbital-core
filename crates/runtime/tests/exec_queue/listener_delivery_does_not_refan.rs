//! A listener delivery runs the listener's `triggers` event on the listener; it does not fan that
//! event out again. Only the seed and the effects' `emit`s reach listeners (EXEC-SEED-3, EXEC-QUEUE-2).
//! A listener whose `triggers` is the very event it listens to used to re-deliver to itself until
//! the 2000-step cap (JS twin: `almadar-runtime/test/stateful-create-keeps-state-address.test.ts`).

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment};
use serde_json::json;
use std::sync::Arc;

fn schema(trigger_is_listened_name: bool) -> OrbitalSchema {
    let triggers = if trigger_is_listened_name {
        "RESULTS"
    } else {
        "GOT"
    };
    serde_json::from_value(json!({
        "name": "refan",
        "typeContractVersion": 1, "orbitals": [
            {
                "name": "A",
                "entity": { "name": "Q", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "answered", "type": "number", "default": 0 }] },
                "traits": [{
                    "name": "Searcher", "linkedEntity": "Q",
                    "listens": [{ "event": "RESULTS", "source": { "kind": "any" }, "triggers": triggers }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "searching" }],
                        "transitions": [
                            { "from": "idle", "to": "searching", "event": "SEARCH", "effects": [["emit", "REQUESTED", {}]] },
                            { "from": "searching", "to": "searching", "event": triggers,
                              "effects": [["set", "@entity.answered", ["+", ["object/get", "@entity", "answered", 0], 1]]] }
                        ]
                    }
                }]
            },
            {
                "name": "B",
                "entity": { "name": "T", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Responder", "linkedEntity": "T",
                    "listens": [{ "event": "REQUESTED", "source": { "kind": "any" }, "triggers": "REQUESTED" }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "REQUESTED", "effects": [["emit", "RESULTS", {}]] }]
                    }
                }]
            }
        ]
    }))
    .unwrap()
}

fn answered(trigger_is_listened_name: bool) -> (Option<f64>, bool) {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel
        .register_schema(schema(trigger_is_listened_name))
        .unwrap();
    let outcomes = kernel
        .dispatch("Searcher", None, "SEARCH", orbital_core_base::value::Value::Null)
        .unwrap();
    let truncated = outcomes.iter().any(|(_, r)| !r.cascade_dropped.is_empty());
    let searcher = outcomes
        .iter()
        .rev()
        .find(|(t, _)| t == "Searcher")
        .map(|(_, r)| serde_json::to_value(&r.entity_data).unwrap())
        .unwrap();
    (searcher["answered"].as_f64(), truncated)
}

#[test]
fn a_listener_whose_trigger_is_the_listened_event_runs_once_per_emit() {
    assert_eq!(answered(true), (Some(1.0), false));
}

#[test]
fn control_a_distinct_trigger_name_runs_once_per_emit() {
    assert_eq!(answered(false), (Some(1.0), false));
}
