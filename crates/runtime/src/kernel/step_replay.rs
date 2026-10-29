//! Replaying authored steps on a kernel: each step seeds a trait (`from`, `entity`, `viewer`), pins the clock (`now`),
//! dispatches one event and reports every outcome of the cascade it started. `orb execute --steps` and `program/step`
//! read this report; the host that owns the program text, its resolution and any live substrate supplies only the
//! dispatch.

use std::prelude::v1::*;

use crate::config::DispatchClock;
use crate::kernel::{RuntimeKernel, TraitSnapshot};
use crate::process_result::ProcessEventResult;
use crate::program_machine::{played_effect, played_ensures};
use crate::resumable::{drive_blocking, BlockingHost, DispatchOutcomes, IoAnswer, IoRequest};
use orbital_core_base::effect_types::Effect;
use orbital_core_base::schema::types::{OrbitalSchema, TraitDefinition, TraitReference};
use orbital_core_base::sync::Mutex;
use orbital_core_base::value::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The clock a replay's dispatches read: the step's `now` while it runs, the wall clock otherwise.
#[derive(Clone, Default)]
pub struct StepClock(Arc<Mutex<Option<f64>>>);

impl StepClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// The kernel's `KernelConfig.clock`.
    pub fn dispatch_clock(&self) -> DispatchClock {
        let pin = Arc::clone(&self.0);
        Arc::new(move || (*pin.lock()).unwrap_or_else(orbital_core_base::time_util::now_millis_f64))
    }

    fn pin(&self, now: Option<f64>) {
        *self.0.lock() = now;
    }
}

/// The inline trait definitions of a resolved program, by trait name.
pub fn trait_definitions(resolved: &OrbitalSchema) -> BTreeMap<String, Arc<TraitDefinition>> {
    resolved
        .orbitals
        .iter()
        .flat_map(|o| o.traits.iter())
        .filter_map(|t| match t {
            TraitReference::Inline(td) => Some((td.name.clone(), Arc::clone(td))),
            _ => None,
        })
        .collect()
}

fn to_value(json: &serde_json::Value) -> Result<Value, String> {
    serde_json::from_value(json.clone())
        .map_err(|e| format!("Failed to convert JSON to evaluator value: {e}"))
}

/// One trait's outcome of a dispatched step: the transition, the emits, the arm that fired, every guard tried and how
/// each authored effect of the fired arm ran.
pub fn outcome_report(trait_name: &str, r: &ProcessEventResult) -> serde_json::Value {
    let emits: Vec<serde_json::Value> = r
        .effect_results
        .iter()
        .filter(|er| !er.is_failed())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event, payload } => Some(serde_json::json!({
                "event": event,
                "payload": serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
            })),
            _ => None,
        })
        .collect();
    let guards: Vec<serde_json::Value> = r
        .guards
        .iter()
        .map(|g| {
            let mut entry = serde_json::json!({
                "arm": g.arm,
                "guard": serde_json::to_value(&*g.guard.guard).unwrap_or(serde_json::Value::Null),
                "passed": g.guard.passed,
            });
            if let Some(error) = &g.guard.error {
                entry["error"] = serde_json::Value::String(error.clone());
            }
            entry
        })
        .collect();
    let effects: Vec<serde_json::Value> = r
        .effect_runs
        .iter()
        .map(|run| serde_json::to_value(played_effect(run)).unwrap_or(serde_json::Value::Null))
        .collect();
    serde_json::json!({
        "trait": trait_name,
        "executed": r.executed,
        "previousState": r.previous_state,
        "newState": r.new_state,
        "emits": emits,
        "firedArm": r.fired_arm,
        "guards": guards,
        "effects": effects,
        "failure": r.failure,
    })
}

