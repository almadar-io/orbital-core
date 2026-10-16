// A keyless `http` (no `auth` key) is client work: a `[runtime]` trait carrying one stays client-only and owes the host
// its leg, which the host runs in the browser store (`docs/Almadar_Embedded_Kernel.md` H.7, P12). One with `auth`
// stays server work. Twin of `almadar-runtime` `test/http-browser-ports.test.ts`.

use orbital_core_base::dispatch_mode::DispatchMode;
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig, orbital_core_runtime::kernel::RuntimeKernel};
use orbital_core_runtime::ServerLegCollector;
use {orbital_core_runtime::effects::executor::EffectDelegate, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::sync::Arc;

const ORIGIN: &str = "https://api.example.com";

fn kernel(effect: serde_json::Value, local: bool) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "feed", "egress": [ORIGIN],
        "orbitals": [{
            "name": "Feed",
            "entity": { "name": "Row", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Loader", "linkedEntity": "Row", "local": local,
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [effect] }] } }]
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

fn init(k: &RuntimeKernel) -> orbital_core_runtime::kernel::ClientDispatch {
    k.dispatch_with_server_leg("Feed", "Loader", None, "INIT", Value::Null, Some("c1"))
        .unwrap()
}

fn keyless() -> serde_json::Value {
    json!(["http", "GET", ORIGIN, "/x", { "query": { "q": "a" } }])
}

fn keyed() -> serde_json::Value {
    json!(["http", "GET", ORIGIN, "/x", { "auth": { "scheme": "bearer", "credential": "KEY" } }])
}

#[test]
fn a_runtime_trait_with_a_keyless_http_is_client_only_and_owes_the_host_its_leg() {
    let d = init(&kernel(keyless(), false));
    assert_eq!(d.mode, DispatchMode::HybridClientOnly);
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].event, "INIT");
}

#[test]
fn control_a_runtime_trait_with_an_authenticated_http_awaits_the_server_optimistically() {
    let d = init(&kernel(keyed(), false));
    assert_eq!(d.mode, DispatchMode::RuntimeOptimistic);
    assert_eq!(d.server_legs.len(), 1);
}

#[test]
fn control_a_local_trait_with_an_authenticated_http_ships_no_leg() {
    let d = init(&kernel(keyed(), true));
    assert_eq!(d.mode, DispatchMode::HybridClientOnly);
    assert!(d.server_legs.is_empty(), "a server-only effect in a local trait is dropped, never shipped as a browser leg");
}

#[test]
fn a_local_trait_with_a_keyless_http_ships_its_leg_to_the_browser() {
    let d = init(&kernel(keyless(), true));
    assert_eq!(d.mode, DispatchMode::HybridClientOnly);
    assert_eq!(d.server_legs.len(), 1);
}
