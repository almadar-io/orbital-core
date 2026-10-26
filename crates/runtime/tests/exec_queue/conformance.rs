//! EXEC-QUEUE conformance: who hears an emit, and what a listener's guard reads.


use orbital_core_base::value::Value;
use serde_json::{json, Value as Json};
use crate::support::TestKernel;

fn emitter() -> Json {
    json!({ "name": "Play", "linkedEntity": "S",
        "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
            "transitions": [{ "from": "on", "to": "on", "event": "UP", "effects": [["emit", "TURN", { "raw": "up" }]] }] } })
}

fn heard_by(traits: Vec<Json>, who: &str) -> bool {
    let kernel = TestKernel::server().build(json!({
        "name": "Queue",
        "orbitals": [{
            "name": "QueueOrbital",
            "entity": { "name": "S", "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }, { "name": "dir", "type": "string" }] },
            "traits": traits
        }]
    }));
    let outcomes = kernel.dispatch("Play", None, "UP", Value::Null).unwrap();
    outcomes.iter().any(|(t, r)| t == who && r.executed)
}

fn listener(guard: Json) -> Json {
    json!({ "name": "Mechanic", "linkedEntity": "S",
        "listens": [{ "event": "Play.TURN", "triggers": "STEER", "guard": guard,
            "payloadMapping": { "mapped": "@payload.raw" } }],
        "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
            "transitions": [{ "from": "on", "to": "on", "event": "STEER", "effects": [["set", "@entity.dir", "@payload.mapped"]] }] } })
}

#[test]
fn exec_queue_3_a_listen_guard_reads_the_raw_source_payload_before_the_with_mapping() {
    assert!(heard_by(vec![emitter(), listener(json!(["=", "@payload.raw", "up"]))], "Mechanic"));
}

#[test]
fn exec_queue_3_control_a_listen_guard_never_sees_the_mapped_field() {
    assert!(!heard_by(vec![emitter(), listener(json!(["=", "@payload.mapped", "up"]))], "Mechanic"));
}

fn bystander() -> Json {
    json!({ "name": "Bystander", "linkedEntity": "S",
        "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
            "transitions": [{ "from": "on", "to": "on", "event": "TURN", "effects": [["set", "@entity.dir", "overheard"]] }] } })
}

#[test]
fn exec_queue_7_a_transition_named_like_the_emit_is_not_a_listen() {
    assert!(!heard_by(vec![emitter(), bystander()], "Bystander"));
}

#[test]
fn exec_queue_7_control_a_declared_listens_edge_hears_the_emit() {
    assert!(heard_by(vec![emitter(), bystander(), listener(json!(true))], "Mechanic"));
}

fn name_only_listener_hears(source: Json) -> bool {
    use orbital_core_base::bus_event::BusEventSource;
    use orbital_core_base::events::types::SubscriptionId;
    use orbital_core_base::schema::types::ListenSource;
    use orbital_core_runtime::listener::ListenerInfo;
    let listener = ListenerInfo {
        event_pattern: "TURN".into(),
        trait_name: "Mechanic".into(),
        orbital: "Main".into(),
        triggers: "STEER".into(),
        source: Some(ListenSource::Trait { trait_name: "Play".into(), trait_id: None }),
        payload_mapping: None,
        guard: None,
        subscription_id: SubscriptionId(0),
    };
    let source: BusEventSource = serde_json::from_value(source).unwrap();
    listener.matches_source(&source)
}

#[test]
fn exec_queue_6_an_emit_with_no_stamped_orbital_matches_no_name_only_source() {
    assert!(!name_only_listener_hears(json!({ "trait": "Play" })));
}

#[test]
fn exec_queue_6_control_the_same_emit_stamped_with_the_listeners_orbital_matches() {
    assert!(name_only_listener_hears(json!({ "orbital": "Main", "trait": "Play" })));
}
