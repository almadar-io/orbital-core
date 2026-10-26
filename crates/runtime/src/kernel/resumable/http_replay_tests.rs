use super::*;
use crate::http_request::StreamFraming;
use std::time::Duration;
use std::sync::atomic::{AtomicUsize, Ordering};

fn call(id: u64, key: Option<&str>) -> PendingRequest {
    PendingRequest {
        id: RequestId(id),
        request: IoRequest::CallService {
            service: "svc".to_string(),
            method: "work".to_string(),
            args: Value::Null,
            key: key.map(str::to_string),
            caller: None,
            site: None,
        },
    }
}

fn widest_overlap(requests: &[PendingRequest]) -> (usize, Vec<IoReply>) {
    let inflight = AtomicUsize::new(0);
    let widest = AtomicUsize::new(0);
    let replies = answer_batch(requests, &|_| {
        let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
        widest.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        inflight.fetch_sub(1, Ordering::SeqCst);
        IoAnswer::Service(Ok(Value::Null))
    });
    (widest.load(Ordering::SeqCst), replies)
}

#[test]
fn keyed_calls_of_one_step_are_answered_at_the_same_time_and_in_request_order() {
    let requests = [call(7, Some("a")), call(8, Some("b")), call(9, Some("c"))];
    let (widest, replies) = widest_overlap(&requests);
    assert!(widest >= 2, "keyed calls ran one at a time: {widest}");
    let ids: Vec<RequestId> = replies.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![RequestId(7), RequestId(8), RequestId(9)]);
}

#[test]
fn control_unkeyed_calls_are_answered_one_at_a_time() {
    let requests = [call(1, None), call(2, None), call(3, None)];
    let (widest, replies) = widest_overlap(&requests);
    assert_eq!(widest, 1);
    assert_eq!(replies.len(), 3);
}

#[test]
fn a_panicking_answerer_becomes_a_failed_call_not_a_crashed_host() {
    let requests = [call(1, Some("a")), call(2, Some("b"))];
    let replies = answer_batch(&requests, &|request| match request {
        IoRequest::CallService { key: Some(key), .. } if key == "a" => panic!("boom"),
        _ => IoAnswer::Service(Ok(Value::Null)),
    });
    assert!(matches!(&replies[0].answer, IoAnswer::Service(Err(message)) if message.contains("panicked")));
    assert!(matches!(&replies[1].answer, IoAnswer::Service(Ok(_))));
}

fn get(url: &str) -> HttpRequest {
    HttpRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers: Vec::new(),
        body: Vec::new(),
        timeout: Some(Duration::from_secs(1)),
    }
}

fn port() -> IoPort {
    IoPort::new(Arc::new(HostAnswered), None, None)
}

fn recorded(request: &HttpRequest) -> Vec<Exchange> {
    vec![Exchange::new(
        IoRequest::Http {
            request: request.clone(),
            key: None,
            delay_ms: None,
            site: None,
        },
        IoAnswer::Http(Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: b"ok".to_vec(),
        })),
    )]
}

#[test]
fn a_replayed_step_is_served_its_recorded_reply_and_asks_nothing() {
    let request = get("https://a.example.com/x");
    let port = port();
    let (reply, end) = port.run_step(recorded(&request), || port.http_send(&request, None, None));
    assert_eq!(reply.map(|r| r.status), Ok(200));
    assert!(end.pending.is_empty() && end.diverged.is_none());
}

#[test]
fn a_replayed_step_that_asks_a_changed_request_diverges() {
    let port = port();
    let (_, end) = port.run_step(recorded(&get("https://a.example.com/x")), || {
        port.http_send(&get("https://a.example.com/y"), None, None)
    });
    assert!(
        matches!(end.diverged, Some(ResumeError::Diverged { .. })),
        "{:?}",
        end.diverged
    );
    assert!(end.pending.is_empty());
}

