// One `now` stamp per dispatch: every guard, effect and cascaded listener in a dispatch reads the same value.

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use std::sync::Arc;

fn schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Source",
                    "linkedEntity": "Item",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{
                            "from": "idle", "to": "idle", "event": "GO",
                            "guard": ["<=", ["time/now"], "@now"],
                            "effects": [
                                ["set", "@entity.viaOperator", ["time/now"]],
                                ["set", "@entity.viaBinding", "@now"],
                                ["emit", "PING"]
                            ]
                        }]
                    },
                    "ticks": [{ "name": "beat", "interval": 1000, "effects": [["set", "@entity.tickedAt", "@now"]] }]
                },
                {
                    "name": "Listener",
                    "linkedEntity": "Item",
                    "listens": [{ "event": "PING", "triggers": "PONG", "source": { "kind": "trait", "trait": "Source" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{
                            "from": "idle", "to": "idle", "event": "PONG",
                            "effects": [["set", "@entity.heardAt", "@now"]]
                        }]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

fn kernel() -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema()).unwrap();
    kernel
}

fn field(
    outcomes: &[(String, orbital_core_runtime::process_result::ProcessEventResult)],
    trait_name: &str,
    key: &str,
) -> f64 {
    let (_, result) = outcomes
        .iter()
        .find(|(t, r)| t == trait_name && r.executed)
        .unwrap_or_else(|| panic!("{trait_name} must execute"));
    result
        .entity_data
        .get_path(key)
        .and_then(Value::as_number)
        .unwrap_or_else(|| {
            panic!(
                "{trait_name}.{key} must be a number, got {:?}",
                result.entity_data
            )
        })
}

fn go(kernel: &RuntimeKernel) -> Vec<(String, orbital_core_runtime::process_result::ProcessEventResult)> {
    kernel
        .dispatch("Source", None, "GO", Value::Object(Default::default()))
        .unwrap()
}

#[test]
fn operator_binding_guard_and_cascaded_listener_share_one_stamp() {
    let kernel = kernel();
    let outcomes = go(&kernel);
    let via_operator = field(&outcomes, "Source", "viaOperator");
    assert_eq!(field(&outcomes, "Source", "viaBinding"), via_operator);
    assert_eq!(field(&outcomes, "Listener", "heardAt"), via_operator);
}

#[test]
fn each_dispatch_gets_its_own_later_stamp() {
    let kernel = kernel();
    let first = field(&go(&kernel), "Source", "viaOperator");
    std::thread::sleep(std::time::Duration::from_millis(5));
    let second = field(&go(&kernel), "Source", "viaOperator");
    assert!(
        second > first,
        "second dispatch must be stamped later ({second} vs {first})"
    );
}

#[test]
fn each_tick_firing_is_stamped_on_its_own() {
    let kernel = kernel();
    let first = kernel.process_tick("Source", None, "beat").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let second = kernel.process_tick("Source", None, "beat").unwrap();
    let at = |r: &orbital_core_runtime::process_result::ProcessEventResult| {
        r.entity_data
            .get_path("tickedAt")
            .and_then(Value::as_number)
    };
    assert!(
        at(&first).is_some(),
        "tick must stamp tickedAt: {:?}",
        first.entity_data
    );
    assert!(at(&second) > at(&first));
}
