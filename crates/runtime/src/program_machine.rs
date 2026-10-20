//! A program's state machine as values (`program/machine`) and the record `program/step` answers with
//! (`PlayedStep`). The rows are the planner's own transition groups ([`planner::plan_trait`]), so the machine a
//! `.lolo` program reads is the one the walkers drive; `@almadar/core` names both as program types.

use crate::embed::eval_wire::EvalStepWire;
use crate::planner;
use orbital_core_base::schema::types::{
    OrbitalSchema, PayloadField, TraitDefinition, TraitReference, TransitionDefinition,
};
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

fn payload_slots(td: &TraitDefinition, event: &str) -> Vec<PayloadSlot> {
    let declared: &[PayloadField] = td
        .state_machine
        .events
        .as_deref()
        .and_then(|events| events.iter().find(|e| e.key == event))
        .and_then(|e| e.payload_schema.as_deref())
        .unwrap_or(&[]);
    declared
        .iter()
        .map(|f| PayloadSlot {
            name: f.name.clone(),
            field_type: f.field_type.clone(),
            required: f.required.unwrap_or(false),
            values: f.values.clone(),
            entity: f.entity.clone(),
        })
        .collect()
}

/// The `position`-th `from --event-->` arm in declaration order, the index the planner and the kernel's
/// `fired_arm` count.
fn arm_at<'a>(
    td: &'a TraitDefinition,
    from: &str,
    event: &str,
    position: usize,
) -> Option<&'a TransitionDefinition> {
    td.state_machine
        .transitions
        .iter()
        .filter(|t| t.event == event && t.from.matches(from))
        .nth(position)
}

/// Every transition of every inline trait of a resolved program, orbital by orbital, in the planner's group
/// order. Sibling unguarded transitions sharing `(from, event)` are one planner step and one row per target.
pub fn machine_rows(schema: &OrbitalSchema) -> Vec<MachineRow> {
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = planner::EntityCtx {
        schema,
        owner_fields: &owner_fields,
        viewer_id: None,
    };
    let mut rows = Vec::new();
    for orbital in &schema.orbitals {
        for reference in &orbital.traits {
            let TraitReference::Inline(td) = reference else {
                continue;
            };
            let seed_id = td.linked_entity.as_deref().map(planner::seed_row_id);
            for step in planner::plan_trait(td, &ctx, seed_id.as_deref()) {
                let row = |to: &str, guarded: bool| MachineRow {
                    orbital: orbital.name.clone(),
                    trait_name: step.trait_name.clone(),
                    from: step.from.clone(),
                    event: step.event.clone(),
                    to: to.to_string(),
                    guarded,
                    payload: payload_slots(td, &step.event),
                };
                match step.arm {
                    Some(position) => {
                        let guarded = arm_at(td, &step.from, &step.event, position)
                            .is_some_and(|t| t.guard.is_some());
                        rows.push(row(&step.to, guarded));
                    }
                    None => rows.extend(step.to_states.iter().map(|to| row(to, false))),
                }
            }
        }
    }
    rows
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn program() -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "machine-fixture",
            "orbitals": [{
                "name": "DoorOrbital",
                "entity": { "name": "Door", "fields": [{ "name": "id", "type": "string", "required": true }] },
                "traits": [
                    {
                        "name": "Lock",
                        "linkedEntity": "Door",
                        "stateMachine": {
                            "states": [{ "name": "locked", "isInitial": true }, { "name": "open" }],
                            "events": [
                                { "key": "UNLOCK", "name": "Unlock", "payloadSchema": [
                                    { "name": "code", "type": "string", "required": true },
                                    { "name": "mode", "type": "string", "values": ["once", "hold"] }
                                ] },
                                { "key": "SHUT", "name": "Shut" }
                            ],
                            "transitions": [
                                { "from": "locked", "to": "open", "event": "UNLOCK", "guard": ["=", "@payload.code", "1234"] },
                                { "from": "locked", "to": "locked", "event": "UNLOCK" },
                                { "from": "open", "to": "locked", "event": "SHUT" }
                            ]
                        }
                    },
                    {
                        "name": "Bell",
                        "linkedEntity": "Door",
                        "stateMachine": {
                            "states": [{ "name": "quiet", "isInitial": true }],
                            "events": [],
                            "transitions": []
                        }
                    }
                ]
            }]
        }))
        .expect("fixture program")
    }

    #[test]
    fn machine_rows_are_every_transition_with_its_guard_and_typed_payload() {
        let rows = machine_rows(&program());
        let edges: Vec<(&str, &str, &str, &str, bool)> = rows
            .iter()
            .map(|r| {
                (
                    r.trait_name.as_str(),
                    r.from.as_str(),
                    r.event.as_str(),
                    r.to.as_str(),
                    r.guarded,
                )
            })
            .collect();
        assert_eq!(
            edges,
            vec![
                ("Lock", "locked", "UNLOCK", "open", true),
                ("Lock", "locked", "UNLOCK", "locked", false),
                ("Lock", "open", "SHUT", "locked", false)
            ]
        );
        assert!(rows.iter().all(|r| r.orbital == "DoorOrbital"));
        let unlock = &rows[0].payload;
        assert_eq!(
            unlock,
            &vec![
                PayloadSlot {
                    name: "code".into(),
                    field_type: "string".into(),
                    required: true,
                    values: None,
                    entity: None
                },
                PayloadSlot {
                    name: "mode".into(),
                    field_type: "string".into(),
                    required: false,
                    values: Some(vec!["once".into(), "hold".into()]),
                    entity: None
                },
            ]
        );
        assert!(
            rows[2].payload.is_empty(),
            "an event declaring no payload has no slots"
        );
        let wire = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(wire["trait"], "Lock");
        assert_eq!(wire["payload"][0]["type"], "string");
        assert!(wire["payload"][0].get("values").is_none());
    }

    #[test]
    fn a_trait_with_no_transitions_has_no_rows() {
        let rows = machine_rows(&program());
        assert!(
            rows.iter().all(|r| r.trait_name != "Bell"),
            "control: Bell declares no transitions"
        );
        let mut empty = program();
        empty.orbitals[0].traits.truncate(0);
        assert!(machine_rows(&empty).is_empty());
    }

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
                "effects": [{ "type": "set", "args": ["@entity.code", "@payload.code"], "status": "executed" }],
                "emitted": [{ "type": "OPENED", "payload": { "by": "code" }, "timestamp": 0 }],
                "guards": [{ "arm": 0, "guard": ["=", "@payload.code", "1234"], "passed": true, "trace": [] }]
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
}
