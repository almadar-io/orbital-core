// The server role reports every executed hop of a dispatch, in order: the seed's, the same-trait
// cascade steps', each listener delivery's. JS twin: `almadar-runtime/test/server-transition-observer.test.ts`.

use orbital_core_runtime::effects::HopEffectStatus;
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema};
use serde_json::json;

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Bridge", "linkedEntity": "Item",
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "on" }],
                      "transitions": [
                          { "from": "idle", "to": "on", "event": "ENABLED",
                            "guard": ["==", 1, 1],
                            "effects": [["emit", "REGISTER", { "id": "a" }], ["emit", "REGISTER", { "id": "b" }], ["emit", "STATUS", { "text": "on" }], ["emit", "UNHEARD", {}]] }
                      ]
                  } },
                { "name": "Saver", "linkedEntity": "Item",
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "saved" }],
                      "transitions": [
                          { "from": "idle", "to": "saved", "event": "SAVE",
                            "effects": [["persist", "create", "Item", { "id": "i1" }]] }
                      ]
                  } },
                { "name": "Shell", "linkedEntity": "Item",
                  "listens": [
                      { "event": "REGISTER", "triggers": "REGISTER", "source": { "kind": "trait", "trait": "Bridge" } },
                      { "event": "STATUS", "triggers": "STATUS", "source": { "kind": "trait", "trait": "Bridge" } }
                  ],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [
                          { "from": "idle", "to": "idle", "event": "REGISTER" },
                          { "from": "idle", "to": "idle", "event": "STATUS" }
                      ]
                  } }
            ]
        }]
    }))
    .unwrap()
}

fn run(event: &str, target: &str) -> EventResponse {
    run_answering(
        event,
        target,
        IoAnswer::Row(Ok(serde_json::from_value(json!({ "id": "i1" })).unwrap())),
    )
}

