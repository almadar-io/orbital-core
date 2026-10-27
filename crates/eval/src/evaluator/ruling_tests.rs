//! Kernel rulings of 2026-10-18: declared arity is a bound, `__proto__` is refused as a key, `validate/check` takes rules
//! by field, `str/template` and `str/truncate` follow the kernel.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::evaluator::context::BindingContext;
use crate::evaluator::Evaluator;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::value::Value;
use serde::Deserialize;

fn run(json: &str) -> Result<Value, String> {
    let expr: SExpression = serde_json::from_str(json).unwrap();
    Evaluator::new().evaluate(&expr, &BindingContext::new()).map_err(|e| e.to_string())
}

fn ok(json: &str) -> Value {
    run(json).unwrap_or_else(|e| panic!("{json}: {e}"))
}

fn err(json: &str) -> String {
    run(json).expect_err(json)
}

fn num(n: f64) -> Value {
    Value::Number(n)
}

#[test]
fn a_call_with_more_arguments_than_declared_is_an_arity_error() {
    for call in [
        r#"["%", 7, 3, 1]"#,
        r#"["math/mod", 7, 3, 1]"#,
        r#"["/", 8, 2, 2]"#,
        r#"["math/map", 5, 0, 10, 0, 100, 1]"#,
    ] {
        let message = err(call);
        assert!(message.contains("Arity mismatch"), "{call}: {message}");
    }
}

