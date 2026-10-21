//! The host half of `program/dispatch` (EXEC-AGENT-1): what a dispatch answers a tool-calling model. A host that
//! dispatches the declared input the kernel asked for, as the dispatch's viewer, through its ordinary input
//! channel, turns that response into the answer with [`dispatch_outcome`]. Twin of `dispatchOutcomeOf`
//! (`@almadar/core`); the shared corpus pins both.

use crate::effects::response::EventResponse;
use orbital_core_base::value::{Value, ValueMap};

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
    let mut map = ValueMap::new();
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
                    let mut event = ValueMap::new();
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

/// The success payload of a substrate or program op's completion, in place or deferred: `{ result }`, plus `text` (the
/// result as JSON text) for an agent-port op.
pub fn completion_result(op: &str, value: Value) -> Value {
    let text = orbital_core_base::agent_ports::is_agent_port_operator(op)
        .then(|| serde_json::to_string(&orbital_core_base::json_value::value_to_json(&value)).unwrap_or_default());
    let mut payload = ValueMap::new();
    payload.insert("result".to_string(), value);
    if let Some(text) = text {
        payload.insert("text".to_string(), Value::String(text));
    }
    Value::Object(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as Json;

    const VECTORS: &str = include_str!("../tests/fixtures/agent-ports/vectors.json");

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
    fn control_a_scripted_response_with_no_failure_is_not_reported_as_one() {
        let response = EventResponse {
            success: true,
            transitioned: true,
            ..EventResponse::default()
        };
        assert_eq!(dispatch_failure(&response), None);
    }
}

