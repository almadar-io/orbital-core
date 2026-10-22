//! Planner — turn a trait's state machine into hermetic per-transition steps.
//!
//! Mirrors `plan-walk.ts`: one step per `(from, event)` with the TS variant set
//! (`success` / `malformed`). The expected target is the **declared** `transition.to`
//! from the resolved `.orb` (an oracle independent of kernel logic). Transient closure
//! of effect-emitted auto-advance is a *client* concern (the kernel fires one event and
//! returns the declared target; the client re-fires emitted follow-ups), so Layer 1 does
//! not need it.

#[cfg(test)]
mod source_provenance_tests {
    use super::*;

    #[test]
    fn named_source_provenance_keeps_expanded_synthesis_and_real_projection() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({"name":"Test","orbitals":[{
            "name":"Thing","entity":{"name":"Thing","persistence":"runtime","fields":[]},"traits":[]
        }]})).unwrap();
        let ctx = EntityCtx { schema: &schema, owner_fields: &[], viewer_id: None };
        let mut field = serde_json::from_value(serde_json::json!({"name":"count","type":"number",
            "projectedFrom":{"typeName":"Thing"}})).unwrap();
        assert!(synth_field(&field, None, &ctx).is_number());
        field.projected_from.as_mut().unwrap().field = Some("id".into());
        assert!(synth_field(&field, None, &ctx).is_string());
    }
}

use serde::{Deserialize, Serialize};
use orbital_core_base::effect_types::{parse_effect, Effect, PersistOperation};
use orbital_core_base::schema::types::{EntityDefinition, EntityRef, OrbitalSchema, TraitDefinition, TraitReference};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_eval::proof::witness::{conform_payload, is_binding, solve_guard, Declared};
pub use orbital_core_eval::proof::witness::const_truth;
pub(crate) use orbital_core_eval::proof::witness::literal_json;

/// Which payload variant of a transition this step exercises.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Variant {
    /// First event into the initial state (no preamble).
    AutoInit,
    /// Full synthesized payload — expect the declared target (or its transient closure).
    Success,
}

/// Which payload a step carries: the full synthesized one, an empty one the event's payload validator
/// must reject, or one that fails the arm's guard.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PayloadCase {
    Success,
    Malformed,
    GuardFail,
}

/// Whether a guarded step's payload satisfies (`pass`) or fails (`fail`) the arm's guard.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuardCase {
    Pass,
    Fail,
}

/// Why a planned payload does not exercise its own arm.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ArmConflict {
    /// The payload passes an earlier arm of the group, which fires instead.
    EarlierArmFirst { arm: usize, to: String },
    /// The payload passes no arm of the group, this one included.
    GuardUnmet,
    /// A `guard-fail` payload still passes the arm's own guard.
    OwnGuardPasses,
    /// A guard could not be evaluated on the payload.
    GuardUnevaluable { arm: usize, message: String },
}

/// Whether a preamble is known to establish what a guard reads.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Proof {
    /// The setter's written value, evaluated on the preamble's payload, satisfies the guard.
    Proven,
    /// The kernel could not show it; a failing step is then inconclusive, not a failure.
    Unproven { reason: UnprovenReason },
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnprovenReason {
    /// The setter's written value could not be evaluated to a field value.
    SetterValueUnresolved,
    /// A same-trait setter exists but the replay path to the step's state does not run it.
    SetterNotOnReplayPath,
    /// The guard reads several unestablished entity fields; only the first is planned.
    MultiplePreconditionsUnplanned,
    /// No transition of this trait or a sibling on the entity sets a satisfying value.
    Unreachable,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindRowFrom {
    pub entity: String,
    #[serde(rename = "payloadField")]
    pub payload_field: String,
}

/// What must hold before a guarded step is fired, for a guard reading `@entity.<field>` that this
/// trait never sets on its own.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Precondition {
    /// Dispatch `event` on `trait` (anchored at `establish_at_state`) before the step.
    Established {
        #[serde(rename = "trait")]
        trait_name: String,
        event: String,
        #[cfg_attr(test, ts(type = "JsonValue"))]
        payload: serde_json::Value,
        #[serde(rename = "establishAtState")]
        establish_at_state: String,
        #[serde(rename = "bindRowFrom", skip_serializing_if = "Option::is_none")]
        bind_row_from: Option<BindRowFrom>,
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<crate::verify::plan_wire::RoleWire>,
        /// Further unestablished guard fields this preamble does not cover.
        #[serde(rename = "unplannedFields", default, skip_serializing_if = "Vec::is_empty")]
        unplanned_fields: Vec<String>,
        proof: Proof,
    },
    /// The trait's own machine sets the field, but not on the replay path to the step's state.
    SameTraitSetter { fields: Vec<String>, proof: Proof },
    /// No setter satisfies the guard.
    Unreachable { fields: Vec<String> },
}

impl Precondition {
    /// Why the step's precondition is not shown to hold, when it is not.
    pub fn gap(&self) -> Option<UnprovenReason> {
        match self {
            Precondition::Established { unplanned_fields, .. } if !unplanned_fields.is_empty() => {
                Some(UnprovenReason::MultiplePreconditionsUnplanned)
            }
            Precondition::Established { proof: Proof::Unproven { reason }, .. } => Some(*reason),
            Precondition::Established { .. } => None,
            Precondition::SameTraitSetter { proof: Proof::Unproven { reason }, .. } => Some(*reason),
            Precondition::SameTraitSetter { .. } => None,
            Precondition::Unreachable { .. } => Some(UnprovenReason::Unreachable),
        }
    }
}

/// Which route of an `http` effect an event is.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplyRoute {
    /// `emit.success`, `emit.cancelled` or `emit.onMessage`.
    Other,
    /// `emit.failure`.
    Failure,
}

/// A `--trait` name no trait in the program carries.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("unknown --trait {name:?} — available traits: {available}")]
pub struct UnknownTrait {
    pub name: String,
    pub available: String,
}

/// One planned step: drive `event` from `from`, expecting any state in `to_states`.
#[derive(Debug, Clone)]
pub struct Step {
    pub trait_name: String,
    pub linked_entity: Option<String>,
    pub from: String,
    pub event: String,
    /// Primary declared target (for display).
    pub to: String,
    /// All declared targets for this `(from, event)` — multiple when sibling transitions
    /// share an event (guard/priority decides the winner at runtime), so the kernel landing
    /// on ANY of them is correct.
    pub to_states: Vec<String>,
    pub variant: Variant,
    pub payload: serde_json::Value,
    /// Any transition in the `(from, event)` group carries a guard. A guarded group that
    /// holds at `from` under a generic synthesized payload is *inconclusive*, not a failure.
    pub guarded: bool,
    /// For a guarded group, the position of this step's arm among the `from --event-->` arms in
    /// declaration order (the index the kernel reports as `fired_arm`); `None` for an unguarded group.
    pub arm: Option<usize>,
    /// The arm's guard was turned into a payload that satisfies it (always `true` when `arm` is `None`).
    pub solved: bool,
    /// The event is a declared reply route of an `http` effect: the step is never driven (the reply arrives
    /// inside the request's own dispatch).
    pub reply: Option<ReplyRoute>,
    pub payload_case: PayloadCase,
    /// Set on the `pass` and `fail` variants of a guarded arm.
    pub guard_case: Option<GuardCase>,
    /// Set with `guard_case`: every binding of the guard is a `@payload` path, so the payload can steer it.
    pub guard_steerable: Option<bool>,
    /// On a `fail` variant: the targets of the sibling arms of the `(from, event)` group that may fire instead.
    pub guard_sibling_targets: Vec<String>,
    /// The guard folds to this constant, so one of its two cases cannot be driven.
    pub const_guard: Option<bool>,
    pub arm_conflict: Option<ArmConflict>,
    pub precondition: Option<Precondition>,
}

