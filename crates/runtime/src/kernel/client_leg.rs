//! The client role: server legs and their residence, seeds, awaiting traits, and folding the server's response.

use super::*;

/// What a mount runs ([`RuntimeKernel::prepare_mount`]): the seeds whose lifecycle event runs here, and the carried rows
/// of the restored traits, which the mount's frames carry back as they were.
#[derive(Debug, Clone, Default)]
pub struct MountPlan {
    pub unseeded: Vec<crate::effects::MountSeed>,
    pub restored_rows: ValueMap,
}

impl RuntimeKernel {
    /// Dispatch through the SEED trait's declared [`DispatchMode`], collecting
    /// whatever server-only effects (`persist`/`fetch`/`call-service`) the
    /// `EffectExecutor` delegated instead of executing — the `RuntimeEnvironment::
    /// Client` path (`executor.rs` `do_persist`/`do_fetch`/`do_call_service`)
    /// routes those to `self.effect_executor`'s configured [`EffectDelegate`]
    /// (installed from `KernelConfig.delegate` at construction — see
    /// [`RuntimeKernel::new`]), which for every client role is the ONE
    /// [`ServerLegCollector`]. `dispatch_with_server_leg` drains it right after
    /// this call's `dispatch`, so effects from an unrelated concurrent dispatch
    /// on another thread must never land here — see the lock-order note on
    /// [`Self::snapshot_trait`]; this method takes no kernel lock itself besides
    /// what `dispatch`/`process_event` already take internally, so two threads
    /// calling `dispatch_with_server_leg` concurrently race on the SAME
    /// collector and may attribute each other's delegated effects to the wrong
    /// leg — acceptable for now (client-role kernels are driven from one
    /// thread; `kernel_concurrent_dispatch_and_apply_no_deadlock` only proves
    /// the LOCKS never deadlock under concurrency, not per-call attribution).
    ///
    /// `orbital` is carried through for tracing/attribution only (no field on
    /// the wire [`EventRequest`] needs it yet — multiple orbitals sharing one
    /// client kernel is future work).
    pub fn dispatch_with_server_leg(
        &self,
        orbital: &str,
        trait_name: &str,
        entity_id: Option<&str>,
        event: &str,
        payload: Value,
        client_id: Option<&str>,
    ) -> Result<ClientDispatch, KernelError> {
        let lane = self.lane.enter(None)?;
        let eid = entity_id.unwrap_or(RUNTIME_SINGLETON).to_string();

        let trait_def = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .cloned()
            .ok_or_else(|| KernelError::TraitNotFound(trait_name.to_string()))?;
        let mode = self.dispatch_mode_of(&trait_def);

        // RuntimeOptimistic commits locally before any server round trip, so the
        // snapshot must be taken BEFORE `dispatch` runs — it's what the caller
        // rolls back to on a disagreeing/erroring server response.
        let snapshot = if matches!(mode, DispatchMode::RuntimeOptimistic) {
            Some(self.snapshot_trait(trait_name, Some(&eid)))
        } else {
            None
        };
        // The leg replays the event from the pre-dispatch state, so it carries
        // the pre-dispatch row too.
        let seed_row = self.snapshot_trait(trait_name, Some(&eid)).entity;

        debug!(
            target: "almadar::kernel",
            orbital = %orbital, trait_name = %trait_name, event = %event, mode = ?mode,
            "dispatch_with_server_leg",
        );

        let outcomes =
            self.dispatch_in_turn(lane.turn(), trait_name, Some(&eid), event, payload.clone())?;
        let mut response = self.response_for(&outcomes, trait_name, event);
        response.fold_rerenders(&self.rerender_stale());

        Ok(self.finish_client_dispatch(LegSeed {
            trait_name,
            event,
            payload,
            entity_id,
            client_id,
            tick: None,
            mode,
            snapshot,
            seed_row: &seed_row,
            outcomes: &outcomes,
            response,
        }))
    }

