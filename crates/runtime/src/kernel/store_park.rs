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
    use crate::resumable::{IoRequest, StoreAsk};
    use orbital_core_base::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[derive(Clone, Debug)]
    enum Role {
        Own,
        Lead(Arc<ReadLead>),
        /// Set once the follower has parked: driven again with no reply, it runs its own driver.
        Follow(Arc<AtomicBool>),
    }

    #[derive(Clone, Debug)]
    pub(crate) struct LegWait {
        lineage: Arc<MountLineage>,
        role: Role,
    }

    impl LegWait {
        pub(crate) fn new(key: &str) -> Self {
            Self {
                lineage: MountLineage::for_store_ask(key),
                role: Role::Own,
            }
        }

        /// Deferred asks and their completions on this thread are the leg's work until the scope drops.
        pub(crate) fn enter(&self) -> LineageScope {
            enter_lineage(Some(self.lineage.clone()))
        }

        pub(crate) fn has_open_work(&self) -> bool {
            !matches!(self.role, Role::Follow(_)) && self.lineage.open_tickets() > 0
        }

        /// The reply has come, or the driver's work has settled without one: waiting would not block. A follower
        /// is ready only once the read it shares has a reply.
        pub(crate) fn ready(&self) -> bool {
            match self.role {
                Role::Follow(_) => self.lineage.has_reply(),
                _ => self.lineage.has_reply() || self.lineage.open_tickets() == 0,
            }
        }

        pub(crate) fn wait(&self) {
            match self.role {
                Role::Follow(_) => self.lineage.wait_offered(),
                _ => self.lineage.wait_reply(),
            }
        }

        pub(crate) fn take_reply(&self) -> Option<DispatchOutcomes> {
            self.lineage.take_reply()
        }

        /// For a follower, the key of the read it shares; `None` for a leg that runs its own driver.
        pub(crate) fn shared_key(&self) -> Option<&str> {
            match self.role {
                Role::Follow(_) => self.lineage.store_ask(),
                _ => None,
            }
        }

        /// A follower driven before its read replied: true the first time (it parks), false after.
        pub(crate) fn park_follower(&self) -> bool {
            match &self.role {
                Role::Follow(parked) => !parked.swap(true, Ordering::SeqCst),
                _ => false,
            }
        }

        /// A leading read has its reply: every follower takes it, and the read stops taking followers.
        pub(crate) fn landed(&self, outcomes: &DispatchOutcomes) {
            if let Role::Lead(lead) = &self.role {
                if !self.lineage.has_reply() {
                    self.lineage.offer_reply(outcomes.clone());
                }
                lead.close();
            }
        }
    }

    /// Hand a finished dispatch's outcomes to the store ask whose work this thread runs, when they reply to it.
    pub(crate) fn offer_reply(outcomes: &DispatchOutcomes) {
        let Some(lineage) = thread_lineage() else {
            return;
        };
        if lineage
            .store_ask()
            .is_some_and(|key| carries_reply(key, outcomes))
        {
            lineage.offer_reply(outcomes.clone());
        }
    }

    struct InFlight {
        driver: String,
        request: IoRequest,
        lineage: Arc<MountLineage>,
    }

    /// EXEC-STORE-7: the row reads in flight that an identical read may share.
    #[derive(Clone, Default)]
    pub(crate) struct SharedReads(Arc<Mutex<Vec<InFlight>>>);

    impl std::fmt::Debug for SharedReads {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SharedReads")
                .field("in_flight", &self.0.lock().len())
                .finish()
        }
    }

    /// The leader of a shared read; dropped before its reply came, it ends every follower with no reply.
    pub(crate) struct ReadLead {
        reads: SharedReads,
        lineage: Arc<MountLineage>,
    }

    impl std::fmt::Debug for ReadLead {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ReadLead").finish()
        }
    }

    impl ReadLead {
        fn close(&self) {
            self.reads
                .0
                .lock()
                .retain(|read| !Arc::ptr_eq(&read.lineage, &self.lineage));
        }
    }

    impl Drop for ReadLead {
        fn drop(&mut self) {
            self.close();
            if !self.lineage.has_reply() {
                self.lineage.offer_reply(DispatchOutcomes::default());
            }
        }
    }

    fn entity_of(request: &IoRequest) -> Option<&str> {
        match request {
            IoRequest::ReadRow { entity_type, .. }
            | IoRequest::ListRows { entity_type }
            | IoRequest::QueryRows { entity_type, .. } => Some(entity_type),
            _ => None,
        }
    }

    impl SharedReads {
        /// The wait for `ask`: a follower of an identical read in flight, a leader others may follow, or a leg of
        /// its own. A write stops every read in flight of the entities it writes from taking followers.
        pub(crate) fn wait_for(&self, ask: &StoreAsk) -> LegWait {
            let mut in_flight = self.0.lock();
            if let IoRequest::WriteRows { batch } = &ask.request {
                in_flight.retain(|read| {
                    entity_of(&read.request)
                        .is_none_or(|entity| !batch.ops.iter().any(|op| op.entity_type() == entity))
                });
            }
            if ask.ensure.is_some() || entity_of(&ask.request).is_none() {
                return LegWait::new(&ask.key);
            }
            if let Some(read) = in_flight
                .iter()
                .find(|read| read.driver == ask.driver && read.request == ask.request)
            {
                return LegWait {
                    lineage: read.lineage.clone(),
                    role: Role::Follow(Arc::new(AtomicBool::new(false))),
                };
            }
            let lineage = MountLineage::for_store_ask(&ask.key);
            in_flight.push(InFlight {
                driver: ask.driver.clone(),
                request: ask.request.clone(),
                lineage: lineage.clone(),
            });
            LegWait {
                lineage: lineage.clone(),
                role: Role::Lead(Arc::new(ReadLead {
                    reads: self.clone(),
                    lineage,
                })),
            }
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

        pub(crate) fn ready(&self) -> bool {
            true
        }

        pub(crate) fn wait(&self) {}

        pub(crate) fn take_reply(&self) -> Option<DispatchOutcomes> {
            None
        }
    }

    impl LegWait {
        pub(crate) fn shared_key(&self) -> Option<&str> {
            None
        }

        pub(crate) fn park_follower(&self) -> bool {
            false
        }

        pub(crate) fn landed(&self, _outcomes: &DispatchOutcomes) {}
    }

    pub(crate) fn offer_reply(_outcomes: &DispatchOutcomes) {}

    /// A kernel with no completion sink answers each read in its own dispatch: no two are in flight together.
    #[derive(Clone, Debug, Default)]
    pub(crate) struct SharedReads;

    impl SharedReads {
        pub(crate) fn wait_for(&self, ask: &crate::resumable::StoreAsk) -> LegWait {
            LegWait::new(&ask.key)
        }
    }
}