/// All inlined trait definitions in a resolved schema (post-`uses` resolution they are
/// all `TraitReference::Inline`).
pub fn inline_traits(schema: &OrbitalSchema) -> Vec<&TraitDefinition> {
    let mut out = Vec::new();
    for orbital in &schema.orbitals {
        for tr in &orbital.traits {
            if let TraitReference::Inline(td) = tr {
                out.push(td);
            }
        }
    }
    out
}

/// `--trait NAME` must resolve both spellings a trait can carry post-resolve: the
/// compiled, possibly orbital-prefixed name (`td.name`, e.g. `ChatMessageOrbitalChatComposer`)
/// AND the bare name it was authored/imported under (`td.source_behavior.original_name`,
/// e.g. `ChatComposer`) — declared provenance, not a name heuristic.
pub fn trait_matches_name(td: &TraitDefinition, name: &str) -> bool {
    td.name == name
        || td
            .source_behavior
            .as_ref()
            .is_some_and(|s| s.original_name == name)
}

/// The name to show for `td` in an error/available-traits listing: its bare authored
/// name when composed from an import, else its own (already-bare) resolved name.
fn trait_display_name(td: &TraitDefinition) -> &str {
    td.source_behavior
        .as_ref()
        .map(|s| s.original_name.as_str())
        .unwrap_or(td.name.as_str())
}

/// Resolve `--trait` names against every inline trait in `schema`, returning the
/// CANONICAL resolved (`td.name`) set, deduplicated, in first-match order. A name
/// matching no trait is an error listing every available trait name — never a
/// silent no-op.
pub fn resolve_trait_names(schema: &OrbitalSchema, names: &[String]) -> Result<Vec<String>, UnknownTrait> {
    let all = inline_traits(schema);
    let mut resolved: Vec<String> = Vec::new();
    for name in names {
        let mut found = false;
        for td in &all {
            if trait_matches_name(td, name) {
                found = true;
                if !resolved.iter().any(|r| r == &td.name) {
                    resolved.push(td.name.clone());
                }
            }
        }
        if !found {
            let mut available: Vec<&str> = all.iter().map(|td| trait_display_name(td)).collect();
            available.sort_unstable();
            available.dedup();
            return Err(UnknownTrait { name: name.clone(), available: available.join(", ") });
        }
    }
    Ok(resolved)
}

/// Every inline trait in `schema` whose resolved name is in `resolved_names` — `None`
/// (no `--trait` filter) keeps every trait, matching today's full walk.
pub fn inline_traits_scoped<'a>(
    schema: &'a OrbitalSchema,
    resolved_names: Option<&[String]>,
) -> Vec<&'a TraitDefinition> {
    let all = inline_traits(schema);
    match resolved_names {
        None => all,
        Some(names) => all
            .into_iter()
            .filter(|td| names.iter().any(|n| n == &td.name))
            .collect(),
    }
}

/// The trait's initial state (`is_initial`, else the first declared state).
pub fn initial_state(td: &TraitDefinition) -> Option<String> {
    let sm = &td.state_machine;
    sm.states
        .iter()
        .find(|s| s.is_initial.unwrap_or(false))
        .or_else(|| sm.states.first())
        .map(|s| s.name.clone())
}

/// Stable, predictable id for the ONE row [`crate::driver::Harness::drive`]
/// seeds into a trait's linked entity before each step — lets a synthesized
/// update/delete payload reference a REAL row instead of a placeholder
/// nothing was ever created for
/// (V-L1-SYNTHETIC-ID-INCOMPATIBLE-WITH-MUTATION-POLICY). The driver seeds
/// under this exact id; `synth_field` below must derive the identical value.
pub fn seed_row_id(linked_entity: &str) -> String {
    format!("{linked_entity}-verify-seed-1")
}

/// The id `linked_entity`'s seeded row (and any payload field addressing that SAME
/// row) should carry. For a self-identity entity — `owner_fields` declares its own
/// `id` as an owner column — the id must equal the bound viewer's, or a self-scoped
/// `@read`/`@update`/`@delete` policy filters the seeded row out unconditionally
/// (the row's own id IS the ownership key); every other entity keeps the synthetic
/// [`seed_row_id`], which the viewer never coincidentally matches. JS twin:
/// `effectiveSeedId` in
/// `packages/almadar-verify/src/observer/probe-listen-cascades.ts`.
pub fn effective_seed_id(
    linked_entity: &str,
    owner_fields: &[(String, String)],
    viewer_id: Option<&str>,
) -> String {
    let entity_key = linked_entity.to_lowercase();
    let owns_own_id = owner_fields
        .iter()
        .any(|(entity, field)| *entity == entity_key && field == "id");
    match (owns_own_id, viewer_id) {
        (true, Some(id)) => id.to_string(),
        _ => seed_row_id(linked_entity),
    }
}

/// The id a `persist create` of `entity` carries: never the seeded row's, or the store refuses the
/// duplicate. A self-identity entity (its own `id` is a declared owner column) is created under the
/// viewer's id, exactly as [`effective_seed_id`] derives it.
pub fn new_row_id(entity: &str, owner_fields: &[(String, String)], viewer_id: Option<&str>) -> String {
    let entity_key = entity.to_lowercase();
    let owns_own_id = owner_fields.iter().any(|(e, field)| *e == entity_key && field == "id");
    match (owns_own_id, viewer_id) {
        (true, Some(id)) => id.to_string(),
        _ => format!("{entity}-verify-new-1"),
    }
}

/// Whether `entity`'s own `id` is a declared owner column: its rows are keyed by the viewer's id.
pub fn is_self_identity(entity: &str, owner_fields: &[(String, String)]) -> bool {
    let entity_key = entity.to_lowercase();
    owner_fields.iter().any(|(e, field)| *e == entity_key && field == "id")
}

