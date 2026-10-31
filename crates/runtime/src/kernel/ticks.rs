//! Ticks: finding a trait's tick and running it in a turn or a dispatch.

use super::*;

impl RuntimeKernel {
    /// The declared tick `tick_name` of `trait_name`.
    pub fn tick_of(
        &self,
        trait_name: &str,
        tick_name: &str,
    ) -> Option<orbital_core_base::schema::types::TickDefinition> {
        self.registry.trait_definitions
            .read()
            .get(trait_name)?
            .ticks
            .as_ref()?
            .iter()
            .find(|tick| tick.name == tick_name)
            .cloned()
    }

    /// Run one scheduled tick of `trait_name` against `entity_id` through the **same**
    /// effect path as a transition (`execute_effects_with_entity`) — no parallel walker.
    /// Mirrors the canonical JS runtime's `executeTick`: build the entity/state context,
    /// evaluate the tick's guard (skip when it fails), run the tick's effects, and persist
    /// any `set` fields back to the entity. A tick is not an event/transition, so this is a
    /// sibling entry point to `process_event` that shares its effect execution + binding
    /// resolution, returning the same [`ProcessEventResult`] shape so the existing
    /// `EventResponse` conversion folds tick effects into the client response unchanged.
    pub fn process_tick(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
    ) -> Result<ProcessEventResult, KernelError> {
        self.process_tick_in_turn(None, trait_name, entity_id, tick_name)
    }

    pub(super) fn process_tick_in_turn(
        &self,
        turn: Option<LaneTurn>,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
    ) -> Result<ProcessEventResult, KernelError> {
        let _lane = self.lane.enter(turn)?;
        let entity_id = entity_id.unwrap_or(RUNTIME_SINGLETON);
        self.process_tick_in_dispatch(trait_name, entity_id, entity_id, tick_name)
            .map_err(Fault::into_kernel)
    }

    /// [`Self::process_tick`] inside a dispatch: the tick reads the row `entity_id` while the trait's
    /// state is held under `state_entity` (a server tick runs over every row against the one trait state).
    pub(super) fn process_tick_in_dispatch(
        &self,
        trait_name: &str,
        entity_id: &str,
        state_entity: &str,
        tick_name: &str,
    ) -> Result<ProcessEventResult, Fault> {
        let stamp = self.begin_dispatch();

        self.ensure_trait_loaded(trait_name)?;
        let trait_def = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .cloned()
            .ok_or_else(|| KernelError::TraitNotFound(trait_name.to_string()))?;

        let tick = trait_def
            .ticks
            .as_ref()
            .and_then(|ticks| ticks.iter().find(|t| t.name == tick_name))
            .ok_or_else(|| {
                KernelError::Internal(format!("tick '{tick_name}' not found on '{trait_name}'"))
            })?;

        let current_state = {
            let mut state_manager = self.state_manager.write();
            let state_key = self.state_key(Some(state_entity));
            state_manager
                .get_or_create_state(trait_name, state_key.as_deref())
                .map_err(|e| KernelError::Internal(e.to_string()))?
                .current_state
                .clone()
        };

        // `appliesTo` lists the trait states the tick runs in; none listed = every state.
        if tick
            .applies_to
            .as_ref()
            .is_some_and(|states| !states.is_empty() && !states.contains(&current_state))
        {
            return Ok(ProcessEventResult {
                event: None,
                executed: false,
                previous_state: current_state.clone(),
                new_state: current_state,
                effects_executed: 0,
                effect_results: Vec::new(),
                entity_data: SharedEntity::null(),
                linked_entity: trait_def
                    .linked_entity
                    .clone()
                    .unwrap_or_else(|| trait_name.to_string()),
                payload: null_payload(),
                entry: None,
                fired_arm: None,
                transition_index: None,
                entity_id: entity_id.to_string(),
                state_entity_id: state_entity.to_string(),
                guards: Vec::new(),
                effect_runs: Vec::new(),
                cascade_dropped: Vec::new(),
                cascade_cut: None,
                consumed_emits: Vec::new(),
                emit_source: BusEventSource::default(),
                failure: None,
                written: SharedEntity::null(),
                callsite_payload: None,
            });
        }

        // A tick has no payload (drift D-2).
        let tick_payload = Value::object(ValueMap::default());

        let user = self.session_user()?;
        let ctx = BindingContextBuilder::new()
            .payload(tick_payload)
            .state(Value::string(current_state.clone()))
            .user(user.clone())
            .now(stamp.now)
            .current_theme(Value::String(
                (self.resolve_current_theme_for_trait(trait_name).key).into(),
            ))
            .pages(self.presentation.nav_pages.read().clone());
        let mut ctx = self.with_viewer_i18n(ctx).build();
        ctx.set_prob_entropy(self.prob_entropy());
        ctx.set_sealed_outputs(self.sealed_outputs());
        ctx.set_config(bind_user_forwards(
            config_defaults_value(&trait_def, &self.registry.trait_definitions.read()),
            &user,
        ));

        let entity_type = trait_def.linked_entity.as_deref().unwrap_or(trait_name);
        let entity_data = self.read_trait_entity(trait_name, entity_type, entity_id);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }
        if !entity_data.is_null() {
            ctx.set_entity_shared(entity_data.clone());
        }

