//! `program/dispatch { reply }` into a guest (EXEC-CONC-7): a call waiting for an outcome the guest reaches later, in any of
//! its dispatches, is answered by the first emit of one of its events whose payload carries the same match value.

use super::RuntimeKernel;
use crate::effects::response::EventResponse;
use crate::process_result::ProcessEventResult;
use crate::resumable::GuestReply;
use orbital_core_base::sync::{Condvar, Mutex};
use orbital_core_base::value::Value;
use std::sync::Arc;

pub(crate) struct ReplyWaiter {
    wanted: GuestReply,
    got: Mutex<Option<(String, Value)>>,
    ready: Condvar,
}

/// One registered wait; dropping it unregisters it, whether it was answered, timed out or its dispatch failed.
pub(crate) struct ReplyWait {
    waiter: Arc<ReplyWaiter>,
    waiters: Arc<Mutex<Vec<Arc<ReplyWaiter>>>>,
}

impl ReplyWait {
    /// The reply `(event, payload)`, or `None` when none came within `timeout`.
    pub(crate) fn wait(&self, timeout: std::time::Duration) -> Option<(String, Value)> {
        let deadline = std::time::Instant::now() + timeout;
        let mut got = self.waiter.got.lock();
        while got.is_none() {
            let now = std::time::Instant::now();
            if now >= deadline {
                return None;
            }
            self.waiter.ready.wait_for(&mut got, deadline - now);
        }
        got.clone()
    }
}

impl Drop for ReplyWait {
    fn drop(&mut self) {
        self.waiters.lock().retain(|w| !Arc::ptr_eq(w, &self.waiter));
    }
}

impl RuntimeKernel {
    pub(crate) fn await_reply(&self, wanted: &GuestReply) -> ReplyWait {
        let waiter = Arc::new(ReplyWaiter { wanted: wanted.clone(), got: Mutex::new(None), ready: Condvar::new() });
        self.reply_waiters.lock().push(Arc::clone(&waiter));
        ReplyWait { waiter, waiters: Arc::clone(&self.reply_waiters) }
    }

    /// Offer what a finished dispatch emitted to the waiting calls; a sealed field reaches the caller as its stub.
    pub(super) fn offer_replies(&self, outcomes: &[(String, ProcessEventResult)]) {
        if self.reply_waiters.lock().is_empty() {
            return;
        }
        let response = EventResponse::from_process_results(outcomes, "", "");
        for emitted in &response.emitted_events {
            let Some(payload) = emitted.payload.as_ref() else { continue };
            let waiters = self.reply_waiters.lock().clone();
            for waiter in waiters {
                let wanted = &waiter.wanted;
                if !wanted.events.iter().any(|e| *e == emitted.event) || payload.get(wanted.field.as_str()) != Some(&wanted.value) {
                    continue;
                }
                let mut got = waiter.got.lock();
                if got.is_none() {
                    let mut seen = Some(payload.clone());
                    if let Some(trait_name) = emitted.source.as_ref().and_then(|source| source.r#trait.as_deref()) {
                        self.redact_sealed_payload(trait_name, &emitted.event, &mut seen);
                    }
                    *got = Some((emitted.event.clone(), seen.unwrap_or(Value::Null)));
                    waiter.ready.notify_all();
                }
            }
        }
    }
}
