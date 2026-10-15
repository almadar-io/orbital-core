use std::net::IpAddr;
use std::time::Duration;

use orbital_core::evaluator::Value;
use orbital_core::runtime::http_request::*;
use orbital_core::runtime::http_service::{HttpRequest, HttpResponse, TransportError};
use serde_json::json;

fn val(j: serde_json::Value) -> Value {
    serde_json::from_value(j).unwrap()
}

fn parse(method: HttpMethod, path: &str, request: serde_json::Value) -> Result<HttpCallSpec, HttpRefusal> {
    parse_http_call(method, "https://api.example.com", path, &val(request))
}

fn spec(request: serde_json::Value) -> HttpCallSpec {
    parse(HttpMethod::Post, "/v1/x", request).unwrap()
}

fn egress() -> Vec<String> {
    vec!["https://api.example.com".to_string()]
}

fn no_secrets(_: &str) -> Option<String> {
    None
}

fn vault(name: &str) -> Option<String> {
    match name {
        "TOKEN" => Some("tok".to_string()),
        "USER" => Some("alice".to_string()),
        "PASS" => Some("s3cret".to_string()),
        "CID" => Some("client-1".to_string()),
        "CSEC" => Some("shh".to_string()),
        "Jefe" => Some("Jefe".to_string()),
        _ => None,
    }
}

fn build(spec: &HttpCallSpec) -> Result<HttpRequest, HttpRefusal> {
    build_request(spec, &vault, &egress(), None)
}

fn header<'a>(r: &'a HttpRequest, name: &str) -> Option<&'a str> {
    r.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn response(status: u16, headers: &[(&str, &str)], body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        body: body.as_bytes().to_vec(),
    }
}

fn get(v: &Value, path: &str) -> Value {
    v.get_path(path).cloned().unwrap_or(Value::Null)
}

#[test]
fn query_is_encoded_ordered_lists_repeat_and_null_is_omitted() {
    let s = spec(json!({"query": {"q": "a b&c", "limit": 1, "tag": ["x", "y"], "skip": null, "on": true}}));
    assert_eq!(
        s.query,
        vec![
            ("q".into(), "a b&c".into()),
            ("limit".into(), "1".into()),
            ("tag".into(), "x".into()),
            ("tag".into(), "y".into()),
            ("on".into(), "true".into()),
        ]
    );
    let r = build(&s).unwrap();
    assert_eq!(
        r.url,
        "https://api.example.com/v1/x?q=a%20b%26c&limit=1&tag=x&tag=y&on=true"
    );
    assert_eq!(r.method, "POST");
}

#[test]
fn control_no_query_means_no_question_mark() {
    assert_eq!(build(&spec(json!({}))).unwrap().url, "https://api.example.com/v1/x");
}

#[test]
fn a_path_with_its_own_query_gets_the_options_appended_with_ampersand() {
    let s = parse(HttpMethod::Get, "/v1?fixed=1", json!({"query": {"a": 2}})).unwrap();
    assert_eq!(build(&s).unwrap().url, "https://api.example.com/v1?fixed=1&a=2");
}

#[test]
fn origin_escapes_are_refused_and_dotdot_on_the_host_is_fine() {
    for path in ["//evil.com", "@evil.com", "https://evil.com/x", "/a\\b", "evil"] {
        assert!(
            matches!(parse(HttpMethod::Get, path, json!({})), Err(HttpRefusal::OriginEscape(_))),
            "{path}"
        );
    }
    assert!(parse(HttpMethod::Get, "/a/../b", json!({})).is_ok());
    assert!(parse(HttpMethod::Get, "/@evil.com", json!({})).is_ok());
    for bad in ["https://api.example.com/base?x=1", "https://api.example.com/a/../b", "https://api.example.com/a//b", "https://api.example.com/a#f", "https://api.example.com/a\\b"] {
        assert!(matches!(parse_http_call(HttpMethod::Get, bad, "/x", &Value::Null), Err(HttpRefusal::Invalid(_))), "{bad}");
    }
}

#[test]
fn egress_matches_scheme_host_and_port_exactly() {
    let e = vec!["https://api.example.com".to_string(), "http://localhost:8080".to_string()];
    assert!(origin_allowed("https://api.example.com", &e));
    assert!(origin_allowed("https://api.example.com:443", &e));
    assert!(origin_allowed("https://API.example.com/v1/x?q=1", &e));
    assert!(origin_allowed("http://localhost:8080", &e));
    assert!(!origin_allowed("http://localhost:8081", &e));
    assert!(!origin_allowed("http://localhost", &e));
    assert!(!origin_allowed("http://api.example.com", &e));
    assert!(!origin_allowed("https://api.example.com:8443", &e));
    assert!(!origin_allowed("https://evil.example.com", &e));
    assert!(!origin_allowed("https://api.example.com.evil.com", &e));
    assert!(!origin_allowed("https://api.example.com", &[]));
}

#[test]
fn build_refuses_an_origin_outside_egress() {
    let s = spec(json!({}));
    assert_eq!(
        build_request(&s, &vault, &[], None),
        Err(HttpRefusal::EgressDenied("https://api.example.com".to_string()))
    );
}

#[test]
fn every_body_kind_sets_its_content_type() {
    let r = build(&spec(json!({"json": {"a": 1, "b": [true, null]}}))).unwrap();
    assert_eq!(r.text(), r#"{"a":1,"b":[true,null]}"#);
    assert_eq!(header(&r, "content-type"), Some("application/json"));

    let r = build(&spec(json!({"form": {"a": "x y", "l": [1, 2]}}))).unwrap();
    assert_eq!(r.text(), "a=x+y&l=1&l=2");
    assert_eq!(header(&r, "content-type"), Some("application/x-www-form-urlencoded"));

    let r = build(&spec(json!({"text": "hello"}))).unwrap();
    assert_eq!(r.text(), "hello");
    assert_eq!(header(&r, "content-type"), Some("text/plain; charset=utf-8"));

    let r = build(&spec(json!({"bytes": "AAH/"}))).unwrap();
    assert_eq!(r.body, vec![0, 1, 255]);
    assert_eq!(header(&r, "content-type"), Some("application/octet-stream"));

    let r = build(&spec(json!({}))).unwrap();
    assert!(r.body.is_empty());
    assert_eq!(header(&r, "content-type"), None);
}

#[test]
fn a_caller_content_type_wins_and_conflicting_bodies_are_invalid() {
    let r = build(&spec(json!({"json": {}, "headers": {"content-type": "application/vnd.x+json"}}))).unwrap();
    assert_eq!(r.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("content-type")).count(), 1);
    assert_eq!(header(&r, "content-type"), Some("application/vnd.x+json"));
    assert!(matches!(parse(HttpMethod::Post, "/x", json!({"json": 1, "text": "a"})), Err(HttpRefusal::Invalid(_))));
    assert!(matches!(parse(HttpMethod::Get, "/x", json!({"json": 1})), Err(HttpRefusal::Invalid(_))));
    assert!(matches!(parse(HttpMethod::Post, "/x", json!({"bytes": "!!"})), Err(HttpRefusal::Invalid(_))));
    assert!(matches!(parse(HttpMethod::Post, "/x", json!({"nope": 1})), Err(HttpRefusal::Invalid(_))));
}

#[test]
fn timeout_and_response_as_are_parsed() {
    let s = spec(json!({"timeoutMs": 6000, "responseAs": "bytes"}));
    assert_eq!(s.response_as, ResponseAs::Bytes);
    assert_eq!(build(&s).unwrap().timeout, Some(Duration::from_millis(6000)));
}

#[test]
fn bearer_and_api_key_and_basic() {
    let r = build(&spec(json!({"auth": {"scheme": "bearer", "credential": "TOKEN"}}))).unwrap();
    assert_eq!(header(&r, "authorization"), Some("Bearer tok"));

    let r = build(&spec(json!({"auth": {"scheme": "api-key", "credential": "TOKEN", "name": "X-Key", "in": "header"}}))).unwrap();
    assert_eq!(header(&r, "x-key"), Some("tok"));

    let r = build(&spec(json!({"query": {"a": 1}, "auth": {"scheme": "api-key", "credential": "TOKEN", "name": "key", "in": "query"}}))).unwrap();
    assert_eq!(r.url, "https://api.example.com/v1/x?a=1&key=tok");

    let r = build(&spec(json!({"auth": {"scheme": "basic", "user": "USER", "credential": "PASS"}}))).unwrap();
    assert_eq!(header(&r, "authorization"), Some("Basic YWxpY2U6czNjcmV0"));
}