    /// Drain the delegated server effects the cascade collected and, for a client-role kernel,
    /// wrap them in the leg that replays `seed.event` on the server. The one body behind
    /// [`Self::dispatch_with_server_leg`] and [`Self::dispatch_tick`].
    pub(super) fn finish_client_dispatch(&self, seed: LegSeed<'_>) -> ClientDispatch {
        let LegSeed {
            trait_name,
            event,
            payload,
            entity_id: raw_entity_id,
            client_id,
            tick,
            mode,
            snapshot,
            seed_row,
            outcomes,
            response,
        } = seed;
        let (drained, delegating_traits) = self.drain_server_leg_collector();
        let (residence, mixed) = self.leg_residence(&drained);
        let mut response = response;
        if mixed {
            response.success = false;
            response.error = Some(format!(
                "residence: one dispatch of {trait_name} on {event} reaches browser-stored data and server data; split it with an emit and a listen"
            ));
        }
        let hybrid = matches!(mode, DispatchMode::HybridClientOnly);
        // A local seed never needs the server for its OWN effects, but the
        // traits its cascade reached (a persistor, a refetching list) may.
        let downstream_needs_server = delegating_traits.iter().any(|t| t != trait_name);

        // The client's mount rides every leg so the host holds a seed
        // for a trait this client has not initialized yet.
        let awaiting = self.awaiting_init_value();
        let with_awaiting = |payload: Value| match payload {
            Value::Object(mut map) => {
                map.insert("_awaitingInit".into(), awaiting.clone());
                Some(Value::Object(map))
            }
            Value::Null => Some(Value::object(ValueMap::from_iter([(
                "_awaitingInit".into(),
                awaiting.clone(),
            )]))),
            other => Some(other),
        };
        let mut server_legs = if mixed {
            Vec::new()
        } else if self.effect_executor.environment().is_client() && !drained.is_empty() {
            if hybrid && !downstream_needs_server && !self.all_browser_data(&drained) {
                // A `local` trait's OWN effects should be client-only by construction;
                // a server-only effect the seed itself delegated is a validator bug
                // (§7 declares `local` legal only where nothing needs the server), not
                // something to silently ship as a leg. Report and drop. Data in the
                // browser store is the exception: the host runs that leg against the store.
                tracing::error!(
                    trait_name = %trait_name, event = %event, drained = drained.len(),
                    "HybridClientOnly trait delegated server-only effects — dropping the server leg",
                );
                Vec::new()
            } else {
                let traits: Vec<TraitFrom> = outcomes
                    .iter()
                    .filter(|(t, r)| r.executed || t == trait_name)
                    .map(|(t, r)| TraitFrom {
                        r#trait: t.clone(),
                        from: r.previous_state.clone(),
                        event: r.event.clone(),
                    })
                    .collect();
                let traits = (!traits.is_empty()).then_some(traits);
                if delegating_traits.contains(trait_name) {
                    // The seed's own arm needs the server: it replays from its
                    // pre-dispatch row and the server cascades the rest itself.
                    // A never-written shared frame replays from the view the step read (its defaults), as the
                    // stateful leg carries it, so the host's echo of those defaults is not taken for a write.
                    let replayed_from = if seed_row.is_null() && self.reads_shared_frame(trait_name)
                    {
                        outcomes
                            .iter()
                            .find(|(t, r)| t == trait_name && r.executed)
                            .and_then(|(_, r)| r.entry.as_ref())
                            .map(|entry| entry.entity_before.to_value())
                            .unwrap_or(Value::Null)
                    } else {
                        seed_row.clone()
                    };
                    let entity_by_trait = (!replayed_from.is_null())
                        .then(|| ValueMap::from_iter([(trait_name.into(), replayed_from)]));
                    vec![EventRequest {
                        event: event.to_string(),
                        tick: tick.map(str::to_string),
                        payload: with_awaiting(payload.clone()),
                        entity_id: raw_entity_id.map(str::to_string),
                        target_trait: Some(trait_name.to_string()),
                        client_id: client_id.map(|s| s.to_string()),
                        source_trait: Some(trait_name.to_string()),
                        traits,
                        entity_by_trait,
                        ..Default::default()
                    }]
                } else {
                    // The seed has nothing to do on the server (the generated handler
                    // would refuse an event it has no arm for): address each
                    // listener whose delivery does.
                    let mut covered: HashSet<&str> = HashSet::new();
                    let mut legs = Vec::new();
                    for (t, r) in outcomes {
                        let Some(entry) = r.entry.as_ref().filter(|_| r.executed) else {
                            continue;
                        };
                        let delivery = DeliveryRecord::from_value(&entry.view.event);
                        let emitter = delivery.as_ref().and_then(|d| d.source.r#trait.as_deref());
                        if emitter.is_some_and(|e| covered.contains(e)) {
                            covered.insert(t.as_str());
                            continue;
                        }
                        if !delegating_traits.contains(t) {
                            continue;
                        }
                        covered.insert(t.as_str());
                        let Some(delivered) = r.event.clone() else {
                            continue;
                        };
                        let dispatch_log = dispatch_log_of(entry);
                        legs.push(EventRequest {
                            event: delivered,
                            tick: tick.map(str::to_string),
                            payload: with_awaiting(Value::clone(&r.payload)),
                            entity_id: raw_entity_id.map(str::to_string),
                            target_trait: Some(t.clone()),
                            client_id: client_id.map(|s| s.to_string()),
                            source_trait: emitter.map(str::to_string),
                            // Only the target replays on the host; every other carried trait is held where the
                            // client left it, or the host's state echo would revert it.
                            traits: traits.as_ref().map(|carried| {
                                carried
                                    .iter()
                                    .map(|c| {
                                        match outcomes.iter().find(|(name, _)| name == &c.r#trait) {
                                            Some((_, ran)) if c.r#trait != *t => TraitFrom {
                                                from: ran.new_state.clone(),
                                                ..c.clone()
                                            },
                                            _ => c.clone(),
                                        }
                                    })
                                    .collect()
                            }),
                            entity_by_trait: (!entry.entity_before.is_null()).then(|| {
                                ValueMap::from_iter([(t.clone().into(), entry.entity_before.to_value())])
                            }),
                            delivery,
                            dispatch_log,
                            ..Default::default()
                        });
                    }
                    legs
                }
            }
        } else {
            Vec::new()
        };
        for leg in &mut server_legs {
            leg.residence = Some(residence);
        }

        // The host holds the circuit state but not a client-side write into a `[shared]` frame: the leg carries the
        // frame the host replays the event from (G-CROSS-260).
        let shared_frame = self
            .reads_shared_frame(trait_name)
            .then(|| {
                outcomes
                    .iter()
                    .find(|(t, r)| t == trait_name && r.executed)
                    .and_then(|(_, r)| r.entry.as_ref())
                    .map(|entry| entry.entity_before.to_value())
                    .filter(|row| !row.is_null())
            })
            .flatten();
        let stateful_leg =
            (server_legs.is_empty() && !hybrid && self.effect_executor.environment().is_client())
                .then(|| StatefulLeg {
                    leg: EventRequest {
                        event: event.to_string(),
                        payload: with_awaiting(payload.clone()),
                        entity_id: raw_entity_id.map(str::to_string),
                        target_trait: Some(trait_name.to_string()),
                        client_id: client_id.map(str::to_string),
                        source_trait: Some(trait_name.to_string()),
                        entity_by_trait: shared_frame
                            .map(|row| ValueMap::from_iter([(trait_name.into(), row)])),
                        ..Default::default()
                    },
                    awaiting: if tick.is_some() {
                        Vec::new()
                    } else {
                        self.awaiting_of(outcomes)
                    },
                });
        let awaiting = if tick.is_some() || server_legs.is_empty() {
            Vec::new()
        } else {
            self.awaiting_of(outcomes)
        };
        ClientDispatch {
            response,
            server_legs,
            outcomes: outcomes.to_vec(),
            seed: (trait_name.to_string(), event.to_string()),
            mode,
            snapshot,
            awaiting,
            stateful_leg,
        }
    }

