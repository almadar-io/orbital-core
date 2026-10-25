//! A program's state machine as values (`MachineRow`, read by the `std-machine` atom) and the record `program/step`
//! answers with (`PlayedStep`); `@almadar/core` names both as program types.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::embed::eval_wire::EvalStepWire;
use crate::process_result::{EffectRun, EffectRunStatus, ProcessEventResult};
use crate::verify::plan_wire::{judge_propositions, PropositionObservation, PropositionVerdict};
use orbital_core_base::schema::types::TraitDefinition;
use orbital_core_base::schema::SExpression;
use orbital_core_base::value::Value;
use serde::{Deserialize, Serialize};

/// One declared field of an event's payload.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PayloadSlot {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub required: bool,
    /// The closed value set, when the field declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    /// The declared entity the value is a row of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
}

/// One transition of a program: `from --event--> to` in `orbital`'s `trait`, and the payload the event declares.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineRow {
    pub orbital: String,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    pub to: String,
    /// The transition carries a guard of its own.
    pub guarded: bool,
    pub payload: Vec<PayloadSlot>,
}

/// What chose the transition: its guard passed, every guard failed, or no guard was tried.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardVerdict {
    Pass,
    Fail,
    #[serde(rename = "none")]
    Unguarded,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayedEffectStatus {
    Executed,
    Failed,
    Skipped,
}

/// The state a played trait was in before and after the step.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayedState {
    pub before: Option<String>,
    pub after: Option<String>,
}

/// One authored effect of the fired arm.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayedEffect {
    #[serde(rename = "type")]
    pub effect_type: String,
    #[cfg_attr(test, ts(type = "Array<JsonValue>"))]
    pub args: Vec<Value>,
    pub status: PlayedEffectStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The effect failed and the program routed that failure to its declared `emit.failure` event.
    pub routed: bool,
}

/// How a proposition fared on the run.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnsuresVerdict {
    Held,
    Broken,
    /// Never a pass: the arm did not fire or complete, or the proposition could not be evaluated.
    Inconclusive,
}

/// The bindings a broken proposition was evaluated under.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropositionBindings {
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub entity: Value,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub from_entity: Value,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: Value,
}

/// One `@ensures` proposition of the played arm, judged by the kernel on the step's rows.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayedEnsures {
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub proposition: SExpression,
    pub verdict: EnsuresVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterexample: Option<PropositionBindings>,
}

/// One event the step emitted.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayedEmit {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "EventPayload", optional))]
    pub payload: Option<Value>,
    pub timestamp: u64,
}

/// One guarded arm tried while choosing the arm that fires.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayedGuard {
    /// The arm's position among the `from --event-->` arms, in declaration order.
    pub arm: u32,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub guard: Value,
    pub passed: bool,
    pub trace: Vec<EvalStepWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The circuit one played request ran: Event, Guard, Transition, Effects.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayedResult {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
    /// A failed guard is never a transition, even for a self-loop.
    pub transition_fired: bool,
    pub guard: GuardVerdict,
    /// The position of the arm that fired; absent when none did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_arm: Option<u32>,
    pub state: PlayedState,
    pub effects: Vec<PlayedEffect>,
    pub emitted: Vec<PlayedEmit>,
    pub guards: Vec<PlayedGuard>,
    /// The fired arm's `@ensures`; when no arm fired, those of every `from --event-->` arm, each inconclusive.
    pub ensures: Vec<PlayedEnsures>,
    /// The slots the played trait filled, walked by the kernel's render probe; present when `options.render` asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<Vec<RenderedSlot>>,
}

/// One non-`system` slot a played trait's `render-ui` filled, as the client circuit rendered it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedSlot {
    pub slot: String,
    /// The trait whose `render-ui` wrote the slot.
    pub source_trait: String,
    /// The `linkedEntity` of `source_trait`; empty when it declares none.
    pub owner_entity: String,
    /// The rendered tree in pre-order, each `@trait` embed's content visited once where it is embedded.
    pub nodes: Vec<RenderedNode>,
}

/// One node of a rendered tree.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedNode {
    /// The node's `type`; empty for a node without one.
    pub pattern: String,
    /// `type` is a binding (`@config.viewPattern`) resolved at run time.
    pub dynamic: bool,
    /// The renderer draws it: a real renderer, a draw host painting its children, or a node without a `type`.
    pub supported: bool,
    /// The renderer declares a stub for it.
    pub stub: bool,
    pub binding: EntityBinding,
    /// The `@trait.X` string children of this node.
    pub trait_refs: Vec<TraitRef>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntityBindingKind {
    /// Not an entity-bound pattern (`data-grid`, `table-view`, `data-list`), or a dynamic `type`.
    None,
    /// An entity-bound pattern with no `entity`, or a null one.
    Absent,
    /// `entity` is a number, boolean or object: neither rows nor a binding.
    Malformed,
    /// `entity` resolved to its rows, carried inline.
    Inline,
    /// `entity` names an entity type, read from the client's loaded rows.
    Named,
    /// `entity` is a `@payload` binding the event did not resolve.
    PayloadUnresolved,
    /// `entity` is another `@` binding (`@entity.items`, `@config.x`) the headless seed does not synthesize.
    ContextUnresolved,
}

