//! A deferred ask's outcome entering the kernel (EXEC-QUEUE-10): the one path a live host's completion sink and a replayed
//! step's scripted answer both take, so a replay mints, budgets and routes exactly as a live host's completion does.

use super::*;
use crate::resumable::{
    completion_event, drive_blocking, program_call_key, CallSite, DispatchError, DispatchOutcomes,
    IoAnswer, IoRequest,
};
use orbital_core_base::os::{OsError, OsErrorKind};

/// A route event a completion dispatched: the event and payload that ran (the failure route when the success payload was
/// refused), whether that is the failure route, and the outcomes of its dispatch.
pub struct DeliveredCompletion {
    pub event: String,
    pub payload: Value,
    pub failed: bool,
    pub result: Result<DispatchOutcomes, DispatchError>,
}

impl RuntimeKernel {
    /// The route event of a deferred os ask's outcome under host key `key`: a deferred `gpu/device-open` mints its device
    /// first. `None` when the route the outcome takes is not declared; the bool is true for the failure route.
    pub fn completion_route(
        &self,
        site: &CallSite,
        key: &str,
        outcome: Result<Value, OsError>,
    ) -> Option<(String, Value, bool)> {
        let outcome = self.gpu_complete(key, outcome);
        let failed = outcome.is_err();
        completion_event(site, key, outcome).map(|(event, payload)| (event, payload, failed))
    }

    /// Dispatch a completion's route event on its row, `answer` answering the asks it makes. A success payload that breaks
    /// its route's declared contract is refused at entry (G-CROSS-335) and the ask still ends: the failure route fires as
    /// `{ error, kind: invalid, key }`, never nothing. A refusal raised by an emit further down the cascade is that
    /// cascade's error, not the completion's: the route event stands.
    pub fn dispatch_route(
        &self,
        site: &CallSite,
        key: &str,
        event: String,
        payload: Value,
        failed: bool,
        answer: &dyn Fn(&IoRequest) -> IoAnswer,
    ) -> DeliveredCompletion {
        let run = |event: &str, payload: &Value| {
            self.start_dispatch(
                &site.trait_name,
                Some(site.entity_id.as_str()),
                event,
                payload.clone(),
                None,
                None,
            )
            .and_then(|progress| drive_blocking(self, progress, answer))
        };
        let (mut event, mut payload, mut failed) = (event, payload, failed);
        let mut result = run(&event, &payload);
        if let Err(DispatchError::Kernel(KernelError::PayloadInvalid(refusal))) = &result {
            // A refused success completion reaches the asker's failure route, whether its own payload or a payload its
            // cascade emits broke a declared contract: the asker always hears an answer (owner 2026-11-12, G-CROSS-581).
            let failure = site.emit.failure.clone().filter(|failure| {
                site.emit.success.as_deref() == Some(event.as_str()) && *failure != event
            });
            tracing::warn!(
                target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                trait_name = %site.trait_name, event = %event, refusal = %refusal, routed_to_failure = failure.is_some(),
                "a completion payload broke its route's declared contract",
            );
            if let Some(failure) = failure {
                let mut fields = ValueMap::default();
                fields.insert(
                    "error".into(),
                    Value::string(format!("{event} was refused: {refusal}")),
                );
                fields.insert(
                    "kind".into(),
                    Value::string(OsErrorKind::Invalid.as_str().to_string()),
                );
                fields.insert(
                    "key".into(),
                    Value::string(program_call_key(key).to_string()),
                );
                (event, payload, failed) = (failure, Value::object(fields), true);
                result = run(&event, &payload);
            }
        }
        DeliveredCompletion {
            event,
            payload,
            failed,
            result,
        }
    }
}
