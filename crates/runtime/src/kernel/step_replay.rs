//! Replaying authored steps on a kernel: each step seeds a trait (`from`, `entity`, `identity`), pins the clock (`now`),
//! dispatches one event and reports every outcome of the cascade it started. `orb execute --steps` and `program/step`
//! read this report; the host that owns the program text, its resolution and any live substrate supplies only the
//! dispatch.

use std::prelude::v1::*;

use crate::config::DispatchClock;
use crate::kernel::{RuntimeKernel, TraitSnapshot};
use crate::process_result::{EffectRunStatus, ProcessEventResult};
use crate::program_machine::{
    played_effect, played_ensures, GuardVerdict, PlayRequest, PlayedEffect, PlayedEmit,
    PlayedEnsures, PlayedGuard, PlayedOutcome, PlayedResult, PlayedState, PlayedStep,
};
use crate::resumable::{
    drive_blocking, BlockingHost, CallSite, DispatchOutcomes, IoAnswer, IoRequest, PortHost,
};
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use crate::secret_source::SecretSource;
use orbital_core_base::dispatch_mode::DispatchMode;
use orbital_core_base::effect_types::Effect;
use orbital_core_base::schema::types::{OrbitalSchema, TraitDefinition, TraitReference};
use orbital_core_base::sync::Mutex;
use orbital_core_base::value::Value;
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

/// The host answers a replayed step scripts in its `answers` (G-CROSS-468): each is one `os/` ask's answer as a host gives
/// it, plain ids and all, so the kernel unwraps, mints, budgets and routes exactly as it does live. An ask takes the first
/// unused answer naming its operator; one no answer names stays pending, as without answers. A deferring op's answer is
/// delivered as its completion once the dispatch that asked has ended, through [`RuntimeKernel::dispatch_route`].
#[derive(Default)]
pub struct StepAnswers {
    scripted: Mutex<Vec<(OsOp, Option<Result<Value, OsError>>)>>,
    deferred: Mutex<Vec<(CallSite, String, Result<Value, OsError>)>>,
}

impl StepAnswers {
    pub fn new() -> Self {
        Self::default()
    }

    fn hold(&self, scripted: Vec<(OsOp, Result<Value, OsError>)>) {
        *self.scripted.lock() = scripted.into_iter().map(|(op, outcome)| (op, Some(outcome))).collect();
        self.deferred.lock().clear();
    }

    fn take(&self, op: OsOp) -> Option<Result<Value, OsError>> {
        self.scripted.lock().iter_mut().find(|(named, outcome)| *named == op && outcome.is_some()).and_then(|(_, outcome)| outcome.take())
    }

    fn unused(&self) -> Vec<String> {
        self.scripted
            .lock()
            .iter()
            .enumerate()
            .filter(|(_, (_, outcome))| outcome.is_some())
            .map(|(n, (op, _))| format!("answers[{n}] ({})", op.name()))
            .collect()
    }

    fn take_deferred(&self) -> Vec<(CallSite, String, Result<Value, OsError>)> {
        std::mem::take(&mut *self.deferred.lock())
    }
}

fn scripted_answers(i: usize, answers: &serde_json::Value, live: bool) -> Result<Vec<(OsOp, Result<Value, OsError>)>, String> {
    let list = match answers {
        serde_json::Value::Null => return Ok(Vec::new()),
        serde_json::Value::Array(_) if live => return Err(format!("step {i}: `answers` apply to a replay with no live host only")),
        serde_json::Value::Array(list) => list,
        _ => return Err(format!("step {i}: `answers` must be a list of host answers")),
    };
    list.iter()
        .enumerate()
        .map(|(n, answer)| {
            let at = format!("step {i}: answers[{n}]");
            let fields = answer.as_object().ok_or_else(|| format!("{at} must be an object"))?;
            if let Some(extra) = fields.keys().find(|k| !["operator", "ok", "error"].contains(&k.as_str())) {
                return Err(format!("{at}: unknown key `{extra}`"));
            }
            let name = fields.get("operator").and_then(serde_json::Value::as_str).ok_or_else(|| format!("{at}: `operator` must be an os operator name"))?;
            let op = OsOp::from_name(name).ok_or_else(|| format!("{at}: `{name}` is not an os operator"))?;
            let outcome = match (fields.get("ok"), fields.get("error")) {
                (Some(ok), None) => Ok(to_value(ok).map_err(|e| format!("{at}: {e}"))?),
                (None, Some(error)) => {
                    let kind = error.get("kind").and_then(serde_json::Value::as_str).and_then(OsErrorKind::from_name);
                    let message = error.get("message").and_then(serde_json::Value::as_str);
                    match (kind, message) {
                        (Some(kind), Some(message)) => Err(OsError::new(kind, message)),
                        _ => return Err(format!("{at}: `error` must be {{ kind: <an os error kind>, message: <string> }}")),
                    }
                }
                _ => return Err(format!("{at}: give exactly one of `ok` or `error`")),
            };
            Ok((op, outcome))
        })
        .collect()
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub collapsed: Option<Vec<StepCollapsed>>,
}

/// A delivery EXEC-QUEUE-4 skipped as an exact repeat within the step's dispatch: the trait and event it was for and its payload.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepCollapsed {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
    #[cfg_attr(test, ts(type = "EventPayload | null"))]
    pub payload: serde_json::Value,
}

/// A keyed ask the host deferred and no event has ended: the operator and arguments as the kernel dispatched them,
/// its key and the trait whose step made it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAsk {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub operator: String,
    #[cfg_attr(test, ts(type = "Array<JsonValue>"))]
    pub args: Vec<Value>,
    pub key: String,
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
    pub pending_asks: Vec<PendingAsk>,
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

/// What `orb execute` prints for a replay that could not run: `{ success: false, error, trait, outcomes }`, where `trait`
/// is the trait of the step that failed (none outside any step) and `outcomes` are those completed before the failure.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepRefusal {
    #[cfg_attr(test, ts(type = "false"))]
    pub success: bool,
    pub error: String,
    #[serde(rename = "trait")]
    pub trait_name: Option<String>,
    pub outcomes: Vec<StepOutcome>,
}

