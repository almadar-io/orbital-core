// `browser/page-facts`, `browser/prompt` and `browser/embed` through the kernel: each is one journaled host ask (EXEC-HTTP-3).

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_base::ir::PageFactField;
use orbital_core_runtime::resumable::{answer_ports, IoAnswer, IoReply, IoRequest, ModelFailure, ModelFailureKind, PageFacts};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

const COLLECTOR: &str = "https://collector.example.com";

fn schema(effect: Json) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "egress": [COLLECTOR],
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Probe", "linkedEntity": "Job",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }, { "name": "failed" }, { "name": "stopped" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "GO", "effects": [effect] },
                        { "from": "idle", "to": "idle", "event": "CANCEL", "effects": [["cancel-call", "job"]] },
                        { "from": "idle", "to": "done", "event": "OK" },
                        { "from": "idle", "to": "failed", "event": "BAD" },
                        { "from": "idle", "to": "stopped", "event": "STOPPED" }
                    ]
                } }]
        }]
    }))
    .unwrap()
}

fn go() -> EventRequest {
    EventRequest {
        event: "GO".to_string(),
        target_trait: Some("Probe".to_string()),
        payload: Some(serde_json::from_value(json!({ "body": "hello" })).unwrap()),
        ..Default::default()
    }
}

fn run(effect: Json, host: impl FnMut(&IoRequest) -> IoAnswer) -> (EventResponse, Vec<IoRequest>) {
    run_with(effect, go(), host)
}

fn run_with(effect: Json, request: EventRequest, mut host: impl FnMut(&IoRequest) -> IoAnswer) -> (EventResponse, Vec<IoRequest>) {
    let server = EmbeddedServer::new(schema(effect)).unwrap();
    let mut seen = Vec::new();
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return (response, seen),
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| IoReply {
                        id: pending.id,
                        answer: match &pending.request {
                            IoRequest::ReadRow { .. } => IoAnswer::Row(Err("no row".to_string())),
                            other => {
                                seen.push(other.clone());
                                host(other)
                            }
                        },
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
            ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
        }
    }
}

fn emitted(response: &EventResponse) -> Vec<(&str, Option<&Value>)> {
    response.emitted_events.iter().map(|e| (e.event.as_str(), e.payload.as_ref())).collect()
}

fn payload_of(response: &EventResponse, event: &str) -> Json {
    let found = emitted(response)
        .into_iter()
        .find(|(e, _)| *e == event)
        .unwrap_or_else(|| panic!("{event} was not emitted: {:?}", emitted(response)));
    serde_json::to_value(found.1).unwrap()
}

fn routes() -> Json {
    json!({ "emit": { "success": "OK", "failure": "BAD", "cancelled": "STOPPED" } })
}

fn facts(hostname: &str, referrer: &str, width: Option<f64>) -> PageFacts {
    PageFacts { available: true, hostname: hostname.into(), referrer: referrer.into(), screen_width: width, href: format!("https://{hostname}/page?q=1") }
}

// --- page facts ---------------------------------------------------------------------------------------------

#[test]
fn page_facts_ask_the_host_once_in_list_order_and_emit_flat() {
    let (response, seen) = run(json!(["browser/page-facts", ["referrer", "screenWidth"], routes()]), |request| match request {
        IoRequest::PageFacts { .. } => IoAnswer::PageFacts(Ok(facts("ignored.example", "https://r.example/", Some(1280.0)))),
        other => panic!("the fixture asks for {other:?}"),
    });
    assert_eq!(seen, vec![IoRequest::PageFacts { fields: vec![PageFactField::Referrer, PageFactField::ScreenWidth] }]);
    assert_eq!(
        payload_of(&response, "OK"),
        json!({ "available": true, "hostname": "", "referrer": "https://r.example/", "screenWidth": 1280.0, "href": "" })
    );
}

#[test]
fn page_facts_keep_unrequested_facts_at_their_defaults_whatever_the_host_answers() {
    let (response, _) = run(json!(["browser/page-facts", ["hostname"], routes()]), |_| {
        IoAnswer::PageFacts(Ok(facts("site.example", "https://leak.example/", Some(99.0))))
    });
    assert_eq!(
        payload_of(&response, "OK"),
        json!({ "available": true, "hostname": "site.example", "referrer": "", "screenWidth": null, "href": "" })
    );
}

