//! The ordering and expectations of the verifier's flow planners: the emit sweep, the pattern event contracts,
//! affordance clicks, forced-failure probes and the CRUD chains. A host finds what it can only see (the
//! rendered buttons, forms and portals) and asks the kernel what to fire and what to expect.

use crate::planner::{self, Precondition};
use crate::verify::plan_wire::{self, PlanOptions, PreambleEvent, RoleWire};
use crate::verify::{payload_solve, precondition};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::schema::types::TraitDefinition;
use orbital_core_base::schema::types::OrbitalSchema;
use serde::{Deserialize, Serialize};

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmitStep {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: serde_json::Value,
    pub navigates: bool,
    pub coverage_key: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractStep {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    pub pattern: String,
    pub coverage_key: String,
}

/// The events the rendered patterns promise to emit, from the installed language data's event contracts.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractEvents {
    /// False when no language data is installed: the table cannot be read, so nothing is planned.
    pub available: bool,
    pub steps: Vec<ContractStep>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffordanceQuery {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
}

/// A rendered affordance the host found, planned: where to stand, what to fire, where it lands.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AffordanceStep {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    /// The declared target; `from` when the trait declares no such transition (a dead affordance).
    pub to: String,
    pub declared: bool,
    pub navigates: bool,
    pub instance_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_row_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preamble: Option<Vec<PreambleEvent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precondition: Option<Precondition>,
    pub coverage_key: String,
}

fn scoped<'a>(schema: &'a OrbitalSchema, options: &PlanOptions) -> Vec<&'a TraitDefinition> {
    let resolved = options.traits.as_deref().and_then(|names| planner::resolve_trait_names(schema, names).ok());
    planner::inline_traits_scoped(schema, resolved.as_deref())
}

fn entity_ctx<'a>(schema: &'a OrbitalSchema, owner_fields: &'a [(String, String)], options: &PlanOptions) -> planner::EntityCtx<'a> {
    planner::EntityCtx { schema, owner_fields, viewer_id: options.viewer_id.clone() }
}

/// The success and failure events a transition's effects declare through an `emit` option object, in order.
fn declared_emits(td: &TraitDefinition) -> Vec<String> {
    let mut out = Vec::new();
    for effect in td.state_machine.transitions.iter().flat_map(|t| t.effects.iter().flatten()) {
        let SExpression::Array(items) = effect else { continue };
        let Some(SExpression::Object(options)) = items.last() else { continue };
        let Some(SExpression::Object(emit)) = options.get("emit") else { continue };
        for key in ["success", "failure"] {
            if let Some(SExpression::String(event)) = emit.get(key) {
                out.push(event.clone());
            }
        }
    }
    out
}

/// Fire each event a trait's effects declare they emit, once, from the trait's initial state: a valid payload
/// from the event's declared schema, solved through the first arm that handles it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransientQuery {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    pub to: String,
}

