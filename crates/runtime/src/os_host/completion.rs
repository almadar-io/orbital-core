//! The completion of a keyed `os/` call that outlives its dispatch (EXEC-QUEUE-10). The host answers the ask
//! `ServiceDeferred` at once and, when the operation ends, hands its outcome to an [`OsCompletionSink`].
//! [`KernelCompletionSink`] turns that outcome into the event of the call's declared route and dispatches it.

use crate::kernel::RuntimeKernel;
pub use crate::resumable::completion_event;
use crate::resumable::{
    drive_blocking, CallSite, DispatchError, DispatchOutcomes, IoAnswer, IoRequest, Progress,
};
use orbital_core_base::os::OsError;
use orbital_core_base::value::Value;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;

/// Receives the outcome of a deferred keyed `os/` call, on the host's watcher thread.
pub trait OsCompletionSink: Send + Sync {
    fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>);
}

/// The sink a program instance delivers to, and the keys whose completion is still wanted.
#[derive(Default)]
pub struct Completions {
    sink: Mutex<Option<Arc<dyn OsCompletionSink>>>,
    live: Mutex<HashSet<String>>,
}

impl Completions {
    pub fn set_sink(&self, sink: Arc<dyn OsCompletionSink>) {
        *self.sink.lock() = Some(sink);
    }

    pub fn sink(&self) -> Option<Arc<dyn OsCompletionSink>> {
        self.sink.lock().clone()
    }

    /// `false` when `key` already has a completion pending.
    pub(super) fn begin(&self, key: &str) -> bool {
        self.live.lock().insert(key.to_string())
    }

    #[cfg(test)]
    pub(super) fn is_live(&self, key: &str) -> bool {
        self.live.lock().contains(key)
    }

    /// Drop the pending completion of `key`: its outcome is never delivered. `false` when none is pending.
    pub fn cancel(&self, key: &str) -> bool {
        self.live.lock().remove(key)
    }

    pub(super) fn clear(&self) {
        self.live.lock().clear();
    }

    /// Take the right to deliver `key`'s completion: `false` when it was cancelled first. Once claimed, a late
    /// `cancel` finds nothing pending, so the host can reap its child and still deliver the outcome.
    pub(super) fn claim(&self, key: &str) -> bool {
        self.live.lock().remove(key)
    }

    /// Hand a claimed completion to the sink.
    pub(super) fn deliver(&self, key: &str, site: &CallSite, outcome: Result<Value, OsError>) {
        if let Some(sink) = self.sink() {
            sink.complete(site, key, outcome);
        }
    }

    /// Run `work` on a thread of its own and deliver its outcome to the sink unless `key` is cancelled first. A blocking
    /// read cannot be claimed before it ends, so a cancelled call's bytes were already consumed and are dropped.
    pub(super) fn run_later(
        this: &Arc<Self>,
        key: &str,
        site: &CallSite,
        work: impl FnOnce() -> Result<Value, OsError> + Send + 'static,
    ) -> Result<(), RunLaterError> {
        if !this.begin(key) {
            return Err(RunLaterError::KeyPending);
        }
        let (completions, key, site) = (this.clone(), key.to_string(), site.clone());
        let started = std::thread::Builder::new().name(format!("os-later-{key}")).spawn({
            let (completions, key) = (completions.clone(), key.clone());
            move || {
                let outcome = work();
                if completions.claim(&key) {
                    completions.deliver(&key, &site, outcome);
                }
            }
        });
        started.map(drop).map_err(|e| {
            completions.cancel(&key);
            RunLaterError::Spawn(e)
        })
    }

    /// Claim then deliver in one step, for tests that do not reap a child between them.
    #[cfg(test)]
    pub(super) fn finish(&self, key: &str, site: &CallSite, outcome: Result<Value, OsError>) {
        let wanted = self.claim(key);
        if !wanted {
            return;
        }
        if let Some(sink) = self.sink() {
            sink.complete(site, key, outcome);
        }
    }
}

/// Why [`Completions::run_later`] did not start.
pub(super) enum RunLaterError {
    KeyPending,
    Spawn(std::io::Error),
}

/// How a [`KernelCompletionSink`] answers the host asks of the completion's own dispatch.
pub type CompletionAnswerer = dyn Fn(&IoRequest) -> IoAnswer + Send + Sync;

/// Told how each completion dispatch ended: the call site, the event dispatched, its outcomes or error.
pub type CompletionDone =
    dyn Fn(&CallSite, &str, Result<DispatchOutcomes, DispatchError>) + Send + Sync;

/// Dispatches a completion into a kernel as the event of the call's route: `emit.success` with the result
/// merged with `key`, `emit.failure` with `{ error, kind, key }`. A call that declared no route for the
/// outcome dispatches nothing. A completion can end before the dispatch that started the call has committed, so
/// the sink runs one dispatch at a time: a host drives every dispatch that may defer a call through
/// [`KernelCompletionSink::drive`], and a completion produced meanwhile is dispatched after it ends.
pub struct KernelCompletionSink {
    kernel: Arc<RuntimeKernel>,
    answer: Arc<CompletionAnswerer>,
    done: Arc<CompletionDone>,
    serial: Mutex<()>,
}