fn run_answering(event: &str, target: &str, create: IoAnswer) -> EventResponse {
    let server = EmbeddedServer::new(schema()).unwrap();
    let request = EventRequest {
        event: event.to_string(),
        target_trait: Some(target.to_string()),
        ..Default::default()
    };
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| IoReply {
                        id: pending.id,
                        answer: match pending.request {
                            IoRequest::ReadRow { .. } => {
                                IoAnswer::Row(Ok(orbital_core_base::value::Value::Null))
                            }
                            IoRequest::CreateRow { .. } => create.clone(),
                            other => panic!("the fixture asks for {other:?}"),
                        },
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

fn hops(response: &EventResponse) -> Vec<String> {
    response
        .transitions
        .iter()
        .flatten()
        .map(|t| format!("{}.{}", t.trait_name, t.event))
        .collect()
}

#[test]
fn each_listener_delivery_of_one_fan_out_is_a_hop() {
    let response = run("ENABLED", "Bridge");
    assert_eq!(
        hops(&response),
        [
            "Bridge.ENABLED",
            "Shell.REGISTER",
            "Shell.REGISTER",
            "Shell.STATUS"
        ]
    );
}

#[test]
fn a_hop_carries_its_states_guard_and_effects() {
    let response = run("ENABLED", "Bridge");
    let seed = &response.transitions.as_ref().unwrap()[0];
    assert_eq!((seed.from.as_str(), seed.to.as_str()), ("idle", "on"));
    assert_eq!(seed.guard_result, Some(true));
    assert_eq!(seed.effects.len(), 4);
    assert_eq!(seed.effects[0].effect_type, "emit");
    assert_eq!(seed.effects[0].args[0].as_str(), Some("REGISTER"));
    let listener = &response.transitions.as_ref().unwrap()[1];
    assert_eq!(listener.guard_result, None);
    assert!(listener.effects.is_empty());
}

#[test]
fn control_an_emit_nobody_listens_to_is_no_hop() {
    let response = run("ENABLED", "Bridge");
    assert!(hops(&response).iter().all(|h| !h.ends_with(".UNHEARD")));
}

#[test]
fn control_an_event_nothing_handles_has_no_hops() {
    let response = run("NOPE", "Bridge");
    assert!(response.transitions.is_none());
}

#[test]
fn a_persist_that_ran_is_executed_in_its_hop() {
    let response = run("SAVE", "Saver");
    let hop = &response.transitions.as_ref().unwrap()[0];
    assert_eq!(hop.effects[0].effect_type, "persist");
    assert_eq!(hop.effects[0].status, HopEffectStatus::Executed);
    assert_eq!(hop.effects[0].error, None);
}

#[test]
fn a_persist_the_host_refused_is_failed_with_its_error() {
    let response = run_answering("SAVE", "Saver", IoAnswer::Row(Err("disk full".to_string())));
    let hop = &response.transitions.as_ref().unwrap()[0];
    assert_eq!(hop.effects[0].status, HopEffectStatus::Failed);
    assert_eq!(hop.effects[0].error.as_deref(), Some("disk full"));
}

// `[runtime]` rows live in the kernel's memory: reading or writing one is never a host ask (EXEC-TICK-5 and D-5 keep their
// rules, only where the rows live moved). The same `emit-fanout` program the benchmark runs made 6 `readRow` asks per `go`.
mod runtime_rows {
    use super::*;

    fn fanout(persistence: &str) -> OrbitalSchema {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../../tools/kernel-bench/fixtures/scenarios/emit-fanout.json");
        let mut doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json");
        doc["schema"]["orbitals"][0]["entity"]["persistence"] = json!(persistence);
        serde_json::from_value(doc["schema"].clone()).expect("schema")
    }

    fn go(schema: OrbitalSchema) -> (EventResponse, Vec<IoRequest>) {
        let server = EmbeddedServer::new(schema).unwrap();
        let request = EventRequest { event: "GO".to_string(), target_trait: Some("Src".to_string()), ..Default::default() };
        let mut seen = Vec::new();
        let mut progress = server.start("Main", request).unwrap();
        loop {
            match progress {
                ServerProgress::Done(response) => return (response, seen),
                ServerProgress::Await { flight, requests } => {
                    let replies = requests
                        .into_iter()
                        .map(|pending| {
                            seen.push(pending.request.clone());
                            IoReply { id: pending.id, answer: IoAnswer::Row(Ok(orbital_core_base::value::Value::Null)) }
                        })
                        .collect();
                    progress = server.resume(flight, replies).unwrap();
                }
            }
        }
    }

    #[test]
    fn a_dispatch_over_a_runtime_entity_makes_no_host_ask() {
        let (response, seen) = go(fanout("runtime"));
        assert!(seen.is_empty(), "{seen:?}");
        assert_eq!(response.states.get("Sink").map(String::as_str), Some("done"));
        assert_eq!(response.emitted_events.len(), 4);
    }

    #[test]
    fn control_a_persistent_entity_still_asks_the_host_for_its_row_but_only_once_per_dispatch() {
        let (response, seen) = go(fanout("persistent"));
        let oracle = adapter_calls()["emitFanoutPersistent"]["frameReads"].as_u64().expect("oracle") as usize;
        assert_eq!(seen.len(), oracle, "{seen:?}");
        assert!(seen.iter().all(|r| matches!(r, IoRequest::ReadRow { .. })), "{seen:?}");
        assert_eq!(response.states.get("Sink").map(String::as_str), Some("done"));
        assert_eq!(response.emitted_events.len(), 4);
    }

    fn adapter_calls() -> serde_json::Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../../packages/almadar-runtime/test/fixtures/adapter-calls.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("oracle")).expect("oracle json")
    }

    #[test]
    fn a_runtime_create_is_kept_in_the_kernel_and_asks_nothing() {
        let server = EmbeddedServer::new(schema_with_runtime_saver()).unwrap();
        let request = EventRequest { event: "SAVE".to_string(), target_trait: Some("Saver".to_string()), ..Default::default() };
        let ServerProgress::Done(response) = server.start("Main", request).unwrap() else { panic!("a runtime create asked the host") };
        assert_eq!(response.states.get("Saver").map(String::as_str), Some("saved"));
        let hop = &response.transitions.as_ref().unwrap()[0];
        assert_eq!(hop.effects[0].status, HopEffectStatus::Executed);
    }

    fn schema_with_runtime_saver() -> OrbitalSchema {
        let mut doc = serde_json::to_value(schema()).unwrap();
        doc["orbitals"][0]["entity"]["persistence"] = json!("runtime");
        serde_json::from_value(doc).unwrap()
    }
}

mod whole_orbital_states {
    use super::*;

    #[test]
    fn a_trait_that_has_not_mounted_reports_its_initial_state() {
        let server = EmbeddedServer::new(schema()).unwrap();
        for name in ["Bridge", "Saver", "Shell"] {
            assert_eq!(server.get_state(name, None), "idle", "{name} holds its initial state before anything touched it");
        }
    }

    #[test]
    fn control_an_unknown_trait_has_no_state() {
        let server = EmbeddedServer::new(schema()).unwrap();
        assert_eq!(server.get_state("NoSuchTrait", None), "");
    }

    #[test]
    fn control_a_dispatched_trait_reports_its_new_state_and_the_rest_stay_initial() {
        let response = run("SAVE", "Saver");
        assert_eq!(response.states.get("Saver").map(String::as_str), Some("saved"));
        assert_eq!(response.states.get("Bridge").map(String::as_str), Some("idle"));
        assert_eq!(response.states.get("Shell").map(String::as_str), Some("idle"));
    }
}
