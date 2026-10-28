use std::net::IpAddr;
use std::time::Duration;

use orbital_core_base::value::Value;
use orbital_core_runtime::http_request::*;
use orbital_core_runtime::http_service::{HttpRequest, HttpResponse, TransportError};
use serde_json::json;


/// The declared shapes the paginated test bodies parse as (a `json/parse` names its type).
const PAGE_SHAPE: &str = "{\"version\":1,\"type\":{\"kind\":\"struct\",\"fields\":[{\"name\":\"items\",\"fieldType\":{\"kind\":\"array\",\"inner\":{\"kind\":\"number\"}},\"required\":false,\"constraints\":[]},{\"name\":\"next\",\"fieldType\":{\"kind\":\"optional\",\"inner\":{\"kind\":\"string\"}},\"required\":false,\"constraints\":[]}]}}";
const LIST_SHAPE: &str = "{\"version\":1,\"type\":{\"kind\":\"array\",\"inner\":{\"kind\":\"number\"}}}";
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

fn no_secrets(_: &CredentialSlot) -> Option<String> {
    None
}

#[test]
fn token_exchange_defaults_isolate_scope_permissions_and_body_order() {
    let mut identities = Vec::new();
    for scope in ["app","user"] {
        for permission in ["calendar","drive"] {
            let auth = json!({"scheme":"token-exchange","tokenUrl":"https://auth.example.com/token","grant":"refresh_token",
                "body":{"form":{"refresh_token":{"credential":"REFRESH","scope":scope},"scope":permission}}});
            let s = spec(json!({"auth":auth}));
            let Some(HttpAuth::TokenExchange(ex)) = &s.auth else { panic!("exchange") };
            assert_eq!(ex.store_as.scope,if scope == "user" {SecretScope::User} else {SecretScope::App});
            let identity = (ex.store_as.name.clone(),ex.store_as.scope);
            assert!(!identities.contains(&identity));
            identities.push(identity);
            let mut reordered = auth.clone();
            reordered["body"]["form"] = json!({"scope":permission,"refresh_token":{"scope":scope,"credential":"REFRESH"}});
            let same = spec(json!({"auth":reordered}));
            assert_eq!(token_cache_name(s.auth.as_ref().unwrap()),token_cache_name(same.auth.as_ref().unwrap()));
            let mut explicit = auth;
            explicit["storeAs"] = json!({"name":"chosen","scope":"app"});
            assert_eq!(parse(HttpMethod::Post,"/",json!({"auth":explicit})).is_err(),scope == "user");
        }
    }
}

fn aws_request(presign: Option<u64>) -> serde_json::Value {
    let mut request = json!({"auth":{"scheme":"aws-sigv4","accessKeyId":"ID","secretAccessKey":"SECRET","sessionToken":"SESSION","region":"us-east-1","service":"s3"},"query":{"q":"a b"}});
    if let Some(expires) = presign { request["presign"] = json!({"expiresSec":expires}); }
    request
}