/// Entities `event` creates and never updates, deletes or upserts: the ones whose seeded row a create
/// payload must not address.
pub fn created_only_entities(td: &TraitDefinition, event: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for t in td.state_machine.transitions.iter().filter(|t| t.event == event) {
        for eff in t.effects.iter().flatten() {
            collect_persist_targets(eff, &mut targets);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for (entity, _) in &targets {
        let only_creates = targets
            .iter()
            .filter(|(e, _)| e == entity)
            .all(|(_, o)| *o == PersistOperation::Create);
        if only_creates && !out.contains(entity) {
            out.push(entity.clone());
        }
    }
    out
}

fn replace_id(value: &mut serde_json::Value, from: &str, to: &str) {
    match value {
        serde_json::Value::String(s) if s == from => *s = to.to_string(),
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| replace_id(v, from, to)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|v| replace_id(v, from, to)),
        _ => {}
    }
}

/// Every entity name the schema declares (primary + auxiliary, inline or ref).
///
/// A `PayloadField`'s `field_type` is a free STRING, not the `FieldType` enum an
/// entity field uses — an entity-typed payload field arrives as the literal name
/// (`data : Patient!` → `"Patient"`). So "is this field an entity?" can only be
/// answered against the schema's declared names; there is no canonical helper to
/// reuse, and matching a `*Id`/`*Entity` naming convention instead would be the
/// name-trick heuristic the project forbids.
pub fn entity_names(schema: &OrbitalSchema) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for orbital in &schema.orbitals {
        let mut refs = vec![&orbital.entity];
        if let Some(aux) = &orbital.auxiliary_entities {
            refs.extend(aux.iter());
        }
        for er in refs {
            // Post-resolve every entity is inline; the ref/extends forms are only
            // present pre-resolution and carry no field shape to synthesize from.
            if let orbital_core_base::schema::types::EntityRef::Inline(def) = er {
                out.insert(def.name.clone());
            }
        }
    }
    out
}

/// Payload fields to synthesize for an event: every declared field, required or
/// not — parity with the TS walker (`synthesizeSuccessPayload` →
/// `buildMinimalPayload`). A `required`-only filter sent a loaded event's optional
/// `data` as `{}`, so a transition reading `@payload.data` failed its `set`.
fn fields_to_synthesize<'a>(td: &'a TraitDefinition, event: &str) -> Vec<&'a orbital_core_base::schema::types::PayloadField> {
    let Some(events) = &td.state_machine.events else {
        return Vec::new();
    };
    let Some(def) = events.iter().find(|e| e.key == event) else {
        return Vec::new();
    };
    let Some(schema) = &def.payload_schema else {
        return Vec::new();
    };
    schema.iter().collect()
}

/// Payload field names this trait's transitions feed into `@entity.id` for `event`.
///
/// `(set @entity.id ?entityId)` lowers to `["set", "@entity.id", "@payload.entityId"]`
/// — the transition DECLARES which payload field is the row key, whatever it is
/// called. Reading it here is what lets a key named `entityId`/`policyId`/… get the
/// same seeded row id a field literally named `id` gets; matching an `*Id` name
/// suffix instead would be the name-trick heuristic the project forbids
/// (V-WALKER-SYNTH-ID-MISSES-NON-`id`-ROW-KEYS).
fn row_key_fields(td: &TraitDefinition, event: &str) -> std::collections::BTreeSet<String> {
    use orbital_core_base::schema::sexpr::SExpression as S;
    let mut out = std::collections::BTreeSet::new();
    for t in &td.state_machine.transitions {
        if t.event != event {
            continue;
        }
        for eff in t.effects.iter().flatten() {
            let S::Array(items) = eff else { continue };
            let [S::String(op), S::String(target), S::String(src)] = &items[..] else {
                continue;
            };
            if op == "set" && target == "@entity.id" {
                if let Some(field) = src.strip_prefix("@payload.") {
                    out.insert(field.to_string());
                }
            }
        }
    }
    out
}

/// Every `(entity, operation)` a `persist` effect performs inside the
/// `(from, event)` transition group — walked through every combinator
/// (`do`/`when`/`if`/…) via the shared `crate::effects::parse_effect`
/// parser, the same one every other consumer of an effect S-expression uses,
/// rather than a second hand-rolled persist matcher. Lets
/// `orbital-verify::verify_with_harness` derive the role a role-only persist
/// policy needs BEFORE driving the step (`entity_access::role_satisfying_policy`),
/// instead of only discovering the denial after.
pub fn persist_targets(
    td: &TraitDefinition,
    from: &str,
    event: &str,
) -> Vec<(String, PersistOperation)> {
    let mut out = Vec::new();
    for t in &td.state_machine.transitions {
        if t.event != event || !t.from.matches(from) {
            continue;
        }
        for eff in t.effects.iter().flatten() {
            collect_persist_targets(eff, &mut out);
        }
    }
    out
}

fn collect_persist_targets(expr: &SExpression, out: &mut Vec<(String, PersistOperation)>) {
    if let Some(Effect::Persist {
        operation,
        entity_type,
        ..
    }) = parse_effect(expr)
    {
        let pair = (entity_type, operation);
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    if let SExpression::Array(items) = expr {
        for item in items {
            collect_persist_targets(item, out);
        }
    }
}

/// Everything [`synth_field`] needs to fully materialize an entity-typed payload
/// field as a real row rather than a bare `{id}` skeleton: the schema (to look up
/// the named entity's own declared fields), the declared `(entity, owner_column)`
/// pairs (identical set the real seeder and the driver's own-row preamble stamp),
/// and the bound viewer (`None` at the schema-level, persona-less enumeration in
/// [`transitions_with_non_main_slots`]).
pub struct EntityCtx<'a> {
    pub schema: &'a OrbitalSchema,
    pub owner_fields: &'a [(String, String)],
    pub viewer_id: Option<String>,
}

/// The inline declared definition of `name`, searched across every orbital's
/// primary + auxiliary entities. `None` for a name the resolved schema never
/// inlines (a dangling/ref-only entity reference).
pub(crate) fn entity_def<'a>(schema: &'a OrbitalSchema, name: &str) -> Option<&'a EntityDefinition> {
    for orbital in &schema.orbitals {
        let mut refs = vec![&orbital.entity];
        if let Some(aux) = &orbital.auxiliary_entities {
            refs.extend(aux.iter());
        }
        for er in refs {
            if let EntityRef::Inline(def) = er {
                if def.name == name {
                    return Some(def);
                }
            }
        }
    }
    None
}

