// Runtime Spec Clause 5.3: a cascade delivery is `(trait, event, from, payload)`; only an exact repeat is a cycle.

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::{RuntimeKernel, CROSS_TRAIT_CASCADE_CAP}};
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

/// `Fanner` re-triggers its own `STEP` from every `STEP_NEXT` it emits.
fn fanner(transitions: Json, states: Json) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Fanner",
                "listens": [{ "event": "STEP_NEXT", "triggers": "STEP", "source": { "kind": "trait", "trait": "Fanner" } }],
                "stateMachine": { "states": states, "transitions": transitions }
            }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
}

fn one_state() -> Json {
    json!([{ "name": "searching", "isInitial": true }])
}

fn executed_steps(kernel: &RuntimeKernel, payload: Json) -> usize {
    let payload: Value = serde_json::from_value(payload).unwrap();
    kernel
        .dispatch("Fanner", None, "STEP", payload)
        .unwrap()
        .iter()
        .filter(|(t, r)| t == "Fanner" && r.executed)
        .count()
}

#[test]
fn a_decrementing_fan_out_delivers_every_step() {
    let kernel = fanner(
        json!([{ "from": "searching", "to": "searching", "event": "STEP",
                 "guard": [">", "@payload.remaining", 0],
                 "effects": [["emit", "STEP_NEXT", { "remaining": ["-", "@payload.remaining", 1] }]] }]),
        one_state(),
    );
    assert_eq!(executed_steps(&kernel, json!({ "remaining": 3 })), 3);
}

#[test]
fn an_exact_repeat_is_a_cycle_and_stops() {
    let kernel = fanner(
        json!([{ "from": "searching", "to": "searching", "event": "STEP",
                 "effects": [["emit", "STEP_NEXT", { "k": 1 }]] }]),
        one_state(),
    );
    assert_eq!(executed_steps(&kernel, json!({ "k": 1 })), 1);
}

#[test]
fn the_same_payload_from_a_new_state_is_new_work() {
    let kernel = fanner(
        json!([
            { "from": "idle", "to": "busy", "event": "STEP", "effects": [["emit", "STEP_NEXT", { "k": 1 }]] },
            { "from": "busy", "to": "busy", "event": "STEP" }
        ]),
        json!([{ "name": "idle", "isInitial": true }, { "name": "busy" }]),
    );
    assert_eq!(executed_steps(&kernel, json!({ "k": 1 })), 2);
}

#[test]
fn a_chain_that_never_repeats_stops_at_the_cap() {
    let kernel = fanner(
        json!([{ "from": "searching", "to": "searching", "event": "STEP",
                 "effects": [["emit", "STEP_NEXT", { "n": ["+", "@payload.n", 1] }]] }]),
        one_state(),
    );
    let steps = executed_steps(&kernel, json!({ "n": 0 }));
    assert!(
        steps > 1 && steps <= CROSS_TRAIT_CASCADE_CAP + 1,
        "must terminate at the cap, ran {steps}"
    );
}

fn fanner_response(kernel: &RuntimeKernel, payload: Json) -> EventResponse {
    let payload: Value = serde_json::from_value(payload).unwrap();
    let outcomes = kernel.dispatch("Fanner", None, "STEP", payload).unwrap();
    EventResponse::from_process_results(&outcomes, "Fanner", "STEP")
}

/// JS twin: `cascade-visit-key.test.ts` "a chain that never repeats stops at the cap and says so".
#[test]
fn a_chain_stopped_at_the_cap_names_the_dropped_trait_on_the_response() {
    let kernel = fanner(
        json!([{ "from": "searching", "to": "searching", "event": "STEP",
                 "effects": [["emit", "STEP_NEXT", { "n": ["+", "@payload.n", 1] }]] }]),
        one_state(),
    );
    let response = fanner_response(&kernel, json!({ "n": 0 }));
    assert_eq!(response.cascade_truncated, Some(vec!["Fanner".to_string()]));
    assert_eq!(
        serde_json::to_value(&response).unwrap()["cascadeTruncated"],
        json!(["Fanner"])
    );
}