#[test]
fn a_missing_credential_is_refused_by_name() {
    let s = spec(json!({"auth": {"scheme": "bearer", "credential": "ABSENT"}}));
    assert_eq!(build_request(&s, &no_secrets, &egress(), None), Err(HttpRefusal::CredentialMissing("ABSENT".into())));
    assert_eq!(credentials_needed(&s), vec!["ABSENT".to_string()]);
    let s = spec(json!({"auth": {"scheme": "basic", "user": "USER", "credential": "PASS"}}));
    assert_eq!(credentials_needed(&s), vec!["USER".to_string(), "PASS".to_string()]);
    assert!(credentials_needed(&spec(json!({}))).is_empty());
    assert!(matches!(
        parse(HttpMethod::Post, "/x", json!({"auth": {"scheme": "digest", "credential": "A"}})),
        Err(HttpRefusal::Invalid(_))
    ));
}

#[test]
fn hmac_matches_rfc_4231_in_hex_and_base64() {
    let auth = |enc: &str| json!({"auth": {"scheme": "hmac-sha256", "credential": "Jefe", "name": "X-Sig", "signs": "what do ya want for nothing?", "encoding": enc}});
    let r = build(&spec(auth("hex"))).unwrap();
    assert_eq!(
        header(&r, "x-sig"),
        Some("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
    );
    let r = build(&spec(auth("base64"))).unwrap();
    assert_eq!(header(&r, "x-sig"), Some("W9zBRr9gdU5qBCQmCJV1x1oAPwidJzmDnexYuWTsOEM="));
}

fn oauth_spec() -> HttpCallSpec {
    spec(json!({"auth": {"scheme": "oauth2-client", "tokenUrl": "https://auth.example.com/token", "clientId": "CID", "clientSecret": "CSEC", "scope": "read write"}}))
}

#[test]
fn oauth2_token_request_cache_name_and_reply() {
    let s = oauth_spec();
    let auth = s.auth.clone().unwrap();
    assert_eq!(token_cache_name(&auth).unwrap(), "oauth2:https://auth.example.com/token|CID");
    assert_eq!(token_cache_name(&HttpAuth::Bearer { credential: "x".into() }), None);
    let t = token_exchange_request(&auth, &vault).unwrap();
    assert_eq!(t.method, "POST");
    assert_eq!(t.url, "https://auth.example.com/token");
    assert_eq!(t.text(), "grant_type=client_credentials&scope=read+write");
    assert_eq!(header(&t, "authorization"), Some("Basic Y2xpZW50LTE6c2ho"));
    assert_eq!(header(&t, "content-type"), Some("application/x-www-form-urlencoded"));
    assert_eq!(
        token_exchange_request(&auth, &no_secrets),
        Err(HttpRefusal::CredentialMissing("CID".into()))
    );

    let ok = response(200, &[], r#"{"access_token":"AT","expires_in":3600}"#);
    assert_eq!(parse_token_reply(&ok, "access_token", Some("expires_in"), 0), Ok(("AT".to_string(), 3_600_000)));
    assert!(parse_token_reply(&response(401, &[], "{}"), "access_token", Some("expires_in"), 0).is_err());
    assert!(parse_token_reply(&response(200, &[], r#"{"expires_in":1}"#), "access_token", Some("expires_in"), 0).is_err());
    assert_eq!(
        parse_token_reply(&response(200, &[], r#"{"access_token":"a"}"#), "access_token", Some("expires_in"), 0),
        Ok(("a".to_string(), 0))
    );
    assert_eq!(
        parse_token_reply(&response(200, &[], r#"{"access_token":"a","expires_in":null}"#), "access_token", Some("expires_in"), 0),
        Ok(("a".to_string(), 0))
    );
    assert!(parse_token_reply(&response(200, &[], r#"{"access_token":"a","expires_in":"soon"}"#), "access_token", Some("expires_in"), 0).is_err());
    assert!(parse_token_reply(&response(200, &[], r#"{"access_token":"a","expires_in":-5}"#), "access_token", Some("expires_in"), 0).is_err());
    assert!(parse_token_reply(&response(200, &[], "nope"), "access_token", Some("expires_in"), 0).is_err());
}

#[test]
fn next_link_reads_rel_next_among_several_entries() {
    let r = response(200, &[("Link", r#"<https://a/x?page=1>; rel="prev", <https://a/x?page=3>; rel="next", <https://a/x?page=9>; rel="last""#)], "");
    assert_eq!(next_link(&r).as_deref(), Some("https://a/x?page=3"));
    let r = response(200, &[("link", "<https://a/x,y>; rel=next")], "");
    assert_eq!(next_link(&r).as_deref(), Some("https://a/x,y"));
    assert_eq!(next_link(&response(200, &[("Link", r#"<https://a/1>; rel="prev""#)], "")), None);
    assert_eq!(next_link(&response(200, &[], "")), None);
}

#[test]
fn sse_frames_multi_line_data_event_id_and_comments() {
    let mut carry = String::new();
    let events = stream_events(
        StreamFraming::Sse,
        ": keep-alive\n\nevent: ping\nid: 7\ndata: line1\ndata: line2\n\ndata: solo\n\n",
        &mut carry,
    );
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], val(json!({"data": "line1\nline2", "event": "ping", "id": "7"})));
    assert_eq!(events[1], val(json!({"data": "solo", "event": null, "id": null})));
    assert_eq!(carry, "");
}

#[test]
fn sse_chunk_split_across_calls_uses_the_carry() {
    let mut carry = String::new();
    assert!(stream_events(StreamFraming::Sse, "data: he", &mut carry).is_empty());
    assert!(stream_events(StreamFraming::Sse, "llo\r\n", &mut carry).is_empty());
    let events = stream_events(StreamFraming::Sse, "\r\ndata: next", &mut carry);
    assert_eq!(events, vec![val(json!({"data": "hello", "event": null, "id": null}))]);
    assert_eq!(carry, "data: next");
    let tail = stream_finish(StreamFraming::Sse, &mut carry);
    assert_eq!(tail, vec![val(json!({"data": "next", "event": null, "id": null}))]);
    assert_eq!(carry, "");
    assert!(stream_finish(StreamFraming::Sse, &mut carry).is_empty());
}

#[test]
fn json_lines_split_blank_and_flushed() {
    let mut carry = String::new();
    let events = stream_events(StreamFraming::JsonLines, "{\"a\":1}\n\n{\"b\"", &mut carry);
    assert_eq!(events, vec![val(json!({"data": "{\"a\":1}", "event": null, "id": null}))]);
    let events = stream_events(StreamFraming::JsonLines, ":2}\r\n{\"c\":3}", &mut carry);
    assert_eq!(events, vec![val(json!({"data": "{\"b\":2}", "event": null, "id": null}))]);
    assert_eq!(
        stream_finish(StreamFraming::JsonLines, &mut carry),
        vec![val(json!({"data": "{\"c\":3}", "event": null, "id": null}))]
    );
}

#[test]
fn public_addresses() {
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();
    for private in [
        "127.0.0.1", "::1", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.1.1", "169.254.1.1",
        "169.254.169.254", "100.64.0.1", "100.127.255.255", "fc00::1", "fd12::1", "fe80::1", "0.0.0.0", "::",
        "224.0.0.1", "ff02::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:169.254.169.254", "::ffff:192.168.0.1", "255.255.255.255",
    ] {
        assert!(!is_public_address(ip(private)), "{private}");
    }
    for public in ["8.8.8.8", "2606:4700::1111", "172.32.0.1", "100.128.0.1", "1.1.1.1", "::ffff:8.8.8.8", "::ffff:1.1.1.1"] {
        assert!(is_public_address(ip(public)), "{public}");
    }
}

#[test]
fn reply_payload_text_bytes_and_lowercased_headers() {
    let r = response(201, &[("Content-Type", "text/plain"), ("X-A", "1"), ("x-a", "2")], "héllo");
    let p = reply_payload(&r, ResponseAs::Text);
    assert_eq!(
        p,
        val(json!({"status": 201, "headers": {"content-type": "text/plain", "x-a": "1, 2"}, "body": "héllo"}))
    );
    let b = HttpResponse { status: 200, headers: vec![], body: vec![0, 1, 255] };
    assert_eq!(get(&reply_payload(&b, ResponseAs::Bytes), "body"), Value::String("AAH/".into()));
}

#[test]
fn failure_and_refusal_payload_shapes() {
    assert_eq!(failure_payload(&TransportError::TimedOut), val(json!({"error": "request timed out", "kind": "timeout", "credential": null})));
    assert_eq!(failure_payload(&TransportError::Failed("boom".into())), val(json!({"error": "boom", "kind": "failed", "credential": null})));
    assert_eq!(failure_payload(&TransportError::Refused("private".into())), val(json!({"error": "private", "kind": "refused", "credential": null})));
    assert_eq!(
        refusal_payload(&HttpRefusal::CredentialMissing("K".into())),
        val(json!({"error": "credential missing: K", "kind": "unconfigured", "credential": "K"}))
    );
    assert_eq!(get(&refusal_payload(&HttpRefusal::EgressDenied("o".into())), "kind"), Value::String("refused".into()));
}

fn http_ask(step: HttpStep) -> HttpRequest {
    match step {
        HttpStep::Ask(HttpAsk::Http { request, .. }) => request,
        other => panic!("expected an Http ask, got {other:?}"),
    }
}

fn done(step: HttpStep) -> Result<Value, Value> {
    match step {
        HttpStep::Done(r) => r,
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn driver_plain_call_echoes_the_key() {
    let (mut d, step) = HttpCallDriver::start(spec(json!({})), egress(), Some("k1".into()));
    let req = http_ask(step);
    assert_eq!(req.url, "https://api.example.com/v1/x");
    let ok = done(d.resume(HttpAnswer::Http(Ok(response(200, &[], "hi"))))).unwrap();
    assert_eq!(get(&ok, "key"), Value::String("k1".into()));
    assert_eq!(get(&ok, "body"), Value::String("hi".into()));

    let (mut d, step) = HttpCallDriver::start(spec(json!({})), egress(), None);
    http_ask(step);
    let err = done(d.resume(HttpAnswer::Http(Err(TransportError::TimedOut)))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("timeout".into()));
    assert_eq!(get(&err, "key"), Value::Null);
}

#[test]
fn driver_refuses_before_any_http_ask() {
    let (_, step) = HttpCallDriver::start(spec(json!({})), vec![], Some("k".into()));
    let err = done(step).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("refused".into()));
    assert_eq!(get(&err, "key"), Value::String("k".into()));

    let s = spec(json!({"auth": {"scheme": "bearer", "credential": "TOKEN"}}));
    let (mut d, step) = HttpCallDriver::start(s, egress(), None);
    assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "TOKEN".into(), scope: SecretScope::App }));
    let err = done(d.resume(HttpAnswer::Secret(Ok(None)))).unwrap_err();
    assert_eq!(get(&err, "error"), Value::String("credential missing: TOKEN".into()));
}

#[test]
fn driver_secret_lookup_goes_into_the_request() {
    let s = spec(json!({"auth": {"scheme": "basic", "user": "USER", "credential": "PASS"}}));
    let (mut d, step) = HttpCallDriver::start(s, egress(), None);
    assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "USER".into(), scope: SecretScope::App }));
    let step = d.resume(HttpAnswer::Secret(Ok(Some("alice".into()))));
    assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "PASS".into(), scope: SecretScope::App }));
    let req = http_ask(d.resume(HttpAnswer::Secret(Ok(Some("s3cret".into())))));
    assert_eq!(header(&req, "authorization"), Some("Basic YWxpY2U6czNjcmV0"));
}

fn grant_reply() -> HttpAnswer {
    HttpAnswer::Http(Ok(response(200, &[], r#"{"access_token":"AT","expires_in":60}"#)))
}

#[test]
fn driver_oauth_miss_grants_stores_then_requests() {
    let (mut d, step) = HttpCallDriver::start(oauth_spec(), egress_with_auth(), None);
    let cache = "oauth2:https://auth.example.com/token|CID".to_string();
    assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: cache.clone(), scope: SecretScope::App }));
    assert_eq!(d.resume(HttpAnswer::Secret(Ok(None))), HttpStep::Ask(HttpAsk::Secret { name: "CID".into(), scope: SecretScope::App }));
    assert_eq!(
        d.resume(HttpAnswer::Secret(Ok(Some("client-1".into())))),
        HttpStep::Ask(HttpAsk::Secret { name: "CSEC".into(), scope: SecretScope::App })
    );
    let grant = http_ask(d.resume(HttpAnswer::Secret(Ok(Some("shh".into())))));
    assert_eq!(grant.url, "https://auth.example.com/token");
    let step = d.resume(grant_reply());
    assert_eq!(step, HttpStep::Ask(HttpAsk::SecretSet { name: cache, value: "AT".into(), ttl_ms: 60_000, scope: SecretScope::App }));
    let req = http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
    assert_eq!(header(&req, "authorization"), Some("Bearer AT"));
    assert_eq!(req.url, "https://api.example.com/v1/x");
}

fn egress_with_auth() -> Vec<String> {
    vec!["https://api.example.com".to_string(), "https://auth.example.com".to_string()]
}

#[test]
fn driver_oauth_cache_hit_skips_the_grant() {
    let (mut d, _) = HttpCallDriver::start(oauth_spec(), egress_with_auth(), None);
    let req = http_ask(d.resume(HttpAnswer::Secret(Ok(Some("CACHED".into())))));
    assert_eq!(header(&req, "authorization"), Some("Bearer CACHED"));
}

#[test]
fn driver_oauth_token_url_must_be_in_egress() {
    let (mut d, _) = HttpCallDriver::start(oauth_spec(), egress(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(HttpAnswer::Secret(Ok(Some("client-1".into()))));
    let err = done(d.resume(HttpAnswer::Secret(Ok(Some("shh".into()))))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("refused".into()));
}

#[test]
fn driver_401_refreshes_once_then_returns_the_reply() {
    let (mut d, _) = HttpCallDriver::start(oauth_spec(), egress_with_auth(), None);
    http_ask(d.resume(HttpAnswer::Secret(Ok(Some("OLD".into())))));
    // the 401 refreshes: the client secrets are asked once (not yet held), then grant, store, retry
    let step = d.resume(HttpAnswer::Http(Ok(response(401, &[], "expired"))));
    assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "CID".into(), scope: SecretScope::App }));
    d.resume(HttpAnswer::Secret(Ok(Some("client-1".into()))));
    http_ask(d.resume(HttpAnswer::Secret(Ok(Some("shh".into())))));
    d.resume(grant_reply());
    let retry = http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
    assert_eq!(header(&retry, "authorization"), Some("Bearer AT"));
    let reply = done(d.resume(HttpAnswer::Http(Ok(response(401, &[], "still"))))).unwrap();
    assert_eq!(get(&reply, "status"), Value::Number(401.0));
}

#[test]
fn driver_paginates_by_cursor_and_stops_on_null() {
    let (mut d, step) = HttpCallDriver::start(paginated_json("cursor", json!({"param": "after"}), 5), egress(), Some("p".into()));
    http_ask(step);
    let req = http_ask(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"items":[1,2],"next":"c2"}"#)))));
    assert_eq!(req.url, "https://api.example.com/v1/x?after=c2");
    let paged = done(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"items":[3],"next":null}"#))))).unwrap();
    assert_eq!(get(&paged, "pages"), Value::Number(2.0));
    assert_eq!(get(&paged, "items"), val(json!([1, 2, 3])));
    assert_eq!(get(&paged, "key"), Value::String("p".into()));
    assert_eq!(get(&paged, "last.status"), Value::Number(200.0));
}

fn paginated_json(style: &str, extra: serde_json::Value, max: u32) -> HttpCallSpec {
    let body = json!(["json/parse", "@response.body", {}]);
    let mut p = json!({"style": style, "next": ["object/get", body.clone(), "next"], "collect": ["object/get", body, "items"], "max": max});
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    spec(json!({"paginate": p}))
}

#[test]
fn driver_pagination_respects_max() {
    let (mut d, step) = HttpCallDriver::start(paginated_json("cursor", json!({"param": "after"}), 2), egress(), None);
    http_ask(step);
    http_ask(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"items":[1],"next":"c2"}"#)))));
    let paged = done(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"items":[2],"next":"c3"}"#))))).unwrap();
    assert_eq!(get(&paged, "pages"), Value::Number(2.0));
    assert_eq!(get(&paged, "items"), val(json!([1, 2])));
}

#[test]
fn driver_non_2xx_page_stops_with_what_it_has() {
    let (mut d, step) = HttpCallDriver::start(paginated_json("cursor", json!({"param": "after"}), 5), egress(), None);
    http_ask(step);
    http_ask(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"items":[1],"next":"c2"}"#)))));
    let paged = done(d.resume(HttpAnswer::Http(Ok(response(500, &[], "down"))))).unwrap();
    assert_eq!(get(&paged, "pages"), Value::Number(2.0));
    assert_eq!(get(&paged, "items"), val(json!([1])));
    assert_eq!(get(&paged, "last.status"), Value::Number(500.0));
}

#[test]
fn driver_link_header_follows_next_on_the_same_origin_only() {
    let s = spec(json!({"paginate": {"style": "link-header", "collect": ["json/parse", "@response.body", {}], "max": 5}}));
    let (mut d, step) = HttpCallDriver::start(s.clone(), egress(), None);
    http_ask(step);
    let link = r#"<https://api.example.com/v1/x?page=2>; rel="next""#;
    let req = http_ask(d.resume(HttpAnswer::Http(Ok(response(200, &[("Link", link)], "[1]")))));
    assert_eq!(req.url, "https://api.example.com/v1/x?page=2");
    let paged = done(d.resume(HttpAnswer::Http(Ok(response(200, &[], "[2]"))))).unwrap();
    assert_eq!(get(&paged, "items"), val(json!([1, 2])));

    let (mut d, step) = HttpCallDriver::start(s, egress(), None);
    http_ask(step);
    let evil = r#"<https://evil.com/x>; rel="next""#;
    let err = done(d.resume(HttpAnswer::Http(Ok(response(200, &[("Link", evil)], "[1]"))))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("refused".into()));
}

#[test]
fn driver_stream_asks_http_stream_and_frames_with_the_key() {
    let s = spec(json!({"stream": "sse"}));
    let (d, step) = HttpCallDriver::start(s, egress(), Some("s".into()));
    assert!(matches!(step, HttpStep::Ask(HttpAsk::HttpStream { framing: StreamFraming::Sse, .. })));
    let mut carry = String::new();
    let events = d.stream_events("data: a\n\ndata: b", &mut carry);
    assert_eq!(events, vec![val(json!({"data": "a", "event": null, "id": null, "key": "s"}))]);
    assert_eq!(d.stream_finish(&mut carry), vec![val(json!({"data": "b", "event": null, "id": null, "key": "s"}))]);
}

#[test]
fn a_driver_serialized_mid_flight_resumes_with_the_same_steps() {
    let s = spec(json!({"auth": {"scheme": "bearer", "credential": "TOKEN"}, "paginate": {"style": "cursor", "param": "after", "next": ["object/get", ["json/parse", "@response.body", {}], "next"], "collect": ["object/get", ["json/parse", "@response.body", {}], "items"], "max": 3}}));
    let (mut live, step) = HttpCallDriver::start(s, egress(), Some("k".into()));
    assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "TOKEN".into(), scope: SecretScope::App }));
    let first = http_ask(live.resume(HttpAnswer::Secret(Ok(Some("tok".into())))));

    let text = serde_json::to_string(&live).unwrap();
    assert!(!text.contains("tok"), "driver JSON must not carry a secret value: {text}");
    let mut revived: HttpCallDriver = serde_json::from_str(&text).unwrap();
    assert_eq!(serde_json::to_string(&revived).unwrap(), text);
    let answer = HttpAnswer::Http(Ok(response(200, &[("X", "y")], r#"{"items":[1],"next":"c2"}"#)));
    let wire = serde_json::to_string(&answer).unwrap();
    let answer: HttpAnswer = serde_json::from_str(&wire).unwrap();
    let a = live.resume(answer.clone());
    assert_eq!(revived.resume(answer), HttpStep::Ask(HttpAsk::Secret { name: "TOKEN".into(), scope: SecretScope::App }));
    let b = revived.resume(HttpAnswer::Secret(Ok(Some("tok".into()))));
    assert_eq!(a, b);
    let step_wire: HttpStep = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
    assert_eq!(step_wire, a);
    assert_eq!(first.url, "https://api.example.com/v1/x");
    assert_eq!(http_ask(a).url, "https://api.example.com/v1/x?after=c2");
}

#[test]
fn control_an_unexpected_answer_fails_instead_of_panicking() {
    let (mut d, _) = HttpCallDriver::start(spec(json!({})), egress(), None);
    let err = done(d.resume(HttpAnswer::SecretSet(Ok(())))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("failed".into()));
}

#[test]
fn frame_stream_is_the_one_place_the_key_is_added() {
    let mut carry = String::new();
    assert_eq!(
        frame_stream(StreamFraming::JsonLines, Some("k"), "{\"a\":1}\n{\"b\"", &mut carry),
        vec![val(json!({"data": "{\"a\":1}", "event": null, "id": null, "key": "k"}))]
    );
    assert_eq!(
        frame_stream_finish(StreamFraming::JsonLines, Some("k"), &mut carry),
        vec![val(json!({"data": "{\"b\"", "event": null, "id": null, "key": "k"}))]
    );
    assert_eq!(
        frame_stream(StreamFraming::Sse, None, "data: x\n\n", &mut String::new()),
        vec![val(json!({"data": "x", "event": null, "id": null, "key": null}))]
    );
    assert_eq!(
        stream_events(StreamFraming::Sse, "data: x\n\n", &mut String::new()),
        vec![val(json!({"data": "x", "event": null, "id": null}))],
        "the bare framer adds no key"
    );
}

#[test]
fn a_refused_call_carries_its_key_from_core() {
    let r = HttpRefusal::Invalid("bad".into());
    let err = done(HttpCallDriver::refused(&r, Some("k"))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("refused".into()));
    assert_eq!(get(&err, "key"), Value::String("k".into()));
    assert_eq!(get(&done(HttpCallDriver::refused(&r, None)).unwrap_err(), "key"), Value::Null);
}

#[test]
fn driver_answers_and_steps_use_the_shared_settled_shape() {
    let answer: HttpAnswer = serde_json::from_str(r#"{"kind":"secret","result":{"ok":"v"}}"#).unwrap();
    assert_eq!(answer, HttpAnswer::Secret(Ok(Some("v".into()))));
    let failed: HttpAnswer = serde_json::from_str(r#"{"kind":"secret","result":{"error":"no"}}"#).unwrap();
    assert_eq!(failed, HttpAnswer::Secret(Err("no".into())));
    let step = HttpStep::Done(Err(val(json!({"error": "x"}))));
    assert!(serde_json::to_string(&step).unwrap().contains(r#""error""#));
    assert_eq!(serde_json::from_str::<HttpStep>(&serde_json::to_string(&step).unwrap()).unwrap(), step);
}

#[test]
fn nothing_the_driver_prints_or_serializes_holds_a_secret_value() {
    let s = spec(json!({"headers": {"X-Trace": "public-trace"}, "auth": {"scheme": "bearer", "credential": "TOKEN"}}));
    let (mut d, step) = HttpCallDriver::start(s, egress(), None);
    assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "TOKEN".into(), scope: SecretScope::App }));
    let answer = HttpAnswer::Secret(Ok(Some("sk-live-123".into())));
    assert!(!format!("{answer:?}").contains("sk-live-123"));
    let step = d.resume(answer);
    let req = http_ask(step.clone());
    assert_eq!(header(&req, "authorization"), Some("Bearer sk-live-123"));
    for shown in [format!("{d:?}"), format!("{step:?}"), format!("{req:?}"), serde_json::to_string(&d).unwrap()] {
        assert!(!shown.contains("sk-live-123"), "leaked: {shown}");
    }
    let driver_json = serde_json::to_string(&d).unwrap();
    assert!(driver_json.contains("public-trace"), "control: a non-secret header is still shown");
    assert!(format!("{req:?}").contains("Authorization"), "control: header names are shown");
}

#[test]
fn a_minted_token_is_not_printed_or_serialized_with_the_driver() {
    let (mut d, _) = HttpCallDriver::start(oauth_spec(), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(HttpAnswer::Secret(Ok(Some("client-id-value".into()))));
    http_ask(d.resume(HttpAnswer::Secret(Ok(Some("client-secret-value".into())))));
    let ask = d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"access_token":"minted-AT","expires_in":60}"#))));
    for shown in [format!("{ask:?}"), format!("{d:?}"), serde_json::to_string(&d).unwrap()] {
        assert!(!shown.contains("minted-AT"), "leaked: {shown}");
        assert!(!shown.contains("client-secret-value") && !shown.contains("client-id-value"), "leaked: {shown}");
    }
    let HttpStep::Ask(HttpAsk::SecretSet { value, .. }) = ask else { panic!("expected SecretSet") };
    assert_eq!(value, "minted-AT");
}

#[test]
fn a_revived_oauth_driver_asks_again_for_the_values_it_lost() {
    let (mut d, _) = HttpCallDriver::start(oauth_spec(), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    let revive = |d: &HttpCallDriver| serde_json::from_str::<HttpCallDriver>(&serde_json::to_string(d).unwrap()).unwrap();
    d.resume(HttpAnswer::Secret(Ok(Some("client-1".into()))));
    let mut d = revive(&d);
    let step = d.resume(HttpAnswer::Secret(Ok(Some("shh".into()))));
    assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "CID".into(), scope: SecretScope::App }));
}

#[test]
fn a_token_reply_without_expires_in_is_stored_for_zero_ms() {
    let (mut d, _) = HttpCallDriver::start(oauth_spec(), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(HttpAnswer::Secret(Ok(Some("client-1".into()))));
    http_ask(d.resume(HttpAnswer::Secret(Ok(Some("shh".into())))));
    let step = d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"access_token":"AT"}"#))));
    assert_eq!(
        step,
        HttpStep::Ask(HttpAsk::SecretSet {
            name: "oauth2:https://auth.example.com/token|CID".into(),
            value: "AT".into(),
            ttl_ms: 0,
            scope: SecretScope::App
        })
    );
}

#[test]
fn control_a_miss_right_after_a_grant_is_a_refusal_not_another_grant() {
    let (mut d, _) = HttpCallDriver::start(oauth_spec(), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(HttpAnswer::Secret(Ok(Some("client-1".into()))));
    http_ask(d.resume(HttpAnswer::Secret(Ok(Some("shh".into())))));
    d.resume(grant_reply());
    let mut revived: HttpCallDriver = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
    let step = revived.resume(HttpAnswer::SecretSet(Ok(())));
    assert!(matches!(step, HttpStep::Ask(HttpAsk::Secret { .. })));
    let err = done(revived.resume(HttpAnswer::Secret(Ok(None)))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("unconfigured".into()));
    assert_eq!(get(&err, "credential"), Value::String("oauth2:https://auth.example.com/token|CID".into()));
}

// --- H.7 P4: the driver asks the host for entropy -----------------------------------------------------------

fn secret_ask(name: &str, scope: SecretScope) -> HttpStep {
    HttpStep::Ask(HttpAsk::Secret { name: name.into(), scope })
}

fn ok_secret(value: &str) -> HttpAnswer {
    HttpAnswer::Secret(Ok(Some(value.into())))
}

fn multipart_spec(kind: &str) -> HttpCallSpec {
    spec(json!({"multipart": {"kind": kind, "parts": [
        {"headers": {"Content-Disposition": "form-data; name=\"a\""}, "text": "x"},
        {"headers": {"Content-Disposition": "form-data; name=\"f\"; filename=\"f.bin\"", "Content-Type": "application/octet-stream"}, "bytes": "AAH/"}
    ]}}))
}

const RANDOM_16: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const BOUNDARY: &str = "almadar-000102030405060708090a0b0c0d0e0f";

#[test]
fn a_multipart_call_asks_the_host_for_random_bytes_before_it_sends() {
    let (mut d, step) = HttpCallDriver::start(multipart_spec("form-data"), egress(), None);
    assert_eq!(step, HttpStep::Ask(HttpAsk::Random { bytes: 16 }));
    let req = http_ask(d.resume(HttpAnswer::Random(Ok(RANDOM_16.to_vec()))));
    assert_eq!(header(&req, "content-type"), Some(format!("multipart/form-data; boundary={BOUNDARY}").as_str()));
    let mut expected = Vec::new();
    expected.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n").as_bytes());
    expected.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"f\"; filename=\"f.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
    );
    expected.extend_from_slice(&[0, 1, 255]);
    expected.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    assert_eq!(req.body, expected);
}

#[test]
fn control_a_call_without_a_multipart_body_never_asks_for_random() {
    for request in [json!({}), json!({"json": {"a": 1}}), json!({"form": {"a": 1}})] {
        let (_, step) = HttpCallDriver::start(spec(request), egress(), None);
        assert!(matches!(step, HttpStep::Ask(HttpAsk::Http { .. })), "{step:?}");
    }
}

#[test]
fn credentials_are_asked_before_the_boundary() {
    let mut s = multipart_spec("related");
    s.auth = Some(HttpAuth::Bearer { credential: "TOKEN".into() });
    let (mut d, step) = HttpCallDriver::start(s, egress(), None);
    assert_eq!(step, secret_ask("TOKEN", SecretScope::App));
    assert_eq!(d.resume(ok_secret("tok")), HttpStep::Ask(HttpAsk::Random { bytes: 16 }));
    let req = http_ask(d.resume(HttpAnswer::Random(Ok(RANDOM_16.to_vec()))));
    assert_eq!(header(&req, "authorization"), Some("Bearer tok"));
    assert!(header(&req, "content-type").unwrap().starts_with("multipart/related; boundary="));
}

#[test]
fn a_failed_or_short_random_answer_fails_the_call() {
    let (mut d, _) = HttpCallDriver::start(multipart_spec("related"), egress(), Some("k".into()));
    let err = done(d.resume(HttpAnswer::Random(Err("no entropy".into())))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("failed".into()));
    assert_eq!(get(&err, "error"), Value::String("no entropy".into()));
    assert_eq!(get(&err, "key"), Value::String("k".into()));

    let (mut d, _) = HttpCallDriver::start(multipart_spec("related"), egress(), None);
    let err = done(d.resume(HttpAnswer::Random(Ok(vec![1, 2, 3])))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("failed".into()));
}

#[test]
fn the_boundary_is_drawn_once_per_call_and_survives_a_revived_driver() {
    let s = spec(json!({
        "multipart": {"kind": "related", "parts": [{"text": "x"}]},
        "retry": {"max": 1, "on": ["5xx"], "backoffMs": 10}
    }));
    let (mut d, _) = HttpCallDriver::start(s, egress(), None);
    let first = http_ask(d.resume(HttpAnswer::Random(Ok(RANDOM_16.to_vec()))));
    let mut revived: HttpCallDriver = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
    let step = revived.resume(HttpAnswer::Http(Ok(response(503, &[], ""))));
    let HttpStep::Ask(HttpAsk::Http { request, delay_ms }) = step else { panic!("expected a retry") };
    assert_eq!(delay_ms, Some(10));
    assert_eq!(request, first, "the same boundary, so the same bytes");
}

#[test]
fn random_asks_and_answers_have_a_stable_wire_shape() {
    let ask = HttpAsk::Random { bytes: 16 };
    assert_eq!(serde_json::to_value(&ask).unwrap(), json!({"kind": "random", "bytes": 16}));
    assert_eq!(serde_json::from_value::<HttpAsk>(json!({"kind": "random", "bytes": 16})).unwrap(), ask);
    let answer = HttpAnswer::Random(Ok(vec![0, 1, 255]));
    assert_eq!(serde_json::to_value(&answer).unwrap(), json!({"kind": "random", "result": {"ok": "AAH/"}}));
    assert_eq!(serde_json::from_value::<HttpAnswer>(json!({"kind": "random", "result": {"ok": "AAH/"}})).unwrap(), answer);
    let failed = serde_json::from_value::<HttpAnswer>(json!({"kind": "random", "result": {"error": "no"}})).unwrap();
    assert_eq!(failed, HttpAnswer::Random(Err("no".into())));
    assert!(serde_json::from_value::<HttpAnswer>(json!({"kind": "random", "result": {"ok": "!!"}})).is_err());
}

#[test]
fn random_bytes_are_not_printed() {
    let shown = format!("{:?}", HttpAnswer::Random(Ok(vec![0xAB, 0xCD])));
    assert!(!shown.contains("171") && !shown.contains("205") && !shown.contains("AB"), "{shown}");
    assert!(shown.contains("Random"), "control: the variant is shown");
}

// --- H.7 multipart bodies -----------------------------------------------------------------------------------

#[test]
fn multipart_bodies_parse_and_are_one_of_the_body_kinds() {
    let s = multipart_spec("related");
    assert!(matches!(s.body, HttpBodySpec::Multipart { kind: MultipartKind::Related, .. }));
    assert_eq!(
        build_request(&s, &vault, &egress(), None),
        Err(HttpRefusal::Invalid("a multipart body needs a boundary from the driver".into()))
    );
    let built = build_request_with_boundary(&s, &vault, &egress(), None, Some("B")).unwrap();
    assert_eq!(header(&built, "content-type"), Some("multipart/related; boundary=B"));
    assert!(built.text().starts_with("--B\r\n"));
    assert!(built.text().ends_with("\r\n--B--\r\n"));

    for bad in [
        json!({"multipart": {"kind": "related", "parts": [{"text": "x"}]}, "json": {}}),
        json!({"multipart": {"kind": "mixed", "parts": [{"text": "x"}]}}),
        json!({"multipart": {"kind": "related", "parts": []}}),
        json!({"multipart": {"kind": "related"}}),
        json!({"multipart": {"kind": "related", "parts": [{"text": "x", "bytes": "AA=="}]}}),
        json!({"multipart": {"kind": "related", "parts": [{"headers": {"A": "b"}}]}}),
        json!({"multipart": {"kind": "related", "parts": [{"text": "x", "extra": 1}]}}),
        json!({"multipart": {"kind": "related", "parts": [{"bytes": "!!"}]}}),
        json!({"multipart": {"kind": "related", "parts": [{"headers": {"A\r\nB": "c"}, "text": "x"}]}}),
        json!({"multipart": {"kind": "related", "parts": [{"headers": {"A": "b\r\nC: d"}, "text": "x"}]}}),
        json!({"multipart": {"kind": "related", "parts": [{"text": "x"}]}, "headers": {"Content-Type": "text/plain"}}),
        json!({"multipart": {"kind": "related", "parts": [{"text": "x"}], "extra": 1}}),
    ] {
        assert!(matches!(parse(HttpMethod::Post, "/x", bad.clone()), Err(HttpRefusal::Invalid(_))), "{bad}");
    }
    assert!(matches!(
        parse(HttpMethod::Get, "/x", json!({"multipart": {"kind": "related", "parts": [{"text": "x"}]}})),
        Err(HttpRefusal::Invalid(_))
    ));
}

// --- H.7 P7: retry ------------------------------------------------------------------------------------------

fn retrying(extra: serde_json::Value) -> HttpCallSpec {
    let mut request = json!({"retry": {"max": 2, "on": ["5xx", "429", "timeout"], "backoffMs": 100}});
    request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    spec(request)
}

fn http_ask_delay(step: HttpStep) -> (HttpRequest, Option<u64>) {
    match step {
        HttpStep::Ask(HttpAsk::Http { request, delay_ms }) => (request, delay_ms),
        other => panic!("expected an Http ask, got {other:?}"),
    }
}

#[test]
fn a_retryable_reply_is_asked_again_with_a_growing_delay_then_given_up_on() {
    let (mut d, step) = HttpCallDriver::start(retrying(json!({})), egress(), Some("r".into()));
    let (first, delay) = http_ask_delay(step);
    assert_eq!(delay, None);
    let (again, delay) = http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], "")))));
    assert_eq!((again.clone(), delay), (first.clone(), Some(100)));
    let (_, delay) = http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(429, &[], "")))));
    assert_eq!(delay, Some(200));
    let last = done(d.resume(HttpAnswer::Http(Ok(response(500, &[], "down"))))).unwrap();
    assert_eq!(get(&last, "status"), Value::Number(500.0));
    assert_eq!(get(&last, "key"), Value::String("r".into()));
}

#[test]
fn a_retry_that_succeeds_returns_that_reply() {
    let (mut d, _) = HttpCallDriver::start(retrying(json!({})), egress(), None);
    http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(502, &[], "")))));
    let ok = done(d.resume(HttpAnswer::Http(Ok(response(200, &[], "fine"))))).unwrap();
    assert_eq!(get(&ok, "body"), Value::String("fine".into()));
}

