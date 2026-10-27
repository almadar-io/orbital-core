//! Processing an event on a trait: the composed step and its payload checks.

use super::*;

impl RuntimeKernel {
    /// Process an event for a specific entity.
    pub fn process_event(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
    ) -> Result<ProcessEventResult, KernelError> {
        let _lane = self.lane.enter(None)?;
        self.process_event_in_dispatch(trait_name, entity_id, event_name, Arc::new(payload), None, false)
            .map_err(Fault::into_kernel)
    }

    /// `process_event` inside a dispatch: `view` supplies `@event`/`@prevEvents`/`@prevStates`.
    /// Guards read the source state; effects read the target (Runtime Spec Clause 3.3).
    pub(super) fn process_event_in_dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Arc<Value>,
        view: Option<&DispatchView>,
        follow_on: bool,
    ) -> Result<ProcessEventResult, Fault> {
        self.process_event_composed(
            trait_name, entity_id, event_name, payload, view, follow_on, None,
        )
    }

    /// [`Self::process_event_in_dispatch`] with an explicit `@callsitePayload` (a repaint of an embedded
    /// child under its composer's payload); `None` reads the payload the trait was last composed with.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn process_event_composed(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Arc<Value>,
        view: Option<&DispatchView>,
        follow_on: bool,
        callsite: Option<&Value>,
    ) -> Result<ProcessEventResult, Fault> {
        // An id-less event (`entity_id: None`) still needs somewhere to keep
        // `@entity` writes across transitions, so the kernel keys a per-trait
        // frame under the literal id "singleton" — one frame per (trait,
        // "singleton") pair, not per row. The compiled TS server reproduces
        // the same view via `@almadar/core`'s `resolveEntityView` (`frame <
        // row < id` precedence), with `OirEntity::frame_fields()` (the
        // `@intrinsic` fields) as the one allow-list for what belongs in the
        // frame tier on both paths.
        let entity_id = entity_id.unwrap_or(RUNTIME_SINGLETON);
        let stamp = self.begin_dispatch();
        let _transition = tracing::trace_span!(target: orbital_core_base::logging::ns::RUNTIME_EFFECTS, "transition", trait_name = %trait_name, event = %event_name).entered();

        debug!(
            trait_name = %trait_name,
            entity_id = %entity_id,
            event = %event_name,
            "Processing event"
        );

        let trait_def = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .cloned()
            .ok_or_else(|| KernelError::TraitNotFound(trait_name.to_string()))?;

        let mut state_manager = self.state_manager.write();

        let state_key = self.state_key(Some(entity_id));
        let current_state = state_manager
            .get_or_create_state(trait_name, state_key.as_deref())
            .map_err(|e| KernelError::Internal(e.to_string()))?
            .clone();

        // `@payload` is this event's own payload; an absent one reads `{}` (drift D-2).
        let effective_payload = if payload.is_null() {
            Value::object(ValueMap::default())
        } else {
            Value::clone(&payload)
        };

        let ctx = BindingContextBuilder::new()
            .payload(effective_payload.clone())
            .state(Value::string(current_state.current_state.clone()))
            .user(self.sessions.current_user.read().clone())
            .now(stamp.now)
            .current_theme(Value::String(
                (self.resolve_current_theme_for_trait(trait_name).key).into(),
            ))
            .pages(self.presentation.nav_pages.read().clone());
        let mut ctx = self.with_viewer_i18n(ctx).build();
        ctx.set_prob_entropy(self.prob_entropy());
        ctx.set_config(bind_user_forwards(
            config_defaults_value(&trait_def, &self.registry.trait_definitions.read()),
            &self.sessions.current_user.read(),
        ));
        if let Some(view) = view {
            view.apply(&mut ctx);
        }
        let capture = callsite
            .cloned()
            .or_else(|| self.composed_callsite.read().get(trait_name).cloned());
        if let Some(capture) = &capture {
            ctx.set_callsite_payload(capture.clone());
        }

        let entity_type = trait_def.linked_entity.as_deref().unwrap_or(trait_name);
        let entity_data = self.read_trait_entity(trait_name, entity_type, entity_id);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }
        if !entity_data.is_null() {
            ctx.set_entity_shared(entity_data.clone());
        }
        debug!(
            target: "almadar::kernel",
            entity_type = %entity_type,
            entity_id = %entity_id,
            resolved = !entity_data.is_null(),
            entity_keys = ?entity_data.fields().map(|o| o.keys().collect::<Vec<_>>()),
            "kernel: entity read for @entity bindings"
        );

        let event_payload = EventPayload {
            event: event_name.to_string(),
            data: Value::clone(&payload),
            source: Some(entity_id.to_string()),
            timestamp: orbital_core_base::time_util::now_millis(),
        };

        // Guard/transition evaluation runs against `ctx` — the SAME context
        // already built above with payload/state/entity/user/config/currentTheme
        // — instead of a second, state+entity-only context the manager would
        // otherwise build from scratch (§157 `identity`: this is what makes
        // `(when ["=", "@user.role", ...])` / `@config.*` guards see real values
        // on the Rust path).
        let result = state_manager
            .process_event_with_ctx(trait_name, state_key.as_deref(), &event_payload, &ctx)
            .map_err(|e| KernelError::Internal(e.to_string()))?;
        // The transition is committed; its effects run without the state lock, so one that reads states (or
        // dispatches in place) does not wait on its own dispatch (EXEC-QUEUE-11). The lane keeps other dispatches out.
        drop(state_manager);

        if result.executed {
            let new_state = result
                .new_state
                .clone()
                .unwrap_or(current_state.current_state.clone());
            ctx.set_state(Value::string(new_state.clone()));
            ctx.from_state = Some(current_state.current_state.clone());
            ctx.to_state = Some(new_state.clone());
            *self.step.step_site.write() = Some((trait_name.to_string(), follow_on));
            *self.step.step_frame.write() = Some(entity_id.to_string());
            let (effect_results, captured_entity_data, effect_runs, failure) =
                match self.server_leg_collector() {
                    Some(collector) => collector.attribute(trait_name, || {
                        self.execute_effects_traced(result.effects(), &ctx)
                    }),
                    None => self.execute_effects_traced(result.effects(), &ctx),
                };
            *self.step.step_site.write() = None;
            *self.step.step_frame.write() = None;

            // Effective entity = read entity with Set-effect fields merged in. Persist
            // the MERGED entity (read-modify-write), never the partial
            // `captured_entity_data` (only the Set-touched fields): a replace-write of
            // the partial would drop every untouched field, so a later re-render's
            // `@entity.<untouched-field>` (tabs `activeTab: @entity.filterKind`, board
            // `current*` card fields, …) reverts to a literal token. The merged value is
            // also what render-ui resolves against below and what the client receives.
            let captured_entity_data = SharedEntity::from_value(captured_entity_data);
            let written = captured_entity_data.clone();
            let effective_entity = entity_data.merged(&captured_entity_data);

            // Resolve EVERY binding (@payload.X / @entity.X / @config.X / @state / @user)
            // in the client-facing effects against the full context, through the ONE
            // canonical resolver — `@payload` is a typed payload so `X` is arbitrary; no
            // per-field special-cases. The entity is the post-Set effective entity.
            ctx.set_entity_shared(effective_entity.clone());
            let sources = render_sources(&effect_results);
            let mut effect_results = effect_results
                .into_iter()
                .map(|er| resolve_effect_result(er, &ctx))
                .collect::<Vec<_>>();
            let mut failure = failure;
            if failure.is_none() {
                failure = effect_results
                    .iter()
                    .find(|r| is_thrown_failure(r))
                    .map(|thrown| thrown.error.clone().unwrap_or_default());
                if failure.is_some() {
                    effect_results.retain(is_thrown_failure);
                }
            }
            self.inject_relation_options(trait_name, &mut effect_results);
            if let Some(refused) =
                self.deliver_remote_emits(trait_name, follow_on, &mut effect_results)
            {
                if failure.is_none() {
                    failure = Some(refused);
                }
            }
            // Read before the frame write below and before the suspension check, so a read that suspends replays the
            // step like any other answer it waits for.
            let entity_after = self
                .record_entity_after
                .load(std::sync::atomic::Ordering::Relaxed)
                .then(|| {
                    self.read_trait_entity(trait_name, entity_type, entity_id)
                        .merged(&captured_entity_data)
                });
            if self.io.suspended() {
                // The transition is committed before its effects' I/O answers: an overlapping dispatch sees the
                // state it entered. The replay runs the step again from `from` (G-CROSS-259).
                *self.suspended_commit.write() = Some(ProvisionalCommit {
                    trait_name: trait_name.to_string(),
                    entity_id: entity_id.to_string(),
                    from: current_state,
                    to: new_state,
                });
                return Err(Fault::Suspended);
            }

            if captured_entity_data.fields().is_some_and(|o| !o.is_empty()) {
                self.write_trait_frame(trait_name, entity_type, entity_id, &captured_entity_data);
            }

            info!(
                trait_name = %trait_name,
                entity_id = %entity_id,
                event = %event_name,
                new_state = %new_state,
                effects_count = effect_results.len(),
                "Event processed successfully"
            );

            self.record_renders(trait_name, entity_id, sources, &effect_results, &ctx);

            Ok(ProcessEventResult {
                executed: true,
                previous_state: current_state.current_state.clone(),
                new_state,
                effects_executed: effect_results.len(),
                effect_results,
                entity_data: effective_entity,
                linked_entity: entity_type.to_string(),
                event: Some(event_name.to_string()),
                payload: payload.clone(),
                entry: Some(DeliveryEntry {
                    entity_before: entity_data,
                    entity_after,
                    view: view.cloned().unwrap_or_default(),
                }),
                fired_arm: result.fired_arm,
                transition_index: result.transition_index,
                entity_id: entity_id.to_string(),
                state_entity_id: entity_id.to_string(),
                guards: result.guards.clone(),
                effect_runs,
                cascade_dropped: Vec::new(),
                cascade_cut: None,
                consumed_emits: Vec::new(),
                failure,
                written,
                callsite_payload: capture,
            })
        } else {
            debug!(
                trait_name = %trait_name,
                entity_id = %entity_id,
                event = %event_name,
                "Event not processed (guard blocked or no matching transition)"
            );

            let entity_data = if entity_data.is_null() {
                SharedEntity::empty_object()
            } else {
                entity_data
            };

            // EXEC-STEP-5: a step that does not fire repaints the slots the state renders into; each shows what it
            // holds now (EXEC-RENDER-4), and a trait that has painted nothing shows the entering transition's render.
            let held = self.renders.read().held(trait_name, entity_id);
            let sources = state_render_sources(&trait_def, &current_state.current_state);
            let mut state_effects = self.get_state_render_effects(
                &trait_def,
                &current_state.current_state,
                &ctx,
                &entity_data,
            );
            if held.is_empty() {
                self.record_renders(trait_name, entity_id, sources, &state_effects, &ctx);
            } else {
                let mut slots: Vec<String> = Vec::new();
                for er in &state_effects {
                    if let Effect::RenderUi { slot, .. } = &er.effect {
                        if !slots.contains(slot) {
                            slots.push(slot.clone());
                        }
                    }
                }
                state_effects = slots
                    .into_iter()
                    .filter_map(|slot| {
                        let painted = held.iter().find(|(held_slot, _)| *held_slot == slot);
                        match painted {
                            Some((_, content)) => Some(EffectResult::executed(Effect::RenderUi {
                                slot,
                                content: content.clone(),
                            })),
                            None => state_effects
                                .iter()
                                .rev()
                                .find(|er| matches!(&er.effect, Effect::RenderUi { slot: s, .. } if *s == slot))
                                .cloned(),
                        }
                    })
                    .collect();
            }

            Ok(ProcessEventResult {
                event: None,
                executed: false,
                previous_state: current_state.current_state.clone(),
                new_state: current_state.current_state.clone(),
                effects_executed: 0,
                effect_results: state_effects,
                entity_data,
                linked_entity: entity_type.to_string(),
                payload: payload.clone(),
                entry: None,
                fired_arm: None,
                transition_index: None,
                entity_id: entity_id.to_string(),
                state_entity_id: entity_id.to_string(),
                guards: result.guards.clone(),
                effect_runs: Vec::new(),
                cascade_dropped: Vec::new(),
                cascade_cut: None,
                consumed_emits: Vec::new(),
                failure: None,
                written: SharedEntity::null(),
                callsite_payload: None,
            })
        }
    }

    /// The refusal text when `payload` does not satisfy the `payloadSchema` any of `targets` (trait, the event it
    /// receives) declares, including nested fields and program-defined types. `None` when it does.
    pub fn payload_rejection<'a>(
        &self,
        targets: impl IntoIterator<Item = (&'a str, &'a str)>,
        payload: &Value,
    ) -> Option<String> {
        let mut failures = Vec::new();
        use orbital_core_base::ir::types::{declared_alias_shape, field_shape, OirField, OirType};
        let entity_types: std::collections::BTreeMap<String, OirType> = self
            .registry.entity_definitions
            .read()
            .iter()
            .map(|(name, entity)| {
                let fields = entity
                    .fields
                    .iter()
                    .filter_map(|field| {
                        let name = field.name.as_ref()?;
                        let mut declared = OirField::new(name.clone(), field_shape(field));
                        declared.required = field.required.unwrap_or(false);
                        Some(declared)
                    })
                    .collect();
                (
                    name.clone(),
                    OirType::Struct {
                        fields,
                        additional: None,
                    },
                )
            })
            .collect();
        let definitions = self.registry.trait_definitions.read();
        for (trait_name, event) in targets {
            let declared = definitions
                .get(trait_name)
                .and_then(|definition| definition.state_machine.events.as_ref())
                .and_then(|events| events.iter().find(|declared| declared.key == event))
                .and_then(|declared| declared.payload_schema.as_deref());
            if let Some(schema) = declared {
                let mut named = entity_types.clone();
                let homes = self.registry.trait_homes.read();
                let schemas = self.registry.schemas.read();
                let mut version = None;
                if let Some((program, index)) = homes
                    .get(trait_name)
                    .and_then(|(name, index)| schemas.get(name).map(|program| (program, *index)))
                {
                    version = program.type_contract_version;
                    if let Some(aliases) = program.types.as_ref() {
                        named.extend(
                            aliases.iter().map(|(name, members)| {
                                (name.clone(), declared_alias_shape(members))
                            }),
                        );
                    }
                    if let Some(aliases) = program
                        .orbitals
                        .get(index)
                        .and_then(|orbital| orbital.types.as_ref())
                    {
                        named.extend(
                            aliases.iter().map(|(name, members)| {
                                (name.clone(), declared_alias_shape(members))
                            }),
                        );
                    }
                }
                failures.extend(
                    crate::payload_validation::validate_event_payload_with_context(
                        event, payload, schema, &named, version,
                    ),
                );
            }
        }
        drop(definitions);
        (!failures.is_empty())
            .then(|| crate::payload_validation::format_payload_failures(&failures))
    }

    /// The API-boundary check of an external event: a server kernel refuses a payload that does not satisfy its
    /// declared event before any transition runs. A client kernel's events come from its own UI and are not checked.
    pub(crate) fn require_payload<'a>(
        &self,
        targets: impl IntoIterator<Item = (&'a str, &'a str)>,
        payload: &Value,
    ) -> Result<(), KernelError> {
        if self.effect_executor.environment().is_client() {
            return Ok(());
        }
        match self.payload_rejection(targets, payload) {
            Some(message) => Err(KernelError::PayloadInvalid(message)),
            None => Ok(()),
        }
    }

    /// The payload `payloadSchema` `trait_name` declares for `event`, when it declares one.
    pub fn event_payload_schema(
        &self,
        trait_name: &str,
        event: &str,
    ) -> Option<Vec<orbital_core_base::schema::types::PayloadField>> {
        self.registry.trait_definitions
            .read()
            .get(trait_name)?
            .state_machine
            .events
            .as_ref()?
            .iter()
            .find(|e| e.key == event)?
            .payload_schema
            .clone()
    }
}