/// What an entity-bound pattern's `entity` resolved to.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityBinding {
    pub kind: EntityBindingKind,
    /// The entity type (`named`), the owner entity of the rendering trait (`inline`), the binding (`payload-unresolved`,
    /// `context-unresolved`); otherwise empty.
    pub entity: String,
    /// The inline rows, or the loaded rows of a `named` entity; otherwise 0.
    pub rows: u32,
    /// `entity` declares a real `@read`, so an empty list may be what the viewer is meant to see (`inline`, `named`).
    pub read_gated: bool,
}

/// A `@trait.X` reference in a rendered tree.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitRef {
    pub name: String,
    /// `X` has rendered content in some orbital after a seeded INIT.
    pub resolved: bool,
    /// `X` renders only once an overlay-open transition fires.
    pub overlay_only: bool,
}

/// A request `program/step` played: the request as asked, and the circuit it ran.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayedStep {
    pub orbital: String,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    #[cfg_attr(test, ts(type = "EventPayload"))]
    pub payload: Value,
    pub result: PlayedResult,
}

/// Each proposition judged on its own through [`judge_propositions`], so every outcome is reported, not the first.
pub fn judge_each_proposition(
    ensures: &[SExpression],
    observation: &PropositionObservation,
) -> Vec<PlayedEnsures> {
    ensures
        .iter()
        .map(|p| {
            let (verdict, reason, counterexample) =
                match judge_propositions(std::slice::from_ref(p), observation) {
                    PropositionVerdict::Held => (EnsuresVerdict::Held, None, None),
                    PropositionVerdict::Broken { .. } => (
                        EnsuresVerdict::Broken,
                        None,
                        Some(PropositionBindings {
                            entity: observation.entity_after.clone(),
                            from_entity: observation.entity_before.clone(),
                            payload: observation.payload.clone(),
                        }),
                    ),
                    PropositionVerdict::Inconclusive { reason } => {
                        (EnsuresVerdict::Inconclusive, Some(reason), None)
                    }
                };
            PlayedEnsures {
                proposition: p.clone(),
                verdict,
                reason,
                counterexample,
            }
        })
        .collect()
}

/// The `@ensures` outcomes of one played hop of `trait_def` for `event`. The kernel must record each step's row
/// after its effects (`set_record_entity_after`), or the run reads as unobserved.
pub fn played_ensures(
    trait_def: &TraitDefinition,
    event: &str,
    hop: &ProcessEventResult,
) -> Vec<PlayedEnsures> {
    let mut arms = trait_def
        .state_machine
        .transitions
        .iter()
        .filter(|t| t.event == event && t.from.states().contains(&hop.previous_state.as_str()));
    let ensures: Vec<SExpression> = match hop.fired_arm {
        Some(n) => arms
            .nth(n)
            .and_then(|t| t.ensures.clone())
            .unwrap_or_default(),
        None => arms
            .flat_map(|t| t.ensures.iter().flatten().cloned())
            .collect(),
    };
    judge_each_proposition(&ensures, &PropositionObservation::of_hop(hop))
}

