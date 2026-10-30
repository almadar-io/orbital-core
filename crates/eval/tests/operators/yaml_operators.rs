//! `(yaml/parse s Shape)`: YAML 1.2 core schema text decoded into a value and checked against its declared shape
//! exactly as `json/parse` does. One document only; anchors and aliases resolve; the 1.1 spellings (`yes`, `on`, `<<`)
//! are plain strings; what a JSON value cannot hold (a non-string key, a custom tag, NaN, an infinity) is null.

use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::context::BindingContextBuilder;
use orbital_core_eval::evaluator::Evaluator;
use serde_json::{json, Value as Json};

fn run(expr: Json) -> Result<Json, String> {
    let sexpr = serde_json::from_value(expr).map_err(|e| e.to_string())?;
    let ctx = BindingContextBuilder::new().build();
    Evaluator::new()
        .evaluate(&sexpr, &ctx)
        .map(|v: Value| serde_json::to_value(v).unwrap())
        .map_err(|e| e.to_string())
}

fn yaml(text: &str, shape: &str) -> Json {
    run(json!(["yaml/parse", text, shape])).unwrap()
}

fn row_shape() -> String {
    json!({ "kind": "struct", "fields": [
        { "name": "id", "fieldType": { "kind": "number" } },
        { "name": "tags", "fieldType": { "kind": "array", "inner": { "kind": "string" } } },
        { "name": "note", "fieldType": { "kind": "string" }, "required": false }
    ] })
    .to_string()
}

#[test]
fn scalars_resolve_by_the_core_schema() {
    for (text, expect) in [
        ("42", json!(42.0)),
        ("-7", json!(-7.0)),
        ("0o17", json!(15.0)),
        ("0x1F", json!(31.0)),
        ("1e3", json!(1000.0)),
        ("2.5", json!(2.5)),
        ("true", json!(true)),
        ("False", json!(false)),
        ("null", Json::Null),
        ("~", Json::Null),
        ("hello", json!("hello")),
        ("'42'", json!("42")),
        ("\"true\"", json!("true")),
        ("!!str 42", json!("42")),
    ] {
        assert_eq!(yaml(text, "Row"), expect, "{text}");
    }
}

#[test]
fn the_yaml_1_1_spellings_are_strings() {
    for word in ["yes", "no", "on", "off", "y", "n", "0777"] {
        let expect = if word == "0777" {
            json!(777.0)
        } else {
            json!(word)
        };
        assert_eq!(yaml(word, "Row"), expect, "{word}");
    }
    assert_eq!(yaml("a: 1\n<<: 2", "Row"), json!({ "a": 1.0, "<<": 2.0 }));
}

#[test]
fn maps_lists_and_nesting_keep_document_values() {
    assert_eq!(
        yaml("env:\n  - variable: A\n    value: x\n  - variable: B\n    secret: s\nrunConfig: { cpu: 1 }\n", "Row"),
        json!({
            "env": [{ "variable": "A", "value": "x" }, { "variable": "B", "secret": "s" }],
            "runConfig": { "cpu": 1.0 }
        })
    );
    assert_eq!(yaml("[1, two, [3]]", "Row"), json!([1.0, "two", [3.0]]));
    assert_eq!(yaml("---\na: 1\n", "Row"), json!({ "a": 1.0 }));
}

#[test]
fn flow_yaml_agrees_with_json_parse_on_the_same_data() {
    let text = r#"{"id": 1, "tags": ["a", "b"], "note": "n"}"#;
    assert_eq!(
        yaml(text, "Row"),
        run(json!(["json/parse", text, "Row"])).unwrap()
    );
}

#[test]
fn anchors_and_aliases_resolve() {
    assert_eq!(
        yaml("base: &b { k: 1 }\ncopy: *b\nlist: [*b, *b]\n", "Row"),
        json!({ "base": { "k": 1.0 }, "copy": { "k": 1.0 }, "list": [{ "k": 1.0 }, { "k": 1.0 }] })
    );
    assert_eq!(
        yaml("a: &s text\nb: *s", "Row"),
        json!({ "a": "text", "b": "text" })
    );
}

#[test]
fn malformed_empty_and_multi_document_text_is_null() {
    for text in [
        "a: [1, 2",
        "a: b: c",
        "a:\n\t- x",
        "",
        "   \n",
        "a: *missing",
        "---\na: 1\n---\nb: 2\n",
        "a\n---\nb",
    ] {
        assert_eq!(yaml(text, "Row"), Json::Null, "{text:?}");
    }
}

#[test]
fn what_a_json_value_cannot_hold_is_null() {
    for text in [
        "1: a",
        "? [x]\n: y",
        "!custom v",
        "a: !custom v",
        "a: .nan",
        "a: .inf",
        "a: -.inf",
    ] {
        assert_eq!(yaml(text, "Row"), Json::Null, "{text:?}");
    }
}

#[test]
fn a_value_not_of_its_shape_is_null_and_one_of_it_is_returned() {
    assert_eq!(yaml("42", "number"), json!(42.0));
    assert_eq!(yaml("abc", "number"), Json::Null);
    assert_eq!(
        yaml("id: 1\ntags: [a]\n", &row_shape()),
        json!({ "id": 1.0, "tags": ["a"] })
    );
    assert_eq!(
        yaml("id: 1\ntags: [a]\nnote: n\n", &row_shape()),
        json!({ "id": 1.0, "tags": ["a"], "note": "n" })
    );
    assert_eq!(yaml("tags: [a]\n", &row_shape()), Json::Null);
    assert_eq!(yaml("id: 1\ntags: [a, 2]\n", &row_shape()), Json::Null);
    assert_eq!(yaml("id: x\ntags: []\n", &row_shape()), Json::Null);
}

#[test]
fn a_non_string_input_or_a_non_literal_shape_is_an_error() {
    let err = run(json!(["yaml/parse", null, "Row"])).unwrap_err();
    assert_eq!(
        err,
        "Evaluation error: Type mismatch: expected string, got null"
    );
    assert!(run(json!(["yaml/parse", 42, "Row"]))
        .unwrap_err()
        .contains("expected string, got number"));
    assert!(run(json!(["yaml/parse", "a: 1", null]))
        .unwrap_err()
        .contains("Type mismatch"));
    assert!(run(json!(["yaml/parse", "a: 1"])).is_err());
}
