//! Declared inbound deliveries: the `/api/hooks/<name>` requests a program accepts from outside
//! (a provider's webhook). The app declares the names (`inbound: ["google-calendar"]`) and a trait
//! declares the handler (`listens { EVENT -> inbound "google-calendar" answers ANSWER { … } }`,
//! lowered onto its `stateMachine.events[]` entry as `inbound`). The host only carries bytes:
//! it hands the kernel `{name, headers, rawBody}` and writes back the answer the kernel returns.
//! Mirrors `@almadar/core` `src/access/inbound.ts` (EXEC-INBOUND-1).

use crate::schema::types::{InboundDeclaration, InboundVerification, OrbitalSchema, TraitReference};
use crate::value::{Value, ValueMap};
use serde::{Deserialize, Serialize};

/// One declared handler: the trait and event a hook name is delivered to, and the event that answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundRoute {
    pub name: String,
    pub orbital: String,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifies: Option<InboundVerification>,
}

/// The HTTP answer the host writes: the status and the JSON text of the body. `stderr`, when the answer carries one,
/// is diagnostic text a terminal host (`orb <command>`) prints to its standard error; an HTTP host does not send it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundAnswer {
    pub status: u16,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

/// Every inbound handler the program declares, in schema order.
pub fn inbound_routes_of(schema: &OrbitalSchema) -> Vec<InboundRoute> {
    let mut out = Vec::new();
    for orbital in &schema.orbitals {
        for trait_ref in &orbital.traits {
            let TraitReference::Inline(td) = trait_ref else {
                continue;
            };
            for ev in td.state_machine.events.iter().flatten() {
                let Some(InboundDeclaration {
                    name,
                    answers,
                    verifies,
                }) = &ev.inbound
                else {
                    continue;
                };
                out.push(InboundRoute {
                    name: name.clone(),
                    orbital: orbital.name.clone(),
                    trait_name: td.name.clone(),
                    event: ev.key.clone(),
                    answer: answers.clone(),
                    verifies: verifies.clone(),
                });
            }
        }
    }
    out
}

/// The handler for `name`: declared by the app's `inbound` list AND by exactly one trait. Anything
/// else is the 404 answer a host writes for a hook it does not serve.
pub fn find_inbound(schema: &OrbitalSchema, name: &str) -> Result<InboundRoute, InboundAnswer> {
    let declared = schema.inbound.iter().flatten().any(|n| n == name);
    let mut routes = inbound_routes_of(schema).into_iter().filter(|r| r.name == name);
    match (declared, routes.next(), routes.next()) {
        (true, Some(route), None) => Ok(route),
        _ => Err(unknown_hook(name)),
    }
}

