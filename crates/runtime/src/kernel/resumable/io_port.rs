//! `IoPort`, the one door the kernel's I/O goes through.

use super::*;

/// The one door the kernel's I/O goes through.
pub(crate) struct IoPort {
    pub(super) source: Arc<dyn EntitySource>,
    pub(super) service: Option<Arc<dyn ServiceHandler>>,
    pub(super) substrate: Option<Arc<dyn SubstrateHandler>>,
    pub(super) ledger: Mutex<Option<StepLedger>>,
    pub(super) gate: Mutex<()>,
    /// The call site the effect being run belongs to, stamped on the `call-service` it makes.
    pub(super) site: Mutex<Option<CallSite>>,
    /// How the host settled the last `call-service`, when not by a result or an error.
    pub(super) verdict: Mutex<Option<CallVerdict>>,
    /// The `key` of the substrate op being run, stamped on its ask with `site`.
    pub(super) substrate_key: Mutex<Option<String>>,
    /// What the host's query push-down honours, per entity type; an entity not listed is never sent a query.
    pub(super) query_capabilities: RwLock<HashMap<String, QueryCapabilities>>,
    /// What the host's conditional writes honour, per entity type; an entity not listed is never sent `WriteRows`.
    pub(super) write_capabilities: RwLock<HashMap<String, WriteCapabilities>>,
    /// Fallback notices already logged, one per (kind, entity, reason).
    pub(super) noticed: Mutex<HashSet<String>>,
    /// The kernel's configured clock: when set, the time a driver reads (TLS) is this clock's, not a host ask.
    pub(super) clock: Option<crate::config::DispatchClock>,
    /// `[runtime]` entities live in `memory` and are never a host ask, when the kernel is configured to own them.
    pub(super) runtime_in_kernel: bool,
    /// Entity -> why its driver could not ensure its declared indexes (EXEC-STORE-6).
    pub(super) index_failures: RwLock<HashMap<String, String>>,
    pub(super) runtime_types: RwLock<HashSet<String>>,
    pub(super) memory: crate::entity_source::InMemoryEntityCache,
    /// `[store: Driver]` bindings: entity type -> the driver orbital that answers its row asks.
    pub(super) store_drivers: RwLock<HashMap<String, String>>,
    /// Entity type -> the `[persistent: tbl]` collection it declares; sent to its store driver (EXEC-STORE-4).
    pub(super) collections: RwLock<HashMap<String, String>>,
    pub(super) store_seq: std::sync::atomic::AtomicU64,
    /// Where a sealed operator's fresh entropy comes from; never journaled.
    pub(super) entropy: Arc<dyn Fn() -> Result<[u8; 32], String> + Send + Sync>,
}

pub(super) struct Installed<'a> {
    pub(super) port: &'a IoPort,
    pub(super) _gate: MutexGuard<'a, ()>,
}

impl Drop for Installed<'_> {
    fn drop(&mut self) {
        *self.port.ledger.lock() = None;
    }
}

impl IoPort {
    pub(crate) fn new(
        source: Arc<dyn EntitySource>,
        service: Option<Arc<dyn ServiceHandler>>,
        substrate: Option<Arc<dyn SubstrateHandler>>,
    ) -> Self {
        Self {
            source,
            service,
            substrate,
            ledger: Mutex::new(None),
            gate: Mutex::new(()),
            site: Mutex::new(None),
            verdict: Mutex::new(None),
            substrate_key: Mutex::new(None),
            query_capabilities: RwLock::new(HashMap::new()),
            write_capabilities: RwLock::new(HashMap::new()),
            noticed: Mutex::new(HashSet::new()),
            clock: None,
            runtime_in_kernel: false,
            index_failures: RwLock::new(HashMap::new()),
            runtime_types: RwLock::new(HashSet::new()),
            memory: crate::entity_source::InMemoryEntityCache::new(),
            store_drivers: RwLock::new(HashMap::new()),
            collections: RwLock::new(HashMap::new()),
            store_seq: std::sync::atomic::AtomicU64::new(0),
            entropy: Arc::new(fresh_entropy),
        }
    }

