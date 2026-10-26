// Runtime Spec Clauses 3.3 + 5.5 on the Rust kernel. Twin of `almadar-runtime/test/dispatch-roots.test.ts`.

use orbital_core_base::effect_types::Effect;
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn once() -> Json {
    json!([
        "not",
        [
            "array/includes",
            [
                "array/map",
                "@prevEvents",
                ["fn", "e", ["object/get", "@e", "event"]]
            ],
            "X"
        ]
    ])
}

fn listener(name: &str, ack: &str, guard: Option<Json>) -> Json {
    let mut listen =
        json!({ "event": "X", "triggers": "GOT", "source": { "kind": "trait", "trait": "Src" } });
    if let Some(g) = guard {
        listen["guard"] = g;
    }
    json!({
        "name": name,
        "listens": [listen],
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "GOT", "effects": [["emit", ack, { "n": "@payload.n" }]] }]
        }
    })
}

fn kernel() -> RuntimeKernel {
    let prov_ack = json!([["emit", "ACK_PROV", {
        "srcTrait": "@event.source.trait", "srcEvent": "@event.event", "srcN": "@event.payload.n",
        "from": "@fromState", "to": "@toState", "state": "@state", "seen": ["array/len", "@prevEvents"]
    }]]);
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "dispatch-roots-app",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Src", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO",
                        "effects": [["emit", "X", { "n": 1 }], ["emit", "X", { "n": 2 }], ["emit", "X", { "n": 3 }]] }]
                } },
                listener("Once", "ACK_ONCE", Some(once())),
                listener("Twin", "ACK_TWIN", Some(once())),
                listener("All", "ACK_ALL", None),
                { "name": "Prov",
                  "listens": [{ "event": "X", "triggers": "GOT", "source": { "kind": "trait", "trait": "Src" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "seen" }],
                    "transitions": [
                        { "from": "idle", "to": "seen", "event": "GOT",
                          "guard": ["and", ["=", "@fromState", "idle"], ["=", "@toState", "seen"], ["=", "@state", "idle"]], "effects": prov_ack },
                        { "from": "seen", "to": "seen", "event": "GOT", "effects": prov_ack }
                    ]
                } },
                { "name": "Reenter",
                  "listens": [{ "event": "X", "triggers": "STEP", "source": { "kind": "trait", "trait": "Src" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "processing" }],
                    "transitions": [
                        { "from": "idle", "to": "processing", "event": "STEP",
                          "guard": ["not", ["array/includes", "@prevStates", "processing"]], "effects": [["emit", "ACK_R", { "to": "@toState" }]] },
                        { "from": "processing", "to": "idle", "event": "STEP", "effects": [["emit", "ACK_R", { "to": "@toState" }]] }
                    ]
                } }
            ]
        }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    k
}

/// The evaluator holds every number as f64; compare integral values as integers, like the JS twin.
fn normalize(v: Json) -> Json {
    match v {
        Json::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 => json!(f as i64),
            _ => Json::Number(n),
        },
        Json::Object(m) => Json::Object(m.into_iter().map(|(k, v)| (k, normalize(v))).collect()),
        Json::Array(a) => Json::Array(a.into_iter().map(normalize).collect()),
        other => other,
    }
}

/// Every payload emitted as `event`, in dispatch order, as JSON.
fn payloads(k: &RuntimeKernel, event: &str) -> Vec<Json> {
    k.dispatch("Src", None, "GO", Value::Null)
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event: e, payload } if e == event => {
                Some(normalize(serde_json::to_value(payload).unwrap()))
            }
            _ => None,
        })
        .collect()
}

// EXEC-QUEUE-5
#[test]
fn once_in_a_listen_when_admits_only_the_first_delivery_and_the_control_gets_all() {
    let k = kernel();
    assert_eq!(payloads(&k, "ACK_ONCE"), vec![json!({ "n": 1 })]);
    assert_eq!(payloads(&kernel(), "ACK_ALL").len(), 3);
}

#[test]
fn each_listener_keeps_its_own_log() {
    assert_eq!(payloads(&kernel(), "ACK_TWIN"), vec![json!({ "n": 1 })]);
}

// EXEC-STEP-2
#[test]
fn event_is_the_source_emit_guards_read_pre_commit_effects_post_commit() {
    assert_eq!(
        payloads(&kernel(), "ACK_PROV"),
        vec![
            json!({ "srcTrait": "Src", "srcEvent": "X", "srcN": 1, "from": "idle", "to": "seen", "state": "seen", "seen": 0 }),
            json!({ "srcTrait": "Src", "srcEvent": "X", "srcN": 2, "from": "seen", "to": "seen", "state": "seen", "seen": 1 }),
            json!({ "srcTrait": "Src", "srcEvent": "X", "srcN": 3, "from": "seen", "to": "seen", "state": "seen", "seen": 2 }),
        ]
    );
}

#[test]
fn prev_states_blocks_re_entry_into_a_state_already_left_this_dispatch() {
    assert_eq!(
        payloads(&kernel(), "ACK_R"),
        vec![json!({ "to": "processing" }), json!({ "to": "idle" })]
    );
}

#[test]
fn the_memory_dies_with_the_dispatch() {
    let k = kernel();
    assert_eq!(payloads(&k, "ACK_ONCE"), vec![json!({ "n": 1 })]);
    assert_eq!(payloads(&k, "ACK_ONCE"), vec![json!({ "n": 1 })]);
}

fn prov_acks(delivery: Option<Json>, log: Option<Json>) -> Vec<Json> {
    let delivery: Option<orbital_core_runtime::effects::DeliveryRecord> =
        delivery.map(|d| serde_json::from_value(d).unwrap());
    let log: Option<orbital_core_runtime::effects::DispatchLog> =
        log.map(|l| serde_json::from_value(l).unwrap());
    kernel()
        .dispatch_continuing(
            "Prov",
            None,
            "GOT",
            serde_json::from_value(json!({ "n": 5 })).unwrap(),
            delivery.as_ref(),
            log.as_ref(),
        )
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event, payload } if event == "ACK_PROV" => {
                Some(normalize(serde_json::to_value(payload).unwrap()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_request_continuing_a_client_dispatch_reads_event_and_the_log_from_it() {
    assert_eq!(
        prov_acks(
            Some(
                json!({ "event": "X", "payload": { "n": 5 }, "source": { "orbital": "Main", "trait": "Src" } })
            ),
            Some(
                json!({ "prevEvents": [{ "event": "X", "payload": { "n": 4 }, "source": { "trait": "Src" } }], "prevStates": [] })
            ),
        ),
        vec![
            json!({ "srcTrait": "Src", "srcEvent": "X", "srcN": 5, "from": "idle", "to": "seen", "state": "seen", "seen": 1 })
        ]
    );
}

#[test]
fn without_them_the_request_is_a_direct_dispatch() {
    let acks = prov_acks(None, None);
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0]["srcEvent"], json!("GOT"));
    assert_eq!(acks[0]["seen"], json!(0));
    assert!(
        acks[0].get("srcTrait").is_none(),
        "a direct dispatch has no source trait: the prop is absent, not the literal token ({})",
        acks[0]
    );
}
