//! The completion of a keyed `os/` call that outlives its dispatch (EXEC-QUEUE-10). The host answers the ask
//! `ServiceDeferred` at once and, when the operation ends, hands its outcome to an [`OsCompletionSink`].
//! [`KernelCompletionSink`] turns that outcome into the event of the call's declared route and dispatches it.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::kernel::RuntimeKernel;
pub use crate::resumable::completion_event;
use crate::effects::executor::SubstrateFailure;
use crate::resumable::{
    drive_blocking, CallRoutes, CallSite, DispatchError, DispatchOutcomes, IoAnswer, IoRequest, Progress,
};
use orbital_core_base::os::OsError;
use orbital_core_base::secret::Secret;
use orbital_core_base::value::Value;
use orbital_core_base::sync::{Condvar, Mutex};
use std::collections::HashMap;
use std::sync::Arc;

/// Receives the outcome of a deferred keyed `os/` call, on the host's watcher thread.
pub trait OsCompletionSink: Send + Sync {
    fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>);

    /// A deferred substrate op's completion, its payload already shaped: `Ok` takes `emit.success`, `Err`
    /// `emit.failure`, each with `key` beside it. A sink that dispatches nothing ignores it.
    fn settle(&self, _site: &CallSite, _key: &str, _outcome: Result<Value, Value>) {}

    /// A keyed stream's source ended and the host closed its key, with no event: the ask is over. A sink that keeps no
    /// record of asks ignores it.
    fn ended(&self, _site: &CallSite, _key: &str) {}

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

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompletionTestPhase { BeforeWork, BeforeClaim, Terminal }

/// The sink a program instance delivers to, and the keys whose completion is still wanted.
#[derive(Default)]
pub struct Completions {
    sink: Mutex<Option<Arc<dyn OsCompletionSink>>>,
    #[cfg(test)]
    test_observer: Mutex<Option<Arc<dyn Fn(&str, u64, CompletionTestPhase) + Send + Sync>>>,
    live: Mutex<HashMap<String, u64>>,
    next: std::sync::atomic::AtomicU64,
    /// Claimed completions still being delivered: open until their dispatch (and any call it starts) has run.
    delivering: Mutex<HashMap<String, usize>>,
}

impl Completions {
    #[cfg(test)]
    pub(super) fn set_test_observer(&self, observer: Arc<dyn Fn(&str, u64, CompletionTestPhase) + Send + Sync>) {
        *self.test_observer.lock() = Some(observer);
    }

    #[cfg(test)]
    fn observe_test_phase(&self, key: &str, lease: u64, phase: CompletionTestPhase) {
        let observer = self.test_observer.lock().clone();
        if let Some(observer) = observer { observer(key, lease, phase); }
    }

    pub fn set_sink(&self, sink: Arc<dyn OsCompletionSink>) {
        *self.sink.lock() = Some(sink);
    }

    pub fn sink(&self) -> Option<Arc<dyn OsCompletionSink>> {
        self.sink.lock().clone()
    }

    pub(super) fn begin_lease(&self, key: &str) -> Option<u64> {
        let mut live = self.live.lock();
        if live.contains_key(key) { return None; }
        let lease = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        live.insert(key.to_string(), lease);
        Some(lease)
    }

    #[cfg(test)]
    pub(super) fn begin(&self, key: &str) -> bool { self.begin_lease(key).is_some() }

    #[cfg(test)]
    pub(super) fn is_live(&self, key: &str) -> bool { self.live.lock().contains_key(key) }

    pub(crate) fn is_current(&self, key: &str, lease: u64) -> bool {
        self.live.lock().get(key).is_some_and(|current| *current == lease)
    }

    /// The source of stream `key` ended: close the key and tell the sink, unless `cancel-call` or a new ask took it first.
    pub(crate) fn end_stream(&self, key: &str, lease: u64, site: &CallSite) {
        if self.cancel_current(key, lease) {
            if let Some(sink) = self.sink() {
                sink.ended(site, key);
            }
        }
    }