#[test]
fn a_timeout_is_retried_only_when_listed_and_other_failures_never() {
    let (mut d, _) = HttpCallDriver::start(retrying(json!({})), egress(), None);
    let (_, delay) = http_ask_delay(d.resume(HttpAnswer::Http(Err(TransportError::TimedOut))));
    assert_eq!(delay, Some(100));
    let err = done(d.resume(HttpAnswer::Http(Err(TransportError::Failed("reset".into()))))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("failed".into()));

    let only_5xx = spec(json!({"retry": {"max": 2, "on": ["5xx"], "backoffMs": 100}}));
    let (mut d, _) = HttpCallDriver::start(only_5xx, egress(), None);
    let err = done(d.resume(HttpAnswer::Http(Err(TransportError::TimedOut)))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("timeout".into()));
}

#[test]
fn control_statuses_outside_the_list_and_calls_without_retry_are_not_retried() {
    for status in [200, 404, 400, 401, 499, 600] {
        let (mut d, _) = HttpCallDriver::start(retrying(json!({})), egress(), None);
        let reply = done(d.resume(HttpAnswer::Http(Ok(response(status, &[], ""))))).unwrap();
        assert_eq!(get(&reply, "status"), Value::Number(f64::from(status)), "{status}");
    }
    let only_429 = spec(json!({"retry": {"max": 3, "on": ["429"], "backoffMs": 1}}));
    let (mut e, _) = HttpCallDriver::start(only_429, egress(), None);
    assert!(done(e.resume(HttpAnswer::Http(Ok(response(500, &[], ""))))).is_ok());
    let (mut plain, _) = HttpCallDriver::start(spec(json!({})), egress(), None);
    assert!(done(plain.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))).is_ok());
}

