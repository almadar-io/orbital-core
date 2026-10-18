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
    let resolved = options.traits.as_deref().and_then(|names| planner::resolve_trait_names(schema, names).ok());
    planner::inline_traits_scoped(schema, resolved.as_deref())
}

fn entity_ctx<'a>(schema: &'a OrbitalSchema, owner_fields: &'a [(String, String)], options: &PlanOptions) -> planner::EntityCtx<'a> {
    planner::EntityCtx { schema, owner_fields, viewer_id: options.viewer_id.clone() }
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
        if !contract.event.starts_with('@') && !is_route(&contract.event) && !out.contains(&contract.event) {
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
    use orbital_core_base::schema::types::StringOrNumber;
    let mut out = Vec::new();
    for td in scoped(schema, options) {
        let Some(initial) = planner::initial_state(td) else { continue };
        let mut declared = vec![initial.clone()];
        for t in &td.state_machine.transitions {
            for state in t.from.states().into_iter().chain(std::iter::once(t.to.as_str())) {
                if state != "*" && !declared.iter().any(|d| d == state) {
                    declared.push(state.to_string());
                }
            }
        }
        for tick in td.ticks.iter().flatten() {
            let StringOrNumber::Number(interval) = &tick.interval else { continue };
            if !(*interval > 0.0) {
                continue;
            }
            out.push(TickStep {
                trait_name: td.name.clone(),
                from: initial.clone(),
                event: tick.name.clone(),
                wait_ms: *interval as u64,
                accept_states: declared.clone(),
                coverage_key: format!("{}:tick({})", td.name, tick.name),
            });
        }
    }
    out
}

fn bare_payload_field(text: &str) -> Option<&str> {
    let field = text.strip_prefix("@payload.")?;
    (!field.is_empty() && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(field)
}

/// The first `persist create|update|delete` of a transition's top-level effects: entity, kind, success event.
fn find_persist_kind(effects: &[SExpression]) -> Option<(String, MutationKind, Option<String>)> {
    for effect in effects {
        let SExpression::Array(items) = effect else { continue };
        let (Some(SExpression::String(op)), Some(SExpression::String(kind)), Some(SExpression::String(entity))) =
            (items.first(), items.get(1), items.get(2))
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

fn persist_calls<'a>(effects: &'a [SExpression], wanted: &[&str]) -> Vec<(&'a str, &'a str, Option<&'a SExpression>)> {
    effects
        .iter()
        .filter_map(|e| match e {
            SExpression::Array(items) => match (items.first(), items.get(1), items.get(2)) {
                (Some(SExpression::String(op)), Some(SExpression::String(kind)), Some(SExpression::String(name)))
                    if op == "persist" && wanted.contains(&kind.as_str()) =>
                {
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
                if schema.iter().any(|f| f.name == field && f.entity.as_deref() == Some(name)) {
                    return Some(PersistBinding { payload_field: field.to_string(), whole_row: true });
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
                    return Some(PersistBinding { payload_field: field.to_string(), whole_row: false });
                }
            }
            Some(SExpression::Object(o)) => {
                if let Some(SExpression::String(id)) = o.get("id") {
                    if let Some(field) = bare_payload_field(id) {
                        return Some(PersistBinding { payload_field: field.to_string(), whole_row: false });
                    }
                }
            }
            _ => {}
        }
    }
    let set_path = effects.iter().find_map(|e| precondition::find_set(e, "@entity.id")).and_then(|v| match v {
        SExpression::String(text) => text.strip_prefix("@payload.").map(str::to_string),
        _ => None,
    });
    let writes_entity_row = persists(&["update", "delete"]).into_iter().any(|(_, name, reference)| {
        name == entity && matches!(reference, Some(SExpression::String(r)) if r == "@entity" || r == "@entity.id")
    });
    set_path.filter(|_| writes_entity_row).map(|payload_field| PersistBinding { payload_field, whole_row: false })
}

fn sets_entity_id(transition: &orbital_core_base::schema::types::TransitionDefinition) -> bool {
    transition.effects.iter().flatten().any(|e| precondition::find_set(e, "@entity.id").is_some())
}

fn entity_id_payload_path(transition: &orbital_core_base::schema::types::TransitionDefinition) -> Option<String> {
    transition.effects.iter().flatten().find_map(|e| match precondition::find_set(e, "@entity.id") {
        Some(SExpression::String(text)) => text.strip_prefix("@payload.").map(str::to_string),
        _ => None,
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
    let seed = td.linked_entity.as_deref().map(|e| planner::effective_seed_id(e, ctx.owner_fields, ctx.viewer_id.as_deref()));
    let from_here = |t: &orbital_core_base::schema::types::TransitionDefinition| t.from.matches(at_state) || t.from.matches("*");
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
        entity_id_payload_path(t).filter(|p| !p.contains('.')).map(|p| (t, p))
    });
    if let Some((selector, payload_field)) = selector {
        return RowEstablish::Established {
            event: selector.event.clone(),
            payload: planner::synth_payload(td, &selector.event, ctx, seed.as_deref()),
            viewer: None,
            bind_row_from: Some(planner::BindRowFrom { entity: entity.to_string(), payload_field }),
            before_replay,
        };
    }
    RowEstablish::Unreachable { trait_name: td.name.clone(), entity: entity.to_string(), at_state: at_state.to_string() }
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
        return Some(resolve_row_establish(schema, ctx, td, entity, initial, false));
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
    Some(resolve_row_establish(schema, ctx, td, entity, initial, true))
}

/// Every persist-writing transition a host can drive as a data-mutation step: not INIT, not fired by an effect's
/// own outcome, not behind a constant-false guard.
pub fn plan_data_mutations(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<DataMutation> {
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let ctx = entity_ctx(schema, &owner_fields, options);
    let mut out = Vec::new();
    for td in scoped(schema, options) {
        let Some(initial) = planner::initial_state(td) else { continue };
        let emitted = planner::effect_emitted_events(td);
        let seed = td.linked_entity.as_deref().map(|e| planner::effective_seed_id(e, &owner_fields, options.viewer_id.as_deref()));
        for transition in &td.state_machine.transitions {
            if transition.event == "INIT" || emitted.contains(&transition.event) {
                continue;
            }
            if transition.guard.as_ref().and_then(planner::const_truth) == Some(false) {
                continue;
            }
            let Some((entity, kind, success_event)) = find_persist_kind(transition.effects.as_deref().unwrap_or(&[])) else { continue };
            let from = transition.from.states().first().copied().unwrap_or("").to_string();
            let bind_row = if kind == MutationKind::Create { None } else { persist_binding(td, transition, &entity) };
            let mut row_establish = if kind == MutationKind::Create || bind_row.is_some() {
                None
            } else {
                plan_row_establish(schema, &ctx, td, &entity, &from, &initial)
            };
            let mut precondition = None;
            if row_establish.is_none() && transition.guard.is_some() {
                precondition = precondition::plan_precondition(&ctx, td, transition, &from, &serde_json::json!({}));
            }
            let established_viewer = matches!(&row_establish, Some(RowEstablish::Established { viewer: Some(_), .. }));
            let operation = match kind {
                MutationKind::Create => "create",
                MutationKind::Update => "update",
                MutationKind::Delete => "delete",
            };
            let viewer = if established_viewer { None } else { viewer_requirement(schema, &entity, operation) };
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
                coverage_key: format!("{}:{from}+{}->{}[data-mutation:{operation}]", td.name, transition.event, transition.to),
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
        J::Number(n) => n.as_f64().map_or_else(|| row.clone(), |n| serde_json::json!(n + 1.0)),
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
            let (Some(name), Some(relation)) = (&field.name, &field.relation) else { continue };
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
                other.push(InboundRelation { entity: def.name.clone(), field: name.clone() });
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
                let create = persists.iter().find(|(k, _, _)| *k == "create").map(|(_, _, t)| *t);
                if let Some(chain) = build_chain(schema, &ctx, &all, persistor, &entity, kind, success, transition, create) {
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
    let row_entity = source.linked_entity.as_deref().unwrap_or(entity);
    let seed = source.linked_entity.as_deref().map(|e| planner::effective_seed_id(e, ctx.owner_fields, ctx.viewer_id.as_deref()));
    let submit_payload = planner::synth_payload(source, &listener.event, ctx, seed.as_deref());
    let entity_row = planner::synth_entity_row(ctx, row_entity);
    let enum_fields: Vec<String> = planner::entity_def(schema, row_entity)
        .into_iter()
        .flat_map(|e| e.fields.iter())
        .filter(|f| f.values.as_ref().is_some_and(|v| !v.is_empty()))
        .filter_map(|f| f.name.clone())
        .collect();
    let (avoid_via, avoid_other) = if crud_kind == CrudKind::Delete { inbound_restrict(schema, entity) } else { (Vec::new(), Vec::new()) };
    let fallback = create.filter(|_| crud_kind == CrudKind::Delete && !(avoid_via.is_empty() && avoid_other.is_empty())).map(|create| {
        let seed = persistor.linked_entity.as_deref().map(|e| planner::effective_seed_id(e, ctx.owner_fields, ctx.viewer_id.as_deref()));
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
        submit_payload,
        edit_entity_row: vary_row(&entity_row, &enum_fields),
        entity_row,
        enum_fields,
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
        avoid_referenced_via: avoid_via,
        avoid_referenced_by_other_entities: avoid_other,
        delete_establish_fallback: fallback,
        coverage_key: format!("{}:{}+{}->{}[{label}]", source.name, open.from.states().first().copied().unwrap_or(""), open.event, open.to),
    })
}

/// The hops that place a fresh machine of `trait_name` in `state`; `None` for an unknown trait or an unreachable state.
pub fn plan_replay(schema: &OrbitalSchema, trait_name: &str, state: &str, options: &PlanOptions) -> Option<Vec<PreambleEvent>> {
    let td = planner::inline_traits(schema).into_iter().find(|td| td.name == trait_name)?;
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
        let Some(initial) = planner::initial_state(td) else { continue };
        let seed_id = td.linked_entity.as_deref().map(|e| planner::effective_seed_id(e, &owner_fields, options.viewer_id.as_deref()));
        let mut seen: Vec<String> = Vec::new();
        for event in declared_emits(schema, td) {
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
            Some(Vec::<&str>::new()),
            "a host mounts the trait, which fires the boot INIT itself: the initial state needs no hop"
        );
        let leave = affordance(&program, "Runner", "ready", "LEAVE").unwrap();
        assert!(leave.navigates);
        assert_eq!(leave.preamble.as_ref().map(|p| p.iter().map(|e| e.event.as_str()).collect::<Vec<_>>()), Some(vec!["GO"]), "LOADED is fired by the fetch, never dispatched");
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

    fn crud_with_relations(on_delete: Option<&str>) -> OrbitalSchema {
        let mut program = serde_json::to_value(crud_schema(json!(["render-ui", "modal", { "type": "form-section" }]))).unwrap();
        let mut relation = json!({ "entity": "Note", "cardinality": "many-to-one" });
        if let Some(rule) = on_delete {
            relation["onDelete"] = json!(rule);
        }
        let orbital = &mut program["orbitals"][0];
        orbital["entity"]["fields"].as_array_mut().unwrap().push(json!({ "name": "parentId", "type": "relation", "relation": relation.clone() }));
        orbital["auxiliaryEntities"] = json!([{ "name": "Comment", "persistence": "runtime", "fields": [
            { "name": "id", "type": "string" }, { "name": "noteId", "type": "relation", "relation": relation }] }]);
        serde_json::from_value(program).unwrap()
    }

    #[test]
    fn a_delete_chain_names_the_restrict_relations_that_can_block_it_and_the_create_that_refills() {
        let chains = plan_crud_chains(&crud_with_relations(None), &PlanOptions::default());
        let delete = chains.iter().find(|c| c.kind == CrudKind::Delete).unwrap();
        assert_eq!(delete.avoid_referenced_via, vec!["parentId".to_string()], "an undeclared onDelete is restrict");
        assert_eq!(delete.avoid_referenced_by_other_entities, vec![InboundRelation { entity: "Comment".into(), field: "noteId".into() }]);
        let fallback = delete.delete_establish_fallback.as_ref().expect("the entity declares its own create");
        assert_eq!((fallback.event.as_str(), fallback.trait_name.as_str()), ("DO_CREATE", "Notes"));
        let create = chains.iter().find(|c| c.kind == CrudKind::Create).unwrap();
        assert!(create.avoid_referenced_via.is_empty() && create.delete_establish_fallback.is_none(), "only a delete chain avoids");
    }

    #[test]
    fn control_a_cascade_relation_blocks_nothing_and_an_entity_without_relations_needs_no_fallback() {
        let cascade = plan_crud_chains(&crud_with_relations(Some("cascade")), &PlanOptions::default());
        let delete = cascade.iter().find(|c| c.kind == CrudKind::Delete).unwrap();
        assert!(delete.avoid_referenced_via.is_empty() && delete.avoid_referenced_by_other_entities.is_empty());
        assert!(delete.delete_establish_fallback.is_none());
        let restrict = plan_crud_chains(&crud_with_relations(Some("restrict")), &PlanOptions::default());
        assert_eq!(restrict.iter().find(|c| c.kind == CrudKind::Delete).unwrap().avoid_referenced_via.len(), 1);
        let plain = plan_crud_chains(&crud_schema(json!(["render-ui", "modal", { "type": "form-section" }])), &PlanOptions::default());
        assert!(plain.iter().find(|c| c.kind == CrudKind::Delete).unwrap().delete_establish_fallback.is_none());
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
        all.iter().find(|m| m.event == event).unwrap_or_else(|| panic!("{event} is planned: {all:?}"))
    }

    #[test]
    fn each_persist_transition_is_a_data_mutation_with_its_row_delta_and_row_binding() {
        let all = plan_data_mutations(&mutation_schema(), &PlanOptions::default());
        let add = mutation(&all, "ADD");
        assert_eq!((add.kind, add.row_delta, add.success_event.as_deref()), (MutationKind::Create, 1, Some("ADDED")));
        assert_eq!(add.coverage_key, "Notes:idle+ADD->idle[data-mutation:create]");
        assert!(add.bind_row.is_none() && add.row_establish.is_none(), "a create has no target row");
        let save = mutation(&all, "SAVE");
        assert_eq!((save.kind, save.row_delta), (MutationKind::Update, 0));
        assert_eq!(save.bind_row, Some(PersistBinding { payload_field: "data".into(), whole_row: true }));
        let remove = mutation(&all, "REMOVE");
        assert_eq!((remove.kind, remove.row_delta), (MutationKind::Delete, -1));
        assert_eq!(remove.bind_row, Some(PersistBinding { payload_field: "id".into(), whole_row: false }));
        assert!(all.iter().all(|m| m.event != "INIT" && m.event != "PICK"), "only persist transitions are mutations");
    }

    #[test]
    fn a_step_that_writes_a_row_nothing_binds_gets_the_establishing_transition_or_a_typed_unreachable() {
        let all = plan_data_mutations(&mutation_schema(), &PlanOptions::default());
        let touch = mutation(&all, "TOUCH");
        assert!(touch.bind_row.is_none());
        match &touch.row_establish {
            Some(RowEstablish::Established { event, bind_row_from, before_replay, .. }) => {
                assert_eq!(event, "ADD", "the create self-loop at the initial state that binds @entity.id");
                assert!(bind_row_from.is_none() && !before_replay);
            }
            other => panic!("expected the create self-loop, got {other:?}"),
        }
        let mut program = serde_json::to_value(mutation_schema()).unwrap();
        let transitions = program["orbitals"][0]["traits"][0]["stateMachine"]["transitions"].as_array_mut().unwrap();
        transitions.retain(|t| t["event"] != "ADD" && t["event"] != "PICK");
        let bare: OrbitalSchema = serde_json::from_value(program).unwrap();
        let touch = plan_data_mutations(&bare, &PlanOptions::default()).into_iter().find(|m| m.event == "TOUCH").unwrap();
        assert_eq!(
            touch.row_establish,
            Some(RowEstablish::Unreachable { trait_name: "Notes".into(), entity: "Note".into(), at_state: "idle".into() })
        );
    }

    #[test]
    fn a_dispatched_row_selector_is_bound_from_a_payload_field_and_a_constant_false_guard_is_not_planned() {
        let mut program = serde_json::to_value(mutation_schema()).unwrap();
        let transitions = program["orbitals"][0]["traits"][0]["stateMachine"]["transitions"].as_array_mut().unwrap();
        transitions.retain(|t| t["event"] != "ADD");
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "NEVER", "guard": ["=", 1, 2],
            "effects": [["persist", "create", "Note", { "title": "t" }]] }));
        let schema: OrbitalSchema = serde_json::from_value(program).unwrap();
        let all = plan_data_mutations(&schema, &PlanOptions::default());
        match &mutation(&all, "TOUCH").row_establish {
            Some(RowEstablish::Established { event, bind_row_from, .. }) => {
                assert_eq!(event, "PICK");
                assert_eq!(bind_row_from.as_ref().map(|b| b.payload_field.as_str()), Some("id"));
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
        let events: Vec<String> = plan_emit_sweep(&program, &PlanOptions::default()).into_iter().map(|s| s.event).collect();
        assert_eq!(events, vec!["LOADED", "LOAD_FAILED", "PING"], "discovery order, http routes and external names left out");
    }

    fn persisting_data(data: serde_json::Value) -> serde_json::Value {
        json!(["persist", "create", "Note", data, { "emit": { "success": "DONE", "failure": "FAILED" } }])
    }

    #[test]
    fn a_policy_no_role_can_fail_owes_a_finding_not_a_probe() {
        let every_role = json!(["or", ["=", "@user.role", "admin"], ["=", "@user.role", "viewer"]]);
        let program = transient_schema(every_role, persisting());
        assert!(matches!(
            transient(&program, "FAILED"),
            Some(TransientOutcome::Finding { finding: TransientFinding::NoDenyingViewer { .. } })
        ));
        let escape = json!(["or", ["=", "@entity.isDirect", true], ["=", "@user.role", "admin"]]);
        let literal_true = transient_schema(escape.clone(), persisting_data(json!({ "isDirect": true })));
        assert!(
            matches!(transient(&literal_true, "FAILED"), Some(TransientOutcome::Finding { .. })),
            "a payload literal that makes the policy unconditionally true leaves no viewer to deny"
        );
        let literal_false = transient_schema(escape, persisting_data(json!({ "isDirect": false })));
        assert!(
            matches!(transient(&literal_false, "FAILED"), Some(TransientOutcome::Probe { .. })),
            "control: the escape hatch is specific to the payload, not the entity"
        );
    }

    #[test]
    fn a_replay_is_the_hops_that_place_a_fresh_machine_in_a_state() {
        let program = schema(json!([]));
        let hops = plan_replay(&program, "Runner", "ready", &PlanOptions::default()).expect("ready is reachable");
        assert_eq!(hops.iter().map(|h| h.event.as_str()).collect::<Vec<_>>(), vec!["GO"], "LOADED is fired by the fetch");
        assert_eq!((hops[0].from.as_str(), hops[0].to.as_str()), ("idle", "loading"));
        assert_eq!(plan_replay(&program, "Runner", "idle", &PlanOptions::default()), Some(Vec::new()));
        assert!(plan_replay(&program, "Runner", "nowhere", &PlanOptions::default()).is_none(), "an unreachable state has no replay");
        assert!(plan_replay(&program, "Nobody", "idle", &PlanOptions::default()).is_none());
    }

    #[test]
    fn a_crud_chain_carries_the_values_a_form_is_filled_with_and_the_edit_that_changes_them() {
        let mut program = serde_json::to_value(crud_schema(json!(["render-ui", "modal", { "type": "form-section" }]))).unwrap();
        let orbital = &mut program["orbitals"][0];
        orbital["entity"]["fields"] = json!([
            { "name": "id", "type": "string" }, { "name": "title", "type": "string" },
            { "name": "count", "type": "number" }, { "name": "status", "type": "string", "values": ["open", "closed"] }]);
        orbital["traits"][1]["stateMachine"]["events"] = json!([
            { "key": "NOTE_SAVE", "name": "NOTE_SAVE", "payloadSchema": [{ "name": "data", "type": "object", "entity": "Note" }] }]);
        let schema: OrbitalSchema = serde_json::from_value(program).unwrap();
        let chains = plan_crud_chains(&schema, &PlanOptions::default());
        let create = chains.iter().find(|c| c.kind == CrudKind::Create).unwrap();
        assert!(create.submit_payload["data"]["title"].is_string(), "{}", create.submit_payload);
        let (row, edited) = (&create.entity_row, &create.edit_entity_row);
        assert_eq!(row["id"], edited["id"], "the row keeps its identity");
        assert_ne!(row["title"], edited["title"]);
        assert_ne!(row["count"], edited["count"]);
        assert_eq!(create.enum_fields, vec!["status".to_string()]);
        assert_eq!(row["status"], edited["status"], "control: a declared vocabulary is not changed out of itself");
    }
}