impl StepRefusal {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            success: false,
            error: error.into(),
            trait_name: None,
            outcomes: Vec::new(),
        }
    }
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
        collapsed: (!r.collapsed.is_empty()).then(|| {
            r.collapsed
                .iter()
                .map(|c| StepCollapsed {
                    trait_name: c.trait_name.clone(),
                    event: c.event.clone(),
                    payload: serde_json::to_value(&*c.payload).unwrap_or(serde_json::Value::Null),
                })
                .collect()
        }),
    }
}

fn played_emits(outcome: &StepOutcome, timestamp: u64) -> Result<Vec<PlayedEmit>, StepRefusal> {
    outcome
        .emits
        .iter()
        .map(|e| {
            let payload =
                match &e.payload {
                    serde_json::Value::Null => None,
                    p => Some(serde_json::from_value::<Value>(p.clone()).map_err(|err| {
                        StepRefusal::new(format!("emit {} payload: {err}", e.event))
                    })?),
                };
            Ok(PlayedEmit {
                event_type: e.event.clone(),
                payload,
                timestamp,
            })
        })
        .collect()
}

fn played_guard(g: &StepGuard) -> Result<PlayedGuard, StepRefusal> {
    Ok(PlayedGuard {
        arm: g.arm as u32,
        guard: serde_json::from_value(g.guard.clone())
            .map_err(|e| StepRefusal::new(format!("guard of arm {}: {e}", g.arm)))?,
        passed: g.passed,
        trace: Vec::new(),
        error: g.error.clone(),
    })
}

