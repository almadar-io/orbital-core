//! EXEC-DIST conformance: where the kernel says a leg runs.


use orbital_core_base::value::Value;
use orbital_core_runtime::effects::response::LegResidence;
use orbital_core_runtime::kernel::ClientDispatch;
use serde_json::{json, Value as Json};
use crate::support::TestKernel;

fn dispatch(mode: Option<&str>, effects: Json) -> ClientDispatch {
    let kernel = TestKernel::client().build(json!({
        "name": "books",
        "typeContractVersion": 1, "orbitals": [
            { "name": "Books",
              "entity": { "name": "Invoice", "persistence": "persistent", "collection": "invoices", "browser": true,
                  "fields": [{ "name": "id", "type": "string" }] },
              "traits": [{ "name": "InvoiceList", "linkedEntity": "Invoice", "dispatch": mode,
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": effects }] } }] },
            { "name": "Ledger",
              "entity": { "name": "Entry", "persistence": "persistent", "collection": "entries",
                  "fields": [{ "name": "id", "type": "string" }] },
              "traits": [] }
        ]
    }));
    kernel.dispatch_with_server_leg("Books", "InvoiceList", None, "INIT", Value::Null, Some("c1")).unwrap()
}

#[test]
fn exec_dist_6_a_leg_touching_only_browser_stored_rows_runs_in_the_browser() {
    let d = dispatch(Some("client"), json!([["fetch", "Invoice", {}]]));
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].residence, Some(LegResidence::Browser));
}

#[test]
fn exec_dist_6_a_leg_with_server_rows_runs_on_the_server() {
    let d = dispatch(None, json!([["fetch", "Entry", {}]]));
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].residence, Some(LegResidence::Server));
}

#[test]
fn exec_dist_6_a_dispatch_mixing_browser_rows_and_server_work_is_refused_and_builds_no_leg() {
    let d = dispatch(None, json!([["fetch", "Invoice", {}], ["fetch", "Entry", {}]]));
    assert!(!d.response.success);
    let error = d.response.error.clone().unwrap_or_default();
    assert!(error.contains("InvoiceList") && error.contains("INIT"), "the refusal names the trait and the event: {error}");
    assert!(d.server_legs.is_empty(), "{:?}", d.server_legs);
}

fn mode_of(declared: Option<&str>, entity_persistence: &str, collection: Option<&str>) -> orbital_core_runtime::DispatchMode {
    let mut entity = json!({ "name": "Row", "persistence": entity_persistence, "fields": [{ "name": "id", "type": "string" }] });
    if let Some(c) = collection {
        entity["collection"] = json!(c);
    }
    let kernel = TestKernel::client().build(json!({
        "name": "modes",
        "typeContractVersion": 1, "orbitals": [{ "name": "App", "entity": entity,
            "traits": [{ "name": "Probe", "linkedEntity": "Row", "dispatch": declared,
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [] }] } }] }]
    }));
    kernel
        .dispatch_with_server_leg("App", "Probe", None, "GO", Value::Null, Some("c1"))
        .unwrap()
        .mode
}

/// EXEC-DIST-1: the dispatch mode is the trait's declared `dispatch`; nothing is derived from the linked entity or the effect row.
#[test]
fn exec_dist_1_the_mode_is_the_declared_dispatch_and_never_derived_from_the_entity() {
    use orbital_core_runtime::DispatchMode;
    assert_eq!(mode_of(Some("client"), "persistent", Some("rows")), DispatchMode::HybridClientOnly, "client on a persistent entity");
    assert_eq!(mode_of(Some("optimistic"), "runtime", None), DispatchMode::RuntimeOptimistic, "optimistic on a runtime entity");
    assert_eq!(mode_of(Some("server"), "runtime", None), DispatchMode::PersistedAwaited, "server on a runtime entity");
    assert_eq!(mode_of(None, "runtime", None), DispatchMode::PersistedAwaited, "control: no declaration is server-awaited even on a runtime entity");
    assert_eq!(mode_of(None, "persistent", Some("rows")), DispatchMode::PersistedAwaited, "control: no declaration is server-awaited on a persistent entity");
}

struct Answer(orbital_core_runtime::effects::response::EventResponse);

impl orbital_core_runtime::client::bridge::ServerBridge for Answer {
    fn send_event_with_options(
        &self,
        _orbital: &str,
        _event: &str,
        _entity_id: Option<&str>,
        _trait_name: Option<&str>,
        _payload: Value,
    ) -> Result<
        orbital_core_runtime::effects::response::EventResponse,
        orbital_core_runtime::client::http::BridgeError,
    > {
        Ok(self.0.clone())
    }
}