#[test]
fn the_delay_saturates_instead_of_overflowing() {
    let s = spec(json!({"retry": {"max": 70, "on": ["5xx"], "backoffMs": 4_000_000_000_000_000_000u64}}));
    let (mut d, _) = HttpCallDriver::start(s, egress(), None);
    let mut last = 0;
    for _ in 0..70 {
        let (_, delay) = http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(500, &[], "")))));
        last = delay.unwrap();
    }
    assert_eq!(last, u64::MAX);
}

#[test]
fn each_page_gets_its_own_retries() {
    let body = json!(["json/parse", "@response.body", {}]);
    let s = retrying(json!({"paginate": {"style": "cursor", "param": "after",
        "next": ["object/get", body.clone(), "next"], "collect": ["object/get", body, "items"], "max": 5}}));
    let (mut d, _) = HttpCallDriver::start(s, egress(), None);
    http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], "")))));
    let (req, delay) = http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"items":[1],"next":"c2"}"#)))));
    assert_eq!((req.url.as_str(), delay), ("https://api.example.com/v1/x?after=c2", None));
    http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], "")))));
    let (_, delay) = http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], "")))));
    assert_eq!(delay, Some(200), "the second page spent its own two retries");
    assert!(done(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))).is_ok());
}

#[test]
fn retry_parse_rules() {
    for bad in [
        json!({"retry": {"max": 0, "on": ["5xx"], "backoffMs": 1}}),
        json!({"retry": {"max": 1.5, "on": ["5xx"], "backoffMs": 1}}),
        json!({"retry": {"on": ["5xx"], "backoffMs": 1}}),
        json!({"retry": {"max": 1, "on": [], "backoffMs": 1}}),
        json!({"retry": {"max": 1, "backoffMs": 1}}),
        json!({"retry": {"max": 1, "on": ["4xx"], "backoffMs": 1}}),
        json!({"retry": {"max": 1, "on": ["5xx"]}}),
        json!({"retry": {"max": 1, "on": ["5xx"], "backoffMs": -1}}),
        json!({"retry": {"max": 1, "on": ["5xx"], "backoffMs": 1, "extra": 1}}),
        json!({"retry": 3}),
        json!({"retry": {"max": 1, "on": ["5xx"], "backoffMs": 1}, "stream": "sse"}),
    ] {
        assert!(matches!(parse(HttpMethod::Post, "/x", bad.clone()), Err(HttpRefusal::Invalid(_))), "{bad}");
    }
    assert!(parse(HttpMethod::Post, "/x", json!({"retry": {"max": 1, "on": ["5xx"], "backoffMs": 0}})).is_ok());
}

