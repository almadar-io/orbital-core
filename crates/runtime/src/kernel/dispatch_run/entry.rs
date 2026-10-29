//! Entry points: starting, continuing and resuming a dispatch, and the run each one builds.

use super::*;

impl RuntimeKernel {
    /// Dispatch an event with full cross-trait listener fan-out, mirroring the canonical
    /// runtime's EventBus. The target trait processes the event; then every trait whose
    /// `listens` matches `(event, source-trait)` processes its triggered event, and any
    /// `emit` effect fans out the same way. Returns one outcome per processed trait, in
    /// dispatch order, so the caller merges their effects into a single response. A
    /// `(trait, event, from, payload, frame)` visited-set bounds cycles (`DispatchVisits`).
    ///
    /// This is the single dispatch entry point the client bridge and server route call —
    /// `process_event` is the single-trait primitive it builds on. Without this, a user
    /// event sent to one trait (e.g. a "New Item" button's `CREATE`) never reaches the
    /// modal trait that *listens* for it, so nothing happens (RS-G20).
    pub fn dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        self.dispatch_continuing(trait_name, entity_id, event_name, payload, None, None)
    }

    /// [`Self::dispatch`] for a request that continues a client-begun dispatch: `delivery` is the
    /// target's `@event` and `log` its `@prevEvents`/`@prevStates` so far (`EventRequest.delivery`
    /// / `.dispatch_log`). `None` for both is a direct dispatch.
    pub fn dispatch_continuing(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        self.dispatch_entered(
            None, trait_name, entity_id, event_name, payload, delivery, log,
        )
    }

    /// [`Self::dispatch`] on behalf of the dispatch holding `turn` (EXEC-CONC-0): it re-enters that dispatch while
    /// it runs and is refused while it is parked on an ask.
    pub fn dispatch_in_turn(
        &self,
        turn: super::LaneTurn,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        self.dispatch_entered(
            Some(turn),
            trait_name,
            entity_id,
            event_name,
            payload,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn dispatch_entered(
        &self,
        turn: Option<super::LaneTurn>,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        let _lane = self.lane.enter(turn)?;
        self.require_payload([(trait_name, event_name)], &payload)?;
        let run = self.run_of_dispatch(trait_name, entity_id, event_name, payload, delivery, log);
        self.run_straight_through(run)
    }

    /// The traits of `candidates` whose held state has an arm for `event`, in the order given: who a
    /// request that names no trait is delivered to. JS twin: `evaluateOrbitalEvent`'s discovery
    /// (`canHandleEvent`).
    pub fn discover_handlers(
        &self,
        candidates: &[String],
        entity_id: Option<&str>,
        event_name: &str,
    ) -> Vec<String> {
        let eid = entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON);
        candidates
            .iter()
            .filter(|t| self.trait_handles(t, &self.current_state_name(t, Some(eid)), event_name))
            .cloned()
            .collect()
    }

    /// The held state of each of `traits` (its initial one while untouched).
    pub fn held_states(
        &self,
        traits: &[String],
        entity_id: Option<&str>,
    ) -> HashMap<String, String> {
        let eid = entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON);
        traits
            .iter()
            .map(|t| (t.clone(), self.current_state_name(t, Some(eid))))
            .collect()
    }

    /// [`Self::dispatch`] to each of `traits` as its own seed, in order, with the shared `payload`.
    pub fn dispatch_each(
        &self,
        traits: &[String],
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        let _lane = self.lane.enter(None)?;
        self.require_payload(traits.iter().map(|t| (t.as_str(), event_name)), &payload)?;
        let run = self.run_of_each(traits, entity_id, event_name, payload, None, None);
        self.run_straight_through(run)
    }

    /// [`Self::dispatch_each`] as a resumable dispatch.
    pub fn start_dispatch_each(
        &self,
        traits: &[String],
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = self.lane.enter(None)?;
        self.require_payload(traits.iter().map(|t| (t.as_str(), event_name)), &payload)?;
        let run = self.run_of_each(traits, entity_id, event_name, payload, delivery, log);
        self.drive_entry(lane, Self::continuation_of(run))
    }

    /// A mount batch (`EventRequest.mount`): each seed runs its own lifecycle event, in
    /// order, with the shared `payload`. The client's mount (`_awaitingInit`) is synced
    /// once against every seed, so no seed's held delivery leaves before its own step.
    /// A seed naming no registered trait is skipped. JS twin: `evaluateOrbitalEvent`'s
    /// `request.mount` branch.
    pub fn dispatch_mount(
        &self,
        seeds: &[MountSeed],
        entity_id: Option<&str>,
        payload: Value,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        let _lane = self.lane.enter(None)?;
        self.require_payload(
            seeds.iter().map(|s| (s.r#trait.as_str(), s.event.as_str())),
            &payload,
        )?;
        let run = self.run_of_mount(seeds, entity_id, payload);
        self.run_straight_through(run)
    }

    /// [`Self::dispatch_continuing`] as a resumable dispatch: it runs until it needs I/O it has no
    /// answer for and returns [`Progress::Await`] with the requests, or [`Progress::Done`] with the outcomes.
    pub fn start_dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = self.lane.enter(None)?;
        self.require_payload([(trait_name, event_name)], &payload)?;
        let run = self.run_of_dispatch(trait_name, entity_id, event_name, payload, delivery, log);
        self.drive_entry(lane, Self::continuation_of(run))
    }

    /// [`Self::dispatch_mount`] as a resumable dispatch.
    pub fn start_dispatch_mount(
        &self,
        seeds: &[MountSeed],
        entity_id: Option<&str>,
        payload: Value,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = self.lane.enter(None)?;
        self.require_payload(
            seeds.iter().map(|s| (s.r#trait.as_str(), s.event.as_str())),
            &payload,
        )?;
        let run = self.run_of_mount(seeds, entity_id, payload);
        self.drive_entry(lane, Self::continuation_of(run))
    }

    /// Go on with a dispatch that returned [`Progress::Await`], given an answer to each of its requests.
    pub fn resume(
        &self,
        mut continuation: DispatchContinuation,
        replies: Vec<IoReply>,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = match continuation.lane.take() {
            Some(hold) => hold.resume(),
            None => self.lane.enter(None)?,
        };
        accept_into(&mut continuation, replies, &|request, answer| self.checked_answer(request, answer))?;
        self.drive_entry(lane, continuation)
    }

    /// Drive a dispatch entered on `lane`: a dispatch parked on an ask keeps the lane until it commits.
    pub(super) fn drive_entry(
        &self,
        lane: super::lane::LaneGuard,
        continuation: DispatchContinuation,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        match self.drive_resumable(continuation)? {
            Progress::Await {
                mut continuation,
                requests,
            } => {
                if Self::parked_on_keyed_calls_only(&continuation, &requests) {
                    let keys = requests
                        .iter()
                        .filter_map(|pending| match &pending.request {
                            IoRequest::CallService { key, .. } => key.clone(),
                            _ => None,
                        })
                        .collect();
                    continuation.keyed = Some(Box::new(self.keyed_parks.hold(keys)));
                    drop(lane);
                } else {
                    continuation.keyed = None;
                    let event = continuation.run.parked_event();
                    continuation.lane = lane.park(event);
                }
                Ok(Progress::Await {
                    continuation,
                    requests,
                })
            }
            done => Ok(done),
        }
    }

    /// A dispatch parked only on keyed, routed `call-service` asks has reached the declared async boundary: it
    /// commits and frees the lane, and its answer is handled as a dispatch queued behind the lane (EXEC-CONC-0). Any
    /// unkeyed ask, or a store leg in flight, keeps the lane until the dispatch commits (EXEC-CONC-1).
    pub(super) fn parked_on_keyed_calls_only(continuation: &DispatchContinuation, requests: &[PendingRequest]) -> bool {
        continuation.store.is_none()
            && !requests.is_empty()
            && requests.iter().all(|pending| match &pending.request {
                IoRequest::CallService { key: Some(_), site: Some(site), .. } => {
                    let routes = &site.emit;
                    routes.success.is_some() || routes.failure.is_some() || routes.cancelled.is_some() || routes.on_message.is_some()
                }
                _ => false,
            })
    }

    pub(super) fn continuation_of(run: DispatchRun) -> DispatchContinuation {
        DispatchContinuation {
            run,
            journal: Vec::new(),
            pending: Vec::new(),
            next_id: 0,
            reads: Default::default(),
            store: None,
            seeds: None,
            lane: None,
            keyed: None,
        }
    }

    pub(super) fn run_of_dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> DispatchRun {
        let lifecycle_targets = orbital_core_base::events::is_bus_excluded_event(event_name)
            .then(|| trait_name.to_string())
            .into_iter()
            .collect();
        self.run_of(vec![SeedPlan {
            trait_name: trait_name.to_string(),
            entity_id: entity_id.map(str::to_string),
            event_name: event_name.to_string(),
            payload,
            delivery: delivery.cloned(),
            log: log.cloned(),
            lifecycle_targets,
            tick: None,
        }])
    }

    /// One firing of `tick_name` on `trait_name` as a resumable dispatch. With no `entity_id` it runs over
    /// every row of the trait's entity (the kernel asks the host to list them); the trait's state is held
    /// under `entity_id`, else the singleton. JS twin: `OrbitalServerRuntime.executeTick`.
    pub fn start_dispatch_tick(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = self.lane.enter(None)?;
        self.ensure_trait_loaded(trait_name)?;
        let defs = self.registry.trait_definitions.read();
        let declared = defs.get(trait_name).ok_or_else(|| {
            DispatchError::Kernel(KernelError::TraitNotFound(trait_name.to_string()))
        })?;
        if !declared
            .ticks
            .iter()
            .flatten()
            .any(|tick| tick.name == tick_name)
        {
            return Err(DispatchError::Kernel(KernelError::Internal(format!(
                "tick '{tick_name}' not found on '{trait_name}'"
            ))));
        }
        drop(defs);
        let plan = SeedPlan {
            trait_name: trait_name.to_string(),
            entity_id: entity_id.map(str::to_string),
            event_name: orbital_core_base::bus_event::tick_event_key(tick_name),
            payload: Value::Null,
            delivery: None,
            log: None,
            lifecycle_targets: Vec::new(),
            tick: Some(TickSeed {
                name: tick_name.to_string(),
                state_entity: entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON).to_string(),
                all_rows: entity_id.is_none(),
            }),
        };
        let run = self.run_of(vec![plan]);
        self.drive_entry(lane, Self::continuation_of(run))
    }

    pub(super) fn run_of_each(
        &self,
        traits: &[String],
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> DispatchRun {
        let lifecycle_targets: Vec<String> =
            if orbital_core_base::events::is_bus_excluded_event(event_name) {
                traits.to_vec()
            } else {
                Vec::new()
            };
        let (payload, awaiting) = take_awaiting_sidecar(payload);
        let plans = traits
            .iter()
            .enumerate()
            .map(|(i, trait_name)| SeedPlan {
                trait_name: trait_name.clone(),
                entity_id: entity_id.map(str::to_string),
                event_name: event_name.to_string(),
                payload: match (&awaiting, i) {
                    (Some(list), 0) => with_awaiting_sidecar(payload.clone(), list),
                    _ => payload.clone(),
                },
                delivery: delivery.cloned(),
                log: log.cloned(),
                lifecycle_targets: lifecycle_targets.clone(),
                tick: None,
            })
            .collect();
        self.run_of(plans)
    }

    pub(super) fn run_of_mount(
        &self,
        seeds: &[MountSeed],
        entity_id: Option<&str>,
        payload: Value,
    ) -> DispatchRun {
        let targets: Vec<String> = seeds.iter().map(|s| s.r#trait.clone()).collect();
        let (payload, awaiting) = take_awaiting_sidecar(payload);
        let mut first = true;
        let mut plans = Vec::new();
        for seed in seeds {
            if !self.registry.trait_definitions.read().contains_key(&seed.r#trait) {
                continue;
            }
            // Only the first seed carries the client's mount, synced against all seeds.
            let seed_payload = match (&awaiting, first) {
                (Some(list), true) => with_awaiting_sidecar(payload.clone(), list),
                _ => payload.clone(),
            };
            first = false;
            plans.push(SeedPlan {
                trait_name: seed.r#trait.clone(),
                entity_id: entity_id.map(str::to_string),
                event_name: seed.event.as_str().to_string(),
                payload: seed_payload,
                delivery: None,
                log: None,
                lifecycle_targets: targets.clone(),
                tick: None,
            });
        }
        self.run_of(plans)
    }

    pub(super) fn run_of(&self, plans: Vec<SeedPlan>) -> DispatchRun {
        let span = match plans.first() {
            Some(first) => tracing::debug_span!(
                target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                "dispatch",
                event = %first.event_name,
                target_trait = %first.trait_name,
                targets = plans.len(),
            ),
            None => tracing::debug_span!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, "dispatch", targets = 0),
        };
        DispatchRun {
            trace_seeds: plans
                .iter()
                .map(|p| crate::dispatch_trace::TraceSeed {
                    trait_name: p.trait_name.clone(),
                    entity_id: p.entity_id.clone(),
                    event: p.event_name.clone(),
                })
                .collect(),
            plans: plans.into(),
            seed: None,
            outcomes: Vec::new(),
            locale: self.step.dispatch_locale.read().clone(),
            user: self.thread_viewer(),
            origin: self.next_dispatch_origin(),
            provisional: None,
            span,
        }
    }

    pub(super) fn observe_dispatch(&self, run: &DispatchRun, error: Option<String>) {
        let Some(observer) = self.dispatch_observer.read().clone() else {
            return;
        };
        if !(observer.enabled)() {
            return;
        }
        let defs = self.registry.trait_definitions.read();
        let outcomes = run
            .outcomes
            .iter()
            .map(|(name, result)| {
                let state_key = self.state_key(Some(&result.state_entity_id));
                let observed_state = self
                    .state_manager
                    .read()
                    .get_state(name, state_key.as_deref())
                    .map(|state| state.current_state.clone());
                let mut trace = crate::dispatch_trace::transition_of(name, result, observed_state);
                let source = self.emit_source(name);
                trace.trait_id = source.trait_id;
                trace.orbital_id = source.orbital_id;
                if let Some(def) = defs.get(name) {
                    trace.definition_fingerprint = Some(
                        orbital_core_base::schema::trait_execution_fingerprint(def)
                            .expect("registered trait definition serializes"),
                    );
                    if let Some(metadata) = def.source_declaration.as_ref() {
                        trace.source_trait_id = Some(metadata.trait_id.clone());
                        trace.source_declaration_fingerprint = Some(metadata.fingerprint.clone());
                    }
                }
                trace
            })
            .collect();
        drop(defs);
        (observer.completed)(crate::dispatch_trace::DispatchTraceRecord {
            dispatch_id: run.origin.seq,
            seeds: run.trace_seeds.clone(),
            outcomes,
            error,
        });
    }
}
