// A `ClientCore` with no client-role kernel resolves a row-set reference through its server bridge: the server holds
// the rows and applies the viewer's `@read`. Controls: a core with no store errs; a bridge with no route refuses.

use orbital_core_runtime::client::bridge::{KernelBridge, ServerBridge};
use orbital_core_runtime::client::client_core::ClientCore;
use orbital_core_runtime::client::http::BridgeError;
use orbital_core_base::row_set::RowSetRef;
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::json;
use std::sync::Arc;

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "rows", "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Ticket", "persistence": "persistent",
                "fields": [{ "name": "id", "type": "string" }, { "name": "owner", "type": "string" }],
                "read_policy": ["=", ["object/get", "@entity", "owner"], "@user.id"]
            },
            "traits": [{
                "name": "Tickets", "linkedEntity": "Ticket",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] }
            }]
        }]
    }))
    .unwrap()
}

fn row(id: &str, owner: &str) -> Value {
    serde_json::from_value(json!({ "id": id, "owner": owner })).unwrap()
}

fn reference(ids: &[&str]) -> RowSetRef {
    RowSetRef {
        entity_type: "Ticket".into(),
        ids: ids.iter().map(|s| s.to_string()).collect(),
        total: ids.len(),
        query: Default::default(),
    }
}

fn ids(page: &orbital_core_runtime::row_query::RowPage) -> Vec<String> {
    page.rows
        .iter()
        .map(|r| {
            r.as_object()
                .unwrap()
                .get("id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

fn server() -> KernelBridge {
    let server = KernelBridge::new(schema()).unwrap();
    server.seed(
        "Ticket",
        vec![row("t1", "u1"), row("t2", "u2"), row("t3", "u1")],
    ).unwrap();
    server.set_current_user(serde_json::from_value(json!({ "id": "u1" })).unwrap());
    server
}

#[test]
fn a_server_hosted_reference_resolves_through_the_bridge_and_omits_a_row_the_viewer_cannot_read() {
    let core = ClientCore::new(schema(), Some(Arc::new(server())));
    let page = core.resolve_rows(&reference(&["t3", "t2", "t1"])).unwrap();
    assert_eq!(ids(&page), vec!["t3", "t1"]);
    assert_eq!(page.total, 2);
}

#[test]
fn a_row_the_server_no_longer_holds_is_omitted() {
    let server = server();
    let core = ClientCore::new(schema(), Some(Arc::new(server)));
    let page = core.resolve_rows(&reference(&["t1", "t9"])).unwrap();
    assert_eq!(ids(&page), vec!["t1"]);
}

#[test]
fn control_a_core_with_no_store_has_nothing_to_resolve_against() {
    let core = ClientCore::new(schema(), None);
    assert!(core.resolve_rows(&reference(&["t1"])).is_err());
}

struct EventsOnly;

impl ServerBridge for EventsOnly {
    fn send_event_with_options(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Value,
    ) -> Result<EventResponse, BridgeError> {
        Ok(EventResponse::success(""))
    }
}

#[test]
fn control_a_bridge_with_no_resolve_route_refuses_instead_of_answering_an_empty_page() {
    let core = ClientCore::new(schema(), Some(Arc::new(EventsOnly)));
    assert!(core.resolve_rows(&reference(&["t1"])).is_err());
}