#[test]
fn page_facts_href_is_the_full_location_when_requested_and_empty_when_not() {
    let answer = |_: &IoRequest| IoAnswer::PageFacts(Ok(facts("site.example", "", None)));
    let (response, seen) = run(json!(["browser/page-facts", ["href"], routes()]), answer);
    assert_eq!(seen, vec![IoRequest::PageFacts { fields: vec![PageFactField::Href] }]);
    assert_eq!(
        payload_of(&response, "OK"),
        json!({ "available": true, "hostname": "", "referrer": "", "screenWidth": null, "href": "https://site.example/page?q=1" })
    );
    let (response, _) = run(json!(["browser/page-facts", ["hostname"], routes()]), answer);
    assert_eq!(payload_of(&response, "OK").get("href"), Some(&json!("")));
}

#[test]
fn page_facts_with_no_page_are_available_false_with_the_defaults() {
    let (response, _) = run(json!(["browser/page-facts", ["hostname", "referrer", "screenWidth"], routes()]), |_| {
        IoAnswer::PageFacts(Ok(PageFacts { available: false, hostname: "x".into(), referrer: "y".into(), screen_width: Some(5.0), href: "https://x/".into() }))
    });
    assert_eq!(
        payload_of(&response, "OK"),
        json!({ "available": false, "hostname": "", "referrer": "", "screenWidth": null, "href": "" })
    );
}

#[test]
fn page_facts_a_getter_that_throws_is_the_failure_route_not_no_page() {
    let (response, _) = run(json!(["browser/page-facts", ["hostname"], routes()]), |_| {
        IoAnswer::PageFacts(Err("referrer blocked".into()))
    });
    assert_eq!(payload_of(&response, "BAD"), json!({ "error": "referrer blocked" }));
    assert!(!emitted(&response).iter().any(|(e, _)| *e == "OK"));
}

#[test]
fn page_facts_refuse_a_bad_field_list_without_asking_the_host() {
    for fields in [json!([]), json!(["userAgent"]), json!(["hostname", "hostname"]), json!("hostname")] {
        let (response, seen) = run(json!(["browser/page-facts", fields, routes()]), |request| panic!("refusal asked {request:?}"));
        assert!(seen.is_empty());
        assert!(payload_of(&response, "BAD").get("error").is_some());
    }
}

// --- beacon -------------------------------------------------------------------------------------------------


#[test]
fn page_facts_key_is_evaluated_and_echoed_on_success_and_failure_and_absent_means_no_key_field() {
    let keyed = |key: Json| json!(["browser/page-facts", ["hostname"], { "key": key, "emit": { "success": "OK", "failure": "BAD" } }]);
    let (response, _) = run(keyed(json!(["str/concat", "k:", "@payload.body"])), |_| {
        IoAnswer::PageFacts(Ok(facts("site.example", "", None)))
    });
    assert_eq!(
        payload_of(&response, "OK"),
        json!({ "available": true, "hostname": "site.example", "referrer": "", "screenWidth": null, "href": "", "key": "k:hello" })
    );
    let (response, _) = run(keyed(json!("fixed")), |_| IoAnswer::PageFacts(Err("blocked".into())));
    assert_eq!(payload_of(&response, "BAD"), json!({ "error": "blocked", "key": "fixed" }));
    let (response, _) = run(json!(["browser/page-facts", ["hostname"], routes()]), |_| {
        IoAnswer::PageFacts(Ok(facts("site.example", "", None)))
    });
    assert!(payload_of(&response, "OK").get("key").is_none());
}

#[test]
fn page_facts_key_that_is_not_a_non_empty_string_never_asks_the_host() {
    for key in [json!(17), json!(null), json!(""), json!(["+", 1, 2])] {
        let (_, seen) = run(
            json!(["browser/page-facts", ["hostname"], { "key": key, "emit": { "success": "OK", "failure": "BAD" } }]),
            |request| panic!("invalid key asked {request:?}"),
        );
        assert!(seen.is_empty());
    }
}

// --- on-device model: `browser/prompt` and `browser/embed` ----------------------------------------------------
// The twin of `packages/almadar-runtime/test/browser-on-device-model.test.ts`: the same effects, the same host
// behaviour, the same emitted payloads.

const SCHEMA: &str = "{\"type\":\"object\"}";

fn prompt(key: Option<&str>) -> Json {
    let options = match key {
        Some(key) => json!({ "key": key, "emit": routes()["emit"].clone() }),
        None => routes(),
    };
    json!(["browser/prompt", "@payload.body", SCHEMA, options])
}

