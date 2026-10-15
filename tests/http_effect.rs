// The `http` effect through the kernel (docs/Almadar_Embedded_Kernel.md, "The `http` effect"; EXEC-HTTP-1):
// credentials are asked by name, the request is built once and answered by the host, a reply of any
// status is a success, a refusal never reaches the host, and every ask is journaled so a replayed step
// repeats nothing.

use orbital_core::embed::{EmbeddedServer, ServerProgress};
use orbital_core::ir::{OirEffect, OirExpr};
use orbital_core::runtime::http_request::HttpMethod;
use orbital_core::runtime::http_service::{HttpRequest, HttpResponse, TransportError};
use orbital_core::runtime::resumable::{IoAnswer, IoReply, IoRequest};
use orbital_core::schema::SExpression;
use orbital_core::{EventRequest, EventResponse, OrbitalSchema, Value};
use serde_json::{json, Value as Json};

const ORIGIN: &str = "https://api.example.com";
const AUTH_ORIGIN: &str = "https://auth.example.com";
const TOKEN_URL: &str = "https://auth.example.com/token";
const CACHE: &str = "oauth2:https://auth.example.com/token|cid_name";

fn options() -> Json {
    json!({ "key": "job", "emit": {
        "success": "SEARCHED", "failure": "SEARCH_FAILED", "cancelled": "SEARCH_CANCELLED", "onMessage": "STEP"
    } })
}

fn bearer(request: Json) -> Json {
    let mut request = request;
    request["auth"] = json!({ "scheme": "bearer", "credential": "api_key_name" });
    request
}

fn call(origin: &str, request: Json) -> Json {
    json!(["http", "GET", origin, "/search", request, options()])
}

fn schema(effect: Json) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "egress": [ORIGIN, AUTH_ORIGIN],
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Fetcher", "linkedEntity": "Job",
                "stateMachine": {
                    "states": [
                        { "name": "idle", "isInitial": true }, { "name": "asking" },
                        { "name": "done" }, { "name": "failed" }, { "name": "stopped" }
                    ],
                    "transitions": [
                        { "from": "idle", "to": "asking", "event": "ASK", "effects": [effect] },
                        { "from": "idle", "to": "idle", "event": "CANCEL", "effects": [["cancel-call", "job"]] },
                        { "from": "asking", "to": "done", "event": "SEARCHED" },
                        { "from": "asking", "to": "failed", "event": "SEARCH_FAILED" },
                        { "from": "asking", "to": "stopped", "event": "SEARCH_CANCELLED" }
                    ]
                } }]
        }]
    }))
    .unwrap()
}

fn ask() -> EventRequest {
    EventRequest {
        event: "ASK".to_string(),
        target_trait: Some("Fetcher".to_string()),
        payload: Some(serde_json::from_value(json!({ "q": "cats" })).unwrap()),
        ..Default::default()
    }
}

/// Run `request`, answering every ask but row reads with `host`; the asks it saw, in order.
fn run(
    schema: OrbitalSchema,
    request: EventRequest,
    mut host: impl FnMut(&IoRequest) -> IoAnswer,
) -> (EventResponse, Vec<IoRequest>) {
    let server = EmbeddedServer::new(schema).unwrap();
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
        }
    }
}

fn reply(status: u16, headers: &[(&str, &str)], body: &str) -> IoAnswer {
    IoAnswer::Http(Ok(HttpResponse {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: body.as_bytes().to_vec(),
    }))
}

/// A host holding the credential `api_key_name`, answering every `Http` with `answer`.
fn keyed_host(answer: IoAnswer) -> impl FnMut(&IoRequest) -> IoAnswer {
    move |request| match request {
        IoRequest::Secret { name, .. } if name == "api_key_name" => {
            IoAnswer::Secret(Ok(Some("s3cret".to_string())))
        }
        IoRequest::Http { .. } => answer.clone(),
        other => panic!("the fixture asks for {other:?}"),
    }
}

