// A configured dispatch clock stamps exactly its time, on both the straight and the resumable entry; no clock is the wall clock.

use orbital_core_runtime::resumable::{drive_blocking, BlockingHost, IoRequest};
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use std::sync::Arc;

fn schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
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

fn kernel(clock: Option<f64>) -> RuntimeKernel {
    let config = KernelConfig {
        clock: clock.map(|t| Arc::new(move || t) as orbital_core_runtime::config::DispatchClock),
        ..KernelConfig::default()
    };
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        config,
    );
    kernel.register_schema(schema()).unwrap();
    kernel
}

fn field(
    outcomes: &[(String, orbital_core_runtime::process_result::ProcessEventResult)],
    trait_name: &str,
    key: &str,
) -> String {
    let (_, result) = outcomes
        .iter()
        .find(|(t, r)| t == trait_name && r.executed)
        .unwrap_or_else(|| panic!("{trait_name} must execute"));
    result
        .entity_data
        .to_value()
        .get_path(key)
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| {
            panic!(
                "{trait_name}.{key} must be an ISO instant, got {:?}",
                result.entity_data
            )
        })
}

fn go(kernel: &RuntimeKernel) -> Vec<(String, orbital_core_runtime::process_result::ProcessEventResult)> {
    kernel
        .dispatch("Source", None, "GO", Value::Object(Default::default()))
        .unwrap()
}

const PINNED: f64 = 1_700_000_000_000.0;
const PINNED_ISO: &str = "2023-11-14T22:13:20.000Z";

#[test]
fn a_pinned_clock_stamps_every_dispatch_with_exactly_that_time() {
    let kernel = kernel(Some(PINNED));
    let first = go(&kernel);
    assert_eq!(field(&first, "Source", "viaOperator"), PINNED_ISO);
    assert_eq!(field(&first, "Source", "viaBinding"), PINNED_ISO);
    assert_eq!(field(&first, "Listener", "heardAt"), PINNED_ISO);
    assert_eq!(field(&go(&kernel), "Source", "viaBinding"), PINNED_ISO);
}

#[test]
fn a_pinned_clock_stamps_the_resumable_dispatch_too() {
    let store: Arc<dyn EntitySource> = Arc::new(InMemoryEntityCache::new());
    let config = KernelConfig {
        clock: Some(Arc::new(|| PINNED) as orbital_core_runtime::config::DispatchClock),
        ..KernelConfig::default()
    };
    let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), config);
    kernel.register_schema(schema()).unwrap();
    let progress = kernel
        .start_dispatch("Source", None, "GO", Value::Object(Default::default()), None, None)
        .unwrap();
    let answer = |request: &IoRequest| {
        BlockingHost { source: store.as_ref(), service: None, substrate: None }.answer(request)
    };
    let outcomes = drive_blocking(&kernel, progress, &answer).unwrap();
    assert_eq!(field(&outcomes, "Source", "viaBinding"), PINNED_ISO);
    assert_eq!(field(&outcomes, "Listener", "heardAt"), PINNED_ISO);
}

#[test]
fn control_without_a_clock_the_wall_clock_stamps() {
    let kernel = kernel(None);
    let stamped = field(&go(&kernel), "Source", "viaBinding");
    assert_ne!(stamped, PINNED_ISO);
    assert!(stamped.as_str() > PINNED_ISO, "an ISO instant orders by its text");
}