/// A forced-failure step: dispatch the transition that ENTERS `from` as a viewer the entity's own policy denies,
/// so the failure event fires for real through the program's own cascade.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransientProbe {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    pub to: String,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: serde_json::Value,
    pub accept_states: Vec<String>,
    pub role: RoleWire,
    /// The nominal failure arm the cascade is expected to prove.
    pub verifies_portal_for: TransientQuery,
    pub instance_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_row_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preamble: Option<Vec<PreambleEvent>>,
    pub coverage_key: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TransientFinding {
    /// No persist of the trait declares this event as its `emit.failure`, so nothing can be forced.
    NoEnteringPersist { event: String },
    /// No viewer role is denied by the entity's policy for that persist.
    NoDenyingViewer { entity: String, operation: String },
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum TransientOutcome {
    /// The arm is not a transient failure arm: no probe is owed.
    NotTransient,
    Probe { step: Box<TransientProbe> },
    Finding { finding: TransientFinding },
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrudKind {
    Create,
    Edit,
    Delete,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerRequirement {
    pub source_entity: String,
    pub source_field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_default_id: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_owner_field: Option<String>,
}

/// Who a persist must run as, from the entity's declared access policy.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerRequirement {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<OwnerRequirement>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowShapeField {
    pub name: String,
    pub whole_row: bool,
}

/// One create, edit or delete chain: the persistor, the overlay trait whose open and submit transitions drive it,
/// and what the chain must leave behind. The host adds what it alone can see (the form, the row's buttons).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrudChain {
    pub kind: CrudKind,
    pub entity: String,
    pub persistor: String,
    pub source_trait: String,
    pub open_from: String,
    pub open_event: String,
    pub open_to: String,
    pub submit_event: String,
    pub success_event: String,
    pub row_delta: i32,
    /// The event the rendered button carries when the open event reaches the overlay through a listener.
    pub open_affordance_event: String,
    /// The open transition renders into a portal slot or a self-overlay pattern.
    pub opens_overlay: bool,
    pub requires_row_context: bool,
    pub payload_row_shape: Vec<RowShapeField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer: Option<ViewerRequirement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precondition: Option<Precondition>,
    pub coverage_key: String,
}

fn persist_failing(node: &SExpression, event: &str) -> Option<(String, &'static str, Option<SExpression>)> {
    let SExpression::Array(items) = node else { return None };
    if let (Some(SExpression::String(op)), Some(SExpression::String(kind)), Some(SExpression::String(entity))) =
        (items.first(), items.get(1), items.get(2))
    {
        let kind = match (op.as_str(), kind.as_str()) {
            ("persist", "create") => Some("create"),
            ("persist", "update") => Some("update"),
            ("persist", "delete") => Some("delete"),
            _ => None,
        };
        if let Some(kind) = kind {
            let fails = items.iter().skip(3).any(|arg| {
                matches!(arg, SExpression::Object(o) if matches!(
                    o.get("emit"),
                    Some(SExpression::Object(emit)) if matches!(emit.get("failure"), Some(SExpression::String(f)) if f == event)))
            });
            if fails {
                return Some((entity.clone(), kind, items.get(3).cloned()));
            }
        }
    }
    items.iter().find_map(|child| persist_failing(child, event))
}

fn collect_failure_events(node: &SExpression, out: &mut Vec<String>) {
    let SExpression::Array(items) = node else { return };
    if matches!(items.first(), Some(SExpression::String(op)) if matches!(op.as_str(), "persist" | "fetch" | "call-service")) {
        for arg in items {
            if let SExpression::Object(o) = arg {
                if let Some(SExpression::Object(emit)) = o.get("emit") {
                    if let Some(SExpression::String(f)) = emit.get("failure") {
                        if !f.is_empty() {
                            out.push(f.clone());
                        }
                    }
                }
            }
        }
    }
    items.iter().for_each(|child| collect_failure_events(child, out));
}

fn policy_of<'a>(def: &'a orbital_core_base::schema::types::EntityDefinition, kind: &str) -> Option<&'a SExpression> {
    match kind {
        "create" => def.create_policy.as_ref(),
        "update" => def.update_policy.as_ref(),
        _ => def.delete_policy.as_ref(),
    }
}

/// A viewer role the entity's own policy denies for `kind`: the first role of the identity entity's declared
/// vocabulary that no clause of the policy names and that the policy, run on the persist's literal row, refuses.
fn denying_viewer(schema: &OrbitalSchema, entity: &str, kind: &str, data: Option<&SExpression>) -> Option<RoleWire> {
    use orbital_core_base::value::Value;
    let defs = orbital_core_base::policy::inline_entity_defs(schema);
    let policy = policy_of(defs.iter().find(|d| d.name == entity)?, kind)?;
    let identity = orbital_core_base::policy::identity_entity_def(schema)?;
    let vocab = orbital_core_base::entity_vocabulary::vocabulary(orbital_core_base::entity_vocabulary::identity_field(identity, "role")?)?;
    let mut literals = std::collections::HashMap::new();
    crate::entity_access::collect_user_field_literals(policy, &mut literals);
    let named = literals.get("role").cloned().unwrap_or_default();
    let mut row = serde_json::Map::new();
    if let Some(SExpression::Object(map)) = data {
        for (key, value) in map {
            let plain = match value {
                SExpression::Null => Some(serde_json::Value::Null),
                SExpression::Bool(b) => Some((*b).into()),
                SExpression::Number(n) => serde_json::Number::from_f64(*n).map(serde_json::Value::Number),
                SExpression::String(text) if !text.starts_with('@') => Some(text.clone().into()),
                _ => None,
            };
            if let Some(v) = plain {
                row.insert(key.clone(), v);
            }
        }
    }
    let entity_row: Value = serde_json::from_value(serde_json::Value::Object(row)).ok()?;
    for candidate in vocab {
        if named.contains(candidate) {
            continue;
        }
        let user: Value = serde_json::from_value(serde_json::json!({ "id": "transient-failure-probe-viewer", "role": candidate })).ok()?;
        let mut ctx = orbital_core_eval::evaluator::context::BindingContextBuilder::new()
            .entity(entity_row.clone())
            .user(user)
            .state(Value::String("transient-failure-probe".to_string()))
            .build();
        ctx.from_state = Some("transient-failure-probe".to_string());
        if matches!(orbital_core_eval::evaluator::Evaluator::new().evaluate(policy, &ctx), Ok(Value::Bool(false))) {
            return Some(RoleWire { field: "role".into(), value: candidate.clone() });
        }
    }
    None
}