fn embed(texts: Json, key: Option<&str>) -> Json {
    let options = match key {
        Some(key) => json!({ "key": key, "emit": routes()["emit"].clone() }),
        None => routes(),
    };
    json!(["browser/embed", texts, options])
}

fn failure(kind: ModelFailureKind, message: &str) -> ModelFailure {
    ModelFailure::new(kind, message)
}

// EXEC-HTTP-3
#[test]
fn a_prompt_asks_the_host_once_with_the_text_and_schema_and_emits_the_reply_with_a_null_key() {
    let (response, seen) = run(prompt(None), |_| IoAnswer::Prompt(Ok("{\"category\":\"a\"}".to_string())));
    assert_eq!(seen, vec![IoRequest::Prompt { text: "hello".into(), schema: SCHEMA.into(), key: None }]);
    assert_eq!(payload_of(&response, "OK"), json!({ "reply": "{\"category\":\"a\"}" }));
}

#[test]
fn a_keyed_prompt_carries_its_key_to_the_host_and_echoes_it() {
    let (response, seen) = run(prompt(Some("batch-1")), |_| IoAnswer::Prompt(Ok("{}".to_string())));
    assert_eq!(seen, vec![IoRequest::Prompt { text: "hello".into(), schema: SCHEMA.into(), key: Some("batch-1".into()) }]);
    assert_eq!(payload_of(&response, "OK"), json!({ "reply": "{}", "key": "batch-1" }));
}

#[test]
fn a_host_with_no_model_fails_prompt_unconfigured_and_a_model_that_cannot_answer_fails_unavailable() {
    let (response, _) = run(prompt(Some("k")), |_| {
        IoAnswer::Prompt(Err(failure(ModelFailureKind::Unconfigured, "browser/prompt: no on-device model is wired into this host")))
    });
    assert_eq!(
        payload_of(&response, "BAD"),
        json!({ "error": "browser/prompt: no on-device model is wired into this host", "kind": "unconfigured", "key": "k" })
    );
    let (response, _) = run(prompt(None), |_| IoAnswer::Prompt(Err(failure(ModelFailureKind::Unavailable, "The on-device model is downloading"))));
    assert_eq!(
        payload_of(&response, "BAD"),
        json!({ "error": "The on-device model is downloading", "kind": "unavailable" })
    );
    let (response, _) = run(prompt(None), |_| IoAnswer::Prompt(Err(failure(ModelFailureKind::Failed, "quota exceeded"))));
    assert_eq!(payload_of(&response, "BAD"), json!({ "error": "quota exceeded", "kind": "failed" }));
    assert!(!emitted(&response).iter().any(|(e, _)| *e == "OK"));
}

#[test]
fn a_blocking_host_with_no_model_or_embedder_answers_unconfigured() {
    let asked = IoRequest::Prompt { text: "t".into(), schema: SCHEMA.into(), key: None };
    assert_eq!(
        answer_ports(None, &asked),
        IoAnswer::Prompt(Err(failure(ModelFailureKind::Unconfigured, "browser/prompt: no on-device model is wired into this host")))
    );
    let asked = IoRequest::Embed { texts: vec!["t".into()], key: None };
    assert_eq!(
        answer_ports(None, &asked),
        IoAnswer::Embed(Err(failure(ModelFailureKind::Unconfigured, "browser/embed: no on-device embedder is wired into this host")))
    );
}

#[test]
fn a_prompt_text_or_schema_that_is_not_text_fails_without_asking_the_host() {
    for (text, schema) in [(json!(5), json!(SCHEMA)), (json!("hello"), json!({ "type": "object" }))] {
        let (response, seen) = run(
            json!(["browser/prompt", text, schema, routes()]),
            |request| panic!("a refusal asked {request:?}"),
        );
        assert!(seen.is_empty());
        assert_eq!(
            payload_of(&response, "BAD"),
            json!({ "error": "browser/prompt: the text and the response schema must be text", "kind": "failed" })
        );
    }
}

#[test]
fn a_prompt_failure_with_no_route_is_a_rejection_and_a_bad_key_never_asks_the_host() {
    let (response, _) = run(
        json!(["browser/prompt", "t", SCHEMA, { "emit": { "success": "OK" } }]),
        |_| IoAnswer::Prompt(Err(failure(ModelFailureKind::Failed, "quota exceeded"))),
    );
    assert!(response
        .rejections
        .iter()
        .flatten()
        .any(|r| r.code == orbital_core_runtime::effects::response::TransitionRejectionCode::EffectFailed));
    for key in [json!(17), json!(null), json!(""), json!(["+", 1, 2])] {
        let (_, seen) = run(
            json!(["browser/prompt", "t", SCHEMA, { "key": key, "emit": routes()["emit"].clone() }]),
            |request| panic!("invalid key asked {request:?}"),
        );
        assert!(seen.is_empty());
    }
}

