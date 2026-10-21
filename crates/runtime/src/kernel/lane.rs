//! The kernel's one dispatch lane (EXEC-CONC-1, G-CROSS-291): a dispatch holds it from its entry to its commit,
//! parked on an unkeyed ask included, and every other dispatch entry waits its turn in arrival order (a ticket
//! queue). The thread holding it re-enters freely, so a dispatch's own sub-dispatches (a store leg's driver, a
//! `process_event` step) never wait on it.

use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::thread::ThreadId;

#[derive(Default)]
struct LaneState {
    next: u64,
    serving: u64,
    held: bool,
    /// The thread running the holder; `None` while the holder is parked on an ask.
    thread: Option<ThreadId>,
    depth: usize,
}

#[derive(Default)]
pub(crate) struct DispatchLane {
    state: Mutex<LaneState>,
    turn: Condvar,
}

impl DispatchLane {
    /// Take the lane, or re-enter it when this thread already runs the holder.
    pub(crate) fn enter(self: &Arc<Self>) -> LaneGuard {
        let me = std::thread::current().id();
        let mut s = self.state.lock();
        if s.held && s.thread == Some(me) {
            s.depth += 1;
            return LaneGuard { lane: Some(self.clone()), outer: false };
        }
        let ticket = s.next;
        s.next += 1;
        while s.held || s.serving != ticket {
            self.turn.wait(&mut s);
        }
        s.held = true;
        s.thread = Some(me);
        s.depth = 1;
        LaneGuard { lane: Some(self.clone()), outer: true }
    }

    fn release(&self, s: &mut LaneState) {
        s.held = false;
        s.thread = None;
        s.depth = 0;
        s.serving += 1;
        self.turn.notify_all();
    }
}

/// This thread's hold on the lane; dropping the outermost one releases it.
pub(crate) struct LaneGuard {
    lane: Option<Arc<DispatchLane>>,
    outer: bool,
}

impl LaneGuard {
    /// The dispatch parked on an ask: keep the lane for it, detached from this thread. A re-entered guard has
    /// nothing of its own to keep (its holder still runs), so it only steps out.
    pub(crate) fn park(mut self) -> Option<LaneHold> {
        let lane = self.lane.take()?;
        let mut s = lane.state.lock();
        if self.outer && s.depth == 1 {
            s.thread = None;
            s.depth = 0;
            drop(s);
            Some(LaneHold { lane: Some(lane) })
        } else {
            s.depth -= 1;
            None
        }
    }
}

impl Drop for LaneGuard {
    fn drop(&mut self) {
        let Some(lane) = self.lane.take() else { return };
        let mut s = lane.state.lock();
        s.depth -= 1;
        if s.depth == 0 {
            lane.release(&mut s);
        }
    }
}

/// The lane held by a parked dispatch; resuming binds it to the resuming thread, dropping it (an abandoned
/// continuation) releases it.
pub struct LaneHold {
    lane: Option<Arc<DispatchLane>>,
}

impl LaneHold {
    pub(crate) fn resume(mut self) -> LaneGuard {
        let lane = self.lane.take().expect("a parked lane hold");
        let mut s = lane.state.lock();
        s.thread = Some(std::thread::current().id());
        s.depth = 1;
        drop(s);
        LaneGuard { lane: Some(lane), outer: true }
    }
}

impl Drop for LaneHold {
    fn drop(&mut self) {
        let Some(lane) = self.lane.take() else { return };
        let mut s = lane.state.lock();
        lane.release(&mut s);
    }
}
