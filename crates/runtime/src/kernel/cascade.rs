//! The cascade: a step's follow-ups, listener targets, repaints, and the sources stamped on emits.

use super::*;

impl RuntimeKernel {
    /// Find every listener reacting to `event` from `source_trait`, evaluate each guard +
    /// payload mapping against `source_payload`, and push `(listener_trait, triggers, payload)`
    /// targets for the cascade queue.
    /// What a processed step queues next: the trait's own emits it handles from its new
    /// state (the same-trait cascade, Runtime Spec 5.3, which runs before any fan-out)
    /// and every listener target of the dispatched event and of each emit.
    pub(super) fn step_followups(
        &self,
        trait_name: &str,
        event: &str,
        payload: &Value,
        result: Option<&ProcessEventResult>,
        direct_scopes: &[String],
        fans_out: bool,
    ) -> StepFollowups {
        let source = self.emit_source(trait_name);
        let mut own: Vec<CascadeItem> = Vec::new();
        let mut fanned = Vec::new();
        let mut consumed_emits = Vec::new();
        if fans_out {
            self.collect_listener_targets(
                &source,
                trait_name,
                event,
                payload,
                direct_scopes,
                &mut fanned,
            );
        }
        for (index, er) in result
            .map_or(&[][..], |r| &r.effect_results[..])
            .iter()
            .enumerate()
        {
            if let Effect::Emit {
                event: emitted,
                payload: ep,
            } = &er.effect
            {
                let handled = result.is_some_and(|r| {
                    r.executed && self.trait_handles(trait_name, &r.new_state, emitted)
                });
                let (own_before, fanned_before) = (own.len(), fanned.len());
                self.emit_followup(
                    trait_name,
                    &source,
                    handled,
                    er.completion,
                    emitted,
                    ep,
                    &mut own,
                    &mut fanned,
                );
                if own.len() > own_before || fanned.len() > fanned_before {
                    consumed_emits.push(index);
                }
            }
        }
        if let Some(result) = result {
            self.refusal_followups(trait_name, &source, result, &mut fanned);
        }
        if let Some(result) = result.filter(|r| r.executed) {
            let mut repaints = self.first_repaints(trait_name, result);
            repaints.append(&mut own);
            own = repaints;
        }
        StepFollowups {
            own,
            fanned,
            consumed_emits,
        }
    }

    /// The re-renders a step's composition calls for: each capture child of `trait_name` that the fired
    /// arm's effects compose, to be re-fired under the step's payload, on either role.
    pub(super) fn first_repaints(&self, trait_name: &str, result: &ProcessEventResult) -> Vec<CascadeItem> {
        let Some(children) = self.capture_children.read().get(trait_name).cloned() else {
            return Vec::new();
        };
        let Some(def) = self.registry.trait_definitions.read().get(trait_name).cloned() else {
            return Vec::new();
        };
        let authored: Vec<&SExpression> =
            result.effect_runs.iter().map(|run| &run.effect).collect();
        let composed = super::callsite_capture::embedded_by_effects(&def, &authored);
        let payload = match result.payload.as_ref() {
            Value::Null => Value::object(ValueMap::default()),
            other => other.clone(),
        };
        let visited = Arc::new(orbital_core_base::sync::Mutex::new(HashSet::new()));
        children
            .into_iter()
            .filter(|child| composed.contains(child))
            .map(|child| {
                Self::repaint_item(
                    child,
                    RepaintJob {
                        composed_with: payload.clone(),
                        keep_composed: false,
                        visited: Arc::clone(&visited),
                    },
                )
            })
            .collect()
    }

    pub(super) fn repaint_item(child: String, job: RepaintJob) -> CascadeItem {
        CascadeItem {
            trait_name: child,
            event: String::new(),
            payload: null_payload(),
            delivery: null_payload(),
            source_payload: null_payload(),
            listen_guard: None,
            fans_out: false,
            repaint: Some(job),
            completion: false,
        }
    }

