//! Twin of `@almadar/core` `__tests__/behavior-value.test.ts`.

use orbital_core_base::value::Value;
use orbital_core_base::schema::behavior_value::{
    apply_trait_overrides, is_trait_value, parse_trait_value_literal, TraitValueRef,
};

fn v(json: &str) -> Value {
    serde_json::from_str(json).expect("value json")
}

fn kanban() -> Value {
    v(r#"{"behavior":"std/std-kanban","trait":"KanbanBoard"}"#)
}

fn r(behavior: &str, t: &str) -> TraitValueRef {
    TraitValueRef {
        behavior: behavior.into(),
        trait_name: t.into(),
    }
}

#[test]
fn parses_the_reference_path_spelling() {
    assert_eq!(
        parse_trait_value_literal("std/std-kanban.traits.KanbanBoard"),
        Some(kanban())
    );
    assert_eq!(
        TraitValueRef::parse("almadar-behaviors/vim-mode.traits.VimMode"),
        Some(r("almadar-behaviors/vim-mode", "VimMode"))
    );
    assert_eq!(
        TraitValueRef::parse("std/behaviors/std-kanban.traits.KanbanBoard"),
        Some(r("std/behaviors/std-kanban", "KanbanBoard"))
    );
}

#[test]
fn parses_the_workspace_relative_spelling() {
    assert_eq!(
        TraitValueRef::parse("./orbitals/TaskBoard.traits.TaskBoardView"),
        Some(r("./orbitals/TaskBoard", "TaskBoardView"))
    );
    assert_eq!(
        TraitValueRef::parse("./a/b/c_d.traits.X"),
        Some(r("./a/b/c_d", "X"))
    );
}

#[test]
fn control_relative_escapes_and_absolutes_are_rejected() {
    for bad in [
        "../outside/X.traits.Y",
        "/abs/X.traits.Y",
        "./X.traits.Y",
        "./a//X.traits.Y",
        "./a/-X.traits.Y",
        "./a/b c.traits.Y",
    ] {
        assert_eq!(TraitValueRef::parse(bad), None, "{bad}");
    }
}

#[test]
fn round_trips_through_format() {
    let k = r("std/std-kanban", "KanbanBoard");
    assert_eq!(k.format(), "std/std-kanban.traits.KanbanBoard");
    assert_eq!(TraitValueRef::parse(&k.format()), Some(k));
}

#[test]
fn control_local_embed_and_malformed_paths_are_not_literals() {
    for bad in [
        "@trait.KanbanBoard",
        "std-kanban.traits.KanbanBoard",
        "std/std-kanban.orbitals.KanbanOrbital",
        "std/std-kanban.traits.",
        "std/std-kanban.traits.kanbanBoard",
        "std/.traits.KanbanBoard",
        "std/other/std-kanban.traits.KanbanBoard",
        ".traits.KanbanBoard",
    ] {
        assert_eq!(TraitValueRef::parse(bad), None, "{bad}");
    }
}

#[test]
fn apply_records_overrides_without_touching_the_identity() {
    let base = kanban();
    let out = apply_trait_overrides(&base, &v(r#"{"config":{"compact":true}}"#)).unwrap();
    assert_eq!(
        out,
        v(r#"{"behavior":"std/std-kanban","trait":"KanbanBoard","config":{"compact":true}}"#)
    );
    assert_eq!(base, kanban());
}

#[test]
fn apply_merges_config_events_fields_later_wins() {
    let a = apply_trait_overrides(
        &kanban(),
        &v(r#"{"config":{"compact":true,"title":"A"},"events":{"OPEN":"SHOW"},"fields":{"name":"title"}}"#),
    )
    .unwrap();
    let b = apply_trait_overrides(
        &a,
        &v(r#"{"config":{"title":"B"},"events":{"CLOSE":"HIDE"},"fields":{"name":"label"}}"#),
    )
    .unwrap();
    let Value::Object(m) = &b else {
        panic!("object")
    };
    assert_eq!(m["config"], v(r#"{"compact":true,"title":"B"}"#));
    assert_eq!(m["events"], v(r#"{"OPEN":"SHOW","CLOSE":"HIDE"}"#));
    assert_eq!(m["fields"], v(r#"{"name":"label"}"#));
}

#[test]
fn apply_replaces_listens_emits_scope_linked_entity() {
    let a = apply_trait_overrides(
        &kanban(),
        &v(r#"{"listens":[{"event":"A","triggers":"B"}],"emitsScope":"internal","linkedEntity":"Task"}"#),
    )
    .unwrap();
    let b = apply_trait_overrides(
        &a,
        &v(r#"{"listens":[],"emitsScope":"external","linkedEntity":"Card"}"#),
    )
    .unwrap();
    let Value::Object(m) = &b else {
        panic!("object")
    };
    assert_eq!(m["listens"], v("[]"));
    assert_eq!(m["emitsScope"], v(r#""external""#));
    assert_eq!(m["linkedEntity"], v(r#""Card""#));
}

#[test]
fn control_empty_override_is_the_identity() {
    assert_eq!(apply_trait_overrides(&kanban(), &v("{}")), Some(kanban()));
}

#[test]
fn control_apply_rejects_non_trait_values() {
    assert_eq!(
        apply_trait_overrides(&v(r#""std/std-kanban.traits.KanbanBoard""#), &v("{}")),
        None
    );
    assert_eq!(
        apply_trait_overrides(&v(r#"{"behavior":"std/std-kanban"}"#), &v("{}")),
        None
    );
    assert_eq!(apply_trait_overrides(&kanban(), &v("[]")), None);
}

#[test]
fn is_trait_value_accepts_the_shape_and_rejects_others() {
    assert!(is_trait_value(&kanban()));
    assert!(is_trait_value(
        &apply_trait_overrides(&kanban(), &v(r#"{"config":{"a":1}}"#)).unwrap()
    ));
    assert!(!is_trait_value(&v(
        r#""std/std-kanban.traits.KanbanBoard""#
    )));
    assert!(!is_trait_value(&v(r#"{"behavior":"std/std-kanban"}"#)));
    assert!(!is_trait_value(&Value::Null));
}

#[test]
fn trait_typed_slot_accepts_a_trait_value_and_not_an_arbitrary_object() {
    use orbital_core_base::type_compat::set_value_type_compatible as ok;
    assert!(ok("trait", "trait"));
    assert!(!ok("trait", "object"));
    assert!(!ok("trait", "number"));
    assert!(!ok("trait", "boolean"));
}

#[test]
fn a_trait_payload_field_is_assignable_to_a_trait_entity_field() {
    use orbital_core_base::ir::OirType;
    use orbital_core_base::type_compat::{oir_type_accepts_kind, set_value_type_compatible};
    assert!(oir_type_accepts_kind(&OirType::Trait, "trait"));
    assert!(set_value_type_compatible("trait", "trait!"));
    assert!(!oir_type_accepts_kind(&OirType::Trait, "number"));
    assert!(!set_value_type_compatible("number", "trait"));
}
