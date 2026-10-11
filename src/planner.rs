//! Planner — turn a trait's state machine into hermetic per-transition steps.
//!
//! Mirrors `plan-walk.ts`: one step per `(from, event)` with the TS variant set
//! (`success` / `malformed`). The expected target is the **declared** `transition.to`
//! from the resolved `.orb` (an oracle independent of kernel logic). Transient closure
//! of effect-emitted auto-advance is a *client* concern (the kernel fires one event and
//! returns the declared target; the client re-fires emitted follow-ups), so Layer 1 does
//! not need it.

use serde::Serialize;
use crate::effects::{parse_effect, Effect, PersistOperation};
use crate::{EntityDefinition, EntityRef, OrbitalSchema, SExpression, TraitDefinition, TraitReference};

/// Which payload variant of a transition this step exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Variant {
    /// First event into the initial state (no preamble).
    AutoInit,
    /// Full synthesized payload — expect the declared target (or its transient closure).
    Success,
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
            if let crate::EntityRef::Inline(def) = er {
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
fn fields_to_synthesize<'a>(td: &'a TraitDefinition, event: &str) -> Vec<&'a crate::PayloadField> {
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
    use crate::SExpression as S;
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
fn entity_def<'a>(schema: &'a OrbitalSchema, name: &str) -> Option<&'a EntityDefinition> {
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
fn synth_entity_row(ctx: &EntityCtx<'_>, entity_name: &str) -> serde_json::Value {
    let seed_id = effective_seed_id(entity_name, ctx.owner_fields, ctx.viewer_id.as_deref());
    let Some(def) = entity_def(ctx.schema, entity_name) else {
        // A ref-only/unresolved entity — the seeded id is still the most useful
        // placeholder a caller can act on.
        return serde_json::json!({ "id": seed_id });
    };
    let mut rows = crate::runtime::sample_rows(def, 1);
    let Some(row) = rows.first_mut() else {
        return serde_json::json!({ "id": seed_id });
    };
    if let crate::Value::Object(map) = row {
        map.insert("id".to_string(), crate::Value::String(seed_id.clone()));
    }
    let mut stamped = [row.clone()];
    crate::runtime::stamp_owner(
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
    serde_json::Value::Object(obj)
}

/// A nested object contract built from every declared property (TS `buildMinimalPayload`),
/// carrying the seeded row id so `(persist update E ?data)` finds `data.id`.
fn synth_object(
    f: &crate::PayloadField,
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

fn synth_field(
    f: &crate::PayloadField,
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
    if let Some(projection) = &f.projected_from {
        if entity_def(ctx.schema, &projection.type_name).is_some() {
            let row = synth_entity_row(ctx, &projection.type_name);
            let value = row.get(&projection.field).cloned().unwrap_or(serde_json::Value::Null);
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
        // string + other entity-typed fields → a placeholder string id.
        _ => serde_json::json!("verify"),
    }
}

/// Plan every `(from, event)` transition group in a trait. Sibling transitions sharing an
/// event are collapsed into one step whose accept-set is the union of their targets — the
/// kernel picks the winner by guard/priority, so any of them is a pass. `seed_id` is the
/// caller-resolved [`effective_seed_id`] of `td.linked_entity` (`None` when the trait
/// links no entity).
pub fn plan_trait(
    td: &TraitDefinition,
    ctx: &EntityCtx<'_>,
    seed_id: Option<&str>,
) -> Vec<Step> {
    use std::collections::BTreeMap;
    let initial = initial_state(td);

    // (from, event) -> (ordered unique targets, any-guarded)
    let mut groups: BTreeMap<(String, String), (Vec<String>, bool)> = BTreeMap::new();
    for t in &td.state_machine.transitions {
        for from in t.from.states() {
            let entry = groups
                .entry((from.to_string(), t.event.clone()))
                .or_insert_with(|| (Vec::new(), false));
            if !entry.0.contains(&t.to) {
                entry.0.push(t.to.clone());
            }
            entry.1 = entry.1 || t.guard.is_some();
        }
    }

    let mut steps = Vec::new();
    for ((from, event), (to_states, guarded)) in groups {
        let variant = if initial.as_deref() == Some(from.as_str()) {
            Variant::AutoInit
        } else {
            Variant::Success
        };
        let payload = synth_payload(td, &event, ctx, seed_id);
        steps.push(Step {
            trait_name: td.name.clone(),
            linked_entity: td.linked_entity.clone(),
            from,
            event,
            to: to_states.first().cloned().unwrap_or_default(),
            to_states,
            variant,
            payload,
            guarded,
        });
    }
    // NOTE: the malformed-payload variant (empty `{}` must be rejected) is a SERVER/CLIENT-layer
    // concern — the bare kernel does not run the API payload validator — verified in Layer 2.
    steps
}

/// A planned "open a non-`main` slot" step: the canonical
/// [`crate::SlotTransition`] plus a synthesized payload for driving the event.
#[derive(Debug, Clone)]
pub struct SlotStep {
    pub info: crate::SlotTransition,
    pub payload: serde_json::Value,
}

/// Plan every transition whose effects open a non-`main` slot (modal/drawer/overlay/
/// toast/hud). Enumeration is the canonical [`crate::non_main_slot_transitions`]
/// (one walk, shared with the L2 render probe); this only adds the synthesized payload.
pub fn transitions_with_non_main_slots(schema: &OrbitalSchema) -> Vec<SlotStep> {
    let owner_fields = crate::runtime::owner_fields_from_schema(schema);
    // No bound persona at this schema-level enumeration (no `Harness` — see
    // `effective_seed_id`), so owner columns go unstamped, same as before.
    let ctx = EntityCtx { schema, owner_fields: &owner_fields, viewer_id: None };
    let by_trait: std::collections::HashMap<&str, &TraitDefinition> =
        inline_traits(schema).into_iter().map(|td| (td.name.as_str(), td)).collect();
    crate::non_main_slot_transitions(schema)
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
    use std::collections::{HashMap, VecDeque};
    // prev: state -> (came_from_state, event)
    let mut prev: HashMap<String, (String, String)> = HashMap::new();
    let mut queue = VecDeque::new();
    queue.push_back(initial.clone());
    let mut visited = std::collections::HashSet::new();
    visited.insert(initial.clone());

    while let Some(state) = queue.pop_front() {
        for t in &td.state_machine.transitions {
            if !t.from.matches(&state) {
                continue;
            }
            if visited.insert(t.to.clone()) {
                prev.insert(t.to.clone(), (state.clone(), t.event.clone()));
                if t.to == target {
                    // Reconstruct.
                    let mut path = Vec::new();
                    let mut cur = target.to_string();
                    while let Some((from, event)) = prev.get(&cur) {
                        path.push((event.clone(), synth_payload(td, event, ctx, seed_id)));
                        cur = from.clone();
                    }
                    path.reverse();
                    return Some(path);
                }
                queue.push_back(t.to.clone());
            }
        }
    }
    None
}
