//! The one native host of a server-role kernel: every ask a dispatch makes, answered in process. Rows come from the
//! entity source, `os/` under the program's declared grants, credentials, random and clock from the secret source and
//! the operating system. Each piece is the existing answerer (`BlockingHost`, `PortHost`,
//! `answer_os`); this owns them so a host can share one answerer across threads and rebuild it when a program registers.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::effects::executor::{ServiceCaller, ServiceHandler, SubstrateFailure, SubstrateHandler};
use crate::entity_source::EntitySource;
use crate::secret_source::SecretSource;
use crate::os_host::{OsGrantTable, OsHandles, OsHost};
use crate::resumable::{drive_concurrent, BlockingHost, CallSite, GuestArenas, PortHost, IoAnswer, IoRequest};
use crate::agent_ports::{dispatch_outcome, guest_failure};
use crate::effects::response::EventResponse;
use crate::kernel::RuntimeKernel;
use crate::entity_source::InMemoryEntityCache;
use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::os::OsErrorKind;
use orbital_core_base::os_grants::OsGrants;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::{Value, ValueMap};
use std::sync::Arc;

/// Receives each event of a stream, with the call site whose `onMessage` route it belongs to.
pub type StreamEvents = dyn Fn(Option<&CallSite>, Value) + Send + Sync;

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

/// The credentials a guest kernel holds: none.
struct NoSecrets;