// --- H.7 P8: formEncoding -----------------------------------------------------------------------------------

#[test]
fn form_encoding_html_is_the_default_and_rfc3986_encodes_spaces_as_percent_20() {
    let form = json!({"a b": "x y", "b": "!'()*~"});
    let html = build(&spec(json!({"form": form.clone()}))).unwrap();
    assert_eq!(html.text(), "a+b=x+y&b=%21%27%28%29*%7E");
    let explicit = build(&spec(json!({"form": form.clone(), "formEncoding": "html"}))).unwrap();
    assert_eq!(explicit.body, html.body);
    let rfc = build(&spec(json!({"form": form, "formEncoding": "rfc3986"}))).unwrap();
    assert_eq!(rfc.text(), "a%20b=x%20y&b=%21%27%28%29%2A~");
    assert_eq!(header(&rfc, "content-type"), Some("application/x-www-form-urlencoded"));
}

#[test]
fn form_encoding_is_a_form_only_closed_choice() {
    for bad in [
        json!({"form": {"a": 1}, "formEncoding": "base64"}),
        json!({"form": {"a": 1}, "formEncoding": 3}),
        json!({"json": {"a": 1}, "formEncoding": "html"}),
        json!({"formEncoding": "rfc3986"}),
    ] {
        assert!(matches!(parse(HttpMethod::Post, "/x", bad.clone()), Err(HttpRefusal::Invalid(_))), "{bad}");
    }
    let r = build(&spec(json!({"form": {"l": [1, 2]}, "formEncoding": "rfc3986"}))).unwrap();
    assert_eq!(r.text(), "l=1&l=2", "a list value repeats the key under either encoding");
}