fn push_request(endpoint: &str) -> serde_json::Value {
    json!({"webpush":{"subscription":{"endpoint":endpoint,"keys":{
        "p256dh":"BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        "auth":"BTBZMqHH6r4Tts7J_aSIgg"}},"payload":"When I grow up, I want to be a watermelon"},
        "auth":{"scheme":"vapid","privateKey":"PRIVATE","publicKey":"BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4","subject":"mailto:owner@example.com"}})
}

#[test]
fn webpush_draws_journaled_entropy_and_signs_vapid_with_the_dispatch_stamp() {
    let s = spec(push_request("https://api.example.com/v1/x"));
    let (mut d, step) = HttpCallDriver::start_stamped(s,egress(),Some("push".into()),Some(1700000000000.0));
    assert_eq!(step,secret_ask("PRIVATE",SecretScope::App));
    let private = codec_private();
    assert_eq!(d.resume(ok_secret(&private)),HttpStep::Ask(HttpAsk::Random {bytes:48}));
    assert_eq!(d.resume(HttpAnswer::Random(Ok(vec![0;48]))),HttpStep::Ask(HttpAsk::Random {bytes:48}));
    let mut entropy = orbital_core_runtime::webpush::decode_base64_any("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw");
    entropy.extend(orbital_core_runtime::webpush::decode_base64_any("DGv6ra1nlYgDCS1FRnbzlw"));
    let request = http_ask(d.resume(HttpAnswer::Random(Ok(entropy))));
    assert_eq!(orbital_core_base::codec::base64url_encode(&request.body),"DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN");
    assert_eq!(header(&request,"ttl"),Some("2419200"));
    assert_eq!(header(&request,"urgency"),Some("normal"));
    assert_eq!(header(&request,"content-encoding"),Some("aes128gcm"));
    let jwt = orbital_core_runtime::webpush::vapid_jwt("https://api.example.com","mailto:owner@example.com",
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",&private,1700043200).unwrap();
    assert!(header(&request,"authorization").unwrap().contains(&jwt));
    assert!(!serde_json::to_string(&d).unwrap().contains(&private));
}

fn codec_private() -> String { orbital_core_base::codec::base64url_encode(&[9;32]) }

#[test]
fn webpush_target_mismatch_invalid_options_and_entropy_errors_never_send() {
    assert!(parse(HttpMethod::Post,"/v1/x",push_request("https://evil.example.com/x")).is_err());
    for option in [json!({"ttl":-1}),json!({"urgency":"immediate"})] {
        let mut request = push_request("https://api.example.com/v1/x");
        request["webpush"].as_object_mut().unwrap().extend(option.as_object().unwrap().clone());
        assert!(parse(HttpMethod::Post,"/v1/x",request).is_err());
    }
    for answer in [HttpAnswer::Random(Ok(vec![1;47])),HttpAnswer::Random(Err("entropy unavailable".into()))] {
        let (mut d,_) = HttpCallDriver::start_stamped(spec(push_request("https://api.example.com/v1/x")),egress(),None,Some(1700000000000.0));
        d.resume(ok_secret(&codec_private()));
        assert!(matches!(d.resume(answer),HttpStep::Done(Err(_))));
    }
}

#[test]
fn legacy_gcm_push_keeps_the_existing_no_vapid_authorization_exception() {
    let origin = "https://android.googleapis.com";
    let path = "/gcm/send/subscription";
    let s = parse_http_call(HttpMethod::Post,origin,path,&val(push_request(&format!("{origin}{path}")))).unwrap();
    let (mut d, _) = HttpCallDriver::start_stamped(s,vec![origin.into()],None,Some(1700000000000.0));
    assert_eq!(d.resume(ok_secret("legacy-unused-private-key")),HttpStep::Ask(HttpAsk::Random {bytes:48}));
    let request = http_ask(d.resume(HttpAnswer::Random(Ok(vec![9;48]))));
    assert_eq!(header(&request,"authorization"),None);
    assert_eq!(header(&request,"content-encoding"),Some("aes128gcm"));
}

#[test]
fn sigv4_and_presign_use_only_the_dispatch_stamp_and_host_held_credentials() {
    for presign in [None, Some(900)] {
        let s = parse(HttpMethod::Get, "/bucket/a%20b", aws_request(presign)).unwrap();
        let (mut d, step) = HttpCallDriver::start_stamped(s, egress(), Some("job".into()), Some(1440938160000.0));
        assert_eq!(step, secret_ask("ID", SecretScope::App));
        assert_eq!(d.resume(ok_secret("AKIDEXAMPLE")), secret_ask("SECRET", SecretScope::App));
        assert_eq!(d.resume(ok_secret("dummy-secret")), secret_ask("SESSION", SecretScope::App));
        let step = d.resume(ok_secret("session-token"));
        let signer = orbital_core_runtime::sigv4::Signer { access_key_id:"AKIDEXAMPLE".into(), secret_access_key:"dummy-secret".into(), region:"us-east-1".into(), service:"s3".into(), session_token:Some("session-token".into()) };
        let query = vec![("q".into(), "a b".into())];
        let target = orbital_core_runtime::sigv4::Target { method:"GET", host:"api.example.com", path:"/bucket/a%20b", query:&query };
        if let Some(expires) = presign {
            let expected = format!("https://api.example.com/bucket/a%20b?{}", signer.presign_query(&target, &expires.to_string(), Some(orbital_core_runtime::sigv4::UNSIGNED_PAYLOAD),1440938160000));
            assert_eq!(step, HttpStep::Done(Ok(val(json!({"url":expected,"key":"job"})))));
            assert!(d.take_secrets().is_empty());
        } else {
            let request = http_ask(step);
            let expected = signer.sign_request(&target, &[], &[], 1440938160000);
            for (name,value) in expected { assert_eq!(header(&request,&name),Some(value.as_str())); }
        }
        assert!(!serde_json::to_string(&d).unwrap().contains("dummy-secret"));
    }
}

#[test]
fn signing_refuses_missing_stamp_and_invalid_presign_contracts() {
    let s = parse(HttpMethod::Get, "/", aws_request(None)).unwrap();
    let (mut d, _) = HttpCallDriver::start(s, egress(), None);
    d.resume(ok_secret("id")); d.resume(ok_secret("secret"));
    let result = d.resume(ok_secret("session"));
    assert!(matches!(result,HttpStep::Done(Err(_))));
    for expires in [0,604801] { assert!(parse(HttpMethod::Get,"/",aws_request(Some(expires))).is_err()); }
    assert!(parse(HttpMethod::Get,"/",json!({"presign":{"expiresSec":900}})).is_err());
    assert!(parse(HttpMethod::Get,"/",json!({"auth":{"scheme":"bearer","credential":"KEY"}})).is_ok());
}

#[test]
fn app_and_user_credentials_with_the_same_name_remain_distinct() {
    for user_name in ["SAME", "OTHER"] {
        let s = spec(json!({"auth":{"scheme":"basic",
            "user":{"credential":"SAME","scope":"app"},
            "credential":{"credential":user_name,"scope":"user"}}}));
        let (mut d, step) = HttpCallDriver::start(s, egress(), None);
        assert_eq!(step, secret_ask("SAME", SecretScope::App));
        assert_eq!(d.resume(ok_secret("APP")), secret_ask(user_name, SecretScope::User));
        let request = http_ask(d.resume(ok_secret("USER")));
        assert_eq!(header(&request, "authorization"), Some("Basic QVBQOlVTRVI="));
    }
}

#[test]
fn auth_headers_refuse_invalid_names_and_secret_values_without_leaking_them() {
    let s = spec(json!({"auth":{"scheme":"bearer","credential":"KEY"}}));
    let injected = "token\r\nInjected: value";
    let result = build_request(&s, &|_| Some(injected.to_string()), &egress(), None);
    let error = result.unwrap_err();
    assert!(!error.message().contains(injected));
    assert!(build_request(&s, &|_| Some("valid-token".to_string()), &egress(), None).is_ok());
    for auth in [
        json!({"scheme":"api-key","credential":"KEY","name":"X:Bad","in":"header"}),
        json!({"scheme":"hmac-sha256","credential":"KEY","name":"X Bad","signs":"data","encoding":"hex"}),
    ] {
        assert!(parse(HttpMethod::Get, "/", json!({"auth":auth})).is_err());
    }
    assert!(parse(HttpMethod::Get, "/", json!({"auth":{"scheme":"api-key","credential":"KEY","name":"X Bad","in":"query"}})).is_ok());
}

#[test]
fn invalid_header_names_and_multiline_values_never_become_requests() {
    for headers in [json!({"X-Test":"line\r\ninjection"}), json!({"X Bad":"value"}), json!({"X:Bad":"value"}), json!({"X-Test":"nul\u{0}value"})] {
        assert!(parse(HttpMethod::Get, "/", json!({"headers":headers})).is_err());
    }
    assert!(parse(HttpMethod::Get, "/", json!({"headers":{"X-Test":"one line"}})).is_ok());
}

#[test]
fn jwt_bearer_signs_credential_issuer_and_authored_claims() {
    use {orbital_core_runtime::jwt::compact, orbital_core_base::jwt_alg::JwtAlg};
    let pem = include_str!("../fixtures/jwt/rs256-pkcs8.pem");
    let credential = json!({"client_email":"service@example.com","private_key":pem}).to_string();
    let claims = json!({"scope":"calendar","aud":"https://tokens.example.com/token","exp":4600,"iat":1000});
    let s = spec(json!({"auth":{
        "scheme":"jwt-bearer","key":"SERVICE_ACCOUNT","alg":"RS256",
        "claims":claims,"tokenUrl":"https://tokens.example.com/token"
    }}));
    let request = token_exchange_request(s.auth.as_ref().unwrap(), &|slot| {
        (slot.name == "SERVICE_ACCOUNT").then(|| credential.clone())
    }).unwrap();
    let expected = compact(JwtAlg::Rs256,
        "{\"iss\":\"service@example.com\",\"scope\":\"calendar\",\"aud\":\"https://tokens.example.com/token\",\"exp\":4600,\"iat\":1000}", pem).unwrap();
    assert_eq!(request.body, orbital_core_base::codec::form_urlencode(&[
        ("grant_type".into(), "urn:ietf:params:oauth:grant-type:jwt-bearer".into()),
        ("assertion".into(), expected),
    ]).into_bytes());
    let missing = token_exchange_request(s.auth.as_ref().unwrap(), &no_secrets);
    assert!(matches!(missing, Err(HttpRefusal::CredentialMissing(name)) if name == "SERVICE_ACCOUNT"));
    let serialized = serde_json::to_string(&s).unwrap();
    assert!(!serialized.contains("service@example.com"));
    assert!(!serialized.contains("PRIVATE KEY"));
}

#[test]
fn jwt_bearer_rejects_claim_issuer_and_invalid_algorithm() {
    for patch in [json!({"iss":"override"}), json!({"alg":"HS256"})] {
        let mut auth = json!({"scheme":"jwt-bearer","key":"SERVICE_ACCOUNT","alg":"RS256",
            "claims":{"aud":"https://tokens.example.com/token","iat":1000,"exp":4600},
            "tokenUrl":"https://tokens.example.com/token"});
        if let Some(issuer) = patch.get("iss") {
            auth["claims"]["iss"] = issuer.clone();
        } else {
            auth["alg"] = patch["alg"].clone();
        }
        assert!(parse(HttpMethod::Get, "/", json!({"auth":auth})).is_err());
    }
}

#[test]
fn jwt_bearer_reuses_token_exchange_cache_margin_and_user_scope() {
    for (alg, pem) in [
        ("RS256", include_str!("../fixtures/jwt/rs256-pkcs8.pem")),
        ("ES256", include_str!("../fixtures/jwt/es256-pkcs8.pem")),
    ] {
        let credential = json!({"client_email":"service@example.com","private_key":pem}).to_string();
        for encoded in [false, true] {
            let s = spec(json!({"auth":{
                "scheme":"jwt-bearer","key":{"credential":"SERVICE_ACCOUNT","scope":"user"},"alg":alg,
                "claims":{"scope":"calendar","aud":"https://auth.example.com/token","exp":4600,"iat":1000},
                "tokenUrl":"https://auth.example.com/token","expiresMarginMs":300000,
                "storeAs":{"name":"google:token","scope":"user"}
            }}));
            let (mut d, step) = HttpCallDriver::start(s.clone(), egress_with_auth(), Some("job".into()));
            assert_eq!(step, secret_ask("google:token", SecretScope::User));
            assert_eq!(d.resume(HttpAnswer::Secret(Ok(None))), secret_ask("SERVICE_ACCOUNT", SecretScope::User));
            let raw = if encoded { orbital_core_base::codec::base64_encode(credential.as_bytes()) } else { credential.clone() };
            let grant = http_ask(d.resume(ok_secret(&raw)));
            assert_eq!(grant.url, "https://auth.example.com/token");
            assert!(grant.text().starts_with("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="));
            for shown in [format!("{d:?}"), serde_json::to_string(&d).unwrap()] {
                assert!(!shown.contains("service@example.com"));
                assert!(!shown.contains("PRIVATE KEY"));
                assert!(!shown.contains(&raw));
            }
            assert_eq!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"access_token":"T1","expires_in":3600}"#)))),
                HttpStep::Ask(HttpAsk::SecretSet {name:"google:token".into(),value:"T1".into(),ttl_ms:3300000,scope:SecretScope::User}));
            let request = http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
            assert_eq!(header(&request, "authorization"), Some("Bearer T1"));
            let (mut cached, _) = HttpCallDriver::start(s, egress_with_auth(), None);
            assert_eq!(header(&http_ask(cached.resume(ok_secret("CACHED"))), "authorization"), Some("Bearer CACHED"));
        }
    }
}

#[test]
fn jwt_bearer_cache_identity_is_permission_bound_not_assertion_time_bound() {
    let cache = |scope: &str, iat: u64| {
        let s = spec(json!({"auth":{"scheme":"jwt-bearer","key":"SERVICE_ACCOUNT","alg":"RS256",
            "claims":{"scope":scope,"aud":"https://auth.example.com/token","iat":iat,"exp":iat+3600},
            "tokenUrl":"https://auth.example.com/token"}}));
        credentials_needed(&s)
    };
    assert_eq!(cache("calendar", 1000), cache("calendar", 1001));
    assert_ne!(cache("calendar", 1000), cache("drive", 1000));
}

#[test]
fn jwt_default_cache_identity_separates_delimiters_and_refresh_policy() {
    let cache = |url: &str, key: &str, margin: u64| {
        let s = spec(json!({"auth":{"scheme":"jwt-bearer","key":key,"alg":"RS256",
            "claims":{"scope":"calendar","aud":"https://auth.example.com/token","iat":1000,"exp":4600},
            "tokenUrl":url,"expiresMarginMs":margin}}));
        token_cache_name(s.auth.as_ref().unwrap())
    };
    let baseline = cache("https://auth.example.com/token|A", "B", 0);
    assert_eq!(baseline, cache("https://auth.example.com/token|A", "B", 0));
    assert_ne!(baseline, cache("https://auth.example.com/token", "A|B", 0));
    assert_ne!(baseline, cache("https://auth.example.com/token|A", "B", 300000));
}

fn vault(slot: &CredentialSlot) -> Option<String> {
    match slot.name.as_str() {
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
    for path in ["", "//evil.com", "@evil.com", "https://evil.com/x", "/a\\b", "evil"] {
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

#[test]
fn hmac_prefix_is_prepended_to_the_header_value_and_absent_stays_bare() {
    let auth = |extra: serde_json::Value| {
        let mut auth = json!({"scheme": "hmac-sha256", "credential": "Jefe", "name": "X-Sig", "signs": "what do ya want for nothing?", "encoding": "hex"});
        auth.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        json!({"auth": auth})
    };
    let hex = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
    let r = build(&spec(auth(json!({"prefix": "sha256="})))).unwrap();
    assert_eq!(header(&r, "x-sig"), Some(format!("sha256={hex}").as_str()));
    let r = build(&spec(auth(json!({})))).unwrap();
    assert_eq!(header(&r, "x-sig"), Some(hex));
    assert!(matches!(
        parse(HttpMethod::Post, "/x", auth(json!({"prefix": 7}))),
        Err(HttpRefusal::Invalid(_))
    ));
}

fn oauth_spec() -> HttpCallSpec {
    spec(json!({"auth": {"scheme": "oauth2-client", "tokenUrl": "https://auth.example.com/token", "clientId": "CID", "clientSecret": "CSEC", "scope": "read write", "onUnauthorized": "refresh"}}))
}

fn oauth_cache_name() -> String {
    token_cache_name(oauth_spec().auth.as_ref().unwrap()).unwrap()
}

#[test]
fn oauth2_token_request_cache_name_and_reply() {
    let s = oauth_spec();
    let auth = s.auth.clone().unwrap();
    assert!(token_cache_name(&auth).unwrap().starts_with("oauth2:https://auth.example.com/token|CID,CSEC|"));
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
fn a_token_reply_accepts_expires_in_as_a_numeric_string() {
    let ttl = |body: &str| parse_token_reply(&response(200, &[], body), "access_token", Some("expires_in"), 0);
    assert_eq!(ttl(r#"{"access_token":"a","expires_in":"3600"}"#), Ok(("a".to_string(), 3_600_000)));
    assert_eq!(ttl(r#"{"access_token":"a","expires_in":3600}"#), Ok(("a".to_string(), 3_600_000)));
    assert!(ttl(r#"{"access_token":"a","expires_in":"36x"}"#).is_err());
    assert!(ttl(r#"{"access_token":"a","expires_in":""}"#).is_err());
    assert!(ttl(r#"{"access_token":"a","expires_in":"-1"}"#).is_err());
    assert!(ttl(r#"{"access_token":"a","expires_in":"inf"}"#).is_err());
    assert!(ttl(r#"{"access_token":"a","expires_in":-1}"#).is_err());
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
    assert_eq!(get(&reply_payload(&b, ResponseAs::Bytes), "body"), Value::string("AAH/"));
}

#[test]
fn failure_and_refusal_payload_shapes() {
    let rest = json!({"phase": null, "grantStatus": null, "grantBody": null, "code": null, "reason": null});
    let with_rest = |mut j: serde_json::Value| {
        j.as_object_mut().unwrap().extend(rest.as_object().unwrap().clone());
        val(j)
    };
    assert_eq!(failure_payload(&TransportError::TimedOut), with_rest(json!({"error": "request timed out", "kind": "timeout", "credential": null})));
    assert_eq!(failure_payload(&TransportError::Failed("boom".into())), with_rest(json!({"error": "boom", "kind": "failed", "credential": null})));
    assert_eq!(failure_payload(&TransportError::Refused("private".into())), with_rest(json!({"error": "private", "kind": "refused", "credential": null})));
    assert_eq!(
        refusal_payload(&HttpRefusal::CredentialMissing("K".into())),
        with_rest(json!({"error": "credential missing: K", "kind": "unconfigured", "credential": "K"}))
    );
    assert_eq!(get(&refusal_payload(&HttpRefusal::EgressDenied("o".into())), "kind"), Value::string("refused"));
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
    assert_eq!(get(&ok, "key"), Value::string("k1"));
    assert_eq!(get(&ok, "body"), Value::string("hi"));

    let (mut d, step) = HttpCallDriver::start(spec(json!({})), egress(), None);
    http_ask(step);
    let err = done(d.resume(HttpAnswer::Http(Err(TransportError::TimedOut)))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::string("timeout"));
    assert_eq!(get(&err, "key"), Value::Null);
}

#[test]
fn driver_refuses_before_any_http_ask() {
    let (_, step) = HttpCallDriver::start(spec(json!({})), vec![], Some("k".into()));
    let err = done(step).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::string("refused"));
    assert_eq!(get(&err, "key"), Value::string("k"));

    let s = spec(json!({"auth": {"scheme": "bearer", "credential": "TOKEN"}}));
    let (mut d, step) = HttpCallDriver::start(s, egress(), None);
    assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "TOKEN".into(), scope: SecretScope::App }));
    let err = done(d.resume(HttpAnswer::Secret(Ok(None)))).unwrap_err();
    assert_eq!(get(&err, "error"), Value::string("credential missing: TOKEN"));
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
    let cache = oauth_cache_name();
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
    assert_eq!(get(&err, "kind"), Value::string("refused"));
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
    assert_eq!(get(&paged, "key"), Value::string("p"));
    assert_eq!(get(&paged, "last.status"), Value::Number(200.0));
}

fn paginated_json(style: &str, extra: serde_json::Value, max: u32) -> HttpCallSpec {
    let body = json!(["json/parse", "@response.body", PAGE_SHAPE]);
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
    let s = spec(json!({"paginate": {"style": "link-header", "collect": ["json/parse", "@response.body", LIST_SHAPE], "max": 5}}));
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
    assert_eq!(get(&err, "kind"), Value::string("refused"));
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
    let s = spec(json!({"auth": {"scheme": "bearer", "credential": "TOKEN"}, "paginate": {"style": "cursor", "param": "after", "next": ["object/get", ["json/parse", "@response.body", PAGE_SHAPE], "next"], "collect": ["object/get", ["json/parse", "@response.body", PAGE_SHAPE], "items"], "max": 3}}));
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
    assert_eq!(get(&err, "kind"), Value::string("failed"));
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
    assert_eq!(get(&err, "kind"), Value::string("refused"));
    assert_eq!(get(&err, "key"), Value::string("k"));
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
            name: oauth_cache_name(),
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
    assert_eq!(get(&err, "kind"), Value::string("unconfigured"));
    assert_eq!(get(&err, "credential"), Value::string(oauth_cache_name()));
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
    assert_eq!(get(&err, "kind"), Value::string("failed"));
    assert_eq!(get(&err, "error"), Value::string("no entropy"));
    assert_eq!(get(&err, "key"), Value::string("k"));

    let (mut d, _) = HttpCallDriver::start(multipart_spec("related"), egress(), None);
    let err = done(d.resume(HttpAnswer::Random(Ok(vec![1, 2, 3])))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::string("failed"));
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
    assert_eq!(get(&last, "key"), Value::string("r"));
}

#[test]
fn delay_ms_is_asked_of_the_host_before_the_first_send_and_only_there() {
    let (mut d, step) = HttpCallDriver::start(spec(json!({"delayMs": 750, "retry": {"max": 1, "on": ["5xx"], "backoffMs": 100}})), egress(), None);
    let (first, delay) = http_ask_delay(step);
    assert_eq!(delay, Some(750));
    // The retry carries its own backoff, not the first send's delay.
    let (again, delay) = http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], "")))));
    assert_eq!((again, delay), (first.clone(), Some(100)));
    // Control: the same request without delayMs asks for no wait, and zero is a wait of zero.
    let (_, none) = http_ask_delay(HttpCallDriver::start(spec(json!({})), egress(), None).1);
    assert_eq!(none, None);
    let (_, zero) = http_ask_delay(HttpCallDriver::start(spec(json!({"delayMs": 0})), egress(), None).1);
    assert_eq!(zero, Some(0));
    let (_, null) = http_ask_delay(HttpCallDriver::start(spec(json!({"delayMs": null})), egress(), None).1);
    assert_eq!(null, None);
}

#[test]
fn a_delayed_page_waits_only_before_the_first_page() {
    let request = json!({"delayMs": 40, "paginate": {"style": "cursor", "param": "after", "next": ["object/get", ["json/parse", "@response.body", PAGE_SHAPE], "next"], "collect": ["object/get", ["json/parse", "@response.body", PAGE_SHAPE], "items"], "max": 3}});
    let (mut d, step) = HttpCallDriver::start(spec(request), egress(), None);
    assert_eq!(http_ask_delay(step).1, Some(40));
    let (_, delay) = http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(200, &[], "{\"items\":[1],\"next\":\"c2\"}")))));
    assert_eq!(delay, None);
}

#[test]
fn delay_ms_must_be_a_whole_number_and_not_ride_a_stream_or_presign() {
    for bad in [json!(-1), json!(1.5), json!("5"), json!(true), json!([1])] {
        assert!(parse(HttpMethod::Post, "/v1/x", json!({"delayMs": bad.clone()})).is_err(), "{bad}");
    }
    assert!(parse(HttpMethod::Post, "/v1/x", json!({"delayMs": 5, "stream": "sse"})).is_err());
    assert!(parse(HttpMethod::Post, "/v1/x", json!({"delayMs": 5})).is_ok());
}

#[test]
fn a_retry_that_succeeds_returns_that_reply() {
    let (mut d, _) = HttpCallDriver::start(retrying(json!({})), egress(), None);
    http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(502, &[], "")))));
    let ok = done(d.resume(HttpAnswer::Http(Ok(response(200, &[], "fine"))))).unwrap();
    assert_eq!(get(&ok, "body"), Value::string("fine"));
}

#[test]
fn a_timeout_is_retried_only_when_listed_and_other_failures_never() {
    let (mut d, _) = HttpCallDriver::start(retrying(json!({})), egress(), None);
    let (_, delay) = http_ask_delay(d.resume(HttpAnswer::Http(Err(TransportError::TimedOut))));
    assert_eq!(delay, Some(100));
    let err = done(d.resume(HttpAnswer::Http(Err(TransportError::Failed("reset".into()))))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::string("failed"));

    let only_5xx = spec(json!({"retry": {"max": 2, "on": ["5xx"], "backoffMs": 100}}));
    let (mut d, _) = HttpCallDriver::start(only_5xx, egress(), None);
    let err = done(d.resume(HttpAnswer::Http(Err(TransportError::TimedOut)))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::string("timeout"));
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
    let body = json!(["json/parse", "@response.body", PAGE_SHAPE]);
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

#[test]
fn retry_on_accepts_exact_status_codes_mixed_with_classes() {
    let codes = spec(json!({"retry": {"max": 2, "on": [500, 502, 503, 504, "timeout"], "backoffMs": 10}}));
    for (status, expected_retry) in [(500, true), (502, true), (503, true), (504, true), (501, false), (505, false), (429, false), (200, false)] {
        let (mut d, _) = HttpCallDriver::start(codes.clone(), egress(), None);
        let step = d.resume(HttpAnswer::Http(Ok(response(status, &[], ""))));
        if expected_retry {
            assert_eq!(http_ask_delay(step).1, Some(10), "{status}");
        } else {
            let reply = done(step).unwrap();
            assert_eq!(get(&reply, "status"), Value::Number(f64::from(status)), "{status}");
        }
    }
    let (mut d, _) = HttpCallDriver::start(codes, egress(), None);
    assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Err(TransportError::TimedOut)))).1, Some(10));

    let mixed = spec(json!({"retry": {"max": 1, "on": [418, "429"], "backoffMs": 7}}));
    for (status, expected_retry) in [(418, true), (429, true), (419, false), (430, false), (500, false)] {
        let (mut d, _) = HttpCallDriver::start(mixed.clone(), egress(), None);
        let step = d.resume(HttpAnswer::Http(Ok(response(status, &[], ""))));
        if expected_retry {
            assert_eq!(http_ask_delay(step).1, Some(7), "{status}");
        } else {
            assert!(done(step).is_ok(), "{status}");
        }
    }
}

