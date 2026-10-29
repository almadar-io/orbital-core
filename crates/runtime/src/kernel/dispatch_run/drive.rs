//! Driving a run straight through or resumably, and the seeds a store driver answers.

use super::*;

impl RuntimeKernel {
    /// Run every unit with the host's handlers answering at the I/O point: no suspension, no replay.
    pub(super) fn run_straight_through(&self, mut run: DispatchRun) -> Result<DispatchOutcomes, KernelError> {
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
        loop {
            let seeds = c.seeds.get_or_insert_with(|| self.pending_store_seeds());
            if c.store.is_none() {
                if let Some((origin, ask)) = seeds.pop_front() {
                    c.store = self.seed_leg_for(origin, ask);
                    continue;
                }
            }
            if let Some(leg) = c.store.take() {
                let StoreLeg { ask, driver, seed } = *leg;
                let driven = match self.drive_resumable(driver) {
                    Ok(progress) => progress,
                    Err(error) => {
                        self.observe_dispatch(&c.run, Some(error.to_string()));
                        return Err(error);
                    }
                };
                match driven {
                    Progress::Await {
                        continuation,
                        requests,
                    } => {
                        c.pending = requests.clone();
                        c.store = Some(Box::new(StoreLeg {
                            ask,
                            driver: continuation,
                            seed,
                        }));
                        return Ok(Progress::Await {
                            continuation: c,
                            requests,
                        });
                    }
                    Progress::Done(outcomes) => {
                        let answer = self.store_reply(&ask, &outcomes);
                        if let Some(origin) = seed {
                            self.seed_landed(&ask, &answer, &origin);
                            continue;
                        }
                        c.journal.push(Exchange::new(ask.request, answer));
                    }
                }
            }
            *self.suspended_commit.write() = None;
            // The step committed its transition when it suspended: it replays from the state it left, and a state
            // another dispatch moved the trait to meanwhile stands once the replay is done.
            let replay = c.run.provisional.take().map(|p| {
                let moved = self.rewind_provisional(&p);
                (p, moved)
            });
            let journal = std::mem::take(&mut c.journal);
            let (result, end) =
                self.io
                    .run_step_reading(journal, std::mem::take(&mut c.reads), || {
                        self.unit(&mut c.run)
                    });
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
                    match self.store_driver_run(&ask) {
                        Ok(driver) => {
                            c.store = Some(Box::new(StoreLeg {
                                ask,
                                driver,
                                seed: None,
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

    /// Every registered seed not yet landed, each under a fresh ask key: a run writes them before its first step,
    /// even while another run's leg for one awaits the host (seeds are creates; `duplicate-id` is their no-op).
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
            Ok(driver) => Some(Box::new(StoreLeg {
                ask,
                driver,
                seed: Some(origin),
            })),
            Err(message) => {
                tracing::warn!(driver = %ask.driver, %message, "kernel: store seed not sent");
                if let Some(ensure) = &ask.ensure {
                    self.io.set_index_failure(
                        &ensure.entity_type,
                        Some(format!("store driver {} could not ensure the indexes of {}: {message}", ask.driver, ensure.entity_type)),
                    );
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
                    return;
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

    /// The driver orbital's dispatch of the verb `ask` maps to, in a frame of its own keyed by the ask.
    pub(crate) fn store_driver_run(&self, ask: &StoreAsk) -> Result<DispatchContinuation, String> {
        let verb = match &ask.ensure {
            Some(ensure) => crate::store_driver::ensure_verb(
                &ensure.entity_type,
                &ensure.indexes,
                &ask.key,
                self.io.collection_of(&ensure.entity_type),
            ),
            None => crate::store_driver::verb_for(&ask.request, &ask.key, &|entity| self.io.collection_of(entity)).ok_or_else(|| {
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