#[test]
fn control_the_declared_arity_runs_and_a_variadic_operator_takes_any_count() {
    assert_eq!(ok(r#"["%", 7, 3]"#), num(1.0));
    assert_eq!(ok(r#"["math/map", 5, 0, 10, 0, 100]"#), num(50.0));
    assert_eq!(ok(r#"["/", 8, 2]"#), num(4.0));
    assert_eq!(ok(r#"["+", 1, 2, 3, 4, 5]"#), num(15.0));
}

#[test]
fn every_key_writing_object_operator_answers_null_for_proto() {
    for call in [
        r#"["object/set", {"a": 1}, "__proto__.x", 1]"#,
        r#"["object/set", {"a": 1}, "a.__proto__", 1]"#,
        r#"["object/merge", {"a": 1}, {"__proto__": {"x": 1}}]"#,
        r#"["object/deepMerge", {"a": {"b": 1}}, {"a": {"__proto__": 1}}]"#,
        r#"["object/deepMerge", {"a": 1}, {"b": {"__proto__": 1}}]"#,
        r#"["object/fromEntries", [["__proto__", 1]]]"#,
        r#"["object/mapKeys", {"a": 1}, ["fn", "k", "__proto__"]]"#,
    ] {
        assert_eq!(ok(call), Value::Null, "{call}");
    }
}

#[test]
fn control_ordinary_keys_including_look_alikes_are_written() {
    assert_eq!(ok(r#"["object/get", ["object/set", {"a": 1}, "b.c", 2], "b.c"]"#), num(2.0));
    assert_eq!(ok(r#"["object/get", ["object/merge", {"a": 1}, {"proto": 2, "constructor": 3}], "constructor"]"#), num(3.0));
    assert_eq!(ok(r#"["object/get", ["object/deepMerge", {"a": {"b": 1}}, {"a": {"c": 2}}], "a.c"]"#), num(2.0));
    assert_eq!(ok(r#"["object/get", ["object/fromEntries", [["__proto", 1]]], "__proto"]"#), num(1.0));
}

#[test]
fn str_template_substitutes_double_braces_and_leaves_single_braces_and_unknown_keys() {
    assert_eq!(ok(r#"["str/template", "Hello, {{name}}!", {"name": "World"}]"#), Value::string("Hello, World!"));
    assert_eq!(ok(r#"["str/template", "Hello, {name}!", {"name": "World"}]"#), Value::string("Hello, {name}!"));
    assert_eq!(ok(r#"["str/template", "{{a}} {{b}}", {"a": 1}]"#), Value::string("1 {{b}}"));
}

#[test]
fn str_truncate_keeps_len_characters_then_appends_the_suffix() {
    assert_eq!(ok(r#"["str/truncate", "Hello World", 8, "..."]"#), Value::string("Hello Wo..."));
    assert_eq!(ok(r#"["str/truncate", "Hello World", 8]"#), Value::string("Hello Wo..."));
    assert_eq!(ok(r#"["str/truncate", "Hello", 8, "..."]"#), Value::string("Hello"));
    assert_eq!(ok(r#"["str/truncate", "Hello Wo", 8, "..."]"#), Value::string("Hello Wo"));
}

#[test]
fn validate_check_min_max_range_rules_run_as_rules_not_as_calls() {
    let result = ok(r#"["validate/check", {"age": 17, "n": 70, "r": 5},
        {"age": [["min", 18]], "n": [["max", 65]], "r": [["range", 1, 10]]}]"#);
    let Value::Object(map) = result else { panic!("not an object") };
    assert_eq!(map["valid"], Value::Bool(false));
    assert_eq!(
        map["errors"],
        Value::array(vec![
            Value::String("age: min validation failed".into()),
            Value::String("n: max validation failed".into()),
        ])
    );
    let control = ok(r#"["validate/check", {"age": 18, "n": 65}, {"age": [["min", 18]], "n": [["max", 65]]}]"#);
    let Value::Object(map) = control else { panic!("not an object") };
    assert_eq!(map["valid"], Value::Bool(true));
}

#[test]
fn validate_check_rule_arguments_are_evaluated_and_a_predicate_is_refused() {
    let bound = ok(r#"["let", [["floor", 3]], ["validate/check", {"s": "ab"}, {"s": [["minLength", "@floor"]]}]]"#);
    let Value::Object(map) = bound else { panic!("not an object") };
    assert_eq!(map["valid"], Value::Bool(false));
    let message = err(r#"["validate/check", 5, ["fn", "v", true]]"#);
    assert!(message.contains("rules by field"), "{message}");
}

#[derive(Deserialize)]
struct Vector {
    expr: serde_json::Value,
    expect: serde_json::Value,
    source: String,
}

fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Number(x), serde_json::Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            (x - y).abs() <= 1e-12 * 1f64.max(x.abs()).max(y.abs())
        }
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, p)| y.get(k).is_some_and(|q| same(p, q)))
        }
        _ => a == b,
    }
}

fn shared(raw: &str) -> Vec<String> {
    let vectors: Vec<Vector> = serde_json::from_str(raw).unwrap();
    assert!(vectors.len() >= 10, "the fixture is the contract");
    vectors
        .iter()
        .filter_map(|v| match run(&v.expr.to_string()) {
            Ok(got) => {
                let got = serde_json::to_value(got).unwrap();
                (!same(&v.expect, &got)).then(|| format!("{}: want {}, got {got}", v.source, v.expect))
            }
            Err(e) => Some(format!("{}: want {}, got error {e}", v.source, v.expect)),
        })
        .collect()
}

#[test]
fn every_shared_validate_vector_matches_the_js_evaluator() {
    let failures = shared(include_str!("../../../../tests/fixtures/operators/validate-vectors.json"));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[derive(Deserialize)]
struct NullProbe {
    operator: String,
    position: usize,
    expr: serde_json::Value,
    rejects: bool,
}

#[test]
fn the_kernel_raises_on_a_null_operand_exactly_where_the_operator_table_flags_it() {
    let probes: Vec<NullProbe> =
        serde_json::from_str(include_str!("../../../../tests/fixtures/operators/null-operands.json")).unwrap();
    assert!(probes.iter().any(|p| p.rejects) && probes.iter().any(|p| !p.rejects));
    let mut failures = Vec::new();
    for probe in &probes {
        if let Err(e) = run(&probe.expr.to_string()) {
            failures.push(format!("{}: the baseline call fails: {e}", probe.operator));
            continue;
        }
        let mut nulled = probe.expr.clone();
        nulled[probe.position + 1] = serde_json::Value::Null;
        let raises = run(&nulled.to_string()).is_err();
        if raises != probe.rejects {
            failures.push(format!("{}[{}]: fixture says rejects={}, kernel raises={raises}", probe.operator, probe.position, probe.rejects));
        }
        let declared = crate::evaluator::operator_signature(&probe.operator)
            .is_some_and(|signature| signature.rejects_null(probe.position));
        if declared != probe.rejects {
            failures.push(format!("{}[{}]: the installed table flags rejectsNull={declared}, the fixture says {}", probe.operator, probe.position, probe.rejects));
        }
    }
    assert!(failures.is_empty(), "{} of {} probes differ:\n{}", failures.len(), probes.len(), failures.join("\n"));
}

#[test]
fn every_shared_format_vector_matches_the_js_evaluator() {
    let failures = shared(include_str!("../../../../tests/fixtures/operators/format-vectors.json"));
    assert!(failures.is_empty(), "{} differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn an_unsupported_locale_or_currency_answers_null_not_a_fallback() {
    assert_eq!(ok(r#"["format/number", 1, {"locale": "xx-YY"}]"#), Value::Null);
    assert_eq!(ok(r#"["format/currency", 1, "XYZ"]"#), Value::Null);
    assert_eq!(ok(r#"["format/currency", 1, "USD", "de-DE"]"#), Value::Null);
    assert_eq!(ok(r#"["format/number", 1, {"locale": "de-DE"}]"#), Value::string("1"));
    assert_eq!(ok(r#"["format/currency", 1, "USD"]"#), Value::string("$1.00"));
}

#[test]
fn bad_fraction_digits_answer_null() {
    for call in [
        r#"["format/number", 1, {"decimals": -1}]"#,
        r#"["format/number", 1, {"decimals": 1.5}]"#,
        r#"["format/number", 1, {"minimumFractionDigits": 3, "maximumFractionDigits": 1}]"#,
        r#"["format/percent", 0.5, 2.5]"#,
    ] {
        assert_eq!(ok(call), Value::Null, "{call}");
    }
    assert!(err(r#"["format/number", 1, {"decimals": "x"}]"#).contains("format/number"));
}

#[test]
fn format_number_honors_minimum_and_maximum_fraction_digits() {
    assert_eq!(ok(r#"["format/number", 5, {"minimumFractionDigits": 2, "maximumFractionDigits": 4}]"#), Value::string("5.00"));
    assert_eq!(ok(r#"["format/number", 5.123456, {"minimumFractionDigits": 5}]"#), Value::string("5.12346"));
    assert_eq!(ok(r#"["format/number", 5.123456, {"maximumFractionDigits": 1}]"#), Value::string("5.1"));
    assert_eq!(ok(r#"["format/number", 5.123456, {"decimals": 2, "maximumFractionDigits": 1}]"#), Value::string("5.12"));
}

fn value(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

/// A whole `@trait.X` string is a client-only frame reference: it evaluates to itself, as the value resolver and the
/// embedded interpolation already keep it, so a branch an operator chooses still embeds the trait.
// EXEC-EVAL-9
#[test]
fn a_trait_reference_evaluates_to_itself_inside_an_operator_branch() {
    assert_eq!(
        ok(r#"["if", true, {"type": "box", "children": ["@trait.HeroArt"]}, null]"#),
        value(r#"{"type": "box", "children": ["@trait.HeroArt"]}"#)
    );
}

#[test]
fn control_an_unresolved_entity_binding_still_evaluates_to_null() {
    assert_eq!(ok(r#"["if", true, "@entity.missing", 1]"#), Value::Null);
}
