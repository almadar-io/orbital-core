//! An id-less event (`entity_id: None`) still keeps a per-trait frame across
//! effects in the SAME transition: `(set @entity.x <payload.v>)` followed by
//! `(emit E {x: @entity.x})` must emit the value just written, not an
//! unresolved binding. Mirrors `intra_transition_set_visibility.rs`'s
//! construction pattern for the "singleton" (id-less) path specifically —
//! see the doc comment beside `RuntimeKernel::process_event`'s
//! `entity_id.unwrap_or("singleton")`.

use orbital_core_base::value::ValueMap;
use orbital_core_base::effect_types::Effect;
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use std::sync::Arc;

fn obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = ValueMap::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    Value::object(m)
}

fn frame_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Widget",
                "fields": [
                    { "name": "x", "type": "string" }
                ]
            },
            "traits": [{
                "name": "WidgetShow",
                "linkedEntity": "Widget",
                "stateMachine": {
                    "states": [
                        { "name": "idle", "isInitial": true }
                    ],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT",
                          "effects": [
                            ["set", "@entity.x", "@payload.v"],
                            ["emit", "WIDGET_SHOWN", {"x": "@entity.x"}]
                          ] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn kernel_with(schema: OrbitalSchema, cache: Arc<InMemoryEntityCache>) -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

#[test]
fn idless_event_emits_the_value_set_earlier_in_the_same_transition() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = kernel_with(frame_schema(), cache);

    // No entity id at all — this is the per-trait "singleton" frame path.
    let result = kernel
        .process_event(
            "WidgetShow",
            None,
            "INIT",
            obj(&[("v", Value::string("hello"))]),
        )
        .unwrap();
    assert!(result.executed, "the transition must fire");

    let emitted = result
        .effect_results
        .iter()
        .find_map(|r| match &r.effect {
            Effect::Emit { event, payload } if event == "WIDGET_SHOWN" => Some(payload.clone()),
            _ => None,
        })
        .expect("the WIDGET_SHOWN emit must be reported");

    assert_eq!(
        emitted.as_object().and_then(|m| m.get("x")),
        Some(&Value::string("hello")),
        "the id-less per-trait frame must carry the value set earlier in the same transition"
    );
}