fn optimistic_state_after(reply: orbital_core_runtime::effects::response::EventResponse) -> (bool, String) {
    use orbital_core_base::schema::types::OrbitalSchema;
    use orbital_core_runtime::client::ClientKernel;
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{ "name": "Main",
            "entity": { "name": "Cursor", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "x", "type": "number" }] },
            "traits": [{ "name": "Move", "linkedEntity": "Cursor", "dispatch": "optimistic",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "moved" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "moved", "event": "MOVE", "effects": [
                            ["set", "@entity.x", 5],
                            ["call-service", "SyncService", "move", {}]
                        ] }
                    ]
                } }] }]
    }))
    .unwrap();
    let bridge: std::sync::Arc<dyn orbital_core_runtime::client::bridge::ServerBridge> = std::sync::Arc::new(Answer(reply));
    let kernel = ClientKernel::new(schema, Some(bridge)).unwrap();
    kernel.dispatch("Move", None, "INIT", Value::Null).unwrap();
    let response = kernel.dispatch("Move", None, "MOVE", Value::Null).unwrap();
    (response.success, kernel.snapshot_trait("Move", None).state)
}

/// EXEC-DIST-3: an optimistic dispatch commits locally first; a rejected leg restores the pre-dispatch snapshot and reports the rejection.
#[test]
fn exec_dist_3_a_rejected_optimistic_leg_restores_the_pre_dispatch_snapshot() {
    use orbital_core_runtime::effects::response::EventResponse;
    let (success, state) = optimistic_state_after(EventResponse::error("sync rejected"));
    assert!(!success, "a rejected leg reports the failure");
    assert_eq!(state, "idle", "rejected: the pre-dispatch snapshot is restored");
    let (success, state) = optimistic_state_after(EventResponse { success: true, ..Default::default() });
    assert!(success, "control: an accepted leg reports success");
    assert_eq!(state, "moved", "control: accepted, the local commit stands");
}

/// EXEC-DIST-4: the response is folded through the same worklist, skipping every (trait, event) the client already delivered; a client-only trait's state is the client's.
#[test]
fn exec_dist_4_the_fold_skips_what_the_client_delivered_and_ignores_a_hosts_state_for_a_client_only_trait() {
    use orbital_core_base::bus_event::{BusEventSource, EmittedEvent};
    use orbital_core_runtime::effects::response::EventResponse;
    use std::collections::{HashMap, HashSet};
    let kernel = || TestKernel::client().build(json!({
        "name": "fold",
        "typeContractVersion": 1, "orbitals": [{ "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Emitter", "linkedEntity": "Item",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] } },
                { "name": "Listener", "linkedEntity": "Item",
                  "listens": [{ "event": "PING", "triggers": "HEARD", "source": { "kind": "trait", "trait": "Emitter" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                      "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD" }] } }
            ]
        }]
    }));
    let echo = EventResponse {
        success: true,
        emitted_events: vec![EmittedEvent {
            event: "PING".to_string(),
            payload: None,
            source: Some(BusEventSource {
                orbital: Some("Main".to_string()),
                r#trait: Some("Emitter".to_string()),
                ..Default::default()
            }),
        }],
        ..Default::default()
    };
    let ran_listener = |delivered: HashSet<(String, String)>| {
        kernel()
            .apply_server_response(&echo, &delivered)
            .unwrap()
            .iter()
            .any(|(t, r)| t == "Listener" && r.executed)
    };
    assert!(
        !ran_listener(HashSet::from([("Emitter".to_string(), "PING".to_string())])),
        "an echo the client already delivered locally is skipped"
    );
    assert!(ran_listener(HashSet::new()), "control: an echo not delivered locally cascades through the same worklist");

    let split = || TestKernel::client().build(json!({
        "name": "split",
        "typeContractVersion": 1, "orbitals": [{ "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Local", "linkedEntity": "Item", "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
                      "transitions": [{ "from": "idle", "to": "busy", "event": "GO" }] } },
                { "name": "Remote", "linkedEntity": "Item", "dispatch": "optimistic",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "answering" }],
                      "transitions": [{ "from": "idle", "to": "answering", "event": "ASK", "effects": [["fetch", "Item", {}]] }] } }
            ]
        }]
    }));
    let k = split();
    k.dispatch_with_server_leg("Main", "Local", None, "GO", Value::Null, None).unwrap();
    let d = k.dispatch_with_server_leg("Main", "Remote", None, "ASK", Value::Null, None).unwrap();
    let host = EventResponse {
        success: true,
        transitioned: true,
        states: HashMap::from([("Local".to_string(), "idle".to_string()), ("Remote".to_string(), "answering".to_string())]),
        ..Default::default()
    };
    k.apply_server_response(&host, &orbital_core_runtime::kernel::already_delivered_from(&d)).unwrap();
    let state = |t: &str| k.get_state(t, None).map(|s| s.current_state);
    assert_eq!(state("Local").as_deref(), Some("busy"), "a host's state for a client-only trait is ignored");
    assert_eq!(state("Remote").as_deref(), Some("answering"), "control: a trait that awaits the host takes its state");
}
