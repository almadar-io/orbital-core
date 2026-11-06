//! The `gpu/` asks a client-role kernel holds for a host that installed a GPU answerer in it (EXEC-GPU-1). A client
//! dispatch never awaits its host, so every `gpu/` ask is answered `ServiceDeferred` and kept here in ask order; the
//! host answers them one at a time, and each outcome comes back as its route event, dispatched with the payload held
//! here, so no handle or byte crosses to the host.

use std::prelude::v1::*;

use super::server::{held_completion, HeldGpuAsk, HeldGpuCompletion};
use super::wire::CallSiteWire;
use crate::kernel::RuntimeKernel;
use crate::resumable::{CallSite, DirectOs, IoAnswer, IoRequest};
use orbital_core_base::os::{family_of, OsError, OsErrorKind, OsFamily};
use orbital_core_base::sync::Mutex;
use orbital_core_base::value::Value;
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Default)]
struct Held {
    on: bool,
    asks: VecDeque<HeldGpuAsk>,
    payloads: HashMap<u64, Value>,
    next: u64,
}

#[derive(Debug, Default)]
pub(crate) struct ClientGpu {
    held: Mutex<Held>,
}

impl ClientGpu {
    pub(crate) fn hold(&self) {
        self.held.lock().on = true;
    }

    pub(crate) fn take_ask(&self) -> Option<HeldGpuAsk> {
        self.held.lock().asks.pop_front()
    }

    pub(crate) fn completion(
        &self,
        kernel: &RuntimeKernel,
        ask: &HeldGpuAsk,
        outcome: Result<Value, OsError>,
    ) -> Option<HeldGpuCompletion> {
        held_completion(kernel, ask, outcome, |payload| {
            let mut held = self.held.lock();
            held.next += 1;
            let ticket = held.next;
            held.payloads.insert(ticket, payload);
            ticket
        })
    }

    pub(crate) fn take_payload(&self, ticket: u64) -> Option<Value> {
        self.held.lock().payloads.remove(&ticket)
    }
}

impl DirectOs for ClientGpu {
    fn ask(&self, request: IoRequest, site: Option<CallSite>) -> Option<IoAnswer> {
        let IoRequest::Os {
            op,
            args,
            key,
            orbital,
            ..
        } = request
        else {
            return None;
        };
        if family_of(op) != OsFamily::Gpu {
            return None;
        }
        let mut held = self.held.lock();
        if !held.on {
            return None;
        }
        let routed = site
            .as_ref()
            .is_some_and(|site| site.emit.success.is_some() || site.emit.failure.is_some());
        if key.is_none() || !routed {
            return Some(IoAnswer::Os(Err(OsError::new(
                OsErrorKind::Unsupported,
                "a client answers a gpu call later, so the call needs a key and an emit route"
                    .to_string(),
            ))));
        }
        held.asks.push_back(HeldGpuAsk {
            op,
            args,
            key,
            site: site.as_ref().map(CallSiteWire::from),
            orbital,
        });
        Some(IoAnswer::ServiceDeferred)
    }
}