#[test]
fn a_cancelled_prompt_or_embed_fires_the_cancelled_route_with_the_key_and_not_success() {
    let (response, _) = run(prompt(Some("job")), |_| IoAnswer::ServiceCancelled);
    assert_eq!(payload_of(&response, "STOPPED"), json!({ "key": "job" }));
    assert!(!emitted(&response).iter().any(|(e, _)| matches!(*e, "OK" | "BAD")));
    let (response, _) = run(embed(json!(["a"]), Some("job")), |_| IoAnswer::ServiceCancelled);
    assert_eq!(payload_of(&response, "STOPPED"), json!({ "key": "job" }));
    assert!(!emitted(&response).iter().any(|(e, _)| matches!(*e, "OK" | "BAD")));
}

#[test]
fn control_an_unkeyed_cancelled_answer_fires_nothing() {
    let (response, _) = run(prompt(None), |_| IoAnswer::ServiceCancelled);
    assert!(emitted(&response).iter().all(|(e, _)| !matches!(*e, "OK" | "BAD" | "STOPPED")));
}

#[test]
fn cancel_call_reaches_the_host_with_the_key_of_the_prompt_and_an_unknown_key_is_the_hosts_no_op() {
    let cancel = EventRequest { event: "CANCEL".to_string(), target_trait: Some("Probe".to_string()), ..Default::default() };
    let (_, seen) = run_with(prompt(Some("job")), cancel, |request| match request {
        IoRequest::CancelCall { key } if key == "job" => IoAnswer::Cancelled(Ok(())),
        other => panic!("the fixture asks for {other:?}"),
    });
    assert_eq!(seen, vec![IoRequest::CancelCall { key: "job".into() }]);
}

#[test]
fn embed_asks_the_host_once_with_the_texts_and_emits_one_vector_per_text_with_the_key() {
    let (response, seen) = run(embed(json!(["a", "b"]), Some("k")), |_| IoAnswer::Embed(Ok(vec![vec![0.0, 0.5], vec![1.0, 1.5]])));
    assert_eq!(seen, vec![IoRequest::Embed { texts: vec!["a".into(), "b".into()], key: Some("k".into()) }]);
    assert_eq!(payload_of(&response, "OK"), json!({ "embeddings": [[0.0, 0.5], [1.0, 1.5]], "key": "k" }));
}

#[test]
fn embed_of_no_texts_succeeds_with_no_vectors_and_never_asks_the_host() {
    let (response, seen) = run(embed(json!([]), None), |request| panic!("an empty embed asked {request:?}"));
    assert!(seen.is_empty());
    assert_eq!(payload_of(&response, "OK"), json!({ "embeddings": [] }));
}

#[test]
fn embed_hands_back_whatever_vectors_the_host_answers_and_leaves_the_count_check_to_the_program() {
    let (response, _) = run(embed(json!(["a", "b"]), None), |_| IoAnswer::Embed(Ok(vec![vec![1.0]])));
    assert_eq!(payload_of(&response, "OK"), json!({ "embeddings": [[1.0]] }));
}

#[test]
fn embed_texts_that_are_not_a_list_of_text_fail_without_asking_the_host() {
    for texts in [json!("a"), json!(["a", 2])] {
        let (response, seen) = run(embed(texts, None), |request| panic!("a refusal asked {request:?}"));
        assert!(seen.is_empty());
        assert_eq!(
            payload_of(&response, "BAD"),
            json!({ "error": "browser/embed: the texts must be a list of text", "kind": "failed" })
        );
    }
}

#[test]
fn a_host_with_no_embedder_fails_embed_unconfigured() {
    let (response, _) = run(embed(json!(["a"]), None), |_| {
        IoAnswer::Embed(Err(failure(ModelFailureKind::Unconfigured, "browser/embed: no on-device embedder is wired into this host")))
    });
    assert_eq!(
        payload_of(&response, "BAD"),
        json!({ "error": "browser/embed: no on-device embedder is wired into this host", "kind": "unconfigured" })
    );
}
