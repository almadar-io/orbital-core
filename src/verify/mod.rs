//! Layer 1 — the kernel circuit walk: plan every trait's transitions, drive the
//! kernel headlessly, observe. Pure over a resolved program, so the compiled
//! `orb verify` and an in-browser client run the same walk.

pub mod driver;
pub mod frame;
pub mod observers;
pub mod report;

pub use report::Report;

use crate::evaluator::ValueMap;
use crate::planner;
use crate::{OrbitalSchema, Value};

/// Verify a resolved schema: plan every trait's transitions, drive the kernel, observe.
///
/// An app that declares an `[identity]` roster has no anonymous viewer. Left
/// unbound, every `@read` directive denies every row, so the walk verifies an
/// empty app and Layer 2 fails `entity_data_bound` on a list that legitimately
/// has data — measured on `std-helpdesk`: 0/1 unbound vs 1/1 (5/5 render) with a
/// persona bound. So with `ALMADAR_PERSONA` unset the default single run binds
/// the FIRST declared persona: deterministic (roster order), never a heuristic
/// ranking of roles. An explicit `ALMADAR_PERSONA` still wins — `Harness::new`
/// resolved it already, and this only fills a viewer nobody set.
pub fn verify_schema(item: &str, schema: OrbitalSchema) -> Result<Report, String> {
    verify_schema_scoped(item, schema, None)
}

/// Same walk as [`verify_schema`], restricted to `traits` when given (`--trait`): only
/// the named trait(s)' `(from, event)` pairs are driven, and coverage/verdicts count
/// only those traits. `traits` accepts either spelling a trait can carry post-resolve
/// (see [`planner::trait_matches_name`]); a name matching nothing is an error listing
/// every available trait name.
pub fn verify_schema_scoped(
    item: &str,
    schema: OrbitalSchema,
    traits: Option<&[String]>,
) -> Result<Report, String> {
    let harness = driver::Harness::new(schema.clone())?;
    if matches!(
        crate::runtime::persona_from_env_spec(),
        Ok(crate::runtime::EnvPersona::None)
    ) {
        let roster = crate::runtime::identity_roster(
            &schema,
            crate::runtime::DEFAULT_IDENTITY_ROSTER_COUNT,
        );
        if let Some(first) = roster.first() {
            harness.set_persona(first.clone());
        }
    }
    verify_with_harness(item, &schema, &harness, traits)
}

/// Verify a resolved schema as a specific persona — same walk as [`verify_schema`],
/// with `@user` bound to `persona` throughout (task #129, persona-parameterised
/// verify run). One fresh `Harness` per call; the persona is bound once before
/// any step is driven, so every fetch/mutation access-policy check sees it.
pub fn verify_schema_as(item: &str, schema: OrbitalSchema, persona: Value) -> Result<Report, String> {
    verify_schema_as_scoped(item, schema, persona, None)
}

/// [`verify_schema_as`], restricted to `traits` — see [`verify_schema_scoped`].
pub fn verify_schema_as_scoped(
    item: &str,
    schema: OrbitalSchema,
    persona: Value,
    traits: Option<&[String]>,
) -> Result<Report, String> {
    let harness = driver::Harness::new(schema.clone())?;
    harness.set_persona(persona);
    verify_with_harness(item, &schema, &harness, traits)
}

fn verify_with_harness(
    item: &str,
    schema: &OrbitalSchema,
    harness: &driver::Harness,
    traits: Option<&[String]>,
) -> Result<Report, String> {
    let resolved_traits = traits
        .map(|names| planner::resolve_trait_names(schema, names))
        .transpose()
        .map_err(|e| e.to_string())?;
    let selected = planner::inline_traits_scoped(schema, resolved_traits.as_deref());
    let mut frames = Vec::new();
    let ctx = harness.entity_ctx(schema);
    // The roster's default persona (or none), read once so a per-step role
    // rewrite always restores exactly what the caller bound — never a
    // previous step's rewritten role.
    let default_persona = harness.current_persona();
    for td in selected.iter().copied() {
        let seed_id = td
            .linked_entity
            .as_deref()
            .map(|e| harness.effective_seed_id(e));
        for step in planner::plan_trait(td, &ctx, seed_id.as_deref()) {
            // A role-only persist policy (`@delete (= @user.role "approver")`)
            // denies the default persona unless it happens to carry that role.
            // Deriving the satisfying role per step and rebinding to it before
            // driving is what lets the single default walk exercise the step
            // for real, instead of bucketing it as an expected access denial
            // that only `--all-personas` would find a passing viewer for.
            let role = planner::persist_targets(td, &step.from, &step.event)
                .into_iter()
                .find_map(|(entity, kind)| {
                    crate::runtime::role_satisfying_policy(schema, &entity, kind)
                });
            if let Some((field, value)) = &role {
                harness.set_persona(persona_with_field(&default_persona, field, value));
            }
            let frame = harness.drive(td, &step, frames.len(), &ctx);
            if role.is_some() {
                harness.set_persona(default_persona.clone());
            }
            frames.push(frame);
        }
    }
    let mut verdicts = observers::run_all(&frames);
    verdicts.push(observers::ref_trait_invariant(&selected));
    let mut report = Report::new(item.to_string(), frames, verdicts);
    report.traits = resolved_traits;
    Ok(report)
}

/// `base` with `field` overridden to `value` — `base` cloned if it is already
/// an object, or a fresh one when no persona was bound at all (an app with no
/// `[identity]` roster still declares a role-scoped policy, so the walker
/// needs a synthesized viewer to carry it).
fn persona_with_field(base: &Value, field: &str, value: &str) -> Value {
    let mut obj = match base {
        Value::Object(m) => m.clone(),
        _ => ValueMap::new(),
    };
    obj.insert(field.to_string(), Value::String(value.to_string()));
    Value::Object(obj)
}