#[test]
fn retry_on_refuses_status_codes_outside_100_to_599_or_not_whole() {
    for bad in [99, 600, 0, -500, 1000] {
        let policy = json!({"retry": {"max": 1, "on": [bad], "backoffMs": 1}});
        assert!(matches!(parse(HttpMethod::Post, "/x", policy.clone()), Err(HttpRefusal::Invalid(_))), "{policy}");
    }
    for bad in [json!(500.5), json!(true), json!(null), json!("500x"), json!([500])] {
        let policy = json!({"retry": {"max": 1, "on": [bad.clone()], "backoffMs": 1}});
        assert!(matches!(parse(HttpMethod::Post, "/x", policy), Err(HttpRefusal::Invalid(_))), "{bad}");
    }
    for edge in [100, 599] {
        assert!(parse(HttpMethod::Post, "/x", json!({"retry": {"max": 1, "on": [edge], "backoffMs": 1}})).is_ok(), "{edge}");
    }
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
        &val(json!({"paginate": {"style": "link-header", "collect": ["json/parse", "@response.body", LIST_SHAPE], "max": 5}})),
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
    assert_eq!(get(&payload, "kind"), Value::string("unconfigured"));
    assert_eq!(get(&payload, "credential"), Value::string("SENDGRID_KEY"));
    assert_eq!(get(&payload, "error"), Value::string("credential missing: SENDGRID_KEY"));
    for other in [
        HttpRefusal::EgressDenied("o".into()),
        HttpRefusal::OriginEscape("p".into()),
        HttpRefusal::Invalid("i".into()),
    ] {
        let payload = refusal_payload(&other);
        assert_eq!(get(&payload, "kind"), Value::string("refused"));
        assert_eq!(get(&payload, "credential"), Value::Null);
    }
}

#[test]
fn a_missing_credential_finishes_the_driver_as_unconfigured_with_the_key() {
    let s = spec(json!({"auth": {"scheme": "bearer", "credential": "TOKEN"}}));
    let (mut d, _) = HttpCallDriver::start(s, egress(), Some("k".into()));
    let err = done(d.resume(HttpAnswer::Secret(Ok(None)))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::string("unconfigured"));
    assert_eq!(get(&err, "credential"), Value::string("TOKEN"));
    assert_eq!(get(&err, "key"), Value::string("k"));
}

// --- H.7 P1: token-exchange ---------------------------------------------------------------------------------

fn login_auth() -> serde_json::Value {
    json!({"scheme": "token-exchange", "tokenUrl": "https://auth.example.com/login",
        "body": {"json": {"username": {"credential": "USER"}, "password": {"credential": "PASS"}, "device": "web", "n": 7}},
        "accessField": "token", "expiresField": "ttl", "expiresMarginMs": 1000, "onUnauthorized": "refresh",
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
    let call = spec(json!({"auth": auth}));
    let cache = token_cache_name(call.auth.as_ref().unwrap()).unwrap();
    assert!(cache.starts_with("token-exchange:https://auth.example.com/token|KEY|"));
    let (mut d, step) = HttpCallDriver::start(call, egress_with_auth(), None);
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
    assert_eq!(get(&err, "kind"), Value::string("failed"));
    assert!(matches!(get(&err, "error"), Value::String(m) if m.contains("token")));
}

#[test]
fn a_missing_slot_credential_or_a_refused_exchange_url_ends_the_call() {
    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress_with_auth(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(ok_secret("alice"));
    let err = done(d.resume(HttpAnswer::Secret(Ok(None)))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::string("unconfigured"));
    assert_eq!(get(&err, "credential"), Value::string("PASS"));

    let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress(), None);
    d.resume(HttpAnswer::Secret(Ok(None)));
    d.resume(ok_secret("alice"));
    let err = done(d.resume(ok_secret("s3cret"))).unwrap_err();
    assert_eq!(get(&err, "kind"), Value::string("refused"), "tokenUrl is outside egress");
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
        "onUnauthorized": "refresh", "storeAs": {"name": oauth_cache_name()}}}))
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

mod p11_authority_contract {
    use super::*;

    fn built(origin: &str, allowed: &str) -> Result<HttpRequest, HttpRefusal> {
        parse_http_call(HttpMethod::Get, origin, "/x", &Value::Null)
            .and_then(|spec| build_request(&spec, &no_secrets, &[allowed.to_string()], None))
    }

    #[test]
    fn authority_space_is_refused_before_request_construction() {
        let result = built("https://api example.com/base", "https://api example.com");
        assert!(matches!(result, Err(HttpRefusal::Invalid(_))), "{result:?}");
        assert!(!origin_allowed("https://api example.com/base", &["https://api example.com".into()]));
    }

    #[test]
    fn empty_bracketed_authority_is_refused_before_request_construction() {
        for (origin, allowed) in [
            ("https://[]/base", "https://[]"),
            ("https://[]:443/base", "https://[]:443"),
        ] {
            let result = built(origin, allowed);
            assert!(matches!(result, Err(HttpRefusal::Invalid(_))), "{origin}: {result:?}");
            assert!(!origin_allowed(origin, &[allowed.into()]), "{origin}");
        }
    }

    #[test]
    fn unbracketed_multiple_colons_are_refused_before_request_construction() {
        let origin = "https://2001:db8::1:443/base";
        let allowed = "https://2001:db8::1:443";
        let result = built(origin, allowed);
        assert!(matches!(result, Err(HttpRefusal::Invalid(_))), "{result:?}");
        assert!(!origin_allowed(origin, &[allowed.into()]));
    }

    #[test]
    fn ordinary_default_port_and_bracketed_ipv6_authorities_still_build() {
        for (origin, allowed, expected) in [
            ("https://api.example.com/base", "https://api.example.com", "https://api.example.com/base/x"),
            ("HTTPS://API.example.com:443/base/", "https://api.example.com", "HTTPS://API.example.com:443/base/x"),
            ("http://api.example.com:80/base", "http://api.example.com", "http://api.example.com:80/base/x"),
            ("https://[2001:db8::1]:443/base", "https://[2001:db8::1]", "https://[2001:db8::1]:443/base/x"),
            ("https://[2001:db8::1]:8443/base", "https://[2001:db8::1]:8443", "https://[2001:db8::1]:8443/base/x"),
        ] {
            assert!(origin_allowed(origin, &[allowed.into()]), "{origin}");
            let request = built(origin, allowed).unwrap();
            assert_eq!(request.url, expected, "{origin}");
            assert_eq!(request.method, "GET");
            assert!(request.body.is_empty());
            let spec = prefixed(origin, "/x");
            let (_, step) = HttpCallDriver::start(spec, vec![allowed.into()], None);
            assert_eq!(http_ask(step), request, "{origin}");
        }
    }
}

mod grant_request_contract {
    use super::*;

    fn grant_auths() -> [serde_json::Value; 3] {
        [
            login_auth(),
            json!({"scheme":"oauth2-client","tokenUrl":"https://auth.example.com/token",
                "clientId":"CID","clientSecret":"CSEC","scope":"read write"}),
            json!({"scheme":"jwt-bearer","tokenUrl":"https://auth.example.com/token",
                "key":"SERVICE_ACCOUNT","alg":"RS256",
                "claims":{"scope":"calendar","aud":"https://auth.example.com/token","iat":1000,"exp":4600}}),
        ]
    }

    #[test]
    fn grant_request_headers_deadlines_and_old_defaults_reach_each_token_lowering() {
        let credential = json!({"client_email":"service@example.com",
            "private_key":include_str!("../fixtures/jwt/rs256-pkcs8.pem")}).to_string();
        let secrets = |slot: &CredentialSlot| {
            if slot.name == "SERVICE_ACCOUNT" { Some(credential.clone()) } else { vault(slot) }
        };
        for auth in grant_auths() {
            let baseline = spec(json!({"auth":auth.clone()}));
            let baseline = token_exchange_request(baseline.auth.as_ref().unwrap(), &secrets).unwrap();
            assert_eq!(baseline.timeout, None);
            assert_eq!(header(&baseline, "X-Grant"), None);
            for (options, deadline) in [
                (json!({}), None),
                (json!({"timeoutMs":null}), None),
                (json!({"timeoutMs":0}), Some(Duration::ZERO)),
                (json!({"timeoutMs":15000,"headers":{"X-Grant":"grant-only"}}),
                    Some(Duration::from_millis(15000))),
            ] {
                let mut configured = auth.clone();
                configured["grantRequest"] = options.clone();
                let call = spec(json!({"auth":configured}));
                let request = token_exchange_request(call.auth.as_ref().unwrap(), &secrets).unwrap();
                assert_eq!(request.method, baseline.method);
                assert_eq!(request.url, baseline.url);
                assert_eq!(request.body, baseline.body);
                assert_eq!(request.timeout, deadline, "{auth}: {options}");
                assert_eq!(header(&request, "content-type"), header(&baseline, "content-type"));
                assert_eq!(header(&request, "authorization"), header(&baseline, "authorization"));
                assert_eq!(header(&request, "X-Grant"),
                    options.get("headers").map(|_| "grant-only"));
            }
        }
    }