fn emitted(response: &EventResponse) -> Vec<(&str, Option<&Value>)> {
    response
        .emitted_events
        .iter()
        .map(|e| (e.event.as_str(), e.payload.as_ref()))
        .collect()
}

fn state(response: &EventResponse) -> Option<&str> {
    response.states.get("Fetcher").map(String::as_str)
}

fn payload_of<'a>(response: &'a EventResponse, event: &str) -> &'a Value {
    emitted(response)
        .into_iter()
        .find(|(e, _)| *e == event)
        .and_then(|(_, p)| p)
        .unwrap_or_else(|| panic!("no {event} with a payload in {:?}", emitted(response)))
}

fn http_asks(seen: &[IoRequest]) -> Vec<&HttpRequest> {
    seen.iter()
        .filter_map(|r| match r {
            IoRequest::Http { request, .. } => Some(request),
            _ => None,
        })
        .collect()
}

fn header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn field_str<'a>(value: &'a Value, path: &str) -> &'a str {
    value
        .get_path(path)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{path} is not a string in {value:?}"))
}

#[test]
fn the_orb_form_lowers_to_an_http_effect_with_its_options() {
    let form: SExpression = serde_json::from_value(json!([
        "http", "POST", ORIGIN, "/items",
        { "json": { "a": 1 }, "paginate": {
            "style": "cursor", "param": "after", "max": 2,
            "next": ["object/get", "@response", "next"], "collect": ["list", "@response.body"] } },
        { "key": "@entity.id", "emit": { "success": "OK", "failure": "BAD" } }
    ]))
    .unwrap();
    let OirExpr::Effect { effect } = orbital_core::lower::lower_sexpr(&form).unwrap() else {
        panic!("an effect");
    };
    let OirEffect::Http {
        method,
        request,
        key,
        emit,
        ..
    } = *effect
    else {
        panic!("an http effect");
    };
    assert_eq!(method, HttpMethod::Post);
    assert!(key.is_some());
    assert_eq!(emit.and_then(|e| e.success), Some("OK".to_string()));
    let request = serde_json::to_string(&request.expect("a request")).unwrap();
    assert!(request.contains("quote"), "next and collect are held as data: {request}");
    assert!(
        !request.contains("@response"),
        "the held expressions carry no raw binding: {request}"
    );
}

#[test]
fn control_a_bare_http_lowers_without_request_key_or_emit() {
    let form: SExpression = serde_json::from_value(json!(["http", "GET", ORIGIN, "/ping"])).unwrap();
    let OirExpr::Effect { effect } = orbital_core::lower::lower_sexpr(&form).unwrap() else {
        panic!("an effect");
    };
    let OirEffect::Http {
        request, key, emit, ..
    } = *effect
    else {
        panic!("an http effect");
    };
    assert!(request.is_none() && key.is_none() && emit.is_none());
}

#[test]
fn a_method_outside_the_five_and_a_short_form_do_not_lower() {
    for form in [
        json!(["http", "FETCH", ORIGIN, "/x"]),
        json!(["http", "GET", ORIGIN]),
        json!(["http", "GET", ORIGIN, "/x", {}, {}, {}]),
    ] {
        let form: SExpression = serde_json::from_value(form).unwrap();
        assert!(orbital_core::lower::lower_sexpr(&form).is_err(), "{form:?}");
    }
}

