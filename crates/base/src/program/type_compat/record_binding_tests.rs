//! A render-ui prop that takes ONE record (`form-section.initialData`,
//! `detail-panel.entity`) must not receive a list, nor a row of another
//! entity. Shared by lolo L1 and compiler L2 (2026-09-30: 1,045 sites
//! bound a collection into a record prop and validated 0/0).
use super::*;

const ENTITIES: [&str; 2] = ["Booking", "Staff"];
fn is_entity(n: &str) -> bool {
    ENTITIES.contains(&n)
}
fn check(ty: &str, expected: Option<&str>) -> Option<RecordBindingMismatch> {
    bound_shape_of_type_str(ty)
        .and_then(|b| record_prop_binding_mismatch(&b, expected, is_entity))
}

#[test]
fn a_collection_is_never_a_record() {
    for ty in ["[Booking]", "[object]", "array", "[Booking]!"] {
        assert_eq!(
            check(ty, Some("Booking")),
            Some(RecordBindingMismatch::CollectionIntoRecord),
            "{ty}"
        );
    }
}

#[test]
fn a_row_of_another_entity_is_the_wrong_record() {
    assert_eq!(
        check("Staff", Some("Booking")),
        Some(RecordBindingMismatch::WrongEntity {
            expected: "Booking".into(),
            actual: "Staff".into()
        })
    );
}

#[test]
fn control_a_row_of_the_same_entity_fits() {
    assert_eq!(check("Booking", Some("Booking")), None);
    assert_eq!(check("Booking!", Some("Booking")), None);
    assert_eq!(check("object", Some("Booking")), None);
}

#[test]
fn edge_a_struct_alias_is_not_compared_as_an_entity() {
    assert_eq!(check("UploadedFile", Some("Booking")), None);
}

#[test]
fn edge_sentinels_are_undecidable_before_inline() {
    for ty in ["@entity", "[@entity]", "$E", "[$E]"] {
        assert_eq!(bound_shape_of_type_str(ty), None, "{ty}");
    }
}

#[test]
fn a_lowered_payload_field_carries_its_entity_separately() {
    // `data : Staff` lowers to type "object" + entity "Staff";
    // `data : [Staff]` to "[object]" + entity "Staff".
    assert_eq!(
        bound_shape_of_payload_field("object", Some("Staff")),
        Some(BoundShape::Record(Some("Staff".into())))
    );
    assert_eq!(
        bound_shape_of_payload_field("[object]", Some("Staff")),
        Some(BoundShape::Collection)
    );
    assert_eq!(
        bound_shape_of_payload_field("object", None),
        Some(BoundShape::Record(None))
    );
    assert_eq!(
        bound_shape_of_payload_field("object", Some("@entity")),
        None
    );
    assert_eq!(
        bound_shape_of_payload_field("string", None),
        Some(BoundShape::Other)
    );
}

#[test]
fn edge_scalars_and_an_unknown_expected_entity_pass() {
    assert_eq!(check("string", Some("Booking")), None);
    assert_eq!(check("Staff", None), None);
}
