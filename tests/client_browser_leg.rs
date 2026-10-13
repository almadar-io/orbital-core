// A client-only trait whose data effects all live in the browser store still owes the host its leg: the host runs it against the store (twin of `seedBrowserLeg`, `almadar-runtime/src/evaluation/client-role.ts`; ui `OrbPreview.browser-stored-entity`).

use orbital_core::runtime::dispatch_mode::DispatchMode;
use orbital_core::runtime::{EntitySource, InMemoryEntityCache, KernelConfig, RuntimeKernel};
use orbital_core::ServerLegCollector;
use orbital_core::{EffectDelegate, OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::json;
use std::sync::Arc;

fn kernel(effects: serde_json::Value, local: bool) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "books",
        "orbitals": [{
            "name": "Books",
            "entity": { "name": "Invoice", "persistence": "persistent", "collection": "invoices", "local": local,
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "InvoiceList", "linkedEntity": "Invoice",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": effects }] } }]
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
    k
}

fn init(k: &RuntimeKernel) -> orbital_core::runtime::ClientDispatch {
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
