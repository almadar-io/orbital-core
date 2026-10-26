// The `http` effect through the kernel (docs/Almadar_Embedded_Kernel.md, "The `http` effect"; EXEC-HTTP-1):
// credentials are asked by name, the request is built once and answered by the host, a reply of any
// status is a success, a refusal never reaches the host, and every ask is journaled so a replayed step
// repeats nothing.

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_base::ir::{OirEffect, OirExpr};
use orbital_core_base::http_wire::HttpMethod;
use orbital_core_runtime::http_service::{HttpRequest, HttpResponse, TransportError};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use orbital_core_base::schema::SExpression;
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

const ORIGIN: &str = "https://api.example.com";
const AUTH_ORIGIN: &str = "https://auth.example.com";
const TOKEN_URL: &str = "https://auth.example.com/token";

fn options() -> Json {
    json!({ "key": "job", "emit": {
        "success": "SEARCHED", "failure": "SEARCH_FAILED", "cancelled": "SEARCH_CANCELLED", "onMessage": "STEP"
    } })
}

#[test]
fn native_sigv4_signing_uses_the_same_stamp_as_authored_now_expressions() {
    let request = json!({"headers":{"X-Dispatch-Now":["str/concat","@now"]},"auth":{
        "scheme":"aws-sigv4","accessKeyId":"ID","secretAccessKey":"SECRET","region":"us-east-1","service":"s3"}});
    let (_, seen) = run(schema(call(ORIGIN,request)),ask(),|request| match request {
        IoRequest::Secret { .. } => IoAnswer::Secret(Ok(Some("dummy".into()))),
        IoRequest::Http { request, .. } => {
            let stamp = request.headers.iter().find(|(name,_)| name == "X-Dispatch-Now").unwrap().1.parse::<i64>().unwrap();
            let expected = orbital_core_runtime::sigv4::amz_date(stamp).1;
            assert!(request.headers.iter().any(|(name,value)| name == "x-amz-date" && value == &expected));
            IoAnswer::Http(Ok(HttpResponse {status:200,headers:vec![],body:b"ok".to_vec()}))
        }
        other => panic!("unexpected {other:?}"),
    });
    assert_eq!(seen.iter().filter(|request| matches!(request,IoRequest::Http { .. })).count(),1);
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

#[test]
fn a_http_parse_refusal_echoes_the_evaluated_key_without_asking_the_host() {
    let effect = call(ORIGIN, json!({"bogus":true}));
    let (response, seen) = run(schema(effect), ask(), |request| panic!("refusal sent {request:?}"));
    assert!(seen.is_empty());
    let payload = payload_of(&response, "SEARCH_FAILED");
    assert_eq!(payload.get_path("key").and_then(Value::as_str), Some("job"));
    assert_eq!(payload.get_path("kind").and_then(Value::as_str), Some("refused"));
}

#[test]
fn a_http_key_expression_is_evaluated_and_echoed() {
    for key in [json!(["str/concat", "request:", "@payload.q"]), json!("@payload.q"), json!("fixed")] {
        let expected = match &key {
            Json::Array(_) => "request:cats",
            Json::String(value) if value == "@payload.q" => "cats",
            _ => "fixed",
        };
        let mut opts = options();
        opts["key"] = key;
        let effect = json!(["http", "GET", ORIGIN, "/search", {}, opts]);
        let (response, seen) = run(schema(effect), ask(), keyed_host(reply(200, &[], "ok")));
        assert!(matches!(&seen[0], IoRequest::Http { key: Some(key), .. } if key == expected), "{seen:?}");
        assert_eq!(payload_of(&response, "SEARCHED").get_path("key").and_then(Value::as_str), Some(expected));
    }
}

fn unkeyed_effect() -> Json {
    json!(["http", "GET", ORIGIN, "/search", {}, { "emit": { "success": "SEARCHED", "failure": "SEARCH_FAILED" } }])
}

#[test]
fn an_unkeyed_http_reply_and_failure_carry_a_null_key() {
    let (response, _) = run(schema(unkeyed_effect()), ask(), |_| reply(200, &[], "ok"));
    let payload = payload_of(&response, "SEARCHED");
    assert_eq!(payload.get_path("key"), Some(&Value::Null));
    assert_eq!(payload.get_path("status"), Some(&Value::Number(200.0)));
    let (response, _) = run(schema(unkeyed_effect()), ask(), |_| IoAnswer::Http(Err(TransportError::TimedOut)));
    let payload = payload_of(&response, "SEARCH_FAILED");
    assert_eq!(payload.get_path("key"), Some(&Value::Null));
    assert_eq!(payload.get_path("kind").and_then(Value::as_str), Some("timeout"));
}

#[test]
fn control_a_keyed_http_reply_and_failure_echo_the_key() {
    let (response, _) = run(schema(call(ORIGIN, json!({}))), ask(), keyed_host(reply(200, &[], "ok")));
    assert_eq!(payload_of(&response, "SEARCHED").get_path("key").and_then(Value::as_str), Some("job"));
    let (response, _) = run(
        schema(call(ORIGIN, json!({}))),
        ask(),
        keyed_host(IoAnswer::Http(Err(TransportError::TimedOut))),
    );
    assert_eq!(payload_of(&response, "SEARCH_FAILED").get_path("key").and_then(Value::as_str), Some("job"));
}

#[test]
fn a_http_key_that_evaluates_to_a_non_string_never_sends() {
    for key in [json!(["+", 1, 2]), json!(17), json!(null), json!("")] {
        let mut opts = options();
        opts["key"] = key;
        let effect = json!(["http", "GET", ORIGIN, "/search", {}, opts]);
        let (_, seen) = run(schema(effect), ask(), |request| panic!("invalid key sent {request:?}"));
        assert!(seen.is_empty());
    }
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
    let OirExpr::Effect { effect } = orbital_core_eval::lower::lower_sexpr(&form).unwrap() else {
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
    let OirExpr::Effect { effect } = orbital_core_eval::lower::lower_sexpr(&form).unwrap() else {
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
        assert!(orbital_core_eval::lower::lower_sexpr(&form).is_err(), "{form:?}");
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
fn a_missing_credential_is_unconfigured_without_an_http_ask() {
    let effect = call(ORIGIN, bearer(json!({})));
    let (response, seen) = run(schema(effect), ask(), |request| match request {
        IoRequest::Secret { .. } => IoAnswer::Secret(Ok(None)),
        other => panic!("the fixture asks for {other:?}"),
    });
    assert!(http_asks(&seen).is_empty(), "{seen:?}");
    let payload = payload_of(&response, "SEARCH_FAILED");
    assert_eq!(field_str(payload, "kind"), "unconfigured");
    assert_eq!(field_str(payload, "credential"), "api_key_name");
    assert_eq!(field_str(payload, "key"), "job");
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
        .any(|r| r.code == orbital_core_runtime::effects::response::TransitionRejectionCode::EffectFailed));
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

fn oauth_refreshing_effect() -> Json {
    call(
        ORIGIN,
        json!({ "auth": { "scheme": "oauth2-client", "tokenUrl": TOKEN_URL, "onUnauthorized": "refresh",
            "clientId": "cid_name", "clientSecret": "csec_name" } }),
    )
}

fn oauth_cache_name() -> String {
    let effect = oauth_effect();
    let request = serde_json::from_value(effect[4].clone()).unwrap();
    let spec = orbital_core_runtime::http_request::parse_http_call(HttpMethod::Get,ORIGIN,"/search",&request).unwrap();
    orbital_core_runtime::http_request::token_cache_name(spec.auth.as_ref().unwrap()).unwrap()
}

fn oauth_host(cached: Option<&'static str>, replies: Vec<IoAnswer>) -> impl FnMut(&IoRequest) -> IoAnswer {
    let mut replies = replies.into_iter();
    move |request| match request {
        IoRequest::Secret { name, .. } if name == &oauth_cache_name() => IoAnswer::Secret(Ok(cached.map(str::to_string))),
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
    assert_eq!(stored, (oauth_cache_name().as_str(), "tok", 3_600_000));
    assert_eq!(header(http_asks(&seen)[1], "Authorization"), Some("Bearer tok"));
}

#[test]
fn a_401_refreshes_the_token_once_and_retries() {
    let (response, seen) = run(
        schema(oauth_refreshing_effect()),
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
fn by_default_a_401_is_the_reply_with_no_second_grant() {
    let (response, seen) = run(
        schema(oauth_effect()),
        ask(),
        oauth_host(Some("stale"), vec![reply(401, &[], "expired")]),
    );
    assert_eq!(urls(&seen), ["https://api.example.com/search"], "{seen:?}");
    assert_eq!(payload_of(&response, "SEARCHED").get_path("status").and_then(Value::as_number), Some(401.0));
}

#[test]
fn a_second_401_is_the_reply_and_asks_nothing_more() {
    let (response, seen) = run(
        schema(oauth_refreshing_effect()),
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
    assert_eq!(*streamed.1, orbital_core_runtime::http_request::StreamFraming::Sse);
    let site = streamed.2.as_ref().expect("the call site");
    assert_eq!(site.trait_name, "Fetcher");
    assert_eq!(site.emit.on_message.as_deref(), Some("STEP"));
    assert_eq!(state(&response), Some("done"));
}

fn http_delays(request: Json) -> Vec<Option<u64>> {
    let (_, seen) = run(schema(call(ORIGIN, bearer(request))), ask(), |request| match request {
        IoRequest::Secret { .. } => IoAnswer::Secret(Ok(Some("s3cret".to_string()))),
        IoRequest::Http { .. } => reply(200, &[], "ok"),
        other => panic!("unexpected {other:?}"),
    });
    seen.iter()
        .filter_map(|request| match request {
            IoRequest::Http { delay_ms, .. } => Some(*delay_ms),
            _ => None,
        })
        .collect()
}

#[test]
fn delay_ms_reaches_the_host_on_the_http_ask_and_a_request_without_it_asks_for_no_wait() {
    assert_eq!(http_delays(json!({ "delayMs": 300 })), vec![Some(300)]);
    // Controls: no option and an explicit null ask for no wait.
    assert_eq!(http_delays(json!({})), vec![None]);
    assert_eq!(http_delays(json!({ "delayMs": null })), vec![None]);
}

#[test]
fn a_keyed_request_cancelled_while_it_waits_out_its_delay_fires_the_cancelled_route() {
    let effect = call(ORIGIN, bearer(json!({ "delayMs": 5000 })));
    let (response, seen) = run(schema(effect.clone()), ask(), |request| match request {
        IoRequest::Secret { .. } => IoAnswer::Secret(Ok(Some("s3cret".to_string()))),
        IoRequest::Http { delay_ms, .. } => {
            assert_eq!(*delay_ms, Some(5000));
            IoAnswer::ServiceCancelled
        }
        other => panic!("unexpected {other:?}"),
    });
    assert_eq!(state(&response), Some("stopped"));
    assert_eq!(field_str(payload_of(&response, "SEARCH_CANCELLED"), "key"), "job");
    assert!(emitted(&response).iter().all(|(e, _)| *e != "SEARCHED" && *e != "SEARCH_FAILED"));
    assert!(seen.iter().any(|r| matches!(r, IoRequest::Http { delay_ms: Some(5000), .. })));
    // Control: the same delayed request that is not cancelled completes with its reply.
    let (response, _) = run(schema(effect), ask(), |request| match request {
        IoRequest::Secret { .. } => IoAnswer::Secret(Ok(Some("s3cret".to_string()))),
        IoRequest::Http { .. } => reply(200, &[], "ok"),
        other => panic!("unexpected {other:?}"),
    });
    assert_eq!(state(&response), Some("done"));
    assert!(emitted(&response).iter().all(|(e, _)| *e != "SEARCH_CANCELLED"));
}

fn retry_backoff_delays(backoff_ms: u64, max: u32) -> Vec<Option<u64>> {
    let effect = call(
        ORIGIN,
        json!({ "retry": { "max": max, "on": ["5xx"], "backoffMs": backoff_ms } }),
    );
    let (response, seen) = run(schema(effect), ask(), |request| match request {
        IoRequest::Http { .. } => reply(503, &[], "unavailable"),
        other => panic!("unexpected {other:?}"),
    });
    assert_eq!(state(&response), Some("done"));
    assert_eq!(
        payload_of(&response, "SEARCHED")
            .get_path("status")
            .and_then(Value::as_number),
        Some(503.0)
    );
    let delays: Vec<Option<u64>> = seen
        .iter()
        .filter_map(|request| match request {
            IoRequest::Http { delay_ms, .. } => Some(*delay_ms),
            _ => None,
        })
        .collect();
    assert_eq!(delays.len(), max as usize + 1);
    assert_eq!(delays[0], None);
    delays
}

#[test]
fn retry_backoff_saturates_after_the_u64_shift_boundary() {
    let delays = retry_backoff_delays(1, 66);
    assert_eq!(delays[1], Some(1));
    assert_eq!(delays[63], Some(1u64 << 62));
    assert_eq!(delays[64], Some(1u64 << 63));
    assert_eq!(delays[65], Some(u64::MAX));
    assert_eq!(delays[66], Some(u64::MAX));
}

#[test]
fn retry_backoff_zero_stays_zero_after_the_u64_shift_boundary() {
    let delays = retry_backoff_delays(0, 66);
    assert!(delays.iter().skip(1).all(|delay| *delay == Some(0)));
}

#[test]
fn retry_backoff_large_base_saturates_before_the_u64_shift_boundary() {
    assert_eq!(
        retry_backoff_delays(1u64 << 62, 4),
        [
            None,
            Some(1u64 << 62),
            Some(1u64 << 63),
            Some(u64::MAX),
            Some(u64::MAX),
        ]
    );
}

mod browser_effects {
    // `browser/page-facts` and `browser/beacon` through the kernel (docs/Almadar_Embedded_Kernel.md, G-CROSS-221):
    // page facts are one journaled host ask answered flat with the unrequested facts at their defaults; a beacon is
    // built and egress-checked by the one http driver and only then handed to the host, which answers how it took it.

    use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
    use orbital_core_base::ir::PageFactField;
    use orbital_core_runtime::http_request::BeaconOutcome;
    use orbital_core_runtime::http_service::TransportError;
    use orbital_core_runtime::resumable::{answer_http, IoAnswer, IoReply, IoRequest, ModelFailure, ModelFailureKind, PageFacts};
    use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
    use serde_json::{json, Value as Json};

    const COLLECTOR: &str = "https://collector.example.com";

    fn schema(effect: Json) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "egress": [COLLECTOR],
            "orbitals": [{
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
        PageFacts { available: true, hostname: hostname.into(), referrer: referrer.into(), screen_width: width }
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
            json!({ "available": true, "hostname": "", "referrer": "https://r.example/", "screenWidth": 1280.0 })
        );
    }

    #[test]
    fn page_facts_keep_unrequested_facts_at_their_defaults_whatever_the_host_answers() {
        let (response, _) = run(json!(["browser/page-facts", ["hostname"], routes()]), |_| {
            IoAnswer::PageFacts(Ok(facts("site.example", "https://leak.example/", Some(99.0))))
        });
        assert_eq!(
            payload_of(&response, "OK"),
            json!({ "available": true, "hostname": "site.example", "referrer": "", "screenWidth": null })
        );
    }

    #[test]
    fn page_facts_with_no_page_are_available_false_with_the_defaults() {
        let (response, _) = run(json!(["browser/page-facts", ["hostname", "referrer", "screenWidth"], routes()]), |_| {
            IoAnswer::PageFacts(Ok(PageFacts { available: false, hostname: "x".into(), referrer: "y".into(), screen_width: Some(5.0) }))
        });
        assert_eq!(
            payload_of(&response, "OK"),
            json!({ "available": false, "hostname": "", "referrer": "", "screenWidth": null })
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

    fn beacon(url: &str) -> Json {
        json!(["browser/beacon", url, { "text": "@payload.body", "key": "k1" }, routes()])
    }

    #[test]
    fn a_queued_beacon_is_sent_via_queue_with_the_built_text_plain_request() {
        let (response, seen) = run(beacon("https://collector.example.com/api/event?v=1"), |request| match request {
            IoRequest::HttpBeacon { .. } => IoAnswer::HttpBeacon(Ok(BeaconOutcome::Queued)),
            other => panic!("the fixture asks for {other:?}"),
        });
        let [IoRequest::HttpBeacon { request, key, .. }] = seen.as_slice() else { panic!("one beacon ask expected: {seen:?}") };
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "https://collector.example.com/api/event?v=1");
        assert_eq!(request.body, b"hello".to_vec());
        assert!(request.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "text/plain"));
        assert_eq!(key.as_deref(), Some("k1"));
        assert_eq!(payload_of(&response, "OK"), json!({ "sent": true, "via": "queue", "status": null, "key": "k1" }));
    }

    #[test]
    fn a_fetched_beacon_reports_the_status_only_and_2xx_is_sent() {
        for (status, sent) in [(200u16, true), (202, true), (204, true), (299, true), (300, false), (400, false), (500, false)] {
            let (response, _) = run(beacon("https://collector.example.com/e"), |_| {
                IoAnswer::HttpBeacon(Ok(BeaconOutcome::Fetched { status }))
            });
            assert_eq!(
                payload_of(&response, "OK"),
                json!({ "sent": sent, "via": "fetch", "status": f64::from(status), "key": "k1" }),
                "status {status}"
            );
        }
    }

    #[test]
    fn a_beacon_whose_fetch_throws_is_sent_false_via_fetch_not_a_failure() {
        let (response, _) = run(beacon("https://collector.example.com/e"), |_| {
            IoAnswer::HttpBeacon(Err(TransportError::Failed("network down".into())))
        });
        assert_eq!(payload_of(&response, "OK"), json!({ "sent": false, "via": "fetch", "status": null, "key": "k1" }));
    }

    #[test]
    fn an_undeclared_origin_is_refused_before_the_host_is_asked() {
        let (response, seen) = run(beacon("https://elsewhere.example.com/e"), |request| panic!("egress denial asked {request:?}"));
        assert!(seen.is_empty());
        let payload = payload_of(&response, "BAD");
        assert_eq!(payload.get("kind"), Some(&json!("refused")));
        assert_eq!(payload.get("key"), Some(&json!("k1")));
    }

    #[test]
    fn a_relative_or_non_http_url_is_a_typed_refusal_never_a_send() {
        for url in ["/api/event", "api/event", "ftp://collector.example.com/e", "//collector.example.com/e", "https://collector.example.com/e#frag"] {
            let (response, seen) = run(beacon(url), |request| panic!("{url} asked {request:?}"));
            assert!(seen.is_empty(), "{url}");
            assert_eq!(payload_of(&response, "BAD").get("kind"), Some(&json!("refused")), "{url}");
        }
    }

    #[test]
    fn a_beacon_request_carries_text_and_nothing_else() {
        for request in [json!({}), json!({ "text": 5 }), json!({ "text": "x", "json": {} }), json!({ "text": "x", "auth": {} })] {
            let (response, seen) = run(
                json!(["browser/beacon", "https://collector.example.com/e", request, routes()]),
                |r| panic!("invalid beacon asked {r:?}"),
            );
            assert!(seen.is_empty());
            assert_eq!(payload_of(&response, "BAD").get("kind"), Some(&json!("refused")));
        }
    }

    #[test]
    fn a_beacon_key_is_optional_and_echoes_null_when_absent() {
        let (response, seen) = run(
            json!(["browser/beacon", "https://collector.example.com/e", { "text": "x" }, routes()]),
            |_| IoAnswer::HttpBeacon(Ok(BeaconOutcome::Queued)),
        );
        assert!(matches!(seen.as_slice(), [IoRequest::HttpBeacon { key: None, .. }]));
        assert_eq!(payload_of(&response, "OK"), json!({ "sent": true, "via": "queue", "status": null, "key": null }));
    }

    #[test]
    fn a_cancelled_beacon_fires_the_cancelled_route_and_not_success() {
        let (response, _) = run(beacon("https://collector.example.com/e"), |_| IoAnswer::ServiceCancelled);
        assert_eq!(payload_of(&response, "STOPPED"), json!({ "key": "k1" }));
        assert!(!emitted(&response).iter().any(|(e, _)| *e == "OK"));
    }


    #[test]
    fn page_facts_key_is_evaluated_and_echoed_on_success_and_failure_and_absent_means_no_key_field() {
        let keyed = |key: Json| json!(["browser/page-facts", ["hostname"], { "key": key, "emit": { "success": "OK", "failure": "BAD" } }]);
        let (response, _) = run(keyed(json!(["str/concat", "k:", "@payload.body"])), |_| {
            IoAnswer::PageFacts(Ok(facts("site.example", "", None)))
        });
        assert_eq!(
            payload_of(&response, "OK"),
            json!({ "available": true, "hostname": "site.example", "referrer": "", "screenWidth": null, "key": "k:hello" })
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
        assert_eq!(payload_of(&response, "OK"), json!({ "reply": "{\"category\":\"a\"}", "key": null }));
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
            json!({ "error": "The on-device model is downloading", "kind": "unavailable", "key": null })
        );
        let (response, _) = run(prompt(None), |_| IoAnswer::Prompt(Err(failure(ModelFailureKind::Failed, "quota exceeded"))));
        assert_eq!(payload_of(&response, "BAD"), json!({ "error": "quota exceeded", "kind": "failed", "key": null }));
        assert!(!emitted(&response).iter().any(|(e, _)| *e == "OK"));
    }

    #[test]
    fn a_blocking_host_with_no_model_or_embedder_answers_unconfigured() {
        let asked = IoRequest::Prompt { text: "t".into(), schema: SCHEMA.into(), key: None };
        assert_eq!(
            answer_http(None, &asked),
            IoAnswer::Prompt(Err(failure(ModelFailureKind::Unconfigured, "browser/prompt: no on-device model is wired into this host")))
        );
        let asked = IoRequest::Embed { texts: vec!["t".into()], key: None };
        assert_eq!(
            answer_http(None, &asked),
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
                json!({ "error": "browser/prompt: the text and the response schema must be text", "kind": "failed", "key": null })
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
        assert_eq!(payload_of(&response, "OK"), json!({ "embeddings": [], "key": null }));
    }

    #[test]
    fn embed_hands_back_whatever_vectors_the_host_answers_and_leaves_the_count_check_to_the_program() {
        let (response, _) = run(embed(json!(["a", "b"]), None), |_| IoAnswer::Embed(Ok(vec![vec![1.0]])));
        assert_eq!(payload_of(&response, "OK"), json!({ "embeddings": [[1.0]], "key": null }));
    }

    #[test]
    fn embed_texts_that_are_not_a_list_of_text_fail_without_asking_the_host() {
        for texts in [json!("a"), json!(["a", 2])] {
            let (response, seen) = run(embed(texts, None), |request| panic!("a refusal asked {request:?}"));
            assert!(seen.is_empty());
            assert_eq!(
                payload_of(&response, "BAD"),
                json!({ "error": "browser/embed: the texts must be a list of text", "kind": "failed", "key": null })
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
            json!({ "error": "browser/embed: no on-device embedder is wired into this host", "kind": "unconfigured", "key": null })
        );
    }
}

mod tab_effects {
    // `browser/tab-*` through the kernel: the request is validated and every result shaped by the kernel, the host is
    // asked only for raw tab and page operations (docs/Almadar_Embedded_Kernel.md D-A92).

    use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
    use orbital_core_base::tab::{TabAsk, TabCommand, TabFailure, TabFailureKind, TabImageFormat};
    use orbital_core_runtime::resumable::{answer_http, IoAnswer, IoReply, IoRequest};
    use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
    use serde_json::{json, Value as Json};

    fn schema(effect: Json) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Probe", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }, { "name": "failed" }, { "name": "stopped" }],
                        "events": [{ "key": "ITEM_SEEN", "name": "Item seen", "input": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "GO", "effects": [effect] },
                            { "from": "idle", "to": "idle", "event": "CANCEL", "effects": [["cancel-call", "job"]] },
                            { "from": "idle", "to": "idle", "event": "ITEM_SEEN" },
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
        EventRequest { event: "GO".to_string(), target_trait: Some("Probe".to_string()), ..Default::default() }
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

    fn keyed() -> Json {
        json!({ "key": "job", "emit": { "success": "OK", "failure": "BAD", "cancelled": "STOPPED" } })
    }

    fn tab(ask: &IoRequest) -> &TabAsk {
        match ask {
            IoRequest::Tab { ask, .. } => ask,
            other => panic!("the fixture asks for {other:?}"),
        }
    }

    fn reply(value: Json) -> IoAnswer {
        IoAnswer::Tab(Ok(serde_json::from_value(value).unwrap()))
    }

    #[test]
    fn tab_list_asks_the_host_once_and_emits_the_tabs_the_pattern_keeps_with_the_key() {
        let tabs = json!([
            { "tab": 1, "url": "https://example.com/a", "title": "A", "active": true },
            { "tab": 2, "url": "https://other.test/", "title": "B", "active": false }
        ]);
        let (response, seen) = run(json!(["browser/tab-list", { "match": "https://example.com/*" }, keyed()]), |_| reply(tabs.clone()));
        assert_eq!(seen, vec![IoRequest::Tab { ask: TabAsk::List, key: Some("job".into()) }]);
        assert_eq!(
            payload_of(&response, "OK"),
            json!({ "tabs": [{ "tab": 1.0, "url": "https://example.com/a", "title": "A", "active": true }], "key": "job" })
        );
    }

    #[test]
    fn tab_open_builds_the_url_from_the_params_and_emits_the_tab() {
        let (response, seen) = run(
            json!(["browser/tab-open", { "url": "https://example.com/results", "params": { "q": "a b" }, "active": false }, routes()]),
            |_| reply(json!(99)),
        );
        assert_eq!(tab(&seen[0]), &TabAsk::Open { url: "https://example.com/results?q=a+b".into(), active: false });
        assert_eq!(payload_of(&response, "OK"), json!({ "tab": 99.0, "key": null }));
    }

    #[test]
    fn tab_capture_asks_for_the_tab_in_its_format_and_emits_the_image_without_its_data_url_prefix() {
        let (response, seen) = run(
            json!(["browser/tab-capture", { "tab": 2, "format": "jpeg", "quality": 70 }, routes()]),
            |_| reply(json!("data:image/jpeg;base64,/9j/4AAQ")),
        );
        assert_eq!(tab(&seen[0]), &TabAsk::Capture { tab: 2, format: TabImageFormat::Jpeg, quality: Some(70) });
        assert_eq!(payload_of(&response, "OK"), json!({ "tab": 2.0, "format": "jpeg", "image": "/9j/4AAQ", "key": null }));
    }

    #[test]
    fn a_request_the_kernel_refuses_fails_invalid_without_asking_the_host() {
        for request in [json!({ "url": "https://a.test/", "match": "https://b.test/*" }), json!({ "url": "/relative" }), json!({ "nope": 1 }), json!("text")] {
            let (response, seen) = run(json!(["browser/tab-open", request, keyed()]), |ask| panic!("a refusal asked {ask:?}"));
            assert!(seen.is_empty());
            let failure = payload_of(&response, "BAD");
            assert_eq!(failure["kind"], "invalid", "{failure}");
            assert_eq!(failure["key"], "job");
        }
    }

    #[test]
    fn control_a_bad_key_never_asks_the_host() {
        for key in [json!(17), json!(""), json!(["+", 1, 2])] {
            let (_, seen) = run(
                json!(["browser/tab-close", { "tab": 1 }, { "key": key, "emit": routes()["emit"].clone() }]),
                |ask| panic!("an invalid key asked {ask:?}"),
            );
            assert!(seen.is_empty());
        }
    }

    #[test]
    fn a_host_failure_emits_its_kind_and_the_key() {
        let (response, _) = run(json!(["browser/tab-close", { "tab": 4 }, keyed()]), |_| {
            IoAnswer::Tab(Err(TabFailure::new(TabFailureKind::Failed, "No tab with id: 4")))
        });
        assert_eq!(payload_of(&response, "BAD"), json!({ "error": "No tab with id: 4", "kind": "failed", "key": "job" }));
        assert!(!emitted(&response).iter().any(|(e, _)| *e == "OK"));
    }

    #[test]
    fn a_blocking_host_with_no_browser_tabs_answers_unavailable() {
        let asked = IoRequest::Tab { ask: TabAsk::List, key: None };
        assert_eq!(
            answer_http(None, &asked),
            IoAnswer::Tab(Err(TabFailure::new(TabFailureKind::Unavailable, "browser/tab: no host answers browser tabs")))
        );
    }

    #[test]
    fn query_waits_for_a_page_that_has_not_loaded_then_reads_for_the_rest_of_the_wait() {
        let mut step = 0;
        let (response, seen) = run(
            json!(["browser/tab-query", { "tab": 4, "selector": ".item", "fields": {}, "waitMs": 1000 }, routes()]),
            |ask| {
                step += 1;
                match (step, tab(ask)) {
                    (1, TabAsk::Command { tab: 4, command: TabCommand::Query { .. } }) => {
                        IoAnswer::Tab(Err(TabFailure::new(TabFailureKind::NotReady, "Tab 4 has no page script")))
                    }
                    (2, TabAsk::AwaitReady { tab: 4, timeout_ms: 1000 }) => reply(json!({ "ready": true, "waitedMs": 400 })),
                    (3, TabAsk::Command { tab: 4, command: TabCommand::Query { wait_ms: Some(600), .. } }) => {
                        reply(json!([{ "elementId": "el-1", "fields": { "title": "x" } }]))
                    }
                    (n, other) => panic!("step {n} asked {other:?}"),
                }
            },
        );
        assert_eq!(seen.len(), 3);
        assert_eq!(
            payload_of(&response, "OK"),
            json!({ "tab": 4.0, "elements": [{ "elementId": "el-1", "fields": { "title": "x" } }], "key": null })
        );
    }

    #[test]
    fn watch_resolves_its_event_address_against_the_declared_inputs_and_asks_the_host_to_deliver_there() {
        let watch = json!(["browser/tab-watch", { "match": "https://example.com/*", "selector": ".item", "fields": { "title": { "selector": "h2" } }, "event": "ITEM_SEEN" }, routes()]);
        let (response, seen) = run(watch, |_| IoAnswer::Tab(Ok(Value::Null)));
        let TabAsk::Watch { watch_id, deliver, match_pattern, .. } = tab(&seen[0]) else { panic!("expected a watch ask") };
        assert_eq!((deliver.orbital.as_str(), deliver.trait_name.as_str(), deliver.event.as_str()), ("Main", "Probe", "ITEM_SEEN"));
        assert_eq!(match_pattern, "https://example.com/*");
        assert_eq!(payload_of(&response, "OK"), json!({ "watchId": watch_id, "key": null }));
    }

    #[test]
    fn control_a_watch_naming_no_declared_input_fails_invalid_before_asking() {
        let watch = json!(["browser/tab-watch", { "match": "https://example.com/*", "selector": ".item", "fields": {}, "event": "GO" }, routes()]);
        let (response, seen) = run(watch, |ask| panic!("a refusal asked {ask:?}"));
        assert!(seen.is_empty());
        assert_eq!(payload_of(&response, "BAD")["kind"], "invalid");
    }

    #[test]
    fn a_cancelled_tab_ask_fires_the_cancelled_route_with_the_key_and_an_unkeyed_one_fires_nothing() {
        let (response, _) = run(json!(["browser/tab-close", { "tab": 1 }, keyed()]), |_| IoAnswer::ServiceCancelled);
        assert_eq!(payload_of(&response, "STOPPED"), json!({ "key": "job" }));
        assert!(!emitted(&response).iter().any(|(e, _)| matches!(*e, "OK" | "BAD")));
        let (response, _) = run(json!(["browser/tab-close", { "tab": 1 }, routes()]), |_| IoAnswer::ServiceCancelled);
        assert!(emitted(&response).iter().all(|(e, _)| !matches!(*e, "OK" | "BAD" | "STOPPED")));
    }

    #[test]
    fn cancel_call_reaches_the_host_with_the_key_of_the_tab_ask() {
        let cancel = EventRequest { event: "CANCEL".to_string(), target_trait: Some("Probe".to_string()), ..Default::default() };
        let (_, seen) = run_with(json!(["browser/tab-close", { "tab": 1 }, keyed()]), cancel, |ask| match ask {
            IoRequest::CancelCall { key } if key == "job" => IoAnswer::Cancelled(Ok(())),
            other => panic!("the fixture asks for {other:?}"),
        });
        assert_eq!(seen, vec![IoRequest::CancelCall { key: "job".into() }]);
    }

    #[test]
    fn apply_hands_the_host_the_styles_of_the_treatment_not_its_name() {
        let (response, seen) = run(
            json!(["browser/tab-apply", { "tab": 3, "elementId": "el-2", "treatment": "blur", "label": "later" }, routes()]),
            |_| reply(json!(true)),
        );
        let TabAsk::Command { tab: 3, command: TabCommand::Apply(apply) } = tab(&seen[0]) else { panic!("expected an apply command") };
        assert_eq!(apply.styles.len(), 1);
        assert_eq!((apply.styles[0].property.as_str(), apply.styles[0].value.as_str()), ("filter", "blur(8px)"));
        assert_eq!(apply.label.as_deref(), Some("later"));
        assert_eq!(payload_of(&response, "OK"), json!({ "applied": true, "tab": 3.0, "elementId": "el-2", "treatment": "blur", "key": null }));
    }
}

mod result_target {
    use super::*;

    fn http_results(response: &EventResponse) -> Vec<Json> {
        response
            .effect_results
            .iter()
            .flatten()
            .map(|r| serde_json::to_value(r).unwrap())
            .filter(|r| r["effect"] == "http")
            .collect()
    }

    fn fails(request: &IoRequest) -> IoAnswer {
        match request {
            IoRequest::Http { .. } => IoAnswer::Http(Err(TransportError::Failed("verify has no network".into()))),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_routed_failure_carries_its_target_and_the_event_it_was_routed_to() {
        let (response, _) = run(schema(call(ORIGIN, json!({}))), ask(), fails);
        assert_eq!(
            http_results(&response),
            vec![json!({
                "effect": "http", "success": false, "target": "GET https://api.example.com/search",
                "error": "verify has no network", "routed": "SEARCH_FAILED"
            })]
        );
    }

    #[test]
    fn an_unrouted_failure_has_a_target_and_no_routed() {
        let effect = json!(["http", "GET", ORIGIN, "/search", {}]);
        let (response, _) = run(schema(effect), ask(), fails);
        let results = http_results(&response);
        assert_eq!(results.len(), 1, "{results:?}");
        assert_eq!(results[0]["target"], "GET https://api.example.com/search");
        assert!(results[0].get("routed").is_none(), "{results:?}");
        assert_eq!(results[0]["success"], false);
    }

    #[test]
    fn a_success_has_a_target_and_no_routed() {
        let (response, _) = run(schema(call(ORIGIN, json!({}))), ask(), |request| match request {
            IoRequest::Http { .. } => IoAnswer::Http(Ok(HttpResponse { status: 200, headers: vec![], body: b"ok".to_vec() })),
            other => panic!("unexpected {other:?}"),
        });
        let results = http_results(&response);
        assert_eq!(results.len(), 1, "{results:?}");
        assert_eq!(results[0]["target"], "GET https://api.example.com/search");
        assert_eq!(results[0]["success"], true);
        assert!(results[0].get("routed").is_none(), "{results:?}");
    }
}

mod repeated_deliveries {
    use super::*;

    fn looping(ask_effects: Json, arm_event: &str) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "egress": [ORIGIN],
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Fetcher", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "asking" }],
                        "transitions": [
                            { "from": "idle", "to": "asking", "event": "ASK", "effects": ask_effects },
                            { "from": "asking", "to": "asking", "event": arm_event, "effects": [["emit", "SAW", {}]] }
                        ]
                    } }]
            }]
        }))
        .unwrap()
    }

    fn saw(response: &EventResponse) -> usize {
        response.emitted_events.iter().filter(|e| e.event == "SAW").count()
    }

    fn two_requests() -> Json {
        let emit = json!({ "emit": { "success": "GOT", "failure": "BAD" } });
        json!([
            ["http", "GET", ORIGIN, "/a", {}, emit],
            ["http", "GET", ORIGIN, "/b", {}, emit]
        ])
    }

    #[test]
    fn two_identical_http_replies_to_two_requests_are_both_delivered() {
        let (response, seen) = run(looping(two_requests(), "GOT"), ask(), |request| match request {
            IoRequest::Http { .. } => reply(200, &[], "same"),
            other => panic!("unexpected {other:?}"),
        });
        assert_eq!(seen.iter().filter(|r| matches!(r, IoRequest::Http { .. })).count(), 2);
        assert_eq!(saw(&response), 2, "{:?}", emitted(&response));
    }

    #[test]
    fn two_identical_http_failures_are_both_delivered() {
        let (response, _) = run(looping(two_requests(), "BAD"), ask(), |request| match request {
            IoRequest::Http { .. } => IoAnswer::Http(Err(TransportError::Failed("down".into()))),
            other => panic!("unexpected {other:?}"),
        });
        assert_eq!(saw(&response), 2, "{:?}", emitted(&response));
    }

    #[test]
    fn control_two_identical_plain_emits_still_collapse_in_one_dispatch() {
        let effects = json!([["emit", "PING", {}], ["emit", "PING", {}]]);
        let (response, _) = run(looping(effects, "PING"), ask(), |request| panic!("unexpected {request:?}"));
        assert_eq!(saw(&response), 1, "{:?}", emitted(&response));
    }
}
