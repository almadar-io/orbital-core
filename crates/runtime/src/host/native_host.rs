//! The one native host of a server-role kernel: every ask a dispatch makes, answered in process. Rows come from the
//! entity source, `http` and `os/` under the program's declared egress and grants, credentials, OAuth, random and clock
//! from the secret source and the operating system. Each piece is the existing answerer (`BlockingHost`, `HttpHost`,
//! `answer_os`); this owns them so a host can share one answerer across threads and rebuild it when a program registers.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::effects::executor::{ServiceCaller, ServiceHandler, SubstrateFailure, SubstrateHandler};
use crate::entity_source::EntitySource;
use crate::http_service::{SecretSource, Transport};
use crate::os_host::{OsGrants, OsHandles, OsHost};
use crate::resumable::{BlockingHost, CallSite, HttpHost, IoAnswer, IoRequest};
use orbital_core_base::value::Value;
use std::sync::Arc;

/// Receives each event of a stream, with the call site whose `onMessage` route it belongs to.
pub type StreamEvents = dyn Fn(Option<&CallSite>, Value) + Send + Sync;

struct LiveHttpStream {
    inner: Arc<dyn Transport>,
    completions: Arc<crate::os_host::completion::Completions>,
    key: String,
    lease: u64,
}

impl LiveHttpStream {
    fn check(&self) -> Result<(), crate::http_service::TransportError> {
        self.completions.is_current(&self.key, self.lease).then_some(()).ok_or_else(|| {
            crate::http_service::TransportError::Failed("HTTP stream cancelled".into())
        })
    }
}

impl Transport for LiveHttpStream {
    fn send(&self, request: &crate::http_service::HttpRequest) -> Result<crate::http_service::HttpResponse, crate::http_service::TransportError> {
        self.check()?;
        let reply = self.inner.send(request)?;
        self.check()?;
        Ok(reply)
    }

    fn stream(&self, request: &crate::http_service::HttpRequest, on_chunk: &mut dyn FnMut(&[u8]) -> Result<(), crate::http_service::TransportError>) -> Result<crate::http_service::HttpResponse, crate::http_service::TransportError> {
        self.check()?;
        self.inner.stream(request, &mut |bytes| {
            self.check()?;
            on_chunk(bytes)?;
            self.check()
        })
    }
}

/// The work of one keyed substrate op answered as a dispatch of its own (R-a), built on the asking thread and run off
/// it; a host that serves the op as the asking viewer captures that viewer when it builds the work.
pub type SubstrateWork = Box<dyn FnOnce() -> Result<Value, SubstrateFailure> + Send>;

/// Builds the [`SubstrateWork`] of a deferred op from its name and arguments.
pub type SubstrateLater = dyn Fn(&str, &[Value]) -> SubstrateWork + Send + Sync;

/// The source of one substrate stream, polled off the asking thread.
pub type SubstrateSource = Box<dyn FnMut() -> crate::os_host::completion::StreamPoll + Send>;

/// Opens the [`SubstrateSource`] of a streaming op from its name and arguments; `None` for an op that does not stream.
pub type SubstrateStreams = dyn Fn(&str, &[Value]) -> Option<SubstrateSource> + Send + Sync;

/// The refusal every `call-service` gets from a native host: it has no provider for any service (hosting and the registry
/// are `@almadar-io/hosting`'s, answered by the JS host; see G-CROSS-170). The wording is the JS host's `NOT_CONFIGURED`
/// integration error, so a program sees one refusal on every host.
struct NoProviders;

impl ServiceHandler for NoProviders {
    fn call(&self, service: &str, method: &str, _: &Value, _: Option<&str>, _: Option<&ServiceCaller>) -> Result<Value, String> {
        Err(format!("Integration not configured: {service}.{method}"))
    }

