//! EXEC-DATA conformance: what writing rows does to the trait that wrote them.


use orbital_core_base::value::Value;
use serde_json::json;
use crate::support::TestKernel;

#[test]
fn exec_data_4_a_create_never_re_addresses_the_trait_that_ran_it() {
    let kernel = TestKernel::server().build(json!({
        "name": "Notes",
        "typeContractVersion": 1, "orbitals": [{
            "name": "NotesOrbital",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }, { "name": "scratch", "type": "string" }] },
            "traits": [{ "name": "Composer", "linkedEntity": "Note",
                "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                    "transitions": [
                        { "from": "on", "to": "on", "event": "DRAFT", "effects": [["set", "@entity.scratch", "kept"]] },
                        { "from": "on", "to": "on", "event": "SAVE", "effects": [["persist", "create", "Note", { "title": "created" }]] }
                    ] } }]
        }]
    }));
    kernel.dispatch("Composer", None, "DRAFT", Value::Null).unwrap();
    let outcomes = kernel.dispatch("Composer", None, "SAVE", Value::Null).unwrap();
    assert!(outcomes.iter().any(|(t, r)| t == "Composer" && r.executed), "control: the create ran");

    let frame = kernel.snapshot_trait("Composer", None).entity;
    assert_eq!(frame.get_path("scratch"), Some(&Value::from("kept")), "the frame's scratch field survives the create");
    assert_ne!(frame.get_path("title"), Some(&Value::from("created")), "the created row is not adopted into the frame");
}

#[test]
fn exec_data_1_a_seeder_stamps_absent_or_blank_timestamps_and_keeps_authored_ones() {
    let instances: Vec<orbital_core_base::schema::sexpr::SObject> = serde_json::from_value(json!([
        { "title": "fresh" },
        { "title": "dated", "createdAt": "2020-01-01T00:00:00Z", "updatedAt": "" }
    ]))
    .unwrap();
    let now = "2026-10-26T00:00:00Z";
    let rows = orbital_core_runtime::instance_rows("Note", &instances, now);

    assert_eq!(rows[0].get_path("createdAt"), Some(&Value::from(now)), "absent is the seeding instant");
    assert_eq!(rows[0].get_path("updatedAt"), Some(&Value::from(now)));
    assert_eq!(rows[1].get_path("createdAt"), Some(&Value::from("2020-01-01T00:00:00Z")), "an authored value is kept");
    assert_eq!(rows[1].get_path("updatedAt"), Some(&Value::from(now)), "blank is the seeding instant");
}

fn carried_kernel(shared: bool) -> orbital_core_runtime::kernel::RuntimeKernel {
    use orbital_core_runtime::entity_source::EntitySource;
    let cache = std::sync::Arc::new(orbital_core_runtime::entity_source::InMemoryEntityCache::new());
    let row = |v: serde_json::Value| -> Value { serde_json::from_value(v).unwrap() };
    cache.write("Note", "a", &row(json!({ "id": "a", "fromA": "A-field" }))).unwrap();
    cache.write("Note", "b", &row(json!({ "id": "b", "fromB": "B-field" }))).unwrap();
    TestKernel::server().source(cache).build(json!({
        "name": "Notes",
        "typeContractVersion": 1, "orbitals": [{
            "name": "NotesOrbital",
            "entity": { "name": "Note", "persistence": "persistent", "shared": shared,
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" },
                    { "name": "fromA", "type": "string" }, { "name": "fromB", "type": "string" },
                    { "name": "seenA", "type": "string" }, { "name": "seenB", "type": "string" }] },
            "traits": [{ "name": "Reader", "linkedEntity": "Note",
                "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                    "transitions": [{ "from": "on", "to": "on", "event": "READ", "effects": [
                        ["set", "@entity.seenA", "@entity.fromA"],
                        ["set", "@entity.seenB", "@entity.fromB"]] }] } }]
        }]
    }))
}

fn read_with_carried(kernel: &orbital_core_runtime::kernel::RuntimeKernel, carried: serde_json::Value) -> (Option<String>, Option<String>) {
    let mut by_trait = orbital_core_base::value::ValueMap::default();
    by_trait.insert("Reader".into(), serde_json::from_value(carried).unwrap());
    kernel.seed_leg(&[], Some(&by_trait), Some("a"));
    let outcomes = kernel.dispatch("Reader", Some("a"), "READ", Value::Null).unwrap();
    assert!(outcomes.iter().any(|(t, r)| t == "Reader" && r.executed), "the read ran");
    let frame = kernel.snapshot_trait("Reader", Some("a")).entity;
    let seen = |field: &str| frame.get_path(field).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    (seen("seenA"), seen("seenB"))
}

/// EXEC-DATA-3: a carried row naming an `id` is that row, not the row at the dispatch's address.
#[test]
fn exec_data_3_a_carried_row_with_an_id_is_addressed_by_that_id() {
    let kernel = carried_kernel(false);
    let (seen_a, seen_b) = read_with_carried(&kernel, json!({ "id": "b", "title": "client" }));
    assert_eq!(seen_b.as_deref(), Some("B-field"), "id-carrying row: the stored row of its own id is read");
    assert_eq!(seen_a, None, "id-carrying row: the row at the dispatch address is not read");

    let control = carried_kernel(false);
    let (seen_a, seen_b) = read_with_carried(&control, json!({ "title": "client" }));
    assert_eq!(seen_a.as_deref(), Some("A-field"), "control, no id: the row at the dispatch address is read");
    assert_eq!(seen_b, None, "control, no id: another row is not read");
}

/// EXEC-DATA-3: an id-carrying row of a persisted shared entity is not folded; a non-shared one is.
#[test]
fn exec_data_3_an_id_carrying_row_of_a_persisted_shared_entity_is_not_folded() {
    let shared = carried_kernel(true);
    let (_, seen_b) = read_with_carried(&shared, json!({ "id": "b", "fromB": "client-B" }));
    assert_ne!(seen_b.as_deref(), Some("client-B"), "shared: the carried copy is not folded");

    let control = carried_kernel(false);
    let (_, seen_b) = read_with_carried(&control, json!({ "id": "b", "fromB": "client-B" }));
    assert_eq!(seen_b.as_deref(), Some("client-B"), "control, not shared: the carried copy folds");
}
