//! The guard precondition of a planned step: a guard that reads an `@entity.<field>` the guarded trait never
//! sets on its own is satisfied by a sibling trait on the same entity (or a setter the trait's own walk reaches).
//! The hermetic reset strands such a field at its default, so the step needs the setter dispatched first.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::planner::{
    collect_binding_texts, collect_emitted, effective_seed_id, entity_def, initial_state,
    inline_traits, literal_json, persist_targets, replay_transitions, synth_payload, BindRowFrom,
    EntityCtx, Precondition, Proof, UnprovenReason,
};
use crate::verify::plan_wire::RoleWire;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::schema::types::{
    ListenSource, OrbitalSchema, TraitDefinition, TransitionDefinition,
};
use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::context::BindingContextBuilder;
use orbital_core_eval::evaluator::Evaluator;

/// `@entity.<field>` bindings the guard reads, `id` excluded (the row identity is the seed row), in discovery order.
fn entity_field_bindings(guard: &SExpression) -> Vec<String> {
    let mut texts = Vec::new();
    collect_binding_texts(guard, &mut texts);
    let mut out: Vec<String> = Vec::new();
    for text in texts {
        let Some(field) = text.strip_prefix("@entity.") else {
            continue;
        };
        let plain =
            !field.is_empty() && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if plain && field != "id" && !out.iter().any(|f| f == field) {
            out.push(field.to_string());
        }
    }
    out
}

/// The value a `(set @entity.<field> value)` anywhere in the effect writes, and its top-level `@payload` source.
struct FieldSet<'a> {
    value: &'a SExpression,
    payload_path: Option<String>,
}

pub(crate) fn find_set<'a>(node: &'a SExpression, target: &str) -> Option<&'a SExpression> {
    match node {
        SExpression::Array(items) => {
            if let [SExpression::String(op), SExpression::String(path), value] = &items[..] {
                if op == "set" && path == target {
                    return Some(value);
                }
            }
            items.iter().find_map(|i| find_set(i, target))
        }
        SExpression::Object(map) => map.values().find_map(|v| find_set(v, target)),
        _ => None,
    }
}

fn field_set<'a>(transition: &'a TransitionDefinition, field: &str) -> Option<FieldSet<'a>> {
    let target = format!("@entity.{field}");
    let value = transition
        .effects
        .iter()
        .flatten()
        .find_map(|e| find_set(e, &target))?;
    let payload_path = match value {
        SExpression::String(v) => v
            .strip_prefix("@payload.")
            .filter(|rest| !rest.contains('.'))
            .map(str::to_string),
        _ => None,
    };
    Some(FieldSet {
        value,
        payload_path,
    })
}

fn evaluate(
    expr: &SExpression,
    entity: &serde_json::Value,
    payload: &serde_json::Value,
    state: &str,
) -> Result<Value, String> {
    let context = |v: &serde_json::Value| {
        serde_json::from_value::<Value>(v.clone()).map_err(|e| e.to_string())
    };
    let mut ctx = BindingContextBuilder::new()
        .entity(context(entity)?)
        .payload(context(payload)?)
        .state(Value::String(state.to_string()))
        .build();
    ctx.from_state = Some(state.to_string());
    Evaluator::new()
        .evaluate(expr, &ctx)
        .map_err(|e| e.to_string())
}

fn guard_holds(
    guard: &SExpression,
    entity: &serde_json::Value,
    payload: &serde_json::Value,
    state: &str,
) -> bool {
    matches!(evaluate(guard, entity, payload, state), Ok(v) if v.is_truthy())
}

fn to_json(value: &Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// The linked entity's declared defaults for `fields`, over the literal writes the trait's own boot makes.
fn baseline(
    schema: &OrbitalSchema,
    td: &TraitDefinition,
    fields: &[String],
) -> (serde_json::Map<String, serde_json::Value>, Vec<String>) {
    let mut out = serde_json::Map::new();
    let mut established = Vec::new();
    let entity = td
        .linked_entity
        .as_deref()
        .and_then(|name| entity_def(schema, name));
    for field in fields {
        let default = entity
            .and_then(|e| {
                e.fields
                    .iter()
                    .find(|f| f.name.as_deref() == Some(field.as_str()))
            })
            .and_then(|f| f.default.as_ref())
            .and_then(literal_json);
        if let Some(value) = default {
            out.insert(field.clone(), value);
        }
    }
    if let Some(initial) = initial_state(td) {
        for t in &td.state_machine.transitions {
            if t.event != "INIT" || !(t.from.matches(&initial) || t.from.matches("*")) {
                continue;
            }
            for field in fields {
                let Some(set) = field_set(t, field) else {
                    continue;
                };
                if set.payload_path.is_some() || out.contains_key(field) && entity.is_none() {
                    continue;
                }
                if let Ok(value) = evaluate(
                    set.value,
                    &serde_json::json!({}),
                    &serde_json::json!({}),
                    &initial,
                ) {
                    out.insert(field.clone(), to_json(&value));
                    established.push(field.clone());
                }
            }
        }
    }
    (out, established)
}

/// Chase an effect-emitted setter event back to the first event a user can dispatch: a result event settles as a
/// side effect of the transition that produces it, so dispatching it directly would no-op.
fn dispatchable(
    td: &TraitDefinition,
    event: &str,
    at_state: &str,
    visited: &mut Vec<String>,
) -> (String, String) {
    if visited.iter().any(|v| v == event) {
        return (event.to_string(), at_state.to_string());
    }
    visited.push(event.to_string());
    let produces = |t: &TransitionDefinition| {
        let mut emits = false;
        t.effects
            .iter()
            .flatten()
            .for_each(|e| collect_emitted(e, &mut |name| emits |= name == event));
        emits
    };
    let Some(producer) = td.state_machine.transitions.iter().find(|t| produces(t)) else {
        return (event.to_string(), at_state.to_string());
    };
    let from = producer.from.states().first().copied().unwrap_or("*");
    let from = if from == "*" {
        initial_state(td).unwrap_or_else(|| at_state.to_string())
    } else {
        from.to_string()
    };
    dispatchable(td, &producer.event, &from, visited)
}

struct Candidate<'a> {
    td: &'a TraitDefinition,
    transition: &'a TransitionDefinition,
    anchor: String,
    event: String,
}