#[test]
fn a_changed_key_or_a_stream_in_place_of_a_send_also_diverges() {
    let request = get("https://a.example.com/x");
    let port = port();
    let (_, end) = port.run_step(recorded(&request), || port.http_send(&request, Some("k"), None));
    assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    let (_, end) = port.run_step(recorded(&request), || {
        port.http_stream(&request, None, StreamFraming::Sse, None)
    });
    assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
}

#[test]
fn a_replayed_step_is_compared_with_its_delay() {
    let request = get("https://a.example.com/x");
    let port = port();
    let delayed = vec![Exchange::new(
        IoRequest::Http { request: request.clone(), key: None, delay_ms: Some(500), site: None },
        IoAnswer::Http(Ok(HttpResponse { status: 200, headers: Vec::new(), body: b"ok".to_vec() })),
    )];
    let (reply, end) = port.run_step(delayed.clone(), || port.http_send(&request, None, Some(500)));
    assert_eq!(reply.map(|r| r.status), Ok(200));
    assert!(end.pending.is_empty() && end.diverged.is_none());
    // Controls: the same request asked with another delay, or with none, diverges.
    for asked in [Some(501), None] {
        let (_, end) = port.run_step(delayed.clone(), || port.http_send(&request, None, asked));
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })), "{asked:?}");
    }
}

#[test]
fn a_step_with_no_record_leaves_the_request_pending() {
    let request = get("https://a.example.com/x");
    let port = port();
    let (_, end) = port.run_step(Vec::new(), || port.http_send(&request, None, None));
    assert!(matches!(end.pending.as_slice(), [IoRequest::Http { .. }]));
}

struct Canned(Vec<u8>);

impl Transport for Canned {
    fn send(&self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
        Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: self.0.clone(),
        })
    }
}

struct Vaulted;

impl SecretSource for Vaulted {
    fn secret(&self, _: &str) -> Result<Option<String>, String> {
        Ok(Some("sk-live-999".to_string()))
    }
}

fn streamed(key: Option<&str>) -> Vec<Value> {
    let transport = Canned(b"data: a\n\ndata: b".to_vec());
    let seen = Mutex::new(Vec::new());
    let on_event = |_: Option<&CallSite>, event: Value| seen.lock().push(event);
    let host = HttpHost {
        transport: &transport,
        secrets: &Vaulted,
        egress: &[],
        on_event: &on_event,
        os: None,
    };
    let request = IoRequest::HttpStream {
        request: get("https://8.8.8.8/x"),
        key: key.map(str::to_string),
        framing: StreamFraming::Sse,
        site: None,
        continuation: None,
    };
    let IoAnswer::Http(Ok(reply)) = answer_http(Some(&host), &request) else {
        panic!("expected an Http reply")
    };
    assert!(reply.body.is_empty());
    let out = seen.lock().clone();
    out
}

fn keys(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .map(|e| match e {
            Value::Object(fields) => fields.get("key").cloned().unwrap_or(Value::string("absent")),
            other => panic!("event is not an object: {other:?}"),
        })
        .collect()
}

#[test]
fn a_blocking_stream_answer_carries_the_key_the_framer_adds_flush_included() {
    let events = streamed(Some("job"));
    assert_eq!(keys(&events), vec![Value::string("job"); 2]);
}

#[test]
fn control_an_unkeyed_blocking_stream_carries_a_null_key() {
    let events = streamed(None);
    assert_eq!(keys(&events), vec![Value::Null; 2]);
}

