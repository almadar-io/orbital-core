//! The text a guest's client host ask travels in (EXEC-ENDPOINT-3a). The guest kernel writes the ask, a viewer relays it
//! with `program/host-ask`, the viewer's host answers it and writes its outcome as text, and
//! `program/dispatch { ask, answer } { guest }` fires the ask's declared route inside the guest. `.lolo` only moves the
//! two strings, so no value crosses the relay untyped.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::client_effect::HostAskCompletion;
use crate::value::{Value, ValueMap};

/// One client host ask a guest made: the op, its resolved arguments and where its answer returns in the guest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayedAsk {
    pub op: String,
    #[serde(default)]
    pub args: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<HostAskCompletion>,
}

/// The outcome a viewer's host reached for a relayed ask: `{ ok }` with the op's success payload, or `{ error, kind }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RelayedAnswer {
    Ok { ok: Value },
    Refused { error: String, kind: String },
}

/// The ask as the text a guest answer carries.
pub fn write(op: &str, args: &[Value], completion: Option<&HostAskCompletion>) -> String {
    let ask = RelayedAsk { op: op.to_string(), args: args.to_vec(), completion: completion.cloned() };
    serde_json::to_string(&ask).unwrap_or_default()
}

/// Read a relayed ask's text; refused unless it names a client host ask other than an identity ask or another relay.
pub fn relayed(arg: Option<&Value>) -> Result<RelayedAsk, String> {
    let Some(text) = arg.and_then(Value::as_str) else {
        return Err("program/host-ask takes the ask text a guest answer carries".to_string());
    };
    let ask: RelayedAsk = serde_json::from_str(text).map_err(|e| format!("program/host-ask: the ask is not one a guest wrote: {e}"))?;
    if ask.op == "program/host-ask" || crate::runs_on::is_identity_ask(&ask.op) {
        return Err(format!("program/host-ask: {} is not an ask a viewer answers for a guest", ask.op));
    }
    if crate::runs_on::runs_on_known(&ask.op) != Some(crate::runs_on::RunsOn::Client) {
        return Err(format!("program/host-ask: {} is not a client host ask", ask.op));
    }
    Ok(ask)
}

/// Read a host's answer text.
pub fn answer(text: &str) -> Result<RelayedAnswer, String> {
    serde_json::from_str(text).map_err(|e| format!("program/dispatch: the answer is not one a host wrote: {e}"))
}

/// The route event and payload the asking trait receives, as a browser host returns a host ask (EXEC-DIST-13): the
/// success route with the answer's fields and `key`, or the failure route with `{ error, kind, key }`. `None` when the
/// ask declared no route for that outcome, so nothing fires.
pub fn route(ask: &RelayedAsk, outcome: &RelayedAnswer) -> Option<(String, Value)> {
    let completion = ask.completion.as_ref()?;
    let mut payload = ValueMap::default();
    let event = match outcome {
        RelayedAnswer::Ok { ok } => {
            if let Value::Object(fields) = ok {
                for (name, value) in fields.iter() {
                    payload.insert(name.clone(), value.clone());
                }
            }
            completion.success.clone()?
        }
        RelayedAnswer::Refused { error, kind } => {
            payload.insert("error".into(), Value::string(error.clone()));
            payload.insert("kind".into(), Value::string(kind.clone()));
            completion.failure.clone()?
        }
    };
    if let Some(key) = &completion.key {
        payload.insert("key".into(), Value::string(key.clone()));
    }
    Some((event, Value::object(payload)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(success: Option<&str>, failure: Option<&str>, key: Option<&str>) -> RelayedAsk {
        RelayedAsk {
            op: "audio/clock".into(),
            args: vec![],
            completion: Some(HostAskCompletion {
                trait_name: "Clock".into(),
                success: success.map(str::to_string),
                failure: failure.map(str::to_string),
                key: key.map(str::to_string),
                ..HostAskCompletion::default()
            }),
        }
    }

    fn field(payload: &Value, name: &str) -> Option<Value> {
        match payload {
            Value::Object(fields) => fields.get(name).cloned(),
            _ => None,
        }
    }

    #[test]
    fn an_answer_fires_the_success_route_with_its_fields_and_the_key() {
        let outcome = answer(r#"{"ok":{"audio":5,"wall":9}}"#).unwrap();
        let (event, payload) = route(&ask(Some("TICKED"), Some("FAILED"), Some("k1")), &outcome).unwrap();
        assert_eq!(event, "TICKED");
        assert_eq!(field(&payload, "audio"), Some(Value::from(5)));
        assert_eq!(field(&payload, "key"), Some(Value::string("k1".to_string())));
    }

    #[test]
    fn a_refusal_fires_the_failure_route_with_error_and_kind() {
        let outcome = answer(r#"{"error":"no AudioContext","kind":"unavailable"}"#).unwrap();
        let (event, payload) = route(&ask(Some("TICKED"), Some("FAILED"), None), &outcome).unwrap();
        assert_eq!(event, "FAILED");
        assert_eq!(field(&payload, "kind"), Some(Value::string("unavailable".to_string())));
        assert_eq!(field(&payload, "key"), None, "edge: no key declared, none added");
    }

    #[test]
    fn control_an_outcome_whose_route_the_ask_did_not_declare_fires_nothing() {
        let refused = answer(r#"{"error":"x","kind":"invalid"}"#).unwrap();
        assert!(route(&ask(Some("TICKED"), None, None), &refused).is_none());
        assert!(route(&RelayedAsk { op: "audio/clock".into(), args: vec![], completion: None }, &refused).is_none());
    }

    #[test]
    fn an_ask_round_trips_through_its_text() {
        let text = write("audio/play-sound", &[Value::string("step".to_string())], ask(Some("A"), None, None).completion.as_ref());
        let back: RelayedAsk = serde_json::from_str(&text).unwrap();
        assert_eq!(back.op, "audio/play-sound");
        assert_eq!(back.args, vec![Value::string("step".to_string())]);
        assert!(answer("not an answer").is_err(), "edge: text no host wrote is refused");
    }
}