/// Each request answered as asked with the circuit it ran: request `i` reads step `i` of `report`, and the outcome of its
/// own trait is the one reported. Refused when that trait did not run.
pub fn played_steps(
    requests: &[PlayRequest],
    report: &StepReport,
) -> Result<Vec<PlayedStep>, StepRefusal> {
    requests
        .iter()
        .enumerate()
        .map(|(i, request)| {
            let step = report.steps.get(i);
            let moved = step.map_or(&[][..], |s| s.outcomes.as_slice());
            let outcome = moved
                .iter()
                .find(|o| o.trait_name == request.trait_name)
                .ok_or_else(|| {
                    StepRefusal::new(format!("trait \"{}\" did not run", request.trait_name))
                })?;
            let timestamp = i as u64;
            let guards = outcome
                .guards
                .iter()
                .map(played_guard)
                .collect::<Result<Vec<_>, _>>()?;
            let guard = match outcome.fired_arm {
                Some(arm) if outcome.guards.iter().any(|g| g.arm == arm) => GuardVerdict::Pass,
                None if !outcome.guards.is_empty() => GuardVerdict::Fail,
                _ => GuardVerdict::Unguarded,
            };
            let outcomes = moved
                .iter()
                .map(|o| {
                    Ok(PlayedOutcome {
                        orbital: o.orbital.clone(),
                        trait_name: o.trait_name.clone(),
                        event: o.event.clone(),
                        from: o.previous_state.clone(),
                        to: o.new_state.clone(),
                        arm: o.fired_arm.map(|a| a as u32),
                        fired: o.executed,
                        effects: o.effects.clone(),
                        emitted: played_emits(o, timestamp)?,
                    })
                })
                .collect::<Result<Vec<_>, StepRefusal>>()?;
            Ok(PlayedStep {
                orbital: request.orbital.clone(),
                trait_name: request.trait_name.clone(),
                from: request.from.clone(),
                event: request.event.clone(),
                payload: request.payload.clone(),
                seeds: request.seeds.clone(),
                result: PlayedResult {
                    trait_name: outcome.trait_name.clone(),
                    event: request.event.clone(),
                    transition_fired: outcome.executed,
                    guard,
                    fired_arm: outcome.fired_arm.map(|a| a as u32),
                    state: PlayedState {
                        before: Some(outcome.previous_state.clone()),
                        after: Some(outcome.new_state.clone()),
                    },
                    effects: outcome.effects.clone(),
                    emitted: played_emits(outcome, timestamp)?,
                    guards,
                    ensures: outcome.ensures.clone(),
                    rendered: None,
                    failure: outcome.failure.clone(),
                    entity_after: step.map_or(serde_json::Value::Null, |s| s.entity_after.clone()),
                    pending_asks: step.map_or_else(Vec::new, |s| s.pending_asks.clone()),
                    outcomes,
                    collapsed: Some(moved.iter().flat_map(|o| o.collapsed.iter().flatten().cloned()).collect::<Vec<_>>())
                        .filter(|c| !c.is_empty()),
                },
            })
        })
        .collect()
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

/// A replayed step's host answer: an os/* ask the step's `answers` script is answered from them (a deferring op's
/// answer waits for the dispatch to end and arrives as its completion); any other keyed os/* ask is left pending (its
/// answer event is a later step's to play, G-ORB-459); a `cancel-call` succeeds, since a replay leaves every keyed call
/// pending and withdrawing one leaves nothing to stop, as a live host answers a key with no call in flight; a credential
/// ask is answered from the step's `credentials`; every
/// other ask is answered as a kernel with no host answers it.
pub fn step_answer(
    kernel: &RuntimeKernel,
    credentials: &StepCredentials,
    answers: &StepAnswers,
    request: &IoRequest,
) -> IoAnswer {
    if let IoRequest::Os { op, key, site, .. } = request {
        if let Some(outcome) = answers.take(*op) {
            return match (key, site) {
                (Some(key), Some(site)) if op.defers_completion() => {
                    answers.deferred.lock().push((site.clone(), key.clone(), outcome));
                    IoAnswer::ServiceDeferred
                }
                _ => IoAnswer::Os(outcome.map(Into::into)),
            };
        }
    }
    match request {
        IoRequest::Os { key: Some(_), .. }
        | IoRequest::Guest { .. }
        | IoRequest::Substrate {
            key: Some(_),
            site: Some(_),
            ..
        } => IoAnswer::ServiceDeferred,
        IoRequest::CancelCall { .. } => IoAnswer::Cancelled(Ok(())),
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

/// The dispatch of a replay with no live host: asks answered by [`step_answer`], then each scripted deferring answer
/// delivered as its completion, in the order the asks were made, until none is left.
pub fn replay_dispatch(
    kernel: &RuntimeKernel,
    credentials: &StepCredentials,
    answers: &StepAnswers,
    trait_name: &str,
    event: &str,
    payload: Value,
) -> Result<DispatchOutcomes, String> {
    let answer = |request: &IoRequest| step_answer(kernel, credentials, answers, request);
    let dispatched = kernel
        .start_dispatch(trait_name, None, event, payload, None, None)
        .and_then(|progress| drive_blocking(kernel, progress, &answer))
        .map_err(|e| e.to_string())?;
    let mut completions = Vec::new();
    loop {
        let deferred = answers.take_deferred();
        if deferred.is_empty() {
            break;
        }
        for (site, key, outcome) in deferred {
            let Some((event, payload, failed)) = kernel.completion_route(&site, &key, outcome) else { continue };
            let delivered = kernel.dispatch_route(&site, &key, event, payload, failed, &answer);
            completions.push(StepCompletion {
                event: delivered.event,
                payload: delivered.payload,
                failed: delivered.failed,
                outcomes: delivered.result.map_err(|e| e.to_string()),
            });
        }
    }
    with_completions(dispatched, completions)
}

/// One deferred ask's completion as a live host delivered it (EXEC-QUEUE-10): the route event it fired, its payload (the
/// ask's `key` among it), whether the ask failed (the `emit.failure` route), and the outcomes of the dispatch that ran it.
#[derive(Debug, Clone)]
pub struct StepCompletion {
    pub event: String,
    pub payload: Value,
    pub failed: bool,
    pub outcomes: Result<DispatchOutcomes, String>,
}

/// The step's dispatched outcomes followed by each delivered completion's hop, in delivery order. An effect whose keyed
/// ask failed and was delivered to its declared failure route reads `failed` and `routed`, as one answered in the
/// dispatch itself does (EXEC-STEP-7: a routed failure is a handled outcome).
pub fn with_completions(
    mut dispatched: DispatchOutcomes,
    completions: Vec<StepCompletion>,
) -> Result<DispatchOutcomes, String> {
    for completion in completions {
        let hops = completion
            .outcomes
            .map_err(|e| format!("the completion {} failed: {e}", completion.event))?;
        if completion.failed {
            let payload = serde_json::to_value(&completion.payload).unwrap_or(serde_json::Value::Null);
            let key = payload.get("key").cloned();
            let error = payload.get("error").and_then(|e| e.as_str().map(str::to_string));
            for run in dispatched
                .iter_mut()
                .flat_map(|(_, r)| r.effect_runs.iter_mut())
                .filter(|run| run.status == EffectRunStatus::Executed && asks_with_key(&run.effect, key.as_ref()))
            {
                run.status = EffectRunStatus::Failed;
                run.error = error.clone();
                run.routed = true;
            }
        }
        dispatched.extend(hops);
    }
    Ok(dispatched)
}

fn asks_with_key(effect: &orbital_core_base::schema::sexpr::SExpression, key: Option<&serde_json::Value>) -> bool {
    let Some(key) = key else { return false };
    serde_json::to_value(effect)
        .ok()
        .and_then(|e| e.as_array().cloned())
        .is_some_and(|parts| parts.iter().any(|part| part.get("key") == Some(key)))
}

/// The snapshot a request's seeds make of a trait's `current` one: its `from` state (empty when none is asked) and the
/// seed fields written over its row. `program/step` plays from it and the render probe replays from the same one.
pub fn seeded_snapshot(
    current: TraitSnapshot,
    from: Option<&str>,
    seed: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<TraitSnapshot, String> {
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
    Ok(TraitSnapshot {
        state: from.map(str::to_string).unwrap_or_default(),
        entity,
    })
}

#[allow(clippy::too_many_arguments)]
fn play_step(
    kernel: &RuntimeKernel,
    clock: &StepClock,
    credentials: &StepCredentials,
    answers: &StepAnswers,
    trait_defs: &BTreeMap<String, Arc<TraitDefinition>>,
    known: &BTreeMap<String, (String, DispatchMode)>,
    i: usize,
    step: &serde_json::Value,
    live: bool,
    dispatch: &mut dyn FnMut(&str, &str, Value, Option<f64>) -> Result<DispatchOutcomes, String>,
    partial: &mut Vec<StepOutcome>,
) -> Result<StepResult, String> {
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
    if !step["viewer"].is_null() {
        return Err(format!("step {i}: `viewer` is retired; a step names `identity`, the session's bound identity"));
    }
    let identity = match &step["identity"] {
        serde_json::Value::Null => None,
        user @ serde_json::Value::Object(_) => Some(to_value(user)?),
        _ => return Err(format!("step {i}: `identity` must be an identity object or null")),
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
    let scripted = scripted_answers(i, &step["answers"], live)?;
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
        let snapshot = seeded_snapshot(kernel.snapshot_trait(trait_name, None), step["from"].as_str(), seed)?;
        kernel.restore_trait(trait_name, None, snapshot);
        if step["from"].is_string() {
            kernel.seeded_past_lifecycle(trait_name);
        }
    }
    clock.pin(now);
    let payload = match &step["payload"] {
        serde_json::Value::Null => Value::Null,
        p => to_value(p)?,
    };
    let viewing = identity
        .map(|user| kernel.hold_replay_identity(user))
        .transpose()
        .map_err(|e| format!("step {i}: {e}"))?;
    credentials.hold(held);
    answers.hold(scripted);
    let outcomes = dispatch(trait_name, event, payload, settle_ms);
    let unused = answers.unused();
    answers.hold(Vec::new());
    credentials.hold(BTreeMap::new());
    drop(viewing);
    clock.pin(None);
    let outcomes = match outcomes {
        Ok(outcomes) => outcomes,
        Err(refusal) => {
            let snapshot = kernel.snapshot_trait(trait_name, None);
            let orbital = kernel
                .orbital_of_trait(trait_name)
                .ok_or_else(|| format!("step {i}: the trait \"{trait_name}\" belongs to no orbital"))?;
            partial.push(StepOutcome {
                trait_name: trait_name.to_string(),
                executed: false,
                previous_state: snapshot.state.clone(),
                new_state: snapshot.state,
                emits: Vec::new(),
                fired_arm: None,
                guards: Vec::new(),
                effects: Vec::new(),
                failure: Some(refusal),
                orbital,
                event: event.to_string(),
                ensures: Vec::new(),
                collapsed: None,
            });
            return Ok(StepResult {
                outcomes: std::mem::take(partial),
                states: kernel
                    .trait_snapshot(None)
                    .into_iter()
                    .map(|(name, (state, _))| (name, state))
                    .collect(),
                entities: serde_json::Map::new(),
                entity_after: serde_json::to_value(snapshot.entity).unwrap_or(serde_json::Value::Null),
                pending_asks: kernel.pending_asks(),
            });
        }
    };
    if !unused.is_empty() {
        return Err(format!("step {i}: no ask took {}", unused.join(", ")));
    }
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
    for (t, r) in &outcomes {
        let orbital = kernel
            .orbital_of_trait(t)
            .ok_or_else(|| format!("step {i}: the trait \"{t}\" belongs to no orbital"))?;
        let event = r.event.as_deref().unwrap_or(event);
        let ensures = trait_defs
            .get(t.as_str())
            .map(|td| played_ensures(td, event, r))
            .unwrap_or_default();
        partial.push(outcome_report(t, orbital, event, ensures, r));
    }
    let outcomes = std::mem::take(partial);
    let states = kernel
        .trait_snapshot(None)
        .into_iter()
        .map(|(name, (state, _))| (name, state))
        .collect();
    let pending_asks = kernel.pending_asks();
    Ok(StepResult {
        outcomes,
        states,
        entities,
        entity_after,
        pending_asks,
    })
}

/// Plays `steps` on `kernel`. A step's `credentials` are held in `credentials` and its `answers` in `answers` while it runs. `live` is true when `dispatch` runs them on a live host (only then does a step take
/// `settleMs`, which `dispatch` receives); `dispatch` returns the outcomes of the cascade one event started.
/// The report is a [`StepReport`].
pub fn play_steps(
    kernel: &RuntimeKernel,
    clock: &StepClock,
    credentials: &StepCredentials,
    answers: &StepAnswers,
    trait_defs: &BTreeMap<String, Arc<TraitDefinition>>,
    steps: &[serde_json::Value],
    live: bool,
    dispatch: &mut dyn FnMut(&str, &str, Value, Option<f64>) -> Result<DispatchOutcomes, String>,
) -> Result<StepReport, StepRefusal> {
    kernel.set_record_entity_after(true);
    let known = kernel.trait_snapshot(None);
    let mut reports = Vec::with_capacity(steps.len());
    for (i, step) in steps.iter().enumerate() {
        let mut partial: Vec<StepOutcome> = Vec::new();
        let played = play_step(
            kernel,
            clock,
            credentials,
            answers,
            trait_defs,
            &known,
            i,
            step,
            live,
            dispatch,
            &mut partial,
        );
        match played {
            Ok(result) => reports.push(result),
            Err(error) => {
                let mut outcomes: Vec<StepOutcome> = reports
                    .iter()
                    .flat_map(|r| r.outcomes.iter().cloned())
                    .collect();
                outcomes.append(&mut partial);
                return Err(StepRefusal {
                    success: false,
                    error,
                    trait_name: step["trait"].as_str().map(str::to_string),
                    outcomes,
                });
            }
        }
    }
    let dispatch_modes = kernel
        .trait_snapshot(None)
        .into_iter()
        .map(|(name, (_, mode))| (name, mode))
        .collect();
    Ok(StepReport {
        success: true,
        steps: reports,
        dispatch_modes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KernelConfig;
    use crate::entity_source::{EntitySource, InMemoryEntityCache};
    use orbital_core_base::effect_types::RuntimeEnvironment;

    const MODES: &str = r#"{"name":"EditorApp","typeContractVersion":1,"orbitals":[{"name":"Editor","entity":{"name":"Doc","persistence":"runtime","fields":[{"name":"id","type":"string"},{"name":"count","type":"number","default":0}]},"pages":[],"traits":[{"name":"Modes","linkedEntity":"Doc","stateMachine":{"states":[{"name":"NORMAL","isInitial":true},{"name":"INSERT"},{"name":"VISUAL"}],"transitions":[{"from":"NORMAL","to":"INSERT","event":"KEY","guard":["=","@payload.key","i"],"effects":[["set","@entity.count",["+","@entity.count",1]]]},{"from":"NORMAL","to":"VISUAL","event":"KEY","guard":["=","@payload.key","v"]}]}}]}]}"#;

    const KEYED: &str = r#"{"name":"KeyedApp","typeContractVersion":1,"orbitals":[{"name":"Keys","entity":{"name":"Job","persistence":"runtime","fields":[{"name":"id","type":"string"},{"name":"len","type":"number","default":-1}]},"pages":[],"traits":[{"name":"Host","linkedEntity":"Job","config":{"password":{"type":"secret","default":"PW_NAME","description":"the password credential"}},"stateMachine":{"states":[{"name":"idle","isInitial":true}],"transitions":[{"from":"idle","to":"idle","event":"GO","effects":[["set","@entity.len",["default",["credential/byte-length","@config.password"],-1]]]}]}}]}]}"#;

    const ASKING: &str = r#"{"name":"AskApp","typeContractVersion":1,"orbitals":[{"name":"Asks","entity":{"name":"Call","persistence":"runtime","fields":[{"name":"id","type":"string"}]},"pages":[],"traits":[{"name":"Caller","linkedEntity":"Call","stateMachine":{"states":[{"name":"idle","isInitial":true}],"transitions":[
        {"from":"idle","to":"idle","event":"GO","effects":[["let",[["path","/items"]],["if",true,["os/read-file",["str/concat","/data","@path"],{"key":"first","emit":{"success":"DONE","failure":"FAILED"}}]]]]},
        {"from":"idle","to":"idle","event":"GO_TWO","effects":[["os/read-file","/a",{"key":"a","emit":{"success":"DONE","failure":"FAILED"}}],["os/stat","/b",{"key":"b","emit":{"success":"DONE","failure":"FAILED"}}]]},
        {"from":"idle","to":"idle","event":"PLAIN","effects":[["set","@entity.id","x"]]},
        {"from":"idle","to":"idle","event":"DONE"}
    ]}}]}]}"#;

    fn play(steps: serde_json::Value) -> Result<serde_json::Value, String> {
        play_on(MODES, steps)
    }

    fn play_on(program: &str, steps: serde_json::Value) -> Result<serde_json::Value, String> {
        play_refusable(program, steps).map_err(|r| r.error)
    }

    fn play_refusable(
        program: &str,
        steps: serde_json::Value,
    ) -> Result<serde_json::Value, StepRefusal> {
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
        let answers = StepAnswers::new();
        play_steps(
            &kernel,
            &clock,
            &credentials,
            &answers,
            &defs,
            &steps,
            false,
            &mut |t, e, p, _| replay_dispatch(&kernel, &credentials, &answers, t, e, p),
        )
        .map(|report| serde_json::to_value(report).unwrap())
    }

    const LOGGING: &str = r#"{"name":"LogApp","typeContractVersion":1,"orbitals":[{"name":"Logs","entity":{"name":"Doc","persistence":"runtime","fields":[{"name":"id","type":"string"},{"name":"n","type":"number","default":2}]},"pages":[],"traits":[{"name":"Logger","linkedEntity":"Doc","stateMachine":{"states":[{"name":"idle","isInitial":true}],"transitions":[
        {"from":"idle","to":"idle","event":"LITERAL","effects":[["log","plain line"]]},
        {"from":"idle","to":"idle","event":"COMPUTED","effects":[["log",["str/concat","n is ","@entity.n"]]]},
        {"from":"idle","to":"idle","event":"LEVELLED","effects":[["log",["str/concat","n is ","@entity.n"],"debug"]]},
        {"from":"idle","to":"idle","event":"BROKEN","effects":[["log",["str/concat","n is ",["+","a",1]]]]},
        {"from":"idle","to":"idle","event":"LOUD","effects":[["log","text","loud"]]}
    ]}}]}]}"#;

    struct Lines(std::sync::Arc<std::sync::Mutex<Vec<(String, tracing::Level, String)>>>);

    impl tracing::Subscriber for Lines {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            self.0.lock().unwrap().push((event.metadata().target().to_string(), *event.metadata().level(), message.0));
        }
    }

    fn logged(event: &str) -> (serde_json::Value, Vec<(String, tracing::Level, String)>) {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let report = tracing::subscriber::with_default(Lines(lines.clone()), || {
            play_on(LOGGING, serde_json::json!([{ "trait": "Logger", "event": event }])).unwrap()
        });
        let program: Vec<_> = lines.lock().unwrap().iter().filter(|(target, ..)| target == "almadar:program").cloned().collect();
        (report["steps"][0]["outcomes"][0]["effects"][0].clone(), program)
    }

    #[test]
    fn a_log_effect_prints_its_evaluated_message_under_the_program_namespace_at_its_level() {
        let (effect, lines) = logged("LITERAL");
        assert_eq!(effect["status"], "executed");
        assert_eq!(lines, [("almadar:program".to_string(), tracing::Level::INFO, "plain line".to_string())]);
        let (effect, lines) = logged("COMPUTED");
        assert_eq!(effect["status"], "executed", "a computed message is a log, not a skipped effect: {effect}");
        assert_eq!(lines, [("almadar:program".to_string(), tracing::Level::INFO, "n is 2".to_string())]);
        let (_, lines) = logged("LEVELLED");
        assert_eq!(lines, [("almadar:program".to_string(), tracing::Level::DEBUG, "n is 2".to_string())]);
    }

    #[test]
    fn a_log_message_that_fails_to_evaluate_fails_the_effect_and_prints_nothing() {
        let (effect, lines) = logged("BROKEN");
        assert_eq!(effect["status"], "failed", "{effect}");
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn a_log_with_an_unknown_level_is_not_an_effect() {
        let (effect, lines) = logged("LOUD");
        assert_eq!(effect["status"], "skipped", "{effect}");
        assert!(lines.is_empty());
    }

    const GPU: &str = r#"{"name":"GpuApp","typeContractVersion":1,"osGrants":{"gpu":[{"target":"device:1024","kinds":["open"]}]},"orbitals":[{"name":"Gpu","entity":{"name":"Job","persistence":"runtime","fields":[{"name":"id","type":"string"}]},"pages":[],"traits":[{"name":"Host","linkedEntity":"Job","stateMachine":{"states":[{"name":"idle","isInitial":true},{"name":"opening"},{"name":"opened"},{"name":"made"},{"name":"failed"}],"transitions":[
        {"from":"idle","to":"opening","event":"OPEN","effects":[["gpu/device-open",{},{"key":"dev","emit":{"success":"OPENED","failure":"FAILED"}}]]},
        {"from":"opening","to":"opened","event":"OPENED","effects":[["gpu/buffer-create","@payload.device",{"size":16,"usage":["storage"]},{"key":"buf","emit":{"success":"MADE","failure":"FAILED"}}]]},
        {"from":"opened","to":"made","event":"MADE"},
        {"from":"opening","to":"failed","event":"FAILED"},
        {"from":"opened","to":"failed","event":"FAILED"}
    ]}}]}]}"#;

    fn last_state(report: &serde_json::Value) -> serde_json::Value {
        report["steps"][0]["outcomes"].as_array().and_then(|o| o.last()).map(|o| o["newState"].clone()).unwrap_or_default()
    }

    fn open_answer() -> serde_json::Value {
        serde_json::json!({ "operator": "gpu/device-open", "ok": { "device": 1, "features": [], "limits": {} } })
    }

    fn buffer_answer() -> serde_json::Value {
        serde_json::json!({ "operator": "gpu/buffer-create", "ok": { "buffer": 7 } })
    }

    #[test]
    fn a_scripted_deferred_answer_is_completed_by_the_kernel_which_mints_the_handle_the_next_ask_uses() {
        let report = play_on(GPU, serde_json::json!([
            { "trait": "Host", "event": "OPEN", "answers": [open_answer(), buffer_answer()] }
        ]))
        .unwrap();
        let step = &report["steps"][0];
        assert_eq!(last_state(&report), "made", "the minted device took the buffer ask to MADE: {report}");
        let events: Vec<_> = step["outcomes"].as_array().unwrap().iter().map(|o| o["event"].clone()).collect();
        assert_eq!(events, [serde_json::json!("OPEN"), serde_json::json!("OPENED"), serde_json::json!("MADE")], "{report}");
        assert_eq!(step["pendingAsks"], serde_json::json!([]), "both asks ended: {report}");
    }

    #[test]
    fn control_a_device_fed_as_a_payload_is_a_forged_number_and_the_kernel_refuses_it() {
        let report = play_on(GPU, serde_json::json!([
            { "trait": "Host", "from": "opening", "event": "OPENED", "payload": { "device": 1, "key": "dev" }, "answers": [] }
        ]))
        .unwrap();
        assert_eq!(last_state(&report), "failed", "{report}");
    }

    #[test]
    fn a_scripted_error_answer_takes_the_failure_route() {
        let report = play_on(GPU, serde_json::json!([
            { "trait": "Host", "event": "OPEN", "answers": [{ "operator": "gpu/device-open", "error": { "kind": "denied", "message": "no adapter" } }] }
        ]))
        .unwrap();
        assert_eq!(last_state(&report), "failed", "{report}");
    }

    #[test]
    fn without_answers_a_keyed_ask_stays_pending() {
        let report = play_on(GPU, serde_json::json!([{ "trait": "Host", "event": "OPEN" }])).unwrap();
        let step = &report["steps"][0];
        assert_eq!(last_state(&report), "opening", "{report}");
        assert_eq!(step["pendingAsks"][0]["operator"], "gpu/device-open", "{report}");
    }

    #[test]
    fn an_answer_no_ask_takes_refuses_the_step() {
        let error = play_on(GPU, serde_json::json!([
            { "trait": "Host", "event": "OPEN", "answers": [open_answer(), buffer_answer(), { "operator": "gpu/submit", "ok": { "reads": [] } }] }
        ]))
        .unwrap_err();
        assert!(error.contains("no ask took answers[2] (gpu/submit)"), "{error}");
    }

    #[test]
    fn a_malformed_answer_refuses_the_step() {
        for (answer, says) in [
            (serde_json::json!({ "operator": "gpu/nope", "ok": {} }), "is not an os operator"),
            (serde_json::json!({ "operator": "gpu/device-open" }), "exactly one of `ok` or `error`"),
            (serde_json::json!({ "operator": "gpu/device-open", "ok": {}, "error": { "kind": "io", "message": "x" } }), "exactly one of `ok` or `error`"),
            (serde_json::json!({ "operator": "gpu/device-open", "error": { "kind": "nope", "message": "x" } }), "`error` must be"),
            (serde_json::json!({ "operator": "gpu/device-open", "ok": {}, "extra": 1 }), "unknown key `extra`"),
        ] {
            let error = play_on(GPU, serde_json::json!([{ "trait": "Host", "event": "OPEN", "answers": [answer] }])).unwrap_err();
            assert!(error.contains(says), "{error}");
        }
    }

    #[test]
    fn a_step_holds_only_its_own_credentials_and_the_report_never_shows_the_value() {
        let go = |step: serde_json::Value| play_on(KEYED, serde_json::json!([step]));
        let held = go(serde_json::json!({ "trait": "Host", "event": "GO", "credentials": { "PW_NAME": "hunter2" } })).unwrap();
        assert_eq!(
            held["steps"][0]["entityAfter"]["len"],
            serde_json::json!(7),
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
            serde_json::json!(3),
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
    fn g_std_300_each_steps_states_are_the_states_after_that_step() {
        let report = play(serde_json::json!([
            { "trait": "Modes", "event": "KEY", "payload": { "key": "i" } },
            { "trait": "Modes", "event": "KEY", "payload": { "key": "x" } }
        ]))
        .unwrap();
        assert_eq!(report["steps"][0]["outcomes"][0]["newState"], "INSERT");
        assert_eq!(report["steps"][0]["states"]["Modes"], "INSERT");
        let control = play(serde_json::json!([
            { "trait": "Modes", "event": "KEY", "payload": { "key": "x" } }
        ]))
        .unwrap();
        assert_eq!(control["steps"][0]["states"]["Modes"], "NORMAL");
    }

    #[test]
    fn a_refusal_mid_replay_carries_the_failing_trait_and_the_outcomes_so_far() {
        let refusal = play_refusable(
            MODES,
            serde_json::json!([
                { "trait": "Modes", "event": "KEY", "payload": { "key": "i" } },
                { "trait": "Nope", "event": "KEY" }
            ]),
        )
        .unwrap_err();
        assert_eq!(refusal.trait_name.as_deref(), Some("Nope"));
        assert_eq!(refusal.outcomes.len(), 1);
        assert_eq!(refusal.outcomes[0].trait_name, "Modes");
        assert_eq!(refusal.outcomes[0].new_state, "INSERT");
        let outside = play_refusable(MODES, serde_json::json!([{ "event": "KEY" }])).unwrap_err();
        assert_eq!(outside.trait_name, None);
        assert!(outside.outcomes.is_empty());
    }

    #[test]
    fn a_refused_dispatch_is_that_steps_result_and_the_later_steps_still_run() {
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
        let (credentials, answers) = (StepCredentials::new(), StepAnswers::new());
        let steps = serde_json::json!([
            { "trait": "Modes", "from": "NORMAL", "event": "KEY", "payload": { "key": "i" } },
            { "trait": "Modes", "from": "NORMAL", "event": "KEY", "payload": { "key": "v" } }
        ]);
        let report = play_steps(
            &kernel,
            &clock,
            &credentials,
            &answers,
            &defs,
            steps.as_array().unwrap(),
            false,
            &mut |t, e, p, _| {
                if serde_json::to_value(&p).unwrap()["key"] == "i" {
                    Err("Payload validation failed for event 'KEY': key (wrong-type)".to_string())
                } else {
                    replay_dispatch(&kernel, &credentials, &answers, t, e, p)
                }
            },
        )
        .expect("a refused dispatch is a step result, not a refused replay");
        let refused = &report.steps[0].outcomes;
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].trait_name, "Modes");
        assert!(!refused[0].executed);
        assert_eq!(refused[0].failure.as_deref(), Some("Payload validation failed for event 'KEY': key (wrong-type)"));
        assert_eq!((refused[0].previous_state.as_str(), refused[0].new_state.as_str()), ("NORMAL", "NORMAL"));
        assert_eq!(report.steps[0].states["Modes"], "NORMAL", "nothing moved");
        assert_eq!(report.steps[1].outcomes[0].new_state, "VISUAL", "control: the next step ran");
        assert_eq!(report.steps[1].outcomes[0].failure, None);
    }

    #[test]
    fn a_replayed_cancel_call_withdraws_the_pending_call_and_succeeds() {
        const CANCELLING: &str = r#"{"name":"CancelApp","typeContractVersion":1,"orbitals":[{"name":"Timer","entity":{"name":"Clock","persistence":"runtime","fields":[{"name":"id","type":"string"},{"name":"key","type":"string","default":"k"}]},"pages":[],"traits":[{"name":"Timeout","linkedEntity":"Clock","stateMachine":{"states":[{"name":"idle","isInitial":true},{"name":"running"}],"transitions":[
            {"from":"running","to":"idle","event":"CANCEL","effects":[["cancel-call","@entity.key"]]},
            {"from":"running","to":"idle","event":"BAD","effects":[["cancel-call",1]]}
        ]}}]}]}"#;
        let report = play_on(CANCELLING, serde_json::json!([{ "trait": "Timeout", "from": "running", "event": "CANCEL" }])).unwrap();
        let outcome = &report["steps"][0]["outcomes"][0];
        assert_eq!(outcome["newState"], "idle");
        assert_eq!(outcome["effects"][0]["status"], "executed", "{outcome}");
        assert_eq!(outcome["failure"], serde_json::Value::Null);
        let control = play_on(CANCELLING, serde_json::json!([{ "trait": "Timeout", "from": "running", "event": "BAD" }])).unwrap();
        assert_ne!(control["steps"][0]["outcomes"][0]["effects"][0]["status"], "executed", "control: a key that is not text is not cancelled");
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
    fn played_steps_answers_each_request_with_the_circuit_it_ran() {
        use crate::program_machine::{GuardVerdict, PlayRequest};
        let report = play(serde_json::json!([
            { "trait": "Modes", "event": "KEY", "payload": { "key": "i" } },
            { "trait": "Modes", "event": "KEY", "from": "NORMAL", "payload": { "key": "z" } }
        ]))
        .unwrap();
        let report: StepReport = serde_json::from_value(report).unwrap();
        let asked = |trait_name: &str, key: &str| serde_json::json!({ "orbital": "Editor", "trait": trait_name, "from": "NORMAL", "event": "KEY", "payload": { "key": key }, "now": 5 });
        let requests: Vec<PlayRequest> = serde_json::from_value(serde_json::json!([
            asked("Modes", "i"),
            asked("Modes", "z")
        ]))
        .unwrap();
        let played = played_steps(&requests, &report).unwrap();
        assert_eq!(played.len(), 2);
        let fired = &played[0].result;
        assert_eq!(fired.guard, GuardVerdict::Pass);
        assert_eq!(fired.fired_arm, Some(0));
        assert!(fired.transition_fired);
        assert_eq!(fired.state.after.as_deref(), Some("INSERT"));
        assert_eq!(fired.outcomes.len(), 1);
        assert!(fired.collapsed.is_none(), "control: no repeat, nothing collapsed");
        assert_eq!(fired.entity_after["count"], serde_json::json!(1.0));
        assert_eq!(played[0].orbital, "Editor");
        assert_eq!(played[0].seeds.now, Some(5.0));
        let refused = &played[1].result;
        assert_eq!(
            refused.guard,
            GuardVerdict::Fail,
            "control: every guard failed"
        );
        assert_eq!(refused.fired_arm, None);
        assert!(!refused.transition_fired);
        assert_eq!(refused.guards.len(), 2);

        let missing: Vec<PlayRequest> =
            serde_json::from_value(serde_json::json!([asked("Nope", "i")])).unwrap();
        let refusal = played_steps(&missing, &report).unwrap_err();
        assert_eq!(refusal.error, "trait \"Nope\" did not run");
    }

    #[test]
    fn the_report_is_the_typed_step_report_and_a_refusal_is_the_other_shape() {
        let report = play(serde_json::json!([
            { "trait": "Modes", "event": "KEY", "payload": { "key": "i" } }
        ]))
        .unwrap();
        let typed: StepReport =
            serde_json::from_value(report.clone()).expect("the report is a StepReport");
        assert!(typed.success);
        let outcome = &typed.steps[0].outcomes[0];
        assert_eq!(
            (
                outcome.trait_name.as_str(),
                outcome.orbital.as_str(),
                outcome.event.as_str()
            ),
            ("Modes", "Editor", "KEY")
        );
        assert_eq!(
            (outcome.previous_state.as_str(), outcome.new_state.as_str()),
            ("NORMAL", "INSERT")
        );
        assert_eq!(outcome.fired_arm, Some(0));
        assert_eq!(
            serde_json::to_value(&typed).unwrap(),
            report,
            "the typed report prints what the replay printed"
        );
        let refusal = serde_json::json!({ "success": false, "error": "no", "trait": "Modes", "outcomes": [] });
        let parsed: StepRefusal = serde_json::from_value(refusal.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), refusal);
        assert!(
            serde_json::from_value::<StepReport>(refusal).is_err(),
            "control: a refusal carries no steps, so it is not a report"
        );
    }

    fn pending_of(
        report: &serde_json::Value,
        step: usize,
    ) -> Vec<(String, String, serde_json::Value, String)> {
        report["steps"][step]["pendingAsks"]
            .as_array()
            .unwrap_or_else(|| panic!("pendingAsks is a list: {report}"))
            .iter()
            .map(|a| {
                (
                    a["trait"].as_str().unwrap().to_string(),
                    a["operator"].as_str().unwrap().to_string(),
                    a["args"].clone(),
                    a["key"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn a_keyed_ask_nested_in_let_and_if_is_listed_pending_with_its_evaluated_args_and_key() {
        let report = play_on(
            ASKING,
            serde_json::json!([{ "trait": "Caller", "event": "GO" }]),
        )
        .unwrap();
        let pending = pending_of(&report, 0);
        assert_eq!(pending.len(), 1, "{report}");
        assert_eq!(pending[0].0, "Caller");
        assert_eq!(pending[0].1, "os/read-file");
        assert_eq!(
            pending[0].2,
            serde_json::json!(["/data/items"]),
            "the path is evaluated, not authored: {report}"
        );
        assert_eq!(pending[0].3, "first");
    }

    #[test]
    fn an_answered_ask_is_not_listed_and_the_one_still_waiting_is() {
        let report = play_on(
            ASKING,
            serde_json::json!([
                { "trait": "Caller", "event": "GO_TWO" },
                { "trait": "Caller", "event": "DONE", "payload": { "key": "a" } },
                { "trait": "Caller", "event": "DONE", "payload": { "key": "b" } }
            ]),
        )
        .unwrap();
        let keys = |step| {
            pending_of(&report, step)
                .into_iter()
                .map(|p| p.3)
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(0), ["a", "b"], "{report}");
        assert_eq!(
            keys(1),
            ["b"],
            "a: answered by its completion event: {report}"
        );
        assert!(keys(2).is_empty(), "b: answered too: {report}");
    }

    #[test]
    fn control_a_step_with_no_ask_lists_none() {
        let report = play_on(
            ASKING,
            serde_json::json!([{ "trait": "Caller", "event": "PLAIN" }]),
        )
        .unwrap();
        assert_eq!(
            report["steps"][0]["pendingAsks"],
            serde_json::json!([]),
            "{report}"
        );
    }

    #[test]
    fn two_pending_asks_keep_the_order_the_kernel_recorded_them() {
        let report = play_on(
            ASKING,
            serde_json::json!([{ "trait": "Caller", "event": "GO_TWO" }]),
        )
        .unwrap();
        let pending = pending_of(&report, 0);
        assert_eq!(
            pending.iter().map(|p| p.3.as_str()).collect::<Vec<_>>(),
            ["a", "b"],
            "{report}"
        );
        assert_eq!(
            pending
                .iter()
                .map(|p| p.2[0].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["/a", "/b"]
        );
        let typed: StepReport = serde_json::from_value(report.clone()).unwrap();
        assert_eq!(serde_json::to_value(&typed).unwrap(), report);
    }

    fn batching(persistence: &str, ops: serde_json::Value) -> String {
        serde_json::json!({"name":"BatchApp","typeContractVersion":1,"orbitals":[{"name":"Batches","entity":{"name":"Row","persistence":persistence,"fields":[{"name":"id","type":"string"}]},"pages":[],"traits":[{"name":"Stager","linkedEntity":"Row","stateMachine":{"states":[{"name":"idle","isInitial":true}],"transitions":[
            {"from":"idle","to":"idle","event":"GO","effects":[["persist","batch",ops,{"emit":{"success":"STAGED","failure":"STAGE_FAILED"}}]]},
            {"from":"idle","to":"idle","event":"STAGED","effects":[["set","@entity.id","ok"]]},
            {"from":"idle","to":"idle","event":"STAGE_FAILED","effects":[["set","@entity.id","failed"]]}
        ]}}]}]}).to_string()
    }

    fn staged(persistence: &str, ops: serde_json::Value) -> serde_json::Value {
        play_on(&batching(persistence, ops), serde_json::json!([{ "trait": "Stager", "event": "GO" }])).unwrap()
    }

    fn two_creates() -> serde_json::Value {
        serde_json::json!([["create", "Row", { "id": "a" }], ["create", "Row", { "id": "b" }]])
    }

    #[test]
    fn control_a_one_op_batch_replayed_answers_its_success_route() {
        let report = staged("persistent", serde_json::json!([["create", "Row", { "id": "a" }]]));
        assert_eq!(report["steps"][0]["entityAfter"]["id"], "ok", "{report}");
    }

    #[test]
    fn a_two_create_batch_replayed_answers_its_success_route_when_every_op_lands() {
        let report = staged("persistent", two_creates());
        assert_eq!(report["steps"][0]["entityAfter"]["id"], "ok", "{report}");
    }

    #[test]
    fn a_compensated_batch_replayed_answers_its_failure_route_never_its_success() {
        let ops = serde_json::json!([["create", "Row", { "id": "a" }], ["create", "Row", { "id": "a" }]]);
        let report = staged("persistent", ops);
        assert_eq!(report["steps"][0]["entityAfter"]["id"], "failed", "{report}");
    }
}
