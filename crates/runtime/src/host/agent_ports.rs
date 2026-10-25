//! The host half of `program/dispatch` (EXEC-AGENT-1): what a dispatch answers a tool-calling model. A host that
//! dispatches the declared input the kernel asked for, as the dispatch's viewer, through its ordinary input
//! channel, turns that response into the answer with [`dispatch_outcome`]. Twin of `dispatchOutcomeOf`
//! (`@almadar/core`); the shared corpus pins both.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::effects::response::EventResponse;
use orbital_core_base::value::{Value, ValueMap};
pub use orbital_core_base::agent_ports::{inputs_reads, plan_inputs, InputsReads};

fn spelled<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|json| json.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Why a dispatch did not take effect, or `None` when it did.
pub fn dispatch_failure(response: &EventResponse) -> Option<String> {
    if let Some(rejections) = response.rejections.as_ref().filter(|r| !r.is_empty()) {
        return Some(
            rejections
                .iter()
                .map(|r| spelled(&r.code))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if let Some(guard) = &response.guard_failed {
        return Some(format!("guard failed: {guard}"));
    }
    if !response.success {
        return Some(
            response
                .error
                .clone()
                .unwrap_or_else(|| "dispatch failed".to_string()),
        );
    }
    if let Some(failed) = response
        .effect_results
        .iter()
        .flatten()
        .find(|r| !r.success)
    {
        return Some(
            failed
                .error
                .clone()
                .unwrap_or_else(|| format!("{} failed", spelled(&failed.effect))),
        );
    }
    if !response.transitioned {
        return Some("no transition".to_string());
    }
    None
}

/// What a dispatch answers the model: `{ ok: true, emitted: [{ event, payload? }] }`, or `{ ok: false, error }`.
pub fn dispatch_outcome(response: &EventResponse) -> Value {
    let mut map = ValueMap::default();
    match dispatch_failure(response) {
        Some(error) => {
            map.insert("ok".to_string(), Value::Bool(false));
            map.insert("error".to_string(), Value::String(error));
        }
        None => {
            let emitted = response
                .emitted_events
                .iter()
                .map(|e| {
                    let mut event = ValueMap::default();
                    event.insert("event".to_string(), Value::String(e.event.clone()));
                    if let Some(payload) = &e.payload {
                        event.insert("payload".to_string(), payload.clone());
                    }
                    Value::Object(event)
                })
                .collect();
            map.insert("ok".to_string(), Value::Bool(true));
            map.insert("emitted".to_string(), Value::Array(emitted));
        }
    }
    Value::Object(map)
}

/// A `program/*` answer as its asker reads it, answered in place or deferred alike: a `program/dispatch` declared
/// input or event request answers the dispatch outcome ([`dispatch_outcome`]); a `wire` request and an inbound
/// delivery answer what the host answered, as does every other op. `args` is the ask the host was given.
pub fn program_answer(op: &str, args: &[Value], answer: Value) -> Result<Value, crate::effects::executor::SubstrateFailure> {
    let raw = op != "program/dispatch"
        || matches!(args, [Value::Object(_)])
        || matches!(args.get(2), Some(Value::Object(options)) if options.get("wire") == Some(&Value::Bool(true)));
    if raw {
        return Ok(answer);
    }
    let response: EventResponse = serde_json::from_value(orbital_core_base::json_value::value_to_json(&answer)).map_err(|e| {
        crate::effects::executor::SubstrateFailure { message: format!("program/dispatch: the host answered no event response: {e}"), errors: Vec::new() }
    })?;
    Ok(dispatch_outcome(&response))
}

/// The success payload of a substrate or program op's completion, in place or deferred: `{ result }`, plus `text` (the
/// result as JSON text) for an agent-port op.
pub fn completion_result(op: &str, value: Value) -> Value {
    let text = orbital_core_base::agent_ports::is_agent_port_operator(op)
        .then(|| serde_json::to_string(&orbital_core_base::json_value::value_to_json(&value)).unwrap_or_default());
    let mut payload = ValueMap::default();
    payload.insert("result".to_string(), value);
    if let Some(text) = text {
        payload.insert("text".to_string(), Value::String(text));
    }
    Value::Object(payload)
}

/// The failure payload of a substrate or program op's completion: `{ error }`, plus `errors` when the op gave any.
pub fn failure_payload(failure: &crate::effects::executor::SubstrateFailure) -> Value {
    let mut payload = ValueMap::default();
    payload.insert("error".to_string(), Value::String(failure.message.clone()));
    if !failure.errors.is_empty() {
        payload.insert("errors".to_string(), Value::Array(failure.errors.clone()));
    }
    Value::Object(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as Json;

    const VECTORS: &str = include_str!("../../tests/fixtures/agent-ports/vectors.json");

    fn norm(json: Json) -> Json {
        match json {
            Json::Number(n) => match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Json::from(f as i64),
                _ => Json::Number(n),
            },
            Json::Array(items) => Json::Array(items.into_iter().map(norm).collect()),
            Json::Object(map) => Json::Object(map.into_iter().map(|(k, v)| (k, norm(v))).collect()),
            other => other,
        }
    }

    #[test]
    fn a_response_answers_the_model_the_same_way_for_every_shared_vector() {
        let fixture: Json = serde_json::from_str(VECTORS).expect("the shared vectors parse");
        let vectors = fixture["dispatchOutcomes"]
            .as_array()
            .expect("dispatchOutcomes");
        assert!(
            vectors.len() >= 10,
            "the corpus carries the whole failure ladder"
        );
        for vector in vectors {
            let name = vector["name"].as_str().unwrap_or_default();
            let response: EventResponse =
                serde_json::from_value(vector["response"].clone()).expect("a response");
            let outcome =
                norm(serde_json::to_value(dispatch_outcome(&response)).expect("serializes"));
            assert_eq!(outcome, norm(vector["outcome"].clone()), "{name}");
        }
    }

    #[test]
    fn a_dispatch_answer_is_shaped_by_its_ask_whether_answered_in_place_or_deferred() {
        let response = || serde_json::from_value::<Value>(serde_json::to_value(EventResponse { success: true, transitioned: true, ..EventResponse::default() }).unwrap()).unwrap();
        let text = |s: &str| Value::String(s.to_string());
        let object = |json: Json| serde_json::from_value::<Value>(json).unwrap();
        let shaped = |args: Vec<Value>| program_answer("program/dispatch", &args, response()).map(|v| norm(serde_json::to_value(&v).unwrap()));
        let shaped_from = |answer: Result<Value, crate::effects::executor::SubstrateFailure>| norm(serde_json::to_value(answer.expect("shaped")).unwrap());
        let outcome = serde_json::json!({ "ok": true, "emitted": [] });
        assert_eq!(shaped(vec![text("O.T.GO"), object(serde_json::json!({})), object(serde_json::json!({ "orbital": "O", "event": "GO", "viewer": null }))]).unwrap(), outcome, "an event request");
        assert_eq!(shaped(vec![text("O.T.GO"), object(serde_json::json!({}))]).unwrap(), outcome, "a declared input");
        let raw = norm(serde_json::to_value(response()).unwrap());
        assert_eq!(shaped(vec![text("O.T.GO"), object(serde_json::json!({})), object(serde_json::json!({ "orbital": "O", "event": "GO", "wire": true }))]).unwrap(), raw, "control: a wire request answers the whole response");
        let hook = object(serde_json::json!({ "status": 200, "body": "{}" }));
        let inbound = program_answer("program/dispatch", &[object(serde_json::json!({ "inbound": "h", "body": "", "headers": {} }))], hook.clone()).unwrap();
        assert_eq!(inbound, hook, "control: an inbound delivery answers the handler's status and body");
        assert_eq!(program_answer("program/read", &[text("x")], text("y")).unwrap(), text("y"), "control: another op is not reshaped");
        assert!(program_answer("program/dispatch", &[text("O.T.GO"), object(serde_json::json!({}))], text("no response")).is_err(), "edge: a host that answers no event response fails");
        let mut counted = ValueMap::default();
        counted.insert("trait".to_string(), text("T"));
        counted.insert("from".to_string(), text("idle"));
        counted.insert("to".to_string(), text("idle"));
        counted.insert("event".to_string(), text("GO"));
        counted.insert("firedArm".to_string(), Value::Number(1.0));
        counted.insert("effects".to_string(), Value::Array(Vec::new()));
        let Value::Object(mut with_arm) = response() else { panic!("a response is a record") };
        with_arm.insert("transitions".to_string(), Value::Array(vec![Value::Object(counted)]));
        assert_eq!(
            shaped_from(program_answer("program/dispatch", &[text("O.T.GO"), object(serde_json::json!({}))], Value::Object(with_arm))),
            outcome,
            "edge: a whole number the host answers as the kernel holds numbers (1.0) still reads as an event response"
        );
    }

    #[test]
    fn control_a_scripted_response_with_no_failure_is_not_reported_as_one() {
        let response = EventResponse {
            success: true,
            transitioned: true,
            ..EventResponse::default()
        };
        assert_eq!(dispatch_failure(&response), None);
    }
}

