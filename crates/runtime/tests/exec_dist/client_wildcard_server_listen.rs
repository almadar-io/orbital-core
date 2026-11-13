// EXEC-DIST-2: a `[client]` front that forwards an ask to a worker only the host runs (`*.FWD`, listed in
// `_metadata.serverListens` with an `any` source) leaves the forwarding step as a leg, also when the front was reached
// by another client trait's emit (std-kflow's dashboard → knowledge-paths-front → knowledge-paths).

use orbital_core_runtime::kernel::RuntimeKernel;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::json;

fn kernel(server_source: serde_json::Value) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1,
        "_metadata": { "serverListens": [{ "orbital": "Paths", "listen": {
            "event": "FWD_LIST", "triggers": "LIST", "source": server_source
        } }] },
        "orbitals": [{
            "name": "Dash",
            "entity": { "name": "Board", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Grid", "linkedEntity": "Board", "dispatch": "client",
                "stateMachine": {
                    "states": [{ "name": "loading", "isInitial": true }],
                    "transitions": [{ "from": "loading", "to": "loading", "event": "LOAD", "effects": [["emit", "ASK_LIST", { "key": "k1" }]] }]
                },
                "emits": [{ "event": "ASK_LIST", "scope": "external" }]
            }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Grid" }, { "ref": "Front" }] }]
        }, {
            "name": "PathFront",
            "entity": { "name": "FrontRun", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Front", "linkedEntity": "FrontRun", "dispatch": "client",
                "listens": [{ "event": "ASK_LIST", "triggers": "ASK_LIST", "source": { "kind": "any" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
                    "transitions": [
                        { "from": "idle", "to": "busy", "event": "ASK_LIST", "effects": [["emit", "FWD_LIST", {}]] },
                        { "from": "idle", "to": "idle", "event": "POKE", "effects": [["emit", "FWD_LIST", {}]] }
                    ]
                },
                "emits": [{ "event": "FWD_LIST", "scope": "external" }]
            }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::client().register(schema)
}

fn any() -> serde_json::Value {
    json!({ "kind": "any" })
}

#[test]
fn a_client_front_seeded_directly_leaves_its_forward_as_a_leg() {
    let d = kernel(any())
        .dispatch_with_server_leg("PathFront", "Front", None, "POKE", Value::Null, Some("c1"))
        .unwrap();
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Front"));
}

#[test]
fn a_client_front_reached_by_another_client_traits_emit_leaves_its_forward_as_a_leg() {
    let d = kernel(any())
        .dispatch_with_server_leg("Dash", "Grid", None, "LOAD", Value::Null, Some("c1"))
        .unwrap();
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Front"));
    assert_eq!(d.server_legs[0].event, "ASK_LIST");
}

#[test]
fn control_a_forward_no_server_orbital_hears_posts_nothing() {
    let d = kernel(json!({ "kind": "trait", "trait": "Elsewhere" }))
        .dispatch_with_server_leg("Dash", "Grid", None, "LOAD", Value::Null, Some("c1"))
        .unwrap();
    assert!(d.server_legs.is_empty(), "{:?}", d.server_legs);
}