impl SecretSource for NoSecrets {
    fn secret(&self, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// `payload` with each Arena handle of the asking kernel replaced by the handle its adopted storage has in `kernel`.
fn rehandle(value: &Value, adopted: &[(i64, Value)]) -> Value {
    match value {
        Value::Sealed(_) if value.is_sealed_as("Arena") => value
            .get("id")
            .and_then(Value::as_int)
            .and_then(|id| adopted.iter().find(|(held, _)| *held == id))
            .map_or_else(|| value.clone(), |(_, handle)| handle.clone()),
        Value::Array(items) => Value::array(items.iter().map(|item| rehandle(item, adopted)).collect::<Vec<_>>()),
        Value::Object(fields) => Value::object(fields.iter().map(|(key, item)| (key.clone(), rehandle(item, adopted))).collect::<ValueMap>()),
        other => other.clone(),
    }
}

/// The whole life of one guest kernel (EXEC-CONC-0a): a fresh kernel for `program` holding exactly `grants`, its own
/// rows, no credentials and no substrate host, the shared arenas of the asker adopted, `address` dispatched with the
/// copied payload. The answer is the dispatch outcome an in-process `program/dispatch` gives.
fn run_guest(program: Value, address: String, payload: Value, grants: OsGrants, arenas: GuestArenas) -> Result<Value, SubstrateFailure> {
    let invalid = |message: String| guest_failure(OsErrorKind::Invalid, message);
    let mut schema: OrbitalSchema = serde_json::from_value(orbital_core_base::json_value::value_to_json(&program))
        .map_err(|e| invalid(format!("program/dispatch: the guest is not a program: {e}")))?;
    schema.os_grants = Some(grants.clone());
    schema.orbitals.iter_mut().for_each(|orbital| orbital.os_grants = None);
    let mut parts = address.split('.');
    let (Some(orbital), Some(trait_name), Some(event), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(invalid(format!("program/dispatch: '{address}' is not a qualified address")));
    };
    let source = Arc::new(InMemoryEntityCache::new());
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        source.clone(),
        crate::config::KernelConfig { environment: RuntimeEnvironment::Server, runtime_rows_in_kernel: true, ..Default::default() },
    );
    kernel.register_schema(schema).map_err(|e| invalid(format!("program/dispatch: the guest does not start: {e}")))?;
    if kernel.orbital_of_trait(trait_name).as_deref() != Some(orbital) {
        return Err(invalid(format!("program/dispatch: the guest has no trait '{trait_name}' in orbital '{orbital}'")));
    }
    let mut adopted = Vec::with_capacity(arenas.0.len());
    for (id, arena) in arenas.0 {
        let handle = kernel.adopt_shared_arena(arena).map_err(|e| guest_failure(e.kind, e.message))?;
        adopted.push((id, handle));
    }
    let payload = rehandle(&payload, &adopted);
    let host = NativeHost::new(source, Arc::new(NoSecrets), grants, Arc::new(OsHandles::default()), Arc::new(|_: Option<&CallSite>, _: Value| {}));
    let started = kernel.start_dispatch(trait_name, Some("default"), event, payload, None, None);
    let outcomes = started
        .and_then(|progress| drive_concurrent(&kernel, progress, &|request: &IoRequest| host.answer(request)))
        .map_err(|e| guest_failure(OsErrorKind::Io, format!("program/dispatch: the guest dispatch failed: {e}")))?;
    Ok(dispatch_outcome(&EventResponse::from_process_results(&outcomes, trait_name, event)))
}

#[derive(Clone)]
pub struct NativeHost {
    source: Arc<dyn EntitySource>,
    secrets: Arc<dyn SecretSource>,
    grants: OsGrantTable,
    handles: Arc<OsHandles>,
    on_event: Arc<StreamEvents>,
    substrate: Option<Arc<dyn SubstrateHandler>>,
    substrate_later: Option<Arc<SubstrateLater>>,
    substrate_streams: Option<Arc<SubstrateStreams>>,
}

impl NativeHost {
    pub fn new(
        source: Arc<dyn EntitySource>,
        secrets: Arc<dyn SecretSource>,
        grants: impl Into<OsGrantTable>,
        handles: Arc<OsHandles>,
        on_event: Arc<StreamEvents>,
    ) -> Self {
        let (grants, problems) = grants.into().resolve_anchors(&handles.anchors);
        for problem in problems {
            tracing::warn!(target: orbital_core_base::logging::ns::RUNTIME_EFFECTS, at = %problem.path, message = %problem.message, "os grant anchor");
        }
        Self {
            source,
            secrets,
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

    /// A guest start, when a completion sink is installed: the guest runs on a thread of its own and its outcome settles
    /// through the sink. Without a sink there is nothing to answer later, and the blocking answer refuses it.
    fn defer_guest(&self, request: &IoRequest) -> Option<IoAnswer> {
        let IoRequest::Guest { program, address, payload, grants, arenas, key, site } = request else {
            return None;
        };
        self.handles.completions.sink()?;
        let (program, address, payload, grants, arenas) =
            (program.clone(), address.clone(), payload.clone(), grants.clone(), arenas.clone());
        Some(crate::os_host::completion::Completions::defer_guest(&self.handles.completions, key, site, move || {
            run_guest(program, address, payload, grants, arenas)
        }))
    }

    pub fn answer(&self, request: &IoRequest) -> IoAnswer {
        if let Some(deferred) = self.defer_substrate(request).or_else(|| self.defer_guest(request)) {
            return deferred;
        }
        let on_event = |site: Option<&CallSite>, event: Value| (self.on_event)(site, event);
        let asking = match request {
            IoRequest::Os { orbital, .. } => orbital.as_deref(),
            _ => None,
        };
        let os = OsHost {
            grants: self.grants.for_orbital(asking),
            handles: &self.handles,
            on_event: &on_event,
            secrets: Some(self.secrets.as_ref()),
        };
        let ports = PortHost {
            secrets: self.secrets.as_ref(),
            on_event: &on_event,
            os: Some(&os),
        };
        BlockingHost {
            source: self.source.as_ref(),
            service: Some(&NoProviders),
            substrate: self.substrate.as_deref(),
        }
        .answer_with_ports(&ports, request)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::effects::executor::SubstrateFailure;
    use crate::entity_source::InMemoryEntityCache;

    struct Echo;

    impl SubstrateHandler for Echo {
        fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
            Ok(Value::string(format!("{op}/{}", args.len())))
        }
    }

    fn host() -> NativeHost {
        NativeHost::new(
            Arc::new(InMemoryEntityCache::new()),
            Arc::new(crate::credentials::Credentials::from_env(Default::default())),
            OsGrantTable::default(),
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
    fn a_program_prove_ask_reaches_the_handler_and_a_host_without_one_names_it() {
        let prove = || IoRequest::Substrate { op: "program/prove".to_string(), args: vec![Value::Null, Value::Null], key: None, site: None };
        match host().with_substrate(Some(Arc::new(Echo))).answer(&prove()) {
            IoAnswer::Substrate(Ok(Value::String(answer))) => assert_eq!(answer, "program/prove/2"),
            other => panic!("expected the handler's answer, got {other:?}"),
        }
        match host().answer(&prove()) {
            IoAnswer::Substrate(Err(failure)) => assert_eq!(failure.message, "no substrate host configured for program/prove"),
            other => panic!("control: expected the refusal, got {other:?}"),
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
            Arc::new(crate::credentials::Credentials::from_env(Default::default())),
            OsGrantTable::default(),
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

    fn guest_ask(program: serde_json::Value, key: &str) -> IoRequest {
        let IoRequest::Substrate { site: Some(site), .. } = keyed("program/dispatch", Some(key)) else { unreachable!() };
        IoRequest::Guest {
            program: serde_json::from_value(program).unwrap(),
            address: "G.Work.GO".to_string(),
            payload: object(&[("n", Value::Int(1))]),
            grants: Default::default(),
            arenas: Default::default(),
            key: key.to_string(),
            site,
        }
    }

    fn guest_program() -> serde_json::Value {
        serde_json::json!({
            "name": "Guest", "typeContractVersion": 1, "orbitals": [{
                "name": "G",
                "entity": { "name": "Cell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Work", "linkedEntity": "Cell",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "GO" }]
                    } }]
            }]
        })
    }

    #[test]
    fn a_guest_ask_is_answered_later_with_the_dispatch_outcome_of_a_fresh_kernel() {
        let (host, settled) = sinking_host(true);
        assert!(matches!(host.answer(&guest_ask(guest_program(), "g1")), IoAnswer::ServiceDeferred));
        let (key, outcome) = settled.recv_timeout(std::time::Duration::from_secs(10)).expect("the guest settles");
        assert_eq!(key, "g1");
        let outcome = outcome.expect("the guest dispatch succeeded");
        let result = outcome.get("result").expect("a result");
        assert_eq!(result.get("ok"), Some(&Value::Bool(true)));
        assert!(matches!(outcome.get("text"), Some(Value::String(_))), "the result also arrives as JSON text");
    }

    #[test]
    fn a_guest_that_is_no_program_or_has_no_such_trait_fails_invalid_with_its_kind() {
        let (host, settled) = sinking_host(true);
        let mut wrong_orbital = guest_ask(guest_program(), "g2");
        if let IoRequest::Guest { address, .. } = &mut wrong_orbital {
            *address = "Other.Work.GO".to_string();
        }
        for (ask, key) in [(guest_ask(serde_json::json!({ "name": 3 }), "g1"), "g1"), (wrong_orbital, "g2")] {
            assert!(matches!(host.answer(&ask), IoAnswer::ServiceDeferred));
            let (settled_key, outcome) = settled.recv_timeout(std::time::Duration::from_secs(10)).expect("the failure settles");
            assert_eq!(settled_key, key);
            let failure = outcome.expect_err("the guest did not run");
            assert_eq!(failure.get("kind"), Some(&Value::string("invalid")), "{failure:?}");
            assert!(failure.get("error").is_some());
        }
    }

    #[test]
    fn edge_a_host_with_no_sink_starts_no_guest_and_names_why() {
        let (host, _settled) = sinking_host(false);
        match host.answer(&guest_ask(guest_program(), "g1")) {
            IoAnswer::Substrate(Err(failure)) => {
                assert_eq!(crate::agent_ports::guest_failure_kind(&failure.errors), Some(Value::string("unsupported")));
            }
            other => panic!("expected the refusal, got {other:?}"),
        }
    }
}
