//! The completion of a keyed `os/` call that outlives its dispatch (EXEC-QUEUE-10). The host answers the ask
//! `ServiceDeferred` at once and, when the operation ends, hands its outcome to an [`OsCompletionSink`].
//! [`KernelCompletionSink`] turns that outcome into the event of the call's declared route and dispatches it.

use crate::kernel::RuntimeKernel;
use crate::resumable::{
    drive_blocking, CallSite, DispatchError, DispatchOutcomes, IoAnswer, IoRequest, Progress,
};
use orbital_core_base::os::OsError;
use orbital_core_base::value::{Value, ValueMap};
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

    /// Deliver `outcome` unless `key` was cancelled meanwhile.
    pub(super) fn finish(&self, key: &str, site: &CallSite, outcome: Result<Value, OsError>) {
        let wanted = self.live.lock().remove(key);
        if !wanted {
            return;
        }
        if let Some(sink) = self.sink() {
            sink.complete(site, key, outcome);
        }
    }
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
            .start_dispatch(&site.trait_name, None, &event, payload, None, None)
            .and_then(|progress| drive_blocking(&self.kernel, progress, &*self.answer));
        (self.done)(site, &event, result);
    }
}

/// The event and payload `outcome` fires on `site`'s routes.
pub fn completion_event(
    site: &CallSite,
    key: &str,
    outcome: Result<Value, OsError>,
) -> Option<(String, Value)> {
    let key = Value::String(key.to_string());
    match outcome {
        Ok(result) => {
            let mut fields = match result {
                Value::Object(fields) => fields,
                other => ValueMap::from([("value".to_string(), other)]),
            };
            fields.insert("key".to_string(), key);
            Some((site.emit.success.clone()?, Value::Object(fields)))
        }
        Err(error) => Some((
            site.emit.failure.clone()?,
            Value::Object(ValueMap::from([
                ("error".to_string(), Value::String(error.message)),
                (
                    "kind".to_string(),
                    Value::String(error.kind.as_str().to_string()),
                ),
                ("key".to_string(), key),
            ])),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resumable::CallRoutes;
    use orbital_core_base::os::OsErrorKind;

    fn site(success: Option<&str>, failure: Option<&str>) -> CallSite {
        CallSite {
            orbital: None,
            trait_name: "Sup".to_string(),
            follow_on: false,
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
