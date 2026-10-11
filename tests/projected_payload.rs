//! `projected_payload`: a `T.f` payload field takes `f` of `T`'s first row; twin of
//! `@almadar-io/verify` `payload-projection.test.ts`.

use orbital_core::runtime::projected_payload;
use orbital_core::schema::PayloadField;
use orbital_core::Value;
use serde_json::json;

fn fields() -> Vec<PayloadField> {
    serde_json::from_value(json!([
        { "name": "tagId", "type": "string", "required": true, "projectedFrom": { "type": "Tag", "field": "id" } },
        { "name": "tagName", "type": "string", "projectedFrom": { "type": "Tag", "field": "name" } },
        { "name": "tagIds", "type": "[string]", "projectedFrom": { "type": "Tag", "field": "id" } },
        { "name": "plain", "type": "string", "required": true }
    ]))
    .expect("fields")
}

fn tags() -> Vec<Value> {
    serde_json::from_value(json!([{ "id": "Tag-1", "name": "urgent" }, { "id": "Tag-2", "name": "later" }])).expect("rows")
}

#[test]
fn a_projected_id_takes_the_first_row_id_and_an_array_wraps_it() {
    let rows = tags();
    let out = projected_payload(&fields(), |t| (t == "Tag").then_some(rows.as_slice()));
    assert_eq!(out.get("tagId"), Some(&Value::String("Tag-1".into())));
    assert_eq!(out.get("tagIds"), Some(&Value::Array(vec![Value::String("Tag-1".into())])));
}

#[test]
fn a_projection_of_another_field_takes_that_column() {
    let rows = tags();
    let out = projected_payload(&fields(), |t| (t == "Tag").then_some(rows.as_slice()));
    assert_eq!(out.get("tagName"), Some(&Value::String("urgent".into())));
}

#[test]
fn control_an_unprojected_field_is_not_filled() {
    let rows = tags();
    let out = projected_payload(&fields(), |t| (t == "Tag").then_some(rows.as_slice()));
    assert!(!out.contains_key("plain"));
}

#[test]
fn edge_a_target_with_no_rows_is_left_out() {
    let out = projected_payload(&fields(), |_| None);
    assert!(out.is_empty());
}

mod through_mapping {
    use super::*;
    use orbital_core::runtime::projected_payload::projected_through_mapping;
    use orbital_core::schema::SExpression;

    fn target() -> Vec<PayloadField> {
        serde_json::from_value(json!([
            { "name": "tagId", "type": "string", "required": true, "projectedFrom": { "type": "Tag", "field": "id" } },
            { "name": "label", "type": "string" }
        ]))
        .expect("target fields")
    }

    fn mapping(pairs: &[(&str, &str)]) -> orbital_core::schema::SObject {
        pairs.iter().map(|(k, v)| (k.to_string(), SExpression::String(v.to_string()))).collect()
    }

    #[test]
    fn a_source_field_passed_into_a_projected_target_field_takes_its_projected_value() {
        let rows = tags();
        let out = projected_through_mapping(&target(), &mapping(&[("tagId", "@payload.id")]), |t| (t == "Tag").then_some(rows.as_slice()));
        assert_eq!(out.get("id"), Some(&Value::String("Tag-1".into())));
    }

    #[test]
    fn control_a_source_field_passed_into_an_unprojected_target_field_is_not_filled() {
        let rows = tags();
        let out = projected_through_mapping(&target(), &mapping(&[("label", "@payload.name")]), |t| (t == "Tag").then_some(rows.as_slice()));
        assert!(out.is_empty());
    }

    #[test]
    fn edge_a_computed_mapping_value_is_not_a_pass_through() {
        let rows = tags();
        let computed: orbital_core::schema::SObject = orbital_core::schema::SObject::from([(
            "tagId".to_string(),
            SExpression::Array(vec![SExpression::String("str/concat".into()), SExpression::String("@payload.id".into())]),
        )]);
        let out = projected_through_mapping(&target(), &computed, |t| (t == "Tag").then_some(rows.as_slice()));
        assert!(out.is_empty());
    }
}