#[test]
fn no_debug_of_a_request_answer_or_journal_shows_a_secret_value() {
    let secret = "sk-live-999";
    let authorized = HttpRequest {
        headers: vec![
            ("Authorization".to_string(), format!("Bearer {secret}")),
            ("X-Trace".to_string(), "t-1".to_string()),
        ],
        body: format!("client_secret={secret}").into_bytes(),
        ..get(&format!("https://a.example.com/x?api_key={secret}"))
    };
    let journal = vec![
        Exchange::new(
            IoRequest::Secret { name: "TOKEN".to_string(), principal: None },
            IoAnswer::Secret(Ok(Some(secret.to_string()))),
        ),
        Exchange::new(
            IoRequest::SecretSet {
                name: "oauth2:t".to_string(),
                value: secret.to_string(),
                ttl_ms: 5,
                principal: None,
            },
            IoAnswer::SecretSet(Ok(())),
        ),
        Exchange::new(
            IoRequest::Http {
                request: authorized.clone(),
                key: None,
                delay_ms: None,
                site: None,
            },
            IoAnswer::Http(Ok(HttpResponse {
                status: 200,
                headers: vec![("Set-Cookie".to_string(), secret.to_string())],
                body: format!("{{\"access_token\":\"{secret}\"}}").into_bytes(),
            })),
        ),
    ];
    let shown = format!("{journal:?} {:?}", ResumeError::Diverged {
        recorded: Box::new(journal[1].request.clone()),
        got: Box::new(journal[2].request.clone()),
    });
    assert!(!shown.contains(secret), "leaked: {shown}");
    assert!(!shown.contains("api_key"), "the query is not shown: {shown}");
    assert!(shown.contains("TOKEN") && shown.contains("oauth2:t"), "control: names are shown");
    assert!(shown.contains("X-Trace") && shown.contains("https://a.example.com/x"), "control: headers and url");
}

#[test]
fn a_blocking_random_answer_is_fresh_csprng_output_of_the_asked_length() {
    let ask = IoRequest::Random { bytes: 32 };
    let (IoAnswer::Random(Ok(a)), IoAnswer::Random(Ok(b))) =
        (answer_http(None, &ask), answer_http(None, &ask))
    else {
        panic!("expected random bytes")
    };
    assert_eq!((a.len(), b.len()), (32, 32));
    assert_ne!(a, b);
    assert!(a.iter().any(|byte| *byte != 0));
}

#[test]
fn a_pinned_dispatch_clock_answers_the_clock_ask_without_asking_the_host() {
    let pinned: crate::config::DispatchClock = Arc::new(|| 1_700_000_000_123.0);
    let port = port().with_clock(Some(pinned));
    let (out, end) = port.run_step(Vec::new(), || port.clock());
    assert_eq!(out, Ok(1_700_000_000_123));
    assert!(end.pending.is_empty() && end.diverged.is_none(), "{:?}", end.pending);
}

#[test]
fn control_with_no_pinned_clock_the_clock_ask_goes_to_the_host() {
    let port = port();
    let (_, end) = port.run_step(Vec::new(), || port.clock());
    assert!(matches!(end.pending.as_slice(), [IoRequest::Clock]));
}

#[test]
fn a_random_ask_is_journaled_and_replayed_without_asking_again() {
    let port = port();
    let ask = IoRequest::Random { bytes: 4 };
    let recorded = vec![Exchange::new(ask, IoAnswer::Random(Ok(vec![1, 2, 3, 4])))];
    let (bytes, end) = port.run_step(recorded.clone(), || port.random(4));
    assert_eq!(bytes, Ok(vec![1, 2, 3, 4]));
    assert!(end.pending.is_empty() && end.diverged.is_none());
    let (_, end) = port.run_step(recorded, || port.random(5));
    assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
}

fn os_ask(op: OsOp, path: &str) -> IoRequest {
    IoRequest::Os { op, args: vec![Value::string(path.to_string())], key: None, site: None }
}

fn os_ok() -> IoAnswer {
    IoAnswer::Os(Ok(Value::object(orbital_core_base::value::ValueMap::from_iter([(
        "data".to_string(),
        Value::String(("hi".to_string()).into()),
    )]))))
}

#[test]
fn an_os_ask_is_journaled_and_replayed_without_asking_again() {
    let port = port();
    let args = [Value::string("/a")];
    let recorded = vec![Exchange::new(os_ask(OsOp::ReadFile, "/a"), os_ok())];
    let (out, end) = port.run_step(recorded.clone(), || port.os(OsOp::ReadFile, &args, None));
    assert_eq!(out.map(|v| v.get_path("data").map(Value::to_string_value)), Ok(Some("hi".to_string())));
    assert!(end.pending.is_empty() && end.diverged.is_none());
}

