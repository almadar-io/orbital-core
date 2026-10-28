// A keyless `http` (no `auth` key) is client work: a `[client]` trait carrying one stays client-only and owes the host
// its leg, which the host runs in the browser store (`docs/Almadar_Embedded_Kernel.md` H.7, P12). One with `auth`
// stays server work; the mode is the trait's declaration, never inferred. Twin of `almadar-runtime` `test/http-browser-ports.test.ts`.

use orbital_core_base::dispatch_mode::DispatchMode;
use orbital_core_runtime::kernel::RuntimeKernel;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::json;

const ORIGIN: &str = "https://api.example.com";

fn kernel(effect: serde_json::Value, dispatch: Option<&str>) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "feed", "egress": [ORIGIN],
        "typeContractVersion": 1, "orbitals": [{
            "name": "Feed",
            "entity": { "name": "Row", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Loader", "linkedEntity": "Row", "dispatch": dispatch,
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [effect] }] } }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::client().register(schema)
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
fn a_client_trait_with_a_keyless_http_is_client_only_and_owes_the_host_its_leg() {
    let d = init(&kernel(keyless(), Some("client")));
    assert_eq!(d.mode, DispatchMode::HybridClientOnly);
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].event, "INIT");
}

#[test]
fn control_an_undeclared_trait_on_a_runtime_entity_awaits_the_server_whatever_its_http() {
    for effect in [keyless(), keyed()] {
        let d = init(&kernel(effect, None));
        assert_eq!(d.mode, DispatchMode::PersistedAwaited);
    }
}

#[test]
fn control_an_optimistic_trait_with_an_authenticated_http_awaits_the_server_optimistically() {
    let d = init(&kernel(keyed(), Some("optimistic")));
    assert_eq!(d.mode, DispatchMode::RuntimeOptimistic);
    assert_eq!(d.server_legs.len(), 1);
}

#[test]
fn control_a_client_trait_with_an_authenticated_http_ships_no_leg() {
    let d = init(&kernel(keyed(), Some("client")));
    assert_eq!(d.mode, DispatchMode::HybridClientOnly);
    assert!(d.server_legs.is_empty(), "a server-only effect in a client trait is dropped, never shipped as a browser leg");
}
