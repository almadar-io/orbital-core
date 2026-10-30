use super::*;
use crate::ir::types::{OirEvent, OirModule};

#[test]
fn row_prints_as_the_bare_entity_where_reference_prints_an_arrow() {
    let p = PrettyPrinter::new();
    assert_eq!(
        p.format_type(&OirType::array(OirType::row("Revision"))),
        "[Revision]"
    );
    assert_eq!(
        p.format_type(&OirType::array(OirType::reference("Revision"))),
        "[-> Revision]"
    );
}

#[test]
fn test_pretty_print_module() {
    let mut module = OirModule::new("TestApp", "1.0.0");
    module.description = Some("A test application".to_string());

    let mut entity = OirEntity::new("Task");
    entity.fields.push(OirField::new("title", OirType::String));
    entity
        .fields
        .push(OirField::optional("description", OirType::String));
    module.entities.push(entity);

    let mut trait_def = OirTrait::new("TaskLifecycle");
    trait_def.states.push(OirState::initial("idle"));
    trait_def.states.push(OirState::new("running"));
    trait_def.events.push(OirEvent::new("START"));
    trait_def
        .transitions
        .push(OirTransition::new("idle", "running", "START"));
    module.traits.push(trait_def);

    let output = pretty_print(&module);
    assert!(output.contains("Module: TestApp v1.0.0"));
    assert!(output.contains("entity Task"));
    assert!(output.contains("trait TaskLifecycle"));
}

#[test]
fn test_pretty_print_expr() {
    let expr = OirExpr::and(
        OirExpr::eq(
            OirExpr::entity_binding(vec!["status".to_string()]),
            OirExpr::string("active"),
        ),
        OirExpr::gt(
            OirExpr::entity_binding(vec!["count".to_string()]),
            OirExpr::number(0.0),
        ),
    );

    let output = pretty_print_expr(&expr);
    assert!(output.contains("@entity.status"));
    assert!(output.contains("=="));
    assert!(output.contains("\"active\""));
}

#[test]
fn a_list_and_a_call_separate_their_items_with_a_comma() {
    let list = OirExpr::array(vec![
        OirExpr::number(1.0),
        OirExpr::string("a"),
        OirExpr::Null,
    ]);
    assert_eq!(pretty_print_expr(&list), "[1, \"a\", null]");
    assert_eq!(
        pretty_print_expr(&OirExpr::call(
            "f",
            vec![OirExpr::number(1.0), OirExpr::number(2.0)]
        )),
        "f(1, 2)"
    );
    assert_eq!(
        pretty_print_expr(&OirExpr::array(vec![])),
        "[]",
        "control: an empty list has no separator"
    );
}

#[test]
fn credential_connect_prints_its_request_object() {
    let request = OirExpr::object(
        [("credential".to_string(), OirExpr::string("token_name"))]
            .into_iter()
            .collect(),
    );
    let printed = pretty_print_expr(&OirExpr::effect(crate::ir::effects::OirEffect::credential_connect(request, None)));
    assert!(printed.starts_with("credential-connect("), "{printed}");
    assert!(printed.contains("credential: \"token_name\""), "{printed}");
}
