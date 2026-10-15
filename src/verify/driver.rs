//! Driver — register the schema in `crate::RuntimeKernel` and fire each planned
//! step (hermetic preamble → `process_event`), capturing a [`Frame`].
//!
//! The kernel is the executor under test; the planner's declared `to` is the oracle.

use crate::evaluator::ValueMap;
use crate::verify::frame::{Cause, EffectSummary, Frame, FrameStatus, Variant};
use crate::planner::{self, Step};
use crate::effects::{ClientEffect, Effect, EffectOutcome, EffectResult};
use crate::runtime::{EntitySource, InMemoryEntityCache, KernelConfig, RuntimeKernel};
use crate::{EventResponse, OrbitalSchema, RuntimeEnvironment, TraitDefinition, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

const ENTITY_ID: &str = "verify";

/// A registered schema + its in-memory entity store, ready to drive.
pub struct Harness {
    kernel: RuntimeKernel,
    entities: Arc<InMemoryEntityCache>,
    /// Declared `(entity_key, owner_column)` pairs, captured before `schema` is moved
    /// into the kernel. The synthetic seed row is stamped with these so an ownership
    /// predicate has something to match
    /// (`V-L1-WALKER-SYNTHETIC-ROW-HAS-NO-OWNER-COLUMN`); [`entity_ctx`](Self::entity_ctx)
    /// shares the same set with entity-typed payload-field synthesis.
    owner_fields: Vec<(String, String)>,
}

impl Harness {
    /// Register a resolved schema in a fresh kernel (server environment so persist/fetch
    /// run; render-ui is client-only and passes through for Layer 2).
    pub fn new(schema: OrbitalSchema) -> Result<Self, String> {
        let owner_fields = crate::runtime::owner_fields_from_schema(&schema);
        let entities = Arc::new(InMemoryEntityCache::new());
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            entities.clone() as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel
            .register_schema(schema)
            .map_err(|e| format!("register_schema: {e}"))?;
        Ok(Self { kernel, entities, owner_fields })
    }

    /// Rebind the viewer `@user` resolves to for every subsequent `drive()` call.
    /// One registered schema/kernel serves every persona in a roster — no
    /// re-registration needed (task #129, persona-parameterised verify run).
    pub fn set_persona(&self, user: Value) {
        self.kernel.set_current_user(user);
    }

    /// The full bound viewer identity (not just its id) — `verify_with_harness`
    /// reads and clones this before temporarily rewriting a role for a persist
    /// step whose policy is role-only, then restores it via [`set_persona`]
    /// once the step has driven.
    ///
    /// [`set_persona`]: Harness::set_persona
    pub fn current_persona(&self) -> Value {
        self.kernel.get_current_user()
    }

    /// The current viewer's `id`, if a persona is bound.
    fn viewer_id(&self) -> Option<String> {
        self.kernel
            .get_current_user()
            .as_object()
            .and_then(|u| u.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The id `entity`'s seeded row (and any payload field addressing that SAME row)
    /// should carry. For a self-identity entity — its own `id` is a declared owner
    /// column — the id must equal the bound viewer's, or a self-scoped `@read`/
    /// `@update`/`@delete` policy filters the seeded row out unconditionally; every
    /// other entity keeps the synthetic [`planner::seed_row_id`]. JS twin:
    /// `effectiveSeedId` in
    /// `packages/almadar-verify/src/observer/probe-listen-cascades.ts`.
    pub fn effective_seed_id(&self, entity: &str) -> String {
        planner::effective_seed_id(entity, &self.owner_fields, self.viewer_id().as_deref())
    }

    /// Context for synthesizing entity-typed payload fields against `schema`: this
    /// harness's declared owner columns plus its currently bound viewer, so a
    /// `data : Entity!` field gets a real, ownership-stamped row instead of a bare
    /// `{id}` skeleton. Built fresh per call so it always reflects whatever persona
    /// is bound right now (`set_persona` may have rebound it since the last call).
    pub fn entity_ctx<'a>(&'a self, schema: &'a OrbitalSchema) -> planner::EntityCtx<'a> {
        planner::EntityCtx { schema, owner_fields: &self.owner_fields, viewer_id: self.viewer_id() }
    }

    /// Drive one step with a hermetic preamble (clear entities + reset state + replay to
    /// `step.from`) and capture a frame.
    pub fn drive(&self, td: &TraitDefinition, step: &Step, index: usize, ctx: &planner::EntityCtx<'_>) -> Frame {
        let mut errors = Vec::new();

        // Hermetic preamble.
        self.entities.clear();
        // Seed ONE real row under `effective_seed_id` so a synthesized update/delete
        // payload (which references the identical id) has a real target instead of a
        // placeholder nothing was ever created for
        // (V-L1-SYNTHETIC-ID-INCOMPATIBLE-WITH-MUTATION-POLICY).
        if let Some(entity) = &step.linked_entity {
            let seed_id = self.effective_seed_id(entity);
            let mut row = ValueMap::new();
            row.insert("id".to_string(), Value::String(seed_id));
            // Stamp the declared owner column(s) with the current viewer, exactly as
            // real seeding does (`stamp_owner`). Without this the row carries only
            // `id`, so `(object/get @entity <ownerCol>)` is absent and ANY policy
            // whose every branch requires an ownership match denies unconditionally —
            // an owner-scoped entity with no privileged role could never go green
            // (`V-L1-WALKER-SYNTHETIC-ROW-HAS-NO-OWNER-COLUMN`).
            let mut rows = [Value::Object(row)];
            crate::runtime::stamp_owner(
                &mut rows,
                entity,
                self.viewer_id().as_deref(),
                &self.owner_fields,
            );
            let [row] = rows;
            let _ = self.entities.create(entity, &row);
        }
        // The instance IS the seeded row, so `@entity.id` names a row that exists.
        let instance_id = step
            .linked_entity
            .as_deref()
            .map_or_else(|| ENTITY_ID.to_string(), |e| self.effective_seed_id(e));
        self.kernel.reset_state(&step.trait_name, Some(&instance_id));
        // `AutoInit` only means "already in `from` after reset — no PATH to walk". It does
        // NOT mean no preamble: the real runtime still fires INIT on mount, and a later
        // step legitimately reads what INIT established (e.g. `(set @entity.projectId ?id)`
        // feeding a subsequent persist). Skipping it made that persist resolve an EMPTY id
        // (`V-L1-WALKER-EMPTY-ID-FROM-REPLAY`). Replay for every step EXCEPT INIT itself,
        // which is the step under test.
        if step.variant != Variant::AutoInit || step.event != "INIT" {
            let seed_id = step
                .linked_entity
                .as_deref()
                .map(|e| self.effective_seed_id(e));
            match planner::replay_path(td, &step.from, ctx, seed_id.as_deref()) {
                Some(path) => {
                    for (event, payload) in path {
                        let _ = self.fire(&step.trait_name, &instance_id, &event, &payload);
                    }
                }
                None => errors.push(format!("unreachable preamble state '{}'", step.from)),
            }
        }

        let state_before = self.current_state(&step.trait_name, &instance_id);

        // The step under test.
        let (executed, state_after, effects, deferred_bindings) =
            match self.fire(&step.trait_name, &instance_id, &step.event, &step.payload) {
                Ok(tuple) => tuple,
                Err(e) => {
                    errors.push(format!("process_event: {e}"));
                    (false, state_before.clone(), Vec::new(), Vec::new())
                }
            };

        let entity_counts = self.entity_counts(step.linked_entity.as_deref());

        // A step is only conclusive if the preamble actually placed us in `from`.
        let status = if state_before != step.from {
            FrameStatus::Inconclusive
        } else if step_passes(step, &state_before, &state_after, executed, &effects) {
            FrameStatus::Pass
        } else {
            FrameStatus::Fail
        };

        Frame {
            index,
            cause: Cause {
                trait_name: step.trait_name.clone(),
                from: step.from.clone(),
                event: step.event.clone(),
                to: step.to.clone(),
                variant: step.variant,
            },
            state_before,
            state_after,
            executed,
            payload: step.payload.clone(),
            effects,
            entity_counts,
            deferred_bindings,
            status,
            errors,
        }
    }

    fn fire(
        &self,
        trait_name: &str,
        instance_id: &str,
        event: &str,
        payload: &serde_json::Value,
    ) -> Result<(bool, String, Vec<EffectSummary>, Vec<String>), String> {
        let value: Value = serde_json::from_value(payload.clone()).unwrap_or(Value::Null);
        let result = self
            .kernel
            .process_event(trait_name, Some(instance_id), event, value)
            .map_err(|e| e.to_string())?;
        // Server-effect outcomes (persist/fetch failures) come from the kernel results.
        let effects = result.effect_results.iter().map(summarize_effect).collect();
        // Render-ui binding deferral is measured on the CANONICAL client response, whose
        // render-ui content has `@entity.*` resolved — so only genuinely client-owned
        // bindings (`@trait.*`) remain literal here, matching what the client actually sees.
        let response = EventResponse::from_process_result(trait_name, &result);
        let mut deferred = Vec::new();
        for effect in response.client_effects.iter().flatten() {
            collect_deferred(effect, &mut deferred);
        }
        Ok((result.executed, result.new_state, effects, deferred))
    }

    fn current_state(&self, trait_name: &str, instance_id: &str) -> String {
        self.kernel
            .get_state(trait_name, Some(instance_id))
            .map(|s| s.current_state)
            .unwrap_or_default()
    }

    /// Rows of the linked entity VISIBLE to the walking persona.
    ///
    /// Counting `self.entities.list()` alone answers a different question — the
    /// unfiltered store size, which is identical for every persona — so an
    /// ownership-scoped organism read as symmetric no matter how correct it was
    /// (`S-C2-ASYMMETRY-BLIND-TO-OWNERSHIP-ONLY-SCOPING`). Narrow through the
    /// entity's declared `@read` first, the same pass a fetch runs.
    fn entity_counts(&self, linked: Option<&str>) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        if let Some(entity) = linked {
            let mut rows: Vec<Value> = self
                .entities
                .list(entity)
                .unwrap_or_default()
                .iter()
                .map(|row| (**row).clone())
                .collect();
            self.kernel.retain_readable(entity, &mut rows);
            counts.insert(entity.to_string(), rows.len());
        }
        counts
    }
}

/// Does the step reach its declared target (with no failed effect)? A GUARDED transition
/// that held at `from` under a generic synthesized payload is treated as passing here —
/// the guard legitimately rejected it; the `Inconclusive` status (from a failed preamble)
/// is decided separately by the caller.
fn step_passes(
    step: &Step,
    before: &str,
    after: &str,
    _executed: bool,
    effects: &[EffectSummary],
) -> bool {
    let no_failed = !effects.iter().any(|e| e.outcome == "failed");
    let reached_target = step.to_states.iter().any(|s| s == after);
    no_failed && (reached_target || (step.guarded && after == before))
}

fn summarize_effect(result: &EffectResult) -> EffectSummary {
    let (kind, target) = describe_effect(&result.effect);
    let outcome = match &result.outcome {
        EffectOutcome::Executed => "executed",
        EffectOutcome::NoOp(_) => "noop",
        EffectOutcome::Delegated(_) => "delegated",
        EffectOutcome::Failed => "failed",
    }
    .to_string();
    EffectSummary {
        kind,
        target,
        outcome,
        error: result.error.clone(),
    }
}

fn describe_effect(effect: &Effect) -> (String, String) {
    match effect {
        Effect::RenderUi { slot, .. } => ("render-ui".into(), slot.clone()),
        Effect::Persist { entity_type, .. } => ("persist".into(), entity_type.clone()),
        Effect::Fetch { entity_type, .. } => ("fetch".into(), entity_type.clone()),
        Effect::Emit { event, .. } => ("emit".into(), event.clone()),
        Effect::Set { path, .. } => ("set".into(), path.clone()),
        Effect::Navigate { path, .. } => ("navigate".into(), path.clone()),
        Effect::Log { message, .. } => ("log".into(), message.clone()),
        _ => ("other".into(), String::new()),
    }
}

/// Collect render-ui `@…` binding literals the kernel passed through — the Layer-2
/// resolution obligation (recorded, not failed).
fn collect_deferred(effect: &ClientEffect, out: &mut Vec<String>) {
    if let ClientEffect::RenderUi { config, props, .. } = effect {
        if let Some(config) = config {
            collect_literals(config, out);
        }
        if let Some(props) = props {
            collect_literals(props, out);
        }
    }
}

fn collect_literals(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) if is_binding_literal(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| collect_literals(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_literals(v, out)),
        _ => {}
    }
}

fn is_binding_literal(s: &str) -> bool {
    const ROOTS: &[&str] = &["@trait", "@entity", "@payload", "@config", "@state", "@user"];
    ROOTS.iter().any(|r| s.starts_with(r))
}