// --- H.7 P11: origin with a path prefix ---------------------------------------------------------------------

fn prefixed(origin: &str, path: &str) -> HttpCallSpec {
    parse_http_call(HttpMethod::Get, origin, path, &Value::Null).unwrap()
}

#[test]
fn an_origin_path_prefix_is_prepended_and_egress_matches_scheme_host_and_port_only() {
    for origin in ["https://api.example.com/esign/v2", "https://api.example.com/esign/v2/"] {
        let s = prefixed(origin, "/envelopes?x=1");
        let r = build(&s).unwrap();
        assert_eq!(r.url, "https://api.example.com/esign/v2/envelopes?x=1", "{origin}");
        let (_, step) = HttpCallDriver::start(s, egress(), None);
        assert!(matches!(step, HttpStep::Ask(HttpAsk::Http { .. })), "{origin}");
    }
    assert!(origin_allowed("https://api.example.com/esign/v2", &egress()));
    assert!(!origin_allowed("https://other.example.com/esign/v2", &egress()));
    let denied = prefixed("https://other.example.com/esign", "/x");
    assert_eq!(build(&denied), Err(HttpRefusal::EgressDenied("https://other.example.com/esign".into())));
    assert_eq!(prefixed("https://api.example.com/", "/x").origin, "https://api.example.com");
}