#[test]
fn a_call_asks_the_credential_then_the_request_and_fires_success_with_the_reply() {
    let effect = call(ORIGIN, bearer(json!({ "query": { "q": "@payload.q" }, "headers": { "Accept": "application/json" } })));
    let (response, seen) = run(
        schema(effect),
        ask(),
        keyed_host(reply(200, &[("Content-Type", "application/json")], "{\"hits\":1}")),
    );
    assert!(matches!(&seen[0], IoRequest::Secret { name, .. } if name == "api_key_name"), "{seen:?}");
    let IoRequest::Http { request, key, .. } = &seen[1] else {
        panic!("the second ask is the request: {seen:?}");
    };
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(key.as_deref(), Some("job"));
    assert_eq!(request.method, "GET");
    assert_eq!(request.url, "https://api.example.com/search?q=cats");
    assert_eq!(header(request, "Authorization"), Some("Bearer s3cret"));
    assert_eq!(header(request, "Accept"), Some("application/json"));
    assert_eq!(state(&response), Some("done"));
    let payload = payload_of(&response, "SEARCHED");
    assert_eq!(payload.get_path("status").and_then(Value::as_number), Some(200.0));
    assert_eq!(field_str(payload, "body"), "{\"hits\":1}");
    assert_eq!(field_str(payload, "headers.content-type"), "application/json");
}

#[test]
fn a_replayed_step_asks_each_request_once() {
    let effect = call(ORIGIN, bearer(json!({})));
    let (_, seen) = run(schema(effect), ask(), keyed_host(reply(200, &[], "")));
    let secrets = seen.iter().filter(|r| matches!(r, IoRequest::Secret { .. })).count();
    assert_eq!((secrets, http_asks(&seen).len()), (1, 1), "{seen:?}");
}

#[test]
fn a_non_2xx_reply_is_a_success() {
    let effect = call(ORIGIN, bearer(json!({})));
    let (response, _) = run(schema(effect), ask(), keyed_host(reply(404, &[], "nope")));
    assert_eq!(state(&response), Some("done"));
    let payload = payload_of(&response, "SEARCHED");
    assert_eq!(payload.get_path("status").and_then(Value::as_number), Some(404.0));
    assert!(emitted(&response).iter().all(|(e, _)| *e != "SEARCH_FAILED"));
}

#[test]
fn a_timeout_fires_failure_with_kind_timeout() {
    let effect = call(ORIGIN, bearer(json!({ "timeoutMs": 50 })));
    let (response, _) = run(
        schema(effect),
        ask(),
        keyed_host(IoAnswer::Http(Err(TransportError::TimedOut))),
    );
    assert_eq!(state(&response), Some("failed"));
    assert_eq!(field_str(payload_of(&response, "SEARCH_FAILED"), "kind"), "timeout");
}

#[test]
fn a_transport_failure_fires_failure_with_kind_failed_and_its_message() {
    let effect = call(ORIGIN, bearer(json!({})));
    let (response, _) = run(
        schema(effect),
        ask(),
        keyed_host(IoAnswer::Http(Err(TransportError::Failed("connection reset".to_string())))),
    );
    let payload = payload_of(&response, "SEARCH_FAILED");
    assert_eq!(field_str(payload, "kind"), "failed");
    assert!(field_str(payload, "error").contains("connection reset"));
}

#[test]
fn an_origin_outside_egress_is_refused_before_any_ask() {
    let effect = call("https://evil.example.com", bearer(json!({})));
    let (response, seen) = run(schema(effect), ask(), keyed_host(reply(200, &[], "")));
    assert!(seen.is_empty(), "nothing reaches the host: {seen:?}");
    assert_eq!(state(&response), Some("failed"));
    assert_eq!(field_str(payload_of(&response, "SEARCH_FAILED"), "kind"), "refused");
}

#[test]
fn a_url_leaving_the_origin_through_the_path_is_refused_without_an_http_ask() {
    let effect = json!(["http", "GET", ORIGIN, "//evil.example.com/x", {}, options()]);
    let (response, seen) = run(schema(effect), ask(), keyed_host(reply(200, &[], "")));
    assert!(http_asks(&seen).is_empty(), "{seen:?}");
    assert_eq!(field_str(payload_of(&response, "SEARCH_FAILED"), "kind"), "refused");
}