#[test]
fn a_changed_os_ask_is_diverged() {
    let port = port();
    let recorded = vec![Exchange::new(os_ask(OsOp::ReadFile, "/a"), os_ok())];
    let other_path = [Value::string("/b")];
    let (_, end) = port.run_step(recorded.clone(), || port.os(OsOp::ReadFile, &other_path, None));
    assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    let same_path = [Value::string("/a")];
    let (_, end) = port.run_step(recorded.clone(), || port.os(OsOp::WriteFile, &same_path, None));
    assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    let (_, end) = port.run_step(recorded, || port.os(OsOp::ReadFile, &same_path, Some("k")));
    assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
}

#[test]
fn an_os_failure_answer_replays_as_the_same_failure() {
    let port = port();
    let failure = OsError::new(OsErrorKind::NotFound, "no such file");
    let recorded = vec![Exchange::new(os_ask(OsOp::Stat, "/gone"), IoAnswer::Os(Err(failure.clone())))];
    let (out, end) = port.run_step(recorded, || port.os(OsOp::Stat, &[Value::string("/gone")], None));
    assert_eq!(out, Err(failure));
    assert!(end.diverged.is_none());
}

#[test]
fn an_os_ask_is_answered_only_by_an_os_answer_or_a_cancel() {
    let ask = os_ask(OsOp::ReadFile, "/a");
    assert!(os_ok().answers(&ask));
    assert!(IoAnswer::ServiceCancelled.answers(&ask));
    assert!(!IoAnswer::Random(Ok(vec![1])).answers(&ask));
    assert!(!IoAnswer::Http(Err(TransportError::Failed("x".into()))).answers(&ask));
    assert!(!os_ok().answers(&IoRequest::Random { bytes: 1 }));
    assert!(!os_ok().answers(&IoRequest::CancelCall { key: "k".into() }));
}

#[test]
fn only_a_streaming_os_ask_carries_the_call_site() {
    let port = port();
    port.set_call_site(Some(CallSite {
        orbital: None,
        trait_name: "T".into(),
        follow_on: false,
        entity_id: "singleton".to_string(),
        emit: CallRoutes::default(),
    }));
    let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::ReadFile, &[Value::Null], None));
    let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
    assert!(site.is_none());
    let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::StdinRead, &[], Some("k")));
    let [IoRequest::Os { site, key, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
    assert_eq!(site.as_ref().map(|s| s.trait_name.clone()), Some("T".to_string()));
    assert_eq!(key.as_deref(), Some("k"));
}

fn routed_site() -> CallSite {
    CallSite {
        orbital: None,
        trait_name: "T".into(),
        follow_on: false,
        entity_id: "singleton".to_string(),
        emit: CallRoutes { success: Some("DONE".into()), failure: Some("FAILED".into()), ..CallRoutes::default() },
    }
}

fn wait_ask(key: Option<&str>, site: Option<CallSite>) -> IoRequest {
    IoRequest::Os { op: OsOp::Wait, args: vec![Value::Number(7.0)], key: key.map(str::to_string), site }
}

#[test]
fn a_keyed_wait_under_a_completion_route_carries_the_call_site_and_the_controls_do_not() {
    let port = port();
    port.set_call_site(Some(routed_site()));
    let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::Wait, &[Value::Number(7.0)], Some("k")));
    let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
    assert_eq!(site.as_ref().map(|s| s.emit.success.clone()), Some(Some("DONE".to_string())));
    let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::Wait, &[Value::Number(7.0)], None));
    let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
    assert!(site.is_none());
    let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::Stat, &[Value::Null], Some("k")));
    let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
    assert_eq!(site.as_ref().map(|s| s.emit.success.clone()), Some(Some("DONE".to_string())));
    port.set_call_site(Some(CallSite { emit: CallRoutes::default(), ..routed_site() }));
    let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::Wait, &[Value::Number(7.0)], Some("k")));
    let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
    assert!(site.is_none());
}

