//! The completion of a keyed `os/` call that outlives its dispatch (EXEC-QUEUE-10). The host answers the ask
//! `ServiceDeferred` at once and, when the operation ends, hands its outcome to an [`OsCompletionSink`].
//! [`KernelCompletionSink`] turns that outcome into the event of the call's declared route and dispatches it.

use crate::kernel::RuntimeKernel;
pub use crate::resumable::completion_event;
use crate::resumable::{
    drive_blocking, CallRoutes, CallSite, DispatchError, DispatchOutcomes, IoAnswer, IoRequest, Progress,
};
use orbital_core_base::os::OsError;
use orbital_core_base::value::Value;
use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Receives the outcome of a deferred keyed `os/` call, on the host's watcher thread.
pub trait OsCompletionSink: Send + Sync {
    fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>);

    /// One event of a keyed stream: the completion of the call's `onMessage` route, the call staying open.
    fn message(&self, site: &CallSite, key: &str, event: Value) {
        let routed = CallSite {
            emit: CallRoutes {
                success: site.emit.on_message.clone(),
                ..CallRoutes::default()
            },
            ..site.clone()
        };
        self.complete(&routed, key, Ok(event));
    }
}

/// The sink a program instance delivers to, and the keys whose completion is still wanted.
#[derive(Default)]
pub struct Completions {
    sink: Mutex<Option<Arc<dyn OsCompletionSink>>>,
    live: Mutex<HashSet<String>>,
    /// Claimed completions still being delivered: open until their dispatch (and any call it starts) has run.
    delivering: Mutex<HashMap<String, usize>>,
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

    pub(super) fn is_live(&self, key: &str) -> bool {
        self.live.lock().contains(key)
    }

    /// Hand one event of the live stream `key` to the sink; `false` when the stream was cancelled or ended.
    pub(super) fn message(&self, key: &str, site: &CallSite, event: Value) -> bool {
        if !self.is_live(key) {
            return false;
        }
        if let Some(sink) = self.sink() {
            sink.message(site, key, event);
        }
        true
    }