#[test]
fn a_prefixed_origin_still_cannot_be_escaped_by_the_path() {
    for path in ["//evil.com", "@evil.com", "https://evil.com/x", "/a\\b"] {
        assert!(
            matches!(parse_http_call(HttpMethod::Get, "https://api.example.com/esign", path, &Value::Null), Err(HttpRefusal::OriginEscape(_))),
            "{path}"
        );
    }
}

#[test]
fn a_link_header_page_resolves_against_the_host_not_the_prefix() {
    let s = parse_http_call(
        HttpMethod::Get,
        "https://api.example.com/esign/v2",
        "/items",
        &val(json!({"paginate": {"style": "link-header", "collect": ["json/parse", "@response.body", {}], "max": 5}})),
    )
    .unwrap();
    let (mut d, step) = HttpCallDriver::start(s, egress(), None);
    http_ask(step);
    let link = r#"</esign/v2/items?page=2>; rel="next""#;
    let req = http_ask(d.resume(HttpAnswer::Http(Ok(response(200, &[("Link", link)], "[1]")))));
    assert_eq!(req.url, "https://api.example.com/esign/v2/items?page=2");
}

// --- H.7 P10: the unconfigured failure kind -----------------------------------------------------------------

#[test]
fn a_missing_credential_is_unconfigured_and_names_the_credential_everything_else_is_not() {
    let payload = refusal_payload(&HttpRefusal::CredentialMissing("SENDGRID_KEY".into()));
    assert_eq!(get(&payload, "kind"), Value::String("unconfigured".into()));
    assert_eq!(get(&payload, "credential"), Value::String("SENDGRID_KEY".into()));
    assert_eq!(get(&payload, "error"), Value::String("credential missing: SENDGRID_KEY".into()));
    for other in [
        HttpRefusal::EgressDenied("o".into()),
        HttpRefusal::OriginEscape("p".into()),
        HttpRefusal::Invalid("i".into()),
    ] {
        let payload = refusal_payload(&other);
        assert_eq!(get(&payload, "kind"), Value::String("refused".into()));
        assert_eq!(get(&payload, "credential"), Value::Null);
    }
}

#[test]
fn a_missing_credential_finishes_the_driver_as_unconfigured_with_the_key() {
    let s = spec(json!({"auth": {"scheme": "bearer", "credential": "TOKEN"}}));
    let (mut d, _) = HttpCallDriver::start(s, egress(), Some("k".into()));
    let err = done(d.resume(HttpAnswer::Secret(Ok(None)))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("unconfigured".into()));
    assert_eq!(get(&err, "credential"), Value::String("TOKEN".into()));
    assert_eq!(get(&err, "key"), Value::String("k".into()));
}

// --- H.7 P1: token-exchange ---------------------------------------------------------------------------------

fn login_auth() -> serde_json::Value {
    json!({"scheme": "token-exchange", "tokenUrl": "https://auth.example.com/login",
        "body": {"json": {"username": {"credential": "USER"}, "password": {"credential": "PASS"}, "device": "web", "n": 7}},
        "accessField": "token", "expiresField": "ttl", "expiresMarginMs": 1000,
        "storeAs": {"name": "bank:session", "scope": "user"}})
}

#[test]
fn a_json_login_exchange_asks_its_slots_posts_stores_under_the_name_and_sends_bearer() {
    let (mut d, step) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress_with_auth(), None);
    assert_eq!(step, secret_ask("bank:session", SecretScope::User));
    assert_eq!(d.resume(HttpAnswer::Secret(Ok(None))), secret_ask("USER", SecretScope::App));
    assert_eq!(d.resume(ok_secret("alice")), secret_ask("PASS", SecretScope::App));
    let grant = http_ask(d.resume(ok_secret("s3cret")));
    assert_eq!(grant.method, "POST");
    assert_eq!(grant.url, "https://auth.example.com/login");
    assert_eq!(grant.text(), r#"{"username":"alice","password":"s3cret","device":"web","n":7}"#);
    assert_eq!(header(&grant, "content-type"), Some("application/json"));
    assert_eq!(header(&grant, "authorization"), None);

    let step = d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"T1","ttl":60}"#))));
    assert_eq!(
        step,
        HttpStep::Ask(HttpAsk::SecretSet { name: "bank:session".into(), value: "T1".into(), ttl_ms: 59_000, scope: SecretScope::User })
    );
    let req = http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
    assert_eq!(header(&req, "authorization"), Some("Bearer T1"));
    assert_eq!(req.url, "https://api.example.com/v1/x");
}

#[test]
fn a_cached_exchange_token_skips_the_exchange() {
    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress_with_auth(), None);
    let req = http_ask(d.resume(ok_secret("CACHED")));
    assert_eq!(header(&req, "authorization"), Some("Bearer CACHED"));
}

fn refresh_auth(client_auth: &str) -> serde_json::Value {
    json!({"scheme": "token-exchange", "tokenUrl": "https://auth.example.com/token", "grant": "refresh_token",
        "body": {"form": {"refresh_token": {"credential": "RT", "scope": "user"}}},
        "clientAuth": client_auth, "clientId": "CID", "clientSecret": "CSEC",
        "storeAs": {"name": "drive:access", "scope": "user"}})
}

#[test]
fn a_refresh_token_grant_with_client_auth_post_sends_the_credentials_in_the_form() {
    let (mut d, step) = HttpCallDriver::start(spec(json!({"auth": refresh_auth("post")})), egress_with_auth(), None);
    assert_eq!(step, secret_ask("drive:access", SecretScope::User));
    assert_eq!(d.resume(HttpAnswer::Secret(Ok(None))), secret_ask("CID", SecretScope::App));
    assert_eq!(d.resume(ok_secret("client-1")), secret_ask("CSEC", SecretScope::App));
    assert_eq!(d.resume(ok_secret("shh")), secret_ask("RT", SecretScope::User));
    let grant = http_ask(d.resume(ok_secret("rt-1")));
    assert_eq!(grant.text(), "grant_type=refresh_token&refresh_token=rt-1&client_id=client-1&client_secret=shh");
    assert_eq!(header(&grant, "authorization"), None);
    assert_eq!(header(&grant, "content-type"), Some("application/x-www-form-urlencoded"));
}

#[test]
fn client_auth_basic_moves_the_credentials_into_the_authorization_header() {
    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": refresh_auth("basic")})), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(ok_secret("client-1"));
    d.resume(ok_secret("shh"));
    let grant = http_ask(d.resume(ok_secret("rt-1")));
    assert_eq!(grant.text(), "grant_type=refresh_token&refresh_token=rt-1");
    assert_eq!(header(&grant, "authorization"), Some("Basic Y2xpZW50LTE6c2ho"));
}

#[test]
fn defaults_are_access_token_expires_in_no_margin_and_a_derived_cache_name() {
    let auth = json!({"scheme": "token-exchange", "tokenUrl": "https://auth.example.com/token",
        "body": {"form": {"k": {"credential": "KEY"}}}});
    let (mut d, step) = HttpCallDriver::start(spec(json!({"auth": auth})), egress_with_auth(), None);
    let cache = "token-exchange:https://auth.example.com/token|KEY".to_string();
    assert_eq!(step, secret_ask(&cache, SecretScope::App));
    d.resume(HttpAnswer::Secret(Ok(None)));
    let grant = http_ask(d.resume(ok_secret("kv")));
    assert_eq!(grant.text(), "k=kv", "grant custom: no grant_type is added");
    let step = d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"access_token":"A","expires_in":10}"#))));
    assert_eq!(
        step,
        HttpStep::Ask(HttpAsk::SecretSet { name: cache, value: "A".into(), ttl_ms: 10_000, scope: SecretScope::App })
    );
}

#[test]
fn a_margin_at_or_past_the_lifetime_stores_for_zero_ms_and_a_missing_access_field_fails() {
    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(ok_secret("alice"));
    http_ask(d.resume(ok_secret("s3cret")));
    let step = d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"T","ttl":1}"#))));
    assert!(matches!(step, HttpStep::Ask(HttpAsk::SecretSet { ttl_ms: 0, .. })), "{step:?}");

    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(ok_secret("alice"));
    http_ask(d.resume(ok_secret("s3cret")));
    let err = done(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"access_token":"T"}"#))))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("failed".into()));
    assert!(matches!(get(&err, "error"), Value::String(m) if m.contains("token")));
}