    /// What one `emit` queues: the emitter's own arm for it (when `handled`, the same-trait
    /// cascade) and every listener target.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_followup(
        &self,
        trait_name: &str,
        source: &BusEventSource,
        handled: bool,
        completion: bool,
        emitted: &str,
        payload: &Value,
        own: &mut Vec<CascadeItem>,
        fanned: &mut Vec<CascadeItem>,
    ) {
        if handled {
            let shared = Arc::new(payload.clone());
            own.push(CascadeItem {
                trait_name: trait_name.to_string(),
                event: emitted.to_string(),
                payload: Arc::clone(&shared),
                delivery: Arc::new(delivery_record_value(emitted, payload, source)),
                source_payload: shared,
                listen_guard: None,
                fans_out: false,
                repaint: None,
                completion,
            });
        }
        let fanned_before = fanned.len();
        self.collect_listener_targets(source, trait_name, emitted, payload, &[], fanned);
        for item in &mut fanned[fanned_before..] {
            item.completion = completion;
        }
    }

    /// The `@config` root a listener's guard reads: the listening trait's own resolved config defaults.
    pub(super) fn listener_config(&self, trait_name: &str) -> Value {
        let defs = self.registry.trait_definitions.read();
        match defs.get(trait_name) {
            Some(def) => {
                bind_user_forwards(config_defaults_value(def, &defs), &self.sessions.current_user.read())
            }
            None => Value::Null,
        }
    }

    /// Whether `trait_name` has an arm for `event` from `state`.
    pub(super) fn trait_handles(&self, trait_name: &str, state: &str, event: &str) -> bool {
        self.registry.trait_definitions
            .read()
            .get(trait_name)
            .is_some_and(|d| {
                d.state_machine
                    .transitions
                    .iter()
                    .any(|tr| tr.event == event && tr.from.matches(state))
            })
    }

    pub(super) fn collect_listener_targets(
        &self,
        source: &BusEventSource,
        source_trait: &str,
        event: &str,
        source_payload: &Value,
        direct_scopes: &[String],
        out: &mut Vec<CascadeItem>,
    ) {
        let listeners = self.local_only(self.listener_manager.matching(event, source));
        let listeners = self.without_browser_listeners(listeners);
        if listeners.is_empty() {
            return;
        }
        let evaluator = Evaluator::new();
        let shared = Arc::new(source_payload.clone());
        let ctx = listeners
            .iter()
            .any(|l| l.payload_mapping.is_some())
            .then(|| BindingContextBuilder::new().payload(source_payload.clone()).build());
        let delivery = Arc::new(delivery_record_value(event, source_payload, source));
        for l in listeners {
            // `direct_scopes` already received this very event as a seed scope on the embed
            // chain; a same-name route back to one of them is that delivery again.
            if l.triggers == event && direct_scopes.contains(&l.trait_name) {
                continue;
            }
            let mapped = match (&l.payload_mapping, &ctx) {
                (Some(_), Some(ctx)) => Arc::new(envelope::mapped_listen_payload(
                    &evaluator,
                    ctx,
                    l.payload_mapping.as_deref(),
                    source_payload,
                )),
                _ => Arc::clone(&shared),
            };
            debug!(
                source_trait = %source_trait, event = %event,
                listener_trait = %l.trait_name, triggers = %l.triggers,
                "listener fan-out",
            );
            out.push(CascadeItem {
                trait_name: l.trait_name,
                event: l.triggers,
                payload: mapped,
                delivery: Arc::clone(&delivery),
                source_payload: Arc::clone(&shared),
                listen_guard: l.guard,
                fans_out: false,
                repaint: None,
                completion: false,
            });
        }
    }

    /// Bring the response's rows to the frames the dispatch wrote: every field a step's `set`s wrote lands in
    /// every trait bound to that frame, so a sibling sharing it (stepped or not) converges on the same values
    /// (EXEC-DIST-7). With `whole_frames` (a `request.mount`) every non-empty frame is echoed whole instead,
    /// seeding the viewer. JS twin: the final-row and frame-echo passes at the end of `evaluateOrbitalEvent`.
    pub fn echo_frames(
        &self,
        response: &mut EventResponse,
        outcomes: &[(String, ProcessEventResult)],
        entity_id: Option<&str>,
        whole_frames: bool,
    ) {
        let defs = self.registry.trait_definitions.read();
        let mut rows = response.entity_by_trait.take().unwrap_or_default();
        let bound_to = |key: &str| -> Vec<String> {
            match key.strip_prefix("$shared::") {
                Some(entity) => defs
                    .values()
                    .filter(|def| def.linked_entity.as_deref() == Some(entity))
                    .map(|def| def.name.clone())
                    .collect(),
                None => defs.get(key).map(|def| def.name.clone()).into_iter().collect(),
            }
        };
        if whole_frames {
            let eid = self.session_eid(entity_id.unwrap_or(RUNTIME_SINGLETON));
            let frames = self.frames.read();
            for ((key, frame_eid), frame) in frames.iter() {
                let Some(frame_fields) = frame.fields().filter(|f| !f.is_empty()) else {
                    continue;
                };
                if *frame_eid != eid {
                    continue;
                }
                for trait_name in bound_to(key) {
                    let row = rows
                        .entry(trait_name)
                        .or_insert_with(|| Value::object(ValueMap::default()));
                    if let Value::Object(fields) = row {
                        fields.extend(frame_fields.iter().map(|(k, v)| (k.clone(), (**v).clone())));
                    }
                }
            }
        } else {
            let mut written: orbital_core_base::collections::IndexMap<String, (ValueMap, Option<Value>)> =
                Default::default();
            for (trait_name, outcome) in outcomes {
                if !outcome.executed || outcome.written_values().next().is_none() {
                    continue;
                }
                let (fields, id) = written
                    .entry(self.trait_frame_key(trait_name))
                    .or_default();
                fields.extend(outcome.written_values().map(|(k, v)| (k.clone(), v.clone())));
                if id.is_none() {
                    *id = outcome.entity_data.get("id").filter(|id| !id.is_null()).cloned();
                }
            }
            for (key, (fields, id)) in written {
                for trait_name in bound_to(&key) {
                    let row = rows
                        .entry(trait_name)
                        .or_insert_with(|| Value::object(ValueMap::default()));
                    if let Value::Object(row_fields) = row {
                        row_fields.extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));
                        if let Some(id) = &id {
                            row_fields.entry("id".to_string()).or_insert_with(|| id.clone());
                        }
                    }
                }
            }
        }
        response.entity_by_trait = (!rows.is_empty()).then_some(rows);
    }

    /// Every event name a pattern prop of the program's effects names: the props whose registry kind is an event
    /// (`event`, `event-ref`, `event-listen`, `event-list`, `event-map`).
    pub(super) fn pattern_event_names(&self) -> HashSet<String> {
        use orbital_core_base::language::pattern_props::PATTERN_EVENT_NAME_PROPS;
        fn walk(value: &serde_json::Value, out: &mut HashSet<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    let props = map
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|pattern| PATTERN_EVENT_NAME_PROPS.get(pattern));
                    for (prop, (kind, field)) in props.into_iter().flatten() {
                        let Some(named) = map.get(*prop) else { continue };
                        match (*kind, named) {
                            ("event" | "event-ref" | "event-listen", serde_json::Value::String(name)) => {
                                out.insert(name.clone());
                            }
                            ("event-list", serde_json::Value::Array(items)) => out.extend(
                                items
                                    .iter()
                                    .filter_map(|item| item.get(*field).and_then(serde_json::Value::as_str))
                                    .map(str::to_string),
                            ),
                            ("event-map", serde_json::Value::Object(entries)) => out.extend(
                                entries.values().filter_map(serde_json::Value::as_str).map(str::to_string),
                            ),
                            _ => {}
                        }
                    }
                    map.values().for_each(|v| walk(v, out));
                }
                serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
                _ => {}
            }
        }
        let mut out = HashSet::new();
        for def in self.registry.trait_definitions.read().values() {
            let transitions = def.state_machine.transitions.iter().flat_map(|t| t.effects.iter().flatten());
            let ticks = def.ticks.iter().flatten().flat_map(|t| t.effects.iter());
            for effect in transitions.chain(ticks) {
                if let Ok(json) = serde_json::to_value(effect) {
                    walk(&json, &mut out);
                }
            }
        }
        out
    }

    /// The orbital that declares `trait_name`.
    pub fn orbital_of_trait(&self, trait_name: &str) -> Option<String> {
        self.emit_source(trait_name).orbital
    }

    /// Complete the source of each event `response` carries with the emitter's identity: orbital, the ids
    /// the schema carries, and the id of the emit contract. A client matches a `listens` source on these.
    pub fn stamp_emitted_sources(&self, response: &mut EventResponse) {
        for emitted in &mut response.emitted_events {
            let Some(source) = emitted.source.as_mut() else {
                continue;
            };
            let Some(trait_name) = source.r#trait.clone() else {
                continue;
            };
            let identity = self.emit_source(&trait_name);
            source.orbital = identity.orbital;
            source.orbital_id = identity.orbital_id;
            source.trait_id = identity.trait_id;
            source.event_id = self
                .registry.trait_definitions
                .read()
                .get(&trait_name)
                .and_then(|def| def.emits.as_ref())
                .and_then(|emits| emits.iter().find(|e| e.event == emitted.event))
                .and_then(|contract| contract.event_id.clone());
        }
    }

    /// The envelope this trait stamps on its emits: `{ orbital, trait }` plus the ids the schema carries.
    pub(super) fn emit_source(&self, trait_name: &str) -> BusEventSource {
        let mut source = BusEventSource::from_trait(trait_name);
        let homes = self.registry.trait_homes.read();
        let Some((schema, index)) = homes.get(trait_name) else {
            return source;
        };
        if let Some(orbital) = self
            .registry.schemas
            .read()
            .get(schema)
            .and_then(|s| s.orbitals.get(*index))
        {
            source.orbital = Some(orbital.name.clone());
            source.orbital_id = orbital.id.clone();
            let found = orbital
                .traits
                .iter()
                .find(|r| r.trait_name() == Some(trait_name));
            if let Some(TraitReference::Inline(def)) = found {
                source.trait_id = def.id.clone();
            }
        }
        source
    }

    /// INIT seeds for the traits listed on the pages of every registered program, in program
    /// then page then trait order, each trait once. `keep` selects the pages.
    /// On a host, the listeners a browser runs itself are left to it (EXEC-ENDPOINT-3a): a `[client]` trait a route
    /// page places. A `[client]` trait on no route (a `boot` or `stdio` endpoint) has no browser, so the host runs it.
    pub(super) fn without_browser_listeners(&self, listeners: Vec<crate::listener::ListenerInfo>) -> Vec<crate::listener::ListenerInfo> {
        if self.effect_executor.environment().is_client() {
            return listeners;
        }
        let client_only: HashSet<String> = {
            let defs = self.registry.trait_definitions.read();
            listeners
                .iter()
                .filter(|l| defs.get(&l.trait_name).is_some_and(|d| self.dispatch_mode_of(d) == DispatchMode::HybridClientOnly))
                .map(|l| l.trait_name.clone())
                .collect()
        };
        if client_only.is_empty() {
            return listeners;
        }
        let routed = self.route_page_traits();
        listeners.into_iter().filter(|l| !(client_only.contains(&l.trait_name) && routed.contains(&l.trait_name))).collect()
    }
}