/// The program JSON text with each step's `config` knob values written over its trait's declared defaults; `None` when
/// no step overrides a knob. The compiler folds `@config` into the program at resolve time, so an override is the
/// program's for the whole call: two steps giving one trait's knob different values are refused.
pub fn with_step_config(
    program_json: &str,
    steps: &[serde_json::Value],
) -> Result<Option<String>, String> {
    let mut knobs: BTreeMap<(String, String), serde_json::Value> = BTreeMap::new();
    for (i, step) in steps.iter().enumerate() {
        match &step["config"] {
            serde_json::Value::Null => {}
            serde_json::Value::Object(values) => {
                let trait_name = step["trait"].as_str().unwrap_or_default().to_string();
                for (knob, value) in values {
                    match knobs.insert((trait_name.clone(), knob.clone()), value.clone()) {
                        Some(earlier) if &earlier != value => {
                            return Err(format!(
                                "step {i}: config.{knob} of {trait_name} is {value}, but an earlier step set it to {earlier}; \
                                 one call takes one value per knob (the compiler folds @config at resolve time)"
                            ))
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                return Err(format!(
                    "step {i}: `config` must be an object of knob values"
                ))
            }
        }
    }
    if knobs.is_empty() {
        return Ok(None);
    }
    let mut program: serde_json::Value = serde_json::from_str(program_json)
        .map_err(|e| format!("step config: the program is not .orb JSON: {e}"))?;
    for ((trait_name, knob), value) in knobs {
        let declared = program["orbitals"]
            .as_array_mut()
            .into_iter()
            .flatten()
            .filter_map(|o| o["traits"].as_array_mut())
            .flatten()
            .find(|t| t["name"] == trait_name.as_str() && t.get("stateMachine").is_some())
            .and_then(|t| t.get_mut("config"))
            .and_then(|c| c.get_mut(&knob))
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| format!("step config: {trait_name} declares no knob `{knob}`"))?;
        declared.insert("default".to_string(), value);
    }
    serde_json::to_string(&program)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// A replayed step's host answer: a keyed os/* or http ask is left pending (its answer event is a later step's to
/// play, G-ORB-459); every other ask is answered as a kernel with no host answers it.
pub fn step_answer(kernel: &RuntimeKernel, request: &IoRequest) -> IoAnswer {
    match request {
        IoRequest::Os { key: Some(_), .. }
        | IoRequest::Http { key: Some(_), .. }
        | IoRequest::Substrate {
            key: Some(_),
            site: Some(_),
            ..
        } => IoAnswer::ServiceDeferred,
        other => BlockingHost {
            source: kernel.entity_source().as_ref(),
            service: None,
            substrate: None,
        }
        .answer(other),
    }
}

/// The dispatch of a replay with no live host: asks answered by [`step_answer`].
pub fn replay_dispatch(
    kernel: &RuntimeKernel,
    trait_name: &str,
    event: &str,
    payload: Value,
) -> Result<DispatchOutcomes, String> {
    kernel
        .start_dispatch(trait_name, None, event, payload, None, None)
        .and_then(|progress| {
            drive_blocking(kernel, progress, &|request| step_answer(kernel, request))
        })
        .map_err(|e| e.to_string())
}

/// Plays `steps` on `kernel`. `live` is true when `dispatch` runs them on a live host (only then does a step take
/// `settleMs`, which `dispatch` receives); `dispatch` returns the outcomes of the cascade one event started.
/// The report is `{ success, steps: [{ outcomes, states, entities, entityAfter }], dispatchModes }`.
pub fn play_steps(
    kernel: &RuntimeKernel,
    clock: &StepClock,
    trait_defs: &BTreeMap<String, Arc<TraitDefinition>>,
    steps: &[serde_json::Value],
    live: bool,
    dispatch: &mut dyn FnMut(&str, &str, Value, Option<f64>) -> Result<DispatchOutcomes, String>,
) -> Result<serde_json::Value, String> {
    kernel.set_record_entity_after(true);
    let known = kernel.trait_snapshot(None);
    let mut reports = Vec::with_capacity(steps.len());
    for (i, step) in steps.iter().enumerate() {
        let field = |key: &str| {
            step[key]
                .as_str()
                .ok_or_else(|| format!("step {i}: `{key}` must be a string"))
        };
        let (trait_name, event) = (field("trait")?, field("event")?);
        if !known.contains_key(trait_name) {
            return Err(format!(
                "step {i}: the program has no trait \"{trait_name}\""
            ));
        }
        let seed = match &step["entity"] {
            serde_json::Value::Null => None,
            serde_json::Value::Object(fields) => Some(fields),
            _ => return Err(format!("step {i}: `entity` must be an object of fields")),
        };
        let viewer = match &step["viewer"] {
            serde_json::Value::Null => None,
            user @ serde_json::Value::Object(_) => Some(to_value(user)?),
            _ => return Err(format!("step {i}: `viewer` must be a user object or null")),
        };
        let settle_ms = match &step["settleMs"] {
            serde_json::Value::Null => None,
            serde_json::Value::Number(n) if live => n.as_f64(),
            serde_json::Value::Number(_) => {
                return Err(format!("step {i}: `settleMs` applies to a live call only"))
            }
            _ => {
                return Err(format!(
                    "step {i}: `settleMs` must be a number of milliseconds"
                ))
            }
        };
        let now = match &step["now"] {
            serde_json::Value::Null => None,
            serde_json::Value::Number(n) => n.as_f64(),
            _ => {
                return Err(format!(
                    "step {i}: `now` must be a number (ms since the epoch)"
                ))
            }
        };
        if step["from"].is_string() || seed.is_some() {
            let current = kernel.snapshot_trait(trait_name, None);
            let entity = match seed {
                None => current.entity,
                Some(fields) => {
                    let mut row = match current.entity {
                        Value::Object(row) => row,
                        _ => Default::default(),
                    };
                    for (key, value) in fields {
                        row.insert(key.clone().into(), to_value(value)?);
                    }
                    Value::Object(row)
                }
            };
            kernel.restore_trait(
                trait_name,
                None,
                TraitSnapshot {
                    state: step["from"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_default(),
                    entity,
                },
            );
        }
        clock.pin(now);
        let payload = match &step["payload"] {
            serde_json::Value::Null => Value::Null,
            p => to_value(p)?,
        };
        let viewing = viewer.map(|user| kernel.enter_viewer(user));
        let outcomes = dispatch(trait_name, event, payload, settle_ms);
        drop(viewing);
        clock.pin(None);
        let outcomes = outcomes.map_err(|e| format!("step {i} dispatch failed: {e}"))?;
        let entity_after = serde_json::to_value(kernel.snapshot_trait(trait_name, None).entity)
            .unwrap_or(serde_json::Value::Null);
        let entities: serde_json::Map<String, serde_json::Value> = outcomes
            .iter()
            .filter(|(_, r)| r.executed)
            .map(|(t, _)| {
                let row = kernel.snapshot_trait(t, None).entity;
                (
                    t.to_string(),
                    serde_json::to_value(&row).unwrap_or(serde_json::Value::Null),
                )
            })
            .collect();
        let outcomes: Vec<serde_json::Value> = outcomes
            .iter()
            .map(|(t, r)| {
                let mut report = outcome_report(t, r);
                report["orbital"] = kernel
                    .orbital_of_trait(t)
                    .map_or(serde_json::Value::Null, serde_json::Value::String);
                report["event"] =
                    serde_json::Value::String(r.event.clone().unwrap_or_else(|| event.to_string()));
                let ensures = trait_defs
                    .get(t.as_str())
                    .map(|td| played_ensures(td, r.event.as_deref().unwrap_or(event), r))
                    .unwrap_or_default();
                report["ensures"] =
                    serde_json::to_value(ensures).unwrap_or(serde_json::Value::Array(Vec::new()));
                report
            })
            .collect();
        let states: serde_json::Map<String, serde_json::Value> = kernel
            .trait_snapshot(None)
            .into_iter()
            .map(|(name, (state, _))| (name, serde_json::Value::String(state)))
            .collect();
        reports.push(serde_json::json!({ "outcomes": outcomes, "states": states, "entities": entities, "entityAfter": entity_after }));
    }
    let dispatch_modes: serde_json::Map<String, serde_json::Value> = kernel
        .trait_snapshot(None)
        .into_iter()
        .map(|(name, (_, mode))| {
            (
                name,
                serde_json::to_value(mode).unwrap_or(serde_json::Value::Null),
            )
        })
        .collect();
    Ok(serde_json::json!({ "success": true, "steps": reports, "dispatchModes": dispatch_modes }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KernelConfig;
    use crate::entity_source::{EntitySource, InMemoryEntityCache};
    use orbital_core_base::effect_types::RuntimeEnvironment;

    const MODES: &str = r#"{"name":"EditorApp","typeContractVersion":1,"orbitals":[{"name":"Editor","entity":{"name":"Doc","persistence":"runtime","fields":[{"name":"id","type":"string"},{"name":"count","type":"number","default":0}]},"pages":[],"traits":[{"name":"Modes","linkedEntity":"Doc","stateMachine":{"states":[{"name":"NORMAL","isInitial":true},{"name":"INSERT"},{"name":"VISUAL"}],"transitions":[{"from":"NORMAL","to":"INSERT","event":"KEY","guard":["=","@payload.key","i"],"effects":[["set","@entity.count",["+","@entity.count",1]]]},{"from":"NORMAL","to":"VISUAL","event":"KEY","guard":["=","@payload.key","v"]}]}}]}]}"#;

    fn play(steps: serde_json::Value) -> Result<serde_json::Value, String> {
        let schema: OrbitalSchema = serde_json::from_str(MODES).unwrap();
        let clock = StepClock::new();
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig {
                clock: Some(clock.dispatch_clock()),
                ..Default::default()
            },
        );
        let defs = trait_definitions(&schema);
        kernel.register_schema(schema).unwrap();
        let steps = steps.as_array().unwrap().clone();
        play_steps(&kernel, &clock, &defs, &steps, false, &mut |t, e, p, _| {
            replay_dispatch(&kernel, t, e, p)
        })
    }

    #[test]
    fn a_step_with_from_plays_from_that_state_and_reports_the_row_it_moved() {
        let report = play(serde_json::json!([
            { "trait": "Modes", "event": "KEY", "payload": { "key": "i" } },
            { "trait": "Modes", "event": "KEY", "payload": { "key": "v" }, "from": "NORMAL" }
        ]))
        .unwrap();
        let second = &report["steps"][1]["outcomes"][0];
        assert_eq!(second["previousState"], "NORMAL");
        assert_eq!(second["newState"], "VISUAL");
        assert_eq!(second["firedArm"], 1);
        assert_eq!(
            report["steps"][0]["entities"]["Modes"]["count"],
            serde_json::json!(1.0),
            "{report}"
        );
    }

    #[test]
    fn control_without_from_the_step_plays_where_the_last_one_left_the_trait_and_moves_no_row() {
        let report = play(serde_json::json!([
            { "trait": "Modes", "event": "KEY", "payload": { "key": "i" } },
            { "trait": "Modes", "event": "KEY", "payload": { "key": "v" } }
        ]))
        .unwrap();
        let second = &report["steps"][1]["outcomes"][0];
        assert_eq!(second["previousState"], "INSERT");
        assert_eq!(second["executed"], false);
        assert_eq!(
            report["steps"][1]["entities"],
            serde_json::json!({}),
            "{report}"
        );
    }

    #[test]
    fn a_seeded_entity_is_the_row_the_step_starts_from() {
        let report = play(serde_json::json!([
            { "trait": "Modes", "event": "KEY", "payload": { "key": "i" }, "entity": { "count": 41 } }
        ]))
        .unwrap();
        assert_eq!(
            report["steps"][0]["entityAfter"]["count"],
            serde_json::json!(42.0),
            "{report}"
        );
    }

    #[test]
    fn edge_a_malformed_step_or_unknown_trait_is_refused() {
        let refusal = |step: serde_json::Value| play(serde_json::json!([step])).unwrap_err();
        assert!(
            refusal(serde_json::json!({ "trait": "Nope", "event": "KEY" })).contains("\"Nope\"")
        );
        assert!(
            refusal(serde_json::json!({ "trait": "Modes" })).contains("`event` must be a string")
        );
        assert!(
            refusal(serde_json::json!({ "trait": "Modes", "event": "KEY", "entity": 3 }))
                .contains("`entity`")
        );
        assert!(
            refusal(serde_json::json!({ "trait": "Modes", "event": "KEY", "now": "x" }))
                .contains("`now`")
        );
        assert!(
            refusal(serde_json::json!({ "trait": "Modes", "event": "KEY", "settleMs": 5 }))
                .contains("live call only")
        );
    }

    #[test]
    fn a_pinned_now_is_what_the_clock_reads_only_while_the_step_runs() {
        let clock = StepClock::new();
        let read = clock.dispatch_clock();
        clock.pin(Some(7.0));
        assert_eq!(read(), 7.0);
        clock.pin(None);
        assert!(read() > 7.0);
    }

    #[test]
    fn step_config_overrides_a_declared_knob_default_and_refuses_conflicts_and_unknown_knobs() {
        let program = r#"{"orbitals":[{"traits":[{"name":"T","stateMachine":{},"config":{"limit":{"default":1}}}]}]}"#;
        let steps = |a: i64, b: i64| {
            vec![
                serde_json::json!({ "trait": "T", "config": { "limit": a } }),
                serde_json::json!({ "trait": "T", "config": { "limit": b } }),
            ]
        };
        let folded = with_step_config(program, &steps(5, 5)).unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&folded).unwrap()["orbitals"][0]["traits"][0]
                ["config"]["limit"]["default"],
            5
        );
        assert!(with_step_config(program, &steps(5, 6))
            .unwrap_err()
            .contains("earlier step set it"));
        assert!(with_step_config(
            program,
            &[serde_json::json!({ "trait": "T", "config": { "nope": 1 } })]
        )
        .unwrap_err()
        .contains("declares no knob"));
        assert_eq!(
            with_step_config(program, &[serde_json::json!({ "trait": "T" })]).unwrap(),
            None
        );
    }
}
