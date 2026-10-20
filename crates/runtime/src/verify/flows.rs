//! The ordering and expectations of the verifier's flow planners: the emit sweep, the pattern event contracts,
//! affordance clicks, forced-failure probes and the CRUD chains. A host finds what it can only see (the
//! rendered buttons, forms and portals) and asks the kernel what to fire and what to expect.

use crate::planner::{self, Precondition};
use crate::verify::plan_wire::{self, PlanOptions, PreambleEvent, RoleWire};
use crate::verify::{payload_solve, precondition};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::schema::types::TraitDefinition;
use serde::{Deserialize, Serialize};

#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
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
    /// A listener persists an update or delete keyed by this emit's payload: the host binds a real row there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_row: Option<EmitRowBinding>,
}

/// The emitted payload field a listener's persist reads as its target row (`whole_row`: the field carries the row).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmitRowBinding {
    pub entity: String,
    pub payload_field: String,
    pub whole_row: bool,
    /// A delete's restrict relations: the host binds a row nothing references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid_referenced_via: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid_referenced_by_other_entities: Vec<InboundRelation>,
}

/// The row a listener of `event` from `td` writes by an id the emit carries, as an emitted payload field.
fn emit_row_binding(
    schema: &OrbitalSchema,
    td: &TraitDefinition,
    event: &str,
) -> Option<EmitRowBinding> {
    planner::listeners_of(schema, td, event)
        .into_iter()
        .find_map(|heard| {
            heard
                .trait_def
                .state_machine
                .transitions
                .iter()
                .filter(|t| t.event == heard.triggers)
                .find_map(|arm| {
                    let (entity, kind, _) =
                        find_persist_kind(arm.effects.as_deref().unwrap_or(&[]))?;
                    if kind == MutationKind::Create {
                        return None;
                    }
                    let binding = persist_binding(heard.trait_def, arm, &entity)?;
                    let payload_field = match &heard.listen.payload_mapping {
                        None => binding.payload_field,
                        Some(mapping) => match mapping.get(&binding.payload_field)? {
                            SExpression::String(text) => bare_payload_field(text)?.to_string(),
                            _ => return None,
                        },
                    };
                    let (avoid_referenced_via, avoid_referenced_by_other_entities) =
                        if kind == MutationKind::Delete {
                            inbound_restrict(schema, &entity)
                        } else {
                            (Vec::new(), Vec::new())
                        };
                    Some(EmitRowBinding {
                        entity,
                        payload_field,
                        whole_row: binding.whole_row,
                        avoid_referenced_via,
                        avoid_referenced_by_other_entities,
                    })
                })
        })
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
pub struct ReplayQuery {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub state: String,
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
    let resolved = options
        .traits
        .as_deref()
        .and_then(|names| planner::resolve_trait_names(schema, names).ok());
    planner::inline_traits_scoped(schema, resolved.as_deref())
}

fn entity_ctx<'a>(
    schema: &'a OrbitalSchema,
    owner_fields: &'a [(String, String)],
    options: &PlanOptions,
) -> planner::EntityCtx<'a> {
    planner::EntityCtx {
        schema,
        owner_fields,
        viewer_id: options.viewer_id.clone(),
    }
}

/// What a trait's sweep fires: every event its effects emit on an outcome, then its declared `emits` contracts,
/// minus the reply routes of an `http` effect (the reply arrives inside the request's own dispatch) and
/// external names (`@…`).
fn declared_emits(schema: &OrbitalSchema, td: &TraitDefinition) -> Vec<String> {
    let routes = planner::http_reply_routes(schema);
    let is_route = |event: &str| routes.contains_key(&(td.name.clone(), event.to_string()));
    let mut out: Vec<String> = Vec::new();
    for event in planner::effect_emitted_events_in_order(td) {
        if !is_route(&event) {
            out.push(event);
        }
    }
    for contract in td.emits.iter().flatten() {
        if !contract.event.starts_with('@')
            && !is_route(&contract.event)
            && !out.contains(&contract.event)
        {
            out.push(contract.event.clone());
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
    Probe {
        step: Box<TransientProbe>,
    },
    Finding {
        finding: TransientFinding,
    },
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
    /// The payload the submit event carries, built from its declared schema.
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub submit_payload: serde_json::Value,
    /// A minimal row of the linked entity: the values a form's fields are filled with.
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub entity_row: serde_json::Value,
    /// The same row with every non-id value changed, so an edit has something to change.
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub edit_entity_row: serde_json::Value,
    /// Entity fields restricted to declared values: a changed value would leave the vocabulary, so a row-content
    /// assertion skips them.
    pub enum_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer: Option<ViewerRequirement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precondition: Option<Precondition>,
    /// Delete only: self-relation fields (restrict rule) a deletable row must not be referenced through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid_referenced_via: Vec<String>,
    /// Delete only: fields of OTHER entities (restrict rule) that reference this entity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid_referenced_by_other_entities: Vec<InboundRelation>,
    /// Delete only: when the entity can run out of unreferenced rows, the create to dispatch first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete_establish_fallback: Option<EstablishFallback>,
    pub coverage_key: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundRelation {
    pub entity: String,
    pub field: String,
}

#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EstablishFallback {
    pub event: String,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: serde_json::Value,
    #[serde(rename = "trait")]
    pub trait_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer: Option<ViewerRequirement>,
}

fn persist_failing(
    node: &SExpression,
    event: &str,
) -> Option<(String, &'static str, Option<SExpression>)> {
    let SExpression::Array(items) = node else {
        return None;
    };
    if let (
        Some(SExpression::String(op)),
        Some(SExpression::String(kind)),
        Some(SExpression::String(entity)),
    ) = (items.first(), items.get(1), items.get(2))
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
    let SExpression::Array(items) = node else {
        return;
    };
    if matches!(items.first(), Some(SExpression::String(op)) if matches!(op.as_str(), "persist" | "fetch" | "call-service"))
    {
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
    items
        .iter()
        .for_each(|child| collect_failure_events(child, out));
}

fn policy_of<'a>(
    def: &'a orbital_core_base::schema::types::EntityDefinition,
    kind: &str,
) -> Option<&'a SExpression> {
    match kind {
        "create" => def.create_policy.as_ref(),
        "update" => def.update_policy.as_ref(),
        _ => def.delete_policy.as_ref(),
    }
}

/// A viewer role the entity's own policy denies for `kind`: the first role of the identity entity's declared
/// vocabulary that no clause of the policy names and that the policy, run on the persist's literal row, refuses.
fn denying_viewer(
    schema: &OrbitalSchema,
    entity: &str,
    kind: &str,
    data: Option<&SExpression>,
) -> Option<RoleWire> {
    use orbital_core_base::value::Value;
    let defs = orbital_core_base::policy::inline_entity_defs(schema);
    let policy = policy_of(defs.iter().find(|d| d.name == entity)?, kind)?;
    let identity = orbital_core_base::policy::identity_entity_def(schema)?;
    let vocab = orbital_core_base::entity_vocabulary::vocabulary(
        orbital_core_base::entity_vocabulary::identity_field(identity, "role")?,
    )?;
    let mut literals = std::collections::HashMap::new();
    crate::entity_access::collect_user_field_literals(policy, &mut literals);
    let named = literals.get("role").cloned().unwrap_or_default();
    let mut row = serde_json::Map::new();
    if let Some(SExpression::Object(map)) = data {
        for (key, value) in map {
            let plain = match value {
                SExpression::Null => Some(serde_json::Value::Null),
                SExpression::Bool(b) => Some((*b).into()),
                SExpression::Number(n) => {
                    serde_json::Number::from_f64(*n).map(serde_json::Value::Number)
                }
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
        let user: Value = serde_json::from_value(
            serde_json::json!({ "id": "transient-failure-probe-viewer", "role": candidate }),
        )
        .ok()?;
        let mut ctx = orbital_core_eval::evaluator::context::BindingContextBuilder::new()
            .entity(entity_row.clone())
            .user(user)
            .state(Value::String("transient-failure-probe".to_string()))
            .build();
        ctx.from_state = Some("transient-failure-probe".to_string());
        if matches!(
            orbital_core_eval::evaluator::Evaluator::new().evaluate(policy, &ctx),
            Ok(Value::Bool(false))
        ) {
            return Some(RoleWire {
                field: "role".into(),
                value: candidate.clone(),
            });
        }
    }
    None
}

/// Plan the forced failure of a transient failure arm: the arm the machine races past through an effect-emitted
/// sibling, so it is reached by making the entering persist fail for real.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TickStep {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    /// The tick's name, fired as the step's event.
    pub event: String,
    pub wait_ms: u64,
    /// A tick's effects may move the trait anywhere in its declared topology: any declared state is a landing.
    pub accept_states: Vec<String>,
    pub coverage_key: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MutationKind {
    Create,
    Update,
    Delete,
}

/// The top-level payload field a persist reads its target row (or id) from.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistBinding {
    pub payload_field: String,
    pub whole_row: bool,
}

/// How a step that writes an existing row gets that row to exist.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RowEstablish {
    /// Dispatch `event` first (a create self-loop, or a transition that binds `@entity.id` from the payload).
    Established {
        event: String,
        #[cfg_attr(test, ts(type = "JsonValue"))]
        payload: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        viewer: Option<ViewerRequirement>,
        #[serde(rename = "bindRowFrom", skip_serializing_if = "Option::is_none")]
        bind_row_from: Option<crate::planner::BindRowFrom>,
        /// Dispatched before the replay path rather than inline with the step.
        #[serde(rename = "beforeReplay")]
        before_replay: bool,
    },
    /// The trait never binds this entity's row identity at that state: the row can never exist before the step.
    Unreachable {
        #[serde(rename = "trait")]
        trait_name: String,
        entity: String,
        #[serde(rename = "atState")]
        at_state: String,
    },
}

/// A persist-writing transition planned as a data-mutation step.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataMutation {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    pub to: String,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: serde_json::Value,
    pub entity: String,
    pub kind: MutationKind,
    pub row_delta: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer: Option<ViewerRequirement>,
    /// Update or delete whose own persist argument names the target row: the host binds a real row there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_row: Option<PersistBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid_referenced_via: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid_referenced_by_other_entities: Vec<InboundRelation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_establish: Option<RowEstablish>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precondition: Option<Precondition>,
    pub coverage_key: String,
}