    pub(crate) fn message(&self, key: &str, lease: u64, site: &CallSite, event: Value) -> bool {
        if !self.is_current(key, lease) { return false; }
        if let Some(sink) = self.sink() { sink.message(site, key, event); }
        true
    }

    /// The keys of every open call, sorted: completions still wanted (streams, pending deferred answers) and claimed
    /// completions whose delivery has not finished.
    pub fn live_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.live.lock().keys().cloned().collect();
        keys.extend(self.delivering.lock().keys().cloned());
        keys.sort();
        keys.dedup();
        keys
    }

    /// Drop the pending completion of `key`: its outcome is never delivered. `false` when none is pending.
    pub fn cancel(&self, key: &str) -> bool {
        self.live.lock().remove(key).is_some()
    }

    pub(super) fn cancel_current(&self, key: &str, lease: u64) -> bool {
        let mut live = self.live.lock();
        if live.get(key) != Some(&lease) { return false; }
        live.remove(key);
        true
    }

    pub(super) fn clear(&self) {
        self.live.lock().clear();
        self.delivering.lock().clear();
    }

    /// Take the right to deliver `key`'s completion: `false` when it was cancelled first. Once claimed, a late
    /// `cancel` finds nothing pending, so the host can reap its child and still deliver the outcome.
    #[cfg(test)]
    pub(super) fn claim(&self, key: &str) -> bool {
        let lease = self.live.lock().get(key).copied();
        lease.is_some_and(|lease| self.claim_current(key, lease))
    }

    pub(super) fn claim_current(&self, key: &str, lease: u64) -> bool {
        let mut live = self.live.lock();
        if live.get(key) != Some(&lease) { return false; }
        live.remove(key);
        *self.delivering.lock().entry(key.to_string()).or_default() += 1;
        true
    }

    /// Hand a claimed completion to the sink; the key stays open until the sink's dispatch has run.
    pub(super) fn deliver(&self, key: &str, site: &CallSite, outcome: Result<Value, OsError>) {
        self.deliver_with(key, |sink| sink.complete(site, key, outcome));
    }

    fn deliver_with(&self, key: &str, hand: impl FnOnce(&dyn OsCompletionSink)) {
        if let Some(sink) = self.sink() {
            hand(sink.as_ref());
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
        Self::spawn_later(this, key, site, move |_| work(), |completions, key, site, outcome| {
            completions.deliver(key, site, outcome)
        })
    }

    /// Answer a keyed, `onMessage`-routed substrate op as a stream (EXEC-QUEUE-13): `next` is polled on a thread of its
    /// own and each item it yields is the call's `onMessage` event, until the source ends or `cancel-call` drops the key.
    pub fn open_substrate_stream(
        this: &Arc<Self>,
        key: &str,
        site: &CallSite,
        mut next: impl FnMut() -> StreamPoll + Send + 'static,
    ) -> IoAnswer {
        let Some(lease) = this.begin_lease(key) else {
            return IoAnswer::Substrate(Err(SubstrateFailure {
                message: format!("key {key} already has a stream open"),
                errors: Vec::new(),
            }));
        };
        let (completions, owned_key, owned_site) = (this.clone(), key.to_string(), site.clone());
        let started = std::thread::Builder::new().name(format!("substrate-stream-{key}")).spawn(move || {
            let (key, site) = (owned_key, owned_site);
            while completions.is_current(&key, lease) {
                match next() {
                    StreamPoll::Item(item) => {
                        if !completions.message(&key, lease, &site, item) {
                            return;
                        }
                    }
                    StreamPoll::Idle => std::thread::sleep(STREAM_IDLE),
                    StreamPoll::Ended => {
                        completions.end_stream(&key, lease, &site);
                        return;
                    }
                }
            }
        });
        match started {
            Ok(_) => IoAnswer::ServiceDeferred,
            Err(error) => {
                this.cancel_current(key, lease);
                IoAnswer::Substrate(Err(SubstrateFailure { message: error.to_string(), errors: Vec::new() }))
            }
        }
    }

    /// Answer a keyed substrate op (R-a) by running `work` on a thread of its own and settling its completion through
    /// the sink, `op` and its `args` shaping the success payload as an in-place completion would.
    pub fn defer_substrate(
        this: &Arc<Self>,
        key: &str,
        site: &CallSite,
        op: &str,
        args: &[Value],
        work: impl FnOnce() -> Result<Value, SubstrateFailure> + Send + 'static,
    ) -> IoAnswer {
        let (op, args) = (op.to_string(), args.to_vec());
        let started = Self::spawn_later(
            this,
            key,
            site,
            move |_| {
                work()
                    .and_then(|value| crate::agent_ports::program_answer(&op, &args, value))
                    .map(|value| crate::agent_ports::completion_result(&op, value))
                    .map_err(|failure| crate::agent_ports::failure_payload(&failure))
            },
            |completions, key, site, outcome| {
                completions.deliver_with(key, |sink| sink.settle(site, key, outcome))
            },
        );
        match started {
            Ok(()) => IoAnswer::ServiceDeferred,
            Err(RunLaterError::KeyPending) => IoAnswer::Substrate(Err(SubstrateFailure {
                message: format!("key {key} already has a completion pending"),
                errors: Vec::new(),
            })),
            Err(RunLaterError::Spawn(error)) => IoAnswer::Substrate(Err(SubstrateFailure {
                message: error.to_string(),
                errors: Vec::new(),
            })),
        }
    }

    pub(super) fn spawn_later<T: Send + 'static>(
        this: &Arc<Self>,
        key: &str,
        site: &CallSite,
        work: impl FnOnce(u64) -> T + Send + 'static,
        deliver: fn(&Completions, &str, &CallSite, T),
    ) -> Result<(), RunLaterError> {
        let Some(lease) = this.begin_lease(key) else {
            return Err(RunLaterError::KeyPending);
        };
        let (completions, key, site) = (this.clone(), key.to_string(), site.clone());
        let started = std::thread::Builder::new().name(format!("os-later-{key}")).spawn({
            let (completions, key) = (completions.clone(), key.clone());
            move || {
                #[cfg(test)]
                completions.observe_test_phase(&key, lease, CompletionTestPhase::BeforeWork);
                {
                    let outcome = work(lease);
                    #[cfg(test)]
                    completions.observe_test_phase(&key, lease, CompletionTestPhase::BeforeClaim);
                    if completions.claim_current(&key, lease) {
                        deliver(&completions, &key, &site, outcome);
                    }
                }
                #[cfg(test)]
                completions.observe_test_phase(&key, lease, CompletionTestPhase::Terminal);
            }
        });
        started.map(drop).map_err(|e| {
            completions.cancel_current(&key, lease);
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

/// One poll of a substrate stream's source.
pub enum StreamPoll {
    /// The next item, delivered on the call's `onMessage` route.
    Item(Value),
    /// Nothing yet; the stream polls again shortly.
    Idle,
    /// The source is gone; the stream closes without an event.
    Ended,
}

const STREAM_IDLE: std::time::Duration = std::time::Duration::from_millis(10);

/// Why [`Completions::run_later`] did not start.
pub(super) enum RunLaterError {
    KeyPending,
    Spawn(std::io::Error),
}

/// How a [`KernelCompletionSink`] answers the host asks of the completion's own dispatch.
pub type CompletionAnswerer = dyn Fn(&IoRequest) -> IoAnswer + Send + Sync;

/// Told how each completion dispatch ended: the call site, the event and payload dispatched, the client whose request
/// deferred the key (EXEC-QUEUE-10: where the completion's result goes), and its outcomes or error.
pub type CompletionDone =
    dyn Fn(&CallSite, &str, &Value, Option<&str>, Result<DispatchOutcomes, DispatchError>) + Send + Sync;

/// A native host's work in flight per session: a keyed completion or stream of its own still live (EXEC-DIST-9).
pub struct OsSessionPending {
    completions: Arc<Completions>,
    sink: Arc<KernelCompletionSink>,
}

impl OsSessionPending {
    pub fn new(completions: Arc<Completions>, sink: Arc<KernelCompletionSink>) -> Self {
        Self { completions, sink }
    }
}

impl crate::kernel::SessionPending for OsSessionPending {
    fn pending(&self, session: &str) -> bool {
        self.sink.session_pending(session, &self.completions.live_keys())
    }
}

/// Dispatches a completion into a kernel as the event of the call's route: `emit.success` with the result
/// merged with `key`, `emit.failure` with `{ error, kind, key }`. A call that declared no route for the
/// outcome dispatches nothing. A completion can end before the dispatch that started the call has committed, so
/// the sink remembers which dispatch deferred which key: a host runs every dispatch that may defer a call
/// inside a [`DispatchScope`] ([`Self::begin_dispatch`], or [`Self::drive`]), and a completion for key K waits
/// for the dispatch that deferred K to end; the kernel's dispatch lane then orders it FIFO behind every other
/// dispatch (EXEC-CONC-1).
pub struct KernelCompletionSink {
    /// Weak: the kernel holds this sink (through its completions and session-pending check), so a strong edge back
    /// would keep an ended kernel, and everything it holds, alive for the life of the process.
    kernel: std::sync::Weak<RuntimeKernel>,
    answer: Arc<CompletionAnswerer>,
    done: Arc<CompletionDone>,
    owners: Arc<Mutex<HashMap<String, Arc<Ended>>>>,
    requesters: Requesters,
}

/// Who asked for a deferred key: the viewer and client of the dispatch that deferred it.
#[derive(Debug, Clone, Default)]
pub struct Requester {
    pub viewer: Option<Arc<Value>>,
    pub client_id: Option<String>,
    /// The frame session of the dispatch that deferred it (EXEC-DIST-9); `None` is the program session.
    pub session: Option<Arc<str>>,
}

/// Each deferred key's requester, and whether the key is a stream (kept until a new ask reuses the key).
type Requesters = Arc<Mutex<HashMap<String, (Arc<Requester>, bool)>>>;

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
    requester: Arc<Requester>,
    requesters: Requesters,
}

impl DispatchScope {
    /// Answer `request` with `inner`. A keyed ask that may defer, or that opens a stream, is registered to this
    /// dispatch before it is answered (the watcher may deliver before the answer returns) and released again if the
    /// host neither deferred it nor opened the stream.
    pub fn answer(&self, inner: &dyn Fn(&IoRequest) -> IoAnswer, request: &IoRequest) -> IoAnswer {
        let (key, stream) = match request {
            IoRequest::Os { op, key: Some(key), .. } if op.defers_completion() || op.is_stream() => (key, op.is_stream()),
            IoRequest::Substrate { key: Some(key), site: Some(_), .. } => (key, false),
            _ => return inner(request),
        };
        self.owners.lock().insert(key.clone(), self.ended.clone());
        self.ended.keys.lock().push(key.clone());
        let answered = inner(request);
        let open = match &answered {
            IoAnswer::ServiceDeferred => true,
            IoAnswer::Os(Ok(_)) => stream,
            _ => false,
        };
        if open {
            self.requesters.lock().insert(key.clone(), (self.requester.clone(), stream));
        } else {
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
            kernel: Arc::downgrade(&kernel),
            answer,
            done,
            owners: Arc::default(),
            requesters: Arc::default(),
        }
    }

    /// Start tracking one dispatch with no client. Hold the scope until the dispatch has committed.
    pub fn begin_dispatch(&self) -> DispatchScope {
        self.begin_dispatch_for(None)
    }

    /// Start tracking one dispatch serving `client_id`; the viewer this thread entered is its requester's.
    pub fn begin_dispatch_for(&self, client_id: Option<&str>) -> DispatchScope {
        let kernel = self.kernel.upgrade();
        self.scope(Arc::new(Requester {
            viewer: kernel.as_ref().and_then(|kernel| kernel.thread_viewer()),
            client_id: client_id.map(str::to_string),
            session: kernel.as_ref().and_then(|kernel| kernel.thread_session()),
        }))
    }

    fn scope(&self, requester: Arc<Requester>) -> DispatchScope {
        DispatchScope {
            ended: Arc::default(),
            owners: self.owners.clone(),
            requester,
            requesters: self.requesters.clone(),
        }
    }

    /// Whether a deferred key of `session` is still wanted: a pending ask, an open stream or subscription. `live` is
    /// the keys of every open call ([`Completions::live_keys`]).
    pub fn session_pending(&self, session: &str, live: &[String]) -> bool {
        self.requesters
            .lock()
            .iter()
            .any(|(key, (requester, _))| requester.session.as_deref() == Some(session) && live.contains(key))
    }

    /// The requester of `key`: taken for a finite ask, kept for a stream's later items.
    fn requester_of(&self, key: &str) -> Arc<Requester> {
        let mut requesters = self.requesters.lock();
        match requesters.get(key) {
            Some((requester, true)) => requester.clone(),
            Some(_) => requesters.remove(key).map(|(requester, _)| requester).unwrap_or_default(),
            None => Arc::default(),
        }
    }

    /// Run `progress` to its end, answering its asks with the sink's answerer, as one tracked dispatch.
    pub fn drive(
        &self,
        progress: Progress<DispatchOutcomes>,
    ) -> Result<DispatchOutcomes, DispatchError> {
        let kernel = self.kernel.upgrade().ok_or_else(|| {
            DispatchError::Kernel(crate::kernel::KernelError::Internal("the kernel this sink served has ended".to_string()))
        })?;
        let scope = self.begin_dispatch();
        let answer = |request: &IoRequest| scope.answer(&*self.answer, request);
        drive_blocking(&kernel, progress, &answer)
    }

    /// Wait for the dispatch that deferred `key` to end, when one is still running.
    fn wait_for_owner(&self, kernel: &RuntimeKernel, key: &str) {
        kernel.wait_for_parked_key(key);
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
        let outcome = self.finish_tls(key, outcome);
        if let Some((event, payload)) = completion_event(site, key, outcome) {
            self.dispatch_completion(site, key, event, payload);
        }
    }

    fn settle(&self, site: &CallSite, key: &str, outcome: Result<Value, Value>) {
        if let Some((event, payload)) = crate::resumable::substrate_completion_event(site, key, outcome) {
            self.dispatch_completion(site, key, event, payload);
        }
    }

    fn ended(&self, site: &CallSite, key: &str) {
        let Some(kernel) = self.kernel.upgrade() else { return };
        self.wait_for_owner(&kernel, key);
        let requester = self.requesters.lock().remove(key).map(|(requester, _)| requester).unwrap_or_default();
        let _session = requester.session.as_ref().map(|session| kernel.enter_session(session));
        if let Err(error) = kernel.end_keyed_ask(&site.trait_name, &site.entity_id, key) {
            tracing::warn!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, key, error = %error, "the end of a stream could not settle its row");
        }
    }
}

impl KernelCompletionSink {
    /// A deferred raw ask the kernel parked a TLS operation on (EXEC-HTTP-7) ends in the kernel: it decrypts or
    /// encrypts the host's raw outcome and answers the next raw ask through the sink's answerer until the program's
    /// outcome is known. Any other key's outcome is returned as it stands.
    #[cfg(feature = "wire-drivers")]
    fn finish_tls(&self, key: &str, outcome: Result<Value, OsError>) -> Result<Value, OsError> {
        use crate::kernel::TlsCompletion;
        let Some(kernel) = self.kernel.upgrade() else { return outcome };
        let mut raw = outcome;
        let mut parked = false;
        loop {
            match kernel.tls_complete(key, raw.clone()) {
                None if !parked => return raw,
                None => return Err(OsError::new(orbital_core_base::os::OsErrorKind::Tls, "the TLS operation vanished")),
                Some(TlsCompletion::Done(done)) => return done,
                Some(TlsCompletion::Ask { op, args }) => {
                    parked = true;
                    raw = match (self.answer)(&IoRequest::Os { op, args, key: None, site: None }) {
                        IoAnswer::Os(answer) => answer.map(Secret::into_inner),
                        other => Err(OsError::new(orbital_core_base::os::OsErrorKind::Io, format!("unexpected host answer {other:?}"))),
                    };
                }
            }
        }
    }

    #[cfg(not(feature = "wire-drivers"))]
    fn finish_tls(&self, _key: &str, outcome: Result<Value, OsError>) -> Result<Value, OsError> {
        outcome
    }

    /// EXEC-QUEUE-10: the completion runs as the viewer that asked, and a key it defers again keeps that requester.
    fn dispatch_completion(&self, site: &CallSite, key: &str, event: String, payload: Value) {
        // A completion that outlives its kernel has nothing to dispatch into.
        let Some(kernel) = self.kernel.upgrade() else { return };
        self.wait_for_owner(&kernel, key);
        let requester = self.requester_of(key);
        let _viewer = requester.viewer.as_ref().map(|viewer| kernel.enter_viewer((**viewer).clone()));
        let _session = requester.session.as_ref().map(|session| kernel.enter_session(session));
        let scope = self.scope(requester.clone());
        let answer = |request: &IoRequest| scope.answer(&*self.answer, request);
        let result = kernel
            .start_dispatch(
                &site.trait_name,
                Some(site.entity_id.as_str()),
                &event,
                payload.clone(),
                None,
                None,
            )
            .and_then(|progress| drive_blocking(&kernel, progress, &answer));
        (self.done)(site, &event, &payload, requester.client_id.as_deref(), result);
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
            entity_id: crate::process_result::RUNTIME_SINGLETON.to_string(),
            emit: CallRoutes {
                success: success.map(str::to_string),
                failure: failure.map(str::to_string),
                ..CallRoutes::default()
            },
        }
    }

    fn exited(code: f64) -> Value {
        Value::object(ValueMap::from_iter([("code".into(), Value::Number(code))]))
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
            Some(&Value::string("k1"))
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
            Some(&Value::string("closed"))
        );
        assert_eq!(
            payload.get_path("key"),
            Some(&Value::string("k1"))
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
        use crate::secret_source::SecretSource;
        use crate::os_host::{OsGrants, OsHandles, OsHost};
        use crate::resumable::{os_completion_cancelled, PortHost};
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
            secrets: None,
        };
        let host = PortHost {
            secrets: &NoSecrets,
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
            "typeContractVersion": 1, "orbitals": [{
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
            Arc::new(|_, _, _, _, result| assert!(result.is_ok(), "{result:?}"));
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

    /// The kernel is returned so the test holds it: the sink only borrows it weakly.
    fn lone_sink() -> (Arc<KernelCompletionSink>, Arc<RuntimeKernel>, std::sync::mpsc::Receiver<String>) {
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
        let done: Arc<CompletionDone> = Arc::new(move |_, event, _, _, _| {
            let _ = told.lock().send(event.to_string());
        });
        (Arc::new(KernelCompletionSink::new(kernel.clone(), answer, done)), kernel, ran)
    }

    #[test]
    fn a_key_re_armed_after_cancel_is_freed_by_its_own_completion_and_not_by_the_cancelled_one() {
        let completions = Completions::default();
        let first = completions.begin_lease("shared").expect("first lease");
        assert!(completions.begin_lease("shared").is_none());
        assert!(completions.cancel("shared"));
        let second = completions.begin_lease("shared").expect("re-armed lease");
        assert!(!completions.claim_current("shared", first));
        assert!(completions.is_live("shared"));
        assert!(completions.claim_current("shared", second));
        assert!(!completions.is_live("shared"));
        assert!(completions.begin_lease("shared").is_some());
    }

    #[test]
    fn a_completion_after_its_kernel_ended_dispatches_nothing() {
        let (sink, kernel, ran) = lone_sink();
        drop(kernel);
        sink.complete(&site(Some("DONE"), None), "k", Ok(exited(0.0)));
        assert!(ran.try_recv().is_err(), "an ended kernel has nothing to dispatch into");
    }

        type Asked = std::sync::mpsc::Receiver<(String, Option<String>, Option<Value>)>;

    /// A sink whose completion dispatches report the event, the client they were delivered for and the viewer they ran as.
    fn requester_sink() -> (Arc<KernelCompletionSink>, Arc<RuntimeKernel>, Asked) {
        use crate::config::KernelConfig;
        use orbital_core_base::effect_types::RuntimeEnvironment;
        let kernel = Arc::new(RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(crate::entity_source::InMemoryEntityCache::new()),
            KernelConfig::default(),
        ));
        let answer: Arc<CompletionAnswerer> = Arc::new(|_| IoAnswer::Service(Err("no host".to_string())));
        let (told, asked) = std::sync::mpsc::channel();
        let told = Mutex::new(told);
        let seen = Arc::downgrade(&kernel);
        let done: Arc<CompletionDone> = Arc::new(move |_, event, _, client_id, _| {
            let viewer = seen.upgrade().and_then(|k| k.thread_viewer()).map(|v| (*v).clone());
            let _ = told.lock().send((event.to_string(), client_id.map(str::to_string), viewer));
        });
        (Arc::new(KernelCompletionSink::new(kernel.clone(), answer, done)), kernel, asked)
    }

    fn viewer(id: &str) -> Value {
        Value::object(ValueMap::from_iter([("id".into(), Value::String(id.to_string().into()))]))
    }

    #[test]
    fn a_completion_runs_as_the_viewer_and_for_the_client_whose_dispatch_deferred_its_key() {
        let (sink, kernel, asked) = requester_sink();
        let asking = {
            let _viewer = kernel.enter_viewer(viewer("ada"));
            let scope = sink.begin_dispatch_for(Some("tab-1"));
            assert!(matches!(scope.answer(&deferred, &defers("k1")), IoAnswer::ServiceDeferred));
            scope
        };
        drop(asking);
        complete_on_a_thread(&sink, "k1", "DONE").join().expect("k1");
        let (event, client, ran_as) = asked.recv_timeout(std::time::Duration::from_secs(5)).expect("dispatched");
        assert_eq!(event, "DONE");
        assert_eq!(client.as_deref(), Some("tab-1"));
        assert_eq!(ran_as, Some(viewer("ada")));
        complete_on_a_thread(&sink, "k1", "AGAIN").join().expect("k1 again");
        let (_, client, ran_as) = asked.recv_timeout(std::time::Duration::from_secs(5)).expect("dispatched");
        assert_eq!((client, ran_as), (None, None), "control: a finite key's requester is used once");
    }

    #[test]
    fn control_a_completion_deferred_with_no_client_or_viewer_runs_with_neither() {
        let (sink, _kernel, asked) = requester_sink();
        let scope = sink.begin_dispatch();
        assert!(matches!(scope.answer(&deferred, &defers("k2")), IoAnswer::ServiceDeferred));
        drop(scope);
        complete_on_a_thread(&sink, "k2", "DONE").join().expect("k2");
        assert_eq!(asked.recv_timeout(std::time::Duration::from_secs(5)).expect("dispatched"), ("DONE".to_string(), None, None));
    }

    #[test]
    fn a_stream_keeps_its_requester_for_every_item() {
        let (sink, kernel, asked) = requester_sink();
        {
            let _viewer = kernel.enter_viewer(viewer("ada"));
            let scope = sink.begin_dispatch_for(Some("tab-1"));
            assert!(matches!(scope.answer(&|_| IoAnswer::Os(Ok(Value::Null.into())), &listens("s1")), IoAnswer::Os(Ok(_))));
        }
        for _ in 0..2 {
            complete_on_a_thread(&sink, "s1", "MSG").join().expect("s1");
            let (_, client, ran_as) = asked.recv_timeout(std::time::Duration::from_secs(5)).expect("dispatched");
            assert_eq!((client.as_deref(), ran_as), (Some("tab-1"), Some(viewer("ada"))));
        }
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
        let (sink, _kernel, ran) = lone_sink();
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
        let (sink, _kernel, ran) = lone_sink();
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
        let (sink, _kernel, ran) = lone_sink();
        let open = sink.begin_dispatch();
        let opened = open.answer(&|_| IoAnswer::Os(Ok(Value::Null.into())), &listens("s1"));
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
        assert_eq!(payload.get_path("key"), Some(&Value::string("s")));
        assert!(got[1].is_none(), "control: a stream with no onMessage route fires nothing");
    }

    #[test]
    fn control_a_key_the_host_did_not_defer_is_not_waited_for() {
        let (sink, _kernel, ran) = lone_sink();
        let open = sink.begin_dispatch();
        let answered = open.answer(&|_| IoAnswer::Service(Err("no".to_string())), &defers("k1"));
        assert!(!matches!(answered, IoAnswer::ServiceDeferred));
        complete_on_a_thread(&sink, "k1", "NOW").join().expect("k1");
        assert_eq!(ran.recv_timeout(std::time::Duration::from_secs(5)).as_deref(), Ok("NOW"));
        drop(open);
    }

    struct Ends(Mutex<Vec<String>>);
    impl OsCompletionSink for Ends {
        fn complete(&self, _: &CallSite, _: &str, _: Result<Value, OsError>) {}
        fn ended(&self, _: &CallSite, key: &str) {
            self.0.lock().push(key.to_string());
        }
    }

    fn open_stream(completions: &Arc<Completions>, key: &str, polls: Vec<StreamPoll>) {
        let mut polls = polls.into_iter();
        let answer = Completions::open_substrate_stream(completions, key, &site(None, None), move || {
            polls.next().unwrap_or(StreamPoll::Idle)
        });
        assert!(matches!(answer, IoAnswer::ServiceDeferred));
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < limit, "timed out waiting for {what}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_stream_whose_source_ends_tells_the_sink_its_key_ended() {
        let completions = Arc::new(Completions::default());
        let ends = Arc::new(Ends(Mutex::new(Vec::new())));
        completions.set_sink(ends.clone());
        open_stream(&completions, "s1", vec![StreamPoll::Item(exited(1.0)), StreamPoll::Ended]);
        wait_until("the end", || !ends.0.lock().is_empty());
        assert_eq!(*ends.0.lock(), vec!["s1".to_string()]);
        assert!(completions.live_keys().is_empty());
    }

    #[test]
    fn control_a_stream_cancelled_by_cancel_call_does_not_report_an_end() {
        let completions = Arc::new(Completions::default());
        let ends = Arc::new(Ends(Mutex::new(Vec::new())));
        completions.set_sink(ends.clone());
        open_stream(&completions, "s2", vec![]);
        assert!(completions.cancel("s2"));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(ends.0.lock().is_empty(), "cancel-call ends the ask in the kernel, not through the host");
    }

    #[test]
    fn a_cancelled_worker_cannot_message_cancel_or_claim_a_reopened_key() {
        struct Count(Mutex<Vec<Value>>);
        impl OsCompletionSink for Count {
            fn complete(&self, _: &CallSite, _: &str, outcome: Result<Value, OsError>) {
                self.0.lock().push(outcome.expect("success"));
            }
        }
        let completions = Completions::default();
        let sink = Arc::new(Count(Mutex::new(vec![])));
        completions.set_sink(sink.clone());
        let site = site(Some("OK"), Some("BAD"));
        let old = completions.begin_lease("reused").expect("first call");
        assert!(completions.cancel("reused"));
        let current = completions.begin_lease("reused").expect("second call");
        assert_ne!(old, current);
        assert!(!completions.is_current("reused", old));
        assert!(!completions.message("reused", old, &site, exited(1.0)));
        assert!(!completions.cancel_current("reused", old));
        assert!(!completions.claim_current("reused", old));
        assert!(completions.message("reused", current, &site, exited(2.0)));
        assert!(completions.claim_current("reused", current));
        completions.deliver("reused", &site, Ok(exited(3.0)));
        assert_eq!(*sink.0.lock(), vec![exited(2.0), exited(3.0)]);
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