/// One authored effect run as `program/step` reports it.
pub fn played_effect(run: &EffectRun) -> PlayedEffect {
    let expr = serde_json::to_value(&run.effect).unwrap_or(serde_json::Value::Null);
    let (effect_type, args) = match expr.as_array().map(Vec::as_slice) {
        Some([serde_json::Value::String(head), args @ ..]) => (
            head.clone(),
            args.iter()
                .map(|a| serde_json::from_value(a.clone()).unwrap_or(Value::Null))
                .collect(),
        ),
        _ => (String::new(), Vec::new()),
    };
    let status = match run.status {
        EffectRunStatus::Executed => PlayedEffectStatus::Executed,
        EffectRunStatus::Failed => PlayedEffectStatus::Failed,
        EffectRunStatus::Skipped => PlayedEffectStatus::Skipped,
    };
    let routed = status == PlayedEffectStatus::Failed
        && orbital_core_base::effect_types::parse_effect(&run.effect)
            .as_ref()
            .and_then(orbital_core_base::effect_types::Effect::emit_routes)
            .is_some_and(|routes| routes.failure.is_some());
    PlayedEffect {
        effect_type,
        args,
        status,
        error: run.error.clone(),
        routed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_played_step_round_trips_through_its_wire() {
        let wire = json!({
            "orbital": "DoorOrbital",
            "trait": "Lock",
            "from": "locked",
            "event": "UNLOCK",
            "payload": { "code": "1234" },
            "result": {
                "trait": "Lock",
                "event": "UNLOCK",
                "transitionFired": true,
                "guard": "pass",
                "firedArm": 0,
                "state": { "before": "locked", "after": "open" },
                "effects": [{ "type": "set", "args": ["@entity.code", "@payload.code"], "status": "executed", "routed": false }],
                "emitted": [{ "type": "OPENED", "payload": { "by": "code" }, "timestamp": 0 }],
                "guards": [{ "arm": 0, "guard": ["=", "@payload.code", "1234"], "passed": true, "trace": [] }],
                "ensures": [{ "proposition": ["=", "@entity.code", "@payload.code"], "verdict": "held" }]
            }
        });
        let played: PlayedStep = serde_json::from_value(wire.clone()).expect("a played step");
        assert_eq!(played.result.guard, GuardVerdict::Pass);
        assert_eq!(played.result.fired_arm, Some(0));
        assert_eq!(serde_json::to_value(&played).unwrap(), wire);

        let mut unfired = wire.clone();
        unfired["result"]["guard"] = json!("none");
        unfired["result"]["transitionFired"] = json!(false);
        unfired["result"]["state"] = json!({ "before": null, "after": null });
        unfired["result"]
            .as_object_mut()
            .unwrap()
            .remove("firedArm");
        let played: PlayedStep = serde_json::from_value(unfired.clone()).expect("an unfired step");
        assert_eq!(played.result.guard, GuardVerdict::Unguarded);
        assert_eq!(played.result.fired_arm, None);
        assert_eq!(serde_json::to_value(&played).unwrap(), unfired);

        let mut bad = wire;
        bad["result"]["guard"] = json!("maybe");
        assert!(
            serde_json::from_value::<PlayedStep>(bad).is_err(),
            "a guard verdict outside pass|fail|none is refused"
        );
    }

    fn value(v: serde_json::Value) -> Value {
        serde_json::from_value(v).expect("a value")
    }

    fn sexpr(v: serde_json::Value) -> SExpression {
        serde_json::from_value(v).expect("an S-expression")
    }

    #[test]
    fn each_ensures_is_held_broken_with_its_counterexample_or_inconclusive_when_unfired() {
        let held = sexpr(json!([
            "=",
            "@entity.count",
            ["+", "@fromEntity.count", "@payload.by"]
        ]));
        let broken = sexpr(json!(["=", "@entity.count", 99]));
        let observation = PropositionObservation {
            executed: true,
            failed: false,
            entity_before: value(json!({ "count": 1 })),
            entity_after: value(json!({ "count": 2 })),
            payload: value(json!({ "by": 1 })),
            unobserved: None,
        };
        let judged = judge_each_proposition(&[held.clone(), broken.clone()], &observation);
        assert_eq!(
            judged.len(),
            2,
            "one outcome per proposition, not the first failure only"
        );
        assert_eq!(judged[0].verdict, EnsuresVerdict::Held);
        assert_eq!(judged[0].proposition, held);
        assert!(judged[0].counterexample.is_none() && judged[0].reason.is_none());
        assert_eq!(judged[1].verdict, EnsuresVerdict::Broken);
        assert_eq!(judged[1].proposition, broken);
        let counter = judged[1]
            .counterexample
            .as_ref()
            .expect("a broken one carries its bindings");
        assert_eq!(
            counter,
            &PropositionBindings {
                entity: value(json!({ "count": 2 })),
                from_entity: value(json!({ "count": 1 })),
                payload: value(json!({ "by": 1 })),
            }
        );

        let unfired = PropositionObservation {
            executed: false,
            ..observation
        };
        let judged = judge_each_proposition(&[held, broken], &unfired);
        assert!(judged
            .iter()
            .all(|j| j.verdict == EnsuresVerdict::Inconclusive
                && j.counterexample.is_none()
                && j.reason.as_deref() == Some("the transition did not fire")));
        assert_eq!(
            serde_json::to_value(&judged[0]).unwrap()["verdict"],
            json!("inconclusive")
        );
    }

    #[test]
    fn a_failed_effect_is_routed_only_when_it_declares_its_failure_event() {
        let routes = sexpr(
            json!(["http", "GET", "https://api.example.com", "/items", {},
            { "emit": { "success": "CALLED", "failure": "CALL_FAILED" } }]),
        );
        let bare = sexpr(json!([
            "http",
            "GET",
            "https://api.example.com",
            "/items",
            {}
        ]));
        let run = |effect: &SExpression, status| {
            played_effect(&EffectRun {
                effect: effect.clone(),
                status,
                error: None,
            })
        };
        let routed = run(&routes, EffectRunStatus::Failed);
        assert!(routed.routed);
        assert_eq!(routed.effect_type, "http");
        assert_eq!(routed.status, PlayedEffectStatus::Failed);
        assert!(
            !run(&bare, EffectRunStatus::Failed).routed,
            "control: no declared failure route"
        );
        assert!(
            !run(&routes, EffectRunStatus::Executed).routed,
            "control: nothing failed, nothing routed"
        );
        assert_eq!(
            serde_json::to_value(run(&bare, EffectRunStatus::Skipped)).unwrap()["routed"],
            json!(false)
        );
    }
}
