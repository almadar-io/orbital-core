//! The one arrival-order queue behind the kernel's lane (EXEC-CONC-2, EXEC-CONC-8): work that cannot run yet waits
//! here under a ticket, and the kernel, not the host, picks what runs next. The server's start/resume queue and the
//! client's fold queue are its two callers; each decides when an entry may run, this decides the order.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::VecDeque;

pub(crate) struct TicketQueue<W> {
    entries: VecDeque<(u64, W)>,
}

impl<W> Default for TicketQueue<W> {
    fn default() -> Self {
        Self { entries: VecDeque::new() }
    }
}

impl<W> TicketQueue<W> {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn push(&mut self, ticket: u64, work: W) {
        self.entries.push_back((ticket, work));
    }

    pub(crate) fn pop_front(&mut self) -> Option<(u64, W)> {
        self.entries.pop_front()
    }

    /// The oldest entry `ready` accepts.
    pub(crate) fn take_first(&mut self, ready: impl Fn(&W) -> bool) -> Option<(u64, W)> {
        let at = self.entries.iter().position(|(_, work)| ready(work))?;
        self.entries.remove(at)
    }

    pub(crate) fn any(&self, matches: impl Fn(&W) -> bool) -> bool {
        self.entries.iter().any(|(_, work)| matches(work))
    }

    pub(crate) fn retain(&mut self, keep: impl Fn(&W) -> bool) {
        self.entries.retain(|(_, work)| keep(work));
    }
}