#[test]
fn control_an_exact_repeat_cycle_is_not_truncation() {
    let kernel = fanner(
        json!([{ "from": "searching", "to": "searching", "event": "STEP",
                 "effects": [["emit", "STEP_NEXT", { "k": 1 }]] }]),
        one_state(),
    );
    let response = fanner_response(&kernel, json!({ "k": 1 }));
    assert_eq!(response.cascade_truncated, None);
    assert!(serde_json::to_value(&response).unwrap().get("cascadeTruncated").is_none());
}

/// `Fanner` with an entity counter `n` the guard bounds; `set` is the effect that moves the frame.
fn counting_fanner(set_value: Json) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [
                { "name": "id", "type": "string" },
                { "name": "n", "type": "number", "default": 0 }
            ] },
            "traits": [{
                "name": "Fanner",
                "listens": [{ "event": "STEP_NEXT", "triggers": "STEP", "source": { "kind": "trait", "trait": "Fanner" } }],
                "stateMachine": { "states": one_state(), "transitions": [{
                    "from": "searching", "to": "searching", "event": "STEP",
                    "guard": ["<", "@entity.n", 3],
                    "effects": [["set", "@entity.n", set_value], ["emit", "STEP_NEXT", { "k": 1 }]]
                }] }
            }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
}

#[test]
fn an_identical_payload_with_a_moved_frame_is_progress_not_a_cycle() {
    let kernel = counting_fanner(json!(["+", "@entity.n", 1]));
    assert_eq!(executed_steps(&kernel, json!({ "k": 1 })), 3);
}

#[test]
fn an_identical_payload_with_an_unchanged_frame_is_still_a_cycle() {
    let kernel = counting_fanner(json!(1));
    assert_eq!(executed_steps(&kernel, json!({ "k": 1 })), 2);
}

/// Progress lives in the trait's persisted row (a `persist update`), never in a `set` frame.
fn persisting_fanner(n_value: Json) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "persistence": "persistent", "collection": "items", "fields": [
                { "name": "id", "type": "string" },
                { "name": "n", "type": "number", "default": 0 }
            ] },
            "traits": [{
                "name": "Fanner", "linkedEntity": "Item",
                "listens": [{ "event": "STEP_NEXT", "triggers": "STEP", "source": { "kind": "trait", "trait": "Fanner" } }],
                "stateMachine": { "states": one_state(), "transitions": [{
                    "from": "searching", "to": "searching", "event": "STEP",
                    "guard": ["<", "@entity.n", 3],
                    "effects": [
                        ["persist", "update", "Item", { "id": "singleton", "n": n_value }],
                        ["emit", "STEP_NEXT", { "k": 1 }]
                    ]
                }] }
            }]
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
        .entity_source()
        .write(
            "Item",
            "singleton",
            &serde_json::from_value::<Value>(json!({ "id": "singleton", "n": 0 })).unwrap(),
        )
        .unwrap();
    kernel
}

#[test]
fn an_identical_payload_with_a_moved_row_is_progress_not_a_cycle() {
    let kernel = persisting_fanner(json!(["+", "@entity.n", 1]));
    assert_eq!(executed_steps(&kernel, json!({ "k": 1 })), 3);
}

#[test]
fn an_identical_payload_with_an_unchanged_row_is_still_a_cycle() {
    let kernel = persisting_fanner(json!(0));
    assert_eq!(executed_steps(&kernel, json!({ "k": 1 })), 1);
}

