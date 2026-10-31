//! One unit of a run: a seed's holds and ticks, a step, the cascade and the repaint.

use super::*;

impl RuntimeKernel {
    /// One unit of the run: begin the next seed, or advance the seed in flight by one delivery.
    pub(super) fn unit(&self, run: &mut DispatchRun) -> Result<Flow, Fault> {
        let DispatchRun {
            plans,
            seed,
            outcomes,
            locale,
            user,
            origin,
            ..
        } = run;
        let Some(in_flight) = seed.as_mut() else {
            if plans
                .front()
                .is_some_and(|plan| plan.tick.as_ref().is_some_and(|tick| tick.all_rows))
            {
                self.expand_tick_rows(plans)?;
                return Ok(Flow::More);
            }
            return match plans.pop_front() {
                Some(plan) => {
                    *seed = Some(self.begin_seed(plan));
                    Ok(Flow::More)
                }
                None => Ok(Flow::Done),
            };
        };
        let _stamp = self.hold_stamp(in_flight.now);
        let _locale = self.hold_locale(locale.as_deref());
        let _user = self.hold_user(user.as_deref());
        let committed_from = origin.emits;
        self.hold_origin(origin);
        let _deliver = in_flight.span.clone().entered();
        let stepped = self.step_seed(in_flight, outcomes);
        self.release_origin(origin, committed_from, stepped.is_ok());
        match stepped? {
            Flow::More => Ok(Flow::More),
            Flow::Done => {
                *seed = None;
                Ok(Flow::More)
            }
        }
    }

    /// The dispatch's one `now`: the outer stamp when one is held, else the clock.
    pub(super) fn hold_stamp(&self, now: f64) -> DispatchStamp {
        let mut slot = self.step.dispatch_now.write();
        match *slot {
            Some(held) => DispatchStamp {
                slot: None,
                now: held,
            },
            None => {
                *slot = Some(now);
                DispatchStamp {
                    slot: Some(Arc::clone(&self.step.dispatch_now)),
                    now,
                }
            }
        }
    }

    /// The run's viewer as the kernel's `@user` for one unit.
    pub(super) fn hold_user(&self, user: Option<&Value>) -> Option<UserHold> {
        let user = user?;
        let previous = std::mem::replace(&mut *self.sessions.current_user.write(), user.clone());
        Some(UserHold {
            slot: Arc::clone(&self.sessions.current_user),
            previous,
        })
    }

    /// The run's viewer locale, held for one unit unless the caller already holds one.
    pub(super) fn hold_locale(&self, locale: Option<&str>) -> Option<LocaleGuard> {
        let mut slot = self.step.dispatch_locale.write();
        match (locale, slot.is_none()) {
            (Some(locale), true) => {
                *slot = Some(locale.to_string());
                Some(LocaleGuard {
                    slot: Arc::clone(&self.step.dispatch_locale),
                })
            }
            _ => None,
        }
    }

