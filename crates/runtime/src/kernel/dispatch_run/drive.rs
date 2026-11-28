//! Driving a run straight through or resumably, and the seeds a store driver answers.

use super::*;

impl RuntimeKernel {
    /// Run every unit with the host's handlers answering at the I/O point: no suspension, no replay.
    pub(super) fn run_straight_through(&self, mut run: DispatchRun) -> Result<DispatchOutcomes, KernelError> {
        let _cost = self.observed_cost_scope();
        loop {
            match self.unit(&mut run) {
                Ok(Flow::More) => {}
                Ok(Flow::Done) => {
                    self.observe_dispatch(&run, None);
                    return Ok(std::mem::take(&mut run.outcomes));
                }
                Err(fault) => {
                    let error = fault.into_kernel();
                    self.observe_dispatch(&run, Some(error.to_string()));
                    return Err(error);
                }
            }
        }
    }

    /// Run units until the dispatch is done or one needs an answer the journal lacks.
    pub(crate) fn drive_resumable(
        &self,
        mut c: DispatchContinuation,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let _dispatch = c.run.span.clone().entered();
        let _cost = self.observed_cost_scope();
        loop {
            // A store ask the program seed made is answered in the program session too (EXEC-DIST-9).
            let seed_program = c.run.seed.as_ref().is_some_and(|seed| seed.program);
            let asker = if seed_program { self.seed_asker() } else { None };
            let _program = (c.run.program || seed_program).then(|| self.enter_program_session());
            let _asker = (c.run.program || seed_program).then(|| self.enter_asker(asker));
            let _seam = c.run.program.then(|| self.enter_store_seam());
            let _program_step = (c.run.program || c.run.seed.as_ref().is_some_and(|seed| seed.program))
                .then(crate::effects::executor::ProgramStepScope::enter);
            // A store ask the asker's seed made is answered in that viewer's session (EXEC-DIST-9).
            let _for_asker = c
                .run
                .seed
                .as_ref()
                .is_some_and(|seed| seed.for_asker)
                .then(|| c.run.asker_deliveries.as_ref().map(|held| self.enter_session(&held.asker)))
                .flatten();
            let seeds = c.seeds.get_or_insert_with(|| self.pending_store_seeds());
            if c.store.is_none() {
                if let Some((origin, ask)) = seeds.pop_front() {
                    c.store = self.seed_leg_for(origin, ask);
                    continue;
                }
                if c.children.is_empty() {
                    if let Err(error) = self.start_keyed_children(&mut c) {
                        self.observe_dispatch(&c.run, Some(error.to_string()));
                        return Err(error);
                    }
                }
                if !c.children.is_empty() {
                    let ready = match c.children.iter().position(DispatchContinuation::reply_ready) {
                        Some(ready) => ready,
                        None if !c.child_waited => {
                            c.child_waited = true;
                            return Ok(Progress::Await { continuation: c, requests: Vec::new() });
                        }
                        None => 0,
                    };
                    c.child_waited = false;
                    let child = c.children.remove(ready).expect("a child at the ready index");
                    match self.drive_resumable(child) {
                        Err(error) => {
                            self.observe_dispatch(&c.run, Some(error.to_string()));
                            return Err(error);
                        }
                        Ok(Progress::Done(outcomes)) => c.run.outcomes.extend(outcomes),
                        Ok(Progress::Await { continuation, requests }) if requests.is_empty() && continuation.parked_on_reply().is_some() => {
                            c.children.push_back(continuation)
                        }
                        Ok(Progress::Await { continuation, requests }) => {
                            c.pending = requests.clone();
                            c.children.push_front(continuation);
                            return Ok(Progress::Await { continuation: c, requests });
                        }
                    }
                    continue;
                }
            }
            if let Some(leg) = c.store.take() {
                let StoreLeg { ask, driver, seed, wait } = *leg;
                if let Some(key) = wait.shared_key() {
                    if !wait.ready() {
                        if wait.park_follower() {
                            c.store = Some(Box::new(StoreLeg { ask, driver, seed, wait }));
                            return Ok(Progress::Await { continuation: c, requests: Vec::new() });
                        }
                        c.store = self.own_store_leg(ask, &mut c.journal);
                        continue;
                    }
                    let shared = StoreAsk { key: key.to_string(), ..ask.clone() };
                    let answer = self.store_reply(&shared, &wait.take_reply().unwrap_or_default());
                    c.journal.push(Exchange::new(ask.request, answer));
                    continue;
                }
                let outcomes = match driver {
                    None => wait.take_reply().unwrap_or_default(),
                    Some(driver) => {
                        let driven = {
                            let _leg = wait.enter();
                            self.drive_resumable(driver)
                        };
                        match driven {
                            Err(error) => {
                                self.observe_dispatch(&c.run, Some(error.to_string()));
                                return Err(error);
                            }
                            Ok(Progress::Await { continuation, requests }) => {
                                c.pending = requests.clone();
                                c.store = Some(Box::new(StoreLeg { ask, driver: Some(continuation), seed, wait }));
                                return Ok(Progress::Await { continuation: c, requests });
                            }
                            Ok(Progress::Done(outcomes)) => {
                                if wait.has_open_work() && !super::store_park::carries_reply(&ask.key, &outcomes) {
                                    c.store = Some(Box::new(StoreLeg { ask, driver: None, seed, wait }));
                                    return Ok(Progress::Await { continuation: c, requests: Vec::new() });
                                }
                                outcomes
                            }
                        }
                    }
                };
                wait.landed(&outcomes);
                let answer = self.store_reply(&ask, &outcomes);
                if let Some(origin) = seed {
                    self.seed_landed(&ask, &answer, &origin);
                    continue;
                }
                c.journal.push(Exchange::new(ask.request, answer));
            }
            *self.suspended_commit.write() = None;
            // The step committed its transition when it suspended: it replays from the state it left, and a state
            // another dispatch moved the trait to meanwhile stands once the replay is done.
            let replay = c.run.provisional.take().map(|p| {
                let moved = self.rewind_provisional(&p);
                (p, moved)
            });
            let journal = std::mem::take(&mut c.journal);
            #[cfg(feature = "wire-drivers")]
            self.tls_begin_pass();
            let (result, end) =
                self.io
                    .run_step_reading(journal, std::mem::take(&mut c.reads), || {
                        self.unit(&mut c.run)
                    });
            #[cfg(feature = "wire-drivers")]
            self.tls_end_pass(end.pending.is_empty() && end.store_asks.is_empty() && end.diverged.is_none());
            let committed = self.suspended_commit.write().take();
            let suspended = !end.pending.is_empty() || !end.store_asks.is_empty();
            match replay {
                Some((p, Some(moved))) => {
                    self.set_held_state(&p.trait_name, &p.entity_id, moved);
                    if suspended {
                        c.run.provisional = Some(committed.map(Box::new).unwrap_or(p));
                    }
                }
                Some((p, None)) if suspended => match committed {
                    Some(again) => c.run.provisional = Some(Box::new(again)),
                    None => {
                        // Suspended before reaching its transition again: hold the state it had entered.
                        let mut entered = p.from.clone();
                        entered.transition_to(&p.to);
                        self.set_held_state(&p.trait_name, &p.entity_id, entered);
                        c.run.provisional = Some(p);
                    }
                },
                _ if suspended => c.run.provisional = committed.map(Box::new),
                _ => {}
            }
            c.reads = end.reads;
            if let Some(diverged) = end.diverged {
                self.observe_dispatch(&c.run, Some(diverged.to_string()));
                return Err(DispatchError::Resume(diverged));
            }
            if suspended {
                if result.is_ok() {
                    let error = DispatchError::Kernel(KernelError::Internal(
                        "a dispatch step committed while it awaited host I/O".to_string(),
                    ));
                    self.observe_dispatch(&c.run, Some(error.to_string()));
                    return Err(error);
                }
                c.journal = end.journal;
                if let Some(ask) = end.store_asks.into_iter().next() {
                    let wait = self.shared_reads.wait_for(&ask);
                    if wait.shared_key().is_some() {
                        c.store = Some(Box::new(StoreLeg { ask, driver: None, seed: None, wait }));
                        continue;
                    }
                    match self.store_driver_run(&ask) {
                        Ok(driver) => {
                            c.store = Some(Box::new(StoreLeg {
                                ask,
                                driver: Some(driver),
                                seed: None,
                                wait,
                            }))
                        }
                        Err(message) => {
                            let answer = Self::store_failed(&ask, message);
                            c.journal.push(Exchange::new(ask.request, answer));
                        }
                    }
                    continue;
                }
                c.pending = number_requests(&mut c.next_id, end.pending);
                tracing::debug!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, asks = c.pending.len(), "parked on host asks");
                let requests = c.pending.clone();
                return Ok(Progress::Await {
                    continuation: c,
                    requests,
                });
            }
            match result {
                Ok(Flow::More) => {}
                Ok(Flow::Done) => {
                    self.observe_dispatch(&c.run, None);
                    super::store_park::offer_reply(&c.run.outcomes);
                    return Ok(Progress::Done(std::mem::take(&mut c.run.outcomes)));
                }
                Err(fault) => {
                    let error = fault.into_kernel();
                    self.observe_dispatch(&c.run, Some(error.to_string()));
                    return Err(DispatchError::Kernel(error));
                }
            }
        }
    }

    /// EXEC-CONC-1a: a route mount's `$MOUNT` deliveries to two or more traits become one run per trait, in delivery
    /// order, settled before the mount's own seeds begin (EXEC-SEED-7).
    fn start_mount_children(&self, c: &mut DispatchContinuation) {
        let run = &mut c.run;
        if run.seed.is_some() || run.provisional.is_some() {
            return;
        }
        let Some(preamble) = run.preamble.as_ref() else { return };
        let mut traits: Vec<&str> = preamble.deliveries.iter().map(|item| item.trait_name.as_str()).collect();
        traits.sort_unstable();
        traits.dedup();
        tracing::debug!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, traits = traits.len(), deliveries = preamble.deliveries.len(), "mount children");
        if traits.len() < 2 {
            return;
        }
        let Preamble { row, deliveries } = *run.preamble.take().expect("a preamble");
        let mut groups: Vec<(String, VecDeque<CascadeItem>)> = Vec::new();
        for item in deliveries {
            match groups.iter_mut().find(|(t, _)| *t == item.trait_name) {
                Some((_, items)) => items.push_back(item),
                None => groups.push((item.trait_name.clone(), VecDeque::from([item]))),
            }
        }
        let locale = run.locale.clone();
        for (_, deliveries) in groups {
            let mut child_run = self.run_of(Vec::new());
            child_run.locale = locale.clone();
            child_run.preamble = Some(Box::new(Preamble { row: row.clone(), deliveries }));
            c.children.push_back(Self::continuation_of(child_run));
        }
    }

    /// EXEC-CONC-1a: a mount's seeds on two or more traits (and no `$MOUNT` deliveries before them) become one run per
    /// trait, in seed order; a tick plan keeps the run whole.
    fn start_seed_children(&self, c: &mut DispatchContinuation) {
        let run = &mut c.run;
        if run.seed.is_some() || run.provisional.is_some() || run.preamble.is_some() || run.plans.len() < 2 {
            return;
        }
        if run.plans.iter().any(|plan| plan.tick.is_some()) {
            return;
        }
        let mut traits: Vec<&str> = run.plans.iter().map(|plan| plan.trait_name.as_str()).collect();
        traits.sort_unstable();
        traits.dedup();
        tracing::debug!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, traits = traits.len(), seeds = run.plans.len(), "seed children");
        if traits.len() < 2 {
            return;
        }
        let mut groups: Vec<(String, Vec<SeedPlan>)> = Vec::new();
        for plan in std::mem::take(&mut run.plans) {
            match groups.iter_mut().find(|(t, _)| *t == plan.trait_name) {
                Some((_, plans)) => plans.push(plan),
                None => groups.push((plan.trait_name.clone(), vec![plan])),
            }
        }
        let locale = run.locale.clone();
        for (_, plans) in groups {
            let mut child_run = self.run_of(plans);
            child_run.locale = locale.clone();
            c.children.push_back(Self::continuation_of(child_run));
        }
    }

    /// EXEC-CONC-1a: once a viewer run has only its program deliveries left, the deliveries to two or more distinct keyed
    /// rows each become a run of their own, grouped by row in delivery order. Each is driven until its first wait, so
    /// their store reads are in flight together, and they finish in that order before the rest steps. A row key that
    /// cannot be resolved here leaves the deliveries in the one run.
    fn start_keyed_children(&self, c: &mut DispatchContinuation) -> Result<(), DispatchError> {
        self.start_mount_children(c);
        if c.children.is_empty() {
            self.start_seed_children(c);
        }
        if !c.children.is_empty() {
            return Ok(());
        }
        let run = &mut c.run;
        if run.seed.is_some() || !run.plans.is_empty() || run.preamble.is_some() || run.provisional.is_some() {
            return Ok(());
        }
        if run.program_deliveries.iter().filter(|item| item.listen_key.is_some()).take(2).count() < 2 {
            return Ok(());
        }
        let boot = self.registry.boot_traits.read().clone();
        let mut rows: Vec<Option<(String, String)>> = Vec::with_capacity(run.program_deliveries.len());
        for item in run.program_deliveries.iter() {
            let row = match item.listen_key.as_ref().filter(|_| boot.contains(&item.trait_name)) {
                None => None,
                Some(key) => {
                    let reads_user = key.expr.any_string(|s| s == "@user" || s.starts_with("@user."));
                    let user = if reads_user {
                        match self.session_user() {
                            Ok(user) => user,
                            Err(_) => {
                                tracing::debug!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, trait_name = %item.trait_name, "keyed children: the session user is not held, one run");
                                return Ok(());
                            }
                        }
                    } else {
                        Value::Null
                    };
                    match self.resolve_listen_key(key, &item.source_payload, user) {
                        Ok(row) => Some(row),
                        Err(reason) => {
                            tracing::debug!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, trait_name = %item.trait_name, %reason, "keyed children: a row key did not resolve, one run");
                            return Ok(());
                        }
                    }
                }
            };
            rows.push(row.map(|row| (item.trait_name.clone(), row)));
        }
        let mut distinct: Vec<&(String, String)> = rows.iter().flatten().collect();
        distinct.sort();
        distinct.dedup();
        tracing::debug!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, rows = distinct.len(), deliveries = rows.len(), "keyed children");
        if distinct.len() < 2 {
            return Ok(());
        }
        let mut groups: Vec<((String, String), VecDeque<CascadeItem>)> = Vec::new();
        let mut rest = VecDeque::new();
        for (item, row) in std::mem::take(&mut run.program_deliveries).into_iter().zip(rows) {
            match row {
                None => rest.push_back(item),
                Some(id) => match groups.iter_mut().find(|(g, _)| *g == id) {
                    Some((_, items)) => items.push_back(item),
                    None => groups.push((id, VecDeque::from([item]))),
                },
            }
        }
        run.program_deliveries = rest;
        let locale = run.locale.clone();
        for (_, items) in groups {
            let mut child_run = self.run_of(Vec::new());
            child_run.locale = locale.clone();
            child_run.program_deliveries = items;
            c.children.push_back(Self::continuation_of(child_run));
        }
        Ok(())
    }

    /// Every registered seed not yet landed, each under a fresh ask key: a run writes them before its first step,
    /// even while another run's leg for one awaits the host (seeds are creates; `duplicate-id` is their no-op).
    /// A failed ensure is not registered any more: its entity's writes answer the recorded failure until the program registers again.
    pub(crate) fn pending_store_seeds(&self) -> std::collections::VecDeque<(String, StoreAsk)> {
        self.store_seeds
            .lock()
            .iter()
            .map(|ask| {
                (
                    ask.key.clone(),
                    StoreAsk {
                        key: self.io.next_store_key(),
                        ..ask.clone()
                    },
                )
            })
            .collect()
    }

    pub(crate) fn seed_leg_for(&self, origin: String, ask: StoreAsk) -> Option<Box<StoreLeg>> {
        match self.store_driver_run(&ask) {
            Ok(driver) => {
                let wait = super::store_park::LegWait::new(&ask.key);
                Some(Box::new(StoreLeg { ask, driver: Some(driver), seed: Some(origin), wait }))
            }
            Err(message) => {
                tracing::warn!(driver = %ask.driver, %message, "kernel: store seed not sent");
                if let Some(ensure) = &ask.ensure {
                    self.io.set_index_failure(
                        &ensure.entity_type,
                        Some(format!("store driver {} could not ensure the indexes of {}: {message}", ask.driver, ensure.entity_type)),
                    );
                    self.store_seeds.lock().retain(|registered| registered.key != origin);
                }
                None
            }
        }
    }

    pub(crate) fn seed_landed(&self, ask: &StoreAsk, answer: &IoAnswer, origin: &str) {
        if let Some(ensure) = &ask.ensure {
            match answer {
                IoAnswer::Wrote(Err(message)) => {
                    tracing::warn!(driver = %ask.driver, entity = %ensure.entity_type, %message, "kernel: store indexes not ensured");
                    self.io.set_index_failure(
                        &ensure.entity_type,
                        Some(format!(
                            "store driver {} could not ensure the indexes of {}: {message}",
                            ask.driver, ensure.entity_type
                        )),
                    );
                }
                _ => self.io.set_index_failure(&ensure.entity_type, None),
            }
        } else {
            Self::report_seed(ask, answer);
        }
        self.store_seeds
            .lock()
            .retain(|registered| registered.key != origin);
    }

    /// A follower whose shared read has not replied runs its own driver (an in-place dispatch cannot wait for it).
    fn own_store_leg(&self, ask: StoreAsk, journal: &mut Vec<Exchange>) -> Option<Box<StoreLeg>> {
        match self.store_driver_run(&ask) {
            Ok(driver) => {
                let wait = super::store_park::LegWait::new(&ask.key);
                Some(Box::new(StoreLeg { ask, driver: Some(driver), seed: None, wait }))
            }
            Err(message) => {
                let answer = Self::store_failed(&ask, message);
                journal.push(Exchange::new(ask.request, answer));
                None
            }
        }
    }

    /// The driver orbital's dispatch of the verb `ask` maps to, in a frame of its own keyed by the ask.
    pub(crate) fn store_driver_run(&self, ask: &StoreAsk) -> Result<DispatchContinuation, String> {
        let verb = match &ask.ensure {
            Some(ensure) => crate::store_driver::ensure_verb(
                &ensure.entity_type,
                &ensure.indexes,
                &ask.key,
                self.io.collection_of(&ensure.entity_type),
            ),
            None => crate::store_driver::verb_for(&ask.request, &ask.key, &|entity| self.io.collection_of(entity), &|entity| self.io.index_paths_of(entity)).ok_or_else(|| {
                format!(
                    "store driver {}: {:?} is not a row ask a driver answers",
                    ask.driver, ask.request
                )
            })?,
        };
        let candidates: Vec<String> = {
            let schemas = self.registry.schemas.read();
            self.registry.trait_homes
                .read()
                .iter()
                .filter(|(_, (schema, at))| {
                    schemas
                        .get(schema)
                        .and_then(|s| s.orbitals.get(*at))
                        .is_some_and(|o| o.name == ask.driver)
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        let handlers = self.discover_handlers(&candidates, Some(&ask.key), verb.event);
        let [target] = handlers.as_slice() else {
            return Err(format!(
                "store driver {} must have exactly one trait handling {}, has {:?}",
                ask.driver, verb.event, handlers
            ));
        };
        let mut driver = Self::continuation_of(self.run_of_dispatch(
            target,
            Some(&ask.key),
            verb.event,
            verb.payload,
            None,
            None,
        ));
        driver.seeds = Some(Default::default());
        driver.run.program = true;
        self.store_ask_frames.write().insert(ask.key.clone());
        Ok(driver)
    }

    /// A seed row the driver did not create is logged; a row already present (`duplicate-id`) is the seed's no-op.
    pub(super) fn report_seed(ask: &StoreAsk, answer: &IoAnswer) {
        match answer {
            IoAnswer::Wrote(Ok(outcomes)) => {
                for outcome in outcomes {
                    if !matches!(
                        outcome,
                        RowWriteOutcome::Created { .. } | RowWriteOutcome::DuplicateId
                    ) {
                        tracing::warn!(driver = %ask.driver, key = %ask.key, ?outcome, "kernel: store seed row not created");
                    }
                }
            }
            other => {
                tracing::warn!(driver = %ask.driver, key = %ask.key, answer = ?other, "kernel: store seed failed")
            }
        }
    }

    pub(crate) fn store_failed(ask: &StoreAsk, message: String) -> IoAnswer {
        crate::store_driver::failed_answer(&ask.request, message).unwrap_or_else(|| {
            IoAnswer::Wrote(Err(format!("store driver {}: not a row ask", ask.driver)))
        })
    }

    /// The answer the driver's keyed reply gives `ask`: a driver that sent none, or a malformed one, failed it.
    pub(crate) fn store_reply(&self, ask: &StoreAsk, outcomes: &DispatchOutcomes) -> IoAnswer {
        use crate::store_driver::*;
        self.store_ask_frames.write().remove(&ask.key);
        self.retire_store_ask(&ask.key);
        if ask.ensure.is_some() {
            return Self::ensure_reply(ask, outcomes);
        }
        const REPLIES: [&str; 7] = [
            ROW_FOUND,
            ROWS_LISTED,
            ROWS_PAGE,
            ROWS_IDS,
            ROWS_WRITTEN,
            STORE_UNSUPPORTED,
            STORE_FAILED,
        ];
        let reply = outcomes
            .iter()
            .flat_map(|(_, r)| r.effect_results.iter())
            .find_map(|er| match &er.effect {
                Effect::Emit { event, payload }
                    if REPLIES.contains(&event.as_str())
                        && reply_key(payload) == Some(ask.key.as_str()) =>
                {
                    Some((event.as_str(), payload))
                }
                _ => None,
            });
        let failed = |message: String| Self::store_failed(ask, message);
        match reply {
            Some((event, payload)) => match answer_for(&ask.request, event, payload) {
                Ok(answer) => self.checked_answer(&ask.request, answer),
                Err(e) => failed(format!("store driver {}: {e}", ask.driver)),
            },
            None => failed(format!(
                "store driver {} sent no reply for ask {}",
                ask.driver, ask.key
            )),
        }
    }

    /// `Wrote(Ok)` when the driver confirmed the ensure, else `Wrote(Err)` with its message.
    pub(super) fn ensure_reply(ask: &StoreAsk, outcomes: &DispatchOutcomes) -> IoAnswer {
        use crate::store_driver::*;
        let reply = outcomes
            .iter()
            .flat_map(|(_, r)| r.effect_results.iter())
            .find_map(|er| match &er.effect {
                Effect::Emit { event, payload }
                    if [ROWS_INDEXES_ENSURED, STORE_FAILED].contains(&event.as_str())
                        && reply_key(payload) == Some(ask.key.as_str()) =>
                {
                    Some((event.as_str(), payload))
                }
                _ => None,
            });
        match reply {
            Some((ROWS_INDEXES_ENSURED, _)) => IoAnswer::Wrote(Ok(Vec::new())),
            Some((_, payload)) => IoAnswer::Wrote(Err(payload
                .as_object()
                .and_then(|o| o.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("the driver failed without a message")
                .to_string())),
            None => IoAnswer::Wrote(Err(format!("the driver sent no reply for ask {}", ask.key))),
        }
    }
}