#[test]
fn keyed_wait_and_finite_fs_with_a_call_site_may_be_answered_deferred() {
    let deferred = IoAnswer::ServiceDeferred;
    assert!(deferred.answers(&wait_ask(Some("k"), Some(routed_site()))));
    assert!(!deferred.answers(&wait_ask(None, Some(routed_site()))));
    assert!(!deferred.answers(&wait_ask(Some("k"), None)));
    let stat = IoRequest::Os { op: OsOp::Stat, args: vec![Value::Null], key: Some("k".into()), site: Some(routed_site()) };
    assert!(deferred.answers(&stat));
    let stat_without_site = IoRequest::Os { op: OsOp::Stat, args: vec![Value::Null], key: Some("k".into()), site: None };
    assert!(!deferred.answers(&stat_without_site));
    let stat_without_key = IoRequest::Os { op: OsOp::Stat, args: vec![Value::Null], key: None, site: Some(routed_site()) };
    assert!(!deferred.answers(&stat_without_key));
    assert!(os_ok().answers(&wait_ask(None, None)));
}

#[test]
fn a_deferred_wait_returns_null_with_the_deferred_verdict_and_a_plain_answer_leaves_none() {
    let port = port();
    port.set_call_site(Some(routed_site()));
    let ask = wait_ask(Some("k"), Some(routed_site()));
    let args = [Value::Number(7.0)];
    let (out, end) = port.run_step(vec![Exchange::new(ask.clone(), IoAnswer::ServiceDeferred)], || {
        port.os(OsOp::Wait, &args, Some("k"))
    });
    assert_eq!(out, Ok(Value::Null));
    assert_eq!(port.take_call_verdict(), Some(CallVerdict::Deferred));
    assert!(end.pending.is_empty() && end.diverged.is_none());
    let (out, _) = port.run_step(vec![Exchange::new(ask, os_ok())], || port.os(OsOp::Wait, &args, Some("k")));
    assert!(out.is_ok_and(|v| v.get_path("data").is_some()));
    assert_eq!(port.take_call_verdict(), None);
}

#[test]
fn the_native_answerer_is_reached_through_the_one_dispatch_point() {
    let ask = os_ask(OsOp::ReadFile, "/a");
    assert!(matches!(answer_http(None, &ask), IoAnswer::Os(Err(OsError { kind: OsErrorKind::Denied, .. }))));
    assert!(!matches!(answer_http(None, &IoRequest::CancelCall { key: "k".into() }), IoAnswer::Os(_)));
}

#[test]
fn a_secret_ask_for_another_principal_diverges_from_the_recorded_one() {
    let port = port();
    let recorded = vec![Exchange::new(
        IoRequest::Secret { name: "t".into(), principal: Some("u1".into()) },
        IoAnswer::Secret(Ok(Some("v".into()))),
    )];
    let (value, end) = port.run_step(recorded.clone(), || port.secret("t", Some("u1")));
    assert_eq!(value, Ok(Some("v".to_string())));
    assert!(end.diverged.is_none());
    let (_, end) = port.run_step(recorded.clone(), || port.secret("t", Some("u2")));
    assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    let (_, end) = port.run_step(recorded, || port.secret("t", None));
    assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
}

struct PerUser;

impl SecretSource for PerUser {
    fn secret(&self, _: &str) -> Result<Option<String>, String> {
        Ok(Some("app".to_string()))
    }
    fn secret_of(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
        match principal {
            None => self.secret(name),
            Some("u1") => Ok(Some("mine".to_string())),
            Some(_) => Ok(None),
        }
    }
    fn forget_of(&self, _: &str, principal: Option<&str>) -> Result<(), String> {
        principal.map(|_| ()).ok_or_else(|| "no app-wide forget".to_string())
    }
}

