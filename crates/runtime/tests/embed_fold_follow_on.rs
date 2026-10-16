// A fold can run an arm that owes the host a leg (a browser-stored refetch after a persist's success): the fold hands that leg back instead of losing it (twin of `applyOrbitalEventResponse`'s own continuations, `almadar-runtime/src/evaluation/client-role.ts`; ui `OrbPreview.browser-stored-entity`).

use orbital_core_runtime::embed::EmbeddedKernel;
use {orbital_core_base::bus_event::{BusEventSource, EmittedEvent}, orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::json;
use std::collections::HashSet;

fn kernel(local: bool) -> EmbeddedKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "books",
        "orbitals": [{
            "name": "Books",
            "entity": { "name": "Invoice", "persistence": "persistent", "collection": "invoices", "local": local,
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "InvoiceList", "linkedEntity": "Invoice",
                  "emits": [{ "event": "ADDED", "scope": "external" }],
                  "stateMachine": {
                    "states": [{ "name": "browsing", "isInitial": true }, { "name": "loading" }],
                    "transitions": [
                        { "from": "browsing", "to": "browsing", "event": "ADD", "effects": [["persist", "create", "Invoice", {}, { "emit": { "success": "ADDED" } }]] },
                        { "from": "browsing", "to": "loading", "event": "ADDED", "effects": [["fetch", "Invoice", {}]] },
                        { "from": "loading", "to": "loading", "event": "NOTE", "effects": [] }] } },
                { "name": "Tally", "linkedEntity": "Invoice",
                  "listens": [{ "event": "ADDED", "source": { "kind": "trait", "trait": "InvoiceList" }, "triggers": "COUNT" }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "counting" }],
                    "transitions": [{ "from": "idle", "to": "counting", "event": "COUNT", "effects": [["fetch", "Invoice", {}]] }] } }
            ]
        }, {
            "name": "Desk",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Badge", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "off", "isInitial": true }, { "name": "on" }],
                    "transitions": [{ "from": "off", "to": "on", "event": "FLIP", "effects": [] }] } }]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "c1").unwrap()
}

fn added() -> EventResponse {
    EventResponse {
        success: true,
        transitioned: true,
        emitted_events: vec![EmittedEvent {
            event: "ADDED".to_string(),
            payload: None,
            source: Some(BusEventSource {
                r#trait: Some("InvoiceList".to_string()),
                ..Default::default()
            }),
        }],
        ..Default::default()
    }
}

fn fold(k: &EmbeddedKernel) -> orbital_core_runtime::embed::wire::AppliedWire {
    k.apply_server_response(&added(), &HashSet::new()).unwrap()
}

#[test]
fn a_trait_continuing_its_own_emit_with_browser_data_gets_its_leg_back() {
    let applied = fold(&kernel(true));
    assert_eq!(
        applied.server_legs.len(),
        2,
        "the own continuation and the browser-data listener"
    );
    let own = applied
        .server_legs
        .iter()
        .find(|r| r.leg.target_trait.as_deref() == Some("InvoiceList"))
        .expect("the trait's own leg");
    assert_eq!(own.orbital, "Books");
    assert_eq!(own.leg.event, "ADDED");
    assert_eq!(own.leg.client_id.as_deref(), Some("c1"));
}

#[test]
fn a_trait_continuing_its_own_emit_with_server_data_also_gets_its_leg_back() {
    let applied = fold(&kernel(false));
    let own: Vec<_> = applied
        .server_legs
        .iter()
        .filter(|r| r.leg.target_trait.as_deref() == Some("InvoiceList"))
        .collect();
    assert_eq!(own.len(), 1);
}

#[test]
fn control_a_listener_whose_data_lives_on_the_server_is_not_posted_from_a_fold() {
    let applied = fold(&kernel(false));
    assert!(applied
        .server_legs
        .iter()
        .all(|r| r.leg.target_trait.as_deref() != Some("Tally")));
}

#[test]
fn control_a_fold_that_delegates_nothing_has_no_legs() {
    let k = kernel(true);
    let applied = k
        .apply_server_response(
            &EventResponse {
                success: true,
                ..Default::default()
            },
            &HashSet::new(),
        )
        .unwrap();
    assert!(applied.server_legs.is_empty());
}

#[test]
fn the_delegated_effects_of_a_fold_do_not_ride_the_next_dispatch() {
    let k = kernel(false);
    fold(&k);
    let next = k
        .dispatch("InvoiceList", None, "NOTE", Value::Null)
        .unwrap();
    assert!(
        next.server_legs.is_empty(),
        "a state-only arm owes the host no delegated work"
    );
}