#[test]
fn a_missing_slot_credential_or_a_refused_exchange_url_ends_the_call() {
    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(ok_secret("alice"));
    let err = done(d.resume(HttpAnswer::Secret(Ok(None)))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("unconfigured".into()));
    assert_eq!(get(&err, "credential"), Value::String("PASS".into()));

    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(ok_secret("alice"));
    let err = done(d.resume(ok_secret("s3cret"))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::String("refused".into()), "tokenUrl is outside egress");
}

#[test]
fn a_401_refreshes_a_token_exchange_once_through_the_same_path() {
    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress_with_auth(), None);
    http_ask(d.resume(ok_secret("OLD")));
    let step = d.resume(HttpAnswer::Http(Ok(response(401, &[], "expired"))));
    assert_eq!(step, secret_ask("USER", SecretScope::App));
    d.resume(ok_secret("alice"));
    http_ask(d.resume(ok_secret("s3cret")));
    d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"NEW","ttl":600}"#))));
    let retry = http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
    assert_eq!(header(&retry, "authorization"), Some("Bearer NEW"));
    let reply = done(d.resume(HttpAnswer::Http(Ok(response(401, &[], "still"))))).unwrap();
    assert_eq!(get(&reply, "status"), Value::Number(401.0));
}

#[test]
fn oauth2_client_is_a_lowering_to_token_exchange() {
    let lowered = oauth_spec().auth.unwrap();
    let explicit = spec(json!({"auth": {
        "scheme": "token-exchange", "tokenUrl": "https://auth.example.com/token", "grant": "client_credentials",
        "body": {"form": {"scope": "read write"}}, "clientAuth": "basic", "clientId": "CID", "clientSecret": "CSEC",
        "storeAs": {"name": "oauth2:https://auth.example.com/token|CID"}}}))
    .auth
    .unwrap();
    assert_eq!(lowered, explicit);
    assert!(matches!(lowered, HttpAuth::TokenExchange(_)));
    let bare = spec(json!({"auth": {"scheme": "oauth2-client", "tokenUrl": "https://auth.example.com/token", "clientId": "CID", "clientSecret": "CSEC"}}));
    let t = token_exchange_request(bare.auth.as_ref().unwrap(), &vault).unwrap();
    assert_eq!(t.text(), "grant_type=client_credentials");
}

#[test]
fn token_exchange_parse_rules() {
    let base = |patch: serde_json::Value| {
        let mut auth = json!({"scheme": "token-exchange", "tokenUrl": "https://auth.example.com/token", "body": {"form": {"a": "b"}}});
        auth.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
        auth
    };
    assert!(parse(HttpMethod::Post, "/x", json!({"auth": base(json!({}))})).is_ok());
    for bad in [
        base(json!({"grant": "implicit"})),
        base(json!({"grant": "refresh_token"})),
        base(json!({"grant": "authorization_code"})),
        base(json!({"body": {"json": {}, "form": {}}})),
        base(json!({"body": {"xml": {}}})),
        base(json!({"body": {"form": {"o": {"nested": 1}}}})),
        base(json!({"body": {"form": {"grant_type": "x"}, }, "grant": "client_credentials"})),
        base(json!({"clientAuth": "basic"})),
        base(json!({"clientId": "CID", "clientSecret": "CSEC"})),
        base(json!({"clientAuth": "digest", "clientId": "CID", "clientSecret": "CSEC"})),
        base(json!({"tokenUrl": "ftp://auth"})),
        base(json!({"expiresMarginMs": -1})),
        base(json!({"accessField": ""})),
        base(json!({"storeAs": {"name": "n", "scope": "world"}})),
        base(json!({"storeAs": {"scope": "user"}})),
        base(json!({"storeAs": {"name": "n", "extra": 1}})),
        base(json!({"extra": 1})),
        base(json!({"body": {"form": {"s": {"credential": "K", "scope": "world"}}}})),
        base(json!({"body": {"form": {"s": {"credential": "K", "extra": 1}}}})),
    ] {
        assert!(matches!(parse(HttpMethod::Post, "/x", json!({"auth": bad.clone()})), Err(HttpRefusal::Invalid(_))), "{bad}");
    }
    assert!(parse(HttpMethod::Post, "/x", json!({"auth": base(json!({"grant": "refresh_token", "body": {"form": {"refresh_token": "rt"}}}))})).is_ok());
    assert!(parse(HttpMethod::Post, "/x", json!({"auth": base(json!({"grant": "authorization_code", "body": {"form": {"code": {"credential": "C"}}}}))})).is_ok());
    assert!(parse(HttpMethod::Post, "/x", json!({"auth": {"scheme": "token-exchange", "tokenUrl": "https://auth.example.com/token"}})).is_ok(), "an empty body is allowed");
}

#[test]
fn credential_slots_accept_the_scope_form_and_pass_the_scope_in_the_ask() {
    let s = spec(json!({"auth": {"scheme": "bearer", "credential": {"credential": "SESSION", "scope": "user"}}}));
    assert_eq!(credentials_needed(&s), vec!["SESSION".to_string()]);
    let (mut d, step) = HttpCallDriver::start(s, egress(), None);
    assert_eq!(step, secret_ask("SESSION", SecretScope::User));
    let req = http_ask(d.resume(ok_secret("sess")));
    assert_eq!(header(&req, "authorization"), Some("Bearer sess"));

    let app = spec(json!({"auth": {"scheme": "bearer", "credential": {"credential": "K", "scope": "app"}}}));
    assert_eq!(HttpCallDriver::start(app, egress(), None).1, secret_ask("K", SecretScope::App));
    for bad in [
        json!({"credential": "K", "scope": "world"}),
        json!({"credential": "K", "extra": 1}),
        json!({"scope": "user"}),
        json!({"credential": 4}),
        json!(5),
    ] {
        assert!(
            matches!(parse(HttpMethod::Post, "/x", json!({"auth": {"scheme": "bearer", "credential": bad.clone()}})), Err(HttpRefusal::Invalid(_))),
            "{bad}"
        );
    }
}

#[test]
fn the_scope_is_part_of_the_ask_wire_and_omitted_for_app() {
    let user = HttpAsk::Secret { name: "N".into(), scope: SecretScope::User };
    assert_eq!(serde_json::to_value(&user).unwrap(), json!({"kind": "secret", "name": "N", "scope": "user"}));
    assert_eq!(serde_json::from_value::<HttpAsk>(json!({"kind": "secret", "name": "N", "scope": "user"})).unwrap(), user);
    let app = HttpAsk::Secret { name: "N".into(), scope: SecretScope::App };
    assert_eq!(serde_json::to_value(&app).unwrap(), json!({"kind": "secret", "name": "N"}));
    assert_eq!(serde_json::from_value::<HttpAsk>(json!({"kind": "secret", "name": "N"})).unwrap(), app);
    let set = HttpAsk::SecretSet { name: "N".into(), value: "v".into(), ttl_ms: 5, scope: SecretScope::User };
    assert_eq!(serde_json::to_value(&set).unwrap(), json!({"kind": "secretSet", "name": "N", "value": "v", "ttlMs": 5, "scope": "user"}));
    assert_eq!(serde_json::from_value::<HttpAsk>(serde_json::to_value(&set).unwrap()).unwrap(), set);
}

#[test]
fn the_delay_is_part_of_the_http_ask_wire_and_omitted_when_absent() {
    let (_, step) = HttpCallDriver::start(spec(json!({})), egress(), None);
    let HttpStep::Ask(ask) = step else { panic!("ask") };
    assert!(serde_json::to_value(&ask).unwrap().get("delayMs").is_none());
    let HttpAsk::Http { request, .. } = ask else { panic!("http") };
    let delayed = HttpAsk::Http { request, delay_ms: Some(250) };
    let wire = serde_json::to_value(&delayed).unwrap();
    assert_eq!(wire["delayMs"], json!(250));
    assert_eq!(serde_json::from_value::<HttpAsk>(wire).unwrap(), delayed);
}

#[test]
fn exchange_credentials_and_tokens_never_reach_the_driver_json_or_debug() {
    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(ok_secret("alice-name-value"));
    http_ask(d.resume(ok_secret("password-value")));
    let ask = d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"minted-token","ttl":60}"#))));
    for shown in [format!("{ask:?}"), format!("{d:?}"), serde_json::to_string(&d).unwrap()] {
        for secret in ["alice-name-value", "password-value", "minted-token"] {
            assert!(!shown.contains(secret), "leaked {secret}: {shown}");
        }
    }
    assert!(serde_json::to_string(&d).unwrap().contains("bank:session"), "control: names are shown");
}