pub(crate) use native::{offer_reply, LegWait, SharedReads};

#[cfg(all(test, feature = "std", not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::resumable::{IoRequest, StoreAsk};
    use crate::row_query::{RowWriteBatch, RowWriteFailureMode, RowWriteOp};
    use orbital_core_base::value::Value;

    fn ask(key: &str, request: IoRequest) -> StoreAsk {
        StoreAsk {
            driver: "Rows".into(),
            key: key.into(),
            request,
            ensure: None,
        }
    }

    fn read(entity: &str, id: &str) -> IoRequest {
        IoRequest::ReadRow {
            entity_type: entity.into(),
            id: id.into(),
        }
    }

    fn write(entity: &str) -> IoRequest {
        IoRequest::WriteRows {
            batch: RowWriteBatch {
                ops: vec![RowWriteOp::Upsert {
                    entity_type: entity.into(),
                    data: Value::Null,
                }],
                on_failure: RowWriteFailureMode::Atomic,
            },
        }
    }

    #[test]
    fn an_identical_read_in_flight_is_followed_and_answered_by_the_leaders_reply() {
        let reads = SharedReads::default();
        let lead = reads.wait_for(&ask("s1", read("Project", "p1")));
        let follow = reads.wait_for(&ask("s2", read("Project", "p1")));
        assert_eq!(lead.shared_key(), None);
        assert_eq!(follow.shared_key(), Some("s1"));
        assert!(!follow.ready());
        lead.landed(&DispatchOutcomes::default());
        assert!(follow.ready());
        assert!(follow.take_reply().is_some());
    }

    #[test]
    fn control_a_different_read_leads_its_own() {
        let reads = SharedReads::default();
        let _first = reads.wait_for(&ask("s1", read("Project", "p1")));
        assert_eq!(
            reads
                .wait_for(&ask("s2", read("Project", "p2")))
                .shared_key(),
            None
        );
        assert_eq!(
            reads.wait_for(&ask("s3", read("Task", "p1"))).shared_key(),
            None
        );
    }

    #[test]
    fn a_landed_read_takes_no_more_followers() {
        let reads = SharedReads::default();
        let lead = reads.wait_for(&ask("s1", read("Project", "p1")));
        lead.landed(&DispatchOutcomes::default());
        assert_eq!(
            reads
                .wait_for(&ask("s2", read("Project", "p1")))
                .shared_key(),
            None
        );
    }

    #[test]
    fn a_write_to_the_entity_stops_a_read_in_flight_from_taking_followers() {
        let reads = SharedReads::default();
        let _lead = reads.wait_for(&ask("s1", read("Project", "p1")));
        let _write = reads.wait_for(&ask("w1", write("Project")));
        assert_eq!(
            reads
                .wait_for(&ask("s2", read("Project", "p1")))
                .shared_key(),
            None
        );
    }

    #[test]
    fn control_a_write_to_another_entity_keeps_the_read_shared() {
        let reads = SharedReads::default();
        let _lead = reads.wait_for(&ask("s1", read("Project", "p1")));
        let _write = reads.wait_for(&ask("w1", write("Task")));
        assert_eq!(
            reads
                .wait_for(&ask("s2", read("Project", "p1")))
                .shared_key(),
            Some("s1")
        );
    }

    #[test]
    fn a_leader_dropped_before_its_reply_ends_its_followers_with_none() {
        let reads = SharedReads::default();
        let lead = reads.wait_for(&ask("s1", read("Project", "p1")));
        let follow = reads.wait_for(&ask("s2", read("Project", "p1")));
        drop(lead);
        assert!(follow.ready());
        assert_eq!(follow.take_reply().map(|o| o.len()), Some(0));
        assert_eq!(
            reads
                .wait_for(&ask("s3", read("Project", "p1")))
                .shared_key(),
            None
        );
    }

    #[test]
    fn a_write_never_shares() {
        let reads = SharedReads::default();
        let _first = reads.wait_for(&ask("w1", write("Project")));
        assert_eq!(
            reads.wait_for(&ask("w2", write("Project"))).shared_key(),
            None
        );
    }
}
