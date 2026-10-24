//! A browser-stored entity (`[persistent: x, browser]`) seeds by the browser store's
//! rule in the mock dataset too: its `instances` only, never mock rows. Twin of `@almadar/runtime` `server-seeds-browser-stored.test.ts`.

use {orbital_core_runtime::seed::seed_mock_dataset, orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}};
use orbital_core_base::schema::OrbitalSchema;
use serde_json::json;

fn rows(entity: serde_json::Value) -> usize {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "seed",
        "orbitals": [{ "name": "Books", "entity": entity, "traits": [], "pages": [] }]
    }))
    .expect("schema");
    let store = InMemoryEntityCache::default();
    seed_mock_dataset(&store, &schema, 4, None, 0.0);
    store.list("Invoice").unwrap_or_default().len()
}

fn invoice(extra: serde_json::Value) -> serde_json::Value {
    let mut e = json!({
        "name": "Invoice", "persistence": "persistent", "collection": "invoices",
        "fields": [{ "name": "id", "type": "string", "required": true }, { "name": "client", "type": "string" }]
    });
    for (k, v) in extra.as_object().unwrap() {
        e[k] = v.clone();
    }
    e
}

#[test]
fn a_browser_entity_with_no_instances_starts_empty() {
    assert_eq!(rows(invoice(json!({ "browser": true }))), 0);
}

#[test]
fn the_retired_seed_mock_flag_is_refused() {
    let schema = serde_json::from_value::<OrbitalSchema>(json!({
        "name": "seed",
        "orbitals": [{ "name": "Books", "entity": invoice(json!({ "browser": true, "seedMock": true })), "traits": [], "pages": [] }]
    }));
    assert!(schema.is_err());
}

#[test]
fn control_a_server_entity_with_neither_source_is_mock_seeded() {
    assert!(rows(invoice(json!({}))) > 0);
}

#[test]
fn an_auxiliary_browser_entity_follows_the_same_rule() {
    let field = json!([{ "name": "id", "type": "string", "required": true }]);
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "seed-aux",
        "orbitals": [{
            "name": "Books",
            "entity": { "name": "Ledger", "persistence": "persistent", "collection": "ledgers", "fields": field },
            "auxiliaryEntities": [
                { "name": "Draft", "persistence": "persistent", "collection": "drafts", "browser": true, "fields": field },
                { "name": "Plain", "persistence": "persistent", "collection": "plains", "fields": field }
            ],
            "traits": [], "pages": []
        }]
    }))
    .expect("schema");
    let store = InMemoryEntityCache::default();
    seed_mock_dataset(&store, &schema, 4, None, 0.0);
    assert_eq!(store.list("Draft").unwrap_or_default().len(), 0);
    assert!(!store.list("Plain").unwrap_or_default().is_empty());
}