#[test]
fn a_blocking_host_answers_credential_asks_per_principal() {
    let transport = Canned(Vec::new());
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let host = HttpHost { transport: &transport, secrets: &PerUser, egress: &[], on_event: &on_event, os: None };
    let status = |principal: Option<&str>| {
        answer_http(
            Some(&host),
            &IoRequest::CredentialStatus { name: "t".into(), principal: principal.map(str::to_string) },
        )
    };
    assert_eq!(status(Some("u1")), IoAnswer::CredentialStatus(Ok(crate::credential_admin::CredentialStanding::of(true))));
    assert_eq!(status(Some("u2")), IoAnswer::CredentialStatus(Ok(crate::credential_admin::CredentialStanding::of(false))));
    let forget = answer_http(
        Some(&host),
        &IoRequest::CredentialForget { name: "t".into(), principal: Some("u1".into()) },
    );
    assert_eq!(forget, IoAnswer::CredentialForget(Ok(())));
    assert_eq!(
        answer_http(None, &IoRequest::CredentialStatus { name: "t".into(), principal: None }),
        IoAnswer::CredentialStatus(Err(NO_HTTP_HOST.to_string()))
    );
}

#[test]
fn a_default_secret_source_keeps_no_user_credentials() {
    let user = Vaulted.secret_of("t", Some("u1"));
    assert!(user.is_err(), "{user:?}");
    assert_eq!(Vaulted.secret_of("t", None), Ok(Some("sk-live-999".to_string())));
    assert!(Vaulted.forget_of("t", Some("u1")).is_err());
}

#[test]
fn outside_a_step_an_http_ask_fails_for_want_of_a_host() {
    let port = port();
    assert_eq!(
        port.http_send(&get("https://a.example.com/x"), None, None).map(|r| r.status),
        Err(TransportError::Failed(NO_HTTP_HOST.to_string()))
    );
}

fn keyed(key: &str) -> IoRequest {
    IoRequest::CallService {
        service: "svc".into(),
        method: "go".into(),
        args: Value::Null,
        key: Some(key.into()),
        caller: None,
        site: None,
    }
}

fn answered(key: &str, text: &str) -> Exchange {
    Exchange::new(
        keyed(key),
        IoAnswer::Service(Ok(Value::string(text))),
    )
}

fn call_keyed(port: &IoPort, key: &str) -> Result<Value, String> {
    port.call("svc", "go", &Value::Null, Some(key), None)
}

fn call_unkeyed(port: &IoPort) -> Result<Value, String> {
    port.call("svc", "go", &Value::Null, None, None)
}

#[test]
fn keyed_calls_are_all_pending_in_issue_order_and_each_returns_the_deferred_value() {
    let port = port();
    let (out, end) = port.run_step(Vec::new(), || {
        (
            call_keyed(&port, "a"),
            call_keyed(&port, "b"),
            call_keyed(&port, "c"),
        )
    });
    assert_eq!(out, (Ok(Value::Null), Ok(Value::Null), Ok(Value::Null)));
    assert_eq!(end.pending.as_slice(), vec![keyed("a"), keyed("b"), keyed("c")].as_slice());
    assert!(end.diverged.is_none());
    assert_eq!(port.take_call_verdict(), Some(CallVerdict::Deferred));
}

#[test]
fn control_an_unkeyed_ask_is_the_one_pending_request_and_withholds_every_later_ask() {
    let port = port();
    let (out, end) = port.run_step(Vec::new(), || {
        (
            call_unkeyed(&port),
            call_keyed(&port, "a"),
            call_unkeyed(&port),
        )
    });
    assert_eq!(out.0, Err(AWAITING_HOST.to_string()));
    assert_eq!(out.1, Err(AWAITING_HOST.to_string()));
    assert_eq!(out.2, Err(AWAITING_HOST.to_string()));
    assert_eq!(end.pending.len(), 1);
    assert!(matches!(
        &end.pending[0],
        IoRequest::CallService { key: None, .. }
    ));
}

