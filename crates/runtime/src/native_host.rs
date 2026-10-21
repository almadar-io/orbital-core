//! The one native host of a server-role kernel: every ask a dispatch makes, answered in process. Rows come from the
//! entity source, `http` and `os/` under the program's declared egress and grants, credentials, OAuth, random and clock
//! from the secret source and the operating system. Each piece is the existing answerer (`BlockingHost`, `HttpHost`,
//! `answer_os`); this owns them so a host can share one answerer across threads and rebuild it when a program registers.

use crate::effects::executor::{ServiceCaller, ServiceHandler, SubstrateFailure, SubstrateHandler};
use crate::entity_source::EntitySource;
use crate::http_service::{SecretSource, Transport};
use crate::os_host::{OsGrants, OsHandles, OsHost};
use crate::resumable::{BlockingHost, CallSite, HttpHost, IoAnswer, IoRequest};
use orbital_core_base::value::Value;
use std::sync::Arc;

/// Receives each event of a stream, with the call site whose `onMessage` route it belongs to.
pub type StreamEvents = dyn Fn(Option<&CallSite>, Value) + Send + Sync;

/// The work of one keyed substrate op answered as a dispatch of its own (R-a), built on the asking thread and run off
/// it; a host that serves the op as the asking viewer captures that viewer when it builds the work.
pub type SubstrateWork = Box<dyn FnOnce() -> Result<Value, SubstrateFailure> + Send>;

/// Builds the [`SubstrateWork`] of a deferred op from its name and arguments.
pub type SubstrateLater = dyn Fn(&str, &[Value]) -> SubstrateWork + Send + Sync;

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

pub struct NativeHost {
    source: Arc<dyn EntitySource>,
    transport: Arc<dyn Transport>,
    secrets: Arc<dyn SecretSource>,
    egress: Vec<String>,
    grants: OsGrants,
    handles: Arc<OsHandles>,
    on_event: Arc<StreamEvents>,
    substrate: Option<Arc<dyn SubstrateHandler>>,
    substrate_later: Option<Arc<SubstrateLater>>,
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
            transport,
            secrets,
            egress,
            grants,
            handles,
            on_event,
            substrate: None,
            substrate_later: None,
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

    /// A keyed substrate op with a declared route, when a completion sink is installed, runs as a dispatch of its own
    /// (R-a): the ask is answered `ServiceDeferred` and its outcome settles through the sink.
    fn defer_substrate(&self, request: &IoRequest) -> Option<IoAnswer> {
        let IoRequest::Substrate { op, args, key: Some(key), site: Some(site) } = request else {
            return None;
        };
        self.handles.completions.sink()?;
        let work = match (&self.substrate_later, &self.substrate) {
            (Some(later), _) => later(op, args),
            (None, Some(handler)) => {
                let (handler, op, args) = (handler.clone(), op.clone(), args.clone());
                Box::new(move || handler.run(&op, &args)) as SubstrateWork
            }
            (None, None) => return None,
        };
        Some(crate::os_host::completion::Completions::defer_substrate(&self.handles.completions, key, site, op, work))
    }

    pub fn answer(&self, request: &IoRequest) -> IoAnswer {
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
mod tests {
    use super::*;
    use crate::effects::executor::SubstrateFailure;
    use crate::entity_source::InMemoryEntityCache;
    use crate::http_service::{HttpRequest, HttpResponse, TransportError};

    struct Echo;
    struct Offline;

    impl Transport for Offline {
        fn send(&self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
            unreachable!("a substrate ask sends no request")
        }
    }

    impl SubstrateHandler for Echo {
        fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
            Ok(Value::String(format!("{op}/{}", args.len())))
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

    struct Settled(parking_lot::Mutex<std::sync::mpsc::Sender<(String, Result<Value, Value>)>>);

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
            handles.completions.set_sink(Arc::new(Settled(parking_lot::Mutex::new(tx))));
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
        Value::Object(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    #[test]
    fn a_keyed_routed_substrate_ask_is_answered_later_through_the_sink() {
        let (host, settled) = sinking_host(true);
        assert!(matches!(host.answer(&keyed("program/read", Some("k1"))), IoAnswer::ServiceDeferred));
        let (key, outcome) = settled.recv_timeout(std::time::Duration::from_secs(2)).expect("the deferred op settles");
        assert_eq!(key, "k1");
        assert_eq!(outcome, Ok(object(&[("result", Value::String("program/read/1".to_string()))])));
    }

    #[test]
    fn control_an_unkeyed_substrate_ask_is_answered_in_place() {
        let (host, settled) = sinking_host(true);
        assert!(matches!(host.answer(&keyed("program/read", None)), IoAnswer::Substrate(Ok(_))));
        assert!(settled.recv_timeout(std::time::Duration::from_millis(100)).is_err(), "nothing settles later");
    }

    #[test]
    fn edge_a_keyed_ask_with_no_sink_is_answered_in_place() {
        let (host, _settled) = sinking_host(false);
        assert!(matches!(host.answer(&keyed("program/read", Some("k1"))), IoAnswer::Substrate(Ok(_))));
    }

    #[test]
    fn edge_a_deferred_agent_port_op_settles_the_payload_an_in_place_completion_carries() {
        let (host, settled) = sinking_host(true);
        assert!(matches!(host.answer(&keyed("program/dispatch", Some("k1"))), IoAnswer::ServiceDeferred));
        let (_, outcome) = settled.recv_timeout(std::time::Duration::from_secs(2)).expect("the deferred op settles");
        let answer = Value::String("program/dispatch/1".to_string());
        assert_eq!(outcome, Ok(object(&[("result", answer), ("text", Value::String("\"program/dispatch/1\"".to_string()))])));
    }
}
