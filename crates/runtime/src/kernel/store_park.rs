//! EXEC-STORE-4 across deferred asks: a store driver's keyed I/O settles in later dispatches, so the ask parks without
//! the lane until a dispatch of the driver's work replies or that work settles with no reply.

use super::resumable::DispatchOutcomes;
use crate::store_driver::{reply_key, REPLY_EVENTS};
use orbital_core_base::effect_types::Effect;

/// Whether `outcomes` carry the driver's reply to the ask `key`.
pub(crate) fn carries_reply(key: &str, outcomes: &DispatchOutcomes) -> bool {
    outcomes.iter().flat_map(|(_, r)| r.effect_results.iter()).any(|er| {
        matches!(&er.effect, Effect::Emit { event, payload } if REPLY_EVENTS.contains(&event.as_str()) && reply_key(payload) == Some(key))
    })
}

#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
mod native {
    use super::*;
    use crate::kernel::mount_barrier::{enter_lineage, thread_lineage, LineageScope, MountLineage};
    use std::sync::Arc;

    #[derive(Clone, Debug)]
    pub(crate) struct LegWait(Arc<MountLineage>);

    impl LegWait {
        pub(crate) fn new(key: &str) -> Self {
            Self(MountLineage::for_store_ask(key))
        }

        /// Deferred asks and their completions on this thread are the leg's work until the scope drops.
        pub(crate) fn enter(&self) -> LineageScope {
            enter_lineage(Some(self.0.clone()))
        }

        pub(crate) fn has_open_work(&self) -> bool {
            self.0.open_tickets() > 0
        }

        pub(crate) fn wait(&self) {
            self.0.wait_reply();
        }

        pub(crate) fn take_reply(&self) -> Option<DispatchOutcomes> {
            self.0.take_reply()
        }
    }

    /// Hand a finished dispatch's outcomes to the store ask whose work this thread runs, when they reply to it.
    pub(crate) fn offer_reply(outcomes: &DispatchOutcomes) {
        let Some(lineage) = thread_lineage() else { return };
        if lineage.store_ask().is_some_and(|key| carries_reply(key, outcomes)) {
            lineage.offer_reply(outcomes.clone());
        }
    }
}

#[cfg(not(all(feature = "std", not(target_arch = "wasm32"))))]
mod native {
    use super::*;

    /// A kernel with no completion sink: a driver's asks are answered in its own dispatch and nothing parks.
    #[derive(Clone, Debug)]
    pub(crate) struct LegWait;

    impl LegWait {
        pub(crate) fn new(_key: &str) -> Self {
            Self
        }

        pub(crate) fn enter(&self) {}

        pub(crate) fn has_open_work(&self) -> bool {
            false
        }

        pub(crate) fn wait(&self) {}

        pub(crate) fn take_reply(&self) -> Option<DispatchOutcomes> {
            None
        }
    }

    pub(crate) fn offer_reply(_outcomes: &DispatchOutcomes) {}
}

pub(crate) use native::{offer_reply, LegWait};
