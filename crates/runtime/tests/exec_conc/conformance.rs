//! EXEC-CONC conformance: what the straight-through dispatch may answer in place.


use orbital_core_base::value::Value;
use serde_json::json;
use crate::support::TestKernel;

fn fetch_errors(store: Option<&str>) -> Vec<String> {
    let mut ticket = json!({ "name": "Ticket", "persistence": "persistent", "collection": "tickets",
        "fields": [{ "name": "id", "type": "string" }] });
    if let Some(driver) = store {
        ticket["store"] = json!(driver);
    }
    let kernel = TestKernel::server().build(json!({
        "name": "Desk",
        "orbitals": [{ "name": "Tickets", "entity": ticket,
            "traits": [{ "name": "Desk", "linkedEntity": "Ticket",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "LOOK",
                        "effects": [["fetch", "Ticket", { "id": "t1" }]] }] } }] }]
    }));
    let outcomes = kernel.dispatch("Desk", None, "LOOK", Value::Null).unwrap();
    outcomes
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter(|e| e.is_failed())
        .filter_map(|e| e.error.clone())
        .collect()
}

#[test]
fn exec_conc_4_the_straight_through_dispatch_refuses_a_store_bound_row_ask() {
    let errors = fetch_errors(Some("Rows"));
    assert!(errors.iter().any(|e| e.contains("bound to store driver Rows")), "{errors:?}");
}

#[test]
fn exec_conc_4_control_an_unbound_entity_is_answered_in_place() {
    let errors = fetch_errors(None);
    assert!(errors.iter().all(|e| !e.contains("store driver")), "answered in place, not refused: {errors:?}");
}