#[test]
fn a_missing_credential_is_refused_without_an_http_ask() {
    let effect = call(ORIGIN, bearer(json!({})));
    let (response, seen) = run(schema(effect), ask(), |request| match request {
        IoRequest::Secret { .. } => IoAnswer::Secret(Ok(None)),
        other => panic!("the fixture asks for {other:?}"),
    });
    assert!(http_asks(&seen).is_empty(), "{seen:?}");
    let payload = payload_of(&response, "SEARCH_FAILED");
    assert_eq!(field_str(payload, "kind"), "refused");
    assert!(field_str(payload, "error").contains("api_key_name"));
}

#[test]
fn a_host_that_cannot_look_up_a_secret_fails_the_call() {
    let effect = call(ORIGIN, bearer(json!({})));
    let (response, seen) = run(schema(effect), ask(), |request| match request {
        IoRequest::Secret { .. } => IoAnswer::Secret(Err("vault down".to_string())),
        other => panic!("the fixture asks for {other:?}"),
    });
    assert!(http_asks(&seen).is_empty());
    assert_eq!(field_str(payload_of(&response, "SEARCH_FAILED"), "kind"), "failed");
}

#[test]
fn a_failure_with_no_failure_route_is_an_effect_failed_rejection() {
    let effect = json!(["http", "GET", "https://evil.example.com", "/x", {}, { "key": "job" }]);
    let (response, _) = run(schema(effect), ask(), keyed_host(reply(200, &[], "")));
    assert!(response
        .rejections
        .iter()
        .flatten()
        .any(|r| r.code == orbital_core::TransitionRejectionCode::EffectFailed));
}

#[test]
fn a_host_that_cancels_the_keyed_request_fires_the_cancelled_route_with_the_key() {
    let effect = call(ORIGIN, bearer(json!({})));
    let (response, _) = run(schema(effect), ask(), keyed_host(IoAnswer::ServiceCancelled));
    assert_eq!(state(&response), Some("stopped"));
    assert_eq!(field_str(payload_of(&response, "SEARCH_CANCELLED"), "key"), "job");
    assert!(emitted(&response).iter().all(|(e, _)| *e != "SEARCH_FAILED"));
}

#[test]
fn cancel_call_reaches_the_host_with_the_key_of_the_http() {
    let effect = call(ORIGIN, bearer(json!({})));
    let cancel = EventRequest {
        event: "CANCEL".to_string(),
        target_trait: Some("Fetcher".to_string()),
        ..Default::default()
    };
    let (_, seen) = run(schema(effect), cancel, |request| match request {
        IoRequest::CancelCall { key } if key == "job" => IoAnswer::Cancelled(Ok(())),
        other => panic!("the fixture asks for {other:?}"),
    });
    assert_eq!(seen, [IoRequest::CancelCall { key: "job".to_string() }]);
}

fn paged(max: u32) -> Json {
    call(
        ORIGIN,
        bearer(json!({ "query": { "limit": 1 }, "paginate": {
            "style": "cursor", "param": "after", "max": max,
            "next": ["object/get", "@response.headers", "x-next"],
            "collect": ["list", "@response.body"] } })),
    )
}

fn three_pages(request: &IoRequest) -> IoAnswer {
    match request {
        IoRequest::Secret { .. } => IoAnswer::Secret(Ok(Some("s3cret".to_string()))),
        IoRequest::Http { request, .. } if request.url.ends_with("after=p3") => reply(200, &[], "c"),
        IoRequest::Http { request, .. } if request.url.ends_with("after=p2") => {
            reply(200, &[("X-Next", "p3")], "b")
        }
        IoRequest::Http { .. } => reply(200, &[("X-Next", "p2")], "a"),
        other => panic!("the fixture asks for {other:?}"),
    }
}