impl KernelCompletionSink {
    pub fn new(
        kernel: Arc<RuntimeKernel>,
        answer: Arc<CompletionAnswerer>,
        done: Arc<CompletionDone>,
    ) -> Self {
        Self {
            kernel,
            answer,
            done,
            serial: Mutex::new(()),
        }
    }

    /// Hold the sink's gate: a completion produced meanwhile is dispatched after the guard drops. A host that
    /// dispatches a deferring call by some route other than [`Self::drive`] takes this around the whole dispatch.
    pub fn hold(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.serial.lock()
    }

    /// Run `progress` to its end, answering its asks with the sink's answerer, while holding the sink's gate.
    pub fn drive(
        &self,
        progress: Progress<DispatchOutcomes>,
    ) -> Result<DispatchOutcomes, DispatchError> {
        let _one_at_a_time = self.serial.lock();
        drive_blocking(&self.kernel, progress, &*self.answer)
    }
}

impl OsCompletionSink for KernelCompletionSink {
    fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>) {
        let Some((event, payload)) = completion_event(site, key, outcome) else {
            return;
        };
        let _one_at_a_time = self.serial.lock();
        let result = self
            .kernel
            .start_dispatch(
                &site.trait_name,
                Some(site.entity_id.as_str()),
                &event,
                payload,
                None,
                None,
            )
            .and_then(|progress| drive_blocking(&self.kernel, progress, &*self.answer));
        (self.done)(site, &event, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resumable::CallRoutes;
    use orbital_core_base::value::ValueMap;
    use orbital_core_base::os::OsErrorKind;

    fn site(success: Option<&str>, failure: Option<&str>) -> CallSite {
        CallSite {
            orbital: None,
            trait_name: "Sup".to_string(),
            follow_on: false,
            entity_id: "singleton".to_string(),
            emit: CallRoutes {
                success: success.map(str::to_string),
                failure: failure.map(str::to_string),
                ..CallRoutes::default()
            },
        }
    }

    fn exited(code: f64) -> Value {
        Value::Object(ValueMap::from([("code".to_string(), Value::Number(code))]))
    }

    #[test]
    fn a_result_fires_the_success_route_with_the_result_and_the_key() {
        let (event, payload) =
            completion_event(&site(Some("OK"), Some("BAD")), "k1", Ok(exited(3.0)))
                .expect("routed");
        assert_eq!(event, "OK");
        assert_eq!(payload.get_path("code"), Some(&Value::Number(3.0)));
        assert_eq!(
            payload.get_path("key"),
            Some(&Value::String("k1".to_string()))
        );
    }

    #[test]
    fn a_failure_fires_the_failure_route_with_error_kind_and_key() {
        let error = OsError::new(OsErrorKind::Closed, "os/wait: pid 9 was already reaped");
        let (event, payload) =
            completion_event(&site(Some("OK"), Some("BAD")), "k1", Err(error)).expect("routed");
        assert_eq!(event, "BAD");
        assert_eq!(
            payload.get_path("kind"),
            Some(&Value::String("closed".to_string()))
        );
        assert_eq!(
            payload.get_path("key"),
            Some(&Value::String("k1".to_string()))
        );
        assert!(payload.get_path("error").is_some());
    }

    #[test]
    fn an_outcome_with_no_declared_route_fires_nothing() {
        assert!(completion_event(&site(None, Some("BAD")), "k", Ok(exited(0.0))).is_none());
        let error = OsError::new(OsErrorKind::Io, "x");
        assert!(completion_event(&site(Some("OK"), None), "k", Err(error)).is_none());
    }

    #[test]
    fn cancel_call_of_a_deferred_key_reaches_the_pending_completion_and_an_unknown_key_does_not() {
        use crate::http_service::{
            HttpRequest, HttpResponse, SecretSource, Transport, TransportError,
        };
        use crate::os_host::{OsGrants, OsHandles, OsHost};
        use crate::resumable::{os_completion_cancelled, HttpHost};
        struct NoTransport;
        impl Transport for NoTransport {
            fn send(&self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
                Err(TransportError::Failed("unused".to_string()))
            }
        }
        struct NoSecrets;
        impl SecretSource for NoSecrets {
            fn secret(&self, _: &str) -> Result<Option<String>, String> {
                Ok(None)
            }
        }
        let grants = OsGrants::default();
        let handles = OsHandles::default();
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let os = OsHost {
            grants: &grants,
            handles: &handles,
            on_event: &on_event,
        };
        let host = HttpHost {
            transport: &NoTransport,
            secrets: &NoSecrets,
            egress: &[],
            on_event: &on_event,
            os: Some(&os),
        };
        assert!(handles.completions.begin("w1"));
        assert!(os_completion_cancelled(&host, "w1"));
        assert!(!handles.completions.is_live("w1"));
        assert!(!os_completion_cancelled(&host, "w1"));
        assert!(!os_completion_cancelled(&host, "never-started"));
    }

    #[test]
    fn a_completion_is_dispatched_to_the_frame_the_call_was_made_on_and_not_to_the_singleton() {
        use crate::config::KernelConfig;
        use orbital_core_base::effect_types::RuntimeEnvironment;
        use orbital_core_base::schema::types::OrbitalSchema;
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Host", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }],
                        "transitions": [{ "from": "idle", "to": "done", "event": "OK" }]
                    } }]
            }]
        }))
        .expect("schema");
        let kernel = Arc::new(RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(crate::entity_source::InMemoryEntityCache::new()),
            KernelConfig::default(),
        ));
        kernel.register_schema(schema).expect("register");
        let answer: Arc<CompletionAnswerer> = Arc::new(|request| match request {
            IoRequest::ReadRow { .. } => IoAnswer::Row(Err("no row".to_string())),
            _ => IoAnswer::Service(Err("no host".to_string())),
        });
        let done: Arc<CompletionDone> =
            Arc::new(|_, _, result| assert!(result.is_ok(), "{result:?}"));
        let sink = KernelCompletionSink::new(kernel.clone(), answer, done);
        let mut at_row = site(Some("OK"), None);
        at_row.trait_name = "Host".to_string();
        at_row.entity_id = "row-7".to_string();
        sink.complete(&at_row, "k", Ok(exited(0.0)));
        assert_eq!(kernel.snapshot_trait("Host", Some("row-7")).state, "done");
        assert_ne!(
            kernel.snapshot_trait("Host", None).state,
            "done",
            "control: the singleton frame is untouched"
        );
        let mut at_singleton = site(Some("OK"), None);
        at_singleton.trait_name = "Host".to_string();
        sink.complete(&at_singleton, "k2", Ok(exited(0.0)));
        assert_eq!(
            kernel.snapshot_trait("Host", None).state,
            "done",
            "control: an id-less call addresses the singleton"
        );
    }

    #[test]
    fn a_completion_waits_while_a_host_holds_the_sink_and_runs_after() {
        use crate::config::KernelConfig;
        use orbital_core_base::effect_types::RuntimeEnvironment;
        let kernel = Arc::new(RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(crate::entity_source::InMemoryEntityCache::new()),
            KernelConfig::default(),
        ));
        let answer: Arc<CompletionAnswerer> = Arc::new(|_| IoAnswer::Service(Err("no host".to_string())));
        let (told, ran) = std::sync::mpsc::channel();
        let told = Mutex::new(told);
        let done: Arc<CompletionDone> = Arc::new(move |_, _, _| {
            let _ = told.lock().send(());
        });
        let sink = Arc::new(KernelCompletionSink::new(kernel, answer, done));
        let guard = sink.hold();
        let worker = {
            let sink = sink.clone();
            std::thread::spawn(move || {
                let mut at = site(Some("OK"), None);
                at.trait_name = "Nowhere".to_string();
                sink.complete(&at, "k", Ok(exited(0.0)));
            })
        };
        assert!(
            ran.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
            "the completion must wait for the holder"
        );
        drop(guard);
        assert!(ran.recv_timeout(std::time::Duration::from_secs(5)).is_ok(), "then it runs");
        worker.join().expect("worker");
    }

    #[test]
    fn a_claimed_completion_cannot_be_cancelled_and_a_cancelled_one_cannot_be_claimed() {
        let completions = Completions::default();
        assert!(completions.begin("late") && completions.claim("late"));
        assert!(
            !completions.cancel("late"),
            "a cancel after the claim finds nothing pending"
        );
        assert!(!completions.claim("late"), "a claim is taken once");
        assert!(completions.begin("early") && completions.cancel("early"));
        assert!(
            !completions.claim("early"),
            "a cancel before the claim leaves the child unreaped: nothing to claim"
        );
    }

    #[test]
    fn a_cancelled_key_delivers_nothing_and_a_live_key_delivers_once() {
        struct Count(Mutex<usize>);
        impl OsCompletionSink for Count {
            fn complete(&self, _: &CallSite, _: &str, _: Result<Value, OsError>) {
                *self.0.lock() += 1;
            }
        }
        let completions = Completions::default();
        let count = Arc::new(Count(Mutex::new(0)));
        completions.set_sink(count.clone());
        let site = site(Some("OK"), None);
        assert!(completions.begin("a") && !completions.begin("a"));
        assert!(completions.cancel("a") && !completions.cancel("a"));
        completions.finish("a", &site, Ok(exited(0.0)));
        assert_eq!(*count.0.lock(), 0);
        assert!(completions.begin("b"));
        completions.finish("b", &site, Ok(exited(0.0)));
        completions.finish("b", &site, Ok(exited(0.0)));
        assert_eq!(*count.0.lock(), 1);
    }
}
