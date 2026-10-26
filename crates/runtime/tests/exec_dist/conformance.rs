//! EXEC-DIST conformance: where the kernel says a leg runs.


use orbital_core_base::value::Value;
use orbital_core_runtime::effects::response::LegResidence;
use orbital_core_runtime::kernel::ClientDispatch;
use serde_json::{json, Value as Json};
use crate::support::TestKernel;

fn dispatch(mode: Option<&str>, effects: Json) -> ClientDispatch {
    let kernel = TestKernel::client().build(json!({
        "name": "books",
        "orbitals": [
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
