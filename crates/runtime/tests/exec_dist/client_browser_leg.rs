// A client-only trait whose data effects all live in the browser store still owes the host its leg: the host runs it against the store (twin of `seedBrowserLeg`, `almadar-runtime/src/evaluation/client-role.ts`; ui `OrbPreview.browser-stored-entity`).

use orbital_core_base::dispatch_mode::DispatchMode;
use orbital_core_runtime::effects::response::LegResidence;
use orbital_core_runtime::ServerLegCollector;
use serde_json::json;
use std::sync::Arc;
use {
    orbital_core_base::effect_types::RuntimeEnvironment,
    orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value,
    orbital_core_runtime::effects::executor::EffectDelegate,
};
use {
    orbital_core_runtime::config::KernelConfig,
    orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache},
    orbital_core_runtime::kernel::RuntimeKernel,
};

fn kernel(effects: serde_json::Value, local: bool) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "books",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Books",
            "entity": { "name": "Invoice", "persistence": "persistent", "collection": "invoices", "browser": local,
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "InvoiceList", "linkedEntity": "Invoice", "dispatch": if local { json!("client") } else { json!(null) },
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": effects }] } }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::client().register(schema)
}

fn init(k: &RuntimeKernel) -> orbital_core_runtime::kernel::ClientDispatch {
    k.dispatch_with_server_leg(
        "Books",
        "InvoiceList",
        None,
        "INIT",
        Value::Null,
        Some("c1"),
    )
    .unwrap()
}

#[test]
fn a_client_only_fetch_of_a_browser_stored_entity_owes_the_host_its_leg() {
    let d = init(&kernel(json!([["fetch", "Invoice", {}]]), true));
    assert_eq!(d.mode, DispatchMode::HybridClientOnly);
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].event, "INIT");
    assert_eq!(
        d.server_legs[0].target_trait.as_deref(),
        Some("InvoiceList")
    );
}

#[test]
fn control_a_client_only_trait_with_no_data_effect_posts_nothing() {
    let d = init(&kernel(
        json!([["render-ui", "main", { "type": "typography", "content": "x" }]]),
        true,
    ));
    assert_eq!(d.mode, DispatchMode::HybridClientOnly);
    assert!(d.server_legs.is_empty());
}

#[test]
fn control_a_server_entity_fetch_still_posts_its_leg() {
    let d = init(&kernel(json!([["fetch", "Invoice", {}]]), false));
    assert_eq!(d.mode, DispatchMode::PersistedAwaited);
    assert_eq!(d.server_legs.len(), 1);
}

// The host ran the browser leg's cascade on a client-only trait (`LOADED` after `INIT`): its reported state continues from
// the state the leg carried, so the client adopts it. A host that reports a client-only trait it never ran keeps no say.
fn cascade_kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "books",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Books",
            "entity": { "name": "Invoice", "persistence": "persistent", "collection": "invoices", "browser": true,
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "InvoiceList", "linkedEntity": "Invoice", "dispatch": "client",
                "stateMachine": {
                    "states": [{ "name": "loading", "isInitial": true }, { "name": "browsing" }],
                    "transitions": [
                        { "from": "loading", "to": "loading", "event": "INIT", "effects": [["fetch", "Invoice", { "emit": { "success": "LOADED" } }]] },
                        { "from": "loading", "to": "browsing", "event": "LOADED", "effects": [] }
                    ] } }]
        }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig {
            environment: RuntimeEnvironment::Client,
            delegate: Some(Arc::new(ServerLegCollector::new()) as Arc<dyn EffectDelegate>),
            ..KernelConfig::default()
        },
    );
    k.register_schema(schema).unwrap();
    k.dispatch_with_server_leg(
        "Books",
        "InvoiceList",
        None,
        "INIT",
        Value::Null,
        Some("c1"),
    )
    .unwrap();
    k
}

fn host_answer(
    transitions: serde_json::Value,
) -> orbital_core_runtime::effects::response::EventResponse {
    serde_json::from_value(json!({
        "success": true, "transitioned": true,
        "states": { "InvoiceList": "browsing" },
        "emittedEvents": [{ "event": "LOADED", "payload": { "data": [] },
            "source": { "orbital": "Books", "trait": "InvoiceList", "dispatched": true } }],
        "transitions": transitions
    }))
    .unwrap()
}

fn state_of(k: &RuntimeKernel) -> Option<String> {
    k.get_state("InvoiceList", None).map(|s| s.current_state)
}

#[test]
fn a_client_only_trait_adopts_the_state_the_host_ran_it_to() {
    let k = cascade_kernel();
    assert_eq!(state_of(&k).as_deref(), Some("loading"));
    k.apply_server_response(
        &host_answer(json!([
            { "trait": "InvoiceList", "from": "loading", "to": "loading", "event": "INIT", "effects": [] },
            { "trait": "InvoiceList", "from": "loading", "to": "browsing", "event": "LOADED", "effects": [] }
        ])),
        &Default::default(),
    )
    .unwrap();
    assert_eq!(state_of(&k).as_deref(), Some("browsing"));
}

#[test]
fn control_a_client_only_trait_the_host_did_not_run_keeps_its_own_state() {
    let k = cascade_kernel();
    k.apply_server_response(&host_answer(json!([])), &Default::default())
        .unwrap();
    assert_eq!(state_of(&k).as_deref(), Some("loading"));
}

fn batch(entity: &str) -> serde_json::Value {
    json!([["persist", "batch", [["create", entity, { "id": "a" }]]]])
}

#[test]
fn a_persist_batch_on_a_browser_stored_entity_owes_the_host_a_browser_leg() {
    let d = init(&kernel(batch("Invoice"), true));
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].residence, Some(LegResidence::Browser));
    assert_eq!(d.response.error, None);
}

#[test]
fn control_a_persist_batch_on_a_server_entity_posts_a_server_leg() {
    let d = init(&kernel(batch("Invoice"), false));
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].residence, Some(LegResidence::Server));
}

fn two_entity_kernel(effects: serde_json::Value) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "books",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Books",
            "entity": { "name": "Invoice", "persistence": "persistent", "collection": "invoices", "browser": true,
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "InvoiceList", "linkedEntity": "Invoice",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": effects }] } }]
        }, {
            "name": "Notes",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes",
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": []
        }]
    }))
    .unwrap();
    crate::support::TestKernel::client().register(schema)
}

#[test]
fn a_batch_naming_a_browser_and_a_server_entity_is_refused_as_mixed() {
    let ops = json!([["persist", "batch", [["create", "Invoice", { "id": "a" }], ["create", "Note", { "id": "b" }]]]]);
    let d = init(&two_entity_kernel(ops));
    assert!(d.server_legs.is_empty());
    assert!(d.response.error.as_deref().is_some_and(|e| e.starts_with("residence: one dispatch of InvoiceList on INIT")));
}

#[test]
fn a_browser_batch_beside_a_server_write_is_refused_as_mixed() {
    let effects = json!([
        ["persist", "batch", [["create", "Invoice", { "id": "a" }]]],
        ["persist", "create", "Note", { "id": "b" }]
    ]);
    let d = init(&two_entity_kernel(effects));
    assert!(d.server_legs.is_empty());
    assert!(d.response.error.as_deref().is_some_and(|e| e.starts_with("residence:")));
}