    /// The server-backed traits of the client's mount still awaiting their lifecycle event, as the
    /// `_awaitingInit` a leg carries: a client-only trait's INIT never posts, so the host would hold its
    /// deliveries forever (twin of `withMountedSet`, `almadar-runtime/src/evaluation/client-role.ts`).
    pub fn awaiting_init_value(&self) -> Value {
        let defs = self.registry.trait_definitions.read();
        let mut names: Vec<String> = self
            .mount
            .read()
            .awaiting
            .iter()
            .filter(|name| {
                defs.get(name.as_str()).is_none_or(|def| {
                    !matches!(self.dispatch_mode_of(def), DispatchMode::HybridClientOnly)
                })
            })
            .cloned()
            .collect();
        names.sort();
        Value::Array(names.into_iter().map(Value::string).collect())
    }

    /// The legs a fold's own arms owe the host, one per trait whose arm delegated an effect: a trait
    /// continuing its own unconsumed emit, one whose delegated data lives in the browser store, or a
    /// listener whose emitter ran in this fold and posts no leg (EXEC-DIST-10). Any other listener's server
    /// work is the host's own cascade (it ran the emitter, or the emitter's own leg fans out there), so it is
    /// dropped. Drains the collector, so the fold's effects never ride a later dispatch.
    pub fn fold_follow_on_legs(
        &self,
        outcomes: &[(String, ProcessEventResult)],
        client_id: Option<&str>,
    ) -> Result<Vec<EventRequest>, KernelError> {
        let _lane = self.lane.enter(None)?;
        let _ = self.drain_server_leg_collector();
        let residence = orbital_core_base::runs_on::Residence::of_entities(
            self.registry.entity_definitions.read().values(),
        );
        let delegates = |result: &ProcessEventResult| {
            result.executed
                && result
                    .effect_results
                    .iter()
                    .any(|r| matches!(r.outcome, EffectOutcome::Delegated(_)))
        };
        let ran_client_side: HashSet<&str> = outcomes
            .iter()
            .filter(|(_, result)| result.executed && !delegates(result))
            .map(|(trait_name, _)| trait_name.as_str())
            .collect();
        Ok(outcomes
            .iter()
            .filter_map(|(trait_name, result)| {
                let entry = result.entry.as_ref().filter(|_| result.executed)?;
                let delegated: Vec<&Effect> = result
                    .effect_results
                    .iter()
                    .filter(|r| matches!(r.outcome, EffectOutcome::Delegated(_)))
                    .map(|r| &r.effect)
                    .collect();
                if delegated.is_empty() {
                    return None;
                }
                let delivery = DeliveryRecord::from_value(&entry.view.event);
                let emitter = delivery.as_ref().and_then(|d| d.source.r#trait.as_deref());
                let own = emitter == Some(trait_name.as_str());
                let unposted_emitter = emitter.is_some_and(|e| ran_client_side.contains(e));
                if !own && !unposted_emitter && !delegated.iter().all(|e| browser_data_effect(e, &residence)) {
                    return None;
                }
                let event = result.event.clone()?;
                Some(EventRequest {
                    event: event.clone(),
                    payload: (!result.payload.is_null()).then(|| Value::clone(&result.payload)),
                    target_trait: Some(trait_name.clone()),
                    source_trait: Some(emitter.unwrap_or(trait_name).to_string()),
                    client_id: client_id.map(str::to_string),
                    traits: Some(vec![TraitFrom {
                        r#trait: trait_name.clone(),
                        from: result.previous_state.clone(),
                        event: Some(event),
                    }]),
                    entity_by_trait: (!entry.entity_before.is_null()).then(|| {
                        ValueMap::from_iter([(trait_name.clone().into(), entry.entity_before.to_value())])
                    }),
                    delivery,
                    dispatch_log: dispatch_log_of(entry),
                    ..Default::default()
                })
            })
            .collect())
    }

    /// Whether every effect is a data effect on a browser-stored entity (the host's store holds the data).
    /// Where a leg carrying `effects` runs, and whether it mixes browser-stored rows with server work (refused).
    pub(super) fn leg_residence(&self, effects: &[Effect]) -> (LegResidence, bool) {
        let residence = orbital_core_base::runs_on::Residence::of_entities(self.registry.entity_definitions.read().values());
        let touch = effects
            .iter()
            .fold(orbital_core_base::runs_on::LegTouch::NONE, |acc, e| {
                acc.then(orbital_core_base::runs_on::LegTouch::of_effect(e, &residence))
            });
        let at = if touch.server_work { LegResidence::Server } else { LegResidence::Browser };
        (at, touch.is_mixed())
    }

    pub(super) fn all_browser_data(&self, effects: &[Effect]) -> bool {
        let residence = orbital_core_base::runs_on::Residence::of_entities(
            self.registry.entity_definitions.read().values(),
        );
        effects.iter().all(|e| browser_data_effect(e, &residence))
    }

    /// Each non-local trait a dispatch's local run executed, by its first firing (the web
    /// client kernel's `awaitingEntries`).
    pub(super) fn awaiting_of(&self, outcomes: &[(String, ProcessEventResult)]) -> Vec<AwaitingTrait> {
        let defs = self.registry.trait_definitions.read();
        let mut out: Vec<AwaitingTrait> = Vec::new();
        for (name, result) in outcomes {
            let Some(event) = result.event.as_ref().filter(|_| result.executed) else {
                continue;
            };
            if out.iter().any(|a| &a.trait_name == name) {
                continue;
            }
            let local = defs.get(name).is_some_and(|def| {
                matches!(self.dispatch_mode_of(def), DispatchMode::HybridClientOnly)
            });
            if local {
                continue;
            }
            out.push(AwaitingTrait {
                trait_name: name.clone(),
                event: event.clone(),
                from: result.previous_state.clone(),
            });
        }
        out
    }

    /// Downcast the configured [`EffectDelegate`] to the concrete
    /// [`ServerLegCollector`] every client-role kernel is configured with
    /// (`KernelConfig.delegate`) and take everything it collected since the
    /// last drain. Empty when no delegate is configured (e.g. a Server-env
    /// kernel, or a Client-env kernel under test with no delegate wired).
    pub(super) fn drain_server_leg_collector(&self) -> (Vec<Effect>, BTreeSet<String>) {
        self.server_leg_collector()
            .map(ServerLegCollector::drain_attributed)
            .unwrap_or_default()
    }

    pub(super) fn server_leg_collector(&self) -> Option<&ServerLegCollector> {
        self.effect_executor
            .delegate()
            .and_then(|d| d.as_any())
            .and_then(|a| a.downcast_ref::<ServerLegCollector>())
    }

    /// Fold a server [`EventResponse`] into this kernel: `data`/`entity_by_trait`
    /// merge into the entity source (server wins), `states{}` write through as a
    /// RECONCILE (no guard evaluation — the server already decided the
    /// transition fired), and every `emitted_events` entry NOT in
    /// `already_delivered` fans out through the SAME cascade [`Self::dispatch`]
    /// uses (`run_cascade`) — the Rust twin of `useTraitStateMachine.ts`'s
    /// `stampLocallyDeliveredEchoes`, so a live-push echo of an event this
    /// client already realized locally doesn't re-run it.
    ///
    /// An emit whose source trait has an arm for it from its reconciled state and
    /// that the host did not already run (`source.dispatched`) is delivered to that
    /// trait first; its listeners fan out from that delivery.
    ///
    /// `already_delivered` filters only those reported emits (EXEC-DIST-10): a step the fold runs is a new
    /// delivery even when its pair matches one the dispatch delivered, and the visit key bounds its cycles.
    pub fn apply_server_response(
        &self,
        response: &EventResponse,
        already_delivered: &HashSet<(String, String)>,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        self.apply_server_response_with(response, already_delivered, None).map(|(outcomes, _)| outcomes)
    }

    /// [`Self::apply_server_response`], also reporting what the response's frame delta did to this kernel's frames
    /// (`base`: the rows the leg carried, by frame, which the host's echo of them must not clobber).
    pub fn apply_server_response_with(
        &self,
        response: &EventResponse,
        already_delivered: &HashSet<(String, String)>,
        base: Option<&HashMap<String, ValueMap>>,
    ) -> Result<(Vec<(String, ProcessEventResult)>, FramesApplied), KernelError> {
        let misfit = response.data.iter().flatten().find_map(|(entity_type, rows)| {
            let keyed: Vec<std::borrow::Cow<'_, Value>> = rows.iter().map(keyed_row).collect();
            self.rows_misfit(entity_type, keyed.iter().map(|row| row.as_ref()))
        });
        if let Some(misfit) = misfit {
            return Err(KernelError::Internal(format!("server response refused: {misfit}")));
        }
        if let Some(data) = &response.data {
            for (entity_type, rows) in data {
                for row in rows {
                    let id = wire_row_id(row);
                    self.entity_source.write(entity_type, &id, row).map_err(KernelError::Internal)?;
                }
                self.rows.note_write(entity_type);
            }
        }
        for written in response.effect_results.iter().flatten() {
            if written.effect == crate::effects::response::ServerEffectKind::Persist
                && written.success
            {
                match &written.entity_type {
                    Some(entity_type) => self.rows.note_write(entity_type),
                    None => {
                        for entity_type in written
                            .data
                            .as_ref()
                            .and_then(|summary| summary.get_path("operations"))
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|op| {
                                op.get_path("entityType")?.as_str().map(str::to_string)
                            })
                        {
                            self.rows.note_write(&entity_type);
                        }
                    }
                }
            }
        }

        // The host's paint of a slot is newer than the kernel's own render of it.
        {
            let mut renders = self.renders.write();
            for painted in response.client_effects_by_trait.iter().flatten() {
                if let orbital_core_base::client_effect::ClientEffect::RenderUi { slot, .. } =
                    &painted.effect
                {
                    renders.forget_slot(&painted.trait_name, slot);
                }
            }
        }

        let client_only: HashSet<String> = {
            let defs: Vec<Arc<TraitDefinition>> =
                self.registry.trait_definitions.read().values().cloned().collect();
            defs.iter()
                .filter(|d| self.dispatch_mode_of(d) == DispatchMode::HybridClientOnly)
                .map(|d| d.name.clone())
                .collect()
        };

        let frames_applied = match &response.frames {
            Some(delta) => self.apply_server_frames(delta, &client_only, base)?,
            None => FramesApplied::default(),
        };

        {
            let mut state_manager = self.state_manager.write();
            // A client-only trait's state is the client's, unless the host ran it (a browser leg's cascade): then the
            // host's state continues from the state the leg carried.
            let host_ran: HashSet<&str> = response
                .transitions
                .iter()
                .flatten()
                .map(|hop| hop.trait_name.as_str())
                .collect();
            for (trait_name, state_name) in &response.states {
                if client_only.contains(trait_name) && !host_ran.contains(trait_name.as_str()) {
                    continue;
                }
                let frame_id = frames_applied.ids.get(&self.trait_frame_key(trait_name));
                let entity_id = self.state_key(Some(frame_id.map_or(RUNTIME_SINGLETON, String::as_str)));
                if state_manager
                    .get_or_create_state(trait_name, entity_id.as_deref())
                    .is_ok()
                {
                    if let Some(state) = state_manager.get_state_mut(trait_name, entity_id.as_deref()) {
                        state.transition_to(state_name);
                    }
                }
            }
        }

        let mut visited = DispatchVisits::default();
        let mut fanned: Vec<CascadeItem> = Vec::new();
        for ev in &response.emitted_events {
            let source = ev.source.clone().unwrap_or_default();
            let source_trait = source.r#trait.clone().unwrap_or_default();
            if already_delivered.contains(&(source_trait.clone(), ev.event.clone())) {
                continue;
            }
            let payload = ev.payload.clone().unwrap_or(Value::Null);
            let host_ran = source.dispatched == Some(true);
            if !host_ran
                && self.trait_handles(
                    &source_trait,
                    &self.current_state_name(&source_trait, Some(RUNTIME_SINGLETON)),
                    &ev.event,
                )
            {
                let payload = Arc::new(payload);
                fanned.push(CascadeItem {
                    trait_name: source_trait.clone(),
                    event: ev.event.clone(),
                    delivery: Arc::new(delivery_record_value(&ev.event, &payload, &source)),
                    source_payload: Arc::clone(&payload),
                    payload,
                    payload_digest: None,
                    listen_guard: None,
                    fans_out: true,
                    repaint: None,
                    completion: false,
                    listen_key: None,
                    row: None,
                });
                continue;
            }
            let before = fanned.len();
            self.collect_listener_targets(
                &source,
                &source_trait,
                &ev.event,
                &payload,
                &[],
                &mut fanned,
            );
            // The host's own fan-out already ran this emit's listeners (their rows and renders are in
            // this response); a client-only listener is the exception, its host run being dropped.
            if host_ran
                && self
                    .host_fans_out
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                let mut reached = fanned.split_off(before);
                reached.retain(|item| client_only.contains(&item.trait_name));
                fanned.extend(reached);
            }
        }