    fn cancel(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct NativeHost {
    source: Arc<dyn EntitySource>,
    inline_http: bool,
    transport: Arc<dyn Transport>,
    secrets: Arc<dyn SecretSource>,
    egress: Vec<String>,
    grants: OsGrants,
    handles: Arc<OsHandles>,
    on_event: Arc<StreamEvents>,
    substrate: Option<Arc<dyn SubstrateHandler>>,
    substrate_later: Option<Arc<SubstrateLater>>,
    substrate_streams: Option<Arc<SubstrateStreams>>,
}

impl NativeHost {
    pub fn new(
        source: Arc<dyn EntitySource>,
        transport: Arc<dyn Transport>,
        secrets: Arc<dyn SecretSource>,
        egress: Vec<String>,
        grants: OsGrants,
        handles: Arc<OsHandles>,
        on_event: Arc<StreamEvents>,
    ) -> Self {
        let (grants, problems) = grants.resolve_anchors(&handles.anchors);
        for problem in problems {
            tracing::warn!(target: orbital_core_base::logging::ns::RUNTIME_EFFECTS, at = %problem.path, message = %problem.message, "os grant anchor");
        }
        Self {
            source,
            inline_http: false,
            transport,
            secrets,
            egress,
            grants,
            handles,
            on_event,
            substrate: None,
            substrate_later: None,
            substrate_streams: None,
        }
    }

    /// Answer the substrate operators (`program/*`, `behavior/*`) through `handler`; without one they fail.
    pub fn with_substrate(mut self, handler: Option<Arc<dyn SubstrateHandler>>) -> Self {
        self.substrate = handler;
        self
    }

    /// How a deferred keyed substrate op runs; without it the substrate handler runs the op on the worker thread.
    pub fn with_substrate_later(mut self, later: Option<Arc<SubstrateLater>>) -> Self {
        self.substrate_later = later;
        self
    }

    /// Which substrate ops stream to their `onMessage` route when keyed, and how their items are read.
    pub fn with_substrate_streams(mut self, streams: Option<Arc<SubstrateStreams>>) -> Self {
        self.substrate_streams = streams;
        self
    }

    /// A keyed substrate op with a declared route, when a completion sink is installed, runs as a dispatch of its own
    /// (R-a): the ask is answered `ServiceDeferred` and its outcome settles through the sink.
    fn defer_substrate(&self, request: &IoRequest) -> Option<IoAnswer> {
        let IoRequest::Substrate { op, args, key: Some(key), site: Some(site) } = request else {
            return None;
        };
        self.handles.completions.sink()?;
        if let (Some(streams), Some(_)) = (&self.substrate_streams, &site.emit.on_message) {
            if let Some(source) = streams(op, args) {
                return Some(crate::os_host::completion::Completions::open_substrate_stream(&self.handles.completions, key, site, source));
            }
        }
        let work = match (&self.substrate_later, &self.substrate) {
            (Some(later), _) => later(op, args),
            (None, Some(handler)) => {
                let (handler, op, args) = (handler.clone(), op.clone(), args.clone());
                Box::new(move || handler.run(&op, &args)) as SubstrateWork
            }
            (None, None) => return None,
        };
        Some(crate::os_host::completion::Completions::defer_substrate(&self.handles.completions, key, site, op, args, work))
    }

    fn defer_http(&self, request: &IoRequest) -> Option<IoAnswer> {
        use crate::driver::{drive, DriveContext, DriveEnd, DriverAnswer};
        use crate::resumable::{Exchange, IoPort};
        let IoRequest::HttpStream { key: Some(key), site: Some(site), continuation: Some(driver), .. } = request else {
            return None;
        };
        if self.inline_http || self.handles.completions.sink().is_none() {
            return None;
        }
        let completions = self.handles.completions.clone();
        let (key, site, driver, request) = (key.clone(), site.clone(), driver.clone(), request.clone());
        let mut worker = self.clone();
        worker.inline_http = true;
        let (route_key, route_site) = (key.clone(), site.clone());
        let completions_for_work = completions.clone();
        Some(crate::os_host::completion::Completions::defer_http(&completions, &route_key, &route_site, move |lease| {
            worker.transport = Arc::new(LiveHttpStream { inner: worker.transport.clone(), completions: completions_for_work.clone(), key: key.clone(), lease });
            worker.on_event = Arc::new({
                let (completions, key, site) = (completions_for_work.clone(), key.clone(), site.clone());
                move |_, event| { completions.message(&key, lease, &site, event); }
            });
            let first = match worker.answer(&request) {
                IoAnswer::Http(reply) => DriverAnswer::Http(reply),
                _ => return Err(Value::string("native HTTP stream did not return an HTTP answer")),
            };
            let port = IoPort::new(worker.source.clone(), None, worker.substrate.clone());
            port.set_call_site(Some(site));
            let mut journal = Vec::new();
            loop {
                let mut resumed = driver.clone();
                let principal = resumed.principal().map(str::to_string);
                let step = resumed.resume(first.clone());
                let (outcome, end) = port.run_step(journal, || {
                    drive(&port, &mut *resumed, step, DriveContext { principal: principal.as_deref(), key: Some(&key) })
                });
                if let Some(error) = end.diverged {
                    return Err(Value::string(error.to_string()));
                }
                journal = end.journal;
                if !end.pending.is_empty() {
                    for ask in end.pending {
                        let answer = worker.answer(&ask);
                        journal.push(Exchange::new(ask, answer));
                    }
                    continue;
                }
                return match outcome {
                    DriveEnd::Done(result) => result,
                    _ => Err(Value::string("native HTTP continuation has an unanswered host request")),
                };
            }
        }))
    }

    pub fn answer(&self, request: &IoRequest) -> IoAnswer {
        if let Some(deferred) = self.defer_http(request) {
            return deferred;
        }
        if let Some(deferred) = self.defer_substrate(request) {
            return deferred;
        }
        let on_event = |site: Option<&CallSite>, event: Value| (self.on_event)(site, event);
        let os = OsHost {
            grants: &self.grants,
            handles: &self.handles,
            on_event: &on_event,
        };
        let http = HttpHost {
            transport: self.transport.as_ref(),
            secrets: self.secrets.as_ref(),
            egress: &self.egress,
            on_event: &on_event,
            os: Some(&os),
        };
        BlockingHost {
            source: self.source.as_ref(),
            service: Some(&NoProviders),
            substrate: self.substrate.as_deref(),
        }
        .answer_with_http(&http, request)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::effects::executor::SubstrateFailure;
    use crate::entity_source::InMemoryEntityCache;
    use crate::http_service::{HttpRequest, HttpResponse, TransportError};

    struct Echo;
    pub(crate) struct Offline;

    impl Transport for Offline {
        fn send(&self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
            unreachable!("a substrate ask sends no request")
        }
    }

    impl SubstrateHandler for Echo {
        fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
            Ok(Value::string(format!("{op}/{}", args.len())))
        }
    }

    fn host() -> NativeHost {
        NativeHost::new(
            Arc::new(InMemoryEntityCache::new()),
            Arc::new(Offline),
            Arc::new(crate::credentials::Credentials::from_env(Default::default())),
            Vec::new(),
            OsGrants::default(),
            Arc::new(OsHandles::default()),
            Arc::new(|_: Option<&CallSite>, _: Value| {}),
        )
    }

    fn ask(host: &NativeHost) -> IoAnswer {
        host.answer(&IoRequest::Substrate { op: "program/read".to_string(), args: vec![Value::Null], key: None, site: None })
    }

    #[test]
    fn a_deferred_http_stream_resumes_the_canonical_driver_for_401_refresh() {
        use crate::driver::{DriverAnswer, DriverAsk, DriverStep};
        use crate::http_request::{parse_http_call, HttpCallDriver, HttpMethod, StreamFraming};
        use crate::http_service::SecretSource;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct RefreshTransport(AtomicUsize);
        impl Transport for RefreshTransport {
            fn send(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
                assert_eq!(request.url, "https://8.8.4.4/token");
                Ok(HttpResponse { status: 200, headers: vec![], body: br#"{"access_token":"FRESH","expires_in":3600}"#.to_vec() })
            }
            fn stream(&self, request: &HttpRequest, on_chunk: &mut dyn FnMut(&[u8]) -> Result<(), TransportError>) -> Result<HttpResponse, TransportError> {
                let call = self.0.fetch_add(1, Ordering::SeqCst);
                let expected = if call == 0 { "Bearer OLD" } else { "Bearer FRESH" };
                assert!(request.headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("authorization") && value == expected));
                if call == 1 { on_chunk(b"data: live\n\n")?; }
                Ok(HttpResponse { status: if call == 0 { 401 } else { 200 }, headers: vec![], body: vec![] })
            }
        }
        struct Secrets;
        impl SecretSource for Secrets {
            fn secret(&self, _: &str) -> Result<Option<String>, String> { Ok(Some("credential".into())) }
            fn store_secret(&self, _: &str, _: &str, _: u64) -> Result<(), String> { Ok(()) }
        }
        let spec: Value = serde_json::from_value(serde_json::json!({"stream": "sse", "auth": {"scheme": "oauth2-client", "tokenUrl": "https://8.8.4.4/token", "clientId": "CID", "clientSecret": "CSEC", "onUnauthorized": "refresh"}})).expect("spec");
        let spec = parse_http_call(HttpMethod::Get, "https://8.8.8.8", "/trace", &spec).expect("parse");
        let egress = vec!["https://8.8.8.8".into(), "https://8.8.4.4".into()];
        let (mut driver, _) = HttpCallDriver::start(spec, egress.clone(), Some("trace".into()));
        let DriverStep::Ask(DriverAsk::HttpStream { request, .. }) = driver.resume(DriverAnswer::Secret(Ok(Some("OLD".into())))) else { panic!("stream ask") };
        let (mut host, settled) = sinking_host(true);
        let transport = Arc::new(RefreshTransport(AtomicUsize::new(0)));
        host.transport = transport.clone();
        host.secrets = Arc::new(Secrets);
        host.egress = egress;
        let site = match keyed("unused", Some("trace")) { IoRequest::Substrate { site, .. } => site, _ => unreachable!() };
        let ask = IoRequest::HttpStream { request, key: Some("trace".into()), site, framing: StreamFraming::Sse, continuation: Some(Box::new(driver)) };
        assert!(matches!(host.answer(&ask), IoAnswer::ServiceDeferred));
        let (_, outcome) = settled.recv_timeout(std::time::Duration::from_secs(3)).expect("settled");
        let payload = outcome.expect("success");
        assert_eq!(payload.get_path("status"), Some(&Value::Number(200.0)));
        assert_eq!(transport.0.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_substrate_ask_reaches_the_handler_the_host_was_given() {
        match ask(&host().with_substrate(Some(Arc::new(Echo)))) {
            IoAnswer::Substrate(Ok(Value::String(answer))) => assert_eq!(answer, "program/read/1"),
            other => panic!("expected the handler's answer, got {other:?}"),
        }
    }

    #[test]
    fn control_a_host_without_a_handler_names_the_op_it_cannot_answer() {
        match ask(&host()) {
            IoAnswer::Substrate(Err(failure)) => assert_eq!(failure.message, "no substrate host configured for program/read"),
            other => panic!("expected the refusal, got {other:?}"),
        }
    }

    struct Settled(orbital_core_base::sync::Mutex<std::sync::mpsc::Sender<(String, Result<Value, Value>)>>);

    impl crate::os_host::completion::OsCompletionSink for Settled {
        fn complete(&self, _: &CallSite, _: &str, _: Result<Value, orbital_core_base::os::OsError>) {}

        fn settle(&self, _: &CallSite, key: &str, outcome: Result<Value, Value>) {
            self.0.lock().send((key.to_string(), outcome)).unwrap();
        }
    }

    fn sinking_host(sink: bool) -> (NativeHost, std::sync::mpsc::Receiver<(String, Result<Value, Value>)>) {
        let handles = Arc::new(OsHandles::default());
        let (tx, rx) = std::sync::mpsc::channel();
        if sink {
            handles.completions.set_sink(Arc::new(Settled(orbital_core_base::sync::Mutex::new(tx))));
        }
        let host = NativeHost::new(
            Arc::new(InMemoryEntityCache::new()),
            Arc::new(Offline),
            Arc::new(crate::credentials::Credentials::from_env(Default::default())),
            Vec::new(),
            OsGrants::default(),
            handles,
            Arc::new(|_: Option<&CallSite>, _: Value| {}),
        )
        .with_substrate(Some(Arc::new(Echo)));
        (host, rx)
    }

    fn keyed(op: &str, key: Option<&str>) -> IoRequest {
        IoRequest::Substrate {
            op: op.to_string(),
            args: vec![Value::Null],
            key: key.map(str::to_string),
            site: Some(CallSite {
                orbital: None,
                trait_name: "Agent".to_string(),
                follow_on: false,
                entity_id: "singleton".to_string(),
                emit: crate::resumable::CallRoutes { success: Some("DONE".to_string()), ..Default::default() },
            }),
        }
    }

    fn object(pairs: &[(&str, Value)]) -> Value {
        Value::Object(pairs.iter().map(|(k, v)| (k.to_string().into(), v.clone())).collect())
    }

    #[test]
    fn a_keyed_routed_substrate_ask_is_answered_later_through_the_sink() {
        let (host, settled) = sinking_host(true);
        assert!(matches!(host.answer(&keyed("program/read", Some("k1"))), IoAnswer::ServiceDeferred));
        let (key, outcome) = settled.recv_timeout(std::time::Duration::from_secs(2)).expect("the deferred op settles");
        assert_eq!(key, "k1");
        assert_eq!(outcome, Ok(object(&[("result", Value::string("program/read/1"))])));
    }

    #[test]
    fn control_an_unkeyed_substrate_ask_is_answered_in_place() {
        let (host, settled) = sinking_host(true);
        assert!(matches!(host.answer(&keyed("program/read", None)), IoAnswer::Substrate(Ok(_))));
        assert!(settled.recv_timeout(std::time::Duration::from_millis(100)).is_err(), "nothing settles later");
    }

    struct Gated {
        entered: std::sync::mpsc::Sender<()>,
        release: orbital_core_base::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl SubstrateHandler for Gated {
        fn run(&self, op: &str, _: &[Value]) -> Result<Value, SubstrateFailure> {
            if op == "behavior/dependencies" {
                self.entered.send(()).unwrap();
                self.release.lock().recv().unwrap();
            }
            Ok(Value::string(op))
        }
    }

    fn gated_host() -> (NativeHost, std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<(String, Result<Value, Value>)>) {
        let (entered, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release) = std::sync::mpsc::channel();
        let (host, settled) = sinking_host(true);
        let host = host.with_substrate(Some(Arc::new(Gated { entered, release: orbital_core_base::sync::Mutex::new(release) })));
        (host, entered_rx, release_tx, settled)
    }

    #[test]
    fn an_unkeyed_substrate_ask_is_answered_while_a_keyed_dependencies_read_is_in_flight() {
        let (host, entered, release, settled) = gated_host();
        assert!(matches!(host.answer(&keyed("behavior/dependencies", Some("deps"))), IoAnswer::ServiceDeferred));
        entered.recv_timeout(std::time::Duration::from_secs(2)).expect("the keyed read is in flight");
        for op in ["behavior/source", "behavior/resolve", "behavior/describe", "program/validate"] {
            match host.answer(&keyed(op, None)) {
                IoAnswer::Substrate(Ok(Value::String(answer))) => assert_eq!(answer, op),
                other => panic!("{op} was not answered while the keyed read was in flight: {other:?}"),
            }
        }
        assert!(settled.recv_timeout(std::time::Duration::from_millis(100)).is_err(), "the keyed read is still in flight");
        release.send(()).unwrap();
        let (key, outcome) = settled.recv_timeout(std::time::Duration::from_secs(2)).expect("the keyed read settles");
        assert_eq!(key, "deps");
        assert_eq!(outcome, Ok(object(&[("result", Value::string("behavior/dependencies"))])));
    }

    #[test]
    fn control_two_keyed_substrate_asks_in_flight_both_settle() {
        let (host, entered, release, settled) = gated_host();
        assert!(matches!(host.answer(&keyed("behavior/dependencies", Some("deps"))), IoAnswer::ServiceDeferred));
        entered.recv_timeout(std::time::Duration::from_secs(2)).expect("the keyed read is in flight");
        assert!(matches!(host.answer(&keyed("behavior/source", Some("src"))), IoAnswer::ServiceDeferred));
        let (key, _) = settled.recv_timeout(std::time::Duration::from_secs(2)).expect("the second settles");
        assert_eq!(key, "src");
        release.send(()).unwrap();
        let (key, _) = settled.recv_timeout(std::time::Duration::from_secs(2)).expect("the first settles");
        assert_eq!(key, "deps");
    }

    #[test]
    fn edge_a_keyed_ask_with_no_sink_is_answered_in_place() {
        let (host, _settled) = sinking_host(false);
        assert!(matches!(host.answer(&keyed("program/read", Some("k1"))), IoAnswer::Substrate(Ok(_))));
    }

    #[test]
    fn edge_a_deferred_agent_port_op_settles_the_payload_an_in_place_completion_carries() {
        let (host, settled) = sinking_host(true);
        let mut ask = keyed("program/dispatch", Some("k1"));
        if let IoRequest::Substrate { args, .. } = &mut ask {
            *args = vec![object(&[("inbound", Value::string("hook")), ("body", Value::string(String::new())), ("headers", object(&[]))])];
        }
        assert!(matches!(host.answer(&ask), IoAnswer::ServiceDeferred));
        let (_, outcome) = settled.recv_timeout(std::time::Duration::from_secs(2)).expect("the deferred op settles");
        let answer = Value::string("program/dispatch/1");
        assert_eq!(outcome, Ok(object(&[("result", answer), ("text", Value::string("\"program/dispatch/1\""))])));
    }
}