        // Guard gates the tick exactly like the JS runtime — evaluated through the one
        // canonical evaluator, never a re-implemented predicate.
        if let Some(guard) = &tick.guard {
            let evaluator = Evaluator::new();
            if !matches!(evaluator.evaluate_guard(guard, &ctx), Ok(true)) {
                return Ok(ProcessEventResult {
                    event: None,
                    executed: false,
                    previous_state: current_state.clone(),
                    new_state: current_state,
                    effects_executed: 0,
                    effect_results: Vec::new(),
                    entity_data,
                    linked_entity: entity_type.to_string(),
                    payload: null_payload(),
                    entry: None,
                    fired_arm: None,
                    transition_index: None,
                    entity_id: entity_id.to_string(),
                    state_entity_id: state_entity.to_string(),
                    guards: Vec::new(),
                    effect_runs: Vec::new(),
                    cascade_dropped: Vec::new(),
                    cascade_cut: None,
                    consumed_emits: Vec::new(),
                    emit_source: BusEventSource::default(),
                    failure: None,
                    written: SharedEntity::null(),
                    callsite_payload: None,
                });
            }
        }

        *self.step.step_site.write() = Some((trait_name.to_string(), false));
        let (effect_results, captured_entity_data, scopes) =
            self.execute_effects_with_entity(&tick.effects, &ctx);
        *self.step.step_site.write() = None;

        // Effective entity = base entity with the tick's `set` fields merged in.
        // Persist the MERGED entity (read-modify-write), not the partial — same
        // reasoning as process_event: a replace-write of only the tick's Set fields
        // would drop every untouched field on the next render.
        let captured_entity_data = SharedEntity::from_value(captured_entity_data);
        let written = captured_entity_data.clone();
        let effective_entity = entity_data.merged(&captured_entity_data);

