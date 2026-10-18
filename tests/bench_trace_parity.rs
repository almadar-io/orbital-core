//! R0 of the embedded-kernel benchmark: the server-role `RuntimeKernel` must leave the same trace as B1
//! (`@almadar/runtime`) on the shared parity fixtures. The trace comparison itself has an in-process
//! control; the B1 comparison reads a bench result written by `@almadar-io/kernel-bench-tool`
//! (`ALMADAR_PARITY_FIXTURES` = the fixtures folder, `ALMADAR_PARITY_REFERENCE` = that JSON) and is
//! run with `-- --ignored`.

#[allow(dead_code)]
#[path = "../benches/parity_fixtures/r0_session.rs"]
mod r0_session;
#[allow(dead_code)]
#[path = "../benches/parity_fixtures/scenario_bench.rs"]
mod scenario_bench;

use r0_session::R0Driver;
use scenario_bench::{compare_traces, load_fixtures, reference_traces, run_scenario};
use serde_json::{json, Value};
use std::path::PathBuf;

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set")))
}

fn r0_differences_from_b1(fixture_names: &[&str]) -> Vec<String> {
    let fixtures = load_fixtures(&env_path("ALMADAR_PARITY_FIXTURES")).unwrap();
    let reference = reference_traces(&env_path("ALMADAR_PARITY_REFERENCE")).unwrap();
    let mut checked = 0;
    let mut differences = Vec::new();
    for fixture in fixtures
        .iter()
        .filter(|f| fixture_names.contains(&f.name.as_str()))
    {
        for (index, scenario) in fixture.scenarios.iter().enumerate() {
            let key = format!("{}#{index}", fixture.name);
            let want = reference
                .get(&key)
                .unwrap_or_else(|| panic!("no B1 trace for {key}"));
            checked += 1;
            match run_scenario(&R0Driver, fixture, scenario) {
                Err(e) => differences.push(format!("{key}: R0 failed: {e}")),
                Ok(trace) => {
                    if let Some(d) = compare_traces(&fixture.clock_fields, want, &trace) {
                        differences.push(format!(
                            "{key}: step {} field {}\n  B1: {}\n  R0: {}",
                            d.step_index + 1,
                            d.field,
                            d.expected,
                            d.actual
                        ));
                    }
                }
            }
        }
    }
    assert!(
        checked >= fixture_names.len(),
        "only {checked} scenarios ran for {fixture_names:?}"
    );
    differences
}

fn r0_equals_b1(fixture_names: &[&str]) {
    let differences = r0_differences_from_b1(fixture_names);
    assert!(
        differences.is_empty(),
        "R0 differs from B1:\n{}",
        differences.join("\n")
    );
}

#[test]
#[ignore = "needs ALMADAR_PARITY_FIXTURES and ALMADAR_PARITY_REFERENCE"]
fn r0_trace_equals_b1_on_audit_provenance_and_visit_key() {
    r0_equals_b1(&["audit-provenance", "visit-key"]);
}

#[test]
#[ignore = "needs ALMADAR_PARITY_FIXTURES and ALMADAR_PARITY_REFERENCE"]
fn r0_trace_equals_b1_on_every_fixture() {
    let fixtures = load_fixtures(&env_path("ALMADAR_PARITY_FIXTURES")).unwrap();
    let names: Vec<String> = fixtures
        .iter()
        .filter(|f| !f.scenarios.is_empty())
        .map(|f| f.name.clone())
        .collect();
    r0_equals_b1(&names.iter().map(String::as_str).collect::<Vec<_>>());
}

fn trace(state: &str, emit_payload: Value) -> Value {
    json!({ "steps": [{
        "trait": "T", "event": "GO", "states": { "T": state }, "effects": [],
        "emits": [{ "event": "DONE", "payload": emit_payload }]
    }] })
}

#[test]
fn equal_traces_compare_equal_and_integral_floats_are_integers() {
    let want = trace("idle", json!({ "n": 3, "b": 1 }));
    let got = trace("idle", json!({ "b": 1.0, "n": 3.0 }));
    assert_eq!(compare_traces(&[], &want, &got), None);
}

#[test]
fn a_different_state_is_reported_with_step_and_field() {
    let diff = compare_traces(&[], &trace("idle", json!({})), &trace("busy", json!({}))).unwrap();
    assert_eq!((diff.step_index, diff.field.as_str()), (0, "states"));
}

#[test]
fn a_different_emit_payload_is_reported_as_emits() {
    let diff = compare_traces(
        &[],
        &trace("idle", json!({ "n": 1 })),
        &trace("idle", json!({ "n": 2 })),
    )
    .unwrap();
    assert_eq!(diff.field, "emits");
}

#[test]
fn a_declared_clock_field_compares_as_a_number_never_by_value() {
    let clock = ["DONE.at".to_string()];
    let a = trace("idle", json!({ "at": 1 }));
    let b = trace("idle", json!({ "at": 99999 }));
    assert_eq!(compare_traces(&clock, &a, &b), None);
    assert!(compare_traces(&[], &a, &b).is_some());
    let not_a_number = trace("idle", json!({ "at": "x" }));
    assert!(compare_traces(&clock, &a, &not_a_number).is_some());
}

fn rows(stamp: &str) -> Value {
    json!({ "data": [{ "id": "a", "createdAt": stamp }, { "id": "b", "createdAt": stamp }], "totalCount": 2 })
}

#[test]
fn a_declared_clock_field_masks_an_iso_timestamp_through_an_array_of_rows() {
    let clock = ["DONE.data.*.createdAt".to_string()];
    let a = trace("idle", rows("2026-10-13T08:30:00.000Z"));
    let b = trace("idle", rows("2026-10-13T09:45:10.123Z"));
    assert_eq!(compare_traces(&clock, &a, &b), None);
    assert!(compare_traces(&[], &a, &b).is_some());
}

#[test]
fn control_a_non_timestamp_string_at_the_clock_path_still_differs() {
    let clock = ["DONE.data.*.createdAt".to_string()];
    let diff = compare_traces(&clock, &trace("idle", rows("soon")), &trace("idle", rows("later"))).unwrap();
    assert_eq!(diff.field, "emits");
}

#[test]
fn a_shorter_trace_is_a_length_difference() {
    let one = trace("idle", json!({}));
    let two = json!({ "steps": [one["steps"][0].clone(), one["steps"][0].clone()] });
    assert_eq!(compare_traces(&[], &two, &one).unwrap().field, "length");
}
