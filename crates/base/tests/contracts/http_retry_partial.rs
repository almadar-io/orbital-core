use orbital_core_base::value::Value;
use orbital_core_base::ir::OirType;
use orbital_core_base::http_wire::{parse_retry, HttpRefusal, HttpRetry, RetryDelay, RetryOn, RetryOperand};
use serde_json::json;

fn literal(value: serde_json::Value) -> RetryOperand {
    RetryOperand::Literal(serde_json::from_value(value).unwrap())
}

fn typed_array(inner: OirType) -> RetryOperand {
    RetryOperand::Typed(OirType::Array {
        inner: Box::new(inner),
    })
}

fn backoff(
    max: RetryOperand,
    on: RetryOperand,
    delay: RetryOperand,
) -> Vec<(String, RetryOperand)> {
    vec![
        ("max".into(), max),
        ("on".into(), on),
        ("backoffMs".into(), delay),
    ]
}

fn schedule(
    max: RetryOperand,
    on: RetryOperand,
    delays: RetryOperand,
) -> Vec<(String, RetryOperand)> {
    vec![
        ("max".into(), max),
        ("on".into(), on),
        ("scheduleMs".into(), delays),
    ]
}

fn cap(
    mut fields: Vec<(String, RetryOperand)>,
    value: RetryOperand,
) -> Vec<(String, RetryOperand)> {
    fields.push(("noResponseMax".into(), value));
    fields
}

fn deferred(fields: Vec<(String, RetryOperand)>) {
    assert_eq!(HttpRetry::check_partial(&fields), Ok(None));
}

fn invalid(fields: Vec<(String, RetryOperand)>) {
    assert!(matches!(
        HttpRetry::check_partial(&fields),
        Err(HttpRefusal::Invalid(_))
    ));
}

fn concrete_operand(value: Value, expand_items: bool) -> RetryOperand {
    match value {
        Value::Array(values) if expand_items => RetryOperand::Items(
            values
                .into_iter()
                .map(|value| concrete_operand(value, true))
                .collect(),
        ),
        value => RetryOperand::Literal(value),
    }
}

fn equivalent(value: serde_json::Value, expand_items: bool) {
    let value: Value = serde_json::from_value(value).unwrap();
    let Value::Object(map) = &value else {
        panic!("retry fixture is not an object")
    };
    let fields: Vec<_> = map
        .iter()
        .map(|(name, value)| (name.clone(), concrete_operand(value.clone(), expand_items)))
        .collect();
    assert_eq!(
        HttpRetry::check_partial(&fields),
        parse_retry(&value).map(Some),
        "concrete equivalence: {value:?}"
    );
}

#[test]
fn concrete_old_and_new_valid_policies_are_exact_runtime_parse_results() {
    for policy in [
        json!({"max":1,"on":["5xx"],"backoffMs":0}),
        json!({"max":2,"on":["429","timeout"],"backoffMs":10}),
        json!({"max":3,"on":["1xx","408","5xx","429","failed","timeout"],"scheduleMs":[100,500,1500],"noResponseMax":2}),
        json!({"max":1,"on":["failed","failed"],"scheduleMs":[0],"noResponseMax":0}),
        json!({"max":4294967295u64,"on":["timeout"],"backoffMs":0,"noResponseMax":4294967295u64}),
    ] {
        equivalent(policy, false);
    }
}