    /// This port draws a sealed operator's entropy from `entropy`.
    #[cfg(test)]
    pub(crate) fn with_entropy(mut self, entropy: impl Fn() -> Result<[u8; 32], String> + Send + Sync + 'static) -> Self {
        self.entropy = Arc::new(entropy);
        self
    }

    /// Run the sealed operator `op` over `args`: `compute` gets fresh entropy that is never journaled, and the journal
    /// records the output it returns. A replay answers from the recorded output without running `compute`, and the same
    /// step run twice outside a replay draws different entropy.
    pub(crate) fn sealed(&self, op: &str, args: &[Value], compute: &mut dyn FnMut([u8; 32]) -> Value) -> Result<Value, String> {
        let request = IoRequest::Sealed { op: op.to_string(), args: args.to_vec() };
        let in_step = {
            let mut slot = self.ledger.lock();
            match slot.as_mut().filter(|ledger| ledger.thread == orbital_core_base::sync::current_thread()) {
                None => false,
                Some(ledger) => {
                    if ledger.diverged.is_some() {
                        return Err(AWAITING_HOST.to_string());
                    }
                    let next = (0..ledger.journal.len()).find(|i| !ledger.claimed.has(*i));
                    match next {
                        Some(i) => {
                            let recorded = &ledger.journal[i];
                            if let (true, IoAnswer::Sealed(out)) = (recorded.request == request, &recorded.answer) {
                                let out = out.clone();
                                ledger.claimed.claim(i);
                                return out;
                            }
                            ledger.diverged = Some(ResumeError::Diverged {
                                recorded: Box::new(recorded.request.clone()),
                                got: Box::new(request),
                            });
                            return Err(AWAITING_HOST.to_string());
                        }
                        None if ledger.blocked => return Err(AWAITING_HOST.to_string()),
                        None => true,
                    }
                }
            }
        };
        let entropy = (self.entropy)()?;
        let output = compute(entropy);
        if in_step {
            if let Some(ledger) = self.ledger.lock().as_mut() {
                ledger.journal.push(Exchange::new(request, IoAnswer::Sealed(Ok(output.clone()))));
                let at = ledger.journal.len() - 1;
                ledger.claimed.claim(at);
            }
        }
        Ok(output)
    }

    /// The create batch that writes `rows` of a store-bound `entity_type` through its driver; `None` when unbound.
    pub(crate) fn store_seed(&self, entity_type: &str, rows: Vec<Value>) -> Option<StoreAsk> {
        let driver = self.store_drivers.read().get(entity_type).cloned()?;
        let ops = rows
            .into_iter()
            .map(|data| RowWriteOp::Create { entity_type: entity_type.to_string(), data })
            .collect();
        Some(StoreAsk {
            driver,
            key: self.next_store_key(),
            request: IoRequest::WriteRows { batch: RowWriteBatch { ops, on_failure: RowWriteFailureMode::Stop } },
            ensure: None,
        })
    }

    /// The ask that sends the driver of `entity_type` its declared `indexes`; `None` when unbound.
    pub(crate) fn store_ensure(
        &self,
        entity_type: &str,
        indexes: Vec<orbital_core_base::schema::types::EntityIndex>,
    ) -> Option<StoreAsk> {
        let driver = self.store_drivers.read().get(entity_type).cloned()?;
        Some(StoreAsk {
            driver,
            key: self.next_store_key(),
            request: IoRequest::ListRows { entity_type: entity_type.to_string() },
            ensure: Some(EnsureIndexes { entity_type: entity_type.to_string(), indexes }),
        })
    }

    /// Record (or clear) why the indexes of `entity_type` could not be ensured: its row asks fail with it.
    pub(crate) fn set_index_failure(&self, entity_type: &str, failure: Option<String>) {
        let mut failures = self.index_failures.write();
        match failure {
            Some(message) => failures.insert(entity_type.to_string(), message),
            None => failures.remove(entity_type),
        };
    }

