//! EXEC-SEED-7 across deferred asks: the work a route mount's `$MOUNT` deliveries started is a lineage, and the page's
//! own seeds wait until it has settled. A keyed ask deferred while a lineage is current holds a ticket until its
//! completion has run, and the completion, and any dispatch the deferred work makes, runs in the same lineage, so an
//! ask either of them defers holds a ticket of its own before the first one is returned.

use orbital_core_base::sync::{Condvar, Mutex};
use std::cell::RefCell;
use std::sync::Arc;

#[derive(Default)]
pub struct MountLineage {
    open: Mutex<usize>,
    settled: Condvar,
}

impl std::fmt::Debug for MountLineage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MountLineage")
            .field("open", &*self.open.lock())
            .finish()
    }
}

impl MountLineage {
    /// One deferred ask of the lineage, open until the ticket is dropped.
    pub fn ticket(self: &Arc<Self>) -> LineageTicket {
        *self.open.lock() += 1;
        LineageTicket(Arc::clone(self))
    }

    /// Block until no ticket of the lineage is open.
    pub fn wait_settled(&self) {
        let mut open = self.open.lock();
        while *open > 0 {
            self.settled.wait(&mut open);
        }
    }

    pub fn open_tickets(&self) -> usize {
        *self.open.lock()
    }
}

#[derive(Debug)]
pub struct LineageTicket(Arc<MountLineage>);

impl Drop for LineageTicket {
    fn drop(&mut self) {
        let mut open = self.0.open.lock();
        *open -= 1;
        if *open == 0 {
            self.0.settled.notify_all();
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<MountLineage>>> = const { RefCell::new(None) };
}

/// The lineage the calling thread runs in.
pub fn thread_lineage() -> Option<Arc<MountLineage>> {
    CURRENT.with(|current| current.borrow().clone())
}

/// Run the calling thread in `lineage` until the scope drops; the outer lineage comes back then.
pub fn enter_lineage(lineage: Option<Arc<MountLineage>>) -> LineageScope {
    LineageScope(CURRENT.with(|current| current.replace(lineage)))
}

pub struct LineageScope(Option<Arc<MountLineage>>);

impl Drop for LineageScope {
    fn drop(&mut self) {
        let outer = self.0.take();
        CURRENT.with(|current| *current.borrow_mut() = outer);
    }
}