#[test]
fn concrete_invalid_policies_preserve_runtime_error_order_and_message() {
    for policy in [
        json!({}),
        json!({"extra":true,"max":0,"on":[],"backoffMs":-1}),
        json!({"max":0,"on":[],"backoffMs":-1}),
        json!({"max":null,"on":["5xx"],"backoffMs":0}),
        json!({"max":1,"on":null,"backoffMs":0}),
        json!({"max":1,"on":[],"backoffMs":-1}),
        json!({"max":1,"on":["4xx"],"backoffMs":-1}),
        json!({"max":1,"on":["5xx"],"backoffMs":null}),
        json!({"max":1,"on":["5xx"],"backoffMs":0,"scheduleMs":[0]}),
        json!({"max":1,"on":["5xx"]}),
        json!({"max":2,"on":["5xx"],"scheduleMs":[-1],"noResponseMax":3}),
        json!({"max":1,"on":["5xx"],"scheduleMs":[null]}),
        json!({"max":1,"on":["5xx"],"scheduleMs":[0],"noResponseMax":null}),
        json!({"max":1,"on":["5xx"],"backoffMs":0,"noResponseMax":2}),
    ] {
        equivalent(policy, false);
    }
    for (policy, message) in [
        (
            json!({"extra":true,"max":0,"on":[],"backoffMs":-1}),
            "retry has no key `extra`",
        ),
        (
            json!({"max":0,"on":[],"backoffMs":-1}),
            "retry.max must be a whole number of at least 1",
        ),
        (
            json!({"max":1,"on":["bad"],"backoffMs":-1}),
            "retry.on entries are 1xx, 408, 5xx, 429, timeout, failed or an integer status code from 100 through 599",
        ),
        (
            json!({"max":1,"on":["5xx"],"backoffMs":null}),
            "retry.backoffMs is required",
        ),
        (
            json!({"max":2,"on":["5xx"],"scheduleMs":[-1],"noResponseMax":3}),
            "retry.scheduleMs must contain exactly max whole-number delays",
        ),
        (
            json!({"max":1,"on":["5xx"],"backoffMs":0,"noResponseMax":null}),
            "retry.noResponseMax must be a whole number from zero through max",
        ),
    ] {
        let value: Value = serde_json::from_value(policy).unwrap();
        let Value::Object(map) = &value else {
            panic!("retry fixture is not an object")
        };
        let fields: Vec<_> = map
            .iter()
            .map(|(name, value)| (name.clone(), RetryOperand::Literal(value.clone())))
            .collect();
        let expected = HttpRefusal::Invalid(message.into());
        assert_eq!(parse_retry(&value), Err(expected.clone()));
        assert_eq!(HttpRetry::check_partial(&fields), Err(expected));
    }
}

#[test]
fn fully_concrete_items_preserve_runtime_success_and_failure() {
    for policy in [
        json!({"max":2,"on":["5xx","failed","failed"],"scheduleMs":[0,3],"noResponseMax":1}),
        json!({"max":1,"on":["timeout"],"backoffMs":0}),
        json!({"max":1,"on":[],"scheduleMs":[0]}),
        json!({"max":2,"on":["5xx"],"scheduleMs":[0]}),
        json!({"max":1,"on":["bad"],"scheduleMs":[0]}),
        json!({"max":1,"on":["5xx"],"scheduleMs":[[0]]}),
    ] {
        equivalent(policy, true);
    }
}

#[test]
fn typed_numeric_fields_defer_without_materializing_values() {
    deferred(cap(
        backoff(
            RetryOperand::Typed(OirType::Number),
            literal(json!(["5xx"])),
            RetryOperand::Typed(OirType::Number),
        ),
        RetryOperand::Typed(OirType::Number),
    ));
}

#[test]
fn typed_string_and_number_arrays_defer_length_and_value_checks() {
    deferred(schedule(
        literal(json!(3)),
        typed_array(OirType::String),
        typed_array(OirType::Number),
    ));
}

#[test]
fn typed_string_on_entry_defers_only_its_tag_value() {
    deferred(backoff(
        literal(json!(1)),
        RetryOperand::Items(vec![
            literal(json!("5xx")),
            RetryOperand::Typed(OirType::String),
        ]),
        literal(json!(0)),
    ));
}

#[test]
fn unresolved_present_fields_defer_but_are_not_absent() {
    deferred(backoff(
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
    ));
}

#[test]
fn mixed_items_preserve_valid_known_neighbors_and_known_length() {
    deferred(schedule(
        literal(json!(3)),
        RetryOperand::Items(vec![literal(json!("failed")), RetryOperand::Unresolved]),
        RetryOperand::Items(vec![
            literal(json!(0)),
            RetryOperand::Typed(OirType::Number),
            RetryOperand::Unresolved,
        ]),
    ));
}

#[test]
fn invalid_known_on_neighbor_is_not_hidden_by_dynamic_max_or_other_items() {
    for invalid_entry in [json!("4xx"), json!(99), json!(600), json!(null)] {
        invalid(backoff(
            RetryOperand::Unresolved,
            RetryOperand::Items(vec![RetryOperand::Unresolved, literal(invalid_entry)]),
            RetryOperand::Unresolved,
        ));
    }
}