    #[test]
    fn grant_request_headers_preserve_authored_order_for_basic_post_and_json_grants() {
        for mut auth in [login_auth(), refresh_auth("basic"), refresh_auth("post")] {
            auth["grantRequest"] = json!({"headers":{
                "X-Zeta":"first","x-alpha":"second","X-Middle":"third"}});
            let call = spec(json!({"auth":auth.clone()}));
            let request = token_exchange_request(call.auth.as_ref().unwrap(), &|slot| {
                if slot.name == "RT" { Some("rt-1".into()) } else { vault(slot) }
            }).unwrap();
            let authored: Vec<_> = request.headers.iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("authorization")
                    && !name.eq_ignore_ascii_case("content-type"))
                .map(|(name, value)| (name.as_str(), value.as_str())).collect();
            assert_eq!(authored, vec![("X-Zeta","first"),("x-alpha","second"),("X-Middle","third")]);
            assert_eq!(request.headers.iter().filter(|(name, _)| name.eq_ignore_ascii_case("content-type")).count(), 1);
            let basic = auth.get("clientAuth").and_then(serde_json::Value::as_str) == Some("basic");
            assert_eq!(header(&request, "authorization"), basic.then_some("Basic Y2xpZW50LTE6c2ho"));
            if auth.get("clientAuth").and_then(serde_json::Value::as_str) == Some("post") {
                assert_eq!(request.text(), "grant_type=refresh_token&refresh_token=rt-1&client_id=client-1&client_secret=shh");
            }
        }
    }

    #[test]
    fn grant_request_shape_is_closed_and_deadlines_are_whole_nonnegative_or_null() {
        assert!(matches!(parse(HttpMethod::Post, "/x", json!({"grantRequest":{}})), Err(HttpRefusal::Invalid(_))));
        for auth in grant_auths() {
            for options in [
                json!(null), json!(3), json!([]), json!("request"),
                json!({"extra":true}), json!({"json":{}}), json!({"query":{}}),
                json!({"timeoutMs":-1}), json!({"timeoutMs":0.5}),
                json!({"timeoutMs":"15000"}), json!({"timeoutMs":true}),
                json!({"timeoutMs":[]}), json!({"timeoutMs":{}}),
                json!({"headers":null}), json!({"retry":null}),
            ] {
                let mut invalid_auth = auth.clone();
                invalid_auth["grantRequest"] = options.clone();
                assert!(matches!(parse(HttpMethod::Post, "/x", json!({"auth":invalid_auth})),
                    Err(HttpRefusal::Invalid(_))), "{auth}: {options}");
            }
            for options in [json!({}), json!({"timeoutMs":null}), json!({"timeoutMs":0}),
                json!({"timeoutMs":15000}), json!({"headers":{}})] {
                let mut valid_auth = auth.clone();
                valid_auth["grantRequest"] = options.clone();
                assert!(parse(HttpMethod::Post, "/x", json!({"auth":valid_auth})).is_ok(),
                    "{auth}: {options}");
            }
        }
    }

    #[test]
    fn grant_request_headers_refuse_owned_names_invalid_syntax_and_secret_values() {
        for auth in grant_auths() {
            for headers in [
                json!({"Authorization":"literal"}), json!({"aUtHoRiZaTiOn":"literal"}),
                json!({"Content-Type":"application/json"}), json!({"cOnTeNt-TyPe":"literal"}),
                json!({"X Bad":"value"}), json!({"X:Bad":"value"}), json!({"":"value"}),
                json!({"X-Test":"first\r\nInjected: second"}), json!({"X-Test":"line\nline"}),
                json!({"X-Test":"nul\u{0}value"}), json!({"X-Test":5}), json!({"X-Test":true}),
                json!({"X-Test":null}), json!({"X-Test":{"credential":"PASS"}}),
                json!({"X-Test":["secret","PASS"]}), json!(["X-Test","value"]),
            ] {
                let mut invalid_auth = auth.clone();
                invalid_auth["grantRequest"] = json!({"headers":headers.clone()});
                assert!(matches!(parse(HttpMethod::Post, "/x", json!({"auth":invalid_auth})),
                    Err(HttpRefusal::Invalid(_))), "{auth}: {headers}");
            }
            let mut valid_auth = auth;
            valid_auth["grantRequest"] = json!({"headers":{"X-Grant":"one line","X-Empty":""}});
            assert!(parse(HttpMethod::Post, "/x", json!({"auth":valid_auth})).is_ok());
        }
    }

    #[test]
    fn retry_schedule_and_no_response_parse_rules_are_shared() {
        for policy in [
            json!({"max":3,"on":["1xx","408","5xx","429","timeout","failed"],"scheduleMs":[100,500,1500],"noResponseMax":2}),
            json!({"max":1,"on":["failed","failed"],"scheduleMs":[0],"noResponseMax":0}),
            json!({"max":2,"on":["timeout"],"backoffMs":0,"noResponseMax":2}),
            json!({"max":1,"on":["5xx"],"backoffMs":7}),
        ] {
            assert!(parse(HttpMethod::Post, "/x", json!({"retry":policy.clone()})).is_ok(), "{policy}");
            for mut auth in grant_auths() {
                auth["grantRequest"] = json!({"retry":policy.clone()});
                assert!(parse(HttpMethod::Post, "/x", json!({"auth":auth})).is_ok(), "{policy}");
            }
        }
        for policy in [
            json!({"max":1,"on":["5xx"]}),
            json!({"max":1,"on":["5xx"],"backoffMs":1,"scheduleMs":[1]}),
            json!({"max":1,"on":["5xx"],"backoffMs":null,"scheduleMs":[1]}),
            json!({"max":1,"on":["5xx"],"scheduleMs":[]}),
            json!({"max":1,"on":["5xx"],"scheduleMs":[1,2]}),
            json!({"max":2,"on":["5xx"],"scheduleMs":[1]}),
            json!({"max":1,"on":["5xx"],"scheduleMs":[-1]}),
            json!({"max":1,"on":["5xx"],"scheduleMs":[0.5]}),
            json!({"max":1,"on":["5xx"],"scheduleMs":[null]}),
            json!({"max":1,"on":["5xx"],"scheduleMs":["1"]}),
            json!({"max":1,"on":["5xx"],"scheduleMs":null}),
            json!({"max":1,"on":["5xx"],"scheduleMs":1}),
            json!({"max":1,"on":["5xx"],"scheduleMs":{}}),
            json!({"max":1,"on":["5xx"],"scheduleMs":true}),
            json!({"max":1,"on":["5xx"],"backoffMs":-1}),
            json!({"max":1,"on":["5xx"],"backoffMs":0.5}),
            json!({"max":1,"on":["5xx"],"backoffMs":1,"noResponseMax":-1}),
            json!({"max":1,"on":["5xx"],"backoffMs":1,"noResponseMax":0.5}),
            json!({"max":1,"on":["5xx"],"backoffMs":1,"noResponseMax":2}),
            json!({"max":1,"on":["5xx"],"backoffMs":1,"noResponseMax":null}),
            json!({"max":1,"on":["5xx"],"backoffMs":1,"noResponseMax":"0"}),
            json!({"max":1,"on":["5xx"],"backoffMs":1,"noResponseMax":false}),
            json!({"max":1,"on":["5xx"],"backoffMs":1,"noResponseMax":[]}),
            json!({"max":1,"on":["5xx"],"backoffMs":1,"noResponseMax":{}}),
            json!({"max":0,"on":["5xx"],"scheduleMs":[]}),
            json!({"max":1.5,"on":["5xx"],"scheduleMs":[1]}),
            json!({"max":4294967296u64,"on":["5xx"],"backoffMs":1}),
            json!({"max":1,"on":[],"scheduleMs":[1]}),
            json!({"max":1,"on":["4xx"],"scheduleMs":[1]}),
            json!({"max":1,"on":["FAILED"],"scheduleMs":[1]}),
            json!({"max":1,"on":[99],"scheduleMs":[1]}),
            json!({"max":1,"on":["failed"],"scheduleMs":[1],"extra":true}),
        ] {
            assert!(matches!(parse(HttpMethod::Post, "/x", json!({"retry":policy.clone()})),
                Err(HttpRefusal::Invalid(_))), "resource: {policy}");
            let mut auth = login_auth();
            auth["grantRequest"] = json!({"retry":policy.clone()});
            assert!(matches!(parse(HttpMethod::Post, "/x", json!({"auth":auth})),
                Err(HttpRefusal::Invalid(_))), "grant: {policy}");
        }
    }

    #[test]
    fn retry_new_status_tags_match_only_their_exact_ranges() {
        for (tag, statuses) in [
            ("1xx", vec![(99,false),(100,true),(199,true),(200,false),(408,false)]),
            ("408", vec![(407,false),(408,true),(409,false),(429,false)]),
            ("5xx", vec![(499,false),(500,true),(599,true),(600,false)]),
            ("429", vec![(428,false),(429,true),(430,false),(408,false)]),
        ] {
            for (status, expected_retry) in statuses {
                for grant in [false, true] {
                    let policy = json!({"max":1,"on":[tag],"backoffMs":9});
                    let request = if grant {
                        let mut auth = login_auth();
                        auth["grantRequest"] = json!({"retry":policy});
                        json!({"auth":auth})
                    } else { json!({"retry":policy}) };
                    let (mut d, first) = HttpCallDriver::start(spec(request), egress_with_auth(), None);
                    let first = if grant {
                        assert_eq!(first, secret_ask("bank:session", SecretScope::User));
                        assert_eq!(d.resume(HttpAnswer::Secret(Ok(None))), secret_ask("USER", SecretScope::App));
                        assert_eq!(d.resume(ok_secret("alice")), secret_ask("PASS", SecretScope::App));
                        http_ask(d.resume(ok_secret("s3cret")))
                    } else { http_ask(first) };
                    let step = d.resume(HttpAnswer::Http(Ok(response(status, &[], "reply"))));
                    let settled = if expected_retry {
                        assert_eq!(http_ask_delay(step), (first, Some(9)), "{tag}: {status}, grant={grant}");
                        d.resume(HttpAnswer::Http(Ok(response(status, &[], "final"))))
                    } else { step };
                    if grant {
                        assert!(matches!(settled, HttpStep::Done(Err(_))), "{tag}: {status}");
                    } else {
                        let reply = done(settled).unwrap();
                        assert_eq!(get(&reply, "status"), Value::Number(f64::from(status)), "{tag}: {status}");
                    }
                }
            }
        }
    }

    #[test]
    fn transport_failed_is_explicit_and_never_means_credential_or_refusal_failure() {
        let policy = json!({"max":1,"on":["failed"],"backoffMs":11});
        let (mut d, step) = HttpCallDriver::start(spec(json!({"retry":policy.clone()})), egress(), None);
        let request = http_ask(step);
        assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Err(TransportError::Failed("reset".into()))))),
            (request, Some(11)));
        assert_eq!(get(&done(d.resume(HttpAnswer::Http(Err(TransportError::Failed("reset again".into()))))).unwrap_err(), "kind"),
            Value::string("failed"));
        for (error, kind) in [(TransportError::TimedOut, "timeout"),
            (TransportError::Refused("host refusal".into()), "refused")] {
            let (mut d, _) = HttpCallDriver::start(spec(json!({"retry":policy.clone()})), egress(), None);
            assert_eq!(get(&done(d.resume(HttpAnswer::Http(Err(error)))).unwrap_err(), "kind"),
                Value::string(kind));
        }
        for answer in [HttpAnswer::Secret(Err("vault unavailable".into())), HttpAnswer::Secret(Ok(None))] {
            let (mut d, step) = HttpCallDriver::start(spec(json!({"auth":{"scheme":"bearer","credential":"TOKEN"},
                "retry":policy.clone()})), egress(), None);
            assert_eq!(step, secret_ask("TOKEN", SecretScope::App));
            assert!(matches!(d.resume(answer), HttpStep::Done(Err(_))));
        }
        let (mut d, _) = HttpCallDriver::start(retrying(json!({})), egress(), None);
        assert!(matches!(d.resume(HttpAnswer::Http(Err(TransportError::Failed("reset".into())))), HttpStep::Done(Err(_))));
    }

    #[test]
    fn scheduled_resource_retries_follow_exact_delays_and_stop_at_max() {
        let call = spec(json!({"text":"identical resource bytes","headers":{"X-Resource":"public"},
            "retry":{"max":3,"on":["5xx","408","429"],"scheduleMs":[100,500,1500]}}));
        let (mut d, step) = HttpCallDriver::start(call, egress(), Some("scheduled".into()));
        let (request, delay) = http_ask_delay(step);
        assert_eq!(delay, None);
        for (status, expected_delay) in [(503,100),(408,500),(429,1500)] {
            assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(status, &[], "retry"))))),
                (request.clone(), Some(expected_delay)));
        }
        let reply = done(d.resume(HttpAnswer::Http(Ok(response(500, &[], "exhausted"))))).unwrap();
        assert_eq!(get(&reply, "status"), Value::Number(500.0));
        assert_eq!(get(&reply, "key"), Value::string("scheduled"));
        for policy in [json!({"max":1,"on":["5xx"],"scheduleMs":[0]}),
            json!({"max":1,"on":["5xx"],"backoffMs":0})] {
            let (mut d, step) = HttpCallDriver::start(spec(json!({"retry":policy})), egress(), None);
            let request = http_ask(step);
            assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (request, Some(0)));
            assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(503, &[], "")))), HttpStep::Done(Ok(_))));
        }
    }

    #[test]
    fn no_response_max_uses_shared_attempt_index_not_an_independent_counter() {
        for first_status in [false, true] {
            for terminal in [TransportError::TimedOut, TransportError::Failed("last reset".into())] {
                let call = spec(json!({"retry":{"max":3,"on":["5xx","failed","timeout"],
                    "scheduleMs":[100,500,1500],"noResponseMax":2}}));
                let (mut d, _) = HttpCallDriver::start(call, egress(), None);
                for (status, delay) in [(first_status,100),(!first_status,500)] {
                    let answer = if status { HttpAnswer::Http(Ok(response(503, &[], ""))) }
                        else { HttpAnswer::Http(Err(TransportError::Failed("reset".into()))) };
                    assert_eq!(http_ask_delay(d.resume(answer)).1, Some(delay));
                }
                let expected = if terminal == TransportError::TimedOut { "timeout" } else { "failed" };
                let err = done(d.resume(HttpAnswer::Http(Err(terminal)))).unwrap_err();
                assert_eq!(get(&err, "kind"), Value::string(expected));
            }
            let call = spec(json!({"retry":{"max":3,"on":["5xx","failed","timeout"],
                "scheduleMs":[100,500,1500],"noResponseMax":2}}));
            let (mut d, _) = HttpCallDriver::start(call, egress(), None);
            for (status, delay) in [(first_status,100),(!first_status,500),(true,1500)] {
                let answer = if status { HttpAnswer::Http(Ok(response(503, &[], ""))) }
                    else { HttpAnswer::Http(Err(TransportError::Failed("reset".into()))) };
                assert_eq!(http_ask_delay(d.resume(answer)).1, Some(delay));
            }
            assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(503, &[], "")))), HttpStep::Done(Ok(_))));
        }
    }

    #[test]
    fn zero_no_response_max_does_not_consume_or_disable_status_retries() {
        let request = json!({"retry":{"max":2,"on":["5xx","failed","timeout"],
            "scheduleMs":[3,4],"noResponseMax":0}});
        for error in [TransportError::TimedOut, TransportError::Failed("reset".into())] {
            let (mut d, _) = HttpCallDriver::start(spec(request.clone()), egress(), None);
            assert!(matches!(d.resume(HttpAnswer::Http(Err(error))), HttpStep::Done(Err(_))));
        }
        let (mut d, _) = HttpCallDriver::start(spec(request), egress(), None);
        for delay in [3,4] {
            assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))).1, Some(delay));
        }
        assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(503, &[], "")))), HttpStep::Done(Ok(_))));
    }

    #[test]
    fn missing_no_response_max_preserves_full_transport_budget() {
        for error in [TransportError::TimedOut, TransportError::Failed("reset".into())] {
            for explicit_max in [false, true] {
                let mut policy = json!({"max":2,"on":["failed","timeout"],"scheduleMs":[3,4]});
                if explicit_max { policy["noResponseMax"] = json!(2); }
                let (mut d, step) = HttpCallDriver::start(spec(json!({"retry":policy})), egress(), None);
                let request = http_ask(step);
                for delay in [3,4] {
                    assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Err(error.clone())))),
                        (request.clone(), Some(delay)));
                }
                assert!(matches!(d.resume(HttpAnswer::Http(Err(error.clone()))), HttpStep::Done(Err(_))));
            }
        }
    }

    #[test]
    fn grant_retries_reuse_the_exact_post_and_never_store_or_send_a_resource_early() {
        let mut auth = login_auth();
        auth["grantRequest"] = json!({"headers":{"X-Grant":"grant-only"},"timeoutMs":15000,
            "retry":{"max":3,"on":["1xx","408","5xx","429","failed","timeout"],
                "noResponseMax":2,"scheduleMs":[100,500,1500]}});
        let (mut d, step) = HttpCallDriver::start(spec(json!({"auth":auth,"timeoutMs":20000,
            "headers":{"X-Resource":"resource-only"}})), egress_with_auth(), Some("grant-job".into()));
        assert_eq!(step, secret_ask("bank:session", SecretScope::User));
        assert_eq!(d.resume(HttpAnswer::Secret(Ok(None))), secret_ask("USER", SecretScope::App));
        assert_eq!(d.resume(ok_secret("alice")), secret_ask("PASS", SecretScope::App));
        let (grant, delay) = http_ask_delay(d.resume(ok_secret("s3cret")));
        assert_eq!(delay, None);
        assert_eq!(grant.url, "https://auth.example.com/login");
        assert_eq!(grant.timeout, Some(Duration::from_millis(15000)));
        assert_eq!(header(&grant, "X-Grant"), Some("grant-only"));
        assert_eq!(header(&grant, "X-Resource"), None);
        for (answer, delay) in [
            (HttpAnswer::Http(Ok(response(503, &[], "temporary"))),100),
            (HttpAnswer::Http(Err(TransportError::Failed("reset".into()))),500),
            (HttpAnswer::Http(Ok(response(408, &[], "temporary"))),1500),
        ] {
            assert_eq!(http_ask_delay(d.resume(answer)), (grant.clone(), Some(delay)));
        }
        assert_eq!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"T1","ttl":60}"#)))),
            HttpStep::Ask(HttpAsk::SecretSet { name:"bank:session".into(), value:"T1".into(),
                ttl_ms:59000, scope:SecretScope::User }));
        let (resource, delay) = http_ask_delay(d.resume(HttpAnswer::SecretSet(Ok(()))));
        assert_eq!(delay, None);
        assert_eq!(resource.url, "https://api.example.com/v1/x");
        assert_eq!(resource.timeout, Some(Duration::from_millis(20000)));
        assert_eq!(header(&resource, "authorization"), Some("Bearer T1"));
        assert_eq!(header(&resource, "X-Resource"), Some("resource-only"));
        assert_eq!(header(&resource, "X-Grant"), None);
        assert_eq!(get(&done(d.resume(HttpAnswer::Http(Ok(response(200, &[], "done"))))).unwrap(), "key"),
            Value::string("grant-job"));
    }

    #[test]
    fn grant_retry_exhaustion_returns_one_existing_failure_without_a_token_write() {
        for (answer, kind) in [
            (HttpAnswer::Http(Ok(response(503, &[], "vendor failure"))), "failed"),
            (HttpAnswer::Http(Err(TransportError::Failed("reset".into()))), "failed"),
            (HttpAnswer::Http(Err(TransportError::TimedOut)), "timeout"),
        ] {
            let mut auth = login_auth();
            auth["grantRequest"] = json!({"retry":{"max":2,"on":["5xx","failed","timeout"],"backoffMs":25}});
            let (mut d, _) = HttpCallDriver::start(spec(json!({"auth":auth})), egress_with_auth(), Some("failed-grant".into()));
            d.resume(HttpAnswer::Secret(Ok(None)));
            d.resume(ok_secret("alice"));
            let grant = http_ask(d.resume(ok_secret("s3cret")));
            for delay in [25,50] {
                assert_eq!(http_ask_delay(d.resume(answer.clone())), (grant.clone(), Some(delay)));
            }
            let err = done(d.resume(answer)).unwrap_err();
            assert_eq!(get(&err, "kind"), Value::string(kind));
            assert_eq!(get(&err, "key"), Value::string("failed-grant"));
            assert!(d.take_secrets().is_empty());
        }
    }

    #[test]
    fn grant_no_response_cap_uses_the_same_shared_index_as_resource_retries() {
        for first_status in [false, true] {
            for final_status in [false, true] {
                let mut auth = login_auth();
                auth["grantRequest"] = json!({"retry":{"max":3,"on":["5xx","failed","timeout"],
                    "scheduleMs":[100,500,1500],"noResponseMax":2}});
                let (mut d, _) = HttpCallDriver::start(spec(json!({"auth":auth})), egress_with_auth(), None);
                d.resume(HttpAnswer::Secret(Ok(None)));
                d.resume(ok_secret("alice"));
                let grant = http_ask(d.resume(ok_secret("s3cret")));
                for (status, delay) in [(first_status,100),(!first_status,500)] {
                    let answer = if status { HttpAnswer::Http(Ok(response(503, &[], ""))) }
                        else { HttpAnswer::Http(Err(TransportError::Failed("reset".into()))) };
                    assert_eq!(http_ask_delay(d.resume(answer)), (grant.clone(), Some(delay)));
                }
                if final_status {
                    assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (grant, Some(1500)));
                    assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"T1","ttl":60}"#)))),
                        HttpStep::Ask(HttpAsk::SecretSet { .. })));
                } else {
                    let err = done(d.resume(HttpAnswer::Http(Err(TransportError::TimedOut)))).unwrap_err();
                    assert_eq!(get(&err, "kind"), Value::string("timeout"));
                    assert!(d.take_secrets().is_empty());
                }
            }
        }
    }

    #[test]
    fn jwt_grant_retries_preserve_assertion_headers_dispatch_stamp_and_secret_privacy() {
        for (alg, pem) in [
            ("RS256", include_str!("../fixtures/jwt/rs256-pkcs8.pem")),
            ("ES256", include_str!("../fixtures/jwt/es256-pkcs8.pem")),
        ] {
            let credential = json!({"client_email":"private-service@example.com","private_key":pem}).to_string();
            let call = spec(json!({"auth":{"scheme":"jwt-bearer",
                "key":{"credential":"SERVICE_ACCOUNT","scope":"user"},"alg":alg,
                "claims":{"scope":"calendar","aud":"https://auth.example.com/token","iat":1000,"exp":4600},
                "tokenUrl":"https://auth.example.com/token","expiresMarginMs":300000,
                "storeAs":{"name":"jwt:access","scope":"user"},
                "grantRequest":{"headers":{"X-Grant":"jwt-only"},"timeoutMs":15000,
                    "retry":{"max":2,"on":["failed","5xx"],"scheduleMs":[3,5]}}},"timeoutMs":20000}));
            let (mut d, step) = HttpCallDriver::start_stamped(call, egress_with_auth(), None, Some(1700000000000.0));
            assert_eq!(step, secret_ask("jwt:access", SecretScope::User));
            assert_eq!(d.resume(HttpAnswer::Secret(Ok(None))), secret_ask("SERVICE_ACCOUNT", SecretScope::User));
            let (grant, delay) = http_ask_delay(d.resume(ok_secret(&credential)));
            assert_eq!(delay, None);
            assert!(grant.text().starts_with("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="));
            assert_eq!(grant.timeout, Some(Duration::from_millis(15000)));
            assert_eq!(header(&grant, "X-Grant"), Some("jwt-only"));
            let grant_text = grant.text();
            for (answer, delay) in [(HttpAnswer::Http(Err(TransportError::Failed("reset".into()))),3),
                (HttpAnswer::Http(Ok(response(503, &[], "retry"))),5)] {
                assert_eq!(http_ask_delay(d.resume(answer)), (grant.clone(), Some(delay)));
                let snapshot = serde_json::to_string(&d).unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&snapshot).unwrap()["nowMs"], json!(1700000000000.0));
                for shown in [snapshot, format!("{d:?}"), format!("{grant:?}")] {
                    for secret in [credential.as_str(), pem, "private-service@example.com", grant_text.as_ref()] {
                        assert!(!shown.contains(secret), "{alg}: secret in driver/debug");
                    }
                }
            }
            assert_eq!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"access_token":"JWT-AT","expires_in":3600}"#)))),
                HttpStep::Ask(HttpAsk::SecretSet { name:"jwt:access".into(), value:"JWT-AT".into(),
                    ttl_ms:3300000, scope:SecretScope::User }));
            let resource = http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
            assert_eq!(header(&resource, "authorization"), Some("Bearer JWT-AT"));
            assert_eq!(header(&resource, "X-Grant"), None);
            assert_eq!(resource.timeout, Some(Duration::from_millis(20000)));
        }
    }

    #[test]
    fn resource_options_never_leak_into_an_unconfigured_grant() {
        for fail_grant in [false, true] {
            let call = spec(json!({"auth":login_auth(),"headers":{"X-Resource":"resource-only","Content-Type":"resource/type"},
                "text":"resource bytes","timeoutMs":20000,"retry":{"max":1,"on":["5xx"],"backoffMs":7}}));
            let (mut d, _) = HttpCallDriver::start(call, egress_with_auth(), None);
            d.resume(HttpAnswer::Secret(Ok(None)));
            d.resume(ok_secret("alice"));
            let grant = http_ask(d.resume(ok_secret("s3cret")));
            assert_eq!(header(&grant, "X-Resource"), None);
            assert_eq!(header(&grant, "content-type"), Some("application/json"));
            assert_eq!(grant.timeout, None);
            if fail_grant {
                assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(503, &[], "no inherited retry")))), HttpStep::Done(Err(_))));
            } else {
                assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"T1","ttl":60}"#)))),
                    HttpStep::Ask(HttpAsk::SecretSet { .. })));
                let resource = http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
                assert_eq!(header(&resource, "X-Resource"), Some("resource-only"));
                assert_eq!(header(&resource, "content-type"), Some("resource/type"));
                assert_eq!(resource.timeout, Some(Duration::from_millis(20000)));
                assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (resource, Some(7)));
            }
        }
    }

    #[test]
    fn grant_options_never_leak_into_resource_retries_or_cached_calls() {
        for cached in [false, true] {
            let mut auth = login_auth();
            auth["grantRequest"] = json!({"headers":{"X-Grant":"grant-only"},"timeoutMs":15000,
                "retry":{"max":1,"on":["5xx"],"scheduleMs":[100]}});
            let (mut d, step) = HttpCallDriver::start(spec(json!({"auth":auth})), egress_with_auth(), None);
            assert_eq!(step, secret_ask("bank:session", SecretScope::User));
            let resource = if cached {
                http_ask(d.resume(ok_secret("CACHED")))
            } else {
                assert_eq!(d.resume(HttpAnswer::Secret(Ok(None))), secret_ask("USER", SecretScope::App));
                assert_eq!(d.resume(ok_secret("alice")), secret_ask("PASS", SecretScope::App));
                let grant = http_ask(d.resume(ok_secret("s3cret")));
                assert_eq!(header(&grant, "X-Grant"), Some("grant-only"));
                assert_eq!(grant.timeout, Some(Duration::from_millis(15000)));
                assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"T1","ttl":60}"#)))),
                    HttpStep::Ask(HttpAsk::SecretSet { .. })));
                http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))))
            };
            assert_eq!(resource.timeout, None);
            assert_eq!(header(&resource, "X-Grant"), None);
            assert_eq!(header(&resource, "authorization"), Some(if cached { "Bearer CACHED" } else { "Bearer T1" }));
            let reply = done(d.resume(HttpAnswer::Http(Ok(response(503, &[], "no inherited retry"))))).unwrap();
            assert_eq!(get(&reply, "status"), Value::Number(503.0));
        }
    }

    #[test]
    fn grant_and_resource_retries_have_independent_attempt_indexes() {
        let mut auth = login_auth();
        auth["grantRequest"] = json!({"retry":{"max":2,"on":["5xx"],"backoffMs":100}});
        let call = spec(json!({"auth":auth,"retry":{"max":1,"on":["5xx"],"backoffMs":7}}));
        let (mut d, _) = HttpCallDriver::start(call, egress_with_auth(), None);
        d.resume(HttpAnswer::Secret(Ok(None)));
        d.resume(ok_secret("alice"));
        let grant = http_ask(d.resume(ok_secret("s3cret")));
        for delay in [100,200] {
            assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (grant.clone(), Some(delay)));
        }
        assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"T1","ttl":60}"#)))),
            HttpStep::Ask(HttpAsk::SecretSet { .. })));
        let (resource, delay) = http_ask_delay(d.resume(HttpAnswer::SecretSet(Ok(()))));
        assert_eq!(delay, None);
        assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (resource, Some(7)));
        let reply = done(d.resume(HttpAnswer::Http(Ok(response(503, &[], "exhausted"))))).unwrap();
        assert_eq!(get(&reply, "status"), Value::Number(503.0));
    }

    #[test]
    fn a_401_refresh_gets_a_fresh_grant_budget_but_does_not_reset_resource_budget() {
        for final_status in [503,401] {
            let mut auth = login_auth();
            auth["grantRequest"] = json!({"retry":{"max":1,"on":["5xx"],"scheduleMs":[100]}});
            let call = spec(json!({"auth":auth,"retry":{"max":1,"on":["5xx"],"scheduleMs":[7]}}));
            let (mut d, _) = HttpCallDriver::start(call, egress_with_auth(), None);
            d.resume(HttpAnswer::Secret(Ok(None)));
            d.resume(ok_secret("alice"));
            let grant = http_ask(d.resume(ok_secret("s3cret")));
            assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (grant.clone(), Some(100)));
            d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"OLD","ttl":60}"#))));
            let resource = http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
            assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (resource, Some(7)));
            assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(401, &[], "expired"))))), (grant.clone(), None));
            assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (grant, Some(100)));
            assert_eq!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"NEW","ttl":60}"#)))),
                HttpStep::Ask(HttpAsk::SecretSet { name:"bank:session".into(), value:"NEW".into(),
                    ttl_ms:59000, scope:SecretScope::User }));
            let (resource, delay) = http_ask_delay(d.resume(HttpAnswer::SecretSet(Ok(()))));
            assert_eq!(delay, None);
            assert_eq!(header(&resource, "authorization"), Some("Bearer NEW"));
            let reply = done(d.resume(HttpAnswer::Http(Ok(response(final_status, &[], "final"))))).unwrap();
            assert_eq!(get(&reply, "status"), Value::Number(f64::from(final_status)));
        }
    }

    #[test]
    fn grant_retry_policy_never_retries_parse_secret_store_or_egress_failures() {
        let mut auth = login_auth();
        auth["grantRequest"] = json!({"retry":{"max":3,"on":["1xx","408","5xx","429","failed","timeout"],
            "scheduleMs":[100,500,1500]}});
        for answer in [HttpAnswer::Secret(Ok(None)), HttpAnswer::Secret(Err("vault unavailable".into()))] {
            let (mut d, _) = HttpCallDriver::start(spec(json!({"auth":auth.clone()})), egress_with_auth(), None);
            assert_eq!(d.resume(HttpAnswer::Secret(Ok(None))), secret_ask("USER", SecretScope::App));
            assert!(matches!(d.resume(answer), HttpStep::Done(Err(_))));
        }
        let (mut d, _) = HttpCallDriver::start(spec(json!({"auth":auth.clone()})), egress(), None);
        d.resume(HttpAnswer::Secret(Ok(None)));
        d.resume(ok_secret("alice"));
        let refused = done(d.resume(ok_secret("s3cret"))).unwrap_err();
        assert_eq!(get(&refused, "kind"), Value::string("refused"));
        for answer in [
            HttpAnswer::Http(Err(TransportError::Refused("private resolved address".into()))),
            HttpAnswer::Http(Ok(response(200, &[], "not JSON"))),
            HttpAnswer::Http(Ok(response(200, &[], "{}"))),
        ] {
            let (mut d, _) = HttpCallDriver::start(spec(json!({"auth":auth.clone()})), egress_with_auth(), None);
            d.resume(HttpAnswer::Secret(Ok(None)));
            d.resume(ok_secret("alice"));
            http_ask(d.resume(ok_secret("s3cret")));
            assert!(matches!(d.resume(answer), HttpStep::Done(Err(_))));
        }
        let (mut d, _) = HttpCallDriver::start(spec(json!({"auth":auth})), egress_with_auth(), None);
        d.resume(HttpAnswer::Secret(Ok(None)));
        d.resume(ok_secret("alice"));
        http_ask(d.resume(ok_secret("s3cret")));
        assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"T1","ttl":60}"#)))),
            HttpStep::Ask(HttpAsk::SecretSet { .. })));
        assert!(matches!(d.resume(HttpAnswer::SecretSet(Err("vault write unavailable".into()))), HttpStep::Done(Err(_))));
    }

    #[test]
    fn delayed_grant_revival_reacquires_secrets_without_serializing_them() {
        for restore_held in [false, true] {
            let mut auth = login_auth();
            auth["grantRequest"] = json!({"headers":{"X-Grant":"public-trace"},"timeoutMs":15000,
                "retry":{"max":3,"on":["5xx"],"scheduleMs":[100,500,1500]}});
            let (mut live, _) = HttpCallDriver::start(spec(json!({"auth":auth})), egress_with_auth(), Some("revived-grant".into()));
            live.resume(HttpAnswer::Secret(Ok(None)));
            live.resume(ok_secret("private-grant-user"));
            let grant = http_ask(live.resume(ok_secret("private-grant-password")));
            assert_eq!(http_ask_delay(live.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (grant.clone(), Some(100)));
            let snapshot = serde_json::to_string(&live).unwrap();
            let grant_text = grant.text();
            for secret in ["private-grant-user","private-grant-password",grant_text.as_ref()] {
                assert!(!snapshot.contains(secret));
                assert!(!format!("{live:?}").contains(secret));
            }
            assert!(snapshot.contains("public-trace"));
            let mut revived: HttpCallDriver = serde_json::from_str(&snapshot).unwrap();
            if restore_held {
                let mut copy = live.clone();
                revived.restore_secrets(copy.take_secrets());
            }
            let answer = HttpAnswer::Http(Ok(response(503, &[], "")));
            let expected = live.resume(answer.clone());
            let mut actual = revived.resume(answer);
            if !restore_held {
                assert_eq!(actual, secret_ask("USER", SecretScope::App));
                assert_eq!(revived.resume(ok_secret("private-grant-user")), secret_ask("PASS", SecretScope::App));
                actual = revived.resume(ok_secret("private-grant-password"));
            }
            assert_eq!(actual, expected);
            assert_eq!(http_ask_delay(actual), (grant.clone(), Some(500)));
            for d in [&mut live, &mut revived] {
                assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (grant.clone(), Some(1500)));
                assert!(matches!(d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"PRIVATE-AT","ttl":60}"#)))),
                    HttpStep::Ask(HttpAsk::SecretSet { .. })));
                assert!(!serde_json::to_string(d).unwrap().contains("PRIVATE-AT"));
                assert_eq!(header(&http_ask(d.resume(HttpAnswer::SecretSet(Ok(())))), "authorization"), Some("Bearer PRIVATE-AT"));
            }
        }
    }

    #[test]
    fn delayed_resource_revival_preserves_delay_and_spent_budget() {
        for (policy, final_delay) in [
            (json!({"max":3,"on":["5xx"],"backoffMs":10}),40),
            (json!({"max":3,"on":["5xx"],"scheduleMs":[10,20,30]}),30),
        ] {
            for restore_held in [false, true] {
                let call = spec(json!({"auth":{"scheme":"bearer","credential":"TOKEN"},"headers":{"X-Trace":"public-trace"},
                    "retry":policy.clone()}));
                let (mut live, step) = HttpCallDriver::start(call, egress(), Some("revived-resource".into()));
                assert_eq!(step, secret_ask("TOKEN", SecretScope::App));
                let request = http_ask(live.resume(ok_secret("private-resource-token")));
                assert_eq!(http_ask_delay(live.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))), (request.clone(), Some(10)));
                let snapshot = serde_json::to_string(&live).unwrap();
                assert!(!snapshot.contains("private-resource-token"));
                assert!(snapshot.contains("public-trace"));
                let mut revived: HttpCallDriver = serde_json::from_str(&snapshot).unwrap();
                if restore_held {
                    let mut copy = live.clone();
                    revived.restore_secrets(copy.take_secrets());
                }
                let answer = HttpAnswer::Http(Ok(response(503, &[], "")));
                let expected = live.resume(answer.clone());
                let mut actual = revived.resume(answer);
                if !restore_held {
                    assert_eq!(actual, secret_ask("TOKEN", SecretScope::App));
                    actual = revived.resume(ok_secret("private-resource-token"));
                }
                assert_eq!(actual, expected);
                assert_eq!(http_ask_delay(actual), (request.clone(), Some(20)));
                for d in [&mut live, &mut revived] {
                    assert_eq!(http_ask_delay(d.resume(HttpAnswer::Http(Ok(response(503, &[], ""))))),
                        (request.clone(), Some(final_delay)));
                    let reply = done(d.resume(HttpAnswer::Http(Ok(response(503, &[], "exhausted"))))).unwrap();
                    assert_eq!(get(&reply, "status"), Value::Number(503.0));
                    assert_eq!(get(&reply, "key"), Value::string("revived-resource"));
                }
            }
        }
    }

    #[test]
    fn retry_schedule_and_grant_options_roundtrip_while_driver_secrets_stay_private() {
        let credential = json!({"client_email":"private-roundtrip@example.com",
            "private_key":include_str!("../fixtures/jwt/rs256-pkcs8.pem")}).to_string();
        let secrets = |slot: &CredentialSlot| {
            if slot.name == "SERVICE_ACCOUNT" { Some(credential.clone()) } else { vault(slot) }
        };
        for mut auth in grant_auths() {
            auth["grantRequest"] = json!({"headers":{"X-Grant":"public-trace"},"timeoutMs":0,
                "retry":{"max":3,"on":["1xx","408","5xx","429","timeout","failed"],
                    "scheduleMs":[100,500,1500],"noResponseMax":2}});
            let call = spec(json!({"auth":auth,"retry":{"max":1,"on":["5xx"],"backoffMs":7}}));
            let wire = serde_json::to_string(&call).unwrap();
            let roundtrip: HttpCallSpec = serde_json::from_str(&wire).unwrap();
            assert_eq!(roundtrip, call);
            assert!(!wire.contains(&credential));
            assert_eq!(token_exchange_request(roundtrip.auth.as_ref().unwrap(), &secrets).unwrap(),
                token_exchange_request(call.auth.as_ref().unwrap(), &secrets).unwrap());
            let (d, step) = HttpCallDriver::start(call, egress_with_auth(), Some("wire".into()));
            assert!(matches!(step, HttpStep::Ask(HttpAsk::Secret { .. })));
            let snapshot = serde_json::to_string(&d).unwrap();
            let restored: HttpCallDriver = serde_json::from_str(&snapshot).unwrap();
            assert_eq!(serde_json::to_string(&restored).unwrap(), snapshot);
            assert!(!snapshot.contains(&credential));
            assert!(snapshot.contains("public-trace"));
        }
    }
}

