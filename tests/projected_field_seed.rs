//! A field typed `T.f` (`projectedFrom`) seeds from `T`'s seeded rows — row `i` takes row
//! `i mod n`'s `f` — through the same placeholder-then-link pass a relation uses. Twin of
//! `@almadar/runtime`'s `MockPersistenceAdapter.projected-field-seed.test.ts`.

use orbital_core::runtime::{seed_mock_dataset, EntitySource, InMemoryEntityCache};
use orbital_core::schema::OrbitalSchema;
use orbital_core::Value;
use serde_json::json;

fn schema(with_tags: bool) -> OrbitalSchema {
    let tag = json!({
        "name": "Tag", "persistence": "persistent", "collection": "tags",
        "fields": [
            { "name": "id", "type": "string", "required": true },
            { "name": "name", "type": "string", "required": true }
        ]
    });
    let note = json!({
        "name": "Note", "persistence": "persistent", "collection": "notes",
        "fields": [
            { "name": "id", "type": "string", "required": true },
            { "name": "title", "type": "string", "required": true },
            { "name": "tagRef", "type": "string", "projectedFrom": { "type": "Tag", "field": "id" } },
            { "name": "tagName", "type": "string", "projectedFrom": { "type": "Tag", "field": "name" } },
            { "name": "tagRefs", "type": "array", "items": { "type": "string", "projectedFrom": { "type": "Tag", "field": "id" } } }
        ]
    });
    let mut orbital = json!({ "name": "NoteOrbital", "entity": note, "traits": [], "pages": [] });
    if with_tags {
        orbital["auxiliaryEntities"] = json!([tag]);
    }
    serde_json::from_value(json!({ "name": "projected-seed", "orbitals": [orbital] })).expect("schema")
}

fn seeded(with_tags: bool) -> InMemoryEntityCache {
    let store = InMemoryEntityCache::default();
    seed_mock_dataset(&store, &schema(with_tags), 4, None, 0.0);
    store
}

fn column(store: &InMemoryEntityCache, entity: &str, field: &str) -> Vec<Value> {
    store.list(entity).unwrap_or_default().iter().map(|r| r.get_path(field).cloned().unwrap_or(Value::Null)).collect()
}

#[test]
fn a_projected_id_holds_a_seeded_tag_id() {
    let store = seeded(true);
    let tag_ids = column(&store, "Tag", "id");
    let refs = column(&store, "Note", "tagRef");
    assert!(!refs.is_empty());
    for r in &refs {
        assert!(tag_ids.contains(r), "{r:?} is not a seeded Tag id ({tag_ids:?})");
    }
}

#[test]
fn a_projected_name_holds_that_column_of_a_seeded_tag() {
    let store = seeded(true);
    let names = column(&store, "Tag", "name");
    for n in column(&store, "Note", "tagName") {
        assert!(names.contains(&n), "{n:?} is not a seeded Tag name ({names:?})");
    }
}

#[test]
fn row_i_takes_the_target_row_i_mod_n() {
    let store = seeded(true);
    let tag_ids = column(&store, "Tag", "id");
    let refs = column(&store, "Note", "tagRef");
    for (i, r) in refs.iter().enumerate() {
        assert_eq!(r, &tag_ids[i % tag_ids.len()]);
    }
}

#[test]
fn control_an_unprojected_field_keeps_its_sampled_value() {
    let store = seeded(true);
    for t in column(&store, "Note", "title") {
        assert!(matches!(&t, Value::String(s) if !s.is_empty()), "{t:?}");
    }
}

#[test]
fn edge_a_target_with_no_rows_leaves_the_placeholder() {
    let store = seeded(false);
    for r in column(&store, "Note", "tagRef") {
        assert_eq!(r, Value::String(String::new()));
    }
}
