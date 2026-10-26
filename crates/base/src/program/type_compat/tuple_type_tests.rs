use super::*;

/// `.lolo` `[T1, T2, …]` (two or more slots) is a tuple; `[T]` stays an array.
#[test]
fn a_bracketed_list_of_two_or_more_types_is_a_tuple() {
    assert_eq!(
        tuple_type_slots("[number, number, number]"),
        Some(vec!["number", "number", "number"])
    );
    assert_eq!(
        tuple_type_slots("[[number], string]"),
        Some(vec!["[number]", "string"])
    );
    assert_eq!(
        tuple_type_slots("[Map<string, number>, number]"),
        Some(vec!["Map<string, number>", "number"])
    );
    assert_eq!(
        tuple_type_slots("[number, number]!"),
        Some(vec!["number", "number"]),
        "a required marker is not part of the type"
    );
}

fn tuple(items: Vec<OirType>) -> OirType {
    OirType::Tuple { items }
}

#[test]
fn a_tuple_relates_slot_by_slot_and_to_arrays() {
    let n3 = tuple(vec![OirType::Number, OirType::Number, OirType::Number]);
    assert!(sub(&n3, &n3).is_some());
    assert!(
        sub(&OirType::array(OirType::Number), &n3).is_some(),
        "an array literal fits when its element fits every slot"
    );
    assert!(
        sub(&n3, &OirType::array(OirType::Number)).is_some(),
        "a tuple is an array of its slots"
    );
    assert!(sub(
        &tuple(vec![OirType::String, OirType::Number]),
        &tuple(vec![OirType::Number, OirType::Number])
    )
    .is_none());
    assert!(sub(&OirType::array(OirType::String), &n3).is_none());
    assert!(
        sub(&tuple(vec![OirType::Number, OirType::Number]), &n3).is_none(),
        "arity must match"
    );
    assert_eq!(oir_type_kind(&n3), "array");
}

/// The kind-string relation knows a declared tuple: an array literal kind fits it, a
/// tuple of the same slots fits, a string does not.
#[test]
fn a_declared_tuple_in_the_kind_relation() {
    assert!(set_value_type_compatible(
        "[number, number, number]",
        "array"
    ));
    assert!(set_value_type_compatible(
        "[number, number, number]",
        "[number, number, number]"
    ));
    assert!(!set_value_type_compatible(
        "[number, number, number]",
        "string"
    ));
    assert!(!set_value_type_compatible(
        "[number, number]",
        "[string, string]"
    ));
}

#[test]
fn control_an_array_and_non_bracketed_types_are_not_tuples() {
    assert_eq!(tuple_type_slots("[number]"), None);
    assert_eq!(tuple_type_slots("Map<string, number>"), None);
    assert_eq!(tuple_type_slots("number"), None);
    assert_eq!(tuple_type_slots("[]"), None);
}
