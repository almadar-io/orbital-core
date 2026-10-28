// A fetch filter's `@config.<knob>` whose value is an `@entity` binding reads the ISSUING trait's entity
// (a detail pane filtered to the conversation its sibling selected), while `["object/get", "@entity", …]`
// stays the candidate row. JS twin: `interpolateFilterTraitRefs` + `chat-thread-opens-seeded-conversation`.

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::sync::Arc;

fn thread_loads(initial_filter_value: &str, active_channel: &str) -> Vec<String> {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "Chat",
        "typeContractVersion": 1, "orbitals": [{
            "name": "ChatOrbital",
            "entity": { "name": "Message", "persistence": "persistent", "shared": true, "fields": [
                { "name": "id", "type": "string" }, { "name": "channel", "type": "string" },
                { "name": "content", "type": "string" }, { "name": "activeChannel", "type": "string" }
            ] },
            "traits": [
                { "name": "Composer", "linkedEntity": "Message",
                  "stateMachine": { "states": [{ "name": "ready", "isInitial": true }],
                      "transitions": [{ "from": "ready", "to": "ready", "event": "PICK",
                          "effects": [["set", "@entity.activeChannel", active_channel]] }] } },
                { "name": "Thread", "linkedEntity": "Message",
                  "config": {
                      "initialFilterField": { "type": "string", "default": "channel" },
                      "initialFilterValue": { "type": "string", "default": initial_filter_value }
                  },
                  "stateMachine": { "states": [{ "name": "loading", "isInitial": true }, { "name": "browsing" }],
                      "transitions": [
                          { "from": "loading", "to": "loading", "event": "INIT",
                            "effects": [["fetch", "Message", {
                                "filter": ["=", ["object/get", "@entity", "@config.initialFilterField"], "@config.initialFilterValue"],
                                "emit": { "success": "Loaded" }
                            }]] },
                          { "from": "loading", "to": "browsing", "event": "Loaded",
                            "effects": [["set", "@entity.content", ["array/count", "@payload.data"]]] }
                      ] } }
            ]
        }]
    }))
    .unwrap();
    let source = Arc::new(InMemoryEntityCache::new());
    let rows: [(&str, &str, &str); 3] = [
        ("m1", "room-a", "in a"),
        ("m2", "room-b", "in b"),
        ("m3", "room-a", "also a"),
    ];
    for (id, channel, content) in rows {
        source
            .create(
                "Message",
                &serde_json::from_value(
                    json!({ "id": id, "channel": channel, "content": content }),
                )
                .unwrap(),
            )
            .unwrap();
    }
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        source as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    k.dispatch("Composer", None, "PICK", Value::Null).unwrap();
    let outcomes = k.dispatch("Thread", None, "INIT", Value::Null).unwrap();
    outcomes
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            orbital_core_base::effect_types::Effect::Fetch { .. } => er.return_value.clone(),
            _ => None,
        })
        .flat_map(|rows| match rows {
            Value::Array(rows) => rows,
            _ => Vec::new().into(),
        })
        .filter_map(|row| {
            row.get_path("id")
                .and_then(|id| id.as_str().map(str::to_string))
        })
        .collect()
}

#[test]
fn a_knob_bound_to_the_issuing_traits_entity_filters_to_its_value() {
    assert_eq!(
        thread_loads("@entity.activeChannel", "room-a"),
        ["m1", "m3"]
    );
}

#[test]
fn control_a_literal_knob_value_filters_to_itself() {
    assert_eq!(thread_loads("room-b", "room-a"), ["m2"]);
}

#[test]
fn edge_an_entity_binding_that_resolves_to_nothing_matches_nothing() {
    assert!(thread_loads("@entity.nothingHere", "room-a").is_empty());
}