/// Synthesize a real row of `entity_name`: every non-intrinsic declared field via
/// the canonical sample synthesizer ([`crate::runtime::sample_rows`] — the
/// SAME one real seeding uses, so an entity-typed payload field looks like a row
/// the app could actually have persisted), the seeded id, and the viewer stamped
/// onto whatever owner columns this entity's own access policies declare
/// ([`crate::runtime::stamp_owner`]). Without the owner stamp a create
/// policy comparing a field to `@user.id` denies unconditionally — no synthesized
/// field ever coincidentally equals the viewer — and without the seeded id a
/// following event addressing `?id` resolves no row key.
pub(crate) fn synth_entity_row(ctx: &EntityCtx<'_>, entity_name: &str) -> serde_json::Value {
    let seed_id = effective_seed_id(entity_name, ctx.owner_fields, ctx.viewer_id.as_deref());
    let Some(def) = entity_def(ctx.schema, entity_name) else {
        // A ref-only/unresolved entity — the seeded id is still the most useful
        // placeholder a caller can act on.
        return serde_json::json!({ "id": seed_id });
    };
    let mut rows = crate::seed::sample_rows(def, 1);
    let Some(row) = rows.first_mut() else {
        return serde_json::json!({ "id": seed_id });
    };
    if let orbital_core_base::value::Value::Object(map) = row {
        map.insert("id".to_string(), orbital_core_base::value::Value::String(seed_id.clone()));
    }
    let mut stamped = [row.clone()];
    orbital_core_base::policy::stamp_owner(
        &mut stamped,
        entity_name,
        ctx.viewer_id.as_deref(),
        ctx.owner_fields,
    );
    let [stamped_row] = stamped;
    serde_json::to_value(&stamped_row).unwrap_or_else(|_| serde_json::json!({ "id": seed_id }))
}

/// Synthesize a full payload satisfying an event's required (+ `id`) fields.
/// `seed_id` — the caller-resolved [`effective_seed_id`] of `td.linked_entity`,
/// `None` when the trait links no entity — is the row a row-key field or an
/// `id`-named field must reference, so it stays in step with whatever id the
/// driver actually seeded (self-identity entities key their row by the VIEWER's
/// id, not the synthetic [`seed_row_id`]).
pub fn synth_payload(
    td: &TraitDefinition,
    event: &str,
    ctx: &EntityCtx<'_>,
    seed_id: Option<&str>,
) -> serde_json::Value {
    let row_keys = row_key_fields(td, event);
    let mut obj = serde_json::Map::new();
    for f in fields_to_synthesize(td, event) {
        // A declared row key gets the seeded id regardless of what it is named.
        let value = match (row_keys.contains(&f.name), seed_id) {
            (true, Some(id)) => serde_json::Value::from(id),
            _ => synth_field(f, seed_id, ctx),
        };
        obj.insert(f.name.clone(), value);
    }
    let mut payload = serde_json::Value::Object(obj);
    for entity in created_only_entities(td, event) {
        let seeded = effective_seed_id(&entity, ctx.owner_fields, ctx.viewer_id.as_deref());
        let fresh = new_row_id(&entity, ctx.owner_fields, ctx.viewer_id.as_deref());
        replace_id(&mut payload, &seeded, &fresh);
    }
    payload
}

/// A nested object contract built from every declared property (TS `buildMinimalPayload`),
/// carrying the seeded row id so `(persist update E ?data)` finds `data.id`.
fn synth_object(
    f: &orbital_core_base::schema::types::PayloadField,
    seed_id: Option<&str>,
    ctx: &EntityCtx<'_>,
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for p in f.properties.as_deref().unwrap_or(&[]) {
        obj.insert(p.name.clone(), synth_field(p, seed_id, ctx));
    }
    if let Some(id) = seed_id {
        obj.entry("id".to_string())
            .or_insert_with(|| serde_json::Value::from(id));
    }
    serde_json::Value::Object(obj)
}

pub(crate) fn synth_field(
    f: &orbital_core_base::schema::types::PayloadField,
    seed_id: Option<&str>,
    ctx: &EntityCtx<'_>,
) -> serde_json::Value {
    let field_type = f.field_type.as_str();
    let field_name = f.name.as_str();
    // An ENTITY-typed payload field is a row, exactly like the anonymous `object`
    // form below — it just names (or, once flattened, carries `entity: Some(name)`
    // alongside `type: "object"`/`"[object]"` + `properties`) the entity instead of
    // being purely structural. `f.entity` is the ONLY source of entity identity for a
    // composed/inlined field (`PayloadField.entity`'s own contract — never inferred
    // from shape or name); a bare field whose `type` literally IS a declared entity
    // name (hand-authored/pre-flatten `.orb`) is the other legitimate spelling.
    // Without this it fell to the `"verify"`/`{}` placeholder, so `(persist update E
    // ?data)` found no `data.id`, resolved an EMPTY id, and the access-policy
    // pre-read 404'd on a correct app (V-WALKER-SYNTH-ID-MISSES-NON-`id`-ROW-KEYS) —
    // or, for a `@create` policy comparing another field to `@user.id`, denied
    // outright because only `{id}` existed to compare against.
    // `T.f`: the value (each element's, for an array) is `f` of `T`'s seeded row.
    if let Some((projection, source_field)) = f.projected_from.as_ref()
        .and_then(|projection| projection.field.as_deref().map(|field| (projection, field))) {
        if entity_def(ctx.schema, &projection.type_name).is_some() {
            let row = synth_entity_row(ctx, &projection.type_name);
            let value = row.get(source_field).cloned().unwrap_or(serde_json::Value::Null);
            return if field_type.starts_with('[') { serde_json::Value::Array(vec![value]) } else { value };
        }
    }
    let entity_name = f.entity.as_deref().or_else(|| {
        entity_def(ctx.schema, field_type).is_some().then_some(field_type)
    });
    if let Some(entity_name) = entity_name {
        return if field_type.starts_with('[') {
            serde_json::Value::Array(vec![synth_entity_row(ctx, entity_name)])
        } else {
            synth_entity_row(ctx, entity_name)
        };
    }
    match field_type {
        "number" | "int" | "integer" | "float" => serde_json::json!(1),
        "boolean" | "bool" => serde_json::json!(true),
        // A `data`-shaped payload field for update/delete must carry a real
        // row id NESTED inside, mirroring how `(persist update Entity ?data)`
        // reads the target id from `data.id` when no separate id arg is
        // given (`crate::effects::executor::resolve_update_id`). A
        // declared nested contract (`data : { content!, channel! }`) is built
        // from its own properties — required leaves and `id` — so the persist
        // behind it sees every required column.
        "object" => synth_object(f, seed_id, ctx),
        // A flattened array (`[object]` + element properties): the properties describe ONE
        // element, so the value is a one-row array — as the TS walker builds it.
        "[object]" if f.properties.as_deref().is_some_and(|p| !p.is_empty()) => {
            serde_json::Value::Array(vec![synth_object(f, seed_id, ctx)])
        }
        // Semantic domains must synthesize a value their own validator accepts —
        // the emitted zod carries `.email()`/`.url()`/`.uuid()` refinements now, so
        // a bare "verify" placeholder would make the verifier fail on a correct app.
        "email" => serde_json::json!("verify@example.com"),
        "url" | "image" => serde_json::json!("https://example.com/verify"),
        "phone" => serde_json::json!("+1-555-0100"),
        "uuid" => serde_json::json!("00000000-0000-4000-8000-000000000001"),
        t if t.starts_with('[') => serde_json::json!([]),
        // An `id`-named field is the entity's own row id — synthesize the same
        // seeded id the driver planted, not a value nothing was ever created
        // under.
        _ if field_name == "id" => {
            seed_id.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("verify"))
        }
        // A named type that is not an entity (a union of record variants) has no placeholder: a string would
        // stand where a record is declared.
        t if t.chars().next().is_some_and(char::is_uppercase) => serde_json::Value::Null,
        // string + other entity-typed fields → a placeholder string id.
        _ => serde_json::json!("verify"),
    }
}