#[test]
fn control_an_unkeyed_ask_after_keyed_ones_is_withheld_and_not_recorded() {
    let port = port();
    let (out, end) = port.run_step(Vec::new(), || {
        (
            call_keyed(&port, "a"),
            call_unkeyed(&port),
            call_keyed(&port, "b"),
        )
    });
    assert_eq!(out.0, Ok(Value::Null));
    assert_eq!(out.1, Err(AWAITING_HOST.to_string()));
    assert_eq!(out.2, Err(AWAITING_HOST.to_string()));
    assert_eq!(end.pending.as_slice(), vec![keyed("a")].as_slice());
}

#[test]
fn a_keyed_call_is_served_its_answer_by_key_whatever_order_the_journal_holds() {
    let port = port();
    let expected = (Ok(Value::string("A")), Ok(Value::string("B")));
    for journal in [
        vec![answered("a", "A"), answered("b", "B")],
        vec![answered("b", "B"), answered("a", "A")],
    ] {
        let (out, end) =
            port.run_step(journal, || (call_keyed(&port, "a"), call_keyed(&port, "b")));
        assert_eq!(out, expected);
        assert!(end.pending.is_empty() && end.diverged.is_none());
    }
}

#[test]
fn control_a_keyed_call_whose_request_changed_since_it_was_recorded_diverges() {
    let port = port();
    let (_, end) = port.run_step(vec![answered("a", "A")], || {
        port.call("svc", "go", &Value::Bool(true), Some("a"), None)
    });
    assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    let (_, end) = port.run_step(vec![answered("a", "A")], || call_keyed(&port, "b"));
    assert!(end.diverged.is_none());
    assert_eq!(end.pending.as_slice(), vec![keyed("b")].as_slice());
}

#[test]
fn a_key_used_twice_in_one_step_is_a_visible_error_and_the_first_call_stands() {
    let port = port();
    let (out, end) = port.run_step(Vec::new(), || {
        (call_keyed(&port, "a"), call_keyed(&port, "a"))
    });
    assert_eq!(out.0, Ok(Value::Null));
    assert!(
        out.1
            .as_ref()
            .is_err_and(|e| e.contains("already used by another call")),
        "{:?}",
        out.1
    );
    assert_eq!(end.pending.as_slice(), vec![keyed("a")].as_slice());
    let (out, end) = port.run_step(vec![answered("a", "A")], || {
        (call_keyed(&port, "a"), call_keyed(&port, "a"))
    });
    assert_eq!(out.0, Ok(Value::string("A")));
    assert!(
        out.1
            .as_ref()
            .is_err_and(|e| e.contains("already used by another call")),
        "{:?}",
        out.1
    );
    assert!(end.pending.is_empty());
}

#[test]
fn cancelling_a_pending_keyed_call_removes_only_that_request_and_the_cancel_asks_nothing() {
    let port = port();
    let (out, end) = port.run_step(Vec::new(), || {
        let _ = call_keyed(&port, "a");
        let _ = call_keyed(&port, "b");
        let _ = call_keyed(&port, "c");
        port.cancel("b")
    });
    assert_eq!(out, Ok(()));
    assert_eq!(end.pending.as_slice(), vec![keyed("a"), keyed("c")].as_slice());
}

#[test]
fn control_cancelling_a_key_that_is_not_pending_in_the_step_is_an_ask_of_its_own() {
    let port = port();
    let (out, end) = port.run_step(Vec::new(), || {
        let _ = call_keyed(&port, "a");
        port.cancel("zzz")
    });
    assert_eq!(out, Err(AWAITING_HOST.to_string()));
    assert_eq!(end.pending.as_slice(), vec![keyed("a")].as_slice());
    let (out, end) = port.run_step(Vec::new(), || port.cancel("zzz"));
    assert_eq!(out, Err(AWAITING_HOST.to_string()));
    assert_eq!(
        end.pending.as_slice(),
        [IoRequest::CancelCall { key: "zzz".into() }]
    );
}