fn error_body(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

/// The answer for a hook name the program does not serve.
pub fn unknown_hook(name: &str) -> InboundAnswer {
    InboundAnswer {
        status: 404,
        body: error_body(&format!("Unknown hook provider: {name}")),
        stderr: None,
    }
}

/// The answer for a delivery whose dispatch failed; the sender retries a 5xx.
pub fn dispatch_failed(message: &str) -> InboundAnswer {
    InboundAnswer {
        status: 500,
        body: error_body(message),
        stderr: None,
    }
}

/// The answer for a delivery whose dispatch reached its step cap before the handler answered: the answer it owed
/// never comes, so the cut is the answer, naming the cap and the trait and event it stopped at.
pub fn cascade_capped(cap: usize, trait_name: &str, event: &str) -> InboundAnswer {
    InboundAnswer {
        status: 500,
        body: serde_json::json!({
            "error": format!("cascade-cap: the dispatch reached its {cap}-step cap before {trait_name} handled {event}, so the handler never answered"),
            "code": "cascade-cap",
            "cap": cap,
            "trait": trait_name,
            "event": event,
        })
        .to_string(),
        stderr: None,
    }
}

/// The answer for a delivery whose handler never emitted its answer event and has no open call left that could bring
/// one: a waiting host would otherwise park forever, so the stall is the answer, naming the handler's trait and event.
pub fn inbound_unanswered(trait_name: &str, event: &str) -> InboundAnswer {
    InboundAnswer {
        status: 500,
        body: serde_json::json!({
            "error": format!("inbound-unanswered: {trait_name} handled {event}, but it never emitted its answer event and no open call is left that could bring one"),
            "code": "inbound-unanswered",
            "trait": trait_name,
            "event": event,
        })
        .to_string(),
        stderr: None,
    }
}

/// Equality in time that depends on the length of the longer value only, never on where they differ.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

/// Whether the sender passed the route's `verifies` check. `None` when the route declares none. An
/// unset or empty credential never passes: a declared check with no secret is refused before delivery
/// ([`credential_unset`]).
pub fn sender_verified(route: &InboundRoute, header_value: &str, credential: Option<&str>) -> Option<bool> {
    route.verifies.as_ref()?;
    Some(match credential {
        None | Some("") => false,
        Some(expected) => constant_time_eq(header_value, expected),
    })
}

/// The answer for a route whose `verifies` names a credential the host holds no value for: nothing runs.
pub fn credential_unset(credential: &str) -> InboundAnswer {
    InboundAnswer {
        status: 401,
        body: error_body(&format!("credential {credential} is not set")),
        stderr: None,
    }
}

/// The payload an inbound event is delivered with: `headers` (names lower-cased; a name given twice
/// joins its values with `, `), `rawBody` exactly as received, `body`, the parsed JSON of `rawBody`
/// or null when it is empty or not JSON, and, when the route declares `verifies`, `verified`: the
/// kernel's comparison of the named header with the named credential, whose value stays out of the payload.
pub fn inbound_payload<'a>(
    route: &InboundRoute,
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
    raw_body: &str,
    credential: Option<&str>,
) -> Value {
    let mut lowered = ValueMap::new();
    for (name, value) in headers {
        let key = name.to_lowercase();
        let joined = match lowered.get(&key) {
            Some(Value::String(prev)) => format!("{prev}, {value}"),
            _ => value.to_string(),
        };
        lowered.insert(key, Value::String(joined));
    }
    let verified = route.verifies.as_ref().and_then(|v| {
        let seen = lowered.get(&v.header).and_then(Value::as_str).unwrap_or("");
        sender_verified(route, seen, credential)
    });
    let body = serde_json::from_str::<Value>(raw_body).unwrap_or(Value::Null);
    let mut payload = ValueMap::new();
    payload.insert("headers".to_string(), Value::Object(lowered));
    payload.insert("body".to_string(), body);
    payload.insert("rawBody".to_string(), Value::String(raw_body.to_string()));
    if let Some(verified) = verified {
        payload.insert("verified".to_string(), Value::Bool(verified));
    }
    Value::Object(payload)
}

/// The trait that handles `event` as an inbound hook, when a client-facing request names it (`trait_name` is
/// the request's target trait, `None` when it names none). Such a request is refused: only the hook intake
/// delivers an inbound event.
pub fn inbound_only_trait(
    schema: &OrbitalSchema,
    orbital: &str,
    trait_name: Option<&str>,
    event: &str,
) -> Option<String> {
    inbound_routes_of(schema)
        .into_iter()
        .find(|r| r.orbital == orbital && r.event == event && trait_name.map_or(true, |t| t == r.trait_name))
        .map(|r| r.trait_name)
}

/// The refusal message for an inbound-only event named on a client-facing route.
pub fn inbound_only_message(trait_name: &str, event: &str) -> String {
    format!("{trait_name}.{event} is an inbound hook event: only /api/hooks/<name> delivers it")
}

/// The answer a dispatch produced: the first emit of the route's answer event, whose payload is
/// `{ status, body, stderr? }` (`stderr` absent or null is none). A dispatch that emitted none, or one whose payload is
/// not that shape, is a 500.
pub fn answer_of<'a>(route: &InboundRoute, emits: impl IntoIterator<Item = (&'a str, &'a Value)>) -> InboundAnswer {
    let Some((_, payload)) = emits.into_iter().find(|(event, _)| *event == route.answer) else {
        return dispatch_failed("inbound-unanswered");
    };
    let status = payload.get_path("status").and_then(Value::as_number);
    let body = payload.get_path("body").and_then(Value::as_str);
    let stderr = match payload.get_path("stderr") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(()),
    };
    match (status, body, stderr) {
        (Some(s), Some(b), Ok(stderr)) if s.fract() == 0.0 && (100.0..=599.0).contains(&s) => InboundAnswer {
            status: s as u16,
            body: b.to_string(),
            stderr,
        },
        _ => dispatch_failed("inbound-answer-invalid"),
    }
}

