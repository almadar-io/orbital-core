//! A Server-env kernel over a seeded `InMemoryEntityCache`: a `fetch` returns the seeded rows,
//! ordered and unordered.

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::sync::Arc;

fn run(entity_extra: serde_json::Value, fetch_cfg: serde_json::Value) -> usize {
    let mut entity = json!({ "name": "Ticket", "persistence": "persistent",
        "fields": [{ "name": "id", "type": "string" }, { "name": "score", "type": "number", "required": true }] });
    for (k, v) in entity_extra.as_object().unwrap() {
        entity[k] = v.clone();
    }
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "sf", "orbitals": [{ "name": "Main", "entity": entity,
            "traits": [{ "name": "T", "linkedEntity": "Ticket", "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "LIST",
                    "effects": [["fetch", "Ticket", fetch_cfg]] }] } }] }]
    }))
    .unwrap();
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache.clone() as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    // Seed AFTER registering: the cache keys its store by the entity's bound `collection`, which
    // `register_schema` binds. Rows written under the entity name earlier sit in a store the entity
    // no longer reads.
    for i in 0..3 {
        let row: Value =
            serde_json::from_value(json!({ "id": format!("t{i}"), "score": i })).unwrap();
        cache.write("Ticket", &format!("t{i}"), &row).unwrap();
    }
    let out = k.dispatch("T", None, "LIST", Value::Null).unwrap();
    out.iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| er.return_value.as_ref()?.as_array().map(|a| a.len()))
        .sum()
}

#[test]
fn unordered_fetch_returns_the_seeded_rows() {
    assert_eq!(run(json!({}), json!({ "emit": { "success": "L" } })), 3);
}

#[test]
fn ordered_fetch_returns_the_seeded_rows() {
    assert_eq!(
        run(
            json!({}),
            json!({ "orderBy": "score:desc", "emit": { "success": "L" } })
        ),
        3
    );
}

#[test]
fn collection_declared_entity_reads_the_entity_name_key() {
    assert_eq!(
        run(
            json!({ "collection": "tickets" }),
            json!({ "emit": { "success": "L" } })
        ),
        3
    );
}

mod runtime_mock_seed {
    use orbital_core_runtime::effects::response::{EventRequest, EventResponse};
    use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
    use orbital_core_base::schema::types::OrbitalSchema;
    use serde_json::{json, Value as Json};

