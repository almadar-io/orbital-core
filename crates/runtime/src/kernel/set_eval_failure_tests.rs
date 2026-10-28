//! A `set` whose value fails to evaluate writes nothing, records a failed
//! effect and stops its transition: no later effect runs, and the step reports
//! none of its client effects or emits. Writes made before the failure stand.
//! JS twin: `effect-failure-contained.test.ts`.
use super::*;
use crate::entity_source::InMemoryEntityCache;

fn kernel() -> RuntimeKernel {
    let broken = serde_json::json!(["set", "@entity.app", ["behavior/ref", "Indigo Pioneer"]]);
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "SetFailure",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Page",
            "entity": { "name": "View", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Preview",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT",
                          "effects": [
                            ["set", "@entity.before", true],
                            ["render-ui", "main", { "type": "typography", "content": "loading" }],
                            broken.clone(),
                            ["set", "@entity.ok", true],
                            ["render-ui", "main", { "type": "typography", "content": "after" }],
                            ["emit", "AFTER"]
                          ] },
                        { "from": "idle", "to": "idle", "event": "NESTED",
                          "effects": [["do", broken.clone(), ["set", "@entity.ok", true]]] },
                        { "from": "idle", "to": "idle", "event": "PLAIN",
                          "effects": [
                            ["set", "@entity.before", true],
                            ["render-ui", "main", { "type": "typography", "content": "loading" }],
                            ["set", "@entity.ok", true],
                            ["render-ui", "main", { "type": "typography", "content": "after" }],
                            ["emit", "AFTER"]
                          ] }
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn run(event: &str) -> ProcessEventResult {
    let outcomes = kernel()
        .dispatch("Preview", None, event, Value::Null)
        .unwrap();
    outcomes
        .into_iter()
        .find(|(t, _)| t == "Preview")
        .expect("Preview ran")
        .1
}

fn has_set(result: &ProcessEventResult, field: &str) -> bool {
    let path = format!("@entity.{field}");
    result
        .effect_results
        .iter()
        .any(|r| matches!(&r.effect, Effect::Set { path: p, .. } if *p == path))
}

#[test]
fn a_set_that_fails_to_evaluate_writes_nothing_and_records_the_failure() {
    let result = run("INIT");
    let row_value = result.entity_data.to_value();
    let row = row_value.as_object().expect("an entity row");
    assert!(!row.contains_key("app"), "{row:?}");
    let failed = result
        .effect_results
        .iter()
        .find(|r| matches!(r.effect, Effect::Set { ref path, .. } if path == "@entity.app"))
        .expect("the set is recorded");
    assert!(
        matches!(failed.outcome, EffectOutcome::Failed),
        "{:?}",
        failed.outcome
    );
    assert!(
        failed
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("specifier"),
        "{:?}",
        failed.error
    );
}

#[test]
fn a_failed_effect_stops_the_remaining_effects_of_its_transition() {
    let result = run("INIT");
    let row_value = result.entity_data.to_value();
    let row = row_value.as_object().expect("an entity row");
    assert!(!row.contains_key("ok"), "{row:?}");
    assert!(!has_set(&result, "ok"));
    assert_eq!(result.effect_runs.len(), 6, "one run per authored effect");
    assert!(
        result.effect_runs[3..]
            .iter()
            .all(|r| r.status == EffectRunStatus::Skipped),
        "{:?}",
        result.effect_runs
    );
}

#[test]
fn writes_before_the_failure_stand() {
    let row = run("INIT").entity_data.to_value();
    assert_eq!(
        row.as_object().and_then(|o| o.get("before")),
        Some(&Value::Bool(true))
    );
}

#[test]
fn the_failing_step_reports_no_client_effect_and_no_emit() {
    let result = run("INIT");
    assert!(
        result
            .effect_results
            .iter()
            .all(|r| !matches!(r.effect, Effect::RenderUi { .. } | Effect::Emit { .. })),
        "{:?}",
        result.effect_results
    );
}

#[test]
fn a_failure_inside_a_do_stops_the_rest_of_the_block() {
    let result = run("NESTED");
    assert!(!has_set(&result, "ok"));
    assert!(result.failure.is_some());
}

#[test]
fn control_a_transition_with_no_failure_runs_every_effect() {
    let result = run("PLAIN");
    assert!(result.failure.is_none());
    let row_value = result.entity_data.to_value();
    let row = row_value.as_object().expect("an entity row");
    assert_eq!(row.get("ok"), Some(&Value::Bool(true)));
    let renders = result
        .effect_results
        .iter()
        .filter(|r| matches!(r.effect, Effect::RenderUi { .. }))
        .count();
    assert_eq!(renders, 2);
    assert!(result
        .effect_results
        .iter()
        .any(|r| matches!(r.effect, Effect::Emit { .. })));
    assert!(result
        .effect_runs
        .iter()
        .all(|r| r.status != EffectRunStatus::Failed));
}
