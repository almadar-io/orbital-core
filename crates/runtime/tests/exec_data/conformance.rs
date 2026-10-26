//! EXEC-DATA conformance: what writing rows does to the trait that wrote them.


use orbital_core_base::value::Value;
use serde_json::json;
use crate::support::TestKernel;

#[test]
fn exec_data_4_a_create_never_re_addresses_the_trait_that_ran_it() {
    let kernel = TestKernel::server().build(json!({
        "name": "Notes",
        "orbitals": [{
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
