// The `listens` address form, shared by `listens` sources and event-typed values.

use orbital_core::schema::{join_event_address, split_event_address, ListenSource};

#[test]
fn each_form_splits_and_joins_back() {
    for raw in [
        "SAVE",
        "Form.SAVE",
        "*.SAVE",
        "Tasks.TaskPersistor.DO_CREATE",
    ] {
        let (event, source) = split_event_address(raw);
        assert_eq!(join_event_address(&event, source.as_ref()), raw);
    }
}

#[test]
fn the_three_part_form_is_orbital_trait_event() {
    let (event, source) = split_event_address("Tasks.TaskPersistor.DO_CREATE");
    assert_eq!(event, "DO_CREATE");
    assert_eq!(
        source,
        Some(ListenSource::Orbital {
            orbital: "Tasks".to_string(),
            orbital_id: None,
            trait_name: "TaskPersistor".to_string(),
            trait_id: None,
        })
    );
}

#[test]
fn control_a_bare_event_has_no_source() {
    assert_eq!(split_event_address("SAVE"), ("SAVE".to_string(), None));
}
