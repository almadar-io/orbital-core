//! One step's exchanges: the asks it made, the answers it got, and its ledger.

use super::*;

/// One request the in-flight step made and the answer it got.
#[derive(Debug, Clone)]
pub(crate) struct Exchange {
    pub(super) request: IoRequest,
    pub(super) answer: IoAnswer,
}

impl Exchange {
    pub(crate) fn new(request: IoRequest, answer: IoAnswer) -> Self {
        Self { request, answer }
    }
}

/// The rows a dispatch has read from the host, by (entity type, id); `None` is a row the host answered as absent. A
/// dispatch asks the host for a row once: its later steps read the answer, until the kernel writes that entity type.
/// A step that stops to await the host keeps none of its own reads or invalidations (its replay makes them again from
/// the journal), so the journal and the cache never both answer the same read.
pub(crate) type ReadCache = HashMap<(String, String), Option<Arc<Value>>>;

/// What the replayed step left behind: the request it could not go on without, or a request that
/// disagrees with the recorded one.
pub(crate) struct StepEnd {
    pub(crate) reads: Arc<ReadCache>,
    pub(crate) journal: Vec<Exchange>,
    pub(crate) pending: Asks,
    /// Row asks on store-bound entities, answered by their driver orbital in-kernel (EXEC-STORE-4).
    pub(crate) store_asks: Vec<StoreAsk>,
    pub(crate) diverged: Option<ResumeError>,
}

/// A row ask on an entity bound to a store-driver orbital: never sent to the host, answered by the driver's
/// keyed reply (`store_driver::answer_for`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StoreAsk {
    pub(crate) driver: String,
    pub(crate) key: String,
    /// For an ensure ask (`ensure` set) this is an unused placeholder: the verb is built from `ensure`.
    pub(crate) request: IoRequest,
    /// The access paths this ask sends its driver (EXEC-STORE-6), in place of a row ask.
    pub(crate) ensure: Option<EnsureIndexes>,
}

/// An entity's declared `@index` paths, sent once to the driver it is bound to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EnsureIndexes {
    pub(crate) entity_type: String,
    pub(crate) indexes: Vec<orbital_core_base::schema::types::EntityIndex>,
}

/// Which journal entries a step has claimed: a prefix counter while answers are claimed in order (no
/// allocation), and the indices claimed ahead of it when a keyed call is answered out of order.
#[derive(Default)]
pub(super) struct Claimed {
    pub(super) prefix: usize,
    pub(super) ahead: Vec<usize>,
}

impl Claimed {
    pub(super) fn covers(&self, entries: usize) -> bool {
        self.prefix >= entries
    }

    pub(super) fn has(&self, index: usize) -> bool {
        index < self.prefix || self.ahead.contains(&index)
    }

    pub(super) fn claim(&mut self, index: usize) {
        if index != self.prefix {
            self.ahead.push(index);
            return;
        }
        self.prefix += 1;
        while let Some(at) = self.ahead.iter().position(|&i| i == self.prefix) {
            self.ahead.swap_remove(at);
            self.prefix += 1;
        }
    }
}

pub(super) struct StepLedger {
    pub(super) thread: ThreadId,
    pub(super) reads: Arc<ReadCache>,
    pub(super) reads_at_start: Arc<ReadCache>,
    pub(super) journal: Vec<Exchange>,
    pub(super) claimed: Claimed,
    pub(super) pending: Asks,
    pub(super) store_asks: Vec<StoreAsk>,
    pub(super) keys: HashSet<String>,
    pub(super) blocked: bool,
    pub(super) diverged: Option<ResumeError>,
}

/// The requests one step leaves outstanding: inline for the common single ask, so a step allocates only once
/// it has more than one.
pub(crate) type Asks = smallvec::SmallVec<[IoRequest; 1]>;

/// The entity a row ask addresses (a batch: its first op's).
pub(super) fn request_entity(request: &IoRequest) -> Option<&str> {
    match request {
        IoRequest::ReadRow { entity_type, .. }
        | IoRequest::ListRows { entity_type }
        | IoRequest::QueryRows { entity_type, .. }
        | IoRequest::CreateRow { entity_type, .. }
        | IoRequest::WriteRow { entity_type, .. }
        | IoRequest::DeleteRow { entity_type, .. } => Some(entity_type),
        IoRequest::WriteRows { batch } => batch.ops.first().map(RowWriteOp::entity_type),
        IoRequest::CommitRows { rows } => rows.first().map(|row| row.entity_type.as_str()),
        _ => None,
    }
}

/// Give each of `requests` the next id of its dispatch, in issue order.
pub(crate) fn number_requests(next_id: &mut u64, requests: Asks) -> Vec<PendingRequest> {
    let mut numbered = Vec::with_capacity(requests.len());
    for request in requests {
        let id = RequestId::from_raw(*next_id);
        *next_id += 1;
        numbered.push(PendingRequest { id, request });
    }
    numbered
}

/// The error a handler returns when the step cannot get an answer yet; the step's result is dropped.
pub(crate) const AWAITING_HOST: &str = "awaiting host I/O";

pub(crate) const NO_PAGE_FACTS_HOST: &str = "browser/page-facts: no host answers page facts";
pub(crate) const NO_TAB_HOST: &str = "browser/tab: no host answers browser tabs";

pub(crate) const NO_PROMPT_HOST: &str = "browser/prompt: no on-device model is wired into this host";

pub(crate) const NO_EMBED_HOST: &str = "browser/embed: no on-device embedder is wired into this host";

/// The error a call settled `Cancelled` carries, as JS's `untilAborted` rejects.
pub(super) const CALL_CANCELLED: &str = "The call was cancelled";

pub(super) enum Asked {
    Direct,
    Answer(IoAnswer),
    Withheld,
}