        ctx.set_entity_shared(effective_entity.clone());
        let sources = render_sources(&effect_results, &scopes);
        let mut effect_results = effect_results
            .into_iter()
            .enumerate()
            .map(|(index, er)| resolve_effect_result(er, &scoped(&ctx, scope_at(&scopes, index))))
            .collect::<Vec<_>>();
        self.inject_relation_options(trait_name, &mut effect_results);
        let failure = self.deliver_remote_emits(trait_name, false, &mut effect_results);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }

        if captured_entity_data.fields().is_some_and(|o| !o.is_empty()) {
            self.write_trait_frame(trait_name, entity_type, entity_id, &captured_entity_data);
        }

        self.record_renders(trait_name, entity_id, sources, &effect_results, &ctx);

        Ok(ProcessEventResult {
            event: None,
            executed: true,
            previous_state: current_state.clone(),
            new_state: current_state,
            effects_executed: effect_results.len(),
            effect_results,
            entity_data: effective_entity,
            linked_entity: entity_type.to_string(),
            payload: null_payload(),
            entry: None,
            fired_arm: None,
            transition_index: None,
            entity_id: entity_id.to_string(),
            state_entity_id: state_entity.to_string(),
            guards: Vec::new(),
            effect_runs: Vec::new(),
            cascade_dropped: Vec::new(),
            cascade_cut: None,
            consumed_emits: Vec::new(),
            emit_source: {
                let mut source = self.emit_source(trait_name);
                source.tick = Some(tick_name.to_string());
                source
            },
            failure,
            written,
            callsite_payload: None,
        })
    }

    /// The tick twin of [`Self::dispatch_with_server_leg`]. Runs the tick's effects through
    /// [`Self::process_tick`], then takes each `emit` it ran through the normal cascade (its
    /// declared listeners, and the emitter's own arm for the event when it has one) WITHOUT
    /// running the emitter as a seed, so its last render is not repainted and no `guardFailed`
    /// is reported for an event it never received. Each emit yields one [`ClientDispatch`], with
    /// the tick's name stamped on the event's source and on the leg (`EventRequest.tick`).
    ///
    /// Web parity (`useClientTicks` -> `runCircuitEffects`): a tick's OWN `persist`/`fetch`/
    /// `call-service` is a documented no-op there (`persistDelegated`, `callServiceDelegated`;
    /// no event exists to replay it under), so it is discarded here too and must not ride the
    /// next dispatch's leg. Server work done by the emit's listeners does produce a leg, as a
    /// listener's dispatch does on web.
    pub fn dispatch_tick(
        &self,
        orbital: &str,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
        client_id: Option<&str>,
    ) -> Result<TickDispatch, KernelError> {
        let _lane = self.lane.enter(None)?;
        let mut dispatched =
            self.dispatch_tick_unrepainted(orbital, trait_name, entity_id, tick_name, client_id)?;
        if dispatched.outcome.executed {
            dispatched.rerenders = self.rerender_stale();
        }
        Ok(dispatched)
    }

    /// [`Self::dispatch_tick`] without the repaint pass: `rerenders` is empty and the caller owns the
    /// [`Self::rerender_stale`] call. A host frame running many ticks calls it once after the last
    /// one, so the render store is scanned once per frame instead of once per tick.
    pub fn dispatch_tick_unrepainted(
        &self,
        orbital: &str,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
        client_id: Option<&str>,
    ) -> Result<TickDispatch, KernelError> {
        let _lane = self.lane.enter(None)?;
        let mut dispatched = self.dispatch_tick_own(trait_name, entity_id, tick_name, client_id)?;
        dispatched.emits = self.dispatch_tick_emits(orbital, trait_name, entity_id, tick_name, client_id, &dispatched.outcome)?;
        Ok(dispatched)
    }

    /// The tick's own effects only: no emit of it has run through the cascade yet ([`Self::dispatch_tick_emits`]).
    /// A host frame seals each tick's own writes against the view record before that tick's cascade reads them,
    /// keeping the server-role order: one tick's own effects, then its cascade, then the next tick (K5).
    pub fn dispatch_tick_own(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
        client_id: Option<&str>,
    ) -> Result<TickDispatch, KernelError> {
        let lane = self.lane.enter(None)?;
        let eid = entity_id.unwrap_or(RUNTIME_SINGLETON).to_string();
        self.ensure_trait_loaded(trait_name)?;
        if !self.registry.trait_definitions.read().contains_key(trait_name) {
            return Err(KernelError::TraitNotFound(trait_name.to_string()));
        }
        // The leg replays the tick from the pre-tick row, so the store applies its writes once.
        let pre_tick_row = self.snapshot_trait(trait_name, Some(&eid)).entity;

        let outcome =
            self.process_tick_in_turn(Some(lane.turn()), trait_name, Some(&eid), tick_name)?;
        let (tick_own, _) = self.drain_server_leg_collector();
        // The tick's own delegated effects, by where their data lives: browser-stored data
        // leaves as one `runTick` leg; server-owned work in a browser-run tick is an
        // `orb validate` error (ORB_T_CLIENT_TICK_SERVER_EFFECT), so reaching here is a bug.
        let (browser_own, server_own): (Vec<Effect>, Vec<Effect>) = if tick_own.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let residence = orbital_core_base::runs_on::Residence::of_entities(
                self.registry.entity_definitions.read().values(),
            );
            tick_own
                .into_iter()
                .partition(|effect| browser_data_effect(effect, &residence))
        };
        let discarded_server_effects = server_own.len();
        if !server_own.is_empty() {
            tracing::error!(
                target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                trait_name = %trait_name, tick = %tick_name, discarded = server_own.len(),
                "browser-run tick reached the server — orb validate should have rejected it (ORB_T_CLIENT_TICK_SERVER_EFFECT)",
            );
        }
        let browser_leg = (!browser_own.is_empty()).then(|| EventRequest {
            event: orbital_core_base::bus_event::tick_event_key(tick_name),
            run_tick: Some(crate::effects::response::RunTick {
                trait_name: trait_name.to_string(),
                tick: tick_name.to_string(),
            }),
            tick: Some(tick_name.to_string()),
            entity_id: Some(eid.clone()),
            target_trait: Some(trait_name.to_string()),
            client_id: client_id.map(str::to_string),
            source_trait: Some(trait_name.to_string()),
            traits: Some(vec![TraitFrom {
                r#trait: trait_name.to_string(),
                from: outcome.previous_state.clone(),
                event: None,
            }]),
            entity_by_trait: (!pre_tick_row.is_null())
                .then(|| ValueMap::from_iter([(trait_name.into(), pre_tick_row.clone())])),
            ..Default::default()
        });
        if !outcome.executed {
            return Ok(TickDispatch {
                outcome,
                discarded_server_effects,
                browser_leg,
                emits: Vec::new(),
                rerenders: Vec::new(),
            });
        }

        Ok(TickDispatch {
            outcome,
            discarded_server_effects,
            browser_leg,
            emits: Vec::new(),
            rerenders: Vec::new(),
        })
    }

    /// Take each `emit` an executed tick ran (`outcome`, from [`Self::dispatch_tick_own`]) through the normal cascade.
    pub fn dispatch_tick_emits(
        &self,
        orbital: &str,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
        client_id: Option<&str>,
        outcome: &ProcessEventResult,
    ) -> Result<Vec<TickEmit>, KernelError> {
        if !outcome.executed || !outcome.effect_results.iter().any(|r| matches!(r.effect, Effect::Emit { .. })) {
            return Ok(Vec::new());
        }
        let _lane = self.lane.enter(None)?;
        let eid = entity_id.unwrap_or(RUNTIME_SINGLETON).to_string();
        let trait_def = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .cloned()
            .ok_or_else(|| KernelError::TraitNotFound(trait_name.to_string()))?;
        debug!(
            target: "almadar::kernel",
            orbital = %orbital, trait_name = %trait_name, tick = %tick_name,
            "dispatch_tick",
        );
        let mut emit_context: Option<(DispatchMode, BusEventSource)> = None;
        let mut emits = Vec::new();
        for er in &outcome.effect_results {
            let Effect::Emit {
                event: emitted,
                payload: ep,
            } = &er.effect
            else {
                continue;
            };
            let (mode, source) = emit_context.get_or_insert_with(|| {
                let mut source = self.emit_source(trait_name);
                source.tick = Some(tick_name.to_string());
                (self.dispatch_mode_of(&trait_def), source)
            });
            let mode = *mode;
            let source = &*source;
            let snapshot = matches!(mode, DispatchMode::RuntimeOptimistic)
                .then(|| self.snapshot_trait(trait_name, Some(&eid)));
            let seed_row = self.snapshot_trait(trait_name, Some(&eid)).entity;

            let _stamp = self.begin_dispatch();
            let mut own = Vec::new();
            let mut fanned = Vec::new();
            let handled = self.trait_handles(trait_name, &outcome.new_state, emitted);
            self.emit_followup(
                trait_name,
                source,
                handled,
                false,
                emitted,
                ep,
                &mut own,
                &mut fanned,
            );
            let queue: VecDeque<CascadeItem> = own.into_iter().chain(fanned).collect();
            let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
            self.run_cascade(
                &eid,
                queue,
                &mut DispatchVisits::default(),
                &mut DispatchMemory::default(),
                &mut outcomes,
            )?;
            let response = self.response_for(&outcomes, trait_name, emitted);
            let dispatch = self.finish_client_dispatch(LegSeed {
                trait_name,
                event: emitted,
                payload: ep.clone(),
                entity_id,
                client_id,
                tick: Some(tick_name),
                mode,
                snapshot,
                seed_row: &seed_row,
                outcomes: &outcomes,
                response,
            });
            emits.push(TickEmit {
                event: emitted.clone(),
                payload: ep.clone(),
                dispatch,
            });
        }
        Ok(emits)
    }
}
