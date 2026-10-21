//! A program's state machine as values (`MachineRow`, read by the `std-machine` atom) and the record `program/step`
//! answers with (`PlayedStep`); `@almadar/core` names both as program types.

use crate::embed::eval_wire::EvalStepWire;
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