/// Plan the forced failure of a transient failure arm: the arm the machine races past through an effect-emitted
/// sibling, so it is reached by making the entering persist fail for real.
pub fn plan_transient_probe(schema: &OrbitalSchema, query: &TransientQuery, options: &PlanOptions) -> Option<TransientOutcome> {
    let td = planner::inline_traits(schema).into_iter().find(|td| td.name == query.trait_name)?;
    let mut failures = Vec::new();
    td.state_machine.transitions.iter().flat_map(|t| t.effects.iter().flatten()).for_each(|e| collect_failure_events(e, &mut failures));
    if !failures.contains(&query.event) || planner::transient_closure(td, &query.from).len() <= 1 {
        return Some(TransientOutcome::NotTransient);
    }
    let entering = td.state_machine.transitions.iter().find_map(|t| {
        t.effects.iter().flatten().find_map(|e| persist_failing(e, &query.event)).map(|found| (t, found))
    });
    let Some((enter, (entity, kind, data))) = entering else {
        return Some(TransientOutcome::Finding { finding: TransientFinding::NoEnteringPersist { event: query.event.clone() } });
    };
    let Some(role) = denying_viewer(schema, &entity, kind, data.as_ref()) else {
        return Some(TransientOutcome::Finding {
            finding: TransientFinding::NoDenyingViewer { entity, operation: kind.to_string() },
        });
    };
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    let seed_id = td.linked_entity.as_deref().map(|e| planner::effective_seed_id(e, &owner_fields, options.viewer_id.as_deref()));
    let enter_from = enter.from.states().first().copied().unwrap_or("").to_string();
    let payload = planner::plan_trait_variants(td, &ctx, seed_id.as_deref())
        .into_iter()
        .find(|s| {
            s.from == enter_from
                && s.event == enter.event
                && s.payload_case == planner::PayloadCase::Success
                && s.guard_case != Some(planner::GuardCase::Fail)
        })
        .map_or_else(|| planner::synth_payload(td, &enter.event, &ctx, seed_id.as_deref()), |s| s.payload);
    let closure = planner::transient_closure(td, &enter.to);
    let setup = plan_wire::drive_setup(schema, td, &ctx, td.linked_entity.as_deref(), &enter_from, &enter.event, false);
    Some(TransientOutcome::Probe {
        step: Box::new(TransientProbe {
            trait_name: td.name.clone(),
            coverage_key: format!("{}:{enter_from}+{}->{}[transient-failure-probe:{}]", td.name, enter.event, enter.to, query.event),
            from: enter_from,
            event: enter.event.clone(),
            to: enter.to.clone(),
            payload,
            accept_states: if closure.len() > 1 { closure } else { Vec::new() },
            role,
            verifies_portal_for: query.clone(),
            instance_id: setup.instance_id,
            seed_row_id: setup.seed_row_id,
            preamble: setup.preamble,
        }),
    })
}

fn find_persist_effect(effects: &[SExpression]) -> Option<(String, &'static str, String)> {
    for effect in effects {
        let SExpression::Array(items) = effect else { continue };
        let (Some(SExpression::String(op)), Some(SExpression::String(kind)), Some(SExpression::String(entity))) =
            (items.first(), items.get(1), items.get(2))
        else {
            continue;
        };
        let kind = match (op.as_str(), kind.as_str()) {
            ("persist", "create") => "create",
            ("persist", "update") => "update",
            ("persist", "delete") => "delete",
            _ => continue,
        };
        for arg in items.iter().skip(3) {
            if let SExpression::Object(o) = arg {
                if let Some(SExpression::Object(emit)) = o.get("emit") {
                    if let Some(SExpression::String(success)) = emit.get("success") {
                        if !success.is_empty() {
                            return Some((entity.clone(), kind, success.clone()));
                        }
                    }
                }
            }
        }
        // A persist with no `emit.success` cannot drive a chain's success expectation.
        return None;
    }
    None
}

fn renders_overlay(node: &SExpression) -> bool {
    let SExpression::Array(items) = node else { return false };
    if let (Some(SExpression::String(op)), Some(SExpression::String(slot))) = (items.first(), items.get(1)) {
        if op == "render-ui" && items.len() >= 3 && !matches!(items[2], SExpression::Null) {
            if slot != "main" && slot != "sidebar" && orbital_core_base::slots::is_portal_slot(slot) {
                return true;
            }
            if let SExpression::Object(config) = &items[2] {
                if let Some(SExpression::String(pattern)) = config.get("type") {
                    if orbital_core_base::slots::SELF_OVERLAY_PATTERN_TYPES.contains(&pattern.as_str()) {
                        return true;
                    }
                }
            }
        }
    }
    items.iter().any(renders_overlay)
}

fn event_schema<'a>(td: &'a TraitDefinition, event: &str) -> &'a [orbital_core_base::schema::types::PayloadField] {
    td.state_machine
        .events
        .iter()
        .flatten()
        .find(|e| e.key == event)
        .and_then(|e| e.payload_schema.as_deref())
        .unwrap_or(&[])
}