    fn server(persistence: &str) -> EmbeddedServer {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "rm", "orbitals": [{ "name": "Main",
                "entity": { "name": "Counter", "persistence": persistence, "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "count", "type": "number", "default": 0 },
                    { "name": "label", "type": "string" }
                ] },
                "traits": [{ "name": "T", "linkedEntity": "Counter", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT",
                          "effects": [["fetch", "Counter", { "emit": { "success": "LOADED" } }]] },
                        { "from": "idle", "to": "idle", "event": "READ_ID",
                          "effects": [["emit", "OBSERVED", { "id": "@entity.id", "count": "@entity.count" }]] },
                        { "from": "idle", "to": "idle", "event": "UPDATE",
                          "effects": [["persist", "update", "Counter", "@entity.id", { "count": 7 }]] },
                        { "from": "idle", "to": "idle", "event": "DELETE",
                          "effects": [["persist", "delete", "Counter", "@entity.id"]] }
                    ] } }] }]
        }))
        .unwrap();
        EmbeddedServer::new(schema).unwrap()
    }

    fn start(server: &EmbeddedServer) -> ServerProgress {
        let request = EventRequest {
            event: "INIT".to_string(),
            target_trait: Some("T".to_string()),
            ..Default::default()
        };
        server.start("Main", request).unwrap()
    }

    fn loaded_rows(response: &EventResponse) -> Vec<Json> {
        let loaded = response
            .emitted_events
            .iter()
            .find(|e| e.event == "LOADED")
            .expect("the fetch fires its success");
        serde_json::to_value(&loaded.payload).unwrap()["data"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn a_seeded_runtime_entity_fetches_its_one_mock_row_at_the_singleton_address() {
        let s = server("runtime");
        s.seed_runtime_mock(Some("viewer-1".to_string()));
        let ServerProgress::Done(response) = start(&s) else {
            panic!("a [runtime] fetch never asks the host");
        };
        let rows = loaded_rows(&response);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["id"], json!("singleton"));
        assert_eq!(rows[0]["count"].as_f64(), Some(0.0), "a literal default is honored");
    }

    #[test]
    fn runtime_singleton_address_preserves_the_seeded_rows_domain_id() {
        let server = server("runtime");
        server.seed_runtime_mock(Some("viewer-1".to_string()));
        let row = server.kernel().entity_source().read("Counter", "singleton").unwrap();
        let expected_id = orbital_core_runtime::seed::mock_row_id("Counter", 1);
        assert_eq!(row.get_path("id").and_then(|id| id.as_str()), Some(expected_id.as_str()));
        let ServerProgress::Done(response) = start(&server) else {
            panic!("a runtime singleton never asks the host");
        };
        assert_eq!(loaded_rows(&response)[0]["id"], json!(expected_id));
    }

    fn dispatch_singleton(server: &EmbeddedServer, event: &str) -> EventResponse {
        let request = EventRequest {
            event: event.to_string(),
            target_trait: Some("T".to_string()),
            ..Default::default()
        };
        let ServerProgress::Done(response) = server.start("Main", request).unwrap() else {
            panic!("a runtime singleton never asks the host");
        };
        response
    }

    #[test]
    fn runtime_singleton_binding_and_persist_keep_address_separate_from_domain_id() {
        let server = server("runtime");
        server.seed_runtime_mock(Some("viewer-1".to_string()));
        let expected_id = orbital_core_runtime::seed::mock_row_id("Counter", 1);
        let observed = dispatch_singleton(&server, "READ_ID");
        let payload = observed.emitted_events.iter().find(|event| event.event == "OBSERVED")
            .expect("READ_ID emits OBSERVED").payload.as_ref().expect("OBSERVED carries its entity fields");
        assert_eq!(payload.get_path("id").and_then(|id| id.as_str()), Some(expected_id.as_str()));
        dispatch_singleton(&server, "UPDATE");
        let row = server.kernel().entity_source().read("Counter", "singleton").unwrap();
        assert_eq!(row.get_path("count").and_then(|value| value.as_number()), Some(7.0));
        assert_eq!(row.get_path("id").and_then(|id| id.as_str()), Some(expected_id.as_str()));
        dispatch_singleton(&server, "DELETE");
        assert!(server.kernel().entity_source().find("Counter", "singleton").unwrap().is_none());
    }

    #[test]
    fn control_an_unseeded_runtime_entity_fetches_nothing() {
        let ServerProgress::Done(response) = start(&server("runtime")) else {
            panic!("a [runtime] fetch never asks the host");
        };
        assert!(loaded_rows(&response).is_empty());
    }

    #[test]
    fn edge_a_persistent_entity_is_not_seeded_and_still_asks_the_host() {
        let s = server("persistent");
        s.seed_runtime_mock(Some("viewer-1".to_string()));
        assert!(matches!(start(&s), ServerProgress::Await { .. }));
    }
}

/// The fetch's own result for a by-id miss: what the verifier's judge reads as handled or not (G-ORBITAL-CORE-035).
fn missed_fetch(fetch_cfg: serde_json::Value) -> orbital_core_base::effect_types::EffectResult {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "sf", "orbitals": [{ "name": "Main",
            "entity": { "name": "Ticket", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "T", "linkedEntity": "Ticket", "stateMachine": {
                "states": [{ "name": "loading", "isInitial": true }, { "name": "error" }],
                "transitions": [
                    { "from": "loading", "to": "loading", "event": "OPEN", "effects": [["fetch", "Ticket", fetch_cfg]] },
                    { "from": "loading", "to": "error", "event": "LOAD_FAILED" }
                ] } }] }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(RuntimeEnvironment::Server, Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>, Default::default());
    k.register_schema(schema).unwrap();
    let out = k.dispatch("T", None, "OPEN", Value::Null).unwrap();
    out.iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .find(|er| matches!(er.effect, orbital_core_base::effect_types::Effect::Fetch { .. }))
        .cloned()
        .expect("the fetch ran")
}

#[test]
fn a_missed_fetch_reports_the_failure_event_it_was_routed_to() {
    let result = missed_fetch(json!({ "id": "absent", "emit": { "success": "LOADED", "failure": "LOAD_FAILED" } }));
    assert!(result.is_failed(), "{result:?}");
    assert_eq!(result.routed_failure().as_deref(), Some("LOAD_FAILED"));
}

#[test]
fn control_a_missed_fetch_with_no_failure_route_reports_none() {
    let result = missed_fetch(json!({ "id": "absent", "emit": { "success": "LOADED" } }));
    assert!(result.is_failed(), "{result:?}");
    assert_eq!(result.routed_failure(), None);
}