#[test]
fn a_cancelled_call_replays_without_diverging_from_the_answers_of_the_others() {
    let port = port();
    let (out, end) = port.run_step(vec![answered("b", "B")], || {
        let _ = call_keyed(&port, "a");
        let kept = call_keyed(&port, "b");
        (kept, port.cancel("a"))
    });
    assert_eq!(out, (Ok(Value::string("B")), Ok(())));
    assert!(end.pending.is_empty() && end.diverged.is_none());
}

#[test]
fn replies_in_any_order_become_the_same_exchanges_in_issue_order() {
    let pending: Vec<PendingRequest> = ["a", "b", "c"]
        .iter()
        .enumerate()
        .map(|(i, key)| PendingRequest {
            id: RequestId::from_raw(i as u64),
            request: keyed(key),
        })
        .collect();
    let reply = |i: u64| IoReply {
        id: RequestId::from_raw(i),
        answer: IoAnswer::Service(Ok(Value::Number(i as f64))),
    };
    let in_order = accept_replies(&pending, vec![reply(0), reply(1), reply(2)]).unwrap();
    for order in [[2, 1, 0], [1, 2, 0], [0, 2, 1], [2, 0, 1], [1, 0, 2]] {
        let got = accept_replies(&pending, order.iter().map(|i| reply(*i)).collect()).unwrap();
        assert_eq!(got.len(), in_order.len());
        for (g, e) in got.iter().zip(&in_order) {
            assert!(g.request == e.request && g.answer == e.answer);
        }
    }
}

#[test]
fn replies_that_do_not_fit_the_pending_requests_are_refused_by_kind() {
    let pending: Vec<PendingRequest> = ["a", "b"]
        .iter()
        .enumerate()
        .map(|(i, key)| PendingRequest {
            id: RequestId::from_raw(i as u64),
            request: keyed(key),
        })
        .collect();
    let reply = |i: u64| IoReply {
        id: RequestId::from_raw(i),
        answer: IoAnswer::Service(Ok(Value::Null)),
    };
    assert!(
        matches!(accept_replies(&pending, vec![reply(1)]), Err(ResumeError::Unanswered(id)) if id.raw() == 0)
    );
    assert!(
        matches!(accept_replies(&pending, vec![reply(0), reply(1), reply(7)]), Err(ResumeError::Unknown(id)) if id.raw() == 7)
    );
    assert!(
        matches!(accept_replies(&pending, vec![reply(0), reply(0), reply(1)]), Err(ResumeError::Duplicate(id)) if id.raw() == 0)
    );
    assert!(accept_replies(&pending, vec![reply(1), reply(0)]).is_ok());
}

#[test]
fn a_keyed_os_or_http_ask_may_be_left_pending_and_an_unkeyed_one_may_not() {
    let deferred = |request: IoRequest| {
        accept_replies(
            &[PendingRequest { id: RequestId::from_raw(0), request }],
            vec![IoReply { id: RequestId::from_raw(0), answer: IoAnswer::ServiceDeferred }],
        )
    };
    let os = |key: Option<&str>| IoRequest::Os {
        op: OsOp::Getenv,
        args: vec![Value::string("HOME")],
        key: key.map(str::to_string),
        site: None,
    };
    let http = |key: Option<&str>| IoRequest::Http {
        request: get("https://a.example.com/x"),
        key: key.map(str::to_string),
        delay_ms: None,
        site: None,
    };
    assert!(deferred(os(Some("home"))).is_ok(), "a keyed os/getenv may stay pending");
    assert!(deferred(http(Some("fetch"))).is_ok(), "a keyed http may stay pending");
    assert!(
        matches!(deferred(os(None)), Err(ResumeError::WrongKind(_))),
        "control: an unkeyed os ask has nothing to answer it later"
    );
    assert!(matches!(deferred(http(None)), Err(ResumeError::WrongKind(_))), "control: nor an unkeyed http");
}