/// A numeric tick is waited out from the initial state; a frame tick is driven by the render loop and a
/// non-positive interval is invalid, so neither is planned.
pub fn plan_tick_steps(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<TickStep> {
    let mut out = Vec::new();
    for td in scoped(schema, options) {
        let Some(initial) = planner::initial_state(td) else {
            continue;
        };
        let mut declared = vec![initial.clone()];
        for t in &td.state_machine.transitions {
            for state in t
                .from
                .states()
                .into_iter()
                .chain(std::iter::once(t.to.as_str()))
            {
                if state != "*" && !declared.iter().any(|d| d == state) {
                    declared.push(state.to_string());
                }
            }
        }
        for tick in td.ticks.iter().flatten() {
            let Some(wait_ms) = tick.duration_ms().filter(|ms| *ms > 0) else {
                continue;
            };
            out.push(TickStep {
                trait_name: td.name.clone(),
                from: initial.clone(),
                event: tick.name.clone(),
                wait_ms,
                accept_states: declared.clone(),
                coverage_key: format!("{}:tick({})", td.name, tick.name),
            });
        }
    }
    out
}

fn bare_payload_field(text: &str) -> Option<&str> {
    let field = text.strip_prefix("@payload.")?;
    (!field.is_empty() && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then_some(field)
}

/// The first `persist create|update|delete` of a transition's top-level effects: entity, kind, success event.
fn find_persist_kind(effects: &[SExpression]) -> Option<(String, MutationKind, Option<String>)> {
    for effect in effects {
        let SExpression::Array(items) = effect else {
            continue;
        };
        let (
            Some(SExpression::String(op)),
            Some(SExpression::String(kind)),
            Some(SExpression::String(entity)),
        ) = (items.first(), items.get(1), items.get(2))
        else {
            continue;
        };
        let kind = match (op.as_str(), kind.as_str()) {
            ("persist", "create") => MutationKind::Create,
            ("persist", "update") => MutationKind::Update,
            ("persist", "delete") => MutationKind::Delete,
            _ => continue,
        };
        let success = items.iter().skip(3).find_map(|arg| match arg {
            SExpression::Object(o) => match o.get("emit") {
                Some(SExpression::Object(emit)) => match emit.get("success") {
                    Some(SExpression::String(s)) if !s.is_empty() => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        });
        return Some((entity.clone(), kind, success));
    }
    None
}

fn persist_calls<'a>(
    effects: &'a [SExpression],
    wanted: &[&str],
) -> Vec<(&'a str, &'a str, Option<&'a SExpression>)> {
    effects
        .iter()
        .filter_map(|e| match e {
            SExpression::Array(items) => match (items.first(), items.get(1), items.get(2)) {
                (
                    Some(SExpression::String(op)),
                    Some(SExpression::String(kind)),
                    Some(SExpression::String(name)),
                ) if op == "persist" && wanted.contains(&kind.as_str()) => {
                    Some((kind.as_str(), name.as_str(), items.get(3)))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The top-level payload field a persist update or delete reads its target row (or id) from.
fn persist_binding(
    td: &TraitDefinition,
    transition: &orbital_core_base::schema::types::TransitionDefinition,
    entity: &str,
) -> Option<PersistBinding> {
    let effects = transition.effects.as_deref().unwrap_or(&[]);
    let schema = event_schema(td, &transition.event);
    let persists = |wanted: &[&str]| persist_calls(effects, wanted);
    for (_, name, reference) in persists(&["update"]) {
        if let Some(SExpression::String(text)) = reference {
            if let Some(field) = bare_payload_field(text) {
                if schema
                    .iter()
                    .any(|f| f.name == field && f.entity.as_deref() == Some(name))
                {
                    return Some(PersistBinding {
                        payload_field: field.to_string(),
                        whole_row: true,
                    });
                }
            }
        }
    }
    for (_, name, reference) in persists(&["update", "delete"]) {
        if name != entity {
            continue;
        }
        match reference {
            Some(SExpression::String(text)) => {
                if let Some(field) = bare_payload_field(text) {
                    return Some(PersistBinding {
                        payload_field: field.to_string(),
                        whole_row: false,
                    });
                }
            }
            Some(SExpression::Object(o)) => {
                if let Some(SExpression::String(id)) = o.get("id") {
                    if let Some(field) = bare_payload_field(id) {
                        return Some(PersistBinding {
                            payload_field: field.to_string(),
                            whole_row: false,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let set_path = effects
        .iter()
        .find_map(|e| precondition::find_set(e, "@entity.id"))
        .and_then(|v| match v {
            SExpression::String(text) => text.strip_prefix("@payload.").map(str::to_string),
            _ => None,
        });
    let writes_entity_row = persists(&["update", "delete"]).into_iter().any(|(_, name, reference)| {
        name == entity && matches!(reference, Some(SExpression::String(r)) if r == "@entity" || r == "@entity.id")
    });
    set_path
        .filter(|_| writes_entity_row)
        .map(|payload_field| PersistBinding {
            payload_field,
            whole_row: false,
        })
}

fn sets_entity_id(transition: &orbital_core_base::schema::types::TransitionDefinition) -> bool {
    transition
        .effects
        .iter()
        .flatten()
        .any(|e| precondition::find_set(e, "@entity.id").is_some())
}

fn entity_id_payload_path(
    transition: &orbital_core_base::schema::types::TransitionDefinition,
) -> Option<String> {
    transition.effects.iter().flatten().find_map(|e| {
        match precondition::find_set(e, "@entity.id") {
            Some(SExpression::String(text)) => text.strip_prefix("@payload.").map(str::to_string),
            _ => None,
        }
    })
}

/// How a step that writes an existing row gets that row to exist, evaluated at `at_state`: a create self-loop
/// that binds `@entity.id`, else a transition that binds it from a payload field, else nothing can.
fn resolve_row_establish(
    schema: &OrbitalSchema,
    ctx: &planner::EntityCtx<'_>,
    td: &TraitDefinition,
    entity: &str,
    at_state: &str,
    before_replay: bool,
) -> RowEstablish {
    let seed = td
        .linked_entity
        .as_deref()
        .map(|e| planner::effective_seed_id(e, ctx.owner_fields, ctx.viewer_id.as_deref()));
    let from_here = |t: &orbital_core_base::schema::types::TransitionDefinition| {
        t.from.matches(at_state) || t.from.matches("*")
    };
    let creator = td.state_machine.transitions.iter().find(|t| {
        t.event != "INIT"
            && from_here(t)
            && t.to == at_state
            && matches!(find_persist_kind(t.effects.as_deref().unwrap_or(&[])), Some((e, MutationKind::Create, _)) if e == entity)
            && sets_entity_id(t)
    });
    if let Some(creator) = creator {
        return RowEstablish::Established {
            event: creator.event.clone(),
            payload: planner::synth_payload(td, &creator.event, ctx, seed.as_deref()),
            viewer: viewer_requirement(schema, entity, "create"),
            bind_row_from: None,
            before_replay,
        };
    }
    let selector = td.state_machine.transitions.iter().find_map(|t| {
        if t.event == "INIT" || !from_here(t) {
            return None;
        }
        entity_id_payload_path(t)
            .filter(|p| !p.contains('.'))
            .map(|p| (t, p))
    });
    if let Some((selector, payload_field)) = selector {
        return RowEstablish::Established {
            event: selector.event.clone(),
            payload: planner::synth_payload(td, &selector.event, ctx, seed.as_deref()),
            viewer: None,
            bind_row_from: Some(planner::BindRowFrom {
                entity: entity.to_string(),
                payload_field,
            }),
            before_replay,
        };
    }
    RowEstablish::Unreachable {
        trait_name: td.name.clone(),
        entity: entity.to_string(),
        at_state: at_state.to_string(),
    }
}

fn plan_row_establish(
    schema: &OrbitalSchema,
    ctx: &planner::EntityCtx<'_>,
    td: &TraitDefinition,
    entity: &str,
    from: &str,
    initial: &str,
) -> Option<RowEstablish> {
    if from == "*" {
        return None;
    }
    if from == initial {
        return Some(resolve_row_establish(
            schema, ctx, td, entity, initial, false,
        ));
    }
    let path = planner::replay_transitions(td, from)?;
    let establishes = path.iter().any(|hop| {
        entity_id_payload_path(hop).is_some()
            || (matches!(find_persist_kind(hop.effects.as_deref().unwrap_or(&[])), Some((e, MutationKind::Create, _)) if e == entity)
                && sets_entity_id(hop))
    });
    if establishes {
        return None;
    }
    Some(resolve_row_establish(
        schema, ctx, td, entity, initial, true,
    ))
}

/// Every persist-writing transition a host can drive as a data-mutation step: not INIT, not fired by an effect's
/// own outcome, not behind a constant-false guard.
pub fn plan_data_mutations(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<DataMutation> {
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    let mut out = Vec::new();
    for td in scoped(schema, options) {
        let Some(initial) = planner::initial_state(td) else {
            continue;
        };
        let emitted = planner::effect_emitted_events(td);
        let seed = td
            .linked_entity
            .as_deref()
            .map(|e| planner::effective_seed_id(e, &owner_fields, options.viewer_id.as_deref()));
        for transition in &td.state_machine.transitions {
            if transition.event == "INIT" || emitted.contains(&transition.event) {
                continue;
            }
            if transition.guard.as_ref().and_then(planner::const_truth) == Some(false) {
                continue;
            }
            let Some((entity, kind, success_event)) =
                find_persist_kind(transition.effects.as_deref().unwrap_or(&[]))
            else {
                continue;
            };
            let from = transition
                .from
                .states()
                .first()
                .copied()
                .unwrap_or("")
                .to_string();
            let bind_row = if kind == MutationKind::Create {
                None
            } else {
                persist_binding(td, transition, &entity)
            };
            let mut row_establish = if kind == MutationKind::Create || bind_row.is_some() {
                None
            } else {
                plan_row_establish(schema, &ctx, td, &entity, &from, &initial)
            };
            let mut precondition = None;
            if row_establish.is_none() && transition.guard.is_some() {
                precondition = precondition::plan_precondition(
                    &ctx,
                    td,
                    transition,
                    &from,
                    &serde_json::json!({}),
                );
            }
            let established_viewer = matches!(
                &row_establish,
                Some(RowEstablish::Established {
                    viewer: Some(_),
                    ..
                })
            );
            let operation = match kind {
                MutationKind::Create => "create",
                MutationKind::Update => "update",
                MutationKind::Delete => "delete",
            };
            let viewer = if established_viewer {
                None
            } else {
                viewer_requirement(schema, &entity, operation)
            };
            let (via, other) = if bind_row.is_some() && kind == MutationKind::Delete {
                inbound_restrict(schema, &entity)
            } else {
                (Vec::new(), Vec::new())
            };
            if matches!(row_establish, Some(RowEstablish::Unreachable { .. })) {
                precondition = None;
            }
            let _ = &mut row_establish;
            out.push(DataMutation {
                trait_name: td.name.clone(),
                coverage_key: format!(
                    "{}:{from}+{}->{}[data-mutation:{operation}]",
                    td.name, transition.event, transition.to
                ),
                from,
                event: transition.event.clone(),
                to: transition.to.clone(),
                payload: planner::synth_payload(td, &transition.event, &ctx, seed.as_deref()),
                entity,
                kind,
                row_delta: match kind {
                    MutationKind::Create => 1,
                    MutationKind::Update => 0,
                    MutationKind::Delete => -1,
                },
                success_event,
                viewer,
                bind_row,
                avoid_referenced_via: via,
                avoid_referenced_by_other_entities: other,
                row_establish,
                precondition,
            });
        }
    }
    out
}

pub fn plan_transient_probe(
    schema: &OrbitalSchema,
    query: &TransientQuery,
    options: &PlanOptions,
) -> Option<TransientOutcome> {
    let td = planner::inline_traits(schema)
        .into_iter()
        .find(|td| td.name == query.trait_name)?;
    let mut failures = Vec::new();
    td.state_machine
        .transitions
        .iter()
        .flat_map(|t| t.effects.iter().flatten())
        .for_each(|e| collect_failure_events(e, &mut failures));
    if !failures.contains(&query.event) || planner::transient_closure(td, &query.from).len() <= 1 {
        return Some(TransientOutcome::NotTransient);
    }
    let entering = td.state_machine.transitions.iter().find_map(|t| {
        t.effects
            .iter()
            .flatten()
            .find_map(|e| persist_failing(e, &query.event))
            .map(|found| (t, found))
    });
    let Some((enter, (entity, kind, data))) = entering else {
        return Some(TransientOutcome::Finding {
            finding: TransientFinding::NoEnteringPersist {
                event: query.event.clone(),
            },
        });
    };
    let Some(role) = denying_viewer(schema, &entity, kind, data.as_ref()) else {
        return Some(TransientOutcome::Finding {
            finding: TransientFinding::NoDenyingViewer {
                entity,
                operation: kind.to_string(),
            },
        });
    };
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    let seed_id = td
        .linked_entity
        .as_deref()
        .map(|e| planner::effective_seed_id(e, &owner_fields, options.viewer_id.as_deref()));
    let enter_from = enter
        .from
        .states()
        .first()
        .copied()
        .unwrap_or("")
        .to_string();
    let payload = planner::plan_trait_variants(td, &ctx, seed_id.as_deref())
        .into_iter()
        .find(|s| {
            s.from == enter_from
                && s.event == enter.event
                && s.payload_case == planner::PayloadCase::Success
                && s.guard_case != Some(planner::GuardCase::Fail)
        })
        .map_or_else(
            || planner::synth_payload(td, &enter.event, &ctx, seed_id.as_deref()),
            |s| s.payload,
        );
    let closure = planner::transient_closure(td, &enter.to);
    let setup = plan_wire::drive_setup(
        schema,
        td,
        &ctx,
        td.linked_entity.as_deref(),
        &enter_from,
        &enter.event,
        false,
    );
    Some(TransientOutcome::Probe {
        step: Box::new(TransientProbe {
            trait_name: td.name.clone(),
            coverage_key: format!(
                "{}:{enter_from}+{}->{}[transient-failure-probe:{}]",
                td.name, enter.event, enter.to, query.event
            ),
            from: enter_from,
            event: enter.event.clone(),
            to: enter.to.clone(),
            payload,
            accept_states: if closure.len() > 1 {
                closure
            } else {
                Vec::new()
            },
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
        let SExpression::Array(items) = effect else {
            continue;
        };
        let (
            Some(SExpression::String(op)),
            Some(SExpression::String(kind)),
            Some(SExpression::String(entity)),
        ) = (items.first(), items.get(1), items.get(2))
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
    let SExpression::Array(items) = node else {
        return false;
    };
    if let (Some(SExpression::String(op)), Some(SExpression::String(slot))) =
        (items.first(), items.get(1))
    {
        if op == "render-ui" && items.len() >= 3 && !matches!(items[2], SExpression::Null) {
            if slot != "main" && slot != "sidebar" && orbital_core_base::slots::is_portal_slot(slot)
            {
                return true;
            }
            if let SExpression::Object(config) = &items[2] {
                if let Some(SExpression::String(pattern)) = config.get("type") {
                    if orbital_core_base::slots::SELF_OVERLAY_PATTERN_TYPES
                        .contains(&pattern.as_str())
                    {
                        return true;
                    }
                }
            }
        }
    }
    items.iter().any(renders_overlay)
}

fn event_schema<'a>(
    td: &'a TraitDefinition,
    event: &str,
) -> &'a [orbital_core_base::schema::types::PayloadField] {
    td.state_machine
        .events
        .iter()
        .flatten()
        .find(|e| e.key == event)
        .and_then(|e| e.payload_schema.as_deref())
        .unwrap_or(&[])
}

fn viewer_requirement(
    schema: &OrbitalSchema,
    entity: &str,
    kind: &str,
) -> Option<ViewerRequirement> {
    let defs = orbital_core_base::policy::inline_entity_defs(schema);
    let policy = policy_of(defs.iter().find(|d| d.name == entity)?, kind)?;
    let operation = match kind {
        "create" => orbital_core_base::effect_types::PersistOperation::Create,
        "update" => orbital_core_base::effect_types::PersistOperation::Update,
        _ => orbital_core_base::effect_types::PersistOperation::Delete,
    };
    let role = crate::entity_access::role_satisfying_policy(schema, entity, operation)
        .map(|(field, value)| RoleWire { field, value });
    let owner = orbital_core_base::policy::owner_columns_from_policy(policy)
        .into_iter()
        .next()
        .map(|column| {
            if kind == "create" {
                OwnerRequirement {
                    source_entity: orbital_core_base::policy::identity_entity_name(schema)
                        .unwrap_or(entity)
                        .to_string(),
                    source_field: "id".into(),
                    use_default_id: Some(true),
                    payload_owner_field: Some(column),
                }
            } else {
                OwnerRequirement {
                    source_entity: entity.to_string(),
                    source_field: column,
                    use_default_id: None,
                    payload_owner_field: None,
                }
            }
        });
    Some(ViewerRequirement { role, owner })
}

/// `row` with every non-id value changed (a vocabulary field keeps its value), so an edit has something to change.
fn vary_row(row: &serde_json::Value, keep: &[String]) -> serde_json::Value {
    use serde_json::Value as J;
    match row {
        J::Object(map) => J::Object(
            map.iter()
                .map(|(k, v)| {
                    let fixed = k == "id" || keep.contains(k);
                    (k.clone(), if fixed { v.clone() } else { vary_row(v, keep) })
                })
                .collect(),
        ),
        J::String(s) => J::String(format!("{s}-edited")),
        J::Number(n) => n
            .as_f64()
            .map_or_else(|| row.clone(), |n| serde_json::json!(n + 1.0)),
        J::Bool(b) => J::Bool(!b),
        other => other.clone(),
    }
}

/// The relation fields that reference `entity` under a restrict rule (an undeclared `onDelete` is restrict): the
/// entity's own (self relations), then those of other entities.
fn inbound_restrict(schema: &OrbitalSchema, entity: &str) -> (Vec<String>, Vec<InboundRelation>) {
    use orbital_core_base::schema::types::{FieldType, OnDelete};
    let (mut own, mut other) = (Vec::new(), Vec::new());
    for def in orbital_core_base::policy::inline_entity_defs(schema) {
        for field in &def.fields {
            let (Some(name), Some(relation)) = (&field.name, &field.relation) else {
                continue;
            };
            if field.field_type != FieldType::Relation
                || relation.entity != entity
                || relation.on_delete.unwrap_or(OnDelete::Restrict) != OnDelete::Restrict
            {
                continue;
            }
            if def.name == entity {
                if !own.contains(name) {
                    own.push(name.clone());
                }
            } else {
                other.push(InboundRelation {
                    entity: def.name.clone(),
                    field: name.clone(),
                });
            }
        }
    }
    (own, other)
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
        let mut buckets: Vec<(
            String,
            Vec<(
                &'static str,
                String,
                &orbital_core_base::schema::types::TransitionDefinition,
            )>,
        )> = Vec::new();
        for transition in &persistor.state_machine.transitions {
            if transition.event == "INIT" {
                continue;
            }
            let Some((entity, kind, success)) =
                find_persist_effect(transition.effects.as_deref().unwrap_or(&[]))
            else {
                continue;
            };
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
                let Some((kind, success, transition)) =
                    persists.iter().find(|(k, _, _)| *k == wanted)
                else {
                    continue;
                };
                let create = persists
                    .iter()
                    .find(|(k, _, _)| *k == "create")
                    .map(|(_, _, t)| *t);
                if let Some(chain) = build_chain(
                    schema, &ctx, &all, persistor, &entity, kind, success, transition, create,
                ) {
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
    create: Option<&orbital_core_base::schema::types::TransitionDefinition>,
) -> Option<CrudChain> {
    use orbital_core_base::schema::types::ListenSource;
    let listener = persistor
        .listens
        .iter()
        .flatten()
        .find(|l| l.triggers.as_deref() == Some(persist.event.as_str()))?;
    let Some(ListenSource::Trait {
        trait_name: source_name,
        ..
    }) = &listener.source
    else {
        return None;
    };
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
    let inbound = source
        .listens
        .iter()
        .flatten()
        .find(|l| l.triggers.as_deref() == Some(open.event.as_str()));
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
    let persist_from = persist
        .from
        .states()
        .first()
        .copied()
        .unwrap_or("")
        .to_string();
    let row_entity = source.linked_entity.as_deref().unwrap_or(entity);
    let seed = source
        .linked_entity
        .as_deref()
        .map(|e| planner::effective_seed_id(e, ctx.owner_fields, ctx.viewer_id.as_deref()));
    let submit_payload = planner::synth_payload(source, &listener.event, ctx, seed.as_deref());
    let entity_row = planner::synth_entity_row(ctx, row_entity);
    let enum_fields: Vec<String> = planner::entity_def(schema, row_entity)
        .into_iter()
        .flat_map(|e| e.fields.iter())
        .filter(|f| f.values.as_ref().is_some_and(|v| !v.is_empty()))
        .filter_map(|f| f.name.clone())
        .collect();
    let (avoid_via, avoid_other) = if crud_kind == CrudKind::Delete {
        inbound_restrict(schema, entity)
    } else {
        (Vec::new(), Vec::new())
    };
    let fallback = create
        .filter(|_| {
            crud_kind == CrudKind::Delete && !(avoid_via.is_empty() && avoid_other.is_empty())
        })
        .map(|create| {
            let seed = persistor
                .linked_entity
                .as_deref()
                .map(|e| planner::effective_seed_id(e, ctx.owner_fields, ctx.viewer_id.as_deref()));
            EstablishFallback {
                event: create.event.clone(),
                payload: planner::synth_payload(persistor, &create.event, ctx, seed.as_deref()),
                trait_name: persistor.name.clone(),
                viewer: viewer_requirement(schema, entity, "create"),
            }
        });
    Some(CrudChain {
        kind: crud_kind,
        entity: entity.to_string(),
        persistor: persistor.name.clone(),
        source_trait: source.name.clone(),
        open_from: open
            .from
            .states()
            .first()
            .copied()
            .unwrap_or("")
            .to_string(),
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
        submit_payload,
        edit_entity_row: vary_row(&entity_row, &enum_fields),
        entity_row,
        enum_fields,
        payload_row_shape: if crud_kind == CrudKind::Delete {
            open_schema
                .iter()
                .map(|f| RowShapeField {
                    name: f.name.clone(),
                    whole_row: f.entity.as_deref() == Some(entity),
                })
                .collect()
        } else {
            Vec::new()
        },
        viewer: viewer_requirement(schema, entity, kind),
        precondition: persist.guard.as_ref().and_then(|_| {
            precondition::plan_precondition(
                ctx,
                persistor,
                persist,
                &persist_from,
                &serde_json::json!({}),
            )
        }),
        avoid_referenced_via: avoid_via,
        avoid_referenced_by_other_entities: avoid_other,
        delete_establish_fallback: fallback,
        coverage_key: format!(
            "{}:{}+{}->{}[{label}]",
            source.name,
            open.from.states().first().copied().unwrap_or(""),
            open.event,
            open.to
        ),
    })
}

/// The hops that place a fresh machine of `trait_name` in `state`; `None` for an unknown trait or an unreachable state.
pub fn plan_replay(
    schema: &OrbitalSchema,
    trait_name: &str,
    state: &str,
    options: &PlanOptions,
) -> Option<Vec<PreambleEvent>> {
    let td = planner::inline_traits(schema)
        .into_iter()
        .find(|td| td.name == trait_name)?;
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    plan_wire::preamble_to(td, &ctx, state, false)
}

pub fn plan_emit_sweep(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<EmitStep> {
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    let types = payload_solve::declared_types(schema);
    let mut out = Vec::new();
    for td in scoped(schema, options) {
        let Some(initial) = planner::initial_state(td) else {
            continue;
        };
        let seed_id = td
            .linked_entity
            .as_deref()
            .map(|e| planner::effective_seed_id(e, &owner_fields, options.viewer_id.as_deref()));
        let mut seen: Vec<String> = Vec::new();
        for event in declared_emits(schema, td) {
            if seen.contains(&event) {
                continue;
            }
            seen.push(event.clone());
            let mut payload = planner::synth_payload(td, &event, &ctx, seed_id.as_deref());
            if let Some(arm) = td
                .state_machine
                .transitions
                .iter()
                .find(|t| t.event == event)
            {
                for (field, text) in
                    payload_solve::json_parse_payload(arm, &types, &ctx, seed_id.as_deref())
                {
                    payload[field] = text;
                }
                let from = arm.from.states().first().copied().unwrap_or("").to_string();
                payload = payload_solve::solve_json_parse_fields(
                    arm,
                    true,
                    &payload,
                    &types,
                    &from,
                    &ctx,
                    seed_id.as_deref(),
                );
            }
            out.push(EmitStep {
                trait_name: td.name.clone(),
                from: initial.clone(),
                navigates: planner::dispatch_navigates(schema, td, &event),
                bind_row: emit_row_binding(schema, td, &event),
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
        return ContractEvents {
            available: false,
            steps: Vec::new(),
        };
    }
    let traits = planner::inline_traits(schema);
    let Some((anchor, initial)) = traits
        .iter()
        .find_map(|td| planner::initial_state(td).map(|s| (*td, s)))
    else {
        return ContractEvents {
            available: true,
            steps: Vec::new(),
        };
    };
    let mut patterns = Vec::new();
    for effect in traits
        .iter()
        .flat_map(|td| td.state_machine.transitions.iter())
        .flat_map(|t| t.effects.iter().flatten())
    {
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
        let Some(contract) =
            orbital_core_base::language::pattern_registry::get_event_contract(&pattern)
        else {
            continue;
        };
        for emit in &contract.emits {
            if emit.optional || seen.contains(&emit.event) || covered.contains(&emit.event) {
                continue;
            }
            seen.push(emit.event.clone());
            steps.push(ContractStep {
                trait_name: anchor.name.clone(),
                from: initial.clone(),
                coverage_key: format!(
                    "{}:{initial}+{}->{initial}[contract:{pattern}]",
                    anchor.name, emit.event
                ),
                event: emit.event.clone(),
                pattern: pattern.clone(),
            });
        }
    }
    ContractEvents {
        available: true,
        steps,
    }
}

/// Plan the click on a rendered affordance the host found: standing in `from`, firing `event`.
pub fn plan_affordance(
    schema: &OrbitalSchema,
    query: &AffordanceQuery,
    options: &PlanOptions,
) -> Option<AffordanceStep> {
    let td = planner::inline_traits(schema)
        .into_iter()
        .find(|td| td.name == query.trait_name)?;
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    let arms: Vec<_> = td
        .state_machine
        .transitions
        .iter()
        .filter(|t| t.from.matches(&query.from) && t.event == query.event)
        .collect();
    let to = arms
        .first()
        .map_or_else(|| query.from.clone(), |t| t.to.clone());
    let boots =
        query.event == "INIT" && planner::initial_state(td).as_deref() == Some(query.from.as_str());
    let setup = plan_wire::drive_setup(
        schema,
        td,
        &ctx,
        td.linked_entity.as_deref(),
        &query.from,
        &query.event,
        boots,
    );
    let precondition = arms.iter().find(|t| t.guard.is_some()).and_then(|t| {
        precondition::plan_precondition(&ctx, td, t, &query.from, &serde_json::json!({}))
    });
    Some(AffordanceStep {
        trait_name: td.name.clone(),
        coverage_key: format!(
            "{}:{}+{}->{to}[affordance]",
            td.name, query.from, query.event
        ),
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

/// The prefix of the test id (web `data-testid`, native identifier) of an element that dispatches an event.
pub const ACTION_TESTID_PREFIX: &str = "action-";

/// The test id of an element that dispatches `event` on a tap (`@almadar/core` `actionTestId`).
pub fn action_test_id(event: &str) -> String {
    format!("{ACTION_TESTID_PREFIX}{event}")
}

fn collect_event_props(node: &SExpression, out: &mut Vec<String>) {
    match node {
        SExpression::Array(items) => items.iter().for_each(|i| collect_event_props(i, out)),
        SExpression::Object(map) => {
            let props = match map.get("type") {
                Some(SExpression::String(pattern)) => {
                    orbital_core_base::language::pattern_props::PATTERN_EVENT_PROPS
                        .get(pattern.as_str())
                }
                _ => None,
            };
            for (key, value) in map {
                match value {
                    SExpression::String(event)
                        if !event.is_empty() && props.is_some_and(|p| p.contains(key.as_str())) =>
                    {
                        if !out.contains(event) {
                            out.push(event.clone())
                        }
                    }
                    _ => collect_event_props(value, out),
                }
            }
        }
        _ => {}
    }
}

/// One click per rendered affordance: every literal event an event-kind pattern prop carries in a state's
/// `render-ui`, planned from the state that mounts it (the transition's target) as [`plan_affordance`] plans it.
pub fn plan_click_paths(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<AffordanceStep> {
    if orbital_core_base::language_data::language_data().is_err() {
        return Vec::new();
    }
    let mut sites: Vec<(String, String, String)> = Vec::new();
    for td in scoped(schema, options) {
        if planner::initial_state(td).is_none() {
            continue;
        }
        for transition in &td.state_machine.transitions {
            for effect in transition.effects.iter().flatten() {
                let SExpression::Array(items) = effect else {
                    continue;
                };
                let (Some(SExpression::String(op)), Some(ui)) = (items.first(), items.get(2))
                else {
                    continue;
                };
                if op != "render-ui" {
                    continue;
                }
                let mut events = Vec::new();
                collect_event_props(ui, &mut events);
                for event in events {
                    let site = (td.name.clone(), transition.to.clone(), event);
                    if !sites.contains(&site) {
                        sites.push(site);
                    }
                }
            }
        }
    }
    sites
        .into_iter()
        .filter_map(|(trait_name, from, event)| {
            plan_affordance(
                schema,
                &AffordanceQuery {
                    trait_name,
                    from,
                    event,
                },
                options,
            )
        })
        .collect()
}

/// What a host saw clicking a planned affordance: whether the element was on the page, and whether any trait's
/// machine answered the click (a recorded transition, a self-loop included).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClickObservation {
    pub found: bool,
    pub answered: bool,
}

/// The verdict on a click (the twin of `assertClickPathSample`): an absent element drove nothing; a click no machine
/// answered is a dead affordance, unless the step's guard precondition is not shown to hold.
pub fn judge_click(step: &AffordanceStep, observed: &ClickObservation) -> plan_wire::VerdictWire {
    use super::frame::FrameStatus;
    use super::plan_wire::{VerdictReason, VerdictWire};
    let verdict = |status, reason| VerdictWire {
        status,
        gap: None,
        reason,
    };
    if !observed.found {
        return verdict(FrameStatus::Inconclusive, VerdictReason::NotDriven);
    }
    if observed.answered {
        return verdict(FrameStatus::Pass, VerdictReason::Reached);
    }
    if step
        .precondition
        .as_ref()
        .is_some_and(|p| p.gap().is_some())
    {
        return verdict(
            FrameStatus::Inconclusive,
            VerdictReason::ArmNotFired { fired: None },
        );
    }
    verdict(
        FrameStatus::Fail,
        VerdictReason::ArmNotFired { fired: None },
    )
}

/// Patterns whose job is to tell the viewer something went wrong (`alert`, std-browse's `error-state`,
/// `violation-alert`); a toast is counted through the `toast` slot.
pub const FAILURE_SURFACE_PATTERNS: &[&str] = &["alert", "error-state", "violation-alert"];

/// One rendered slot as the host read it: its direct children and the patterns mounted inside it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotSnapshot {
    pub slot: String,
    pub children: usize,
    pub patterns: Vec<String>,
    /// The `data-pattern` of the slot's top child: what the renderer mounted there last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

/// What a host saw around a fired step that may fail an effect: the events emitted after the fire, the errors the
/// host reported for it, and the slots before and after.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureObservation {
    pub emitted: Vec<String>,
    pub host_errors: Vec<String>,
    pub slots_before: Vec<SlotSnapshot>,
    pub slots_after: Vec<SlotSnapshot>,
}

const FAILABLE_EFFECTS: &[&str] = &["persist", "fetch", "call-service"];

fn failable_routes(node: &SExpression, out: &mut Vec<(String, Option<String>)>) {
    let SExpression::Array(items) = node else {
        return;
    };
    if let Some(SExpression::String(head)) = items.first() {
        if FAILABLE_EFFECTS.contains(&head.as_str()) {
            let failure = match items.last() {
                Some(SExpression::Object(options)) => match options.get("emit") {
                    Some(SExpression::Object(emit)) => match emit.get("failure") {
                        Some(SExpression::String(event)) => Some(event.clone()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            };
            out.push((head.clone(), failure));
            return;
        }
    }
    items.iter().for_each(|item| failable_routes(item, out));
}

fn surfaces(slots: &[SlotSnapshot]) -> (usize, usize) {
    let toast = slots
        .iter()
        .filter(|s| s.slot == "toast")
        .map(|s| s.children)
        .sum();
    let alerts = slots
        .iter()
        .flat_map(|s| &s.patterns)
        .filter(|p| FAILURE_SURFACE_PATTERNS.contains(&p.as_str()))
        .count();
    (toast, alerts)
}

/// The twin of `assertEffectFailureNotSurfaced` in `@almadar-io/verify`, over one fired step: a failable effect of the
/// arm that fired must route its failure (`emit.failure`), and a routed failure that fired must reach the viewer as a
/// toast or a failure pattern the step newly mounted. `None` when no failure happened.
pub fn judge_failure_surface(
    schema: &OrbitalSchema,
    trait_name: &str,
    from: &str,
    event: &str,
    fired_arm: Option<usize>,
    observed: &FailureObservation,
) -> Option<super::observers::Verdict> {
    let td = planner::inline_traits(schema)
        .into_iter()
        .find(|td| td.name == trait_name)?;
    let arms: Vec<_> = td
        .state_machine
        .transitions
        .iter()
        .filter(|t| t.from.matches(from) && t.event == event)
        .collect();
    let fired: Vec<_> = match fired_arm {
        Some(i) => arms.get(i).copied().into_iter().collect(),
        None => arms,
    };
    let mut routes = Vec::new();
    for effect in fired.iter().flat_map(|t| t.effects.iter().flatten()) {
        failable_routes(effect, &mut routes);
    }
    let label = format!("{trait_name} {from} --{event}-->");
    if !observed.host_errors.is_empty() {
        if let Some((kind, _)) = routes.iter().find(|(_, failure)| failure.is_none()) {
            return Some(super::observers::Verdict::from_offenders(
                "effect-failure-unrouted",
                vec![format!(
                    "{label}: a {kind} failed ({}) and declares no emit.failure route",
                    observed.host_errors.join("; ")
                )],
            ));
        }
    }
    let failed: Vec<&String> = routes
        .iter()
        .filter_map(|(_, f)| f.as_ref())
        .filter(|f| observed.emitted.contains(f))
        .collect();
    let failure = failed.first()?;
    let (toast_before, alerts_before) = surfaces(&observed.slots_before);
    let (toast_after, alerts_after) = surfaces(&observed.slots_after);
    let offenders = if toast_after > toast_before || alerts_after > alerts_before {
        Vec::new()
    } else {
        vec![format!("{label}: '{failure}' fired but no toast mounted and no failure pattern rendered; the failure never reached the user")]
    };
    Some(super::observers::Verdict::from_offenders(
        "effect-failure-not-surfaced",
        offenders,
    ))
}

/// One unconditional `render-ui` of an arm into a slot the web renderer mounts as `#slot-<name>`: the root
/// pattern it paints, or `None` when it clears the slot.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotRender {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    pub to: String,
    pub slot: String,
    pub pattern: Option<String>,
}

fn mounted_as_slot_element(slot: &str) -> bool {
    use orbital_core_base::slots::{slot_role, SlotRole};
    !slot.contains('.') && slot != "hud" && slot != "screen" && slot_role(slot) != SlotRole::System
}

/// Every arm's top-level `render-ui` into a `#slot-<name>` slot (a render nested in a conditional is not planned). A
/// trait another trait embeds (`@trait.<Name>`) paints inside its host's frame, never a slot of its own, so it plans
/// none.
pub fn plan_slot_renders(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<SlotRender> {
    let embedded: std::collections::HashSet<String> = planner::inline_traits(schema)
        .into_iter()
        .flat_map(orbital_core_base::schema::embed_tokens::trait_embed_children)
        .collect();
    let mut out = Vec::new();
    for td in scoped(schema, options)
        .into_iter()
        .filter(|td| !embedded.contains(&td.name))
    {
        for transition in &td.state_machine.transitions {
            for effect in transition.effects.iter().flatten() {
                let SExpression::Array(items) = effect else {
                    continue;
                };
                let (Some(SExpression::String(op)), Some(SExpression::String(slot)), Some(content)) =
                    (items.first(), items.get(1), items.get(2))
                else {
                    continue;
                };
                if op != "render-ui" || !mounted_as_slot_element(slot) {
                    continue;
                }
                let pattern = match content {
                    SExpression::Null => None,
                    SExpression::Object(map) => match map.get("type") {
                        Some(SExpression::String(t)) => Some(t.clone()),
                        _ => continue,
                    },
                    _ => continue,
                };
                for from in transition.from.states() {
                    out.push(SlotRender {
                        trait_name: td.name.clone(),
                        from: from.to_string(),
                        event: transition.event.clone(),
                        to: transition.to.clone(),
                        slot: slot.clone(),
                        pattern: pattern.clone(),
                    });
                }
            }
        }
    }
    out
}

/// The renders of the arm a step fired, `(trait, from, event, to)`.
pub fn renders_of<'a>(
    renders: &'a [SlotRender],
    trait_name: &str,
    from: &str,
    event: &str,
    to: &str,
) -> Vec<&'a SlotRender> {
    renders
        .iter()
        .filter(|r| r.trait_name == trait_name && r.from == from && r.event == event && r.to == to)
        .collect()
}

/// The twin of `assertPortalPerStep`: after a step that reached `to`, each slot its arm painted holds content and
/// each slot it cleared is empty. `None` when the arm paints no `#slot-` slot.
pub fn judge_portal(
    step: &[&SlotRender],
    slots_after: &[SlotSnapshot],
) -> Option<super::observers::Verdict> {
    if step.is_empty() {
        return None;
    }
    let mut offenders = Vec::new();
    for render in step {
        let slot = slots_after.iter().find(|s| s.slot == render.slot);
        let label = format!(
            "{} {} --{}--> {}",
            render.trait_name, render.from, render.event, render.to
        );
        match (&render.pattern, slot) {
            (None, Some(s)) if s.children > 0 => offenders.push(format!("{label}: expected slot '{}' cleared, found {} child(ren)", render.slot, s.children)),
            (None, _) => {}
            (Some(p), None) => offenders.push(format!("{label}: expected '{p}' in slot '{}', the slot is not mounted", render.slot)),
            (Some(p), Some(s)) if s.children == 0 => {
                offenders.push(format!("{label}: expected '{p}' in slot '{}', the slot is mounted but empty (a blank portal)", render.slot))
            }
            (Some(_), Some(_)) => {}
        }
    }
    Some(super::observers::Verdict::from_offenders(
        "portal", offenders,
    ))
}

/// The twin of `assertSlotShowsForeignTransitionRender`: a slot the step's arm painted shows another arm's pattern,
/// authored elsewhere, so the step's paint lost to a later writer. A successor arm of the same trait (an event its
/// own effect produces, or a state it moved on to) is not foreign.
pub fn judge_foreign_render(
    schema: &OrbitalSchema,
    step: &[&SlotRender],
    renders: &[SlotRender],
    state_after: &str,
    slots_after: &[SlotSnapshot],
) -> Option<super::observers::Verdict> {
    let painted: Vec<&&SlotRender> = step.iter().filter(|r| r.pattern.is_some()).collect();
    if painted.is_empty() {
        return None;
    }
    let mut offenders = Vec::new();
    for render in painted {
        let Some(shown) = slots_after
            .iter()
            .find(|s| s.slot == render.slot)
            .and_then(|s| s.root.as_ref())
        else {
            continue;
        };
        if Some(shown) == render.pattern.as_ref() {
            continue;
        }
        let Some(writer) = renders
            .iter()
            .find(|w| w.slot == render.slot && w.pattern.as_ref() == Some(shown))
        else {
            continue;
        };
        let produced = planner::inline_traits(schema)
            .into_iter()
            .find(|td| td.name == writer.trait_name)
            .is_some_and(|td| planner::effect_emitted_events(td).contains(&writer.event));
        if writer.trait_name == render.trait_name && (produced || state_after != render.to) {
            continue;
        }
        offenders.push(format!(
            "{} {}: expected '{}' in slot '{}', but it shows '{shown}', authored by {}.{}",
            render.trait_name,
            render.event,
            render.pattern.as_deref().unwrap_or_default(),
            render.slot,
            writer.trait_name,
            writer.event,
        ));
    }
    Some(super::observers::Verdict::from_offenders(
        "slot-shows-foreign-transition-render",
        offenders,
    ))
}

/// What a host saw after driving a planned [`DataMutation`]: the persist's own outcome when it reports one (`success`,
/// `denied`, `failed`), the events the dispatch's cascade emitted, and how the entity's row count moved.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationObservation {
    /// The state the machine stood in when the mutation's event was fired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persist_outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persist_error: Option<String>,
    #[serde(default)]
    pub emitted: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_delta: Option<i32>,
}

/// The verdict on a driven data mutation (the twin of `assertDataMutation` in `@almadar-io/verify`): a refused persist
/// fails; a persist that reports success passes; else a declared success event must be in the cascade; else the row
/// count must move by the planned delta.
pub fn judge_data_mutation(
    mutation: &DataMutation,
    observed: &MutationObservation,
) -> super::plan_wire::VerdictWire {
    use super::frame::FrameStatus;
    use super::plan_wire::{VerdictReason, VerdictWire};
    let verdict = |status, reason| VerdictWire {
        status,
        gap: None,
        reason,
    };
    if let Some(actual) = observed
        .state_before
        .as_ref()
        .filter(|s| **s != mutation.from)
    {
        return verdict(
            FrameStatus::Inconclusive,
            VerdictReason::PreambleMissed {
                actual: actual.clone(),
            },
        );
    }
    match observed.persist_outcome.as_deref() {
        Some(outcome @ ("denied" | "failed")) => {
            return verdict(
                FrameStatus::Fail,
                VerdictReason::MutationRefused {
                    outcome: outcome.to_string(),
                    error: observed.persist_error.clone(),
                },
            )
        }
        Some("success") => return verdict(FrameStatus::Pass, VerdictReason::MutationLanded),
        _ => {}
    }
    if let Some(expected) = &mutation.success_event {
        return if observed.emitted.contains(expected) {
            verdict(FrameStatus::Pass, VerdictReason::MutationLanded)
        } else {
            verdict(
                FrameStatus::Fail,
                VerdictReason::SuccessNotEmitted {
                    expected: expected.clone(),
                    emitted: observed.emitted.clone(),
                },
            )
        };
    }
    if observed.row_delta == Some(mutation.row_delta) {
        verdict(FrameStatus::Pass, VerdictReason::MutationLanded)
    } else {
        verdict(
            FrameStatus::Fail,
            VerdictReason::RowDeltaMismatch {
                expected: mutation.row_delta,
                actual: observed.row_delta,
            },
        )
    }
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
        assert_eq!(
            (loaded.trait_name.as_str(), loaded.from.as_str()),
            ("Runner", "idle")
        );
        assert_eq!(loaded.coverage_key, "Runner:idle+LOADED->idle[emit]");
        assert!(
            loaded.payload["count"].is_number(),
            "payload is built from the event's declared schema: {}",
            loaded.payload
        );
        assert!(!loaded.navigates);
    }

    fn confirm_schema(
        persist: serde_json::Value,
        mapping: Option<serde_json::Value>,
    ) -> OrbitalSchema {
        let mut listen = json!({ "event": "CONFIRM", "triggers": "DO_IT", "source": { "kind": "trait", "trait": "Confirm" } });
        if let Some(mapping) = mapping {
            listen["payloadMapping"] = mapping;
        }
        serde_json::from_value(json!({
            "name": "Crm",
            "orbitals": [{
                "name": "ContactOrbital",
                "entity": { "name": "Contact", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }, { "name": "name", "type": "string" }] },
                "traits": [
                    { "name": "Confirm", "linkedEntity": "Contact",
                      "emits": [
                          { "event": "CONFIRM", "scope": "external", "payloadSchema": [{ "name": "id", "type": "string" }] },
                          { "event": "CANCEL", "scope": "external" }],
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "confirming" }],
                        "transitions": [
                            { "from": "idle", "to": "confirming", "event": "REQUEST" },
                            { "from": "confirming", "to": "idle", "event": "CONFIRM" },
                            { "from": "confirming", "to": "idle", "event": "CANCEL" }] } },
                    { "name": "Persistor", "linkedEntity": "Contact",
                      "listens": [listen],
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "DO_IT", "effects": [persist] }] } }
                ]
            }]
        }))
        .unwrap()
    }

    fn sweep_of(schema: &OrbitalSchema, event: &str) -> EmitStep {
        plan_emit_sweep(schema, &PlanOptions::default())
            .into_iter()
            .find(|s| s.event == event)
            .expect("the emit is swept")
    }

    #[test]
    fn an_emit_a_listener_persists_by_id_binds_the_row_it_names() {
        let schema = confirm_schema(json!(["persist", "delete", "Contact", "@payload.id"]), None);
        assert_eq!(
            sweep_of(&schema, "CONFIRM").bind_row,
            Some(EmitRowBinding {
                entity: "Contact".into(),
                payload_field: "id".into(),
                whole_row: false,
                avoid_referenced_via: Vec::new(),
                avoid_referenced_by_other_entities: Vec::new()
            })
        );
    }

    #[test]
    fn control_an_emit_no_listener_writes_by_id_binds_no_row() {
        let delete = confirm_schema(json!(["persist", "delete", "Contact", "@payload.id"]), None);
        assert_eq!(
            sweep_of(&delete, "CANCEL").bind_row,
            None,
            "no listener hears CANCEL"
        );
        let create = confirm_schema(
            json!(["persist", "create", "Contact", { "name": "@payload.id" }]),
            None,
        );
        assert_eq!(
            sweep_of(&create, "CONFIRM").bind_row,
            None,
            "a create names no existing row"
        );
        let computed = confirm_schema(
            json!(["persist", "delete", "Contact", "@payload.id"]),
            Some(json!({ "id": ["str/concat", "@payload.id", "-x"] })),
        );
        assert_eq!(
            sweep_of(&computed, "CONFIRM").bind_row,
            None,
            "a computed mapping names no emitted field"
        );
    }

    #[test]
    fn a_delete_binding_avoids_rows_a_restrict_relation_references() {
        let mut program = serde_json::to_value(confirm_schema(
            json!(["persist", "delete", "Contact", "@payload.id"]),
            None,
        ))
        .unwrap();
        program["orbitals"][0]["auxiliaryEntities"] = json!([{ "name": "Deal", "persistence": "runtime", "fields": [
            { "name": "id", "type": "string" },
            { "name": "contactId", "type": "relation", "relation": { "entity": "Contact", "cardinality": "many-to-one" } }] }]);
        let schema: OrbitalSchema = serde_json::from_value(program).unwrap();
        let binding = sweep_of(&schema, "CONFIRM")
            .bind_row
            .expect("the delete binds a row");
        assert_eq!(
            binding.avoid_referenced_by_other_entities,
            vec![InboundRelation {
                entity: "Deal".into(),
                field: "contactId".into()
            }]
        );
    }

    #[test]
    fn a_mapped_listen_binds_the_emitter_field_its_mapping_reads() {
        let mapped = confirm_schema(
            json!(["persist", "delete", "Contact", "@payload.id"]),
            Some(json!({ "id": "@payload.contactId" })),
        );
        assert_eq!(
            sweep_of(&mapped, "CONFIRM").bind_row,
            Some(EmitRowBinding {
                entity: "Contact".into(),
                payload_field: "contactId".into(),
                whole_row: false,
                avoid_referenced_via: Vec::new(),
                avoid_referenced_by_other_entities: Vec::new()
            })
        );
    }

    #[test]
    fn control_a_trait_that_declares_no_emit_sweeps_nothing_and_an_emit_that_navigates_is_flagged()
    {
        let none = plan_emit_sweep(
            &schema(json!([])),
            &PlanOptions {
                traits: Some(vec!["Picker".into()]),
                ..Default::default()
            },
        );
        assert!(none.is_empty());
        let navigating = schema(json!([
            { "from": "idle", "to": "idle", "event": "SAVE", "effects": [["persist", "create", "Job", {}, { "emit": { "success": "LEAVE" } }]] }
        ]));
        let steps = plan_emit_sweep(&navigating, &PlanOptions::default());
        assert!(
            steps
                .iter()
                .find(|s| s.event == "LEAVE")
                .is_some_and(|s| s.navigates),
            "{steps:?}"
        );
    }

    #[test]
    fn contract_events_plan_the_non_optional_emits_of_the_patterns_a_program_renders() {
        let plan = plan_contract_events(&schema(json!([])), &PlanOptions::default());
        assert!(
            plan.available,
            "the installed language data supplies the event contracts"
        );
        let grid: Vec<&ContractStep> = plan
            .steps
            .iter()
            .filter(|s| s.pattern == "data-grid")
            .collect();
        assert!(!grid.is_empty(), "data-grid is rendered: {:?}", plan.steps);
        for step in &grid {
            assert_eq!(
                (step.trait_name.as_str(), step.from.as_str()),
                ("Runner", "idle")
            );
            assert_eq!(
                step.coverage_key,
                format!("Runner:idle+{}->idle[contract:data-grid]", step.event)
            );
        }
        assert!(
            plan.steps.iter().all(|s| s.pattern != "form-section"),
            "control: a pattern the program never renders plans nothing"
        );
    }

    #[test]
    fn contract_events_skip_the_events_the_caller_already_covered() {
        let all = plan_contract_events(&schema(json!([])), &PlanOptions::default());
        let first = all.steps[0].event.clone();
        let covered = PlanOptions {
            covered_events: Some(vec![first.clone()]),
            ..Default::default()
        };
        let rest = plan_contract_events(&schema(json!([])), &covered);
        assert!(rest.steps.iter().all(|s| s.event != first));
        assert_eq!(rest.steps.len() + 1, all.steps.len());
    }

    #[test]
    fn a_routed_failure_must_reach_the_viewer() {
        let schema = schema(json!([
            { "from": "ready", "to": "ready", "event": "SAVE", "effects": [["persist", "create", "Job", {}, { "emit": { "failure": "SAVE_FAILED" } }]] },
            { "from": "ready", "to": "ready", "event": "DROP", "effects": [["persist", "delete", "Job", "@entity.id"]] }
        ]));
        let slot = |slot: &str, children, patterns: &[&str]| SlotSnapshot {
            slot: slot.into(),
            children,
            patterns: patterns.iter().map(|p| p.to_string()).collect(),
            root: None,
        };
        let observe = |emitted: &[&str], after: Vec<SlotSnapshot>| FailureObservation {
            emitted: emitted.iter().map(|e| e.to_string()).collect(),
            host_errors: Vec::new(),
            slots_before: vec![slot("main", 1, &["typography"])],
            slots_after: after,
        };
        let judged = |o: &FailureObservation| {
            judge_failure_surface(&schema, "Runner", "ready", "SAVE", None, o)
        };
        let alert = judged(&observe(
            &["SAVE_FAILED"],
            vec![slot("main", 1, &["alert"])],
        ))
        .expect("a failure");
        assert!(
            alert.passed,
            "an alert the failure mounted reaches the viewer"
        );
        let toast = judged(&observe(
            &["SAVE_FAILED"],
            vec![
                slot("main", 1, &["typography"]),
                slot("toast", 1, &["toast"]),
            ],
        ))
        .expect("a failure");
        assert!(toast.passed);
        let silent = judged(&observe(
            &["SAVE_FAILED"],
            vec![slot("main", 1, &["typography"])],
        ))
        .expect("a failure");
        assert!(
            !silent.passed && silent.name == "effect-failure-not-surfaced",
            "{}",
            silent.detail
        );
        assert!(
            judged(&observe(&[], vec![slot("main", 1, &["typography"])])).is_none(),
            "control: no failure fired"
        );
        let unrouted = judge_failure_surface(
            &schema,
            "Runner",
            "ready",
            "DROP",
            None,
            &FailureObservation {
                host_errors: vec!["denied".into()],
                ..Default::default()
            },
        )
        .expect("an unrouted failure");
        assert!(!unrouted.passed && unrouted.name == "effect-failure-unrouted");
        assert!(judge_failure_surface(
            &schema,
            "Runner",
            "ready",
            "DROP",
            None,
            &FailureObservation::default()
        )
        .is_none());
    }

    #[test]
    fn a_painted_slot_must_hold_content_and_show_its_own_pattern() {
        let program = schema(json!([
            { "from": "ready", "to": "ready", "event": "SHOW", "effects": [
                ["render-ui", "modal", { "type": "form-section" }], ["render-ui", "main", null]
            ] },
            { "from": "ready", "to": "ready", "event": "PEEK", "effects": [["render-ui", "modal", { "type": "stack" }]] }
        ]));
        let renders = plan_slot_renders(&program, &PlanOptions::default());
        let show = renders_of(&renders, "Runner", "ready", "SHOW", "ready");
        assert_eq!(show.len(), 2, "{renders:?}");
        let slot = |slot: &str, children, root: Option<&str>| SlotSnapshot {
            slot: slot.into(),
            children,
            patterns: vec![],
            root: root.map(str::to_string),
        };
        assert!(judge_portal(
            &show,
            &[
                slot("modal", 1, Some("form-section")),
                slot("main", 0, None)
            ]
        )
        .is_some_and(|v| v.passed));
        let blank = judge_portal(&show, &[slot("modal", 0, None)]).expect("a verdict");
        assert!(
            !blank.passed && blank.detail.contains("blank portal"),
            "{}",
            blank.detail
        );
        let uncleared = judge_portal(&show, &[slot("modal", 1, None), slot("main", 2, None)])
            .expect("a verdict");
        assert!(!uncleared.passed && uncleared.detail.contains("cleared"));
        let foreign = judge_foreign_render(
            &program,
            &show,
            &renders,
            "ready",
            &[slot("modal", 1, Some("stack"))],
        )
        .expect("a verdict");
        assert!(
            !foreign.passed && foreign.detail.contains("Runner.PEEK"),
            "{}",
            foreign.detail
        );
        assert!(judge_foreign_render(
            &program,
            &show,
            &renders,
            "ready",
            &[slot("modal", 1, Some("form-section"))]
        )
        .is_some_and(|v| v.passed));
        assert!(
            judge_portal(&renders_of(&renders, "Runner", "idle", "INIT", "idle"), &[]).is_none(),
            "control: an arm that paints nothing"
        );
        let hosted = schema(json!([
            { "from": "ready", "to": "ready", "event": "HOST", "effects": [["render-ui", "main", { "type": "stack", "children": ["@trait.Picker"] }]] }
        ]));
        let picker = |schema: &OrbitalSchema| {
            plan_slot_renders(schema, &PlanOptions::default())
                .into_iter()
                .filter(|r| r.trait_name == "Picker")
                .count()
        };
        let mut painting = hosted.clone();
        if let Some(orbital_core_base::schema::types::TraitReference::Inline(def)) =
            painting.orbitals[0].traits.get_mut(1)
        {
            def.state_machine.transitions[0].effects = Some(vec![serde_json::from_value(
                json!(["render-ui", "main", { "type": "data-grid" }]),
            )
            .unwrap()]);
        }
        assert_eq!(picker(&painting), 0, "an embedded trait owns no slot");
        let mut alone = schema(json!([]));
        if let Some(orbital_core_base::schema::types::TraitReference::Inline(def)) =
            alone.orbitals[0].traits.get_mut(1)
        {
            def.state_machine.transitions[0].effects = Some(vec![serde_json::from_value(
                json!(["render-ui", "main", { "type": "data-grid" }]),
            )
            .unwrap()]);
        }
        assert_eq!(
            picker(&alone),
            1,
            "control: the same render, not embedded, plans its slot"
        );
    }

    #[test]
    fn click_paths_come_from_event_props_in_the_state_that_mounts_them() {
        let labelled = schema(json!([
            { "from": "ready", "to": "ready", "event": "SHOW", "effects": [
                ["render-ui", "main", { "type": "button", "label": "LEAVE", "action": "LEAVE" }]
            ] }
        ]));
        let clicks = plan_click_paths(&labelled, &PlanOptions::default());
        let sites: Vec<(&str, &str, &str, bool)> = clicks
            .iter()
            .map(|c| {
                (
                    c.trait_name.as_str(),
                    c.from.as_str(),
                    c.event.as_str(),
                    c.declared,
                )
            })
            .collect();
        assert_eq!(
            sites,
            vec![
                ("Runner", "loading", "GO", false),
                ("Runner", "ready", "LEAVE", true)
            ]
        );
        assert_eq!(action_test_id("GO"), "action-GO");
    }

    #[test]
    fn a_click_is_judged_by_whether_a_machine_answered_it() {
        let step = plan_click_paths(&schema(json!([])), &PlanOptions::default()).remove(0);
        let judged =
            |found, answered| judge_click(&step, &ClickObservation { found, answered }).status;
        use crate::verify::frame::FrameStatus;
        assert_eq!(judged(true, true), FrameStatus::Pass);
        assert_eq!(
            judged(true, false),
            FrameStatus::Fail,
            "a click nothing answered is a dead affordance"
        );
        assert_eq!(
            judged(false, false),
            FrameStatus::Inconclusive,
            "control: an absent element drove nothing"
        );
    }

    fn affordance(
        schema: &OrbitalSchema,
        trait_name: &str,
        from: &str,
        event: &str,
    ) -> Option<AffordanceStep> {
        plan_affordance(
            schema,
            &AffordanceQuery {
                trait_name: trait_name.into(),
                from: from.into(),
                event: event.into(),
            },
            &PlanOptions::default(),
        )
    }

    #[test]
    fn an_affordance_is_planned_from_the_state_that_renders_it_with_its_target_and_navigation() {
        let program = schema(json!([]));
        let go = affordance(&program, "Runner", "idle", "GO").unwrap();
        assert_eq!(
            (go.to.as_str(), go.declared, go.navigates),
            ("loading", true, false)
        );
        assert_eq!(go.coverage_key, "Runner:idle+GO->loading[affordance]");
        assert_eq!(
            go.preamble.as_ref().map(|p| p.iter().map(|e| e.event.as_str()).collect::<Vec<_>>()),
            Some(Vec::<&str>::new()),
            "a host mounts the trait, which fires the boot INIT itself: the initial state needs no hop"
        );
        let leave = affordance(&program, "Runner", "ready", "LEAVE").unwrap();
        assert!(leave.navigates);
        assert_eq!(
            leave
                .preamble
                .as_ref()
                .map(|p| p.iter().map(|e| e.event.as_str()).collect::<Vec<_>>()),
            Some(vec!["GO"]),
            "LOADED is fired by the fetch, never dispatched"
        );
    }

    #[test]
    fn control_a_button_for_an_event_the_state_does_not_handle_is_a_dead_affordance_and_an_unknown_trait_is_none(
    ) {
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
        assert!(
            matches!(press.precondition, Some(Precondition::Established { ref trait_name, .. }) if trait_name == "Picker"),
            "{press:?}"
        );
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
            &TransientQuery {
                trait_name: "Flow".into(),
                from: "saving".into(),
                event: event.into(),
                to: "failed".into(),
            },
            &PlanOptions::default(),
        )
    }

    fn persisting() -> serde_json::Value {
        json!(["persist", "create", "Note", { "title": "x" }, { "emit": { "success": "DONE", "failure": "FAILED" } }])
    }

    #[test]
    fn a_transient_failure_arm_is_forced_through_the_entering_persist_as_a_denied_viewer() {
        let program = transient_schema(json!(["=", "@user.role", "admin"]), persisting());
        let Some(TransientOutcome::Probe { step }) = transient(&program, "FAILED") else {
            panic!("expected a probe")
        };
        assert_eq!(
            (step.from.as_str(), step.event.as_str(), step.to.as_str()),
            ("idle", "START", "saving")
        );
        assert_eq!(
            step.role,
            RoleWire {
                field: "role".into(),
                value: "viewer".into()
            }
        );
        assert_eq!(
            step.accept_states,
            vec![
                "saving".to_string(),
                "saved".to_string(),
                "failed".to_string()
            ]
        );
        assert_eq!(step.verifies_portal_for.event, "FAILED");
        assert_eq!(
            step.coverage_key,
            "Flow:idle+START->saving[transient-failure-probe:FAILED]"
        );
    }

    #[test]
    fn control_an_event_that_is_no_failure_route_or_with_no_denying_viewer_or_persist_owes_no_probe(
    ) {
        let program = transient_schema(json!(["=", "@user.role", "admin"]), persisting());
        assert_eq!(
            transient(&program, "DONE"),
            Some(TransientOutcome::NotTransient)
        );
        let open = transient_schema(json!(true), persisting());
        assert_eq!(
            transient(&open, "FAILED"),
            Some(TransientOutcome::Finding {
                finding: TransientFinding::NoDenyingViewer {
                    entity: "Note".into(),
                    operation: "create".into()
                }
            })
        );
        let fetching = transient_schema(
            json!(["=", "@user.role", "admin"]),
            json!(["fetch", "Note", { "emit": { "success": "DONE", "failure": "FETCH_FAILED" } }]),
        );
        assert_eq!(
            transient(&fetching, "FETCH_FAILED"),
            Some(TransientOutcome::Finding {
                finding: TransientFinding::NoEnteringPersist {
                    event: "FETCH_FAILED".into()
                }
            })
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
        let chains = plan_crud_chains(
            &crud_schema(json!(["render-ui", "modal", { "type": "form-section" }])),
            &PlanOptions::default(),
        );
        let create = chains
            .iter()
            .find(|c| c.kind == CrudKind::Create)
            .expect("a create chain");
        assert_eq!(
            (create.persistor.as_str(), create.source_trait.as_str()),
            ("Notes", "NoteModal")
        );
        assert_eq!(
            (create.open_event.as_str(), create.submit_event.as_str()),
            ("OPEN", "NOTE_SAVE")
        );
        assert_eq!(
            (
                create.success_event.as_str(),
                create.row_delta,
                create.opens_overlay
            ),
            ("NOTE_CREATED", 1, true)
        );
        assert_eq!(
            create.coverage_key,
            "NoteModal:closed+OPEN->open[crud-create]"
        );
        let delete = chains
            .iter()
            .find(|c| c.kind == CrudKind::Delete)
            .expect("a delete chain");
        assert_eq!(
            (
                delete.row_delta,
                delete.open_affordance_event.as_str(),
                delete.opens_overlay
            ),
            (-1, "REQUEST_DELETE", true)
        );
        assert!(delete.requires_row_context);
        assert_eq!(
            delete.payload_row_shape,
            vec![
                RowShapeField {
                    name: "id".into(),
                    whole_row: false
                },
                RowShapeField {
                    name: "row".into(),
                    whole_row: true
                }
            ]
        );
    }

    #[test]
    fn control_an_open_arm_that_renders_inline_is_not_an_overlay_and_a_persist_without_a_listener_has_no_chain(
    ) {
        let inline = plan_crud_chains(
            &crud_schema(json!(["render-ui", "main", { "type": "table-view" }])),
            &PlanOptions::default(),
        );
        assert!(inline
            .iter()
            .find(|c| c.kind == CrudKind::Create)
            .is_some_and(|c| !c.opens_overlay));
        assert!(
            inline.iter().all(|c| c.kind != CrudKind::Edit),
            "no update persist, no edit chain"
        );
    }

    fn crud_with_relations(on_delete: Option<&str>) -> OrbitalSchema {
        let mut program = serde_json::to_value(crud_schema(
            json!(["render-ui", "modal", { "type": "form-section" }]),
        ))
        .unwrap();
        let mut relation = json!({ "entity": "Note", "cardinality": "many-to-one" });
        if let Some(rule) = on_delete {
            relation["onDelete"] = json!(rule);
        }
        let orbital = &mut program["orbitals"][0];
        orbital["entity"]["fields"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "name": "parentId", "type": "relation", "relation": relation.clone() }));
        orbital["auxiliaryEntities"] = json!([{ "name": "Comment", "persistence": "runtime", "fields": [
            { "name": "id", "type": "string" }, { "name": "noteId", "type": "relation", "relation": relation }] }]);
        serde_json::from_value(program).unwrap()
    }

    #[test]
    fn a_delete_chain_names_the_restrict_relations_that_can_block_it_and_the_create_that_refills() {
        let chains = plan_crud_chains(&crud_with_relations(None), &PlanOptions::default());
        let delete = chains.iter().find(|c| c.kind == CrudKind::Delete).unwrap();
        assert_eq!(
            delete.avoid_referenced_via,
            vec!["parentId".to_string()],
            "an undeclared onDelete is restrict"
        );
        assert_eq!(
            delete.avoid_referenced_by_other_entities,
            vec![InboundRelation {
                entity: "Comment".into(),
                field: "noteId".into()
            }]
        );
        let fallback = delete
            .delete_establish_fallback
            .as_ref()
            .expect("the entity declares its own create");
        assert_eq!(
            (fallback.event.as_str(), fallback.trait_name.as_str()),
            ("DO_CREATE", "Notes")
        );
        let create = chains.iter().find(|c| c.kind == CrudKind::Create).unwrap();
        assert!(
            create.avoid_referenced_via.is_empty() && create.delete_establish_fallback.is_none(),
            "only a delete chain avoids"
        );
    }

    #[test]
    fn control_a_cascade_relation_blocks_nothing_and_an_entity_without_relations_needs_no_fallback()
    {
        let cascade = plan_crud_chains(
            &crud_with_relations(Some("cascade")),
            &PlanOptions::default(),
        );
        let delete = cascade.iter().find(|c| c.kind == CrudKind::Delete).unwrap();
        assert!(
            delete.avoid_referenced_via.is_empty()
                && delete.avoid_referenced_by_other_entities.is_empty()
        );
        assert!(delete.delete_establish_fallback.is_none());
        let restrict = plan_crud_chains(
            &crud_with_relations(Some("restrict")),
            &PlanOptions::default(),
        );
        assert_eq!(
            restrict
                .iter()
                .find(|c| c.kind == CrudKind::Delete)
                .unwrap()
                .avoid_referenced_via
                .len(),
            1
        );
        let plain = plan_crud_chains(
            &crud_schema(json!(["render-ui", "modal", { "type": "form-section" }])),
            &PlanOptions::default(),
        );
        assert!(plain
            .iter()
            .find(|c| c.kind == CrudKind::Delete)
            .unwrap()
            .delete_establish_fallback
            .is_none());
    }

    fn tick_schema() -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{ "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Clock", "linkedEntity": "Job",
                    "ticks": [
                        { "name": "beat", "interval": 1000, "effects": [] },
                        { "name": "frame", "interval": "frame", "effects": [] },
                        { "name": "never", "interval": 0, "effects": [] }
                    ],
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
                        "transitions": [{ "from": "idle", "to": "busy", "event": "GO" }] } }] }]
        }))
        .unwrap()
    }

    #[test]
    fn a_numeric_tick_is_waited_out_and_may_land_in_any_declared_state() {
        let ticks = plan_tick_steps(&tick_schema(), &PlanOptions::default());
        assert_eq!(
            ticks,
            vec![TickStep {
                trait_name: "Clock".into(),
                from: "idle".into(),
                event: "beat".into(),
                wait_ms: 1000,
                accept_states: vec!["idle".to_string(), "busy".to_string()],
                coverage_key: "Clock:tick(beat)".into(),
            }],
            "a frame tick and a non-positive interval are not waited out"
        );
    }

    #[test]
    fn a_duration_string_tick_is_waited_out_and_a_cron_frame_or_unknown_unit_is_not() {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{ "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Clock", "linkedEntity": "Job",
                    "ticks": [
                        { "name": "five", "interval": "5s", "effects": [] },
                        { "name": "minute", "interval": "1m", "effects": [] },
                        { "name": "hour", "interval": "1h", "effects": [] },
                        { "name": "milli", "interval": "250ms", "effects": [] },
                        { "name": "bare", "interval": "300", "effects": [] },
                        { "name": "cron", "interval": "0 9 * * *", "effects": [] },
                        { "name": "frame", "interval": "frame", "effects": [] },
                        { "name": "day", "interval": "1d", "effects": [] },
                        { "name": "zero", "interval": "0s", "effects": [] }
                    ],
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "GO" }] } }] }]
        }))
        .unwrap();
        let waits: Vec<(String, u64)> = plan_tick_steps(&schema, &PlanOptions::default())
            .into_iter()
            .map(|t| (t.event, t.wait_ms))
            .collect();
        assert_eq!(
            waits,
            vec![
                ("five".into(), 5000),
                ("minute".into(), 60_000),
                ("hour".into(), 3_600_000),
                ("milli".into(), 250),
                ("bare".into(), 300)
            ]
        );
    }

    fn mutation_schema() -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{ "name": "Main",
                "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
                "traits": [{ "name": "Notes", "linkedEntity": "Note",
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "editing" }],
                        "events": [
                            { "key": "SAVE", "name": "SAVE", "payloadSchema": [{ "name": "data", "type": "object", "entity": "Note" }] },
                            { "key": "REMOVE", "name": "REMOVE", "payloadSchema": [{ "name": "id", "type": "string" }] },
                            { "key": "TOUCH", "name": "TOUCH" },
                            { "key": "PICK", "name": "PICK", "payloadSchema": [{ "name": "id", "type": "string" }] }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "ADD", "effects": [
                                ["set", "@entity.id", "@payload.id"],
                                ["persist", "create", "Note", { "title": "t" }, { "emit": { "success": "ADDED" } }]] },
                            { "from": "idle", "to": "editing", "event": "PICK", "effects": [["set", "@entity.id", "@payload.id"]] },
                            { "from": "editing", "to": "idle", "event": "SAVE", "effects": [["persist", "update", "Note", "@payload.data", { "emit": { "success": "SAVED" } }]] },
                            { "from": "editing", "to": "idle", "event": "REMOVE", "effects": [["persist", "delete", "Note", "@payload.id"]] },
                            { "from": "idle", "to": "idle", "event": "TOUCH", "effects": [["persist", "update", "Note", { "title": "x" }]] }
                        ] } }] }]
        }))
        .unwrap()
    }

    fn mutation<'a>(all: &'a [DataMutation], event: &str) -> &'a DataMutation {
        all.iter()
            .find(|m| m.event == event)
            .unwrap_or_else(|| panic!("{event} is planned: {all:?}"))
    }

    #[test]
    fn each_persist_transition_is_a_data_mutation_with_its_row_delta_and_row_binding() {
        let all = plan_data_mutations(&mutation_schema(), &PlanOptions::default());
        let add = mutation(&all, "ADD");
        assert_eq!(
            (add.kind, add.row_delta, add.success_event.as_deref()),
            (MutationKind::Create, 1, Some("ADDED"))
        );
        assert_eq!(
            add.coverage_key,
            "Notes:idle+ADD->idle[data-mutation:create]"
        );
        assert!(
            add.bind_row.is_none() && add.row_establish.is_none(),
            "a create has no target row"
        );
        let save = mutation(&all, "SAVE");
        assert_eq!((save.kind, save.row_delta), (MutationKind::Update, 0));
        assert_eq!(
            save.bind_row,
            Some(PersistBinding {
                payload_field: "data".into(),
                whole_row: true
            })
        );
        let remove = mutation(&all, "REMOVE");
        assert_eq!((remove.kind, remove.row_delta), (MutationKind::Delete, -1));
        assert_eq!(
            remove.bind_row,
            Some(PersistBinding {
                payload_field: "id".into(),
                whole_row: false
            })
        );
        assert!(
            all.iter().all(|m| m.event != "INIT" && m.event != "PICK"),
            "only persist transitions are mutations"
        );
    }

    #[test]
    fn a_step_that_writes_a_row_nothing_binds_gets_the_establishing_transition_or_a_typed_unreachable(
    ) {
        let all = plan_data_mutations(&mutation_schema(), &PlanOptions::default());
        let touch = mutation(&all, "TOUCH");
        assert!(touch.bind_row.is_none());
        match &touch.row_establish {
            Some(RowEstablish::Established {
                event,
                bind_row_from,
                before_replay,
                ..
            }) => {
                assert_eq!(
                    event, "ADD",
                    "the create self-loop at the initial state that binds @entity.id"
                );
                assert!(bind_row_from.is_none() && !before_replay);
            }
            other => panic!("expected the create self-loop, got {other:?}"),
        }
        let mut program = serde_json::to_value(mutation_schema()).unwrap();
        let transitions = program["orbitals"][0]["traits"][0]["stateMachine"]["transitions"]
            .as_array_mut()
            .unwrap();
        transitions.retain(|t| t["event"] != "ADD" && t["event"] != "PICK");
        let bare: OrbitalSchema = serde_json::from_value(program).unwrap();
        let touch = plan_data_mutations(&bare, &PlanOptions::default())
            .into_iter()
            .find(|m| m.event == "TOUCH")
            .unwrap();
        assert_eq!(
            touch.row_establish,
            Some(RowEstablish::Unreachable {
                trait_name: "Notes".into(),
                entity: "Note".into(),
                at_state: "idle".into()
            })
        );
    }

    #[test]
    fn a_dispatched_row_selector_is_bound_from_a_payload_field_and_a_constant_false_guard_is_not_planned(
    ) {
        let mut program = serde_json::to_value(mutation_schema()).unwrap();
        let transitions = program["orbitals"][0]["traits"][0]["stateMachine"]["transitions"]
            .as_array_mut()
            .unwrap();
        transitions.retain(|t| t["event"] != "ADD");
        transitions.push(
            json!({ "from": "idle", "to": "idle", "event": "NEVER", "guard": ["=", 1, 2],
            "effects": [["persist", "create", "Note", { "title": "t" }]] }),
        );
        let schema: OrbitalSchema = serde_json::from_value(program).unwrap();
        let all = plan_data_mutations(&schema, &PlanOptions::default());
        match &mutation(&all, "TOUCH").row_establish {
            Some(RowEstablish::Established {
                event,
                bind_row_from,
                ..
            }) => {
                assert_eq!(event, "PICK");
                assert_eq!(
                    bind_row_from.as_ref().map(|b| b.payload_field.as_str()),
                    Some("id")
                );
            }
            other => panic!("expected the selecting transition, got {other:?}"),
        }
        assert!(all.iter().all(|m| m.event != "NEVER"));
    }

    #[test]
    fn the_emit_sweep_covers_effect_emitted_events_and_declared_emits_but_no_http_reply_route() {
        let program: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "egress": ["https://api.example.com"],
            "orbitals": [{ "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Hub", "linkedEntity": "Job",
                    "emits": [{ "event": "PING" }, { "event": "@external" }],
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "waiting" }],
                        "transitions": [
                            { "from": "idle", "to": "waiting", "event": "GO", "effects": [
                                ["fetch", "Job", { "emit": { "success": "LOADED", "failure": "LOAD_FAILED", "onMessage": "LIVE" } }],
                                ["http", "GET", "https://api.example.com", "/x", {}, { "emit": { "success": "HTTP_DONE" } }]] },
                            { "from": "waiting", "to": "idle", "event": "HTTP_DONE" },
                            { "from": "waiting", "to": "idle", "event": "LOADED" }
                        ] } }] }]
        }))
        .unwrap();
        let events: Vec<String> = plan_emit_sweep(&program, &PlanOptions::default())
            .into_iter()
            .map(|s| s.event)
            .collect();
        assert_eq!(
            events,
            vec!["LOADED", "LOAD_FAILED", "PING"],
            "discovery order, http routes and external names left out"
        );
    }

    fn persisting_data(data: serde_json::Value) -> serde_json::Value {
        json!(["persist", "create", "Note", data, { "emit": { "success": "DONE", "failure": "FAILED" } }])
    }

    #[test]
    fn a_policy_no_role_can_fail_owes_a_finding_not_a_probe() {
        let every_role = json!([
            "or",
            ["=", "@user.role", "admin"],
            ["=", "@user.role", "viewer"]
        ]);
        let program = transient_schema(every_role, persisting());
        assert!(matches!(
            transient(&program, "FAILED"),
            Some(TransientOutcome::Finding {
                finding: TransientFinding::NoDenyingViewer { .. }
            })
        ));
        let escape = json!([
            "or",
            ["=", "@entity.isDirect", true],
            ["=", "@user.role", "admin"]
        ]);
        let literal_true =
            transient_schema(escape.clone(), persisting_data(json!({ "isDirect": true })));
        assert!(
            matches!(
                transient(&literal_true, "FAILED"),
                Some(TransientOutcome::Finding { .. })
            ),
            "a payload literal that makes the policy unconditionally true leaves no viewer to deny"
        );
        let literal_false = transient_schema(escape, persisting_data(json!({ "isDirect": false })));
        assert!(
            matches!(
                transient(&literal_false, "FAILED"),
                Some(TransientOutcome::Probe { .. })
            ),
            "control: the escape hatch is specific to the payload, not the entity"
        );
    }

    #[test]
    fn a_replay_is_the_hops_that_place_a_fresh_machine_in_a_state() {
        let program = schema(json!([]));
        let hops = plan_replay(&program, "Runner", "ready", &PlanOptions::default())
            .expect("ready is reachable");
        assert_eq!(
            hops.iter().map(|h| h.event.as_str()).collect::<Vec<_>>(),
            vec!["GO"],
            "LOADED is fired by the fetch"
        );
        assert_eq!(
            (hops[0].from.as_str(), hops[0].to.as_str()),
            ("idle", "loading")
        );
        assert_eq!(
            plan_replay(&program, "Runner", "idle", &PlanOptions::default()),
            Some(Vec::new())
        );
        assert!(
            plan_replay(&program, "Runner", "nowhere", &PlanOptions::default()).is_none(),
            "an unreachable state has no replay"
        );
        assert!(plan_replay(&program, "Nobody", "idle", &PlanOptions::default()).is_none());
    }

    #[test]
    fn a_crud_chain_carries_the_values_a_form_is_filled_with_and_the_edit_that_changes_them() {
        let mut program = serde_json::to_value(crud_schema(
            json!(["render-ui", "modal", { "type": "form-section" }]),
        ))
        .unwrap();
        let orbital = &mut program["orbitals"][0];
        orbital["entity"]["fields"] = json!([
            { "name": "id", "type": "string" }, { "name": "title", "type": "string" },
            { "name": "count", "type": "number" }, { "name": "status", "type": "string", "values": ["open", "closed"] }]);
        orbital["traits"][1]["stateMachine"]["events"] = json!([
            { "key": "NOTE_SAVE", "name": "NOTE_SAVE", "payloadSchema": [{ "name": "data", "type": "object", "entity": "Note" }] }]);
        let schema: OrbitalSchema = serde_json::from_value(program).unwrap();
        let chains = plan_crud_chains(&schema, &PlanOptions::default());
        let create = chains.iter().find(|c| c.kind == CrudKind::Create).unwrap();
        assert!(
            create.submit_payload["data"]["title"].is_string(),
            "{}",
            create.submit_payload
        );
        let (row, edited) = (&create.entity_row, &create.edit_entity_row);
        assert_eq!(row["id"], edited["id"], "the row keeps its identity");
        assert_ne!(row["title"], edited["title"]);
        assert_ne!(row["count"], edited["count"]);
        assert_eq!(create.enum_fields, vec!["status".to_string()]);
        assert_eq!(
            row["status"], edited["status"],
            "control: a declared vocabulary is not changed out of itself"
        );
    }

    fn create(success: Option<&str>) -> DataMutation {
        let mut mutation: DataMutation = serde_json::from_value(serde_json::json!({
            "trait": "Notes", "from": "idle", "event": "SAVE", "to": "idle", "payload": {},
            "entity": "Note", "kind": "create", "rowDelta": 1, "coverageKey": "Notes:idle+SAVE"
        }))
        .unwrap();
        mutation.success_event = success.map(str::to_string);
        mutation
    }

    #[test]
    fn a_data_mutation_fails_when_its_persist_is_refused_and_passes_when_it_lands() {
        use crate::verify::frame::FrameStatus;
        let refused = MutationObservation {
            persist_outcome: Some("denied".into()),
            persist_error: Some("policy".into()),
            ..Default::default()
        };
        assert_eq!(
            judge_data_mutation(&create(Some("SAVED")), &refused).status,
            FrameStatus::Fail
        );
        let succeeded = MutationObservation {
            persist_outcome: Some("success".into()),
            ..Default::default()
        };
        assert_eq!(
            judge_data_mutation(&create(Some("SAVED")), &succeeded).status,
            FrameStatus::Pass
        );
    }

    #[test]
    fn without_a_persist_outcome_the_declared_success_event_or_the_row_delta_decides() {
        use crate::verify::frame::FrameStatus;
        let emitted = |events: &[&str]| MutationObservation {
            emitted: events.iter().map(|e| e.to_string()).collect(),
            ..Default::default()
        };
        assert_eq!(
            judge_data_mutation(&create(Some("SAVED")), &emitted(&["SAVED"])).status,
            FrameStatus::Pass
        );
        assert_eq!(
            judge_data_mutation(&create(Some("SAVED")), &emitted(&["SAVE_FAILED"])).status,
            FrameStatus::Fail,
            "control: the failure route instead"
        );
        let elsewhere = MutationObservation {
            state_before: Some("loading".into()),
            ..Default::default()
        };
        assert_eq!(
            judge_data_mutation(&create(Some("SAVED")), &elsewhere).status,
            FrameStatus::Inconclusive,
            "fired outside `from`"
        );
        let moved = MutationObservation {
            row_delta: Some(1),
            ..Default::default()
        };
        assert_eq!(
            judge_data_mutation(&create(None), &moved).status,
            FrameStatus::Pass
        );
        assert_eq!(
            judge_data_mutation(&create(None), &MutationObservation::default()).status,
            FrameStatus::Fail,
            "no change recorded"
        );
    }
}