#[test]
fn invalid_known_schedule_neighbor_is_not_hidden_by_dynamic_max() {
    for invalid_entry in [json!(-1), json!(0.5), json!(null), json!("1")] {
        invalid(schedule(
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
            RetryOperand::Items(vec![RetryOperand::Unresolved, literal(invalid_entry)]),
        ));
    }
}

#[test]
fn known_schedule_count_mismatch_is_not_hidden_by_dynamic_entries() {
    invalid(schedule(
        literal(json!(3)),
        RetryOperand::Unresolved,
        RetryOperand::Items(vec![RetryOperand::Unresolved, RetryOperand::Unresolved]),
    ));
}

#[test]
fn known_empty_on_is_invalid_even_with_other_unresolved_fields() {
    invalid(backoff(
        RetryOperand::Unresolved,
        RetryOperand::Items(vec![]),
        RetryOperand::Unresolved,
    ));
}

#[test]
fn dynamic_max_and_empty_fixed_schedule_are_always_contradictory() {
    invalid(schedule(
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
        RetryOperand::Items(vec![]),
    ));
}

#[test]
fn dynamic_max_fixed_schedule_one_and_cap_two_are_always_contradictory() {
    invalid(cap(
        schedule(
            RetryOperand::Typed(OirType::Number),
            RetryOperand::Unresolved,
            RetryOperand::Items(vec![RetryOperand::Unresolved]),
        ),
        literal(json!(2)),
    ));
}

#[test]
fn dynamic_max_fixed_schedule_three_and_cap_two_remain_deferred() {
    deferred(cap(
        schedule(
            RetryOperand::Typed(OirType::Number),
            typed_array(OirType::String),
            RetryOperand::Items(vec![
                RetryOperand::Unresolved,
                RetryOperand::Unresolved,
                RetryOperand::Unresolved,
            ]),
        ),
        literal(json!(2)),
    ));
}

#[test]
fn both_delay_keys_are_invalid_by_presence_even_when_values_are_dynamic_or_null() {
    for second in [RetryOperand::Unresolved, literal(json!(null))] {
        let mut fields = backoff(
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
        );
        fields.push(("scheduleMs".into(), second));
        invalid(fields);
    }
}

#[test]
fn absent_delay_keys_are_invalid_even_with_dynamic_required_fields() {
    invalid(vec![
        ("max".into(), RetryOperand::Unresolved),
        ("on".into(), RetryOperand::Unresolved),
    ]);
}

#[test]
fn missing_max_or_on_is_not_hidden_by_another_dynamic_field() {
    invalid(vec![
        ("on".into(), RetryOperand::Unresolved),
        ("backoffMs".into(), RetryOperand::Unresolved),
    ]);
    invalid(vec![
        ("max".into(), RetryOperand::Unresolved),
        ("backoffMs".into(), RetryOperand::Unresolved),
    ]);
}

#[test]
fn known_max_and_backoff_range_errors_survive_partial_values() {
    for max in [json!(0), json!(-1), json!(0.5), json!(4294967296u64)] {
        invalid(backoff(
            literal(max),
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
        ));
    }
    for delay in [json!(-1), json!(0.5), json!("0"), json!(null)] {
        invalid(backoff(
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
            literal(delay),
        ));
    }
}

#[test]
fn known_no_response_range_errors_survive_dynamic_max() {
    for maximum in [
        json!(-1),
        json!(0.5),
        json!(4294967296u64),
        json!(null),
        json!("0"),
    ] {
        invalid(cap(
            backoff(
                RetryOperand::Unresolved,
                RetryOperand::Unresolved,
                RetryOperand::Unresolved,
            ),
            literal(maximum),
        ));
    }
}

#[test]
fn known_no_response_exceeding_known_max_is_invalid_with_dynamic_neighbors() {
    invalid(cap(
        backoff(
            literal(json!(1)),
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
        ),
        literal(json!(2)),
    ));
}

#[test]
fn wrong_dynamic_scalar_types_are_not_runtime_deferrals() {
    invalid(backoff(
        RetryOperand::Typed(OirType::String),
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
    ));
    invalid(backoff(
        RetryOperand::Unresolved,
        RetryOperand::Typed(OirType::String),
        RetryOperand::Unresolved,
    ));
    invalid(backoff(
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
        RetryOperand::Typed(OirType::String),
    ));
    invalid(cap(
        backoff(
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
        ),
        RetryOperand::Typed(OirType::String),
    ));
}

