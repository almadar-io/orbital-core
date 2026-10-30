//! Replaying authored steps on a kernel: each step seeds a trait (`from`, `entity`, `viewer`), pins the clock (`now`),
//! dispatches one event and reports every outcome of the cascade it started. `orb execute --steps` and `program/step`
//! read this report; the host that owns the program text, its resolution and any live substrate supplies only the
//! dispatch.

use std::prelude::v1::*;

use crate::config::DispatchClock;
use crate::kernel::{RuntimeKernel, TraitSnapshot};
use crate::process_result::ProcessEventResult;
use crate::program_machine::{played_effect, played_ensures, PlayedEffect, PlayedEnsures};
use crate::resumable::{
    drive_blocking, BlockingHost, DispatchOutcomes, IoAnswer, IoRequest, PortHost,
};
use crate::secret_source::SecretSource;
use orbital_core_base::effect_types::Effect;
use orbital_core_base::schema::types::{OrbitalSchema, TraitDefinition, TraitReference};
use orbital_core_base::sync::Mutex;
use orbital_core_base::value::Value;
use orbital_core_base::dispatch_mode::DispatchMode;
use serde::{Deserialize, Serialize};
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

/// The credentials a replayed step holds: the step's `credentials` (name to value), answering the kernel's credential
/// asks while that step runs and nothing else. Never printed, never journaled.
#[derive(Default)]
pub struct StepCredentials(Mutex<BTreeMap<String, String>>);

impl StepCredentials {
    pub fn new() -> Self {
        Self::default()
    }

    fn hold(&self, held: BTreeMap<String, String>) {
        *self.0.lock() = held;
    }
}

impl SecretSource for StepCredentials {
    fn secret(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().get(name).cloned())
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

/// One event a step's arm emitted.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepEmit {
    pub event: String,
    #[cfg_attr(test, ts(type = "EventPayload | null"))]
    pub payload: serde_json::Value,
}

/// One guarded arm tried while choosing the arm that fires.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepGuard {
    pub arm: usize,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub guard: serde_json::Value,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One trait's outcome of a dispatched step: the transition, the emits, the arm that fired, every guard tried and how
/// each authored effect of the fired arm ran.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepOutcome {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub executed: bool,
    pub previous_state: String,
    pub new_state: String,
    pub emits: Vec<StepEmit>,
    pub fired_arm: Option<usize>,
    pub guards: Vec<StepGuard>,
    pub effects: Vec<PlayedEffect>,
    pub failure: Option<String>,
    pub orbital: String,
    pub event: String,
    pub ensures: Vec<PlayedEnsures>,
}

/// One step of a replay: every outcome of the cascade its event started, and the states and rows after it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    pub outcomes: Vec<StepOutcome>,
    pub states: BTreeMap<String, String>,
    #[cfg_attr(test, ts(type = "{ [key in string]: JsonValue }"))]
    pub entities: serde_json::Map<String, serde_json::Value>,
    #[cfg_attr(test, ts(type = "EventPayload | null"))]
    pub entity_after: serde_json::Value,
}

/// What `orb execute --steps` prints for a replay that ran, and `program/step { report: "raw" }` answers.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepReport {
    #[cfg_attr(test, ts(type = "true"))]
    pub success: bool,
    pub steps: Vec<StepResult>,
    pub dispatch_modes: BTreeMap<String, DispatchMode>,
}

/// What `orb execute` prints for a replay that could not run: the std command's answer, `{ success: false, error }`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRefusal {
    #[cfg_attr(test, ts(type = "false"))]
    pub success: bool,
    pub error: String,
}

/// One trait's outcome of a dispatched step, as the report keeps it.
pub fn outcome_report(
    trait_name: &str,
    orbital: String,
    event: &str,
    ensures: Vec<PlayedEnsures>,
    r: &ProcessEventResult,
) -> StepOutcome {
    let emits = r
        .effect_results
        .iter()
        .filter(|er| !er.is_failed())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event, payload } => Some(StepEmit {
                event: event.to_string(),
                payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
            }),
            _ => None,
        })
        .collect();
    let guards = r
        .guards
        .iter()
        .map(|g| StepGuard {
            arm: g.arm,
            guard: serde_json::to_value(&*g.guard.guard).unwrap_or(serde_json::Value::Null),
            passed: g.guard.passed,
            error: g.guard.error.clone(),
        })
        .collect();
    StepOutcome {
        trait_name: trait_name.to_string(),
        executed: r.executed,
        previous_state: r.previous_state.clone(),
        new_state: r.new_state.clone(),
        emits,
        fired_arm: r.fired_arm,
        guards,
        effects: r.effect_runs.iter().map(played_effect).collect(),
        failure: r.failure.clone(),
        orbital,
        event: event.to_string(),
        ensures,
    }
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

