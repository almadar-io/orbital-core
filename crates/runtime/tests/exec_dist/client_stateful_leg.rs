// A dispatch that delegated no effect still has a leg a stateful host is posted: the host holds the circuit state and must advance its copy (twin of `postServerLeg`'s `!carriesCircuitState` branch, `almadar-runtime/src/evaluation/client-role.ts`; ui `BrowserPlayground.wire`).

use orbital_core_runtime::kernel::RuntimeKernel;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::json;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "shop",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Shop",
            "entity": { "name": "Item", "persistence": "persistent", "collection": "items",
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Lister", "linkedEntity": "Item",
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "open" }],
                    "transitions": [
                        { "from": "idle", "to": "open", "event": "OPEN", "effects": [] },
                        { "from": "open", "to": "open", "event": "LOAD", "effects": [["fetch", "Item", {}]] }] } },
                { "name": "Badge", "linkedEntity": "Scratch", "dispatch": "client",
                  "stateMachine": {
                    "states": [{ "name": "off", "isInitial": true }, { "name": "on" }],
                    "transitions": [{ "from": "off", "to": "on", "event": "FLIP", "effects": [] }] } }
            ]
        }, {
            "name": "Scratchpad",
            "entity": { "name": "Scratch", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": []
        }]
    }))
    .unwrap();
    crate::support::TestKernel::client().register(schema)
}

fn go(t: &str, event: &str) -> orbital_core_runtime::kernel::ClientDispatch {
    kernel()
        .dispatch_with_server_leg(
            "Shop",
            t,
            None,
            event,
            serde_json::from_value::<Value>(json!({ "q": 1 })).unwrap(),
            Some("c1"),
        )
        .unwrap()
}

#[test]
fn a_server_backed_dispatch_with_no_delegated_effect_has_a_stateful_leg() {
    let d = go("Lister", "OPEN");
    assert!(d.server_legs.is_empty());
    let stateful = d.stateful_leg.expect("a plain leg");
    assert_eq!(stateful.leg.event, "OPEN");
    assert_eq!(stateful.leg.target_trait.as_deref(), Some("Lister"));
    assert_eq!(stateful.leg.source_trait.as_deref(), Some("Lister"));
    assert_eq!(stateful.leg.client_id.as_deref(), Some("c1"));
    let payload = serde_json::to_value(stateful.leg.payload).unwrap();
    assert_eq!(payload["q"].as_f64(), Some(1.0));
    assert_eq!(stateful.awaiting.len(), 1);
    assert_eq!(stateful.awaiting[0].trait_name, "Lister");
}

#[test]
fn control_a_dispatch_that_collected_a_leg_has_no_stateful_leg() {
    let k = kernel();
    k.dispatch_with_server_leg("Shop", "Lister", None, "OPEN", Value::Null, Some("c1"))
        .unwrap();
    let d = k
        .dispatch_with_server_leg("Shop", "Lister", None, "LOAD", Value::Null, Some("c1"))
        .unwrap();
    assert_eq!(d.server_legs.len(), 1);
    assert!(d.stateful_leg.is_none());
}

#[test]
fn control_a_client_only_dispatch_has_no_stateful_leg() {
    let d = go("Badge", "FLIP");
    assert!(d.server_legs.is_empty());
    assert!(d.stateful_leg.is_none());
}

#[test]
fn the_stateful_leg_addresses_the_entity_the_caller_named_and_none_otherwise() {
    let k = kernel();
    let named = k
        .dispatch_with_server_leg(
            "Shop",
            "Lister",
            Some("row-7"),
            "OPEN",
            Value::Null,
            Some("c1"),
        )
        .unwrap();
    assert_eq!(
        named.stateful_leg.unwrap().leg.entity_id.as_deref(),
        Some("row-7")
    );
    let anonymous = go("Lister", "OPEN");
    assert_eq!(anonymous.stateful_leg.unwrap().leg.entity_id, None);
}

#[test]
fn a_collected_leg_addresses_the_entity_the_caller_named_and_none_otherwise() {
    let open = |entity: Option<&str>| {
        let k = kernel();
        k.dispatch_with_server_leg("Shop", "Lister", entity, "OPEN", Value::Null, Some("c1"))
            .unwrap();
        k.dispatch_with_server_leg("Shop", "Lister", entity, "LOAD", Value::Null, Some("c1"))
            .unwrap()
    };
    assert_eq!(
        open(Some("row-7")).server_legs[0].entity_id.as_deref(),
        Some("row-7")
    );
    assert_eq!(open(None).server_legs[0].entity_id, None);
}