// --- onUnauthorized, queryEncoding, grantBody ----------------------------------------------------------------

#[test]
fn on_unauthorized_defaults_to_reply_and_is_a_closed_choice() {
    let mut auth = login_auth();
    auth.as_object_mut().unwrap().remove("onUnauthorized");
    let s = spec(json!({"auth": auth.clone()}));
    let Some(HttpAuth::TokenExchange(ex)) = &s.auth else { panic!("exchange") };
    assert_eq!(ex.on_unauthorized, OnUnauthorized::Reply);
    for value in ["reply", "refresh"] {
        auth["onUnauthorized"] = json!(value);
        assert!(parse(HttpMethod::Post, "/", json!({"auth": auth.clone()})).is_ok(), "{value}");
    }
    for bad in [json!("retry"), json!(true), json!(1)] {
        auth["onUnauthorized"] = bad;
        assert!(parse(HttpMethod::Post, "/", json!({"auth": auth.clone()})).is_err());
    }
    assert!(parse(HttpMethod::Post, "/", json!({"auth": {"scheme": "bearer", "credential": "K", "onUnauthorized": "refresh"}})).is_err(), "bearer has no such key");
}

#[test]
fn by_default_a_401_sends_one_grant_and_one_resource_request_and_refresh_sends_two_of_each() {
    let drive = |on: Option<&str>| {
        let mut auth = login_auth();
        match on {
            Some(v) => auth["onUnauthorized"] = json!(v),
            None => {
                auth.as_object_mut().unwrap().remove("onUnauthorized");
            }
        }
        let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": auth})), egress_with_auth(), None);
        let mut sent = vec![];
        d.resume(HttpAnswer::Secret(Ok(None)));
        d.resume(ok_secret("alice"));
        sent.push(http_ask(d.resume(ok_secret("s3cret"))).url);
        d.resume(HttpAnswer::Http(Ok(response(200, &[], r#"{"token":"T1","ttl":60}"#))));
        sent.push(http_ask(d.resume(HttpAnswer::SecretSet(Ok(())))).url);
        match d.resume(HttpAnswer::Http(Ok(response(401, &[], "no")))) {
            HttpStep::Done(Ok(reply)) => assert_eq!(get(&reply, "status"), Value::Number(401.0)),
            HttpStep::Ask(_) => {
                sent.push("https://auth.example.com/login".into());
                sent.push("https://api.example.com/v1/x".into());
            }
            other => panic!("{other:?}"),
        }
        sent.len()
    };
    assert_eq!(drive(None), 2);
    assert_eq!(drive(Some("reply")), 2);
    assert_eq!(drive(Some("refresh")), 4);
}

#[test]
fn query_encoding_defaults_to_uri_and_rfc3986_also_escapes_the_marks() {
    let value = "a b!'()*~-._é";
    let send = |extra: serde_json::Value| {
        let mut request = json!({"query": {"q": value}});
        for (k, v) in extra.as_object().unwrap() {
            request[k] = v.clone();
        }
        let s = parse(HttpMethod::Get, "/s", request).unwrap();
        let (_, step) = HttpCallDriver::start(s, egress(), None);
        http_ask(step).url
    };
    assert_eq!(send(json!({})), "https://api.example.com/s?q=a%20b!'()*~-._%C3%A9");
    assert_eq!(send(json!({"queryEncoding": "uri"})), "https://api.example.com/s?q=a%20b!'()*~-._%C3%A9");
    assert_eq!(send(json!({"queryEncoding": "rfc3986"})), "https://api.example.com/s?q=a%20b%21%27%28%29%2A~-._%C3%A9");
    assert!(parse(HttpMethod::Get, "/s", json!({"queryEncoding": "html"})).is_err());
    assert!(parse(HttpMethod::Get, "/s", json!({"queryEncoding": 3})).is_err());
}

#[test]
fn a_non_2xx_grant_exposes_its_body_text_a_2xx_grant_never_does() {
    let grant_failure = |status: u16, body: &str| {
        let (mut d, _) = HttpCallDriver::start(spec(json!({"auth": login_auth()})), egress_with_auth(), None);
        d.resume(HttpAnswer::Secret(Ok(None)));
        d.resume(ok_secret("alice"));
        http_ask(d.resume(ok_secret("s3cret")));
        done(d.resume(HttpAnswer::Http(Ok(response(status, &[], body))))).unwrap_err()
    };
    let rejected = grant_failure(401, r#"{"error":"invalid_grant"}"#);
    assert_eq!(get(&rejected, "grantBody"), Value::string(r#"{"error":"invalid_grant"}"#));
    assert_eq!(get(&rejected, "grantStatus"), Value::Number(401.0));
    let empty = grant_failure(500, "");
    assert_eq!(get(&empty, "grantBody"), Value::string(String::new()));
    let malformed_2xx = grant_failure(200, r#"{"access_token":"SECRET-TOKEN"}"#);
    assert_eq!(get(&malformed_2xx, "grantBody"), Value::Null);
    assert!(!format!("{malformed_2xx:?}").contains("SECRET-TOKEN"));

    let (mut d, _) = HttpCallDriver::start(spec(json!({})), egress(), None);
    let resource = done(d.resume(HttpAnswer::Http(Err(TransportError::Failed("down".into()))))).unwrap_err();
    assert_eq!(get(&resource, "grantBody"), Value::Null, "a resource failure has no grant body");
}

mod failure_wire {
// The shared `http` failure payload: `phase`, `grantStatus`, `code`, `reason` and the kinds `oauth` and `unavailable`.

    use orbital_core_base::value::Value;
    use orbital_core_runtime::http_request::*;
    use orbital_core_runtime::http_service::{HttpResponse, TransportError};
    use serde_json::json;

    fn val(j: serde_json::Value) -> Value {
        serde_json::from_value(j).unwrap()
    }

    fn get(v: &Value, path: &str) -> Value {
        v.get_path(path).cloned().unwrap_or(Value::Null)
    }

    fn text(s: &str) -> Value {
        Value::string(s)
    }

    fn spec(request: serde_json::Value) -> HttpCallSpec {
        parse_http_call(HttpMethod::Post, "https://api.example.com", "/v1/x", &val(request)).unwrap()
    }

    fn egress() -> Vec<String> {
        vec!["https://api.example.com".into(), "https://auth.example.com".into()]
    }

    fn response(status: u16, body: &str) -> HttpResponse {
        HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() }
    }

    fn done(step: HttpStep) -> Result<Value, Value> {
        match step {
            HttpStep::Done(r) => r,
            other => panic!("expected Done, got {other:?}"),
        }
    }

    fn exchange_auth() -> serde_json::Value {
        json!({"scheme": "token-exchange", "tokenUrl": "https://auth.example.com/token", "grant": "refresh_token",
            "body": {"form": {"refresh_token": {"credential": "RT"}}}, "accessField": "id_token",
            "storeAs": {"name": "ID", "scope": "app"}})
    }

    /// A driver that has sent its grant request (cache miss, refresh token held).
    fn at_grant_reply() -> HttpCallDriver {
        let (mut d, step) = HttpCallDriver::start(spec(json!({"auth": exchange_auth()})), egress(), Some("k".into()));
        assert!(matches!(step, HttpStep::Ask(HttpAsk::Secret { .. })));
        let step = d.resume(HttpAnswer::Secret(Ok(None)));
        assert!(matches!(step, HttpStep::Ask(HttpAsk::Secret { .. })));
        let step = d.resume(HttpAnswer::Secret(Ok(Some("refresh-1".into()))));
        assert!(matches!(step, HttpStep::Ask(HttpAsk::Http { .. })), "grant request: {step:?}");
        d
    }

    #[test]
    fn every_failure_carries_the_four_new_fields_null_by_default() {
        for payload in [
            failure_payload(&TransportError::TimedOut),
            failure_payload(&TransportError::Failed("boom".into())),
            failure_payload(&TransportError::Refused("no".into())),
            refusal_payload(&HttpRefusal::CredentialMissing("K".into())),
            refusal_payload(&HttpRefusal::EgressDenied("o".into())),
        ] {
            for field in ["phase", "grantStatus", "grantBody", "code", "reason"] {
                assert_eq!(get(&payload, field), Value::Null, "{field} in {payload:?}");
            }
        }
    }

    #[test]
    fn oauth_and_unavailable_payloads_carry_their_kind_code_and_reason() {
        let oauth = oauth_failure_payload("State is unknown", "VALIDATION_ERROR", "state-unknown");
        assert_eq!(get(&oauth, "kind"), text("oauth"));
        assert_eq!(get(&oauth, "error"), text("State is unknown"));
        assert_eq!(get(&oauth, "code"), text("VALIDATION_ERROR"));
        assert_eq!(get(&oauth, "reason"), text("state-unknown"));
        assert_eq!(get(&oauth, "credential"), Value::Null);
        assert_eq!(get(&oauth, "phase"), Value::Null);
        assert_eq!(get(&oauth, "grantStatus"), Value::Null);

        let unavailable = unavailable_payload("the host has no beacon port");
        assert_eq!(get(&unavailable, "kind"), text("unavailable"));
        assert_eq!(get(&unavailable, "error"), text("the host has no beacon port"));
        for field in ["credential", "phase", "grantStatus", "grantBody", "code", "reason"] {
            assert_eq!(get(&unavailable, field), Value::Null, "{field}");
        }
    }

    #[test]
    fn a_failed_grant_reply_is_phase_grant_with_its_status() {
        for status in [400u16, 401, 500] {
            let mut d = at_grant_reply();
            let err = done(d.resume(HttpAnswer::Http(Ok(response(status, r#"{"error":"INVALID_REFRESH_TOKEN"}"#))))).unwrap_err();
            assert_eq!(get(&err, "kind"), text("failed"));
            assert_eq!(get(&err, "phase"), text("grant"));
            assert_eq!(get(&err, "grantStatus"), Value::Number(f64::from(status)));
            assert_eq!(get(&err, "key"), text("k"));
            assert_eq!(get(&err, "grantBody"), text(r#"{"error":"INVALID_REFRESH_TOKEN"}"#), "a rejected grant exposes its body");
            assert!(!format!("{err:?}").contains("refresh-1"), "no held secret");
        }
    }

    #[test]
    fn a_grant_transport_failure_is_phase_grant_without_a_status() {
        for (err, kind) in [(TransportError::TimedOut, "timeout"), (TransportError::Failed("down".into()), "failed")] {
            let mut d = at_grant_reply();
            let payload = done(d.resume(HttpAnswer::Http(Err(err)))).unwrap_err();
            assert_eq!(get(&payload, "kind"), text(kind));
            assert_eq!(get(&payload, "phase"), text("grant"));
            assert_eq!(get(&payload, "grantStatus"), Value::Null);
        }
    }

    #[test]
    fn a_2xx_grant_reply_without_the_token_is_phase_grant_without_a_status() {
        let mut d = at_grant_reply();
        let err = done(d.resume(HttpAnswer::Http(Ok(response(200, r#"{"other":1}"#))))).unwrap_err();
        assert_eq!(get(&err, "phase"), text("grant"));
        assert_eq!(get(&err, "grantStatus"), Value::Null);
    }

    #[test]
    fn the_authored_request_failing_is_phase_request_and_a_refusal_has_no_phase() {
        let (mut d, _) = HttpCallDriver::start(spec(json!({})), egress(), None);
        let err = done(d.resume(HttpAnswer::Http(Err(TransportError::Failed("down".into()))))).unwrap_err();
        assert_eq!(get(&err, "phase"), text("request"));
        assert_eq!(get(&err, "grantStatus"), Value::Null);

        let (_, step) = HttpCallDriver::start(spec(json!({})), vec![], None);
        let err = done(step).unwrap_err();
        assert_eq!(get(&err, "kind"), text("refused"));
        assert_eq!(get(&err, "phase"), Value::Null);
    }

    #[test]
    fn a_successful_call_has_no_failure_fields() {
        let (mut d, _) = HttpCallDriver::start(spec(json!({})), egress(), None);
        let ok = done(d.resume(HttpAnswer::Http(Ok(response(500, "x"))))).unwrap();
        assert_eq!(get(&ok, "status"), Value::Number(500.0));
        assert_eq!(get(&ok, "phase"), Value::Null);
    }

}

mod token_rotate {
// `token-exchange` `rotate: { field, credential }`: the grant reply's replacement refresh token is written to the
// host-held credential (a durable `SecretRotate` ask) before the access token is cached and before the resource
// request is sent.

    use orbital_core_base::value::Value;
    use orbital_core_runtime::http_request::*;
    use orbital_core_runtime::http_service::{HttpRequest, HttpResponse, TransportError};
    use serde_json::json;

    fn val(j: serde_json::Value) -> Value {
        serde_json::from_value(j).unwrap()
    }

    fn get(v: &Value, path: &str) -> Value {
        v.get_path(path).cloned().unwrap_or(Value::Null)
    }

    fn text(s: &str) -> Value {
        Value::string(s)
    }

    fn parse(auth: serde_json::Value) -> Result<HttpCallSpec, HttpRefusal> {
        parse_http_call(HttpMethod::Get, "https://api.example.com", "/x", &val(json!({ "auth": auth })))
    }

    fn auth(extra: serde_json::Value) -> serde_json::Value {
        let mut base = json!({
            "scheme": "token-exchange", "tokenUrl": "https://auth.example.com/token", "grant": "refresh_token",
            "body": {"form": {"refresh_token": {"credential": "RT"}}},
            "accessField": "id_token", "expiresField": "expires_in", "expiresMarginMs": 1000,
            "rotate": {"field": "refresh_token", "credential": "RT"}, "onUnauthorized": "refresh",
            "storeAs": {"name": "ID", "scope": "app"}
        });
        for (k, v) in extra.as_object().unwrap() {
            if v.is_null() {
                base.as_object_mut().unwrap().remove(k);
            } else {
                base[k] = v.clone();
            }
        }
        base
    }

    fn egress() -> Vec<String> {
        vec!["https://api.example.com".into(), "https://auth.example.com".into()]
    }

    fn response(status: u16, body: &str) -> HttpResponse {
        HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() }
    }

    fn ok_secret(v: &str) -> HttpAnswer {
        HttpAnswer::Secret(Ok(Some(v.into())))
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

    fn header<'a>(r: &'a HttpRequest, name: &str) -> Option<&'a str> {
        r.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    const GRANT_OK: &str = r#"{"id_token":"ID-1","expires_in":3600,"refresh_token":"refresh-2"}"#;

    /// A driver that has sent its grant request: cache miss, refresh token `refresh-1` held.
    fn at_grant_reply(auth_json: serde_json::Value) -> HttpCallDriver {
        let (mut d, step) = HttpCallDriver::start(parse(auth_json).unwrap(), egress(), Some("k".into()));
        assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "ID".into(), scope: SecretScope::App }));
        let step = d.resume(HttpAnswer::Secret(Ok(None)));
        assert_eq!(step, HttpStep::Ask(HttpAsk::Secret { name: "RT".into(), scope: SecretScope::App }));
        let grant = http_ask(d.resume(ok_secret("refresh-1")));
        assert_eq!(grant.url, "https://auth.example.com/token");
        assert_eq!(grant.text(), "grant_type=refresh_token&refresh_token=refresh-1");
        d
    }

    #[test]
    fn rotate_parses_onto_the_exchange_with_the_body_slot() {
        let spec = parse(auth(json!({}))).unwrap();
        let Some(HttpAuth::TokenExchange(ex)) = &spec.auth else { panic!("exchange") };
        let rotate = ex.rotate.as_ref().expect("rotate");
        assert_eq!(rotate.field, "refresh_token");
        assert_eq!(rotate.credential, CredentialSlot { name: "RT".into(), scope: SecretScope::App });
        let none = parse(auth(json!({"rotate": null}))).unwrap();
        let Some(HttpAuth::TokenExchange(ex)) = &none.auth else { panic!("exchange") };
        assert!(ex.rotate.is_none(), "control: absent rotate stays None");
    }

    #[test]
    fn rotate_may_name_the_slot_as_an_object_and_a_user_scope_must_match_the_body() {
        let user = json!({
            "body": {"form": {"refresh_token": {"credential": "RT", "scope": "user"}}},
            "rotate": {"field": "refresh_token", "credential": {"credential": "RT", "scope": "user"}},
            "storeAs": {"name": "ID", "scope": "user"}
        });
        let spec = parse(auth(user)).unwrap();
        let Some(HttpAuth::TokenExchange(ex)) = &spec.auth else { panic!("exchange") };
        assert_eq!(ex.rotate.as_ref().unwrap().credential.scope, SecretScope::User);

        let mismatch = json!({"rotate": {"field": "refresh_token", "credential": {"credential": "RT", "scope": "user"}}});
        assert!(parse(auth(mismatch)).is_err(), "app body slot, user rotate slot");
    }

    #[test]
    fn rotate_is_refused_unless_it_is_a_well_formed_refresh_token_rotation() {
        let cases = [
            ("a custom grant", json!({"grant": null})),
            ("a client_credentials grant", json!({"grant": "client_credentials"})),
            ("an empty field", json!({"rotate": {"field": "", "credential": "RT"}})),
            ("a missing field", json!({"rotate": {"credential": "RT"}})),
            ("a missing credential", json!({"rotate": {"field": "refresh_token"}})),
            ("an unknown rotate key", json!({"rotate": {"field": "refresh_token", "credential": "RT", "ttl": 1}})),
            ("a credential that is not the body's refresh_token slot", json!({"rotate": {"field": "refresh_token", "credential": "OTHER"}})),
            ("a non-object rotate", json!({"rotate": "refresh_token"})),
        ];
        for (name, extra) in cases {
            assert!(matches!(parse(auth(extra)), Err(HttpRefusal::Invalid(_))), "{name}");
        }
        let oauth2 = json!({"scheme": "oauth2-client", "tokenUrl": "https://auth.example.com/token", "clientId": "CID", "clientSecret": "CSEC",
            "rotate": {"field": "refresh_token", "credential": "RT"}});
        assert!(parse(oauth2).is_err(), "oauth2-client has no rotate key");
    }

    #[test]
    fn a_grant_reply_rotates_the_refresh_token_before_the_access_token_is_cached_or_sent() {
        let mut d = at_grant_reply(auth(json!({})));
        let step = d.resume(HttpAnswer::Http(Ok(response(200, GRANT_OK))));
        assert_eq!(
            step,
            HttpStep::Ask(HttpAsk::SecretRotate { name: "RT".into(), value: "refresh-2".into(), scope: SecretScope::App }),
            "rotation comes first"
        );
        let step = d.resume(HttpAnswer::SecretRotate(Ok(())));
        assert_eq!(
            step,
            HttpStep::Ask(HttpAsk::SecretSet { name: "ID".into(), value: "ID-1".into(), ttl_ms: 3_599_000, scope: SecretScope::App })
        );
        let request = http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
        assert_eq!(header(&request, "authorization"), Some("Bearer ID-1"));
        assert_eq!(request.url, "https://api.example.com/x");
        let reply = done(d.resume(HttpAnswer::Http(Ok(response(200, "ok"))))).unwrap();
        assert_eq!(get(&reply, "status"), Value::Number(200.0));
        assert_eq!(get(&reply, "key"), text("k"));
    }

    #[test]
    fn a_failed_rotation_ends_the_call_in_the_grant_phase_without_caching_or_sending() {
        let mut d = at_grant_reply(auth(json!({})));
        d.resume(HttpAnswer::Http(Ok(response(200, GRANT_OK))));
        let err = done(d.resume(HttpAnswer::SecretRotate(Err("store down".into())))).unwrap_err();
        assert_eq!(get(&err, "kind"), text("failed"));
        assert_eq!(get(&err, "phase"), text("grant"));
        assert_eq!(get(&err, "grantStatus"), Value::Null);
        assert_eq!(get(&err, "key"), text("k"));
        let shown = format!("{err:?}");
        assert!(!shown.contains("refresh-2") && !shown.contains("ID-1"), "no token in the failure: {shown}");
    }

    #[test]
    fn a_grant_reply_without_the_rotated_field_fails_the_grant_before_any_write() {
        for body in [r#"{"id_token":"ID-1","expires_in":3600}"#, r#"{"id_token":"ID-1","refresh_token":""}"#, r#"{"id_token":"ID-1","refresh_token":7}"#] {
            let mut d = at_grant_reply(auth(json!({})));
            let err = done(d.resume(HttpAnswer::Http(Ok(response(200, body))))).unwrap_err();
            assert_eq!(get(&err, "kind"), text("failed"), "{body}");
            assert_eq!(get(&err, "phase"), text("grant"), "{body}");
        }
    }

    #[test]
    fn a_failed_grant_reply_never_rotates_and_reports_its_status() {
        let mut d = at_grant_reply(auth(json!({})));
        let err = done(d.resume(HttpAnswer::Http(Ok(response(400, r#"{"refresh_token":"leak"}"#))))).unwrap_err();
        assert_eq!(get(&err, "phase"), text("grant"));
        assert_eq!(get(&err, "grantStatus"), Value::Number(400.0));
        assert_eq!(get(&err, "grantBody"), text(r#"{"refresh_token":"leak"}"#));
    }

    #[test]
    fn without_rotate_the_exchange_never_asks_for_a_rotation() {
        let mut d = at_grant_reply(auth(json!({"rotate": null})));
        let step = d.resume(HttpAnswer::Http(Ok(response(200, GRANT_OK))));
        assert!(matches!(step, HttpStep::Ask(HttpAsk::SecretSet { .. })), "{step:?}");
    }

    #[test]
    fn a_cached_token_skips_the_grant_and_the_rotation() {
        let (mut d, _) = HttpCallDriver::start(parse(auth(json!({}))).unwrap(), egress(), None);
        let request = http_ask(d.resume(ok_secret("CACHED")));
        assert_eq!(header(&request, "authorization"), Some("Bearer CACHED"));
    }

    #[test]
    fn a_401_regrant_in_the_same_call_uses_the_rotated_refresh_token() {
        let mut d = at_grant_reply(auth(json!({})));
        d.resume(HttpAnswer::Http(Ok(response(200, GRANT_OK))));
        d.resume(HttpAnswer::SecretRotate(Ok(())));
        http_ask(d.resume(HttpAnswer::SecretSet(Ok(()))));
        let step = d.resume(HttpAnswer::Http(Ok(response(401, "expired"))));
        let grant = http_ask(step);
        assert_eq!(grant.text(), "grant_type=refresh_token&refresh_token=refresh-2", "the held token is the rotated one");
    }

    #[test]
    fn a_transport_failure_of_the_grant_does_not_rotate() {
        let mut d = at_grant_reply(auth(json!({})));
        let err = done(d.resume(HttpAnswer::Http(Err(TransportError::TimedOut)))).unwrap_err();
        assert_eq!(get(&err, "kind"), text("timeout"));
        assert_eq!(get(&err, "phase"), text("grant"));
    }

    #[test]
    fn the_rotate_ask_and_answer_round_trip_the_wire_and_redact_the_value() {
        let ask = HttpAsk::SecretRotate { name: "RT".into(), value: "refresh-2".into(), scope: SecretScope::User };
        let wire = serde_json::to_value(&ask).unwrap();
        assert_eq!(wire, json!({"kind": "secretRotate", "name": "RT", "value": "refresh-2", "scope": "user"}));
        assert_eq!(serde_json::from_value::<HttpAsk>(wire).unwrap(), ask);
        let app = HttpAsk::SecretRotate { name: "RT".into(), value: "v".into(), scope: SecretScope::App };
        assert_eq!(serde_json::to_value(&app).unwrap(), json!({"kind": "secretRotate", "name": "RT", "value": "v"}));
        assert!(!format!("{ask:?}").contains("refresh-2"));

        for answer in [HttpAnswer::SecretRotate(Ok(())), HttpAnswer::SecretRotate(Err("no".into()))] {
            let wire = serde_json::to_value(&answer).unwrap();
            assert_eq!(wire["kind"], "secretRotate");
            assert_eq!(serde_json::from_value::<HttpAnswer>(wire).unwrap(), answer);
        }
    }

    #[test]
    fn a_driver_survives_serialization_while_awaiting_the_rotation() {
        let mut d = at_grant_reply(auth(json!({})));
        let step = d.resume(HttpAnswer::Http(Ok(response(200, GRANT_OK))));
        assert!(matches!(step, HttpStep::Ask(HttpAsk::SecretRotate { .. })));
        let wire = serde_json::to_string(&d).unwrap();
        assert!(!wire.contains("refresh-1") && !wire.contains("refresh-2") && !wire.contains("ID-1"), "{wire}");
        let mut revived: HttpCallDriver = serde_json::from_str(&wire).unwrap();
        revived.restore_secrets(d.take_secrets());
        let step = revived.resume(HttpAnswer::SecretRotate(Ok(())));
        assert_eq!(
            step,
            HttpStep::Ask(HttpAsk::SecretSet { name: "ID".into(), value: "ID-1".into(), ttl_ms: 3_599_000, scope: SecretScope::App })
        );
    }

}

mod dynamic_origin {
// D-A88: an `origin` evaluated from data (a push subscription's endpoint) is checked against the app's static
// `egress` list by the driver before anything is sent; a miss fails `refused`.

    use orbital_core_base::value::Value;
    use orbital_core_runtime::http_request::*;
    use serde_json::json;

    fn get(v: &Value, path: &str) -> Value {
        v.get_path(path).cloned().unwrap_or(Value::Null)
    }

    fn start(origin: &str, egress: &[&str]) -> Result<HttpStep, HttpRefusal> {
        let request: Value = serde_json::from_value(json!({})).unwrap();
        let spec = parse_http_call(HttpMethod::Post, origin, "/send/abc", &request)?;
        let egress = egress.iter().map(|e| e.to_string()).collect();
        Ok(HttpCallDriver::start(spec, egress, Some("k".into())).1)
    }

    fn sends(step: &HttpStep) -> Option<String> {
        match step {
            HttpStep::Ask(HttpAsk::Http { request, .. }) => Some(request.url.clone()),
            _ => None,
        }
    }

    fn refused(step: HttpStep) {
        match step {
            HttpStep::Done(Err(payload)) => {
                assert_eq!(get(&payload, "kind"), Value::string("refused"));
                assert_eq!(get(&payload, "key"), Value::string("k"));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_origin_held_in_egress_is_sent_to() {
        let step = start("https://push.example.com", &["https://push.example.com"]).unwrap();
        assert_eq!(sends(&step).as_deref(), Some("https://push.example.com/send/abc"));
    }

    #[test]
    fn one_of_several_egress_entries_is_enough() {
        let step = start("https://b.example.com", &["https://a.example.com", "https://b.example.com"]).unwrap();
        assert!(sends(&step).is_some());
    }

    #[test]
    fn an_origin_outside_egress_is_refused_before_any_ask() {
        refused(start("https://evil.example.com", &["https://push.example.com"]).unwrap());
        refused(start("https://push.example.com", &[]).unwrap());
    }

    #[test]
    fn a_port_or_scheme_mismatch_is_refused() {
        refused(start("https://push.example.com:8443", &["https://push.example.com"]).unwrap());
        refused(start("https://push.example.com", &["https://push.example.com:8443"]).unwrap());
        refused(start("http://push.example.com", &["https://push.example.com"]).unwrap());
        refused(start("https://push.example.com", &["http://push.example.com"]).unwrap());
    }

    #[test]
    fn the_default_port_and_host_case_do_not_change_the_origin() {
        assert!(sends(&start("https://PUSH.example.com:443", &["https://push.example.com"]).unwrap()).is_some());
        assert!(sends(&start("https://push.example.com", &["https://push.example.com:443"]).unwrap()).is_some());
    }

    #[test]
    fn a_lookalike_host_is_refused() {
        refused(start("https://push.example.com.evil.example", &["https://push.example.com"]).unwrap());
        refused(start("https://evilpush.example.com", &["https://push.example.com"]).unwrap());
        refused(start("https://example.com", &["https://push.example.com"]).unwrap());
    }

    #[test]
    fn a_path_prefix_origin_matches_on_its_origin_part_and_keeps_its_prefix() {
        let step = start("https://push.example.com/wpush/v2", &["https://push.example.com"]).unwrap();
        assert_eq!(sends(&step).as_deref(), Some("https://push.example.com/wpush/v2/send/abc"));
        refused(start("https://push.example.com/wpush/v2", &["https://other.example.com"]).unwrap());
    }

    #[test]
    fn an_origin_that_hides_another_host_in_userinfo_does_not_parse() {
        let hidden = start("https://push.example.com@evil.example", &["https://push.example.com"]);
        match hidden {
            Err(_) => {}
            Ok(step) => refused(step),
        }
    }

    #[test]
    fn control_a_static_origin_behaves_the_same() {
        assert!(sends(&start("https://push.example.com", &["https://push.example.com"]).unwrap()).is_some());
        refused(start("https://push.example.com", &["https://other.example.com"]).unwrap());
    }

}

mod oauth_client {
// `oauth-client` auth and the `oauth-endpoint` target: parse rules, the closed op set, and the one
// assumption the knowledge refresh depends on (a `tokenUrl` may carry a `?key=` query).

    use orbital_core_base::value::Value;
    use orbital_core_runtime::http_request::*;
    use serde_json::json;

    fn val(j: serde_json::Value) -> Value {
        serde_json::from_value(j).unwrap()
    }

    fn client() -> serde_json::Value {
        json!({"tag":"google","mode":"real","issuer":"https://idp.example.com","clientId":"CID","clientSecret":"CSEC","clientAuth":"post"})
    }

    fn call(method: HttpMethod, word: &str, auth: serde_json::Value) -> Result<HttpCallSpec, HttpRefusal> {
        parse_http_call(method, "oauth-endpoint", word, &val(json!({ "auth": auth })))
    }

    fn connect() -> serde_json::Value {
        json!({"scheme":"oauth-client","op":"connect","client":client(),"provider":"google","code":"C","state":"S"})
    }

    #[test]
    fn a_token_url_may_carry_a_key_query_and_egress_matches_scheme_host_port() {
        let auth = json!({"scheme":"token-exchange","tokenUrl":"https://securetoken.example.com/v1/token?key=KEY","grant":"refresh_token",
            "body":{"form":{"refresh_token":{"credential":"RT"}}},"accessField":"id_token"});
        let spec = parse_http_call(HttpMethod::Get, "https://api.example.com", "/x", &val(json!({ "auth": auth }))).unwrap();
        let egress = vec!["https://api.example.com".to_string(), "https://securetoken.example.com".to_string()];
        let (mut d, step) = HttpCallDriver::start(spec, egress, None);
        assert!(matches!(step, HttpStep::Ask(HttpAsk::Secret { .. })));
        let step = d.resume(HttpAnswer::Secret(Ok(None)));
        assert!(matches!(step, HttpStep::Ask(HttpAsk::Secret { ref name, .. }) if name == "RT"), "{step:?}");
        let step = d.resume(HttpAnswer::Secret(Ok(Some("r".into()))));
        let HttpStep::Ask(HttpAsk::Http { request, .. }) = step else { panic!("grant request") };
        assert_eq!(request.url, "https://securetoken.example.com/v1/token?key=KEY");
    }

    #[test]
    fn every_op_parses_against_its_endpoint_word_and_method() {
        let cases = [
            ("code", "token", HttpMethod::Post, json!({"code":"C","state":"S"})),
            ("connect", "token", HttpMethod::Post, json!({"code":"C","state":"S","provider":"google"})),
            ("refresh", "token", HttpMethod::Post, json!({"refreshToken":"R"})),
            ("revoke", "revocation", HttpMethod::Post, json!({"token":"T"})),
            ("userinfo", "userinfo", HttpMethod::Get, json!({"accessToken":"A"})),
        ];
        for (op, word, method, inputs) in cases {
            let mut auth = json!({"scheme":"oauth-client","op":op,"client":client()});
            for (k, v) in inputs.as_object().unwrap() {
                auth[k] = v.clone();
            }
            let spec = call(method, word, auth.clone()).unwrap_or_else(|e| panic!("{op}: {e:?}"));
            assert_eq!((spec.origin.as_str(), spec.path.as_str()), ("oauth-endpoint", word));
            assert_eq!(credential_slots_needed(&spec).iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["CID", "CSEC"]);
            let other = if word == "token" { "userinfo" } else { "token" };
            assert!(call(method, other, auth).is_err(), "{op} must not serve {other}");
        }
    }

    #[test]
    fn discovery_is_a_get_against_a_literal_origin_and_never_the_endpoint_target() {
        let auth = json!({"scheme":"oauth-client","op":"discovery","client":client()});
        let spec = parse_http_call(HttpMethod::Get, "https://idp.example.com", "/.well-known/openid-configuration", &val(json!({"auth":auth.clone()}))).unwrap();
        assert!(matches!(spec.auth, Some(HttpAuth::OAuthClient { op: orbital_core_base::oauth_client::OAuthOp::Discovery, .. })));
        assert!(parse_http_call(HttpMethod::Post, "https://idp.example.com", "/x", &val(json!({"auth":auth.clone()}))).is_err());
        assert!(call(HttpMethod::Get, "token", auth).is_err());
        let token = json!({"scheme":"oauth-client","op":"code","client":client(),"code":"C","state":"S"});
        assert!(parse_http_call(HttpMethod::Post, "https://idp.example.com", "/token", &val(json!({"auth":token}))).is_err());
    }

    #[test]
    fn op_code_may_route_by_a_clients_map_keyed_by_tag_and_no_other_op_may() {
        let mut other = client();
        other["tag"] = json!("github");
        let code = |clients: serde_json::Value| json!({"scheme":"oauth-client","op":"code","clients":clients,"code":"C","state":"S"});
        let spec = call(HttpMethod::Post, "token", code(json!({"google": client(), "github": other.clone()}))).unwrap();
        let Some(HttpAuth::OAuthClient { clients, .. }) = &spec.auth else { panic!("oauth") };
        assert_eq!(clients.iter().map(|c| c.tag.name()).collect::<Vec<_>>(), ["google", "github"]);
        assert!(credential_slots_needed(&spec).is_empty(), "a routed call asks credentials only after the take");
        for (what, auth) in [
            ("a key that is not the client's tag", code(json!({"github": client()}))),
            ("an unknown tag", code(json!({"okta": client()}))),
            ("no clients", code(json!({}))),
            ("not an object", code(json!([client()]))),
        ] {
            assert!(call(HttpMethod::Post, "token", auth).is_err(), "{what}");
        }
        let mut both = code(json!({"google": client()}));
        both["client"] = client();
        assert!(call(HttpMethod::Post, "token", both).is_err(), "client and clients");
        let refresh = json!({"scheme":"oauth-client","op":"refresh","clients":{"google": client()},"refreshToken":"R"});
        assert!(call(HttpMethod::Post, "token", refresh).is_err(), "only code routes");
    }

    #[test]
    fn control_a_valid_connect_parses() {
        assert!(call(HttpMethod::Post, "token", connect()).is_ok());
    }

    #[test]
    fn malformed_clients_and_inputs_are_refused() {
        let mut bad = Vec::new();
        let mut a = connect();
        a["op"] = json!("rotate");
        bad.push(("unknown op", a));
        let mut a = connect();
        a.as_object_mut().unwrap().remove("provider");
        bad.push(("missing provider", a));
        let mut a = connect();
        a["extra"] = json!("x");
        bad.push(("unknown key", a));
        let mut a = connect();
        a["provider"] = json!("okta");
        bad.push(("unknown provider", a));
        let mut a = connect();
        a["client"]["endpoints"] = json!({"authorization":"https://a/x","token":"https://a/t"});
        bad.push(("issuer and endpoints", a));
        let mut a = connect();
        a["client"].as_object_mut().unwrap().remove("issuer");
        bad.push(("neither issuer nor endpoints", a));
        let mut a = connect();
        a["client"]["clientAuth"] = json!("digest");
        bad.push(("clientAuth", a));
        let mut a = connect();
        a["client"]["mode"] = json!("fake");
        bad.push(("mode", a));
        let mut a = connect();
        a["client"]["tag"] = json!("okta");
        bad.push(("tag", a));
        let mut a = connect();
        a["client"]["bogus"] = json!(1);
        bad.push(("client key", a));
        for (what, auth) in bad {
            assert!(call(HttpMethod::Post, "token", auth).is_err(), "{what} must be refused");
        }
    }

    #[test]
    fn an_oauth_client_request_carries_nothing_but_auth() {
        for extra in [json!({"json":{}}), json!({"query":{"a":"b"}}), json!({"retry":{"max":1,"on":["5xx"],"backoffMs":1}}), json!({"headers":{"X":"y"}})] {
            let mut request = json!({"auth": connect()});
            for (k, v) in extra.as_object().unwrap() {
                request[k] = v.clone();
            }
            assert!(parse_http_call(HttpMethod::Post, "oauth-endpoint", "token", &val(request)).is_err(), "{extra}");
        }
    }

    #[test]
    fn a_call_without_an_oauth_client_auth_cannot_use_the_endpoint_target() {
        assert!(parse_http_call(HttpMethod::Post, "oauth-endpoint", "token", &val(json!({}))).is_err());
        let bearer = json!({"auth":{"scheme":"bearer","credential":"T"}});
        assert!(parse_http_call(HttpMethod::Post, "oauth-endpoint", "token", &val(bearer)).is_err());
        assert!(parse_http_call(HttpMethod::Post, "oauth-endpoint", "elsewhere", &val(json!({"auth":connect()}))).is_err());
    }

    #[test]
    fn an_unset_client_secret_slot_fails_unconfigured_client_before_any_request() {
        let spec = call(HttpMethod::Post, "token", connect()).unwrap();
        let (mut d, _) = HttpCallDriver::start(spec, vec![], None);
        let step = d.resume(HttpAnswer::Secret(Ok(Some("id".into()))));
        assert!(matches!(step, HttpStep::Ask(HttpAsk::Secret { ref name, .. }) if name == "CSEC"));
        let HttpStep::Done(Err(payload)) = d.resume(HttpAnswer::Secret(Ok(None))) else { panic!("fails") };
        assert_eq!(payload.get_path("kind"), Some(&Value::string("oauth")));
        assert_eq!(payload.get_path("reason"), Some(&Value::string("unconfigured-client")));
        assert_eq!(payload.get_path("code"), Some(&Value::string("NOT_CONFIGURED")));
    }
}


mod tcp_egress {
    use super::*;

    fn declared() -> Vec<String> {
        vec![
            "redis://cache.internal:6379".to_string(),
            "postgres://db.internal".to_string(),
            "https://api.example.com".to_string(),
        ]
    }

    #[test]
    fn a_declared_redis_target_is_allowed_with_its_default_port() {
        let target = tcp_target_allowed("redis://cache.internal", &declared()).expect("declared");
        assert_eq!((target.scheme(), target.host(), target.port()), ("redis", "cache.internal", 6379));
        assert!(tcp_target_allowed("redis://cache.internal:6379", &declared()).is_ok());
    }

    #[test]
    fn a_postgres_target_matches_by_default_port() {
        let target = tcp_target_allowed("postgres://db.internal:5432", &declared()).expect("declared");
        assert_eq!(target.port(), 5432);
    }

    #[test]
    fn an_undeclared_target_is_refused_as_http_refuses() {
        for (target, why) in [
            ("redis://other.internal:6379", "host"),
            ("redis://cache.internal:6380", "port"),
            ("postgres://db.internal:5433", "port"),
        ] {
            assert_eq!(
                tcp_target_allowed(target, &declared()),
                Err(HttpRefusal::EgressDenied(target.to_string())),
                "{why}"
            );
        }
    }

    #[test]
    fn control_the_scheme_must_match_both_ways() {
        let https_only = vec!["https://cache.internal:6379".to_string()];
        assert!(tcp_target_allowed("redis://cache.internal:6379", &https_only).is_err());
        let redis_only = vec!["redis://api.example.com:443".to_string()];
        assert!(!origin_allowed("https://api.example.com", &redis_only));
        assert!(tcp_target_allowed("rediss://cache.internal:6379", &declared()).is_err());
        assert!(tcp_target_allowed("https://api.example.com", &declared()).is_err());
    }

    #[test]
    fn http_calls_to_a_tcp_origin_stay_refused() {
        assert!(parse_http_call(HttpMethod::Get, "redis://cache.internal:6379", "/x", &val(json!({}))).is_err());
        assert!(!origin_allowed("redis://cache.internal:6379", &declared()));
    }
}

#[test]
fn stream_decoder_preserves_utf8_across_every_byte_boundary_and_flushes_the_tail() {
    for framing in [StreamFraming::Sse, StreamFraming::JsonLines] {
        let body = match framing {
            StreamFraming::Sse => "data: ž🌿\r\n\r\ndata: last",
            StreamFraming::JsonLines => "ž🌿\nlast",
        };
        let mut carry = String::new();
        let mut expected = frame_stream(framing, Some("trace"), body, &mut carry);
        expected.extend(frame_stream_finish(framing, Some("trace"), &mut carry));
        for boundary in 0..=body.len() {
            let mut decoder = StreamDecoder::new(framing, Some("trace"));
            let mut events = decoder.push(&body.as_bytes()[..boundary]);
            events.extend(decoder.push(&body.as_bytes()[boundary..]));
            events.extend(decoder.finish());
            assert_eq!(events, expected, "boundary {boundary}, {framing:?}");
        }
    }
}

#[test]
fn stream_decoder_matches_text_response_replacement_of_invalid_and_incomplete_utf8() {
    let mut decoder = StreamDecoder::new(StreamFraming::JsonLines, None);
    assert!(decoder.push(b"a\xff\xe2").is_empty());
    assert!(decoder.push(b"\x82").is_empty());
    assert_eq!(decoder.finish(), vec![val(json!({"data": "a��", "event": null, "id": null, "key": null}))]);
    assert!(decoder.finish().is_empty());
}
