//! The kernel's one dispatch lane (EXEC-CONC-0/1, G-CROSS-291): a dispatch holds it from its entry to its commit,
//! parked on an unkeyed ask included, and every other dispatch entry waits its turn in arrival order (a ticket
//! queue). Each holding is a [`LaneTurn`]. A call carrying the holder's turn re-enters while the holder runs and is
//! refused ([`KernelError::LaneHeldByParkedDispatch`]) while it is parked: it would otherwise wait on itself. A call
//! carrying no turn waits its FIFO turn, whatever thread it runs on, except an in-place call made on the thread the
//! running holder occupies (a blocking host answering an ask with a nested dispatch), which re-enters.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::sync::{Condvar, Mutex};
use std::sync::Arc;
use orbital_core_base::sync::ThreadKey as ThreadId;

use super::KernelError;

/// One holding of the lane: the ticket it was granted. A dispatch hands it to the calls it makes on its own behalf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LaneTurn(u64);

#[derive(Default)]
struct LaneState {
    next: u64,
    serving: u64,
    held: bool,
    /// The thread running the holder; `None` while the holder is parked on an ask.
    running_on: Option<ThreadId>,
    depth: usize,
    /// The event of the holder while it is parked on an ask.
    parked: Option<String>,
}

#[derive(Default)]
pub(crate) struct DispatchLane {
    state: Mutex<LaneState>,
    turn: Condvar,
}

impl DispatchLane {
    /// Take the lane in arrival order, or re-enter it as the holder `turn` names.
    pub(crate) fn enter(
        self: &Arc<Self>,
        turn: Option<LaneTurn>,
    ) -> Result<LaneGuard, KernelError> {
        let mut s = self.state.lock();
        if s.held {
            let holder = LaneTurn(s.serving);
            let in_place = turn.is_none() && s.running_on == Some(orbital_core_base::sync::current_thread());
            if turn == Some(holder) || in_place {
                if let Some(event) = &s.parked {
                    return Err(KernelError::LaneHeldByParkedDispatch {
                        ticket: holder.0,
                        event: event.clone(),
                    });
                }
                s.depth += 1;
                return Ok(LaneGuard {
                    lane: Some(self.clone()),
                    turn: holder,
                    outer: false,
                });
            }
        }
        let ticket = s.next;
        s.next += 1;
        while s.held || s.serving != ticket {
            self.turn.wait(&mut s);
        }
        s.held = true;
        s.running_on = Some(orbital_core_base::sync::current_thread());
        s.depth = 1;
        Ok(LaneGuard {
            lane: Some(self.clone()),
            turn: LaneTurn(ticket),
            outer: true,
        })
    }

    fn release(&self, s: &mut LaneState) {
        s.held = false;
        s.running_on = None;
        s.depth = 0;
        s.parked = None;
        s.serving += 1;
        self.turn.notify_all();
    }
}

/// A hold on the lane under one turn; dropping the outermost one releases it.
pub(crate) struct LaneGuard {
    lane: Option<Arc<DispatchLane>>,
    turn: LaneTurn,
    outer: bool,
}

impl LaneGuard {
    pub(crate) fn turn(&self) -> LaneTurn {
        self.turn
    }

    /// The dispatch parked on an ask: keep the lane for it, detached from any thread. A re-entered guard has
    /// nothing of its own to keep (its holder still runs), so it only steps out. `event` names the parked dispatch.
    pub(crate) fn park(mut self, event: String) -> Option<LaneHold> {
        let lane = self.lane.take()?;
        let mut s = lane.state.lock();
        if self.outer && s.depth == 1 {
            s.parked = Some(event);
            s.running_on = None;
            s.depth = 0;
            drop(s);
            Some(LaneHold {
                lane: Some(lane),
                turn: self.turn,
            })
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

/// The lane held by a parked dispatch; resuming binds it to the resuming call, dropping it (an abandoned
/// continuation) releases it.
pub struct LaneHold {
    lane: Option<Arc<DispatchLane>>,
    turn: LaneTurn,
}

impl LaneHold {
    pub(crate) fn turn(&self) -> LaneTurn {
        self.turn
    }

    pub(crate) fn resume(mut self) -> LaneGuard {
        let lane = self.lane.take().expect("a parked lane hold");
        let mut s = lane.state.lock();
        s.running_on = Some(orbital_core_base::sync::current_thread());
        s.depth = 1;
        s.parked = None;
        drop(s);
        LaneGuard {
            lane: Some(lane),
            turn: self.turn,
            outer: true,
        }
    }
}

impl Drop for LaneHold {
    fn drop(&mut self) {
        let Some(lane) = self.lane.take() else { return };
        let mut s = lane.state.lock();
        lane.release(&mut s);
    }
}