/// A replayed step's host answer: a keyed os/* ask is left pending (its answer event is a later step's to
/// play, G-ORB-459); a credential ask is answered from the step's `credentials`; every other ask is answered as a
/// kernel with no host answers it.
pub fn step_answer(
    kernel: &RuntimeKernel,
    credentials: &StepCredentials,
    request: &IoRequest,
) -> IoAnswer {
    match request {
        IoRequest::Os { key: Some(_), .. }
        | IoRequest::Substrate {
            key: Some(_),
            site: Some(_),
            ..
        } => IoAnswer::ServiceDeferred,
        IoRequest::Secret { .. } => BlockingHost {
            source: kernel.entity_source().as_ref(),
            service: None,
            substrate: None,
        }
        .answer_with_ports(
            &PortHost {
                secrets: credentials,
                on_event: &|_, _| {},
                #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
                os: None,
            },
            request,
        ),
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
    credentials: &StepCredentials,
    trait_name: &str,
    event: &str,
    payload: Value,
) -> Result<DispatchOutcomes, String> {
    kernel
        .start_dispatch(trait_name, None, event, payload, None, None)
        .and_then(|progress| {
            drive_blocking(kernel, progress, &|request| {
                step_answer(kernel, credentials, request)
            })
        })
        .map_err(|e| e.to_string())
}

/// Plays `steps` on `kernel`. A step's `credentials` are held in `credentials` while it runs. `live` is true when `dispatch` runs them on a live host (only then does a step take
/// `settleMs`, which `dispatch` receives); `dispatch` returns the outcomes of the cascade one event started.
/// The report is a [`StepReport`].
pub fn play_steps(
    kernel: &RuntimeKernel,
    clock: &StepClock,
    credentials: &StepCredentials,
    trait_defs: &BTreeMap<String, Arc<TraitDefinition>>,
    steps: &[serde_json::Value],
    live: bool,
    dispatch: &mut dyn FnMut(&str, &str, Value, Option<f64>) -> Result<DispatchOutcomes, String>,
) -> Result<StepReport, String> {
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
        let held = match &step["credentials"] {
            serde_json::Value::Null => BTreeMap::new(),
            serde_json::Value::Object(_) if live => {
                return Err(format!(
                    "step {i}: `credentials` apply to a replay with no live host only"
                ))
            }
            serde_json::Value::Object(named) => named
                .iter()
                .map(|(name, value)| {
                    value
                        .as_str()
                        .map(|v| (name.clone(), v.to_string()))
                        .ok_or_else(|| format!("step {i}: `credentials.{name}` must be a string"))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?,
            _ => {
                return Err(format!(
                    "step {i}: `credentials` must be an object of credential values"
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
        credentials.hold(held);
        let outcomes = dispatch(trait_name, event, payload, settle_ms);
        credentials.hold(BTreeMap::new());
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
        let outcomes = outcomes
            .iter()
            .map(|(t, r)| {
                let orbital = kernel
                    .orbital_of_trait(t)
                    .ok_or_else(|| format!("step {i}: the trait \"{t}\" belongs to no orbital"))?;
                let event = r.event.as_deref().unwrap_or(event);
                let ensures = trait_defs
                    .get(t.as_str())
                    .map(|td| played_ensures(td, event, r))
                    .unwrap_or_default();
                Ok(outcome_report(t, orbital, event, ensures, r))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let states = kernel
            .trait_snapshot(None)
            .into_iter()
            .map(|(name, (state, _))| (name, state))
            .collect();
        reports.push(StepResult { outcomes, states, entities, entity_after });
    }
    let dispatch_modes = kernel
        .trait_snapshot(None)
        .into_iter()
        .map(|(name, (_, mode))| (name, mode))
        .collect();
    Ok(StepReport { success: true, steps: reports, dispatch_modes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KernelConfig;
    use crate::entity_source::{EntitySource, InMemoryEntityCache};
    use orbital_core_base::effect_types::RuntimeEnvironment;

    const MODES: &str = r#"{"name":"EditorApp","typeContractVersion":1,"orbitals":[{"name":"Editor","entity":{"name":"Doc","persistence":"runtime","fields":[{"name":"id","type":"string"},{"name":"count","type":"number","default":0}]},"pages":[],"traits":[{"name":"Modes","linkedEntity":"Doc","stateMachine":{"states":[{"name":"NORMAL","isInitial":true},{"name":"INSERT"},{"name":"VISUAL"}],"transitions":[{"from":"NORMAL","to":"INSERT","event":"KEY","guard":["=","@payload.key","i"],"effects":[["set","@entity.count",["+","@entity.count",1]]]},{"from":"NORMAL","to":"VISUAL","event":"KEY","guard":["=","@payload.key","v"]}]}}]}]}"#;

    const KEYED: &str = r#"{"name":"KeyedApp","typeContractVersion":1,"orbitals":[{"name":"Keys","entity":{"name":"Job","persistence":"runtime","fields":[{"name":"id","type":"string"},{"name":"len","type":"number","default":-1}]},"pages":[],"traits":[{"name":"Host","linkedEntity":"Job","config":{"password":{"type":"secret","default":"PW_NAME","description":"the password credential"}},"stateMachine":{"states":[{"name":"idle","isInitial":true}],"transitions":[{"from":"idle","to":"idle","event":"GO","effects":[["set","@entity.len",["default",["credential/byte-length","@config.password"],-1]]]}]}}]}]}"#;

    fn play(steps: serde_json::Value) -> Result<serde_json::Value, String> {
        play_on(MODES, steps)
    }

    fn play_on(program: &str, steps: serde_json::Value) -> Result<serde_json::Value, String> {
        let schema: OrbitalSchema = serde_json::from_str(program).unwrap();
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
        let credentials = StepCredentials::new();
        play_steps(
            &kernel,
            &clock,
            &credentials,
            &defs,
            &steps,
            false,
            &mut |t, e, p, _| replay_dispatch(&kernel, &credentials, t, e, p),
        )
        .map(|report| serde_json::to_value(report).unwrap())
    }

    #[test]
    fn a_step_holds_only_its_own_credentials_and_the_report_never_shows_the_value() {
        let go = |step: serde_json::Value| play_on(KEYED, serde_json::json!([step]));
        let held = go(serde_json::json!({ "trait": "Host", "event": "GO", "credentials": { "PW_NAME": "hunter2" } })).unwrap();
        assert_eq!(
            held["steps"][0]["entityAfter"]["len"],
            serde_json::json!(7.0),
            "{held}"
        );
        assert!(
            !held.to_string().contains("hunter2"),
            "the credential value stays out of the report: {held}"
        );
        for unheld in [
            serde_json::json!({ "trait": "Host", "event": "GO" }),
            serde_json::json!({ "trait": "Host", "event": "GO", "credentials": { "OTHER": "hunter2" } }),
        ] {
            let report = go(unheld).unwrap();
            assert_eq!(
                report["steps"][0]["entityAfter"]["len"],
                serde_json::json!(-1.0),
                "control: not held is -1: {report}"
            );
        }
        let two = play_on(
            KEYED,
            serde_json::json!([
                { "trait": "Host", "event": "GO", "credentials": { "PW_NAME": "abc" } },
                { "trait": "Host", "event": "GO" }
            ]),
        )
        .unwrap();
        assert_eq!(
            two["steps"][0]["entityAfter"]["len"],
            serde_json::json!(3.0),
            "{two}"
        );
        assert_eq!(
            two["steps"][1]["entityAfter"]["len"],
            serde_json::json!(-1.0),
            "a step's credentials end with the step: {two}"
        );
        assert!(
            go(serde_json::json!({ "trait": "Host", "event": "GO", "credentials": 3 }))
                .unwrap_err()
                .contains("`credentials`")
        );
        assert!(go(
            serde_json::json!({ "trait": "Host", "event": "GO", "credentials": { "PW_NAME": 3 } })
        )
        .unwrap_err()
        .contains("must be a string"));
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

    #[test]
    fn the_report_is_the_typed_step_report_and_a_refusal_is_the_other_shape() {
        let report = play(serde_json::json!([
            { "trait": "Modes", "event": "KEY", "payload": { "key": "i" } }
        ]))
        .unwrap();
        let typed: StepReport = serde_json::from_value(report.clone()).expect("the report is a StepReport");
        assert!(typed.success);
        let outcome = &typed.steps[0].outcomes[0];
        assert_eq!((outcome.trait_name.as_str(), outcome.orbital.as_str(), outcome.event.as_str()), ("Modes", "Editor", "KEY"));
        assert_eq!((outcome.previous_state.as_str(), outcome.new_state.as_str()), ("NORMAL", "INSERT"));
        assert_eq!(outcome.fired_arm, Some(0));
        assert_eq!(serde_json::to_value(&typed).unwrap(), report, "the typed report prints what the replay printed");
        let refusal = serde_json::json!({ "success": false, "error": "no" });
        let parsed: StepRefusal = serde_json::from_value(refusal.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), refusal);
        assert!(
            serde_json::from_value::<StepReport>(refusal).is_err(),
            "control: a refusal carries no steps, so it is not a report"
        );
    }
}