/// The answer a host reads off a finished dispatch's response: [`answer_of`] its emitted events when the route's
/// answer event is among them; otherwise `failure` (the response's error) or `inbound-unanswered`, as a 500. The
/// response-level twin of `inboundAnswerOf` in `@almadar/core`.
pub fn answer_of_response<'a>(
    route: &InboundRoute,
    emitted: impl IntoIterator<Item = (&'a str, &'a Value)> + Clone,
    failure: Option<&str>,
) -> InboundAnswer {
    if emitted.clone().into_iter().any(|(event, _)| event == route.answer) {
        return answer_of(route, emitted);
    }
    dispatch_failed(failure.unwrap_or("inbound-unanswered"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(inbound: serde_json::Value, second_handler: bool) -> OrbitalSchema {
        let mut events = vec![
            json!({ "key": "INIT" }),
            json!({ "key": "PUSH", "listened": true, "inbound": { "name": "google-calendar", "answers": "PUSH_ANSWERED" } }),
        ];
        if second_handler {
            events.push(
                json!({ "key": "PUSH_AGAIN", "inbound": { "name": "google-calendar", "answers": "PUSH_ANSWERED" } }),
            );
        }
        serde_json::from_value(json!({
            "name": "App",
            "inbound": inbound,
            "orbitals": [{
                "name": "CalOrbital",
                "entity": { "name": "Req", "collection": "reqs", "persistence": "runtime",
                            "fields": [{ "name": "id", "type": "string", "required": true }] },
                "traits": [{
                    "name": "Inbound",
                    "linkedEntity": "Req",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": events,
                        "transitions": [{ "from": "idle", "to": "idle", "event": "PUSH" }]
                    }
                }],
                "pages": []
            }]
        }))
        .expect("fixture deserializes")
    }

    #[test]
    fn a_declared_handler_is_found_by_its_name() {
        let route = find_inbound(&schema(json!(["google-calendar"]), false), "google-calendar").unwrap();
        assert_eq!(
            route,
            InboundRoute {
                name: "google-calendar".into(),
                orbital: "CalOrbital".into(),
                trait_name: "Inbound".into(),
                event: "PUSH".into(),
                answer: "PUSH_ANSWERED".into(),
                verifies: None,
            }
        );
    }

    #[test]
    fn a_handler_the_app_does_not_declare_is_not_served() {
        let refused = find_inbound(&schema(json!([]), false), "google-calendar").unwrap_err();
        assert_eq!(refused.status, 404);
        assert_eq!(refused.body, r#"{"error":"Unknown hook provider: google-calendar"}"#);
        let absent: OrbitalSchema = serde_json::from_value(json!({ "name": "App", "orbitals": [] })).unwrap();
        assert_eq!(find_inbound(&absent, "google-calendar").unwrap_err().status, 404);
    }

    #[test]
    fn a_declared_name_nobody_handles_and_a_name_two_traits_handle_are_not_served() {
        assert_eq!(
            find_inbound(&schema(json!(["slack"]), false), "slack")
                .unwrap_err()
                .status,
            404
        );
        assert_eq!(
            find_inbound(&schema(json!(["google-calendar"]), true), "google-calendar")
                .unwrap_err()
                .status,
            404
        );
    }

    #[test]
    fn the_payload_lower_cases_header_names_and_parses_the_body() {
        let payload = inbound_payload(
            &route(),
            [("X-Goog-Channel-Id", "c1"), ("Accept", "a"), ("accept", "b")],
            r#"{"k":1}"#,
            None,
        );
        assert_eq!(
            payload.get_path("headers.x-goog-channel-id"),
            Some(&Value::String("c1".into()))
        );
        assert_eq!(payload.get_path("headers.accept"), Some(&Value::String("a, b".into())));
        assert_eq!(payload.get_path("body.k"), Some(&Value::Number(1.0)));
        assert_eq!(payload.get_path("rawBody"), Some(&Value::String(r#"{"k":1}"#.into())));
    }

    #[test]
    fn an_empty_or_non_json_body_is_null_and_the_raw_text_is_kept() {
        for raw in ["", "not json"] {
            let payload = inbound_payload(&route(), [], raw, None);
            assert_eq!(payload.get_path("body"), Some(&Value::Null), "{raw}");
            assert_eq!(payload.get_path("rawBody"), Some(&Value::String(raw.into())), "{raw}");
        }
    }

    fn verifying_route() -> InboundRoute {
        InboundRoute {
            verifies: Some(InboundVerification {
                header: "x-goog-channel-token".into(),
                credential: "GOOGLE_CALENDAR_CHANNEL_TOKEN".into(),
            }),
            ..route()
        }
    }

    fn verified(route: &InboundRoute, token: Option<&str>, credential: Option<&str>) -> Option<bool> {
        let headers: Vec<(&str, &str)> = token.map(|t| ("X-Goog-Channel-Token", t)).into_iter().collect();
        inbound_payload(route, headers, "", credential)
            .get_path("verified")
            .and_then(Value::as_bool)
    }

    #[test]
    fn the_kernel_compares_the_named_header_with_the_credential_and_the_secret_stays_out_of_the_payload() {
        assert_eq!(verified(&verifying_route(), Some("s3cret"), Some("s3cret")), Some(true));
        assert_eq!(verified(&verifying_route(), Some("wrong"), Some("s3cret")), Some(false));
        assert_eq!(verified(&verifying_route(), None, Some("s3cret")), Some(false));
        let payload = inbound_payload(
            &verifying_route(),
            [("X-Goog-Channel-Token", "wrong")],
            "",
            Some("s3cret"),
        );
        assert!(!serde_json::to_string(&payload).unwrap().contains("s3cret"));
    }

    #[test]
    fn an_unset_or_empty_credential_never_verifies() {
        assert_eq!(verified(&verifying_route(), Some("anything"), None), Some(false));
        assert_eq!(verified(&verifying_route(), None, Some("")), Some(false));
        assert_eq!(verified(&verifying_route(), Some(""), Some("")), Some(false), "an empty header never matches an empty secret");
    }

    #[test]
    fn an_unset_credential_is_a_401_naming_it() {
        let answer = credential_unset("GOOGLE_CALENDAR_CHANNEL_TOKEN");
        assert_eq!(answer.status, 401);
        assert_eq!(answer.body, r#"{"error":"credential GOOGLE_CALENDAR_CHANNEL_TOKEN is not set"}"#);
    }

    #[test]
    fn control_a_route_with_no_verifies_carries_no_verified_field() {
        assert_eq!(verified(&route(), Some("s3cret"), Some("s3cret")), None);
    }

    #[test]
    fn the_comparison_is_exact_for_unequal_lengths_and_non_ascii() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("abcd", "abc"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
        assert!(constant_time_eq("tökén", "tökén"));
        assert!(!constant_time_eq("tökén", "token"));
    }

    #[test]
    fn an_inbound_event_is_refused_on_a_client_route_and_nothing_else_is() {
        let s = schema(json!(["google-calendar"]), false);
        assert_eq!(
            inbound_only_trait(&s, "CalOrbital", Some("Inbound"), "PUSH"),
            Some("Inbound".to_string())
        );
        assert_eq!(
            inbound_only_trait(&s, "CalOrbital", None, "PUSH"),
            Some("Inbound".to_string())
        );
        assert_eq!(inbound_only_trait(&s, "CalOrbital", Some("Other"), "PUSH"), None);
        assert_eq!(inbound_only_trait(&s, "CalOrbital", Some("Inbound"), "INIT"), None);
        assert_eq!(inbound_only_trait(&s, "OtherOrbital", None, "PUSH"), None);
    }

    fn route() -> InboundRoute {
        find_inbound(&schema(json!(["google-calendar"]), false), "google-calendar").unwrap()
    }

    fn emit(status: serde_json::Value, body: serde_json::Value) -> Value {
        serde_json::from_value(json!({ "status": status, "body": body })).unwrap()
    }

    #[test]
    fn the_answer_is_the_first_emit_of_the_answer_event() {
        let first = emit(json!(400), json!("{\"error\":\"bad-token\"}"));
        let second = emit(json!(200), json!("{}"));
        let other = Value::Null;
        let answer = answer_of(
            &route(),
            [("OTHER", &other), ("PUSH_ANSWERED", &first), ("PUSH_ANSWERED", &second)],
        );
        assert_eq!(
            answer,
            InboundAnswer {
                status: 400,
                body: "{\"error\":\"bad-token\"}".into(),
                stderr: None
            }
        );
    }

    #[test]
    fn a_stalled_delivery_answers_naming_the_handler() {
        let answer = inbound_unanswered("VerifyWalkCli", "VERIFY_WALK_CALLED");
        assert_eq!(answer.status, 500);
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(body["code"], "inbound-unanswered");
        assert_eq!(body["trait"], "VerifyWalkCli");
        assert_eq!(body["event"], "VERIFY_WALK_CALLED");
        assert!(body["error"].as_str().unwrap().starts_with("inbound-unanswered: VerifyWalkCli handled VERIFY_WALK_CALLED"));
    }

    #[test]
    fn an_answer_carries_its_stderr_and_one_without_or_with_null_stderr_has_none() {
        let with: Value = serde_json::from_value(json!({ "status": 500, "body": "", "stderr": "orb x: failed" })).unwrap();
        let answer = answer_of(&route(), [("PUSH_ANSWERED", &with)]);
        assert_eq!(
            answer,
            InboundAnswer {
                status: 500,
                body: String::new(),
                stderr: Some("orb x: failed".into())
            }
        );
        let null: Value = serde_json::from_value(json!({ "status": 200, "body": "ok", "stderr": null })).unwrap();
        assert_eq!(answer_of(&route(), [("PUSH_ANSWERED", &null)]).stderr, None);
        let without = emit(json!(200), json!("ok"));
        assert_eq!(
            answer_of(&route(), [("PUSH_ANSWERED", &without)]),
            InboundAnswer {
                status: 200,
                body: "ok".into(),
                stderr: None
            }
        );
    }

    #[test]
    fn a_dispatch_with_no_answer_or_a_malformed_one_is_a_500() {
        let none = answer_of(&route(), std::iter::empty());
        assert_eq!(
            (none.status, none.body.as_str()),
            (500, r#"{"error":"inbound-unanswered"}"#)
        );
        for bad in [
            emit(json!("200"), json!("{}")),
            emit(json!(200.5), json!("{}")),
            emit(json!(99), json!("{}")),
            emit(json!(600), json!("{}")),
            emit(json!(200), json!(1)),
            serde_json::from_value(json!({ "status": 200, "body": "", "stderr": 1 })).unwrap(),
        ] {
            let answer = answer_of(&route(), [("PUSH_ANSWERED", &bad)]);
            assert_eq!(
                (answer.status, answer.body.as_str()),
                (500, r#"{"error":"inbound-answer-invalid"}"#),
                "{bad:?}"
            );
        }
    }

    fn calendar_route() -> InboundRoute {
        find_inbound(&schema(json!(["google-calendar"]), false), "google-calendar").unwrap()
    }

    #[test]
    fn a_response_that_emitted_the_answer_event_answers_with_its_status_and_body() {
        let answer = json!({ "status": 202, "body": "ok" });
        let other = json!({});
        let answer: Value = serde_json::from_value(answer).unwrap();
        let other: Value = serde_json::from_value(other).unwrap();
        let emitted = [("NOISE", &other), ("PUSH_ANSWERED", &answer)];
        assert_eq!(answer_of_response(&calendar_route(), emitted, Some("ignored")), InboundAnswer { status: 202, body: "ok".into(), stderr: None });
    }

    #[test]
    fn control_without_the_answer_event_the_responses_error_or_unanswered_is_a_500() {
        let empty: [(&str, &Value); 0] = [];
        assert_eq!(answer_of_response(&calendar_route(), empty, Some("boom")), dispatch_failed("boom"));
        assert_eq!(answer_of_response(&calendar_route(), empty, None), dispatch_failed("inbound-unanswered"));
    }
}