fn viewer_requirement(schema: &OrbitalSchema, entity: &str, kind: &str) -> Option<ViewerRequirement> {
    let defs = orbital_core_base::policy::inline_entity_defs(schema);
    let policy = policy_of(defs.iter().find(|d| d.name == entity)?, kind)?;
    let operation = match kind {
        "create" => orbital_core_base::effect_types::PersistOperation::Create,
        "update" => orbital_core_base::effect_types::PersistOperation::Update,
        _ => orbital_core_base::effect_types::PersistOperation::Delete,
    };
    let role = crate::entity_access::role_satisfying_policy(schema, entity, operation)
        .map(|(field, value)| RoleWire { field, value });
    let owner = orbital_core_base::policy::owner_columns_from_policy(policy).into_iter().next().map(|column| {
        if kind == "create" {
            OwnerRequirement {
                source_entity: orbital_core_base::policy::identity_entity_name(schema).unwrap_or(entity).to_string(),
                source_field: "id".into(),
                use_default_id: Some(true),
                payload_owner_field: Some(column),
            }
        } else {
            OwnerRequirement { source_entity: entity.to_string(), source_field: column, use_default_id: None, payload_owner_field: None }
        }
    });
    Some(ViewerRequirement { role, owner })
}

/// Every create, edit and delete chain: a persistor trait's persist, paired through its `listens` with the
/// overlay trait whose open and submit transitions drive it.
pub fn plan_crud_chains(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<CrudChain> {
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    let all = planner::inline_traits(schema);
    let mut out = Vec::new();
    for persistor in scoped(schema, options) {
        if planner::initial_state(persistor).is_none() {
            continue;
        }
        let mut buckets: Vec<(String, Vec<(&'static str, String, &orbital_core_base::schema::types::TransitionDefinition)>)> = Vec::new();
        for transition in &persistor.state_machine.transitions {
            if transition.event == "INIT" {
                continue;
            }
            let Some((entity, kind, success)) = find_persist_effect(transition.effects.as_deref().unwrap_or(&[])) else { continue };
            let at = match buckets.iter().position(|(e, _)| *e == entity) {
                Some(at) => at,
                None => {
                    buckets.push((entity.clone(), Vec::new()));
                    buckets.len() - 1
                }
            };
            let bucket = &mut buckets[at].1;
            bucket.retain(|(k, _, _)| *k != kind);
            bucket.push((kind, success, transition));
        }
        for (entity, persists) in buckets {
            for wanted in ["create", "update", "delete"] {
                let Some((kind, success, transition)) = persists.iter().find(|(k, _, _)| *k == wanted) else { continue };
                if let Some(chain) = build_chain(schema, &ctx, &all, persistor, &entity, kind, success, transition) {
                    out.push(chain);
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn build_chain(
    schema: &OrbitalSchema,
    ctx: &planner::EntityCtx<'_>,
    all: &[&TraitDefinition],
    persistor: &TraitDefinition,
    entity: &str,
    kind: &str,
    success: &str,
    persist: &orbital_core_base::schema::types::TransitionDefinition,
) -> Option<CrudChain> {
    use orbital_core_base::schema::types::ListenSource;
    let listener = persistor.listens.iter().flatten().find(|l| l.triggers.as_deref() == Some(persist.event.as_str()))?;
    let Some(ListenSource::Trait { trait_name: source_name, .. }) = &listener.source else { return None };
    let source = all.iter().find(|t| &t.name == source_name)?;
    let initial = planner::initial_state(source)?;
    let open = source
        .state_machine
        .transitions
        .iter()
        .find(|t| t.from.matches(&initial) && t.to != initial && t.event != "INIT")?;
    let submit = source
        .state_machine
        .transitions
        .iter()
        .find(|t| t.from.matches(&open.to) && t.event == listener.event)?;
    let inbound = source.listens.iter().flatten().find(|l| l.triggers.as_deref() == Some(open.event.as_str()));
    let crud_kind = match kind {
        "create" => CrudKind::Create,
        "update" => CrudKind::Edit,
        _ => CrudKind::Delete,
    };
    let label = match crud_kind {
        CrudKind::Create => "crud-create",
        CrudKind::Edit => "crud-edit",
        CrudKind::Delete => "crud-delete",
    };
    let open_schema = event_schema(source, &open.event);
    let row_scoped = crud_kind != CrudKind::Create;
    let persist_from = persist.from.states().first().copied().unwrap_or("").to_string();
    Some(CrudChain {
        kind: crud_kind,
        entity: entity.to_string(),
        persistor: persistor.name.clone(),
        source_trait: source.name.clone(),
        open_from: open.from.states().first().copied().unwrap_or("").to_string(),
        open_event: open.event.clone(),
        open_to: open.to.clone(),
        submit_event: submit.event.clone(),
        success_event: success.to_string(),
        row_delta: match crud_kind {
            CrudKind::Create => 1,
            CrudKind::Edit => 0,
            CrudKind::Delete => -1,
        },
        open_affordance_event: inbound.map_or_else(|| open.event.clone(), |l| l.event.clone()),
        opens_overlay: open.effects.iter().flatten().any(renders_overlay),
        requires_row_context: row_scoped && open_schema.iter().any(|f| f.required == Some(true)),
        payload_row_shape: if crud_kind == CrudKind::Delete {
            open_schema
                .iter()
                .map(|f| RowShapeField { name: f.name.clone(), whole_row: f.entity.as_deref() == Some(entity) })
                .collect()
        } else {
            Vec::new()
        },
        viewer: viewer_requirement(schema, entity, kind),
        precondition: persist
            .guard
            .as_ref()
            .and_then(|_| precondition::plan_precondition(ctx, persistor, persist, &persist_from, &serde_json::json!({}))),
        coverage_key: format!("{}:{}+{}->{}[{label}]", source.name, open.from.states().first().copied().unwrap_or(""), open.event, open.to),
    })
}

pub fn plan_emit_sweep(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<EmitStep> {
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    let types = payload_solve::declared_types(schema);
    let mut out = Vec::new();
    for td in scoped(schema, options) {
        let Some(initial) = planner::initial_state(td) else { continue };
        let seed_id = td.linked_entity.as_deref().map(|e| planner::effective_seed_id(e, &owner_fields, options.viewer_id.as_deref()));
        let mut seen: Vec<String> = Vec::new();
        for event in declared_emits(td) {
            if seen.contains(&event) {
                continue;
            }
            seen.push(event.clone());
            let mut payload = planner::synth_payload(td, &event, &ctx, seed_id.as_deref());
            if let Some(arm) = td.state_machine.transitions.iter().find(|t| t.event == event) {
                for (field, text) in payload_solve::json_parse_payload(arm, &types, &ctx, seed_id.as_deref()) {
                    payload[field] = text;
                }
                let from = arm.from.states().first().copied().unwrap_or("").to_string();
                payload = payload_solve::solve_json_parse_fields(arm, true, &payload, &types, &from, &ctx, seed_id.as_deref());
            }
            out.push(EmitStep {
                trait_name: td.name.clone(),
                from: initial.clone(),
                navigates: planner::dispatch_navigates(schema, td, &event),
                coverage_key: format!("{}:{initial}+{event}->{initial}[emit]", td.name),
                event,
                payload,
            });
        }
    }
    out
}

fn collect_pattern_types(node: &SExpression, out: &mut Vec<String>) {
    match node {
        SExpression::Array(items) => items.iter().for_each(|i| collect_pattern_types(i, out)),
        SExpression::Object(map) => {
            if let Some(SExpression::String(pattern)) = map.get("type") {
                if !out.contains(pattern) {
                    out.push(pattern.clone());
                }
            }
            map.values().for_each(|v| collect_pattern_types(v, out));
        }
        _ => {}
    }
}

/// The events the patterns a program renders must emit, anchored on the first trait with a state machine. The
/// contract table is the installed language data's event contracts; without it nothing is planned.
pub fn plan_contract_events(schema: &OrbitalSchema, options: &PlanOptions) -> ContractEvents {
    if orbital_core_base::language_data::language_data().is_err() {
        return ContractEvents { available: false, steps: Vec::new() };
    }
    let traits = planner::inline_traits(schema);
    let Some((anchor, initial)) = traits.iter().find_map(|td| planner::initial_state(td).map(|s| (*td, s))) else {
        return ContractEvents { available: true, steps: Vec::new() };
    };
    let mut patterns = Vec::new();
    for effect in traits.iter().flat_map(|td| td.state_machine.transitions.iter()).flat_map(|t| t.effects.iter().flatten()) {
        if let SExpression::Array(items) = effect {
            if let (Some(SExpression::String(op)), Some(config)) = (items.first(), items.get(2)) {
                if op == "render-ui" {
                    collect_pattern_types(config, &mut patterns);
                }
            }
        }
    }
    let covered = options.covered_events.as_deref().unwrap_or(&[]);
    let mut seen: Vec<String> = Vec::new();
    let mut steps = Vec::new();
    for pattern in patterns {
        let Some(contract) = orbital_core_base::language::pattern_registry::get_event_contract(&pattern) else { continue };
        for emit in &contract.emits {
            if emit.optional || seen.contains(&emit.event) || covered.contains(&emit.event) {
                continue;
            }
            seen.push(emit.event.clone());
            steps.push(ContractStep {
                trait_name: anchor.name.clone(),
                from: initial.clone(),
                coverage_key: format!("{}:{initial}+{}->{initial}[contract:{pattern}]", anchor.name, emit.event),
                event: emit.event.clone(),
                pattern: pattern.clone(),
            });
        }
    }
    ContractEvents { available: true, steps }
}

/// Plan the click on a rendered affordance the host found: standing in `from`, firing `event`.
pub fn plan_affordance(schema: &OrbitalSchema, query: &AffordanceQuery, options: &PlanOptions) -> Option<AffordanceStep> {
    let td = planner::inline_traits(schema).into_iter().find(|td| td.name == query.trait_name)?;
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    let arms: Vec<_> = td
        .state_machine
        .transitions
        .iter()
        .filter(|t| t.from.matches(&query.from) && t.event == query.event)
        .collect();
    let to = arms.first().map_or_else(|| query.from.clone(), |t| t.to.clone());
    let boots = query.event == "INIT" && planner::initial_state(td).as_deref() == Some(query.from.as_str());
    let setup = plan_wire::drive_setup(schema, td, &ctx, td.linked_entity.as_deref(), &query.from, &query.event, boots);
    let precondition = arms
        .iter()
        .find(|t| t.guard.is_some())
        .and_then(|t| precondition::plan_precondition(&ctx, td, t, &query.from, &serde_json::json!({})));
    Some(AffordanceStep {
        trait_name: td.name.clone(),
        coverage_key: format!("{}:{}+{}->{to}[affordance]", td.name, query.from, query.event),
        from: query.from.clone(),
        event: query.event.clone(),
        declared: !arms.is_empty(),
        navigates: planner::dispatch_navigates(schema, td, &query.event),
        to,
        instance_id: setup.instance_id,
        seed_row_id: setup.seed_row_id,
        role: setup.role,
        preamble: setup.preamble,
        precondition,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(extra_transitions: serde_json::Value) -> OrbitalSchema {
        let mut transitions = vec![
            json!({ "from": "idle", "to": "idle", "event": "INIT" }),
            json!({ "from": "idle", "to": "loading", "event": "GO", "effects": [
                ["fetch", "Job", { "emit": { "success": "LOADED", "failure": "LOAD_FAILED" } }],
                ["render-ui", "main", { "type": "data-grid", "entity": "Job", "children": [{ "type": "button", "action": "GO" }] }]
            ] }),
            json!({ "from": "loading", "to": "ready", "event": "LOADED" }),
            json!({ "from": "loading", "to": "idle", "event": "LOAD_FAILED" }),
            json!({ "from": "ready", "to": "idle", "event": "LEAVE", "effects": [["navigate", "/away"]] }),
        ];
        transitions.extend(extra_transitions.as_array().unwrap().clone());
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [
                    { "name": "id", "type": "string" }, { "name": "activeId", "type": "string", "default": "" }] },
                "traits": [
                    { "name": "Runner", "linkedEntity": "Job",
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "loading" }, { "name": "ready" }],
                        "events": [{ "key": "LOADED", "name": "LOADED", "payloadSchema": [{ "name": "count", "type": "number" }] }],
                        "transitions": transitions } },
                    { "name": "Picker", "linkedEntity": "Job",
                      "stateMachine": {
                        "states": [{ "name": "waiting", "isInitial": true }, { "name": "picked" }],
                        "events": [{ "key": "SELECT", "name": "SELECT", "payloadSchema": [{ "name": "id", "type": "string" }] }],
                        "transitions": [
                            { "from": "waiting", "to": "waiting", "event": "INIT" },
                            { "from": "waiting", "to": "picked", "event": "SELECT", "effects": [["set", "@entity.activeId", "@payload.id"]] }
                        ] } }
                ]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn the_emit_sweep_fires_each_declared_emit_once_from_the_initial_state() {
        let steps = plan_emit_sweep(&schema(json!([])), &PlanOptions::default());
        let events: Vec<&str> = steps.iter().map(|s| s.event.as_str()).collect();
        assert_eq!(events, vec!["LOADED", "LOAD_FAILED"]);
        let loaded = &steps[0];
        assert_eq!((loaded.trait_name.as_str(), loaded.from.as_str()), ("Runner", "idle"));
        assert_eq!(loaded.coverage_key, "Runner:idle+LOADED->idle[emit]");
        assert!(loaded.payload["count"].is_number(), "payload is built from the event's declared schema: {}", loaded.payload);
        assert!(!loaded.navigates);
    }

    #[test]
    fn control_a_trait_that_declares_no_emit_sweeps_nothing_and_an_emit_that_navigates_is_flagged() {
        let none = plan_emit_sweep(&schema(json!([])), &PlanOptions { traits: Some(vec!["Picker".into()]), ..Default::default() });
        assert!(none.is_empty());
        let navigating = schema(json!([
            { "from": "idle", "to": "idle", "event": "SAVE", "effects": [["persist", "create", "Job", {}, { "emit": { "success": "LEAVE" } }]] }
        ]));
        let steps = plan_emit_sweep(&navigating, &PlanOptions::default());
        assert!(steps.iter().find(|s| s.event == "LEAVE").is_some_and(|s| s.navigates), "{steps:?}");
    }

    #[test]
    fn contract_events_plan_the_non_optional_emits_of_the_patterns_a_program_renders() {
        let plan = plan_contract_events(&schema(json!([])), &PlanOptions::default());
        assert!(plan.available, "the installed language data supplies the event contracts");
        let grid: Vec<&ContractStep> = plan.steps.iter().filter(|s| s.pattern == "data-grid").collect();
        assert!(!grid.is_empty(), "data-grid is rendered: {:?}", plan.steps);
        for step in &grid {
            assert_eq!((step.trait_name.as_str(), step.from.as_str()), ("Runner", "idle"));
            assert_eq!(step.coverage_key, format!("Runner:idle+{}->idle[contract:data-grid]", step.event));
        }
        assert!(plan.steps.iter().all(|s| s.pattern != "form-section"), "control: a pattern the program never renders plans nothing");
    }

    #[test]
    fn contract_events_skip_the_events_the_caller_already_covered() {
        let all = plan_contract_events(&schema(json!([])), &PlanOptions::default());
        let first = all.steps[0].event.clone();
        let covered = PlanOptions { covered_events: Some(vec![first.clone()]), ..Default::default() };
        let rest = plan_contract_events(&schema(json!([])), &covered);
        assert!(rest.steps.iter().all(|s| s.event != first));
        assert_eq!(rest.steps.len() + 1, all.steps.len());
    }

    fn affordance(schema: &OrbitalSchema, trait_name: &str, from: &str, event: &str) -> Option<AffordanceStep> {
        plan_affordance(
            schema,
            &AffordanceQuery { trait_name: trait_name.into(), from: from.into(), event: event.into() },
            &PlanOptions::default(),
        )
    }

    #[test]
    fn an_affordance_is_planned_from_the_state_that_renders_it_with_its_target_and_navigation() {
        let program = schema(json!([]));
        let go = affordance(&program, "Runner", "idle", "GO").unwrap();
        assert_eq!((go.to.as_str(), go.declared, go.navigates), ("loading", true, false));
        assert_eq!(go.coverage_key, "Runner:idle+GO->loading[affordance]");
        assert_eq!(
            go.preamble.as_ref().map(|p| p.iter().map(|e| e.event.as_str()).collect::<Vec<_>>()),
            Some(vec!["INIT"]),
            "standing in the initial state replays the boot INIT"
        );
        let leave = affordance(&program, "Runner", "ready", "LEAVE").unwrap();
        assert!(leave.navigates);
        assert_eq!(leave.preamble.as_ref().map(|p| p.iter().map(|e| e.event.as_str()).collect::<Vec<_>>()), Some(vec!["GO", "LOADED"]));
    }

    #[test]
    fn control_a_button_for_an_event_the_state_does_not_handle_is_a_dead_affordance_and_an_unknown_trait_is_none() {
        let program = schema(json!([]));
        let dead = affordance(&program, "Runner", "idle", "LEAVE").unwrap();
        assert_eq!((dead.to.as_str(), dead.declared), ("idle", false));
        assert!(affordance(&program, "Nobody", "idle", "GO").is_none());
    }

    #[test]
    fn a_guarded_affordance_carries_the_precondition_its_guard_needs() {
        let guarded = schema(json!([
            { "from": "idle", "to": "idle", "event": "PRESS", "guard": "@entity.activeId" }
        ]));
        let press = affordance(&guarded, "Runner", "idle", "PRESS").unwrap();
        assert!(matches!(press.precondition, Some(Precondition::Established { ref trait_name, .. }) if trait_name == "Picker"), "{press:?}");
    }

    fn transient_schema(policy: serde_json::Value, failing: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }],
                            "create_policy": policy },
                "auxiliaryEntities": [{ "name": "Member", "persistence": "runtime", "identity": true, "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "role", "type": "enum", "values": ["admin", "viewer"] }] }],
                "traits": [{ "name": "Flow", "linkedEntity": "Note",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "saving" }, { "name": "saved" }, { "name": "failed" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "saving", "event": "START", "effects": [failing] },
                            { "from": "saving", "to": "saved", "event": "DONE" },
                            { "from": "saving", "to": "failed", "event": "FAILED" },
                            { "from": "saving", "to": "failed", "event": "FETCH_FAILED" }
                        ] } }]
            }]
        }))
        .unwrap()
    }

    fn transient(schema: &OrbitalSchema, event: &str) -> Option<TransientOutcome> {
        plan_transient_probe(
            schema,
            &TransientQuery { trait_name: "Flow".into(), from: "saving".into(), event: event.into(), to: "failed".into() },
            &PlanOptions::default(),
        )
    }

    fn persisting() -> serde_json::Value {
        json!(["persist", "create", "Note", { "title": "x" }, { "emit": { "success": "DONE", "failure": "FAILED" } }])
    }

    #[test]
    fn a_transient_failure_arm_is_forced_through_the_entering_persist_as_a_denied_viewer() {
        let program = transient_schema(json!(["=", "@user.role", "admin"]), persisting());
        let Some(TransientOutcome::Probe { step }) = transient(&program, "FAILED") else { panic!("expected a probe") };
        assert_eq!((step.from.as_str(), step.event.as_str(), step.to.as_str()), ("idle", "START", "saving"));
        assert_eq!(step.role, RoleWire { field: "role".into(), value: "viewer".into() });
        assert_eq!(step.accept_states, vec!["saving".to_string(), "saved".to_string(), "failed".to_string()]);
        assert_eq!(step.verifies_portal_for.event, "FAILED");
        assert_eq!(step.coverage_key, "Flow:idle+START->saving[transient-failure-probe:FAILED]");
    }

    #[test]
    fn control_an_event_that_is_no_failure_route_or_with_no_denying_viewer_or_persist_owes_no_probe() {
        let program = transient_schema(json!(["=", "@user.role", "admin"]), persisting());
        assert_eq!(transient(&program, "DONE"), Some(TransientOutcome::NotTransient));
        let open = transient_schema(json!(true), persisting());
        assert_eq!(
            transient(&open, "FAILED"),
            Some(TransientOutcome::Finding { finding: TransientFinding::NoDenyingViewer { entity: "Note".into(), operation: "create".into() } })
        );
        let fetching = transient_schema(
            json!(["=", "@user.role", "admin"]),
            json!(["fetch", "Note", { "emit": { "success": "DONE", "failure": "FETCH_FAILED" } }]),
        );
        assert_eq!(
            transient(&fetching, "FETCH_FAILED"),
            Some(TransientOutcome::Finding { finding: TransientFinding::NoEnteringPersist { event: "FETCH_FAILED".into() } })
        );
    }

    fn crud_schema(open_effect: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
                "traits": [
                    { "name": "Notes", "linkedEntity": "Note",
                      "listens": [
                          { "event": "NOTE_SAVE", "triggers": "DO_CREATE", "source": { "kind": "trait", "trait": "NoteModal" } },
                          { "event": "CONFIRM_DELETE", "triggers": "DO_DELETE", "source": { "kind": "trait", "trait": "NoteConfirm" } }
                      ],
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "DO_CREATE", "effects": [["persist", "create", "Note", "@payload.data", { "emit": { "success": "NOTE_CREATED" } }]] },
                            { "from": "idle", "to": "idle", "event": "DO_DELETE", "effects": [["persist", "delete", "Note", "@payload.id", { "emit": { "success": "NOTE_DELETED" } }]] }
                        ] } },
                    { "name": "NoteModal", "linkedEntity": "Note",
                      "stateMachine": {
                        "states": [{ "name": "closed", "isInitial": true }, { "name": "open" }],
                        "transitions": [
                            { "from": "closed", "to": "open", "event": "OPEN", "effects": [open_effect] },
                            { "from": "open", "to": "closed", "event": "NOTE_SAVE" }
                        ] } },
                    { "name": "NoteConfirm", "linkedEntity": "Note",
                      "listens": [{ "event": "REQUEST_DELETE", "triggers": "ASK", "source": { "kind": "trait", "trait": "NoteBrowse" } }],
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "confirming" }],
                        "events": [{ "key": "ASK", "name": "ASK", "payloadSchema": [
                            { "name": "id", "type": "string", "required": true },
                            { "name": "row", "type": "object", "entity": "Note" }] }],
                        "transitions": [
                            { "from": "idle", "to": "confirming", "event": "ASK", "effects": [["render-ui", "modal", { "type": "confirm-dialog" }]] },
                            { "from": "confirming", "to": "idle", "event": "CONFIRM_DELETE" }
                        ] } }
                ]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn crud_chains_pair_each_persist_with_the_overlay_trait_that_drives_it() {
        let chains = plan_crud_chains(&crud_schema(json!(["render-ui", "modal", { "type": "form-section" }])), &PlanOptions::default());
        let create = chains.iter().find(|c| c.kind == CrudKind::Create).expect("a create chain");
        assert_eq!((create.persistor.as_str(), create.source_trait.as_str()), ("Notes", "NoteModal"));
        assert_eq!((create.open_event.as_str(), create.submit_event.as_str()), ("OPEN", "NOTE_SAVE"));
        assert_eq!((create.success_event.as_str(), create.row_delta, create.opens_overlay), ("NOTE_CREATED", 1, true));
        assert_eq!(create.coverage_key, "NoteModal:closed+OPEN->open[crud-create]");
        let delete = chains.iter().find(|c| c.kind == CrudKind::Delete).expect("a delete chain");
        assert_eq!((delete.row_delta, delete.open_affordance_event.as_str(), delete.opens_overlay), (-1, "REQUEST_DELETE", true));
        assert!(delete.requires_row_context);
        assert_eq!(
            delete.payload_row_shape,
            vec![RowShapeField { name: "id".into(), whole_row: false }, RowShapeField { name: "row".into(), whole_row: true }]
        );
    }

    #[test]
    fn control_an_open_arm_that_renders_inline_is_not_an_overlay_and_a_persist_without_a_listener_has_no_chain() {
        let inline = plan_crud_chains(&crud_schema(json!(["render-ui", "main", { "type": "table-view" }])), &PlanOptions::default());
        assert!(inline.iter().find(|c| c.kind == CrudKind::Create).is_some_and(|c| !c.opens_overlay));
        assert!(inline.iter().all(|c| c.kind != CrudKind::Edit), "no update persist, no edit chain");
    }
}