    pub(crate) fn next_store_key(&self) -> String {
        format!("store-{}", self.store_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    /// `entity_type` declares `store: <driver>`: its row asks are answered by that driver orbital.
    pub(crate) fn bind_store(&self, entity_type: &str, driver: &str) {
        self.store_drivers.write().insert(entity_type.to_string(), driver.to_string());
    }

    /// A store-bound entity's rows exist only in its driver, reached on the resumable path.
    pub(super) fn store_off_path(&self, entity_type: &str) -> String {
        let driver = self.store_drivers.read().get(entity_type).cloned().unwrap_or_default();
        format!("entity {entity_type} is bound to store driver {driver}: its rows are answered only on the resumable path (EXEC-STORE-4)")
    }

    /// The collection `entity_type` declares, when it declares one.
    pub(crate) fn collection_of(&self, entity_type: &str) -> Option<String> {
        self.collections.read().get(entity_type).cloned()
    }

    /// `data` for a create on a store-bound entity: a row naming no id is minted a UUID v4 drawn from the host's
    /// journaled randomness, so ids stay unique across restarts of a persistent store (EXEC-STEP-8, create versus
    /// upsert). A replay keeps the id its journaled create already carries, or re-reads the journaled draw.
    pub(super) fn with_minted_id<'d>(&self, entity_type: &str, data: &'d Value) -> Result<std::borrow::Cow<'d, Value>, String> {
        let Value::Object(fields) = data else {
            return Ok(std::borrow::Cow::Borrowed(data));
        };
        if !self.store_bound(entity_type)
            || self.direct()
            || fields.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty())
        {
            return Ok(std::borrow::Cow::Borrowed(data));
        }
        let id = match self.journaled_mint(entity_type, fields) {
            Some(id) => id,
            None => uuid_v4(&self.random(16)?)?,
        };
        let mut fields = fields.clone();
        fields.insert("id".into(), Value::string(id));
        Ok(std::borrow::Cow::Owned(Value::Object(fields)))
    }

    /// The id the next unclaimed journal entry already minted for this very create, when it is one.
    fn journaled_mint(&self, entity_type: &str, fields: &orbital_core_base::value::ValueMap) -> Option<String> {
        let slot = self.ledger.lock();
        let ledger = slot.as_ref()?;
        let next = (0..ledger.journal.len()).find(|i| !ledger.claimed.has(*i))?;
        let IoRequest::WriteRows { batch } = &ledger.journal[next].request else {
            return None;
        };
        let [RowWriteOp::Create { entity_type: recorded, data: Value::Object(row) }] = batch.ops.as_slice() else {
            return None;
        };
        let id = row.get("id").and_then(Value::as_str)?;
        let bare = |map: &orbital_core_base::value::ValueMap| {
            map.iter().filter(|(name, _)| name.as_str() != "id").map(|(n, v)| (n.clone(), v.clone())).collect::<Vec<_>>()
        };
        (recorded == entity_type && bare(row) == bare(fields)).then(|| id.to_string())
    }

    pub(super) fn store_bound(&self, entity_type: &str) -> bool {
        self.store_drivers.read().contains_key(entity_type)
    }

    /// The driver orbital that answers `request`; a batch is answered by one only when every op is bound to it.
    pub(super) fn store_driver_for(&self, request: &IoRequest) -> Option<String> {
        let drivers = self.store_drivers.read();
        match request {
            IoRequest::ReadRow { entity_type, .. }
            | IoRequest::ListRows { entity_type }
            | IoRequest::QueryRows { entity_type, .. } => drivers.get(entity_type).cloned(),
            IoRequest::WriteRows { batch } => {
                let mut ops = batch.ops.iter().map(|op| drivers.get(op.entity_type()));
                let first = ops.next()??;
                ops.all(|d| d == Some(first)).then(|| first.clone())
            }
            _ => None,
        }
    }

    /// A single delete or update on a store-bound entity: ONE conditional op, so a missing row fails on every
    /// store (EXEC-STEP-8). `None` when the entity is not bound or no step is in flight.
    pub(super) fn bound_single_write(
        &self,
        entity_type: &str,
        op: impl FnOnce() -> RowWriteOp,
    ) -> Option<Result<RowWriteOutcome, String>> {
        if !self.store_bound(entity_type) {
            return None;
        }
        if self.direct() {
            return Some(Err(self.store_off_path(entity_type)));
        }
        let batch = RowWriteBatch { ops: vec![op()], on_failure: RowWriteFailureMode::Stop };
        Some(match self.ask(|| IoRequest::WriteRows { batch }) {
            Asked::Answer(IoAnswer::Wrote(outcomes)) => {
                self.forget_reads(entity_type);
                outcomes.and_then(|mut o| o.pop().ok_or_else(|| "the store driver answered a write with no outcome".to_string()))
            }
            Asked::Direct | Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        })
    }

    /// Serve the rows of `[runtime]` entities from the kernel's own memory instead of asking the host.
    pub(crate) fn with_runtime_rows(mut self, in_kernel: bool) -> Self {
        self.runtime_in_kernel = in_kernel;
        self
    }

    /// `entity_type` is a `[runtime]` entity: its rows live in the kernel when the kernel owns them.
    pub(crate) fn claim_runtime(&self, entity_type: &str) {
        if self.runtime_in_kernel {
            self.runtime_types.write().insert(entity_type.to_string());
        }
    }

    pub(super) fn local(&self, entity_type: &str) -> bool {
        self.runtime_in_kernel && self.runtime_types.read().contains(entity_type)
    }

    /// Where the rows of `entity_type` are read and written when no resumable step is in flight.
    pub(super) fn source_for(&self, entity_type: &str) -> &dyn EntitySource {
        if self.local(entity_type) {
            &self.memory
        } else {
            self.source.as_ref()
        }
    }

    /// The entity a row request addresses, when it is one a kernel-owned `[runtime]` entity answers itself.
    pub(super) fn local_entity<'r>(&self, request: &'r IoRequest) -> Option<&'r str> {
        if let IoRequest::WriteRows { batch } = request {
            let first = batch.ops.first()?.entity_type();
            return batch.ops.iter().all(|op| self.local(op.entity_type())).then_some(first);
        }
        let entity = match request {
            IoRequest::ReadRow { entity_type, .. }
            | IoRequest::ListRows { entity_type }
            | IoRequest::QueryRows { entity_type, .. }
            | IoRequest::CreateRow { entity_type, .. }
            | IoRequest::WriteRow { entity_type, .. }
            | IoRequest::DeleteRow { entity_type, .. } => entity_type.as_str(),
            _ => return None,
        };
        self.local(entity).then_some(entity)
    }

    /// This port reads time from `clock`, the kernel's configured dispatch clock (`KernelConfig.clock`).
    pub(crate) fn with_clock(mut self, clock: Option<crate::config::DispatchClock>) -> Self {
        self.clock = clock;
        self
    }

    pub(crate) fn set_query_capabilities(
        &self,
        entity_type: &str,
        capabilities: QueryCapabilities,
    ) {
        self.query_capabilities
            .write()
            .insert(entity_type.to_string(), capabilities);
    }

    pub(crate) fn set_write_capabilities(
        &self,
        entity_type: &str,
        capabilities: WriteCapabilities,
    ) {
        self.write_capabilities
            .write()
            .insert(entity_type.to_string(), capabilities);
    }

    /// What the host declared its query push-down honours for `entity_type`, when it declared anything.
    pub(crate) fn query_capabilities_of(&self, entity_type: &str) -> Option<QueryCapabilities> {
        self.query_capabilities.read().get(entity_type).cloned()
    }

    /// One warn per (kind, entity, reason) per port.
    pub(super) fn notice(&self, kind: &str, entity_type: &str, reason: &str, detail: &str) {
        if self
            .noticed
            .lock()
            .insert(format!("{kind}\u{1f}{entity_type}\u{1f}{reason}"))
        {
            tracing::warn!(
                target: orbital_core_base::logging::ns::RUNTIME_FETCH,
                entity = %entity_type,
                reason = %reason,
                detail = %detail,
                "{}",
                kind
            );
        }
    }

    /// Whether I/O right now goes straight to the host handlers (no resumable step in flight on this thread).
    pub(super) fn direct(&self) -> bool {
        match self.ledger.lock().as_ref() {
            None => true,
            Some(ledger) => ledger.thread != orbital_core_base::sync::current_thread(),
        }
    }

    /// Stamp `site` on the `call-service` made while it is set.
    pub(crate) fn set_call_site(&self, site: Option<CallSite>) {
        *self.site.lock() = site;
        *self.verdict.lock() = None;
        *self.substrate_key.lock() = None;
    }

    /// Stamp `site` and `key` on the substrate op run while they are set.
    pub(crate) fn set_substrate_call(&self, site: Option<CallSite>, key: Option<String>) {
        *self.site.lock() = site;
        *self.verdict.lock() = None;
        *self.substrate_key.lock() = key;
    }

    /// A keyed substrate op with a declared route: the key and site its ask carries, else neither.
    pub(super) fn substrate_site(&self) -> (Option<String>, Option<CallSite>) {
        let key = self.substrate_key.lock().clone();
        let site = self.site.lock().clone().filter(|site| {
            site.emit.success.is_some() || site.emit.failure.is_some() || site.emit.on_message.is_some()
        });
        match (key, site) {
            (Some(key), Some(site)) => (Some(key), Some(site)),
            _ => (None, None),
        }
    }

    /// How the host settled the last `call-service`, when not by a result or an error.
    pub(crate) fn take_call_verdict(&self) -> Option<CallVerdict> {
        self.verdict.lock().take()
    }

    pub(crate) fn has_service(&self) -> bool {
        self.service.is_some()
    }

    pub(crate) fn has_substrate(&self) -> bool {
        self.substrate.is_some()
    }

    /// Whether the step in flight on this thread has asked for I/O it has no answer for yet, or
    /// diverged from its record: the step's result is garbage and must be rolled back.
    pub(crate) fn suspended(&self) -> bool {
        self.ledger.lock().as_ref().is_some_and(|l| {
            l.thread == orbital_core_base::sync::current_thread()
                && (!l.pending.is_empty() || !l.store_asks.is_empty() || l.diverged.is_some())
        })
    }

    /// Whether the step in flight is waiting on host or store answers, and has not diverged from its record.
    pub(crate) fn awaiting_host(&self) -> bool {
        self.ledger.lock().as_ref().is_some_and(|l| {
            l.thread == orbital_core_base::sync::current_thread()
                && l.diverged.is_none()
                && (!l.pending.is_empty() || !l.store_asks.is_empty())
        })
    }

    /// Run `step` with `journal` as the answers its I/O is served from. Steps from different threads
    /// take turns; I/O from threads that are not running a step goes straight to the host handlers.
    pub(crate) fn run_step<R>(
        &self,
        journal: Vec<Exchange>,
        step: impl FnOnce() -> R,
    ) -> (R, StepEnd) {
        self.run_step_reading(journal, Arc::default(), step)
    }

    /// [`Self::run_step`] with the rows the dispatch has already read: the step reads them without asking.
    pub(crate) fn run_step_reading<R>(
        &self,
        journal: Vec<Exchange>,
        reads: Arc<ReadCache>,
        step: impl FnOnce() -> R,
    ) -> (R, StepEnd) {
        let gate = self.gate.lock();
        *self.ledger.lock() = Some(StepLedger {
            thread: orbital_core_base::sync::current_thread(),
            claimed: Claimed::default(),
            reads_at_start: reads.clone(),
            reads,
            journal,
            pending: Asks::new(),
            store_asks: Vec::new(),
            keys: HashSet::new(),
            blocked: false,
            diverged: None,
        });
        let installed = Installed {
            port: self,
            _gate: gate,
        };
        let result = step();
        let ledger = installed.port.ledger.lock().take();
        drop(installed);
        let end = match ledger {
            Some(l) => StepEnd {
                reads: if l.pending.is_empty() && l.store_asks.is_empty() && l.diverged.is_none() {
                    l.reads
                } else {
                    l.reads_at_start
                },
                journal: l.journal,
                pending: l.pending,
                store_asks: l.store_asks,
                diverged: l.diverged,
            },
            None => StepEnd {
                reads: Arc::default(),
                journal: Vec::new(),
                pending: Asks::new(),
                store_asks: Vec::new(),
                diverged: None,
            },
        };
        (result, end)
    }

    /// The row the dispatch already read from the host, when the step in flight may answer from it.
    pub(super) fn read_before(&self, entity_type: &str, id: &str) -> Option<Option<Arc<Value>>> {
        let slot = self.ledger.lock();
        let ledger = slot.as_ref()?;
        if ledger.thread != orbital_core_base::sync::current_thread() || ledger.blocked || ledger.diverged.is_some() {
            return None;
        }
        ledger.reads.get(&(entity_type.to_string(), id.to_string())).cloned()
    }

    pub(super) fn remember_read(&self, entity_type: &str, id: &str, row: Option<Arc<Value>>) {
        if self.local(entity_type) {
            return;
        }
        if let Some(ledger) = self.ledger.lock().as_mut() {
            if ledger.thread == orbital_core_base::sync::current_thread() {
                Arc::make_mut(&mut ledger.reads).insert((entity_type.to_string(), id.to_string()), row);
            }
        }
    }

    /// The kernel wrote `entity_type`: what the dispatch read of it is no longer what the host holds.
    pub(super) fn forget_reads(&self, entity_type: &str) {
        if let Some(ledger) = self.ledger.lock().as_mut() {
            if ledger.thread == orbital_core_base::sync::current_thread() && ledger.reads.keys().any(|(t, _)| t == entity_type) {
                Arc::make_mut(&mut ledger.reads).retain(|(t, _), _| t != entity_type);
            }
        }
    }

    /// Serve `request` from the journal, or record it. A keyed `call-service` is matched to its recorded
    /// answer by key and, with no answer yet, is recorded as one more pending request while the call
    /// returns the deferred value, so the step goes on and a later keyed call is asked in the same step.
    /// Every other ask is matched to the first unclaimed journal entry and, with no answer yet, is the
    /// step's one pending request; any further ask of that kind is withheld unrecorded.
    pub(super) fn ask(&self, request: impl FnOnce() -> IoRequest) -> Asked {
        let mut slot = self.ledger.lock();
        let direct = match slot.as_ref() {
            None => true,
            Some(ledger) => ledger.thread != orbital_core_base::sync::current_thread(),
        };
        if direct {
            drop(slot);
            return self.direct_ask(request);
        }
        let Some(ledger) = slot.as_mut() else {
            return Asked::Direct;
        };
        if ledger.diverged.is_some() || (ledger.blocked && ledger.claimed.covers(ledger.journal.len())) {
            return Asked::Withheld;
        }
        let request = request();
        if let Some(answer) = self.index_failure_answer(&request) {
            return Asked::Answer(answer);
        }
        let keyed = match &request {
            IoRequest::CallService { key: Some(key), .. } => Some(key.clone()),
            _ => None,
        };
        if let Some(key) = &keyed {
            if !ledger.keys.insert(key.clone()) {
                return Asked::Answer(IoAnswer::Service(Err(format!(
                    "call-service key '{key}' is already used by another call in this step"
                ))));
            }
        }
        let store_driver = self.store_driver_for(&request);
        let claimed = &ledger.claimed;
        let at = ledger.journal.iter().enumerate().position(|(i, recorded)| {
            !claimed.has(i)
                && match &keyed {
                    Some(key) => matches!(
                        &recorded.request,
                        IoRequest::CallService { key: Some(recorded_key), .. } if recorded_key == key
                    ),
                    None => true,
                }
        });
        if let Some(driver) = store_driver.filter(|_| at.is_none() && !ledger.blocked) {
            ledger.store_asks.push(StoreAsk { driver, key: self.next_store_key(), request, ensure: None });
            ledger.blocked = true;
            return Asked::Withheld;
        }
        match at {
            Some(i) => {
                let recorded = &ledger.journal[i];
                if recorded.request == request {
                    let answer = recorded.answer.clone();
                    ledger.claimed.claim(i);
                    Asked::Answer(answer)
                } else {
                    ledger.diverged = Some(ResumeError::Diverged {
                        recorded: Box::new(recorded.request.clone()),
                        got: Box::new(request),
                    });
                    Asked::Withheld
                }
            }
            None if !ledger.blocked && self.local_entity(&request).is_some() => {
                let answer = BlockingHost { source: &self.memory, service: None, substrate: None }.answer(&request);
                ledger.journal.push(Exchange::new(request, answer.clone()));
                let at = ledger.journal.len() - 1;
                ledger.claimed.claim(at);
                Asked::Answer(answer)
            }
            None if keyed.is_some() && !ledger.blocked => {
                ledger.pending.push(request);
                Asked::Answer(IoAnswer::ServiceDeferred)
            }
            None => {
                if ledger.pending.is_empty() && !ledger.blocked {
                    ledger.pending.push(request);
                }
                ledger.blocked = true;
                Asked::Withheld
            }
        }
    }

    /// The failed answer of a row ask on an entity whose declared indexes the driver could not ensure.
    pub(super) fn index_failure_answer(&self, request: &IoRequest) -> Option<IoAnswer> {
        let failures = self.index_failures.read();
        if failures.is_empty() {
            return None;
        }
        let message = failures.get(request_entity(request)?)?;
        crate::store_driver::failed_answer(request, message.clone())
    }

    /// An ask with no step in flight runs in place, except a row ask on a store-bound entity, which fails.
    pub(super) fn direct_ask(&self, request: impl FnOnce() -> IoRequest) -> Asked {
        if self.store_drivers.read().is_empty() {
            return Asked::Direct;
        }
        let request = request();
        let Some(entity) = request_entity(&request) else {
            return Asked::Direct;
        };
        if !self.store_bound(entity) {
            return Asked::Direct;
        }
        let message = self.store_off_path(entity);
        match crate::store_driver::failed_answer(&request, message) {
            Some(answer) => Asked::Answer(answer),
            None => Asked::Direct,
        }
    }

    /// Take the keyed `call-service` this step has recorded as pending out of the step, so it is never sent.
    /// `false` when the step has no such pending call (or can no longer trust its own state); the cancel is
    /// then an ask of its own.
    pub(super) fn withdraw_pending_call(&self, key: &str) -> bool {
        let mut slot = self.ledger.lock();
        let Some(ledger) = slot.as_mut() else {
            return false;
        };
        if ledger.thread != orbital_core_base::sync::current_thread()
            || ledger.blocked
            || ledger.diverged.is_some()
        {
            return false;
        }
        let Some(at) = ledger.pending.iter().position(|pending| {
            matches!(pending, IoRequest::CallService { key: Some(pending_key), .. } if pending_key == key)
        }) else {
            return false;
        };
        ledger.pending.remove(at);
        true
    }
}

/// A UUID v4 string from exactly 16 random bytes.
fn uuid_v4(raw: &[u8]) -> Result<String, String> {
    let mut bytes: [u8; 16] = raw
        .try_into()
        .map_err(|_| format!("the host answered {} random bytes for an id, asked for 16", raw.len()))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = orbital_core_base::codec::hex(&bytes);
    Ok(format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]))
}
