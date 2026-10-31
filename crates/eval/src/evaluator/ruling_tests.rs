//! Kernel rulings of 2026-10-18: declared arity is a bound, `__proto__` is an ordinary key, `validate/check` takes rules
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
    Evaluator::new()
        .evaluate(&expr, &BindingContext::new())
        .map_err(|e| e.to_string())
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
fn every_dispatched_operator_with_a_declared_maximum_refuses_one_extra_argument() {
    use crate::evaluator::operators::DISPATCHED_NAMES;
    use orbital_core_base::language::registry::operators;
    let table = operators();
    let mut checked = 0;
    let mut leaked = Vec::new();
    for name in DISPATCHED_NAMES {
        let Some(max) = table.get(*name).and_then(|operator| operator.max_arity) else {
            continue;
        };
        if name.starts_with("behavior/") {
            assert!(
                run(&format!(r#"["{name}"]"#)).is_err(),
                "{name} refuses every call at run time"
            );
            continue;
        }
        let extra = vec!["1"; max as usize + 1].join(", ");
        let call = format!(r#"["{name}", {extra}]"#);
        checked += 1;
        if !run(&call).is_err_and(|message| message.contains("Arity mismatch")) {
            leaked.push(*name);
        }
    }
    assert!(
        checked > 50,
        "the table declares maxima for many dispatched operators: {checked}"
    );
    assert!(leaked.is_empty(), "accepted an extra argument: {leaked:?}");
}

#[test]
fn let_refuses_an_extra_body_slot() {
    assert!(err(r#"["let", [["x", 1]], "@x", 2]"#).contains("Arity mismatch"));
    assert_eq!(ok(r#"["let", [["x", 1]], "@x"]"#), num(1.0));
}

#[test]
fn membership_operators_compare_structurally_never_across_kinds() {
    assert_eq!(ok(r#"["array/includes", [1], "1"]"#), Value::Bool(false));
    assert_eq!(ok(r#"["array/includes", [1], true]"#), Value::Bool(false));
    assert_eq!(ok(r#"["array/indexOf", [1, "1"], "1"]"#), num(1.0));
    assert_eq!(ok(r#"["array/indexOf", [true], 1]"#), num(-1.0));
    assert_eq!(
        ok(r#"["array/unique", [1, "1", true, 1]]"#)
            .as_array()
            .map(|items| items.len()),
        Some(3)
    );
    assert_eq!(
        ok(r#"["validate/oneOf", 1, ["list", "1", true]]"#),
        Value::Bool(false)
    );
    assert_eq!(
        ok(r#"["validate/noneOf", 1, ["list", "1", true]]"#),
        Value::Bool(true)
    );
    assert_eq!(ok(r#"["array/includes", [1], 1]"#), Value::Bool(true));
    assert_eq!(
        ok(r#"["validate/oneOf", "1", ["list", "1"]]"#),
        Value::Bool(true)
    );
}

#[test]
fn control_the_declared_arity_runs_and_a_variadic_operator_takes_any_count() {
    assert_eq!(ok(r#"["%", 7, 3]"#), num(1.0));
    assert_eq!(ok(r#"["math/map", 5, 0, 10, 0, 100]"#), num(50.0));
    assert_eq!(ok(r#"["/", 8, 2]"#), num(4.0));
    assert_eq!(ok(r#"["+", 1, 2, 3, 4, 5]"#), num(15.0));
}

#[test]
fn proto_is_an_ordinary_own_key_in_every_key_writing_object_operator() {
    assert_eq!(
        ok(r#"["object/get", ["object/set", {"a": 1}, "__proto__", 7], "__proto__"]"#),
        num(7.0)
    );
    assert_eq!(
        ok(r#"["object/get", ["object/set", {"a": 1}, "__proto__.x", 1], "__proto__.x"]"#),
        num(1.0)
    );
    assert_eq!(
        ok(r#"["object/get", ["object/set", {"a": {"b": 0}}, "a.__proto__", 2], "a.__proto__"]"#),
        num(2.0)
    );
    assert_eq!(
        ok(r#"["object/get", ["object/merge", {"a": 1}, {"__proto__": 3}], "__proto__"]"#),
        num(3.0)
    );
    assert_eq!(
        ok(
            r#"["object/get", ["object/deepMerge", {"a": {"b": 1}}, {"a": {"__proto__": 4}}], "a.__proto__"]"#
        ),
        num(4.0)
    );
    assert_eq!(
        ok(r#"["object/get", ["object/fromEntries", [["__proto__", 5]]], "__proto__"]"#),
        num(5.0)
    );
    assert_eq!(
        ok(
            r#"["object/get", ["object/mapKeys", {"a": 6}, ["fn", "k", "__proto__"]], "__proto__"]"#
        ),
        num(6.0)
    );
    assert_eq!(
        ok(r#"["object/keys", ["object/set", {"a": 1}, "__proto__", 7]]"#),
        ok(r#"["list", "a", "__proto__"]"#)
    );
    assert_eq!(
        ok(r#"["object/has", ["object/set", {"a": 1}, "__proto__", 7], "__proto__"]"#),
        Value::Bool(true)
    );
}

#[test]
fn object_set_on_an_object_with_a_computed_proto_key_is_not_null() {
    let call = r#"["object/set", {"a": 1}, ["str/concat", "__pro", "to__"], 9]"#;
    assert_ne!(ok(call), Value::Null);
    assert_eq!(
        ok(&format!(r#"["object/get", {call}, "__proto__"]"#)),
        num(9.0)
    );
}

#[test]
fn merge_deep_merge_from_entries_and_map_keys_never_answer_null_for_object_operands() {
    for call in [
        r#"["object/merge", {"a": 1}, {"b": 2}]"#,
        r#"["object/merge", {}, {}]"#,
        r#"["object/deepMerge", {"a": {"b": 1}}, {"a": {"c": 2}}]"#,
        r#"["object/deepMerge", {}, {"a": 1}]"#,
        r#"["object/fromEntries", [["a", 1], ["b", 2]]]"#,
        r#"["object/fromEntries", []]"#,
        r#"["object/mapKeys", {"a": 1}, ["fn", "k", ["str/upper", "@k"]]]"#,
        r#"["object/mapKeys", {}, ["fn", "k", "x"]]"#,
        r#"["object/merge", {"a": 1}, {"__proto__": 2}]"#,
    ] {
        assert!(matches!(ok(call), Value::Object(_)), "{call}");
    }
    for call in [
        r#"["object/merge", {"a": 1}, null]"#,
        r#"["object/mapKeys", null, ["fn", "k", "x"]]"#,
    ] {
        err(call);
    }
}

#[test]
fn control_ordinary_keys_including_look_alikes_are_written() {
    assert_eq!(
        ok(r#"["object/get", ["object/set", {"a": 1}, "b.c", 2], "b.c"]"#),
        num(2.0)
    );
    assert_eq!(
        ok(
            r#"["object/get", ["object/merge", {"a": 1}, {"proto": 2, "constructor": 3}], "constructor"]"#
        ),
        num(3.0)
    );
    assert_eq!(
        ok(r#"["object/get", ["object/deepMerge", {"a": {"b": 1}}, {"a": {"c": 2}}], "a.c"]"#),
        num(2.0)
    );
    assert_eq!(
        ok(r#"["object/get", ["object/fromEntries", [["__proto", 1]]], "__proto"]"#),
        num(1.0)
    );
}

#[test]
fn str_template_substitutes_double_braces_and_leaves_single_braces_and_unknown_keys() {
    assert_eq!(
        ok(r#"["str/template", "Hello, {{name}}!", {"name": "World"}]"#),
        Value::string("Hello, World!")
    );
    assert_eq!(
        ok(r#"["str/template", "Hello, {name}!", {"name": "World"}]"#),
        Value::string("Hello, {name}!")
    );
    assert_eq!(
        ok(r#"["str/template", "{{a}} {{b}}", {"a": 1}]"#),
        Value::string("1 {{b}}")
    );
}

#[test]
fn str_truncate_keeps_len_characters_then_appends_the_suffix() {
    assert_eq!(
        ok(r#"["str/truncate", "Hello World", 8, "..."]"#),
        Value::string("Hello Wo...")
    );
    assert_eq!(
        ok(r#"["str/truncate", "Hello World", 8]"#),
        Value::string("Hello Wo...")
    );
    assert_eq!(
        ok(r#"["str/truncate", "Hello", 8, "..."]"#),
        Value::string("Hello")
    );
    assert_eq!(
        ok(r#"["str/truncate", "Hello Wo", 8, "..."]"#),
        Value::string("Hello Wo")
    );
}

#[test]
fn validate_check_min_max_range_rules_run_as_rules_not_as_calls() {
    let result = ok(r#"["validate/check", {"age": 17, "n": 70, "r": 5},
        {"age": [["min", 18]], "n": [["max", 65]], "r": [["range", 1, 10]]}]"#);
    let Value::Object(map) = result else {
        panic!("not an object")
    };
    assert_eq!(map["valid"], Value::Bool(false));
    assert_eq!(
        map["errors"],
        Value::array(vec![
            Value::String("age: min validation failed".into()),
            Value::String("n: max validation failed".into()),
        ])
    );
    let control = ok(
        r#"["validate/check", {"age": 18, "n": 65}, {"age": [["min", 18]], "n": [["max", 65]]}]"#,
    );
    let Value::Object(map) = control else {
        panic!("not an object")
    };
    assert_eq!(map["valid"], Value::Bool(true));
}

#[test]
fn validate_check_rule_arguments_are_evaluated_and_a_predicate_is_refused() {
    let bound = ok(
        r#"["let", [["floor", 3]], ["validate/check", {"s": "ab"}, {"s": [["minLength", "@floor"]]}]]"#,
    );
    let Value::Object(map) = bound else {
        panic!("not an object")
    };
    assert_eq!(map["valid"], Value::Bool(false));
    let message = err(r#"["validate/check", {}, ["fn", "v", true]]"#);
    assert!(message.contains("rules by field"), "{message}");
}

#[derive(Deserialize)]
struct Vector {
    expr: serde_json::Value,
    #[serde(default)]
    expect: serde_json::Value,
    #[serde(default)]
    error: bool,
    source: String,
}

fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Number(x), serde_json::Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            (x - y).abs() <= 1e-12 * 1f64.max(x.abs()).max(y.abs())
        }
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
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
            Ok(got) if v.error => Some(format!(
                "{}: want an error, got {}",
                v.source,
                serde_json::to_value(got).unwrap()
            )),
            Err(_) if v.error => None,
            Ok(got) => {
                let got = serde_json::to_value(got).unwrap();
                (!same(&v.expect, &got))
                    .then(|| format!("{}: want {}, got {got}", v.source, v.expect))
            }
            Err(e) => Some(format!("{}: want {}, got error {e}", v.source, v.expect)),
        })
        .collect()
}

#[test]
fn every_shared_validate_vector_matches_the_js_evaluator() {
    let failures = shared(include_str!(
        "../../../../tests/fixtures/operators/validate-vectors.json"
    ));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[derive(Deserialize)]
struct NullProbe {
    operator: String,
    position: usize,
    expr: serde_json::Value,
}

/// S3 ratchet: `operator[position]` sites whose declared type excludes null but which still accept it. Only shrinks.
fn known_lenient() -> Vec<String> {
    serde_json::from_str(include_str!(
        "../../../../tests/fixtures/operators/null-lenient-known.json"
    ))
    .unwrap()
}

#[test]
fn the_kernel_raises_on_a_null_operand_exactly_where_the_declared_type_excludes_null() {
    let probes: Vec<NullProbe> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/operators/null-operands.json"
    ))
    .unwrap();
    let lenient = known_lenient();
    let mut failures = Vec::new();
    let mut still_lenient = Vec::new();
    for probe in &probes {
        if let Err(e) = run(&probe.expr.to_string()) {
            failures.push(format!("{}: the baseline call fails: {e}", probe.operator));
            continue;
        }
        let mut nulled = probe.expr.clone();
        nulled[probe.position + 1] = serde_json::Value::Null;
        let raises = run(&nulled.to_string()).is_err();
        let rejects = crate::evaluator::operator_signature(&probe.operator)
            .is_some_and(|signature| signature.rejects_null(probe.position));
        let site = format!("{}[{}]", probe.operator, probe.position);
        match (rejects, raises) {
            (true, false) => still_lenient.push(site),
            (false, true) => failures.push(format!(
                "{site}: the declared type admits null, but the kernel raises"
            )),
            _ => {}
        }
    }
    let new_lenient: Vec<&String> = still_lenient
        .iter()
        .filter(|s| !lenient.contains(s))
        .collect();
    let fixed: Vec<&String> = lenient
        .iter()
        .filter(|s| !still_lenient.contains(s))
        .collect();
    for site in new_lenient {
        failures.push(format!(
            "{site}: the declared type excludes null, but the kernel accepts it"
        ));
    }
    for site in fixed {
        failures.push(format!(
            "{site}: now raises; remove it from null-lenient-known.json"
        ));
    }
    assert!(
        failures.is_empty(),
        "{} of {} probes differ:\n{}",
        failures.len(),
        probes.len(),
        failures.join("\n")
    );
}

#[test]
fn every_shared_format_vector_matches_the_js_evaluator() {
    let failures = shared(include_str!(
        "../../../../tests/fixtures/operators/format-vectors.json"
    ));
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn an_unsupported_locale_or_currency_answers_null_not_a_fallback() {
    assert_eq!(
        ok(r#"["format/number", 1, {"locale": "xx-YY"}]"#),
        Value::Null
    );
    assert_eq!(ok(r#"["format/currency", 1, "XYZ"]"#), Value::Null);
    assert_eq!(ok(r#"["format/currency", 1, "USD", "de-DE"]"#), Value::Null);
    assert_eq!(
        ok(r#"["format/number", 1, {"locale": "de-DE"}]"#),
        Value::string("1")
    );
    assert_eq!(
        ok(r#"["format/currency", 1, "USD"]"#),
        Value::string("$1.00")
    );
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
    assert_eq!(
        ok(r#"["format/number", 5, {"minimumFractionDigits": 2, "maximumFractionDigits": 4}]"#),
        Value::string("5.00")
    );
    assert_eq!(
        ok(r#"["format/number", 5.123456, {"minimumFractionDigits": 5}]"#),
        Value::string("5.12346")
    );
    assert_eq!(
        ok(r#"["format/number", 5.123456, {"maximumFractionDigits": 1}]"#),
        Value::string("5.1")
    );
    assert_eq!(
        ok(r#"["format/number", 5.123456, {"decimals": 2, "maximumFractionDigits": 1}]"#),
        Value::string("5.12")
    );
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

#[test]
fn an_operator_that_answers_null_declares_a_nullable_result() {
    for (operator, call) in [
        ("array/first", r#"["array/first", ["list"]]"#),
        ("array/last", r#"["array/last", ["list"]]"#),
        (
            "array/find",
            r#"["array/find", ["list", 1, 2], ["fn", "x", [">", "@x", 5]]]"#,
        ),
        ("array/nth", r#"["array/nth", ["list", 1], 3]"#),
    ] {
        assert_eq!(
            run(call).unwrap(),
            Value::Null,
            "{operator} answers null here"
        );
        let declared = crate::evaluator::operator_signature(operator)
            .expect("declared")
            .returns();
        assert!(
            declared.admits_null(),
            "{operator} answers null, so its declared result must admit null: {declared:?}"
        );
    }
    let declared = crate::evaluator::operator_signature("array/len")
        .expect("declared")
        .returns();
    assert!(
        !declared.admits_null(),
        "control: array/len always answers a number"
    );
}
