// A host's state and row for a client-only trait never overwrite the client's (twin of `withoutClientOnlyRuns`, `almadar-runtime/src/evaluation/client-role.ts`; ui `BrowserPlayground.cross-orbital-answer`).

use orbital_core::runtime::{
    already_delivered_from, EntitySource, InMemoryEntityCache, KernelConfig, RuntimeKernel,
};
use orbital_core::ServerLegCollector;
use orbital_core::{EffectDelegate, EventResponse, OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "split",
        "orbitals": [
            { "name": "SearchOrbital",
              "entity": { "name": "Query", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
              "traits": [{ "name": "Searcher", "linkedEntity": "Query",
                  "emits": [{ "event": "REQUESTED", "scope": "external" }],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "searching" }],
                      "transitions": [{ "from": "idle", "to": "searching", "event": "GO", "effects": [["emit", "REQUESTED", {}]] }] } }] },
            { "name": "TaskOrbital",
              "entity": { "name": "Task", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
              "traits": [{ "name": "Responder", "linkedEntity": "Task",
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "answering" }],
                      "transitions": [{ "from": "idle", "to": "answering", "event": "ASK", "effects": [["fetch", "Task", {}]] }] } }] }
        ]
    }))
    .unwrap();
    let collector = Arc::new(ServerLegCollector::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig {
            environment: RuntimeEnvironment::Client,
            delegate: Some(collector as Arc<dyn EffectDelegate>),
            ..KernelConfig::default()
        },
    );
    k.register_schema(schema).unwrap();
    k
}

fn host_answer() -> EventResponse {
    EventResponse {
        success: true,
        transitioned: true,
        states: HashMap::from([
            ("Responder".to_string(), "answering".to_string()),
            ("Searcher".to_string(), "idle".to_string()),
        ]),
        ..EventResponse::default()
    }
}

fn host_row() -> EventResponse {
    EventResponse {
        success: true,
        entity_by_trait: Some(HashMap::from([(
            "Searcher".to_string(),
            serde_json::from_value::<Value>(json!({ "id": "q1", "text": "from the host" }))
                .unwrap(),
        )])),
        ..EventResponse::default()
    }
}

#[test]
fn a_host_state_for_a_client_only_trait_is_ignored() {
    let k = kernel();
    let d = k
        .dispatch_with_server_leg("split", "Searcher", None, "GO", Value::Null, None)
        .unwrap();
    k.apply_server_response(&host_answer(), &already_delivered_from(&d))
        .unwrap();
    let state = |t: &str| k.get_state(t, None).map(|s| s.current_state);
    assert_eq!(state("Searcher").as_deref(), Some("searching"));
}

#[test]
fn a_host_state_for_a_trait_that_awaits_the_host_is_still_folded() {
    let k = kernel();
    let d = k
        .dispatch_with_server_leg("split", "Searcher", None, "GO", Value::Null, None)
        .unwrap();
    k.apply_server_response(&host_answer(), &already_delivered_from(&d))
        .unwrap();
    let state = k.get_state("Responder", None).map(|s| s.current_state);
    assert_eq!(state.as_deref(), Some("answering"));
}

#[test]
fn a_host_row_for_a_client_only_trait_is_ignored() {
    let k = kernel();
    k.apply_server_response(&host_row(), &Default::default())
        .unwrap();
    assert_eq!(k.snapshot_trait("Searcher", Some("q1")).entity, Value::Null);
}

#[test]
fn a_host_row_for_a_trait_that_awaits_the_host_is_still_folded() {
    let k = kernel();
    let row =
        serde_json::from_value::<Value>(json!({ "id": "t1", "text": "from the host" })).unwrap();
    let response = EventResponse {
        success: true,
        entity_by_trait: Some(HashMap::from([("Responder".to_string(), row.clone())])),
        ..EventResponse::default()
    };
    k.apply_server_response(&response, &Default::default())
        .unwrap();
    assert_eq!(k.snapshot_trait("Responder", Some("t1")).entity, row);
}