#[test]
fn wrong_dynamic_array_element_types_are_not_runtime_deferrals() {
    deferred(backoff(
        RetryOperand::Unresolved,
        typed_array(OirType::Number),
        RetryOperand::Unresolved,
    ));
    invalid(backoff(
        RetryOperand::Unresolved,
        typed_array(OirType::Boolean),
        RetryOperand::Unresolved,
    ));
    invalid(schedule(
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
        typed_array(OirType::String),
    ));
    invalid(schedule(
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
        RetryOperand::Typed(OirType::Number),
    ));
    deferred(schedule(
        RetryOperand::Unresolved,
        RetryOperand::Items(vec![RetryOperand::Typed(OirType::Number)]),
        RetryOperand::Unresolved,
    ));
    invalid(schedule(
        RetryOperand::Unresolved,
        RetryOperand::Items(vec![RetryOperand::Typed(OirType::Boolean)]),
        RetryOperand::Unresolved,
    ));
    invalid(schedule(
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
        RetryOperand::Items(vec![RetryOperand::Typed(OirType::String)]),
    ));
}

#[test]
fn absent_optional_cap_differs_from_explicit_null_and_typed_number() {
    let fields = backoff(
        literal(json!(1)),
        literal(json!(["5xx"])),
        literal(json!(0)),
    );
    assert!(matches!(HttpRetry::check_partial(&fields), Ok(Some(_))));
    invalid(cap(
        backoff(
            literal(json!(1)),
            literal(json!(["5xx"])),
            literal(json!(0)),
        ),
        literal(json!(null)),
    ));
    deferred(cap(
        backoff(
            literal(json!(1)),
            literal(json!(["5xx"])),
            literal(json!(0)),
        ),
        RetryOperand::Typed(OirType::Number),
    ));
}

#[test]
fn closed_keys_are_checked_even_when_their_value_and_other_fields_are_dynamic() {
    let mut fields = backoff(
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
    );
    fields.push(("extra".into(), RetryOperand::Unresolved));
    invalid(fields);
}

#[test]
fn nonfinite_direct_values_are_invalid_despite_unknown_neighbors() {
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        invalid(backoff(
            RetryOperand::Literal(Value::Number(number)),
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
        ));
        invalid(backoff(
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
            RetryOperand::Literal(Value::Number(number)),
        ));
        invalid(cap(
            backoff(
                RetryOperand::Unresolved,
                RetryOperand::Unresolved,
                RetryOperand::Unresolved,
            ),
            RetryOperand::Literal(Value::Number(number)),
        ));
        invalid(schedule(
            RetryOperand::Unresolved,
            RetryOperand::Unresolved,
            RetryOperand::Items(vec![
                RetryOperand::Unresolved,
                RetryOperand::Literal(Value::Number(number)),
            ]),
        ));
    }
}

#[test]
fn exact_legacy_two_to_64_delay_saturates_but_the_next_float_is_rejected() {
    let boundary = 18446744073709551616.0_f64;
    let above = f64::from_bits(boundary.to_bits() + 1);
    for field in ["backoffMs", "scheduleMs"] {
        let mut value: Value = serde_json::from_value(json!({"max":1,"on":["5xx"]})).unwrap();
        let Value::Object(map) = &mut value else {
            panic!("retry fixture is not an object")
        };
        map.insert(
            field.into(),
            if field == "backoffMs" {
                Value::Number(boundary)
            } else {
                Value::Array(vec![Value::Number(boundary)])
            },
        );
        let expected = HttpRetry {
            max: 1,
            on: vec![RetryOn::Server],
            delay: if field == "backoffMs" {
                RetryDelay::Backoff {
                    backoff_ms: u64::MAX,
                }
            } else {
                RetryDelay::Schedule {
                    schedule_ms: vec![u64::MAX],
                }
            },
            no_response_max: None,
        };
        let Value::Object(map) = &value else {
            panic!("retry fixture is not an object")
        };
        let fields: Vec<_> = map
            .iter()
            .map(|(name, value)| (name.clone(), RetryOperand::Literal(value.clone())))
            .collect();
        assert_eq!(parse_retry(&value), Ok(expected.clone()));
        assert_eq!(HttpRetry::check_partial(&fields), Ok(Some(expected)));
        let Value::Object(map) = &mut value else {
            panic!("retry fixture is not an object")
        };
        map.insert(
            field.into(),
            if field == "backoffMs" {
                Value::Number(above)
            } else {
                Value::Array(vec![Value::Number(above)])
            },
        );
        let Value::Object(map) = &value else {
            panic!("retry fixture is not an object")
        };
        let fields: Vec<_> = map
            .iter()
            .map(|(name, value)| (name.clone(), RetryOperand::Literal(value.clone())))
            .collect();
        let expected = HttpRefusal::Invalid(
            if field == "backoffMs" {
                "retry.backoffMs must be a whole number"
            } else {
                "retry.scheduleMs entry must be a whole number"
            }
            .into(),
        );
        assert_eq!(parse_retry(&value), Err(expected.clone()));
        assert_eq!(HttpRetry::check_partial(&fields), Err(expected));
    }
}