/// Server reports `X` from `Source`; the client fans it through `Relay` to `Chime` (EXEC-DIST-10).
fn relay_chain() -> RuntimeKernel {
    let idle = json!([{ "name": "idle", "isInitial": true }]);
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Source", "stateMachine": { "states": idle, "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }] } },
                { "name": "Relay",
                  "listens": [{ "event": "X", "triggers": "GO", "source": { "kind": "trait", "trait": "Source" } }],
                  "stateMachine": { "states": idle, "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "RING", { "heardFrom": "@event.source.trait", "heardEvent": "@event.event" }]] }] } },
                { "name": "Chime",
                  "listens": [{ "event": "RING", "triggers": "PING", "source": { "kind": "trait", "trait": "Relay" } }],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "pinged" }],
                      "transitions": [{ "from": "idle", "to": "pinged", "event": "PING" }]
                  } }
            ]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
}

fn x_from_source() -> orbital_core_runtime::effects::response::EventResponse {
    serde_json::from_value(json!({
        "success": true, "transitioned": true, "states": {},
        "emittedEvents": [{ "event": "X", "source": { "orbital": "Main", "trait": "Source" } }]
    }))
    .unwrap()
}

fn chime_pinged(already: &std::collections::HashSet<(String, String)>) -> bool {
    relay_chain()
        .apply_server_response(&x_from_source(), already)
        .unwrap()
        .iter()
        .any(|(t, r)| t == "Chime" && r.executed && r.new_state == "pinged")
}

#[test]
fn with_nothing_delivered_the_fold_reaches_chime() {
    assert!(chime_pinged(&Default::default()));
}

#[test]
fn a_pair_the_fold_reaches_through_a_fresh_step_is_delivered_though_the_dispatch_delivered_it() {
    let already = std::iter::once(("Chime".to_string(), "PING".to_string())).collect();
    assert!(chime_pinged(&already));
}

#[test]
fn control_an_echo_of_an_emit_the_client_already_ran_is_not_folded_again() {
    let already = std::iter::once(("Source".to_string(), "X".to_string())).collect();
    assert!(!chime_pinged(&already));
}

#[test]
fn the_folded_delivery_sees_the_server_emit_as_event() {
    let outcomes = relay_chain()
        .apply_server_response(&x_from_source(), &Default::default())
        .unwrap();
    let ring = outcomes
        .iter()
        .filter(|(t, _)| t == "Relay")
        .flat_map(|(_, r)| r.effect_results.iter())
        .find_map(|er| match &er.effect {
            orbital_core_base::effect_types::Effect::Emit { event, payload } if event == "RING" => {
                Some(serde_json::to_value(payload).unwrap())
            }
            _ => None,
        })
        .expect("Relay emits RING");
    assert_eq!(ring, json!({ "heardFrom": "Source", "heardEvent": "X" }));
}

/// Twin of `almadar-runtime/test/cascade-visit-key.test.ts` (one budget per dispatch): a trait's own
/// follow-up steps draw from `CROSS_TRAIT_CASCADE_CAP` like every other step; no per-trait cap below it.
fn self_looper(guarded: bool) -> RuntimeKernel {
    let mut arm = json!({
        "from": "running", "to": "running", "event": "TICK",
        "effects": [["set", "@entity.n", ["+", "@entity.n", 1]], ["emit", "TICK"]]
    });
    if guarded {
        arm["guard"] = json!(["<", "@entity.n", 30]);
    }
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [
                { "name": "id", "type": "string" },
                { "name": "n", "type": "number", "default": 0 }
            ] },
            "traits": [{
                "name": "Looper",
                "stateMachine": { "states": [{ "name": "running", "isInitial": true }], "transitions": [arm] }
            }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
}

fn looper_steps(kernel: &RuntimeKernel) -> usize {
    kernel
        .dispatch(
            "Looper",
            None,
            "TICK",
            serde_json::from_value(json!({})).unwrap(),
        )
        .unwrap()
        .iter()
        .filter(|(t, r)| t == "Looper" && r.executed)
        .count()
}

#[test]
fn a_traits_own_thirty_step_loop_runs_to_the_end() {
    assert_eq!(looper_steps(&self_looper(true)), 30);
}

#[test]
fn a_traits_own_endless_loop_stops_at_the_dispatch_budget() {
    // The requested step plus the dispatch's CROSS_TRAIT_CASCADE_CAP follow-ups (JS twin pins 2001).
    assert_eq!(
        looper_steps(&self_looper(false)),
        CROSS_TRAIT_CASCADE_CAP + 1
    );
}