fn candidates<'a>(
    schema: &'a OrbitalSchema,
    linked: &str,
    field: &str,
    guarded: &TraitDefinition,
    at_state: &str,
) -> Vec<Candidate<'a>> {
    let mut out = Vec::new();
    for td in inline_traits(schema) {
        if td.linked_entity.as_deref() != Some(linked) {
            continue;
        }
        let same = td.name == guarded.name;
        let Some(required) = (if same {
            Some(at_state.to_string())
        } else {
            initial_state(td)
        }) else {
            continue;
        };
        let emitted = crate::planner::effect_emitted_events(td);
        for t in &td.state_machine.transitions {
            if t.event == "INIT" || !(t.from.matches(&required) || t.from.matches("*")) {
                continue;
            }
            if same && t.to != at_state {
                continue;
            }
            if field_set(t, field).is_none() {
                continue;
            }
            let (event, anchor) = if emitted.contains(&t.event) {
                dispatchable(td, &t.event, &required, &mut Vec::new())
            } else {
                (t.event.clone(), required.clone())
            };
            out.push(Candidate {
                td,
                transition: t,
                anchor,
                event,
            });
        }
    }
    out
}

/// What the written value does to the guard: it satisfies it, it does not, or it could not be evaluated.
enum Verdict {
    Satisfies,
    Fails,
    Unknown,
}

fn judge_value(
    set: &FieldSet<'_>,
    field: &str,
    guard: &SExpression,
    base: &serde_json::Map<String, serde_json::Value>,
    payload: &serde_json::Value,
    state: &str,
    step_payload: &serde_json::Value,
    others_unestablished: bool,
) -> Verdict {
    match evaluate(
        set.value,
        &serde_json::Value::Object(base.clone()),
        payload,
        state,
    ) {
        Err(_) => Verdict::Unknown,
        Ok(value) => {
            let mut entity = base.clone();
            entity.insert(field.to_string(), to_json(&value));
            if guard_holds(
                guard,
                &serde_json::Value::Object(entity),
                step_payload,
                state,
            ) {
                Verdict::Satisfies
            } else if others_unestablished {
                Verdict::Unknown
            } else {
                Verdict::Fails
            }
        }
    }
}

fn reads_payload(value: &SExpression) -> bool {
    let mut texts = Vec::new();
    collect_binding_texts(value, &mut texts);
    texts.iter().any(|t| {
        *t == "@payload" || t.starts_with("@payload.") || t.starts_with("@callsitePayload")
    })
}

fn referenced_entity(
    schema: &OrbitalSchema,
    setter: &TraitDefinition,
    event: &str,
    payload_path: &str,
) -> Option<String> {
    let declared = setter
        .state_machine
        .events
        .iter()
        .flatten()
        .find(|e| e.key == event)
        .and_then(|e| e.payload_schema.as_ref())
        .and_then(|fields| fields.iter().find(|f| f.name == payload_path));
    if let Some(entity) = declared.and_then(|f| f.entity.clone()) {
        return Some(entity);
    }
    let listen = setter.listens.iter().flatten().find(|l| {
        l.triggers.as_deref() == Some(event) && matches!(l.source, Some(ListenSource::Trait { .. }))
    })?;
    let SExpression::String(mapped) = listen.payload_mapping.as_ref()?.get(payload_path)? else {
        return None;
    };
    let single = mapped
        .strip_prefix("@payload.")
        .is_some_and(|r| !r.is_empty() && !r.contains('.'));
    if !single {
        return None;
    }
    let Some(ListenSource::Trait { trait_name, .. }) = &listen.source else {
        return None;
    };
    inline_traits(schema)
        .into_iter()
        .find(|t| &t.name == trait_name)
        .and_then(|t| t.linked_entity.clone())
}