/// Plan every `(from, event)` transition group in a trait, as the steps a native walk drives: the
/// success variant of each unguarded group and the guard-satisfying variant of each guarded arm.
/// [`plan_trait_variants`] adds the variants only a host that also checks rejection drives.
pub fn plan_trait(
    td: &TraitDefinition,
    ctx: &EntityCtx<'_>,
    seed_id: Option<&str>,
) -> Vec<Step> {
    plan_trait_variants(td, ctx, seed_id)
        .into_iter()
        .filter(|s| s.payload_case == PayloadCase::Success && s.guard_case != Some(GuardCase::Fail))
        .collect()
}

/// Plan every `(from, event)` transition group in a trait with all its variants. Sibling transitions
/// sharing an unguarded event collapse into one step whose accept-set is the union of their targets; a
/// guarded group is planned arm by arm. Per arm: `malformed` (empty payload, when the event declares a
/// required field), `pass` (a payload its guard accepts) and `fail` (one it rejects). `seed_id` is the
/// caller-resolved [`effective_seed_id`] of `td.linked_entity` (`None` when the trait links no entity).
pub fn plan_trait_variants(
    td: &TraitDefinition,
    ctx: &EntityCtx<'_>,
    seed_id: Option<&str>,
) -> Vec<Step> {
    let initial = initial_state(td);

    let mut groups: Vec<((String, String), Vec<&orbital_core_base::schema::types::TransitionDefinition>)> = Vec::new();
    for t in &td.state_machine.transitions {
        for from in t.from.states() {
            let key = (from.to_string(), t.event.clone());
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, arms)) => arms.push(t),
                None => groups.push((key, vec![t])),
            }
        }
    }

    let reply_routes = http_reply_routes(ctx.schema);
    let types = crate::verify::payload_solve::declared_types(ctx.schema);
    let mut steps = Vec::new();
    for ((from, event), arms) in groups {
        let variant = if initial.as_deref() == Some(from.as_str()) {
            Variant::AutoInit
        } else {
            Variant::Success
        };
        let mut to_states: Vec<String> = Vec::new();
        for t in &arms {
            if !to_states.contains(&t.to) {
                to_states.push(t.to.clone());
            }
        }
        let guarded = arms.iter().any(|t| t.guard.is_some());
        let base = synth_payload(td, &event, ctx, seed_id);
        let required = has_required_payload_field(td, &event);
        let declared = declared_for(td, &event, &base, seed_id, ctx);
        let overlay = |payload: &serde_json::Value, t: &orbital_core_base::schema::types::TransitionDefinition| {
            let mut out = payload.clone();
            for (field, text) in crate::verify::payload_solve::json_parse_payload(t, &types, ctx, seed_id) {
                out[field] = text;
            }
            out
        };
        let reply = reply_routes.get(&(td.name.clone(), event.clone())).copied();
        let make = |arm: Option<usize>,
                    solved: bool,
                    to: String,
                    to_states: Vec<String>,
                    payload: serde_json::Value,
                    payload_case: PayloadCase,
                    guard_case: Option<GuardCase>,
                    guard_steerable: Option<bool>,
                    guard_sibling_targets: Vec<String>,
                    const_guard: Option<bool>| Step {
            arm_conflict: None,
            precondition: None,
            trait_name: td.name.clone(),
            linked_entity: td.linked_entity.clone(),
            from: from.clone(),
            event: event.clone(),
            to,
            to_states,
            variant,
            payload,
            guarded,
            arm,
            solved,
            reply,
            payload_case,
            guard_case,
            guard_steerable,
            guard_sibling_targets,
            const_guard,
        };
        if !guarded {
            let base = arms.iter().fold(base.clone(), |acc, t| overlay(&acc, t));
            if required {
                steps.push(make(
                    None,
                    true,
                    to_states.first().cloned().unwrap_or_default(),
                    to_states.clone(),
                    serde_json::json!({}),
                    PayloadCase::Malformed,
                    None,
                    None,
                    Vec::new(),
                    None,
                ));
            }
            steps.push(make(
                None,
                true,
                to_states.first().cloned().unwrap_or_default(),
                to_states.clone(),
                base,
                PayloadCase::Success,
                None,
                None,
                Vec::new(),
                None,
            ));
            continue;
        }
        for (position, t) in arms.iter().enumerate() {
            let constant = t.guard.as_ref().and_then(const_truth);
            let steerable = t.guard.as_ref().map(guard_is_payload_steerable);
            if required && constant != Some(true) {
                steps.push(make(
                    Some(position),
                    true,
                    t.to.clone(),
                    vec![t.to.clone()],
                    serde_json::json!({}),
                    PayloadCase::Malformed,
                    None,
                    None,
                    Vec::new(),
                    constant,
                ));
            }
            let base_arm = overlay(&base, t);
            let has_json = crate::verify::payload_solve::has_json_sites(t, &types);
            let conformed = t.guard.as_ref().map_or(Some(base_arm.clone()), |guard| conform_payload(guard, &base_arm, &declared));
            let (pass, pass_solved) = if has_json {
                let start = conformed.unwrap_or_else(|| base_arm.clone());
                let solved = crate::verify::payload_solve::solve_json_parse_fields(t, true, &start, &types, &from, ctx, seed_id);
                let ok = t.guard.as_ref().is_none_or(|g| guard_passes(g, &solved, &from) == Ok(true));
                (Some(solved), ok)
            } else {
                let ok = conformed.is_some();
                (conformed, ok)
            };
            let pass_conflict = match (&pass, steerable) {
                (Some(payload), Some(true)) => pass_arm_conflict(&arms, position, payload, &from),
                _ => None,
            };
            steps.push(make(
                Some(position),
                pass_solved && pass_conflict.is_none(),
                t.to.clone(),
                vec![t.to.clone()],
                pass.unwrap_or_else(|| base_arm.clone()),
                PayloadCase::Success,
                Some(GuardCase::Pass),
                steerable,
                Vec::new(),
                constant,
            ));
            if let Some(last) = steps.last_mut() {
                last.arm_conflict = pass_conflict;
                last.precondition = crate::verify::precondition::plan_precondition(ctx, td, t, &from, &last.payload);
            }
            let fail_symbolic = t.guard.as_ref().and_then(|guard| {
                let mut payload = base_arm.clone();
                solve_guard(guard, false, &mut payload, &declared).then_some(payload)
            });
            let (fail, fail_solved) = if has_json {
                let start = fail_symbolic.unwrap_or_else(|| base_arm.clone());
                let solved = crate::verify::payload_solve::solve_json_parse_fields(t, false, &start, &types, &from, ctx, seed_id);
                let ok = t.guard.as_ref().is_none_or(|g| guard_passes(g, &solved, &from) == Ok(false));
                (Some(solved), ok)
            } else {
                let ok = fail_symbolic.is_some();
                (fail_symbolic, ok)
            };
            let mut siblings: Vec<String> = arms.iter().filter(|o| o.to != t.to).map(|o| o.to.clone()).collect();
            let mut fail_conflict = None;
            if let (Some(payload), Some(true), true) = (&fail, steerable, fail_solved) {
                match first_passing_arm(&arms, payload, &from) {
                    Ok(Some(i)) if i == position => fail_conflict = Some(ArmConflict::OwnGuardPasses),
                    Ok(Some(i)) => {
                        if !siblings.contains(&arms[i].to) {
                            siblings.push(arms[i].to.clone());
                        }
                    }
                    Ok(None) | Err(_) => {}
                }
            }
            steps.push(make(
                Some(position),
                fail_solved && fail_conflict.is_none(),
                t.to.clone(),
                vec![t.to.clone()],
                fail.unwrap_or_else(|| base_arm.clone()),
                PayloadCase::GuardFail,
                Some(GuardCase::Fail),
                steerable,
                siblings,
                constant,
            ));
            if let Some(last) = steps.last_mut() {
                last.arm_conflict = fail_conflict;
            }

        }
    }
    steps
}