    /// Replace the front tick plan that runs over every row by one plan per row of its trait's entity.
    pub(super) fn expand_tick_rows(&self, plans: &mut VecDeque<SeedPlan>) -> Result<(), Fault> {
        let Some(front) = plans.front() else {
            return Ok(());
        };
        let entity_type = self
            .registry.trait_definitions
            .read()
            .get(&front.trait_name)
            .and_then(|def| def.linked_entity.clone())
            .unwrap_or_else(|| front.trait_name.clone());
        let singleton_entity = self
            .registry.entity_definitions
            .read()
            .get(&entity_type)
            .is_some_and(|def| {
                matches!(
                    def.persistence,
                    Some(orbital_core_base::schema::Persistence::Runtime)
                )
            });
        let listed = self.entity_source.list(&entity_type);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }
        let rows = listed.map_err(KernelError::Internal)?;
        let Some(plan) = plans.pop_front() else {
            return Ok(());
        };
        let ids: Vec<String> = rows
            .iter()
            .filter_map(|row| {
                row.get_path("id")
                    .and_then(|id| id.as_str().map(str::to_string))
            })
            .collect();
        // A `[runtime]` entity with no rows ticks once on the `singleton` frame (EXEC-TICK-5).
        if ids.is_empty() && singleton_entity {
            plans.push_front(SeedPlan {
                tick: plan.tick.clone().map(|tick| TickSeed {
                    all_rows: false,
                    ..tick
                }),
                ..plan
            });
            return Ok(());
        }
        for id in ids.into_iter().rev() {
            plans.push_front(SeedPlan {
                entity_id: Some(id),
                trait_name: plan.trait_name.clone(),
                event_name: plan.event_name.clone(),
                payload: plan.payload.clone(),
                delivery: plan.delivery.clone(),
                log: plan.log.clone(),
                lifecycle_targets: plan.lifecycle_targets.clone(),
                tick: plan.tick.clone().map(|tick| TickSeed {
                    all_rows: false,
                    ..tick
                }),
            });
        }
        Ok(())
    }

    pub(super) fn begin_seed(&self, plan: SeedPlan) -> SeedRun {
        let now = self.step.dispatch_now.read().unwrap_or_else(|| self.clock_now());
        let SeedPlan {
            trait_name,
            entity_id,
            event_name,
            payload,
            delivery,
            log,
            lifecycle_targets,
            tick,
        } = plan;
        let eid = entity_id.unwrap_or_else(|| crate::process_result::RUNTIME_SINGLETON.to_string());
        // A server leg carries the client's mount (`_awaitingInit`, never a
        // payload binding): deliveries held for a trait the client has since
        // initialized go out first, ahead of this request's seed.
        let (payload, awaiting) = take_awaiting_sidecar(payload);
        let released = match awaiting {
            Some(awaiting) => {
                let targets: Vec<&str> = lifecycle_targets.iter().map(String::as_str).collect();
                self.mount.write().sync(awaiting, &targets)
            }
            None => Vec::new(),
        };

        // Seed bubble (§158): the JS runtime fans a UI-originated emit to
        // every scope on the embed chain, innermost first (`TraitScopeProvider`
        // R-EMBEDDED-EMIT-BUBBLE) — a host with no transition for the event is
        // a tolerated no-op, same as `TraitNotFound` below, but it still serves
        // as a listener source. Only the SEED event bubbles this way; anything
        // this loop queues afterwards (cascaded listens/emits) keeps the
        // single-scope semantics `useEventBus.ts` reserves for machine-
        // originated emits. A trait with no ancestors runs this loop exactly
        // once, byte-identical to the pre-bubble single-scope dispatch.
        let mut seed_scopes = vec![trait_name.clone()];
        if tick.is_none() {
            seed_scopes.extend(
                self.registry.embed_index
                    .read()
                    .ancestors(&trait_name)
                    .iter()
                    .cloned(),
            );
        }
        let mut memory = DispatchMemory::default();
        if let Some(log) = &log {
            let keyed = self.is_keyed_row(&trait_name, &eid);
            memory.seed(&keyed_rows::memory_scope(&trait_name, &eid, keyed), log);
        }
        let seed_delivery = Arc::new(match &delivery {
            Some(d) => d.value(),
            None => delivery_record_value(&event_name, &payload, &BusEventSource::default()),
        });
        let stage = if released.is_empty() {
            Stage::Scopes {
                idx: 0,
                own: Vec::new(),
                fanned: Vec::new(),
                drops: VecDeque::new(),
            }
        } else {
            Stage::Released(Cascade::new(released.into()))
        };
        let span = tracing::trace_span!(
            target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
            "deliver",
            event = %event_name,
            entity = %eid,
        );
        SeedRun {
            eid,
            event_name,
            payload: Arc::new(payload),
            now,
            seed_scopes,
            seed_delivery,
            visited: DispatchVisits::default(),
            memory,
            stage,
            tick,
            span,
        }
    }

    pub(super) fn step_seed(
        &self,
        seed: &mut SeedRun,
        outcomes: &mut DispatchOutcomes,
    ) -> Result<Flow, Fault> {
        let SeedRun {
            eid,
            event_name,
            payload,
            seed_scopes,
            seed_delivery,
            visited,
            memory,
            stage,
            tick,
            ..
        } = seed;
        match stage {
            Stage::Released(cascade) => {
                if !self.cascade_unit(eid, cascade, visited, memory, outcomes)? {
                    *stage = Stage::Scopes {
                        idx: 0,
                        own: Vec::new(),
                        fanned: Vec::new(),
                        drops: VecDeque::new(),
                    };
                }
                Ok(Flow::More)
            }
            Stage::Scopes { idx, own, fanned, drops } => {
                if let Some(drop) = drops.front() {
                    self.run_row_drop(&[own.as_slice(), fanned.as_slice()], &VecDeque::new(), drop)?;
                    drops.pop_front();
                    return Ok(Flow::More);
                }
                let Some(scope) = seed_scopes.get(*idx) else {
                    let queue: VecDeque<CascadeItem> = std::mem::take(own)
                        .into_iter()
                        .chain(std::mem::take(fanned))
                        .collect();
                    *stage = Stage::Main(Cascade::new(queue));
                    return Ok(Flow::More);
                };
                let from = self.current_state_name(scope, Some(eid));
                let frame = self.step_frame(scope, eid);
                if self.io.suspended() {
                    return Err(Fault::Suspended);
                }
                let visit = visited.visit(scope, event_name, &from, payload, &VisitFrame::of(&frame));
                if visited.seen(&visit) {
                    debug!(trait_name = %scope, event = %event_name, from = %from, "seed:visited-skip");
                    *idx += 1;
                    return Ok(Flow::More);
                }
                let remembered = keyed_rows::memory_scope(scope, eid, self.is_keyed_row(scope, eid));
                let view = memory.view(&remembered, seed_delivery);
                let ran = match tick {
                    Some(tick) => {
                        self.process_tick_in_dispatch(scope, eid, &tick.state_entity, &tick.name)
                    }
                    None => self.process_event_in_dispatch(
                        scope,
                        Some(eid),
                        event_name,
                        Arc::clone(payload),
                        Some(&view),
                        false,
                    ),
                };
                let result = match ran {
                    Ok(r) => Some(r),
                    Err(Fault::Kernel(KernelError::TraitNotFound(_))) => None,
                    Err(fault) => return Err(fault),
                };
                if let Some(refusal) = self.emit_rejection(scope, result.as_ref()) {
                    return Err(Fault::Kernel(KernelError::PayloadInvalid(refusal)));
                }
                visited.record(visit);
                if result.is_some() {
                    fanned.extend(self.mount.write().release_after(scope, event_name));
                }
                let followups = self.step_followups(
                    scope,
                    event_name,
                    payload,
                    result.as_ref(),
                    seed_scopes,
                    tick.is_none(),
                );
                own.extend(followups.own);
                fanned.extend(followups.fanned);
                let mut result = result;
                if let Some(result) = result.as_mut() {
                    result.consumed_emits = followups.consumed_emits;
                }
                if let (Some(result), None) = (&result, tick.as_ref()) {
                    memory.record_delivery(&remembered, seed_delivery, view);
                    if result.executed {
                        memory.record_transition(&remembered, &result.previous_state, &result.new_state);
                    }
                }
                if let Some(result) = result {
                    outcomes.push((scope.clone(), result.released()));
                }
                let quiet = (tick.is_none() && self.is_keyed_row(scope, eid)).then_some((scope.as_str(), eid.as_str()));
                *drops = self.row_drops_after(quiet, eid);
                *idx += 1;
                Ok(Flow::More)
            }
            Stage::Main(cascade) => {
                if self.cascade_unit(eid, cascade, visited, memory, outcomes)? {
                    Ok(Flow::More)
                } else {
                    Ok(Flow::Done)
                }
            }
        }
    }

    /// Shared BFS body: drain `queue`, processing each `(trait, event, payload)`
    /// through `process_event` and fanning out its own emits + declared listeners.
    /// The single owner of cross-trait cascade fan-out — `dispatch` (seeded from the
    /// target trait's own bubble scopes) and `apply_server_response` (seeded from
    /// un-delivered server-emitted events) both build on this, so there is ONE BFS,
    /// not two independently-maintained copies that could drift apart.
    pub(in crate::kernel) fn run_cascade(
        &self,
        eid: &str,
        queue: VecDeque<CascadeItem>,
        visited: &mut DispatchVisits,
        memory: &mut DispatchMemory,
        outcomes: &mut Vec<(String, ProcessEventResult)>,
    ) -> Result<(), KernelError> {
        let mut cascade = Cascade::new(queue);
        loop {
            match self.cascade_unit(
                eid,
                &mut cascade,
                visited,
                memory,
                outcomes,
            ) {
                Ok(true) => {}
                Ok(false) => return Ok(()),
                Err(fault) => return Err(fault.into_kernel()),
            }
        }
    }

    /// Re-render one embedded child under the payload its composer gave it. The child's lifecycle arm runs
    /// only when it stays in its state and renders; otherwise the walk goes through it to its own capture
    /// children, which keep the payload they were last composed with. JS twin:
    /// `OrbitalServerRuntime.rerenderCallsiteCaptureChildren`.
    pub(super) fn repaint_unit(
        &self,
        eid: &str,
        cascade: &mut Cascade,
        item: CascadeItem,
        job: RepaintJob,
        outcomes: &mut DispatchOutcomes,
    ) -> Result<(), Fault> {
        let child = item.trait_name.clone();
        if job.visited.lock().contains(&child) {
            return Ok(());
        }
        let composed_with = if job.keep_composed {
            self.composed_callsite
                .read()
                .get(&child)
                .cloned()
                .unwrap_or_else(|| job.composed_with.clone())
        } else {
            job.composed_with.clone()
        };
        let def = self.registry.trait_definitions.read().get(&child).cloned();
        let lifecycle = def.as_ref().and_then(|def| {
            crate::callsite_capture::repaint_lifecycle_event(
                def,
                &self.current_state_name(&child, Some(eid)),
            )
        });
        let result = match lifecycle {
            Some(event) => match self.process_event_composed(
                &child,
                Some(eid),
                event,
                Arc::new(Value::object(ValueMap::default())),
                None,
                false,
                Some(&composed_with),
            ) {
                Ok(result) if result.executed => Some(result),
                Ok(_) => None,
                Err(Fault::Suspended) => {
                    cascade.queue.push_front(item);
                    return Err(Fault::Suspended);
                }
                Err(Fault::Kernel(KernelError::TraitNotFound(_))) => None,
                Err(fault) => return Err(fault),
            },
            None => None,
        };
        job.visited.lock().insert(child.clone());
        let (keep_composed, composed_by) = match (&result, &def) {
            (Some(result), Some(def)) => {
                self.composed_callsite
                    .write()
                    .insert(child.clone().into(), composed_with.clone());
                let authored: Vec<&SExpression> =
                    result.effect_runs.iter().map(|run| &run.effect).collect();
                (
                    job.keep_composed,
                    Some(crate::callsite_capture::embedded_by_effects(def, &authored)),
                )
            }
            _ => (true, None),
        };
        let grandchildren = self
            .capture_children
            .read()
            .get(&child)
            .cloned()
            .unwrap_or_default();
        for name in grandchildren.into_iter().rev().filter(|name| {
            composed_by
                .as_ref()
                .is_none_or(|composed| composed.contains(name))
        }) {
            cascade.queue.push_front(Self::repaint_item(
                name,
                RepaintJob {
                    composed_with: composed_with.clone(),
                    keep_composed,
                    visited: Arc::clone(&job.visited),
                },
            ));
        }
        if let Some(result) = result {
            outcomes.push((child, result.released()));
        }
        Ok(())
    }

    /// Deliver the next item of the worklist. `Ok(true)` when the worklist has more, `Ok(false)` once
    /// it is drained or cut at the cap. A delivery that needs I/O the journal lacks is put back at the
    /// front of the worklist and the unit returns `Fault::Suspended` with nothing committed.
    pub(super) fn cascade_unit(
        &self,
        eid: &str,
        cascade: &mut Cascade,
        visited: &mut DispatchVisits,
        memory: &mut DispatchMemory,
        outcomes: &mut Vec<(String, ProcessEventResult)>,
    ) -> Result<bool, Fault> {
        if let Some(drop) = cascade.drops.front() {
            self.run_row_drop(&[], &cascade.queue, drop)?;
            cascade.drops.pop_front();
            return Ok(true);
        }
        let Some(mut item) = cascade.queue.pop_front() else {
            return Ok(false);
        };
        cascade.settle_pass();
        if let Some(job) = item.repaint.clone() {
            self.repaint_unit(eid, cascade, item, job, outcomes)?;
            return Ok(true);
        }
        {
            let mut mount = self.mount.write();
            if mount.holds(&item) {
                debug!(trait_name = %item.trait_name, event = %item.event, "fanout:held-until-init");
                mount.hold(item);
                return Ok(true);
            }
        }
        if let Some(listen_key) = item.listen_key.clone() {
            match self.resolve_listen_key(&listen_key, &item.source_payload) {
                Err(reason) => {
                    let mut refused = Vec::new();
                    self.refuse_key(&item, &listen_key, &reason, &mut refused)?;
                    cascade.queue.extend(refused);
                    return Ok(true);
                }
                Ok(row) => {
                    let missing = self.keyed_instance_missing(&item.trait_name, &row);
                    if missing {
                        if let Err(fault) = self.create_keyed_row(&item.trait_name, &row) {
                            cascade.queue.push_front(item);
                            return Err(fault);
                        }
                    }
                    item.listen_key = None;
                    item.row = Some(row.clone());
                    if missing {
                        let init = CascadeItem {
                            trait_name: item.trait_name.clone(),
                            event: keyed_rows::ROW_INIT_EVENT.to_string(),
                            payload: null_payload(),
                            payload_digest: None,
                            delivery: null_payload(),
                            source_payload: null_payload(),
                            listen_guard: None,
                            fans_out: false,
                            repaint: None,
                            completion: false,
                            listen_key: None,
                            row: Some(row),
                        };
                        cascade.queue.push_front(item);
                        cascade.queue.push_front(init);
                        return Ok(true);
                    }
                }
            }
        }
        let outer_eid = eid;
        let queued_row = item.row.clone();
        let keyed = item.row.is_some() || self.is_keyed_row(&item.trait_name, eid);
        let eid = queued_row.as_deref().unwrap_or(eid);
        let from = self.current_state_name(&item.trait_name, Some(eid));
        let frame = self.step_frame(&item.trait_name, eid);
        if self.io.suspended() {
            cascade.queue.push_front(item);
            return Err(Fault::Suspended);
        }
        let visit = (!item.completion)
            .then(|| visited.visit_digested(&item.trait_name, &item.event, &from, item.payload_digest(), &VisitFrame::of(&frame)));
        if visit.as_ref().is_some_and(|v| visited.seen(v)) {
            debug!(trait_name = %item.trait_name, event = %item.event, from = %from, "fanout:visited-skip");
            return Ok(true);
        }
        let view = memory.view(&keyed_rows::memory_scope(&item.trait_name, eid, keyed), &item.delivery);
        if let Some(guard) = &item.listen_guard {
            if !listen_guard_passes(
                guard,
                &item.source_payload,
                &from,
                self.listener_config(&item.trait_name),
                &view,
            ) {
                debug!(trait_name = %item.trait_name, event = %item.event, "fanout:listen-guard-blocked");
                let quiet = keyed.then_some((item.trait_name.as_str(), eid));
                cascade.drops = self.row_drops_after(quiet, outer_eid);
                return Ok(true);
            }
        }
        let cap = if memory.delivered() >= CROSS_TRAIT_DISPATCH_CAP { CROSS_TRAIT_DISPATCH_CAP } else { CROSS_TRAIT_CASCADE_CAP };
        if cascade.steps >= CROSS_TRAIT_CASCADE_CAP || memory.delivered() >= CROSS_TRAIT_DISPATCH_CAP {
            let at_cap = match visit {
                Some(v) => v,
                None => visited.visit_digested(&item.trait_name, &item.event, &from, item.payload_digest(), &VisitFrame::of(&frame)),
            };
            visited.record(at_cap);
            let cut = crate::process_result::CascadeCut {
                cap,
                trait_name: item.trait_name.clone(),
                event: item.event.clone(),
            };
            let mut dropped: Vec<String> = Vec::new();
            for name in std::iter::once(item.trait_name).chain(
                std::mem::take(&mut cascade.queue)
                    .into_iter()
                    .map(|i| i.trait_name),
            ) {
                if !dropped.contains(&name) {
                    dropped.push(name);
                }
            }
            tracing::warn!(
                target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                cap,
                dropped = ?dropped,
                "cascade:truncated",
            );
            if let Some((_, last)) = outcomes.last_mut() {
                last.cascade_dropped = dropped;
                last.cascade_cut = Some(cut);
            }
            return Ok(false);
        }
        // A source trait may not exist as a processable trait (e.g. a render-only
        // layout that only emits) — skip its own processing but still fan out.
        let result = match self.process_event_in_dispatch(
            &item.trait_name,
            Some(eid),
            &item.event,
            Arc::clone(&item.payload),
            Some(&view),
            true,
        ) {
            Ok(r) => Some(r),
            Err(Fault::Kernel(KernelError::TraitNotFound(_))) => None,
            Err(Fault::Suspended) => {
                cascade.queue.push_front(item);
                return Err(Fault::Suspended);
            }
            Err(fault) => return Err(fault),
        };
        if let Some(refusal) = self.emit_rejection(&item.trait_name, result.as_ref()) {
            return Err(Fault::Kernel(KernelError::PayloadInvalid(refusal)));
        }
        cascade.steps += 1;
        memory.count_delivery();
        if let Some(v) = visit {
            visited.record(v);
        }
        let CascadeItem {
            trait_name: t,
            event: ev,
            payload: pl,
            delivery,
            fans_out,
            ..
        } = item;

        // Fan out from the trait that just handled this event (source = `t`): the dispatched
        // event itself (unless it is a listener's private trigger), plus any events it emitted.
        let StepFollowups {
            own,
            fanned,
            consumed_emits,
        } = self.step_followups(&t, &ev, &pl, result.as_ref(), &[], fans_out);
        let mut result = result;
        if let Some(result) = result.as_mut() {
            result.consumed_emits = consumed_emits;
        }
        if let Some(result) = &result {
            let remembered = keyed_rows::memory_scope(&t, eid, keyed);
            memory.record_delivery(&remembered, &delivery, view);
            if result.executed {
                memory.record_transition(&remembered, &result.previous_state, &result.new_state);
            }
        }
        if let Some(result) = result {
            cascade
                .queue
                .extend(self.mount.write().release_after(&t, &ev));
            outcomes.push((t.clone(), result.released()));
        }
        for mut item in own.into_iter().rev() {
            if keyed {
                item.row = Some(eid.to_string());
            }
            cascade.queue.push_front(item);
        }
        cascade.queue.extend(fanned);
        cascade.drops = self.row_drops_after(keyed.then_some((t.as_str(), eid)), outer_eid);
        Ok(true)
    }
}