#[test]
fn typed_tuples_preserve_known_length_and_each_known_or_unknown_slot() {
    deferred(schedule(
        literal(json!(2)),
        RetryOperand::Typed(OirType::Tuple {
            items: vec![OirType::String, OirType::Json],
        }),
        RetryOperand::Typed(OirType::Tuple {
            items: vec![OirType::Number, OirType::Json],
        }),
    ));
    invalid(schedule(
        literal(json!(2)),
        RetryOperand::Unresolved,
        RetryOperand::Typed(OirType::Tuple {
            items: vec![OirType::Json, OirType::String],
        }),
    ));
    invalid(backoff(
        RetryOperand::Unresolved,
        RetryOperand::Typed(OirType::Tuple {
            items: vec![OirType::Json, OirType::Boolean],
        }),
        RetryOperand::Unresolved,
    ));
    invalid(schedule(
        literal(json!(2)),
        RetryOperand::Unresolved,
        RetryOperand::Typed(OirType::Tuple {
            items: vec![OirType::Json],
        }),
    ));
    invalid(schedule(
        RetryOperand::Unresolved,
        RetryOperand::Unresolved,
        RetryOperand::Typed(OirType::Tuple { items: vec![] }),
    ));
    invalid(backoff(
        RetryOperand::Unresolved,
        RetryOperand::Typed(OirType::Tuple { items: vec![] }),
        RetryOperand::Unresolved,
    ));
}

#[test]
fn dynamic_numeric_fields_allow_u32_max_cap_but_not_the_next_integer() {
    deferred(cap(
        backoff(
            RetryOperand::Typed(OirType::Number),
            RetryOperand::Unresolved,
            RetryOperand::Typed(OirType::Number),
        ),
        literal(json!(4294967295u64)),
    ));
    deferred(cap(
        backoff(
            literal(json!(4294967295u64)),
            RetryOperand::Unresolved,
            RetryOperand::Typed(OirType::Number),
        ),
        literal(json!(4294967295u64)),
    ));
    invalid(cap(
        backoff(
            RetryOperand::Typed(OirType::Number),
            RetryOperand::Unresolved,
            RetryOperand::Typed(OirType::Number),
        ),
        literal(json!(4294967296u64)),
    ));
    invalid(backoff(
        literal(json!(4294967296u64)),
        RetryOperand::Unresolved,
        RetryOperand::Typed(OirType::Number),
    ));
}

#[test]
fn exact_status_code_on_entries_parse_and_defer_like_tags() {
    let parsed = parse_retry(&serde_json::from_value(json!({"max":1,"on":[503,"429",503],"backoffMs":0})).unwrap()).unwrap();
    assert_eq!(parsed.on, vec![RetryOn::Status(503), RetryOn::TooManyRequests]);
    let wire = serde_json::to_value(&parsed).unwrap();
    assert_eq!(wire["on"], json!([503, "429"]));
    assert_eq!(serde_json::from_value::<HttpRetry>(wire).unwrap(), parsed);
    assert!(serde_json::from_value::<HttpRetry>(json!({"max":1,"on":[99],"backoffMs":0})).is_err());
    deferred(backoff(
        literal(json!(1)),
        RetryOperand::Items(vec![literal(json!(503)), RetryOperand::Typed(OirType::Number)]),
        literal(json!(0)),
    ));
    invalid(backoff(
        literal(json!(1)),
        RetryOperand::Items(vec![literal(json!(600))]),
        literal(json!(0)),
    ));
}