pub(crate) fn guard_passes(guard: &SExpression, payload: &serde_json::Value, from: &str) -> Result<bool, String> {
    let value: orbital_core_base::value::Value = serde_json::from_value(payload.clone()).map_err(|e| e.to_string())?;
    let mut ctx = orbital_core_eval::evaluator::context::BindingContextBuilder::new()
        .payload(value)
        .state(orbital_core_base::value::Value::String(from.to_string()))
        .build();
    ctx.from_state = Some(from.to_string());
    orbital_core_eval::evaluator::Evaluator::new()
        .evaluate_guard(guard, &ctx)
        .map_err(|e| e.to_string())
}

fn arm_guard_passes(
    arm: &orbital_core_base::schema::types::TransitionDefinition,
    payload: &serde_json::Value,
    from: &str,
) -> Result<bool, String> {
    match &arm.guard {
        None => Ok(true),
        Some(guard) => guard_passes(guard, payload, from),
    }
}

/// The first arm of the group, in declaration order, whose guard the real evaluator passes on `payload`:
/// the arm the kernel fires. `Err` carries the arm whose guard could not be evaluated.
fn first_passing_arm(
    arms: &[&orbital_core_base::schema::types::TransitionDefinition],
    payload: &serde_json::Value,
    from: &str,
) -> Result<Option<usize>, (usize, String)> {
    for (i, arm) in arms.iter().enumerate() {
        if arm_guard_passes(arm, payload, from).map_err(|e| (i, e))? {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

/// Whether `payload`, solved from arm `position`'s guard, would fire that arm rather than an earlier one.
fn pass_arm_conflict(
    arms: &[&orbital_core_base::schema::types::TransitionDefinition],
    position: usize,
    payload: &serde_json::Value,
    from: &str,
) -> Option<ArmConflict> {
    match first_passing_arm(&arms[..=position], payload, from) {
        Err((arm, message)) => Some(ArmConflict::GuardUnevaluable { arm, message }),
        Ok(Some(i)) if i == position => None,
        Ok(Some(i)) => Some(ArmConflict::EarlierArmFirst { arm: i, to: arms[i].to.clone() }),
        Ok(None) => Some(ArmConflict::GuardUnmet),
    }
}

fn has_required_payload_field(td: &TraitDefinition, event: &str) -> bool {
    td.state_machine
        .events
        .iter()
        .flatten()
        .find(|e| e.key == event)
        .and_then(|e| e.payload_schema.as_ref())
        .is_some_and(|fields| fields.iter().any(|f| f.required == Some(true)))
}

pub(crate) fn collect_binding_texts<'a>(expr: &'a SExpression, out: &mut Vec<&'a str>) {
    match expr {
        SExpression::String(s) if is_binding(s) => out.push(s),
        SExpression::Array(items) => items.iter().for_each(|i| collect_binding_texts(i, out)),
        SExpression::Object(map) => map.values().for_each(|v| collect_binding_texts(v, out)),
        _ => {}
    }
}

/// Every binding the guard references is a `@payload` path: the only input a planned payload controls.
/// A guard with no binding is a folded constant, which nothing in the payload can move.
fn guard_is_payload_steerable(guard: &SExpression) -> bool {
    let mut bindings = Vec::new();
    collect_binding_texts(guard, &mut bindings);
    !bindings.is_empty() && bindings.iter().all(|b| b.starts_with("@payload"))
}

pub(crate) fn collect_emitted(node: &SExpression, found: &mut impl FnMut(String)) {
    match node {
        SExpression::Array(items) => items.iter().for_each(|i| collect_emitted(i, found)),
        SExpression::Object(map) => {
            if let Some(SExpression::Object(emit)) = map.get("emit") {
                for (key, value) in emit {
                    if key == "onMessage" {
                        continue;
                    }
                    if let SExpression::String(event) = value {
                        if !event.is_empty() {
                            found(event.clone());
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Every event an effect of this trait emits on its outcome (a fetch's success or failure callback, a
/// persist's, a call's): events the program fires itself, never a user.
pub fn effect_emitted_events(td: &TraitDefinition) -> std::collections::BTreeSet<String> {
    effect_emitted_events_in_order(td).into_iter().collect()
}

/// [`effect_emitted_events`] in the order the effects first name them.
pub fn effect_emitted_events_in_order(td: &TraitDefinition) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in &td.state_machine.transitions {
        t.effects.iter().flatten().for_each(|e| {
            collect_emitted(e, &mut |event| {
                if !out.contains(&event) {
                    out.push(event);
                }
            })
        });
    }
    out
}

/// `start` and every state reachable from it by transitions whose event an effect emits: a state with such
/// an outgoing transition advances on its own the instant the effect resolves, so a host that observes the
/// settled state may see any of them. `[start]` when the trait emits no event from an effect.
pub fn transient_closure(td: &TraitDefinition, start: &str) -> Vec<String> {
    let emitted = effect_emitted_events(td);
    if emitted.is_empty() {
        return vec![start.to_string()];
    }
    let mut seen = vec![start.to_string()];
    let mut queue = std::collections::VecDeque::from([start.to_string()]);
    while let Some(state) = queue.pop_front() {
        for t in &td.state_machine.transitions {
            if emitted.contains(&t.event) && t.from.matches(&state) && !seen.contains(&t.to) {
                seen.push(t.to.clone());
                queue.push_back(t.to.clone());
            }
        }
    }
    seen
}

fn effects_navigate(effects: &Option<Vec<SExpression>>) -> bool {
    effects.iter().flatten().any(|e| {
        matches!(e, SExpression::Array(items)
            if matches!(items.first(), Some(SExpression::String(op)) if op == "navigate" || op == "navigate-back"))
    })
}

/// Whether dispatching `event` at `td` ever changes the route: the trait's own arm navigates, or an arm a
/// listener of that trait's event triggers does. One hop: a listener's own listener is a dispatch of its own.
pub fn dispatch_navigates(schema: &OrbitalSchema, td: &TraitDefinition, event: &str) -> bool {
    if td.state_machine.transitions.iter().any(|t| t.event == event && effects_navigate(&t.effects)) {
        return true;
    }
    listeners_of(schema, td, event).into_iter().filter(|heard| !heard.wildcard).any(|heard| {
        heard.trait_def.state_machine.transitions.iter().any(|t| t.event == heard.triggers && effects_navigate(&t.effects))
    })
}

/// One `listens` entry that hears an emit: the listening trait, the event it triggers, and the listen itself.
pub struct HeardBy<'a> {
    pub trait_def: &'a TraitDefinition,
    pub triggers: String,
    pub listen: &'a orbital_core_base::schema::types::ListenDefinition,
    /// Heard through a `* EVENT` wildcard rather than a listen sourced on the emitter.
    pub wildcard: bool,
}

/// The `listens` entries, in every orbital, that hear `event` emitted by `td`.
pub fn listeners_of<'a>(schema: &'a OrbitalSchema, td: &TraitDefinition, event: &str) -> Vec<HeardBy<'a>> {
    use orbital_core_base::schema::types::{split_event_address, ListenSource};
    let Some(source_orbital) = schema.orbitals.iter().find(|o| {
        o.traits.iter().any(|r| matches!(r, TraitReference::Inline(t) if std::ptr::eq(t, td)))
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for orbital in &schema.orbitals {
        for reference in &orbital.traits {
            let TraitReference::Inline(listener) = reference else { continue };
            for l in listener.listens.iter().flatten() {
                let (bare, addressed) = match &l.source {
                    Some(source) => (l.event.clone(), Some(source.clone())),
                    None => split_event_address(&l.event),
                };
                let (heard, wildcard) = match addressed {
                    Some(ListenSource::Trait { trait_name, .. }) => (orbital.name == source_orbital.name && trait_name == td.name, false),
                    Some(ListenSource::Orbital { orbital: name, trait_name, .. }) => (name == source_orbital.name && trait_name == td.name, false),
                    Some(ListenSource::Any) => (true, true),
                    None => (false, false),
                };
                if heard && bare == event {
                    out.push(HeardBy { trait_def: listener, triggers: l.triggers.clone().unwrap_or(bare), listen: l, wildcard });
                }
            }
        }
    }
    out
}

/// A planned "open a non-`main` slot" step: the canonical
/// [`crate::SlotTransition`] plus a synthesized payload for driving the event.
#[derive(Debug, Clone)]
pub struct SlotStep {
    pub info: orbital_core_base::slots::SlotTransition,
    pub payload: serde_json::Value,
}

/// Plan every transition whose effects open a non-`main` slot (modal/drawer/overlay/
/// toast/hud). Enumeration is the canonical [`crate::non_main_slot_transitions`]
/// (one walk, shared with the L2 render probe); this only adds the synthesized payload.
pub fn transitions_with_non_main_slots(schema: &OrbitalSchema) -> Vec<SlotStep> {
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    // No bound persona at this schema-level enumeration (no `Harness` — see
    // `effective_seed_id`), so owner columns go unstamped, same as before.
    let ctx = EntityCtx { schema, owner_fields: &owner_fields, viewer_id: None };
    let by_trait: std::collections::HashMap<&str, &TraitDefinition> =
        inline_traits(schema).into_iter().map(|td| (td.name.as_str(), td)).collect();
    orbital_core_base::slots::non_main_slot_transitions(schema)
        .into_iter()
        .map(|info| {
            let payload = by_trait
                .get(info.trait_name.as_str())
                .map(|td| {
                    let seed_id = td.linked_entity.as_deref().map(seed_row_id);
                    synth_payload(td, &info.event, &ctx, seed_id.as_deref())
                })
                .unwrap_or_else(|| serde_json::json!({}));
            SlotStep { info, payload }
        })
        .collect()
}

/// Shortest event path (with synthesized payloads) from the initial state to `target`,
/// over the success-transition graph. `Some(vec![])` when `target` is the initial state;
/// `None` when unreachable. `seed_id` is the caller-resolved [`effective_seed_id`] of
/// `td.linked_entity` (`None` when the trait links no entity).
pub fn replay_path(
    td: &TraitDefinition,
    target: &str,
    ctx: &EntityCtx<'_>,
    seed_id: Option<&str>,
) -> Option<Vec<(String, serde_json::Value)>> {
    let initial = initial_state(td)?;
    if initial == target {
        // Already in `target` after `reset_state` — but the REAL runtime fires INIT on
        // mount, and an INIT self-arm's effects (field sets, fetches) are part of the
        // state a later step legitimately reads. Returning an empty path skipped them,
        // so e.g. `(set @entity.projectId ?id)` never ran and a downstream persist
        // resolved an EMPTY id (`V-L1-WALKER-EMPTY-ID-FROM-REPLAY`). Replay INIT only
        // when it lands back on `initial`; an INIT that moves elsewhere would leave
        // the state under test.
        let has_init_self_arm = td.state_machine.transitions.iter().any(|t| {
            t.event == "INIT" && t.from.matches(&initial) && t.to == initial
        });
        return Some(if has_init_self_arm {
            vec![("INIT".to_string(), synth_payload(td, "INIT", ctx, seed_id))]
        } else {
            Vec::new()
        });
    }
    replay_steps(td, target, ctx, seed_id).map(|steps| steps.into_iter().map(|s| (s.event, s.payload)).collect())
}

/// One hop of a replay preamble: the event with a payload that fires its transition, and where the machine stands
/// before and after.
#[derive(Debug, Clone)]
pub struct ReplayStep {
    pub event: String,
    pub payload: serde_json::Value,
    pub from: String,
    pub to: String,
    pub accept_states: Vec<String>,
    pub navigates: bool,
}

/// The hops that place a fresh machine in `target`, each with a payload its guard accepts.
pub fn replay_steps(
    td: &TraitDefinition,
    target: &str,
    ctx: &EntityCtx<'_>,
    seed_id: Option<&str>,
) -> Option<Vec<ReplayStep>> {
    let hops = replay_hops(td, target)?;
    Some(
        hops.into_iter()
            .map(|hop| {
                let base = synth_payload(td, &hop.transition.event, ctx, seed_id);
                let payload = match &hop.transition.guard {
                    Some(guard) => {
                        let declared = declared_for(td, &hop.transition.event, &base, seed_id, ctx);
                        conform_payload(guard, &base, &declared).unwrap_or(base)
                    }
                    None => base,
                };
                let closure = transient_closure(td, &hop.to);
                ReplayStep {
                    navigates: dispatch_navigates(ctx.schema, td, &hop.transition.event),
                    accept_states: if closure.len() > 1 { closure } else { Vec::new() },
                    event: hop.transition.event.clone(),
                    payload,
                    from: hop.from,
                    to: hop.to,
                }
            })
            .collect(),
    )
}

/// A transition taken during replay, with the state the machine stood in and where it lands.
pub struct ReplayHop<'a> {
    pub transition: &'a orbital_core_base::schema::types::TransitionDefinition,
    pub from: String,
    pub to: String,
}

type ReplayPath<'a> = Vec<(String, &'a orbital_core_base::schema::types::TransitionDefinition)>;

fn flood<'a>(
    td: &TraitDefinition,
    state: &str,
    path: &ReplayPath<'a>,
    visited: &mut std::collections::HashSet<String>,
    queue: &mut std::collections::VecDeque<(String, ReplayPath<'a>)>,
) {
    for member in transient_closure(td, state) {
        if visited.insert(member.clone()) {
            queue.push_back((member, path.clone()));
        }
    }
}

/// The shortest path of dispatchable hops from the initial state to `target`; `None` when unreachable, empty
/// when `target` is the initial state. An event an effect emits is fired by the runtime, never dispatched, so it
/// is no hop: the machine settles through its closure, and a target reachable only that way is reached by the
/// hop into the state that advances. INIT out of the initial state is the boot, not a hop.
pub fn replay_hops<'a>(td: &'a TraitDefinition, target: &str) -> Option<Vec<ReplayHop<'a>>> {
    use std::collections::{HashMap, HashSet, VecDeque};
    let initial = initial_state(td)?;
    if initial == target {
        return Some(Vec::new());
    }
    let emitted = effect_emitted_events(td);
    let mut adjacency: HashMap<&str, Vec<&'a orbital_core_base::schema::types::TransitionDefinition>> = HashMap::new();
    for t in &td.state_machine.transitions {
        if emitted.contains(&t.event) {
            continue;
        }
        for from in t.from.states() {
            if from == "*" || (t.event == "INIT" && from == initial) {
                continue;
            }
            adjacency.entry(from).or_default().push(t);
        }
    }
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, ReplayPath<'a>)> = VecDeque::new();
    flood(td, &initial, &Vec::new(), &mut visited, &mut queue);
    while let Some((state, path)) = queue.pop_front() {
        if state == target {
            let mut out = Vec::new();
            for (from, t) in path {
                out.push(ReplayHop { from, to: t.to.clone(), transition: t });
            }
            return Some(out);
        }
        for edge in adjacency.get(state.as_str()).into_iter().flatten() {
            if visited.contains(&edge.to) {
                continue;
            }
            let mut next = path.clone();
            next.push((state.clone(), *edge));
            flood(td, &edge.to, &next, &mut visited, &mut queue);
        }
    }
    None
}

/// The transitions of [`replay_hops`].
pub fn replay_transitions<'a>(
    td: &'a TraitDefinition,
    target: &str,
) -> Option<Vec<&'a orbital_core_base::schema::types::TransitionDefinition>> {
    replay_hops(td, target).map(|hops| hops.into_iter().map(|h| h.transition).collect())
}

fn declared_for(
    td: &TraitDefinition,
    event: &str,
    base: &serde_json::Value,
    seed_id: Option<&str>,
    ctx: &EntityCtx<'_>,
) -> Declared {
    let fields = fields_to_synthesize(td, event);
    let types = fields.iter().map(|f| (f.name.clone(), f.field_type.clone())).collect();
    let mut rows = std::collections::BTreeMap::new();
    for f in &fields {
        let Some(element) = f.field_type.strip_prefix('[').and_then(|t| t.strip_suffix(']')) else { continue };
        let entity = f
            .entity
            .as_deref()
            .or_else(|| entity_def(ctx.schema, element).is_some().then_some(element))
            .or(td.linked_entity.as_deref());
        if let Some(entity) = entity {
            rows.insert(f.name.clone(), synth_entity_row(ctx, entity));
        }
    }
    Declared { types, base: base.clone(), seed_id: seed_id.map(str::to_string), rows }
}

/// The `(trait, event)` pairs that are a declared reply route of an `http` effect, with which kind.
///
/// An `emit` is delivered by the kernel (`step_followups` / `emit_followup`) to the emitting trait itself
/// (its own arm for the event) and to every trait whose `listens` entry names the event and accepts the
/// emitter as its source, under that entry's `triggers` event. A same-named event on any other trait is an
/// ordinary event. An event that is the failure route of one effect and a success-side route of another
/// is a success-side route.
pub fn http_reply_routes(
    schema: &OrbitalSchema,
) -> std::collections::BTreeMap<(String, String), ReplyRoute> {
    use orbital_core_base::schema::types::{split_event_address, ListenSource};
    let traits = inline_traits(schema);
    let mut routes = std::collections::BTreeMap::new();
    for emitter in &traits {
        let mut declared = std::collections::BTreeMap::new();
        for t in &emitter.state_machine.transitions {
            for eff in t.effects.iter().flatten() {
                collect_http_routes(eff, &mut declared);
            }
        }
        for (event, route) in declared {
            let mut deliver = |trait_name: &str, delivered: &str| {
                let slot = routes.entry((trait_name.to_string(), delivered.to_string())).or_insert(route);
                if route == ReplyRoute::Other {
                    *slot = ReplyRoute::Other;
                }
            };
            deliver(&emitter.name, &event);
            for listener in &traits {
                for l in listener.listens.iter().flatten() {
                    let (bare, addressed) = match &l.source {
                        Some(source) => (l.event.clone(), Some(source.clone())),
                        None => split_event_address(&l.event),
                    };
                    let accepts = match &addressed {
                        None | Some(ListenSource::Any) => true,
                        Some(ListenSource::Trait { trait_name, .. })
                        | Some(ListenSource::Orbital { trait_name, .. }) => *trait_name == emitter.name,
                    };
                    if bare == event && accepts {
                        deliver(&listener.name, l.triggers.as_deref().unwrap_or(&bare));
                    }
                }
            }
        }
    }
    routes
}

fn collect_http_routes(expr: &SExpression, out: &mut std::collections::BTreeMap<String, ReplyRoute>) {
    if let Some(Effect::Http { emit: Some(emit), .. } | Effect::Redis { emit: Some(emit), .. } | Effect::Postgres { emit: Some(emit), .. }) = parse_effect(expr) {
        for event in [&emit.success, &emit.cancelled, &emit.on_message].into_iter().flatten() {
            out.insert(event.clone(), ReplyRoute::Other);
        }
        if let Some(event) = &emit.failure {
            out.entry(event.clone()).or_insert(ReplyRoute::Failure);
        }
    }
    if let SExpression::Array(items) = expr {
        for item in items {
            collect_http_routes(item, out);
        }
    }
}