    /// The keys of every open call, sorted: completions still wanted (streams, pending deferred answers) and claimed
    /// completions whose delivery has not finished.
    pub fn live_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.live.lock().iter().cloned().collect();
        keys.extend(self.delivering.lock().keys().cloned());
        keys.sort();
        keys.dedup();
        keys
    }

    /// Drop the pending completion of `key`: its outcome is never delivered. `false` when none is pending.
    pub fn cancel(&self, key: &str) -> bool {
        self.live.lock().remove(key)
    }

    pub(super) fn clear(&self) {
        self.live.lock().clear();
        self.delivering.lock().clear();
    }

    /// Take the right to deliver `key`'s completion: `false` when it was cancelled first. Once claimed, a late
    /// `cancel` finds nothing pending, so the host can reap its child and still deliver the outcome.
    pub(super) fn claim(&self, key: &str) -> bool {
        let mut live = self.live.lock();
        if !live.remove(key) {
            return false;
        }
        *self.delivering.lock().entry(key.to_string()).or_default() += 1;
        true
    }

    /// Hand a claimed completion to the sink; the key stays open until the sink's dispatch has run.
    pub(super) fn deliver(&self, key: &str, site: &CallSite, outcome: Result<Value, OsError>) {
        if let Some(sink) = self.sink() {
            sink.complete(site, key, outcome);
        }
        let mut delivering = self.delivering.lock();
        if let Some(count) = delivering.get_mut(key) {
            *count -= 1;
            if *count == 0 {
                delivering.remove(key);
            }
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
        if self.claim(key) {
            self.deliver(key, site, outcome);
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
/// the sink remembers which dispatch deferred which key: a host runs every dispatch that may defer a call
/// inside a [`DispatchScope`] ([`Self::begin_dispatch`], or [`Self::drive`]), and a completion for key K waits
/// for the dispatch that deferred K to end and for no other, so unrelated dispatches run side by side.
pub struct KernelCompletionSink {
    kernel: Arc<RuntimeKernel>,
    answer: Arc<CompletionAnswerer>,
    done: Arc<CompletionDone>,
    owners: Arc<Mutex<HashMap<String, Arc<Ended>>>>,
}

/// Whether a dispatch has ended, and the keys it deferred.
#[derive(Default)]
struct Ended {
    over: Mutex<bool>,
    ended: Condvar,
    keys: Mutex<Vec<String>>,
}

/// One dispatch in flight: its asks go through [`Self::answer`]; dropping it ends the dispatch for every completion
/// waiting on a key it deferred.
pub struct DispatchScope {
    ended: Arc<Ended>,
    owners: Arc<Mutex<HashMap<String, Arc<Ended>>>>,
}

impl DispatchScope {
    /// Answer `request` with `inner`. A keyed ask that may defer, or that opens a stream, is registered to this
    /// dispatch before it is answered (the watcher may deliver before the answer returns) and released again if the
    /// host neither deferred it nor opened the stream.
    pub fn answer(&self, inner: &dyn Fn(&IoRequest) -> IoAnswer, request: &IoRequest) -> IoAnswer {
        let IoRequest::Os { op, key: Some(key), .. } = request else { return inner(request) };
        if !op.defers_completion() && !op.is_stream() {
            return inner(request);
        }
        self.owners.lock().insert(key.clone(), self.ended.clone());
        self.ended.keys.lock().push(key.clone());
        let answered = inner(request);
        let open = match &answered {
            IoAnswer::ServiceDeferred => true,
            IoAnswer::Os(Ok(_)) => op.is_stream(),
            _ => false,
        };
        if !open {
            self.release(key);
        }
        answered
    }

    fn release(&self, key: &str) {
        let mut owners = self.owners.lock();
        if owners.get(key).is_some_and(|owner| Arc::ptr_eq(owner, &self.ended)) {
            owners.remove(key);
        }
    }
}

impl Drop for DispatchScope {
    fn drop(&mut self) {
        let keys = std::mem::take(&mut *self.ended.keys.lock());
        for key in &keys {
            self.release(key);
        }
        *self.ended.over.lock() = true;
        self.ended.ended.notify_all();
    }
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
            owners: Arc::default(),
        }
    }

    /// Start tracking one dispatch. Hold the scope until the dispatch has committed.
    pub fn begin_dispatch(&self) -> DispatchScope {
        DispatchScope {
            ended: Arc::default(),
            owners: self.owners.clone(),
        }
    }

    /// Run `progress` to its end, answering its asks with the sink's answerer, as one tracked dispatch.
    pub fn drive(
        &self,
        progress: Progress<DispatchOutcomes>,
    ) -> Result<DispatchOutcomes, DispatchError> {
        let scope = self.begin_dispatch();
        let answer = |request: &IoRequest| scope.answer(&*self.answer, request);
        drive_blocking(&self.kernel, progress, &answer)
    }

    /// Wait for the dispatch that deferred `key` to end, when one is still running.
    fn wait_for_owner(&self, key: &str) {
        let owner = self.owners.lock().get(key).cloned();
        if let Some(owner) = owner {
            let mut over = owner.over.lock();
            while !*over {
                owner.ended.wait(&mut over);
            }
        }
    }
}

impl OsCompletionSink for KernelCompletionSink {
    fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>) {
        let Some((event, payload)) = completion_event(site, key, outcome) else {
            return;
        };
        self.wait_for_owner(key);
        let scope = self.begin_dispatch();
        let answer = |request: &IoRequest| scope.answer(&*self.answer, request);
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
            .and_then(|progress| drive_blocking(&self.kernel, progress, &answer));
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

    fn lone_sink() -> (Arc<KernelCompletionSink>, std::sync::mpsc::Receiver<String>) {
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
        let done: Arc<CompletionDone> = Arc::new(move |_, event, _| {
            let _ = told.lock().send(event.to_string());
        });
        (Arc::new(KernelCompletionSink::new(kernel, answer, done)), ran)
    }

    fn defers(key: &str) -> IoRequest {
        IoRequest::Os {
            op: orbital_core_base::os::OsOp::Wait,
            args: Vec::new(),
            key: Some(key.to_string()),
            site: Some(site(Some("OK"), None)),
        }
    }

    fn complete_on_a_thread(sink: &Arc<KernelCompletionSink>, key: &'static str, event: &str) -> std::thread::JoinHandle<()> {
        let sink = sink.clone();
        let mut at = site(Some(event), None);
        at.trait_name = "Nowhere".to_string();
        std::thread::spawn(move || sink.complete(&at, key, Ok(exited(0.0))))
    }

    fn deferred(_: &IoRequest) -> IoAnswer {
        IoAnswer::ServiceDeferred
    }

    #[test]
    fn a_completion_waits_for_the_dispatch_that_deferred_its_key_and_for_no_other() {
        let (sink, ran) = lone_sink();
        let (a, b) = (sink.begin_dispatch(), sink.begin_dispatch());
        assert!(matches!(a.answer(&deferred, &defers("k1")), IoAnswer::ServiceDeferred));
        assert!(matches!(b.answer(&deferred, &defers("k2")), IoAnswer::ServiceDeferred));
        let (w1, w2) = (complete_on_a_thread(&sink, "k1", "DONE1"), complete_on_a_thread(&sink, "k2", "DONE2"));
        assert!(ran.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "both wait while both dispatches run");
        drop(b);
        assert_eq!(ran.recv_timeout(std::time::Duration::from_secs(5)).as_deref(), Ok("DONE2"), "k2 runs once B commits, with A still open");
        assert!(ran.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "k1 still waits for A");
        drop(a);
        assert_eq!(ran.recv_timeout(std::time::Duration::from_secs(5)).as_deref(), Ok("DONE1"), "then k1 applies");
        w1.join().expect("k1");
        w2.join().expect("k2");
    }

    #[test]
    fn control_a_completion_of_a_key_no_dispatch_deferred_runs_at_once_beside_an_open_dispatch() {
        let (sink, ran) = lone_sink();
        let open = sink.begin_dispatch();
        assert!(matches!(open.answer(&deferred, &defers("k1")), IoAnswer::ServiceDeferred));
        complete_on_a_thread(&sink, "stranger", "STRANGER").join().expect("stranger");
        assert_eq!(ran.recv_timeout(std::time::Duration::from_secs(5)).as_deref(), Ok("STRANGER"));
        drop(open);
    }

    fn listens(key: &str) -> IoRequest {
        IoRequest::Os {
            op: orbital_core_base::os::OsOp::TcpListen,
            args: Vec::new(),
            key: Some(key.to_string()),
            site: Some(site(Some("OK"), None)),
        }
    }

    #[test]
    fn a_stream_event_waits_for_the_dispatch_that_opened_the_stream_and_a_refused_open_is_not_waited_for() {
        let (sink, ran) = lone_sink();
        let open = sink.begin_dispatch();
        let opened = open.answer(&|_| IoAnswer::Os(Ok(Value::Null)), &listens("s1"));
        assert!(matches!(opened, IoAnswer::Os(Ok(_))));
        let refused = open.answer(&|_| IoAnswer::Os(Err(OsError::new(OsErrorKind::Denied, "no"))), &listens("s2"));
        assert!(matches!(refused, IoAnswer::Os(Err(_))));
        let w1 = complete_on_a_thread(&sink, "s1", "MSG");
        complete_on_a_thread(&sink, "s2", "REFUSED").join().expect("s2");
        assert_eq!(ran.recv_timeout(std::time::Duration::from_secs(5)).as_deref(), Ok("REFUSED"), "control: a refused open owns nothing");
        assert!(ran.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "the event waits while the opening dispatch runs");
        drop(open);
        assert_eq!(ran.recv_timeout(std::time::Duration::from_secs(5)).as_deref(), Ok("MSG"));
        w1.join().expect("s1");
    }

    #[test]
    fn a_stream_message_fires_the_message_route_and_never_success() {
        let mut routed = site(Some("OK"), Some("BAD"));
        routed.emit.on_message = Some("MSG".to_string());
        struct Fired(Mutex<Vec<Option<(String, Value)>>>);
        impl OsCompletionSink for Fired {
            fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>) {
                self.0.lock().push(completion_event(site, key, outcome));
            }
        }
        let fired = Fired(Mutex::new(Vec::new()));
        fired.message(&routed, "s", exited(1.0));
        fired.message(&site(Some("OK"), Some("BAD")), "s", exited(1.0));
        let got = fired.0.into_inner();
        let (event, payload) = got[0].clone().expect("routed");
        assert_eq!(event, "MSG");
        assert_eq!(payload.get_path("key"), Some(&Value::String("s".to_string())));
        assert!(got[1].is_none(), "control: a stream with no onMessage route fires nothing");
    }

    #[test]
    fn control_a_key_the_host_did_not_defer_is_not_waited_for() {
        let (sink, ran) = lone_sink();
        let open = sink.begin_dispatch();
        let answered = open.answer(&|_| IoAnswer::Service(Err("no".to_string())), &defers("k1"));
        assert!(!matches!(answered, IoAnswer::ServiceDeferred));
        complete_on_a_thread(&sink, "k1", "NOW").join().expect("k1");
        assert_eq!(ran.recv_timeout(std::time::Duration::from_secs(5)).as_deref(), Ok("NOW"));
        drop(open);
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

    #[test]
    fn a_completion_being_delivered_keeps_its_key_open_until_its_follow_up_call_opens() {
        // A one-shot `a` whose completion starts the one-shot `b`: no instant between them may show no open key.
        struct Chain {
            completions: std::sync::Weak<Completions>,
            seen_while_delivering: Mutex<Vec<Vec<String>>>,
            delivered: Mutex<Vec<String>>,
            done: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        }
        impl OsCompletionSink for Chain {
            fn complete(&self, site: &CallSite, key: &str, _: Result<Value, OsError>) {
                let completions = self.completions.upgrade().expect("completions");
                self.seen_while_delivering.lock().push(completions.live_keys());
                self.delivered.lock().push(key.to_string());
                if key == "a" {
                    Completions::run_later(&completions, "b", site, || Ok(exited(0.0))).is_ok().then_some(()).expect("b starts");
                } else if let Some(done) = self.done.lock().take() {
                    let _ = done.send(());
                }
            }
        }
        let completions = Arc::new(Completions::default());
        let (done, finished) = std::sync::mpsc::channel();
        let chain = Arc::new(Chain {
            completions: Arc::downgrade(&completions),
            seen_while_delivering: Mutex::new(Vec::new()),
            delivered: Mutex::new(Vec::new()),
            done: Mutex::new(Some(done)),
        });
        completions.set_sink(chain.clone());
        Completions::run_later(&completions, "a", &site(Some("OK"), None), || Ok(exited(0.0))).is_ok().then_some(()).expect("a starts");
        finished.recv_timeout(std::time::Duration::from_secs(5)).expect("b's completion is delivered");
        assert_eq!(*chain.delivered.lock(), vec!["a".to_string(), "b".to_string()]);
        for seen in chain.seen_while_delivering.lock().iter() {
            assert!(!seen.is_empty(), "a completion in delivery is still an open call: {seen:?}");
        }
    }
}