/// The precondition of the `pass` step of `transition` at `at_state`, or `None` when the guard needs none.
pub fn plan_precondition(
    ctx: &EntityCtx<'_>,
    td: &TraitDefinition,
    transition: &TransitionDefinition,
    at_state: &str,
    step_payload: &serde_json::Value,
) -> Option<Precondition> {
    let guard = transition.guard.as_ref()?;
    let linked = td.linked_entity.as_deref()?;
    if at_state == "*" {
        return None;
    }
    let fields = entity_field_bindings(guard);
    if fields.is_empty() {
        return None;
    }
    let schema = ctx.schema;
    let (base, init_established) = baseline(schema, td, &fields);
    if !base.is_empty()
        && guard_holds(
            guard,
            &serde_json::Value::Object(base.clone()),
            step_payload,
            at_state,
        )
    {
        return None;
    }
    let viewer = ctx.viewer_id.as_deref();
    for field in &fields {
        for candidate in candidates(schema, linked, field, td, at_state) {
            let Some(set) = field_set(candidate.transition, field) else {
                continue;
            };
            let seed = candidate
                .td
                .linked_entity
                .as_deref()
                .map(|e| effective_seed_id(e, ctx.owner_fields, viewer));
            let mut payload = synth_payload(candidate.td, &candidate.event, ctx, seed.as_deref());
            let chased = candidate.event != candidate.transition.event;
            let mut bind_row_from = None;
            if let (Some(path), false) = (&set.payload_path, chased) {
                if let Some(entity) =
                    referenced_entity(schema, candidate.td, &candidate.event, path)
                {
                    payload[path] = serde_json::Value::from(effective_seed_id(
                        &entity,
                        ctx.owner_fields,
                        viewer,
                    ));
                    bind_row_from = Some(BindRowFrom {
                        entity,
                        payload_field: path.clone(),
                    });
                }
            }
            // A chased setter's payload is not the one the arm it reaches receives, so only a value that reads no
            // payload can be judged there.
            let verdict = if chased && reads_payload(set.value) {
                Verdict::Unknown
            } else {
                judge_value(
                    &set,
                    field,
                    guard,
                    &base,
                    &payload,
                    &candidate.anchor,
                    step_payload,
                    fields
                        .iter()
                        .any(|f| f != field && !init_established.contains(f)),
                )
            };
            let proof = match verdict {
                Verdict::Fails => continue,
                Verdict::Satisfies => Proof::Proven,
                Verdict::Unknown => Proof::Unproven {
                    reason: UnprovenReason::SetterValueUnresolved,
                },
            };
            let role = persist_targets(candidate.td, &candidate.anchor, &candidate.event)
                .into_iter()
                .find_map(|(entity, kind)| {
                    crate::entity_access::role_satisfying_policy(schema, &entity, kind)
                })
                .map(|(field, value)| RoleWire { field, value });
            let unplanned_fields = fields
                .iter()
                .filter(|f| *f != field && !init_established.contains(f))
                .cloned()
                .collect();
            return Some(Precondition::Established {
                trait_name: candidate.td.name.clone(),
                event: candidate.event,
                payload,
                establish_at_state: candidate.anchor,
                bind_row_from,
                role,
                unplanned_fields,
                proof,
            });
        }
    }
    let setters_value = |t: &TransitionDefinition| {
        fields.iter().any(|field| {
            field_set(t, field).is_some_and(|set| {
                !matches!(
                    judge_value(
                        &set,
                        field,
                        guard,
                        &base,
                        &serde_json::json!({}),
                        at_state,
                        step_payload,
                        fields.len() > 1
                    ),
                    Verdict::Fails
                )
            })
        })
    };
    let on_path =
        replay_transitions(td, at_state).is_some_and(|path| path.into_iter().any(setters_value));
    if on_path {
        return None;
    }
    // A tick writes the trait's own frame on its clock, off any replay path (G-VERIFY-044).
    let tick_sets = td.ticks.iter().flatten().any(|tick| {
        fields.iter().any(|field| {
            let target = format!("@entity.{field}");
            tick.effects
                .iter()
                .find_map(|e| find_set(e, &target))
                .is_some_and(|value| {
                    let set = FieldSet {
                        value,
                        payload_path: None,
                    };
                    !matches!(
                        judge_value(
                            &set,
                            field,
                            guard,
                            &base,
                            &serde_json::json!({}),
                            at_state,
                            step_payload,
                            fields.len() > 1
                        ),
                        Verdict::Fails
                    )
                })
        })
    });
    if tick_sets
        || td
            .state_machine
            .transitions
            .iter()
            .any(|t| t.event != "INIT" && setters_value(t))
    {
        return Some(Precondition::SameTraitSetter {
            fields,
            proof: Proof::Unproven {
                reason: UnprovenReason::SetterNotOnReplayPath,
            },
        });
    }
    Some(Precondition::Unreachable { fields })
}