        let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
        // No single entity id is carried on the wire per emitted event (mirrors
        // `dispatch`'s own single-`eid`-per-call model) — cascaded processing uses
        // the same id-less convention `process_event` already falls back to.
        self.run_cascade(
            RUNTIME_SINGLETON,
            fanned.into(),
            &mut visited,
            &mut DispatchMemory::default(),
            &mut outcomes,
        )?;
        Ok((outcomes, frames_applied))
    }

    /// Deliver an emit to its declared listeners only: guards and payload mapping run, the emitter's own arm does not,
    /// and neither `host_fans_out` nor the emit's `dispatched` stamp narrows who hears it (the push of another
    /// client's emit, which must run the listeners here and nothing else).
    pub fn deliver_emit_to_listeners(
        &self,
        emitted: &orbital_core_base::bus_event::EmittedEvent,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        self.deliver_to_listeners(emitted, false)
    }

    /// [`Self::deliver_emit_to_listeners`] for the live message of this client's own running call: the emitter ran its own
    /// arm as that call's progress dispatch, so a same-name listener route back to it is that arm again (EXEC-DIST-8).
    pub fn deliver_origin_emit_to_listeners(
        &self,
        emitted: &orbital_core_base::bus_event::EmittedEvent,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        self.deliver_to_listeners(emitted, true)
    }

    fn deliver_to_listeners(
        &self,
        emitted: &orbital_core_base::bus_event::EmittedEvent,
        emitter_ran: bool,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        let source = emitted.source.clone().unwrap_or_default();
        let source_trait = source.r#trait.clone().unwrap_or_default();
        let payload = emitted.payload.clone().unwrap_or(Value::Null);
        let ran: &[String] = if emitter_ran { std::slice::from_ref(&source_trait) } else { &[] };
        let mut fanned: Vec<CascadeItem> = Vec::new();
        self.collect_listener_targets(&source, &source_trait, &emitted.event, &payload, ran, &mut fanned);
        let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
        self.run_cascade(
            RUNTIME_SINGLETON,
            fanned.into(),
            &mut DispatchVisits::default(),
            &mut DispatchMemory::default(),
            &mut outcomes,
        )?;
        Ok(outcomes)
    }

    /// A mount's plan (EXEC-DIST-9), the one decision every host runs a mount through: of the traits the request
    /// carries a copy of, only those whose state the ended session held (`restored`) are restored — seeded at their
    /// carried `from` state and row, their lifecycle event not run. Every other seed runs its lifecycle event from the
    /// trait's own state; a carried copy of it is the client's own step and is ignored.
    pub fn prepare_mount(
        &self,
        restored: &std::collections::BTreeSet<String>,
        seeds: &[crate::effects::MountSeed],
        traits: Option<&[TraitFrom]>,
        entity_by_trait: Option<&ValueMap>,
        entity_id: Option<&str>,
    ) -> MountPlan {
        let carried: HashSet<String> = traits
            .into_iter()
            .flatten()
            .map(|t| t.r#trait.clone())
            .chain(entity_by_trait.into_iter().flat_map(|rows| rows.keys().map(|k| k.to_string())))
            .filter(|name| restored.contains(name) && seeds.iter().any(|seed| &seed.r#trait == name))
            .collect();
        let restored_rows: ValueMap = entity_by_trait
            .iter()
            .flat_map(|rows| rows.iter())
            .filter(|(name, _)| carried.contains(name.as_str()))
            .map(|(name, row)| (name.clone(), row.clone()))
            .collect();
        if !carried.is_empty() {
            let restored_traits: Vec<TraitFrom> = traits.into_iter().flatten().filter(|t| carried.contains(&t.r#trait)).cloned().collect();
            self.seed_leg(&restored_traits, Some(&restored_rows), entity_id);
        }
        MountPlan {
            unseeded: seeds.iter().filter(|seed| !carried.contains(&seed.r#trait)).cloned().collect(),
            restored_rows,
        }
    }

    /// Seed a delegated server leg with the client's circuit state: each carried trait is held at its
    /// `from` state, and each carried row is folded into its trait's frame (`@entity`), so the leg replays
    /// from where the client was. JS twin: `evaluateOrbitalEvent`'s `manager.seedState` + frame fold.
    pub fn seed_leg(
        &self,
        traits: &[TraitFrom],
        entity_by_trait: Option<&ValueMap>,
        entity_id: Option<&str>,
    ) {
        let eid = entity_id.unwrap_or(RUNTIME_SINGLETON);
        for carried in traits {
            if !self.registry.trait_definitions.read().contains_key(&carried.r#trait) {
                continue;
            }
            let mut state_manager = self.state_manager.write();
            let state_key = self.state_key(Some(eid));
            if state_manager
                .get_or_create_state(&carried.r#trait, state_key.as_deref())
                .is_ok()
            {
                if let Some(state) = state_manager.get_state_mut(&carried.r#trait, state_key.as_deref()) {
                    state.transition_to(&carried.from);
                }
            }
        }
        for (trait_name, row) in entity_by_trait.into_iter().flatten() {
            let entity_type = match self.registry.trait_definitions.read().get(trait_name.as_str()) {
                Some(def) => def
                    .linked_entity
                    .clone()
                    .unwrap_or_else(|| trait_name.to_string()),
                None => continue,
            };
            if !row.as_object().is_some_and(|o| !o.is_empty()) {
                continue;
            }
            let carried_id = row
                .get_path("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string);
            // A persisted `[shared]` entity's frame is its one working instance: a detail row's id never addresses it.
            // A `[runtime]` row has no store behind it, so the client's copy is the row and always folds.
            let persisted_shared = self
                .registry.entity_definitions
                .read()
                .get(&entity_type)
                .is_some_and(|d| d.shared && d.persistence != Some(Persistence::Runtime));
            if persisted_shared && carried_id.is_some() {
                continue;
            }
            let slot = self.frame_slot(trait_name, &entity_type, eid);
            match carried_id {
                Some(id) => self.carried_rows.write().insert(slot, id),
                None => self.carried_rows.write().remove(&slot),
            };
            self.write_trait_frame(
                trait_name,
                &entity_type,
                eid,
                &SharedEntity::from_value_ref(row),
            );
        }
    }

    /// Get the current state for an entity.
    /// Put a suspended step's trait back to the state it is replayed from; returns the state another dispatch moved it to
    /// meanwhile (`None` when it still holds the state the step entered).
    pub(crate) fn rewind_provisional(&self, commit: &ProvisionalCommit) -> Option<TraitState> {
        let mut state_manager = self.state_manager.write();
        let state_key = self.state_key(Some(&commit.entity_id));
        let held = state_manager.get_state_mut(&commit.trait_name, state_key.as_deref())?;
        let moved = (held.current_state != commit.to).then(|| held.clone());
        *held = commit.from.clone();
        moved
    }

    /// Hold `state` as `trait_name`'s state under `entity_id`.
    pub(crate) fn set_held_state(&self, trait_name: &str, entity_id: &str, state: TraitState) {
        let mut state_manager = self.state_manager.write();
        let state_key = self.state_key(Some(entity_id));
        if state_manager
            .get_or_create_state(trait_name, state_key.as_deref())
            .is_ok()
        {
            if let Some(held) = state_manager.get_state_mut(trait_name, state_key.as_deref()) {
                *held = state;
            }
        }
    }

    /// The browser's view of `response` (EXEC-DIST-8): the one projection a response takes on its way to a browser. An
    /// emit stays when the browser can use it: a client-run trait (client or optimistic) listens to it, its emitter is
    /// client-run, its contract is `external`, it carries a persist success, or a pattern prop of some `render-ui`
    /// names it. An emit only server traits hear is dropped. In-process consumers keep the full response; call this after
    /// [`stamp_emitted_sources`](Self::stamp_emitted_sources).
    pub fn client_view(&self, mut response: EventResponse) -> EventResponse {
        if response.emitted_events.is_empty() {
            return response;
        }
        let mut named: Option<HashSet<String>> = None;
        let emitted = std::mem::take(&mut response.emitted_events);
        response.emitted_events = emitted
            .into_iter()
            .filter(|e| match self.browser_hears(e) {
                Some(heard) => heard,
                None => named
                    .get_or_insert_with(|| self.pattern_event_names())
                    .contains(&e.event),
            })
            .collect();
        response
    }

    /// `Some(true)` when the browser hears `emitted` by a rule that needs no program scan, `None` when only a pattern
    /// prop could name it.
    pub(super) fn browser_hears(&self, emitted: &orbital_core_base::bus_event::EmittedEvent) -> Option<bool> {
        let Some(source) = emitted.source.as_ref() else {
            return Some(true);
        };
        if source.persist_success == Some(true) {
            return Some(true);
        }
        let Some(emitter) = source.r#trait.as_deref() else {
            return Some(true);
        };
        let defs = self.registry.trait_definitions.read();
        let client_run = |name: &str| {
            defs.get(name)
                .is_some_and(|d| self.dispatch_mode_of(d) != DispatchMode::PersistedAwaited)
        };
        let external = defs
            .get(emitter)
            .and_then(|d| d.emits.as_ref())
            .is_some_and(|contracts| {
                contracts.iter().any(|c| {
                    c.event == emitted.event
                        && matches!(c.scope, Some(orbital_core_base::schema::types::EventScope::External))
                })
            });
        let heard = client_run(emitter)
            || external
            || self
                .listener_manager
                .matching(&emitted.event, source)
                .iter()
                .any(|l| client_run(&l.trait_name));
        heard.then_some(true)
    }

    /// The payload a deferred completion's push carries to the browser (EXEC-QUEUE-10): the payload when the completing
    /// trait runs on the client, `Null` (no payload) when it runs on the server.
    pub fn browser_completion_payload(&self, trait_name: &str, payload: &Value) -> Value {
        let server_run = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .is_some_and(|d| self.dispatch_mode_of(d) == DispatchMode::PersistedAwaited);
        if server_run {
            Value::Null
        } else {
            payload.clone()
        }
    }
}

/// A row the server sent as it is stored: the wire leaves the runtime singleton's id implicit, so an id-less row is
/// checked with the id the fold gives it.
fn keyed_row(row: &Value) -> std::borrow::Cow<'_, Value> {
    match row {
        Value::Object(fields) if !fields.contains_key("id") => {
            let mut fields = fields.clone();
            fields.insert(orbital_core_base::static_key!("id"), Value::string(wire_row_id(row)));
            std::borrow::Cow::Owned(Value::Object(fields))
        }
        _ => std::borrow::Cow::Borrowed(row),
    }
}

/// The kernel id of a row the server sent: the wire spells the runtime singleton `singleton`, and a row with no id is it.
fn wire_row_id(row: &Value) -> String {
    match row.as_object().and_then(|o| o.get("id")).and_then(Value::as_str) {
        Some("singleton") | None => RUNTIME_SINGLETON.to_string(),
        Some(id) => id.to_string(),
    }
}
