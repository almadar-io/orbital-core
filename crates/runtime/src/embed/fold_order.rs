//! The client role's dispatch order (EXEC-CONC-8): a dispatch that owes the host a leg opens a flight in the
//! order the kernel ran it, and the answers fold in flight order, whatever order the posts resolve in. An answer
//! that arrives ahead of an older open flight is queued here and handed back by [`EmbeddedKernel::next_fold`] once
//! that flight ends; the host orders nothing.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::wire::AppliedWire;
use super::FoldBase;
use crate::effects::response::EventResponse;
use orbital_core_base::bus_event::EmittedEvent;
use orbital_core_base::sync::Mutex;
use super::ticket_queue::TicketQueue;
use std::collections::{BTreeSet, HashSet};

/// Who has already run an emit that is delivered to its listeners alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmitOrigin {
    /// Another client's push: the emitter's own arm did not run here.
    Peer,
    /// A live message of this client's own running call: the emitter's own arm ran as that call's progress dispatch.
    Own,
}

/// What a flight folds: a host's answer, or an emit delivered to its listeners alone.
pub(super) enum Work {
    Answer {
        response: EventResponse,
        already_delivered: HashSet<(String, String)>,
        base: Option<FoldBase>,
    },
    Emit(EmittedEvent, EmitOrigin),
}

/// An answer waiting for the flights ahead of it.
struct QueuedFold {
    flight: u64,
    work: Work,
}

/// An answer folded now, or the ticket it waits under.
#[derive(Debug)]
pub enum FoldProgress {
    Applied(Box<AppliedWire>),
    Queued { ticket: u64 },
}

/// A queued answer the kernel folded once its flight was the oldest open one, under the ticket `fold` answered with.
#[derive(Debug)]
pub struct FoldedNext {
    pub ticket: u64,
    pub outcome: Result<AppliedWire, String>,
}

#[derive(Default)]
struct Order {
    next_flight: u64,
    next_ticket: u64,
    open: BTreeSet<u64>,
    queued: TicketQueue<QueuedFold>,
}

#[derive(Default)]
pub(super) struct FoldOrder {
    state: Mutex<Order>,
}

pub(super) struct Ready {
    pub(super) work: Work,
}

impl FoldOrder {
    pub(super) fn open(&self) -> u64 {
        let mut s = self.state.lock();
        let flight = s.next_flight;
        s.next_flight += 1;
        s.open.insert(flight);
        flight
    }

    /// Fold now when `flight` is the oldest open one and nothing of its own waits ahead; otherwise queue the answer.
    pub(super) fn admit(
        &self,
        flight: u64,
        work: Work,
    ) -> Result<Result<Ready, u64>, String> {
        let mut s = self.state.lock();
        if !s.open.contains(&flight) {
            return Err(format!("no dispatch is in flight as {flight}"));
        }
        let oldest = s.open.first() == Some(&flight);
        if oldest && !s.queued.any(|q| q.flight == flight) {
            return Ok(Ok(Ready { work }));
        }
        let ticket = s.next_ticket;
        s.next_ticket += 1;
        s.queued.push(
            ticket,
            QueuedFold { flight, work },
        );
        Ok(Err(ticket))
    }

    /// The oldest queued answer of the oldest open flight, if it has one.
    pub(super) fn take_next(&self) -> Option<(u64, Ready)> {
        let mut s = self.state.lock();
        let oldest = *s.open.first()?;
        let (ticket, q) = s.queued.take_first(|q| q.flight == oldest)?;
        Some((ticket, Ready { work: q.work }))
    }

    /// The flight ended (its legs are folded, or the host gave up on them); answers still queued for it are dropped.
    pub(super) fn end(&self, flight: u64) -> bool {
        let mut s = self.state.lock();
        s.queued.retain(|q| q.flight != flight);
        s.open.remove(&flight)
    }

    /// Every open flight ends: a host that posts and folds synchronously has no overlapping dispatch.
    pub(super) fn end_all(&self) {
        let mut s = self.state.lock();
        s.queued.retain(|_| false);
        s.open.clear();
    }
}