/// JS twin: "edge: a trait's own endless loop stops at the dispatch budget and says so".
#[test]
fn a_traits_own_endless_loop_names_itself_as_truncated() {
    let kernel = self_looper(false);
    let outcomes = kernel
        .dispatch("Looper", None, "TICK", serde_json::from_value(json!({})).unwrap())
        .unwrap();
    let response = EventResponse::from_process_results(&outcomes, "Looper", "TICK");
    assert_eq!(response.cascade_truncated, Some(vec!["Looper".to_string()]));
    let bounded = self_looper(true);
    let outcomes = bounded
        .dispatch("Looper", None, "TICK", serde_json::from_value(json!({})).unwrap())
        .unwrap();
    assert_eq!(EventResponse::from_process_results(&outcomes, "Looper", "TICK").cascade_truncated, None);
}

/// L20: the serialized response of a two-hop cascade with sets and a render stays byte-identical across the entity-sharing change.
/// The row is only the fields the dispatch wrote (EXEC-DIST-7), in first-write order; the unwritten `title` default is not echoed.
#[test]
fn a_two_hop_cascade_response_serializes_byte_identically() {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "title", "type": "string", "default": "t" },
                { "name": "tags", "type": "array", "default": ["a"] },
                { "name": "hits", "type": "number", "default": 0 }
            ] },
            "traits": [{
                "name": "Hop", "linkedEntity": "Note",
                "listens": [{ "event": "HOP_NEXT", "triggers": "HOP", "source": { "kind": "trait", "trait": "Hop" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "HOP",
                        "guard": [">", "@payload.left", 0],
                        "effects": [
                            ["set", "@entity.hits", ["+", "@entity.hits", 1]],
                            ["set", "@entity.tags", ["array/append", "@entity.tags", "b"]],
                            ["render-ui", "main", { "type": "typography", "content": "@entity.title" }],
                            ["emit", "HOP_NEXT", { "left": ["-", "@payload.left", 1] }]
                        ] }]
                }
            }]
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema).unwrap();
    let payload: Value = serde_json::from_value(json!({ "left": 2 })).unwrap();
    let outcomes = kernel.dispatch("Hop", None, "HOP", payload).unwrap();
    let response = EventResponse::from_process_results(&outcomes, "Hop", "HOP");
    let bytes = serde_json::to_string(&response).unwrap();
    println!("TWO_HOP_RESPONSE {bytes}");
    assert_eq!(bytes, TWO_HOP_RESPONSE);
}

const TWO_HOP_RESPONSE: &str = r#"{"success":true,"transitioned":true,"states":{"Hop":"idle"},"emittedEvents":[{"event":"HOP_NEXT","payload":{"left":1.0},"source":{"trait":"Hop","dispatched":true}},{"event":"HOP_NEXT","payload":{"left":0.0},"source":{"trait":"Hop","dispatched":true}}],"entityByTrait":{"Hop":{"hits":2.0,"tags":["a","b","b"]}},"clientEffectsByTrait":[{"traitName":"Hop","effect":["render-ui","main",{"type":"typography","content":"t"}],"event":"HOP","fromState":"idle"},{"traitName":"Hop","effect":["render-ui","main",{"type":"typography","content":"t"}],"event":"HOP","fromState":"idle"},{"traitName":"Hop","effect":["render-ui","main",{"type":"typography","content":"t"}]}],"effectResults":[{"effect":"set","action":"@entity.hits","success":true},{"effect":"set","action":"@entity.tags","success":true},{"effect":"emit","success":true,"target":"HOP_NEXT"},{"effect":"set","action":"@entity.hits","success":true},{"effect":"set","action":"@entity.tags","success":true},{"effect":"emit","success":true,"target":"HOP_NEXT"}],"guardFailed":"Hop.HOP"}"#;