#[test]
fn pagination_stops_at_max_pages_and_collects_what_it_walked() {
    let (response, seen) = run(schema(paged(2)), ask(), three_pages);
    let asks = http_asks(&seen);
    assert_eq!(asks.len(), 2, "{seen:?}");
    assert!(asks[1].url.contains("after=p2"), "{}", asks[1].url);
    let payload = payload_of(&response, "SEARCHED");
    assert_eq!(payload.get_path("pages").and_then(Value::as_number), Some(2.0));
    let items: Vec<&str> = payload
        .get_path("items")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(items, ["a", "b"]);
    assert_eq!(field_str(payload, "last.body"), "b");
}

#[test]
fn control_pagination_with_room_stops_when_next_is_null() {
    let (response, seen) = run(schema(paged(5)), ask(), three_pages);
    assert_eq!(http_asks(&seen).len(), 3, "{seen:?}");
    let payload = payload_of(&response, "SEARCHED");
    assert_eq!(payload.get_path("pages").and_then(Value::as_number), Some(3.0));
    assert_eq!(field_str(payload, "last.body"), "c");
}

#[test]
fn a_collect_that_is_not_a_list_fails_the_call() {
    let effect = call(
        ORIGIN,
        bearer(json!({ "paginate": {
            "style": "cursor", "param": "after", "max": 2,
            "next": ["object/get", "@response.headers", "x-next"],
            "collect": "@response.body" } })),
    );
    let (response, _) = run(schema(effect), ask(), keyed_host(reply(200, &[], "a")));
    assert_eq!(field_str(payload_of(&response, "SEARCH_FAILED"), "kind"), "failed");
}

fn oauth_effect() -> Json {
    call(
        ORIGIN,
        json!({ "auth": { "scheme": "oauth2-client", "tokenUrl": TOKEN_URL,
            "clientId": "cid_name", "clientSecret": "csec_name" } }),
    )
}

fn oauth_host(cached: Option<&'static str>, replies: Vec<IoAnswer>) -> impl FnMut(&IoRequest) -> IoAnswer {
    let mut replies = replies.into_iter();
    move |request| match request {
        IoRequest::Secret { name, .. } if name == CACHE => IoAnswer::Secret(Ok(cached.map(str::to_string))),
        IoRequest::Secret { name, .. } if name == "cid_name" => IoAnswer::Secret(Ok(Some("the-id".to_string()))),
        IoRequest::Secret { name, .. } if name == "csec_name" => IoAnswer::Secret(Ok(Some("the-secret".to_string()))),
        IoRequest::SecretSet { .. } => IoAnswer::SecretSet(Ok(())),
        IoRequest::Http { .. } => replies.next().expect("a scripted reply"),
        other => panic!("the fixture asks for {other:?}"),
    }
}

fn token_reply(token: &str) -> IoAnswer {
    reply(200, &[("Content-Type", "application/json")], &format!("{{\"access_token\":\"{token}\",\"expires_in\":3600}}"))
}

fn urls(seen: &[IoRequest]) -> Vec<&str> {
    http_asks(seen).iter().map(|r| r.url.as_str()).collect()
}

#[test]
fn an_oauth2_cache_hit_makes_the_request_without_a_grant() {
    let (response, seen) = run(
        schema(oauth_effect()),
        ask(),
        oauth_host(Some("cached"), vec![reply(200, &[], "ok")]),
    );
    assert_eq!(state(&response), Some("done"));
    assert_eq!(urls(&seen), ["https://api.example.com/search"], "{seen:?}");
    assert_eq!(header(http_asks(&seen)[0], "Authorization"), Some("Bearer cached"));
    assert!(seen.iter().all(|r| !matches!(r, IoRequest::SecretSet { .. })), "{seen:?}");
}

