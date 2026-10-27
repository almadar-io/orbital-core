//! G-CROSS-335: a row a host answers is checked once, where it arrives, against its entity's declared fields; a row
//! that does not fit fails the asking effect as a store failure.

use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_runtime::config::KernelConfig;
use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
use orbital_core_runtime::kernel::RuntimeKernel;
use orbital_core_runtime::resumable::{drive_blocking, IoAnswer, IoRequest};
use serde_json::json;
use std::sync::Arc;

fn kernel() -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema()).unwrap();
    kernel
}

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [
                { "name": "id", "type": "string" },
                { "name": "count", "type": "number" },
                { "name": "tags", "type": "array", "items": { "type": "string" } },
                { "name": "replies", "type": "relation", "relation": { "entity": "Note", "cardinality": "many" } }
            ] },
            "traits": [{
                "name": "Browse",
                "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "LOAD", "effects": [["fetch", "Note", { "emit": { "success": "LOADED", "failure": "LOAD_FAILED" } }]] },
                        { "from": "idle", "to": "idle", "event": "LOADED" },
                        { "from": "idle", "to": "idle", "event": "LOAD_FAILED" }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// The events the fetch answered with, when the host lists `rows`.
fn answered(rows: serde_json::Value) -> Vec<String> {
    let kernel = kernel();
    let rows: Vec<Value> = serde_json::from_value(rows).unwrap();
    let progress = kernel
        .start_dispatch("Browse", None, "LOAD", Value::Null, None, None)
        .unwrap();
    let outcomes = drive_blocking(&kernel, progress, &|request| match request {
        IoRequest::ListRows { .. } => {
            IoAnswer::Rows(Ok(rows.iter().cloned().map(Arc::new).collect()))
        }
        IoRequest::QueryRows { .. } => IoAnswer::QueryUnsupported("listed".into()),
        IoRequest::ReadRow { .. } => IoAnswer::Row(Ok(Value::Null)),
        other => panic!("unexpected ask {other:?}"),
    })
    .unwrap();
    outcomes
        .iter()
        .filter_map(|(_, result)| result.event.clone())
        .collect()
}

#[test]
fn a_row_whose_field_contradicts_its_declaration_fails_the_fetch() {
    let events = answered(json!([{ "id": "n1", "count": 5 }, { "id": "n2", "count": "5" }]));
    assert!(events.iter().any(|e| e == "LOAD_FAILED"), "{events:?}");
    assert!(!events.iter().any(|e| e == "LOADED"), "{events:?}");
}

#[test]
fn a_list_field_is_checked_element_by_element() {
    let events = answered(json!([{ "id": "n1", "count": 1, "tags": ["a", 2] }]));
    assert!(events.iter().any(|e| e == "LOAD_FAILED"), "{events:?}");
}

#[test]
fn control_rows_that_fit_absent_or_null_fields_and_undeclared_extras_load() {
    let events = answered(json!([
        { "id": "n1", "count": 5, "tags": ["a"] },
        { "id": "n2" },
        { "id": "n3", "count": null, "createdAt": "2026-10-27" }
    ]));
    assert!(events.iter().any(|e| e == "LOADED"), "{events:?}");
    assert!(!events.iter().any(|e| e == "LOAD_FAILED"), "{events:?}");
}

#[test]
fn a_relation_holds_ids_or_rows_that_fit_the_related_entity() {
    let events = answered(
        json!([{ "id": "n1", "count": 1, "replies": ["n2", { "id": "n3", "count": 2 }] }]),
    );
    assert!(events.iter().any(|e| e == "LOADED"), "{events:?}");
    let events =
        answered(json!([{ "id": "n1", "count": 1, "replies": [{ "id": "n3", "count": "2" }] }]));
    assert!(
        events.iter().any(|e| e == "LOAD_FAILED"),
        "a hydrated row is checked as its entity: {events:?}"
    );
    let events = answered(json!([{ "id": "n1", "replies": [7] }]));
    assert!(
        events.iter().any(|e| e == "LOAD_FAILED"),
        "an id is a string: {events:?}"
    );
}

#[test]
fn a_host_seed_whose_row_contradicts_its_entity_is_refused_and_writes_nothing() {
    let mut runtime = serde_json::to_value(schema()).unwrap();
    runtime["orbitals"][0]["entity"]["persistence"] = json!("runtime");
    let server =
        orbital_core_runtime::embed::EmbeddedServer::new(serde_json::from_value(runtime).unwrap())
            .unwrap();
    let rows = |v: serde_json::Value| -> Vec<Value> { serde_json::from_value(v).unwrap() };
    let refused = server.seed_rows(
        "Note",
        &rows(json!([{ "id": "n1", "count": 1 }, { "id": "n2", "count": "2" }])),
    );
    assert!(refused.is_err_and(|e| e.contains("Note.count")));
    assert!(
        server.kernel().entity_source().read("Note", "n1").is_err(),
        "nothing was written"
    );
    server
        .seed_rows("Note", &rows(json!([{ "id": "n1", "count": 1 }])))
        .unwrap();
    assert!(
        server.kernel().entity_source().read("Note", "n1").is_ok(),
        "control: a fitting seed is written"
    );
}