#[test]
fn an_oauth2_cache_miss_grants_keeps_the_token_and_uses_it() {
    let (response, seen) = run(
        schema(oauth_effect()),
        ask(),
        oauth_host(None, vec![token_reply("tok"), reply(200, &[], "ok")]),
    );
    assert_eq!(state(&response), Some("done"));
    assert_eq!(urls(&seen), [TOKEN_URL, "https://api.example.com/search"], "{seen:?}");
    let grant = http_asks(&seen)[0];
    assert_eq!(grant.method, "POST");
    assert!(grant.text().contains("grant_type=client_credentials"), "{}", grant.text());
    let stored = seen
        .iter()
        .find_map(|r| match r {
            IoRequest::SecretSet { name, value, ttl_ms, .. } => Some((name.as_str(), value.as_str(), *ttl_ms)),
            _ => None,
        })
        .expect("the token is kept");
    assert_eq!(stored, (CACHE, "tok", 3_600_000));
    assert_eq!(header(http_asks(&seen)[1], "Authorization"), Some("Bearer tok"));
}

#[test]
fn a_401_refreshes_the_token_once_and_retries() {
    let (response, seen) = run(
        schema(oauth_effect()),
        ask(),
        oauth_host(
            Some("stale"),
            vec![reply(401, &[], "expired"), token_reply("fresh"), reply(200, &[], "ok")],
        ),
    );
    assert_eq!(urls(&seen), ["https://api.example.com/search", TOKEN_URL, "https://api.example.com/search"], "{seen:?}");
    assert_eq!(header(http_asks(&seen)[2], "Authorization"), Some("Bearer fresh"));
    assert_eq!(payload_of(&response, "SEARCHED").get_path("status").and_then(Value::as_number), Some(200.0));
}

#[test]
fn a_second_401_is_the_reply_and_asks_nothing_more() {
    let (response, seen) = run(
        schema(oauth_effect()),
        ask(),
        oauth_host(
            Some("stale"),
            vec![reply(401, &[], "a"), token_reply("fresh"), reply(401, &[], "b")],
        ),
    );
    assert_eq!(http_asks(&seen).len(), 3, "{seen:?}");
    assert_eq!(state(&response), Some("done"));
    assert_eq!(payload_of(&response, "SEARCHED").get_path("status").and_then(Value::as_number), Some(401.0));
}

#[test]
fn an_oauth2_token_url_outside_egress_is_refused_before_the_grant() {
    let effect = call(
        ORIGIN,
        json!({ "auth": { "scheme": "oauth2-client", "tokenUrl": "https://evil.example.com/token",
            "clientId": "cid_name", "clientSecret": "csec_name" } }),
    );
    let (response, seen) = run(
        schema(effect),
        ask(),
        |request| match request {
            IoRequest::Secret { name, .. } if name.starts_with("oauth2:") => IoAnswer::Secret(Ok(None)),
            IoRequest::Secret { .. } => IoAnswer::Secret(Ok(Some("v".to_string()))),
            other => panic!("the fixture asks for {other:?}"),
        },
    );
    assert!(http_asks(&seen).is_empty(), "{seen:?}");
    assert_eq!(field_str(payload_of(&response, "SEARCH_FAILED"), "kind"), "refused");
}

#[test]
fn a_stream_asks_http_stream_with_its_framing_and_call_site() {
    let effect = call(ORIGIN, bearer(json!({ "stream": "sse" })));
    let (response, seen) = run(schema(effect), ask(), |request| match request {
        IoRequest::Secret { .. } => IoAnswer::Secret(Ok(Some("s3cret".to_string()))),
        IoRequest::HttpStream { .. } => reply(200, &[], ""),
        other => panic!("the fixture asks for {other:?}"),
    });
    let streamed = seen
        .iter()
        .find_map(|r| match r {
            IoRequest::HttpStream { key, framing, site, .. } => Some((key, framing, site)),
            _ => None,
        })
        .expect("a stream ask");
    assert_eq!(streamed.0.as_deref(), Some("job"));
    assert_eq!(*streamed.1, orbital_core::runtime::http_request::StreamFraming::Sse);
    let site = streamed.2.as_ref().expect("the call site");
    assert_eq!(site.trait_name, "Fetcher");
    assert_eq!(site.emit.on_message.as_deref(), Some("STEP"));
    assert_eq!(state(&response), Some("done"));
}
