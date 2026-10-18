//! The verifier's planner and verdict logic as one typed wire: what to fire, what to expect, and the
//! verdict on what was observed. A host that drives the program itself (the browser driver, a shell)
//! asks the kernel for the plan and judges each observed step here, so the planner and the verdict
//! exist once. The native walk (`verify::driver`) judges through the same [`judge`].

use crate::planner::{self, ArmConflict, Precondition, GuardCase, PayloadCase, ReplyRoute, Step, Variant};
use crate::verify::frame::{EffectSummary, FiredHop, FrameStatus, GapReason};
use orbital_core_base::schema::types::OrbitalSchema;
use serde::{Deserialize, Serialize};

/// The `ts-rs` mirror of `serde_json::Value` is the `JsonValue` of `@almadar/core`.
type Json = serde_json::Value;

#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlanOptions {
    /// Restrict the plan to these traits (either name spelling a trait carries after resolve).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<String>>,
    /// The bound viewer's id; shapes the seed row id and ownership-stamped payloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer_id: Option<String>,
    /// Events the caller already covers; the contract plan skips them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub covered_events: Option<Vec<String>>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleWire {
    pub field: String,
    pub value: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreambleEvent {
    pub event: String,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: Json,
    /// The state the machine stands in when this hop is fired, and where the declared transition leads.
    pub from: String,
    pub to: String,
    /// States the machine may settle in beyond `to`, because an effect-emitted event advances it on its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accept_states: Vec<String>,
    /// Firing the hop can change the route.
    pub navigates: bool,
}

/// One planned step: place the machine in `from`, fire `event` with `payload`, then judge against
/// the [`Expectation`] projected from it.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepWire {
    #[serde(rename = "trait")]
    pub trait_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_entity: Option<String>,
    /// The instance the step fires on: the seed row id of the linked entity, or the verifier's fixed id.
    pub instance_id: String,
    /// The id of the row to seed before the step; absent when the step's own create makes that row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_row_id: Option<String>,
    /// The role a role-only persist policy needs the viewer to carry for this step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleWire>,
    pub from: String,
    pub event: String,
    pub to: String,
    pub to_states: Vec<String>,
    pub variant: Variant,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: Json,
    pub guarded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arm: Option<usize>,
    pub solved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<ReplyRoute>,
    /// The events (with payloads) that place a fresh machine in `from`; absent when `from` is unreachable.
    /// Each is a repositioning step: it moves the machine, it is not under test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preamble: Option<Vec<PreambleEvent>>,
    pub payload_case: PayloadCase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard_case: Option<GuardCase>,
    /// The payload can steer the arm's guard; absent on an unguarded step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard_steerable: Option<bool>,
    /// On a `guard-fail` step: the targets of the sibling arms that may fire in its place.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guard_sibling_targets: Vec<String>,
    /// Why the planned payload does not exercise this arm; the step is then unsolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arm_conflict: Option<ArmConflict>,
    /// What must be dispatched first for a guard reading `@entity` fields this trait does not set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precondition: Option<Precondition>,
    /// States beyond `toStates` the machine may settle in, because an effect-emitted event advances it on its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accept_states: Vec<String>,
    /// Firing the event can change the route, so the trait may unmount.
    pub navigates: bool,
    /// `trait:from+event->to` plus `[payloadCase]` (`[guardCase]` without one); the boot INIT carries no suffix.
    pub coverage_key: String,
}

/// What a step must do to pass: the oracle, independent of any observation.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expectation {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    pub to_states: Vec<String>,
    pub guarded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arm: Option<usize>,
    pub solved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<ReplyRoute>,
    pub payload_case: PayloadCase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard_case: Option<GuardCase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard_steerable: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accept_states: Vec<String>,
    /// The step's precondition is not shown to hold; a failure is then inconclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precondition_gap: Option<planner::UnprovenReason>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservedOutcome {
    Executed,
    Noop,
    Delegated,
    Failed,
}

#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedEffect {
    pub outcome: ObservedOutcome,
    /// The declared `emit.failure` event the program routed this failure to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routed: Option<String>,
}

/// What the host saw after firing a step.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub state_before: String,
    pub state_after: String,
    /// The arm the kernel selected among the `from --event-->` arms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired_arm: Option<usize>,
    pub effects: Vec<ObservedEffect>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum VerdictReason {
    Reached,
    HeldUnderGuard,
    NotDriven,
    PreambleMissed {
        actual: String,
    },
    ArmUnsolved,
    /// The guard-fail arm did not fire: the guard rejected the payload.
    GuardRejected,
    /// The guard-fail arm fired: the guard accepted a payload planned to fail it.
    GuardAccepted,
    /// The guard does not depend on the payload, so the planned branch cannot be driven from here.
    GuardUnsteerable,
    /// The empty payload is rejected by the event's payload validator, which the kernel does not run.
    PayloadValidatorNotRun,
    ArmNotFired {
        fired: Option<usize>,
    },
    WrongState {
        actual: String,
    },
    EffectFailed,
    /// The step failed, but the kernel could not show its guard precondition holds.
    PreconditionUnproven { reason: planner::UnprovenReason },
}

#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictWire {
    pub status: FrameStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<GapReason>,
    pub reason: VerdictReason,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PlanFailure {
    InvalidProgram { message: String },
    InvalidInput { message: String },
    UnknownTrait { name: String, available: String },
}

/// One config knob varied from the baseline.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigVariant {
    /// `knob = value`.
    pub label: String,
    pub field: String,
    /// The baseline defaults with `field` changed.
    #[cfg_attr(test, ts(type = "JsonObject"))]
    pub config: serde_json::Value,
}

/// A trait's config sweep: only values the knob declares. A knob with none (a number, a free string) is
/// listed in `noDeclaredValues` and varies nowhere.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitConfigVariants {
    #[serde(rename = "trait")]
    pub trait_name: String,
    #[cfg_attr(test, ts(type = "JsonObject"))]
    pub baseline: serde_json::Value,
    pub variants: Vec<ConfigVariant>,
    pub no_declared_values: Vec<String>,
}

/// The boot credit of one trait: the INIT the runtime fires on mount, whether or not the trait declares an INIT arm.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitCredit {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub state: String,
    pub coverage_key: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum PlanOutcome {
    #[serde(rename_all = "camelCase")]
    Planned {
        steps: Vec<StepWire>,
        config_variants: Vec<TraitConfigVariants>,
        init_credits: Vec<InitCredit>,
        emit_sweep: Vec<crate::verify::flows::EmitStep>,
        contract_events: crate::verify::flows::ContractEvents,
        crud_chains: Vec<crate::verify::flows::CrudChain>,
        tick_steps: Vec<crate::verify::flows::TickStep>,
        data_mutations: Vec<crate::verify::flows::DataMutation>,
    },
    Refused { failure: PlanFailure },
}

impl Expectation {
    pub fn from_step(step: &Step) -> Self {
        Self {
            trait_name: step.trait_name.clone(),
            from: step.from.clone(),
            event: step.event.clone(),
            to_states: step.to_states.clone(),
            guarded: step.guarded,
            arm: step.arm,
            solved: step.solved,
            reply: step.reply,
            payload_case: step.payload_case,
            guard_case: step.guard_case,
            guard_steerable: step.guard_steerable,
            accept_states: Vec::new(),
            precondition_gap: step.precondition.as_ref().and_then(Precondition::gap),
        }
    }
}

impl StepWire {
    pub fn expectation(&self) -> Expectation {
        Expectation {
            trait_name: self.trait_name.clone(),
            from: self.from.clone(),
            event: self.event.clone(),
            to_states: self.to_states.clone(),
            guarded: self.guarded,
            arm: self.arm,
            solved: self.solved,
            reply: self.reply,
            payload_case: self.payload_case,
            guard_case: self.guard_case,
            guard_steerable: self.guard_steerable,
            accept_states: self.accept_states.clone(),
            precondition_gap: self.precondition.as_ref().and_then(Precondition::gap),
        }
    }
}

impl ObservedEffect {
    pub fn from_summary(effect: &EffectSummary) -> Self {
        let outcome = match effect.outcome.as_str() {
            "executed" => ObservedOutcome::Executed,
            "delegated" => ObservedOutcome::Delegated,
            "failed" => ObservedOutcome::Failed,
            _ => ObservedOutcome::Noop,
        };
        Self {
            outcome,
            routed: effect.routed.clone(),
        }
    }
}

/// The verdict on one observed step. A step that could not be placed in `from`, or whose planned arm
/// did not fire, is inconclusive (a coverage gap, not a transition failure); a reply route is never driven.
pub fn judge(expectation: &Expectation, observation: &Observation) -> VerdictWire {
    let verdict = judge_observed(expectation, observation);
    match (verdict.status, expectation.precondition_gap, &verdict.reason) {
        (FrameStatus::Fail, Some(reason), VerdictReason::WrongState { .. } | VerdictReason::ArmNotFired { .. }) => {
            VerdictWire {
                status: FrameStatus::Inconclusive,
                gap: None,
                reason: VerdictReason::PreconditionUnproven { reason },
            }
        }
        _ => verdict,
    }
}

fn judge_observed(expectation: &Expectation, observation: &Observation) -> VerdictWire {
    let verdict = |status, gap, reason| VerdictWire {
        status,
        gap,
        reason,
    };
    if expectation.reply.is_some() {
        return verdict(
            FrameStatus::Gap,
            Some(GapReason::NotReachableWithoutNetwork),
            VerdictReason::NotDriven,
        );
    }
    if observation.state_before != expectation.from {
        return verdict(
            FrameStatus::Inconclusive,
            None,
            VerdictReason::PreambleMissed {
                actual: observation.state_before.clone(),
            },
        );
    }
    if expectation.payload_case == PayloadCase::Malformed {
        return verdict(
            FrameStatus::Inconclusive,
            None,
            VerdictReason::PayloadValidatorNotRun,
        );
    }
    if let Some(arm) = expectation.arm {
        let planned_fired = observation.fired_arm == Some(arm);
        let planned_branch = if expectation.guard_case == Some(GuardCase::Fail) {
            !planned_fired
        } else {
            planned_fired
        };
        if expectation.guard_case.is_some()
            && expectation.guard_steerable == Some(false)
            && !planned_branch
        {
            return verdict(
                FrameStatus::Inconclusive,
                None,
                VerdictReason::GuardUnsteerable,
            );
        }
        if !expectation.solved {
            return verdict(FrameStatus::Inconclusive, None, VerdictReason::ArmUnsolved);
        }
        if expectation.guard_case == Some(GuardCase::Fail) {
            return if planned_fired {
                verdict(FrameStatus::Fail, None, VerdictReason::GuardAccepted)
            } else {
                verdict(FrameStatus::Pass, None, VerdictReason::GuardRejected)
            };
        }
        if !planned_fired {
            return verdict(
                FrameStatus::Fail,
                None,
                VerdictReason::ArmNotFired {
                    fired: observation.fired_arm,
                },
            );
        }
    }
    let effect_failed = observation
        .effects
        .iter()
        .any(|e| e.outcome == ObservedOutcome::Failed && e.routed.is_none());
    if effect_failed {
        return verdict(FrameStatus::Fail, None, VerdictReason::EffectFailed);
    }
    if expectation
        .to_states
        .iter()
        .chain(&expectation.accept_states)
        .any(|s| *s == observation.state_after)
    {
        return verdict(FrameStatus::Pass, None, VerdictReason::Reached);
    }
    if expectation.guarded
        && expectation.arm.is_none()
        && observation.state_after == observation.state_before
    {
        return verdict(FrameStatus::Pass, None, VerdictReason::HeldUnderGuard);
    }
    verdict(
        FrameStatus::Fail,
        None,
        VerdictReason::WrongState {
            actual: observation.state_after.clone(),
        },
    )
}

/// One planned step as the credit pass sees it: which transition it stands for, whether it is an `http`
/// reply route, and the transitions its own dispatch fired.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditStep {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<ReplyRoute>,
    #[serde(default)]
    pub fired: Vec<FiredHop>,
}

/// A step passed by another step's dispatch rather than driven itself.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credit {
    pub index: usize,
    pub state_before: String,
    pub state_after: String,
}

/// A failure-route step is never driven: the reply arrives inside the request's own dispatch. It is credited
/// from the hop that dispatch fired for the same `(trait, from, event)`, matching the arm when the step has one.
pub fn credit_failure_routes(steps: &[CreditStep]) -> Vec<Credit> {
    steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.reply == Some(ReplyRoute::Failure))
        .filter_map(|(index, step)| {
            let hop = steps.iter().flat_map(|s| s.fired.iter()).find(|hop| {
                hop.trait_name == step.trait_name
                    && hop.from == step.from
                    && hop.event == step.event
                    && step.arm.is_none_or(|arm| hop.arm == Some(arm))
            })?;
            Some(Credit { index, state_before: hop.from.clone(), state_after: hop.to.clone() })
        })
        .collect()
}

fn traits_in_scope<'a>(
    schema: &'a OrbitalSchema,
    options: &PlanOptions,
) -> Vec<&'a orbital_core_base::schema::types::TraitDefinition> {
    let resolved = options
        .traits
        .as_deref()
        .and_then(|names| planner::resolve_trait_names(schema, names).ok());
    planner::inline_traits_scoped(schema, resolved.as_deref())
}

/// The config sweep of every trait in scope: the baseline defaults, then each knob varied alone over the values
/// it declares (both booleans, an enum's members). Number and free-string knobs declare none and are reported.
pub fn plan_config_variants(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<TraitConfigVariants> {
    let mut out = Vec::new();
    for td in traits_in_scope(schema, options) {
        let Some(config) = td.config.as_ref().filter(|c| !c.is_empty()) else {
            continue;
        };
        let mut names: Vec<&String> = config.keys().collect();
        names.sort();
        let mut baseline = serde_json::Map::new();
        for name in &names {
            if let Some(default) = config[*name].default.as_ref().and_then(planner::literal_json) {
                baseline.insert((*name).clone(), default);
            }
        }
        let mut variants = Vec::new();
        let mut no_declared_values = Vec::new();
        for name in names {
            let field = &config[name];
            let declared: Vec<serde_json::Value> = match (field.field_type.as_str(), field.values.as_deref()) {
                ("boolean", _) => vec![true.into(), false.into()],
                ("string", Some(values)) if !values.is_empty() => values.iter().map(|v| v.clone().into()).collect(),
                _ => {
                    no_declared_values.push(name.clone());
                    continue;
                }
            };
            for value in declared {
                if baseline.get(name) == Some(&value) {
                    continue;
                }
                let label = match &value {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                let mut config = baseline.clone();
                config.insert(name.clone(), value);
                variants.push(ConfigVariant {
                    label: format!("{name} = {label}"),
                    field: name.clone(),
                    config: serde_json::Value::Object(config),
                });
            }
        }
        out.push(TraitConfigVariants {
            trait_name: td.name.clone(),
            baseline: serde_json::Value::Object(baseline),
            variants,
            no_declared_values,
        });
    }
    out
}

/// The boot credit of every trait in scope.
pub fn plan_init_credits(schema: &OrbitalSchema, options: &PlanOptions) -> Vec<InitCredit> {
    traits_in_scope(schema, options)
        .into_iter()
        .filter_map(|td| {
            let state = planner::initial_state(td)?;
            Some(InitCredit {
                coverage_key: format!("{}:{state}+INIT->{state}", td.name),
                trait_name: td.name.clone(),
                state,
            })
        })
        .collect()
}

/// The hops that place a fresh machine in `from` (none at the boot), each with the payload its guard accepts.
pub(crate) fn preamble_to(
    td: &orbital_core_base::schema::types::TraitDefinition,
    ctx: &planner::EntityCtx<'_>,
    from: &str,
    boots: bool,
) -> Option<Vec<PreambleEvent>> {
    if boots {
        return Some(Vec::new());
    }
    let seed_id = td.linked_entity.as_deref().map(|e| planner::effective_seed_id(e, ctx.owner_fields, ctx.viewer_id.as_deref()));
    planner::replay_steps(td, from, ctx, seed_id.as_deref()).map(|path| {
        path.into_iter()
            .map(|h| PreambleEvent { event: h.event, payload: h.payload, from: h.from, to: h.to, accept_states: h.accept_states, navigates: h.navigates })
            .collect()
    })
}

/// What a host does to place the machine at `from` and fire `event` there.
pub(crate) struct DriveSetup {
    pub seed_row_id: Option<String>,
    pub instance_id: String,
    pub role: Option<RoleWire>,
    pub preamble: Option<Vec<PreambleEvent>>,
}

pub(crate) fn drive_setup(
    schema: &OrbitalSchema,
    td: &orbital_core_base::schema::types::TraitDefinition,
    ctx: &planner::EntityCtx<'_>,
    linked_entity: Option<&str>,
    from: &str,
    event: &str,
    boots: bool,
) -> DriveSetup {
    let viewer = ctx.viewer_id.as_deref();
    let owner_fields = ctx.owner_fields;
    let seed_id = td.linked_entity.as_deref().map(|e| planner::effective_seed_id(e, owner_fields, viewer));
    let creates_own_row = |entity: &str| {
        planner::is_self_identity(entity, owner_fields)
            && planner::created_only_entities(td, event).iter().any(|e| e == entity)
    };
    DriveSetup {
        seed_row_id: linked_entity
            .filter(|e| !creates_own_row(e))
            .map(|e| planner::effective_seed_id(e, owner_fields, viewer)),
        instance_id: linked_entity.map_or_else(
            || crate::verify::driver::ENTITY_ID.to_string(),
            |e| planner::effective_seed_id(e, owner_fields, viewer),
        ),
        role: planner::persist_targets(td, from, event)
            .into_iter()
            .find_map(|(entity, kind)| crate::entity_access::role_satisfying_policy(schema, &entity, kind))
            .map(|(field, value)| RoleWire { field, value }),
        preamble: preamble_to(td, ctx, from, boots),
    }
}

/// Plan every trait of a resolved program (or the `options.traits` scope): the steps a host fires.
pub fn plan_program(
    schema: &OrbitalSchema,
    options: &PlanOptions,
) -> Result<Vec<StepWire>, PlanFailure> {
    let resolved = options
        .traits
        .as_deref()
        .map(|names| planner::resolve_trait_names(schema, names))
        .transpose()
        .map_err(|e| PlanFailure::UnknownTrait {
            name: e.name,
            available: e.available,
        })?;
    let owner_fields = orbital_core_base::policy::owner_fields_from_schema(schema);
    let viewer = options.viewer_id.as_deref();
    let ctx = planner::EntityCtx {
        schema,
        owner_fields: &owner_fields,
        viewer_id: options.viewer_id.clone(),
    };
    let mut out = Vec::new();
    for td in planner::inline_traits_scoped(schema, resolved.as_deref()) {
        let seed_id = td
            .linked_entity
            .as_deref()
            .map(|e| planner::effective_seed_id(e, &owner_fields, viewer));
        for step in planner::plan_trait_variants(td, &ctx, seed_id.as_deref()) {
            let undrivable = match (step.guard_case, step.const_guard) {
                (Some(GuardCase::Pass), Some(false)) | (Some(GuardCase::Fail), Some(true)) => true,
                _ => false,
            };
            if undrivable {
                continue;
            }
            let boots = step.variant == Variant::AutoInit && step.event == "INIT";
            let DriveSetup { seed_row_id, instance_id, role, preamble } = drive_setup(
                schema,
                td,
                &ctx,
                step.linked_entity.as_deref(),
                &step.from,
                &step.event,
                boots,
            );
            let base_key = format!(
                "{}:{}+{}->{}",
                step.trait_name, step.from, step.event, step.to
            );
            let coverage_key = match (boots, step.payload_case, step.guard_case) {
                (true, _, _) => base_key,
                (_, PayloadCase::Success, Some(GuardCase::Pass) | None) => {
                    format!("{base_key}[success]")
                }
                (_, PayloadCase::Malformed, _) => format!("{base_key}[malformed]"),
                _ => format!("{base_key}[guard-fail]"),
            };
            let settles = step.guard_case != Some(GuardCase::Fail)
                && step.payload_case != PayloadCase::Malformed;
            let accept_states = if settles {
                let emitted = planner::effect_emitted_events(td);
                let mut closure: Vec<String> = Vec::new();
                let starts: Vec<&str> = if emitted.contains(&step.event) {
                    vec![step.from.as_str()]
                } else {
                    step.to_states.iter().map(String::as_str).collect()
                };
                for start in starts {
                    let reach = planner::transient_closure(td, start);
                    if reach.len() > 1 {
                        for state in reach {
                            if !closure.contains(&state) {
                                closure.push(state);
                            }
                        }
                    }
                }
                closure
            } else {
                Vec::new()
            };
            let navigates = planner::dispatch_navigates(schema, td, &step.event);
            out.push(StepWire {
                trait_name: step.trait_name,
                linked_entity: step.linked_entity,
                instance_id,
                seed_row_id,
                role,
                from: step.from,
                event: step.event,
                to: step.to,
                to_states: step.to_states,
                variant: step.variant,
                payload: step.payload,
                guarded: step.guarded,
                arm: step.arm,
                solved: step.solved,
                reply: step.reply,
                preamble,
                payload_case: step.payload_case,
                guard_case: step.guard_case,
                guard_steerable: step.guard_steerable,
                guard_sibling_targets: step.guard_sibling_targets,
                arm_conflict: step.arm_conflict,
                precondition: step.precondition,
                accept_states,
                navigates,
                coverage_key,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(
        before: &str,
        after: &str,
        arm: Option<usize>,
        effects: Vec<ObservedEffect>,
    ) -> Observation {
        Observation {
            state_before: before.into(),
            state_after: after.into(),
            fired_arm: arm,
            effects,
        }
    }

    fn expect(to: &[&str], guarded: bool, arm: Option<usize>, solved: bool) -> Expectation {
        Expectation {
            trait_name: "T".into(),
            from: "a".into(),
            event: "E".into(),
            to_states: to.iter().map(|s| s.to_string()).collect(),
            guarded,
            arm,
            solved,
            reply: None,
            payload_case: PayloadCase::Success,
            guard_case: arm.map(|_| GuardCase::Pass),
            guard_steerable: arm.map(|_| true),
            accept_states: Vec::new(),
            precondition_gap: None,
        }
    }

    #[test]
    fn judge_decides_each_outcome_from_the_expectation_alone() {
        let failed = ObservedEffect {
            outcome: ObservedOutcome::Failed,
            routed: None,
        };
        let routed = ObservedEffect {
            outcome: ObservedOutcome::Failed,
            routed: Some("X_FAILED".into()),
        };
        let status = |e: &Expectation, o: &Observation| judge(e, o).status;
        let plain = expect(&["b"], false, None, true);
        assert_eq!(
            status(&plain, &obs("a", "b", None, vec![])),
            FrameStatus::Pass
        );
        assert_eq!(
            status(&plain, &obs("a", "c", None, vec![])),
            FrameStatus::Fail
        );
        assert_eq!(
            judge(&plain, &obs("a", "c", None, vec![])).reason,
            VerdictReason::WrongState { actual: "c".into() }
        );
        assert_eq!(
            judge(&plain, &obs("a", "b", None, vec![failed])).reason,
            VerdictReason::EffectFailed
        );
        assert_eq!(
            status(&plain, &obs("a", "b", None, vec![routed])),
            FrameStatus::Pass
        );
        assert_eq!(
            status(&plain, &obs("z", "b", None, vec![])),
            FrameStatus::Inconclusive
        );
        let guarded = expect(&["b"], true, None, true);
        assert_eq!(
            judge(&guarded, &obs("a", "a", None, vec![])).reason,
            VerdictReason::HeldUnderGuard
        );
        let arm = expect(&["b"], true, Some(1), true);
        assert_eq!(
            status(&arm, &obs("a", "b", Some(0), vec![])),
            FrameStatus::Fail
        );
        assert_eq!(
            judge(&arm, &obs("a", "b", Some(0), vec![])).reason,
            VerdictReason::ArmNotFired { fired: Some(0) }
        );
        assert_eq!(
            judge(&arm, &obs("a", "a", None, vec![])).status,
            FrameStatus::Fail,
            "a solved pass payload that fires no arm is a divergence too"
        );
        assert_eq!(
            status(&arm, &obs("a", "b", Some(1), vec![])),
            FrameStatus::Pass
        );
        assert_eq!(
            status(
                &expect(&["b"], true, Some(1), false),
                &obs("a", "b", Some(1), vec![])
            ),
            FrameStatus::Inconclusive
        );
        let mut reply = plain.clone();
        reply.reply = Some(ReplyRoute::Other);
        let gap = judge(&reply, &obs("a", "a", None, vec![]));
        assert_eq!(
            (gap.status, gap.gap),
            (
                FrameStatus::Gap,
                Some(GapReason::NotReachableWithoutNetwork)
            )
        );
    }

    #[test]
    fn judge_ports_the_step_expectation_rules() {
        let status = |e: &Expectation, o: &Observation| judge(e, o).reason;
        let mut fail_arm = expect(&["b"], true, Some(1), true);
        fail_arm.guard_case = Some(GuardCase::Fail);
        fail_arm.payload_case = PayloadCase::GuardFail;
        assert_eq!(
            status(&fail_arm, &obs("a", "a", None, vec![])),
            VerdictReason::GuardRejected
        );
        assert_eq!(
            status(&fail_arm, &obs("a", "c", Some(0), vec![])),
            VerdictReason::GuardRejected
        );
        assert_eq!(
            status(&fail_arm, &obs("a", "b", Some(1), vec![])),
            VerdictReason::GuardAccepted
        );
        assert_eq!(
            judge(&fail_arm, &obs("a", "b", Some(1), vec![])).status,
            FrameStatus::Fail
        );
        let mut unsteerable = fail_arm.clone();
        unsteerable.guard_steerable = Some(false);
        assert_eq!(
            status(&unsteerable, &obs("a", "b", Some(1), vec![])),
            VerdictReason::GuardUnsteerable
        );
        assert_eq!(
            status(&unsteerable, &obs("a", "a", None, vec![])),
            VerdictReason::GuardRejected
        );
        let mut malformed = expect(&["b"], false, None, true);
        malformed.payload_case = PayloadCase::Malformed;
        assert_eq!(
            judge(&malformed, &obs("a", "b", None, vec![])).status,
            FrameStatus::Inconclusive
        );
        let mut settles = expect(&["b"], false, None, true);
        assert_eq!(
            judge(&settles, &obs("a", "c", None, vec![])).status,
            FrameStatus::Fail
        );
        settles.accept_states = vec!["c".into()];
        assert_eq!(
            judge(&settles, &obs("a", "c", None, vec![])).status,
            FrameStatus::Pass
        );
    }

    fn bare_guard_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Gate", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "open" }],
                        "events": [
                            { "key": "OPEN_ROW", "name": "OPEN_ROW", "payloadSchema": [{ "name": "row", "type": "object" }] },
                            { "key": "OPEN_FLAG", "name": "OPEN_FLAG", "payloadSchema": [{ "name": "on", "type": "boolean" }] },
                            { "key": "OPEN_TEXT", "name": "OPEN_TEXT", "payloadSchema": [{ "name": "text", "type": "string" }] },
                            { "key": "OPEN_COUNT", "name": "OPEN_COUNT", "payloadSchema": [{ "name": "count", "type": "number" }] }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "open", "event": "OPEN_ROW", "guard": "@payload.row" },
                            { "from": "idle", "to": "open", "event": "OPEN_FLAG", "guard": "@payload.on" },
                            { "from": "idle", "to": "open", "event": "OPEN_TEXT", "guard": "@payload.text" },
                            { "from": "idle", "to": "open", "event": "OPEN_COUNT", "guard": "@payload.count" }
                        ]
                    } }]
            }]
        }))
        .unwrap()
    }

    fn payload_of(steps: &[StepWire], event: &str, case: PayloadCase) -> serde_json::Value {
        steps
            .iter()
            .find(|s| s.event == event && s.payload_case == case)
            .unwrap_or_else(|| panic!("{event} {case:?} is planned"))
            .payload
            .clone()
    }

    #[test]
    fn a_bare_binding_guard_gets_a_value_of_the_declared_type() {
        let steps = plan_program(&bare_guard_schema(), &PlanOptions::default()).unwrap();
        let pass = |event: &str| payload_of(&steps, event, PayloadCase::Success);
        assert!(pass("OPEN_ROW")["row"].as_object().is_some_and(|o| !o.is_empty()), "{}", pass("OPEN_ROW"));
        assert_eq!(pass("OPEN_FLAG")["on"], true);
        assert!(pass("OPEN_TEXT")["text"].as_str().is_some_and(|t| !t.is_empty()), "{}", pass("OPEN_TEXT"));
        assert!(pass("OPEN_COUNT")["count"].as_f64().is_some_and(|n| n != 0.0), "{}", pass("OPEN_COUNT"));
    }

    #[test]
    fn control_the_failing_payload_of_a_bare_binding_guard_is_falsy_in_the_declared_type() {
        let steps = plan_program(&bare_guard_schema(), &PlanOptions::default()).unwrap();
        let fail = |event: &str| payload_of(&steps, event, PayloadCase::GuardFail);
        assert_eq!(fail("OPEN_FLAG")["on"], false);
        assert_eq!(fail("OPEN_TEXT")["text"], "");
        assert_eq!(fail("OPEN_COUNT")["count"], 0);
        assert!(fail("OPEN_ROW")["row"].is_null(), "{}", fail("OPEN_ROW"));
    }

    fn arms_schema(arms: serde_json::Value) -> OrbitalSchema {
        let mut transitions = vec![serde_json::json!({ "from": "idle", "to": "idle", "event": "INIT" })];
        transitions.extend(arms.as_array().unwrap().clone());
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Arms", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }, { "name": "failed" }],
                        "transitions": transitions
                    } }]
            }]
        }))
        .unwrap()
    }

    fn arm_step(steps: &[StepWire], arm: usize, case: PayloadCase) -> StepWire {
        steps
            .iter()
            .find(|s| s.event == "REPLY" && s.arm == Some(arm) && s.payload_case == case)
            .unwrap_or_else(|| panic!("arm {arm} {case:?} is planned"))
            .clone()
    }

    #[test]
    fn an_arm_an_earlier_arm_takes_first_is_unsolved_with_the_typed_conflict() {
        let shadowed = arms_schema(serde_json::json!([
            { "from": "idle", "to": "done", "event": "REPLY", "guard": ["<", "@payload.status", 500] },
            { "from": "idle", "to": "failed", "event": "REPLY", "guard": ["<", "@payload.status", 300] }
        ]));
        let steps = plan_program(&shadowed, &PlanOptions::default()).unwrap();
        let first = arm_step(&steps, 0, PayloadCase::Success);
        assert!(first.solved && first.arm_conflict.is_none());
        let second = arm_step(&steps, 1, PayloadCase::Success);
        assert!(!second.solved);
        assert_eq!(second.arm_conflict, Some(ArmConflict::EarlierArmFirst { arm: 0, to: "done".into() }));
    }

    #[test]
    fn control_disjoint_arms_each_take_their_own_payload() {
        let disjoint = arms_schema(serde_json::json!([
            { "from": "idle", "to": "done", "event": "REPLY", "guard": ["<", "@payload.status", 300] },
            { "from": "idle", "to": "failed", "event": "REPLY", "guard": [">=", "@payload.status", 300] }
        ]));
        let steps = plan_program(&disjoint, &PlanOptions::default()).unwrap();
        for arm in 0..2 {
            let step = arm_step(&steps, arm, PayloadCase::Success);
            assert!(step.solved && step.arm_conflict.is_none(), "arm {arm}");
        }
    }

    #[test]
    fn a_fail_payload_that_still_passes_its_own_guard_is_unsolved_and_a_catching_sibling_is_named() {
        let own = arms_schema(serde_json::json!([
            { "from": "idle", "to": "done", "event": "REPLY",
              "guard": ["or", ["<", "@payload.status", 5], [">", "@payload.status", 1]] }
        ]));
        let steps = plan_program(&own, &PlanOptions::default()).unwrap();
        let fail = arm_step(&steps, 0, PayloadCase::GuardFail);
        assert!(!fail.solved, "{fail:?}");
        assert_eq!(fail.arm_conflict, Some(ArmConflict::OwnGuardPasses));
        let siblings = arms_schema(serde_json::json!([
            { "from": "idle", "to": "done", "event": "REPLY", "guard": ["<", "@payload.status", 300] },
            { "from": "idle", "to": "done", "event": "REPLY", "guard": [">=", "@payload.status", 300] }
        ]));
        let steps = plan_program(&siblings, &PlanOptions::default()).unwrap();
        let fail = arm_step(&steps, 0, PayloadCase::GuardFail);
        assert!(fail.solved && fail.arm_conflict.is_none());
        assert_eq!(fail.guard_sibling_targets, vec!["done".to_string()], "the catching sibling is named even when it shares the target");
    }

    fn json_schema(transitions: serde_json::Value, events: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "parse",
            "orbitals": [{
                "name": "O",
                "entity": { "name": "Thing", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "types": {
                    "Item": [{ "name": "name", "type": "string" }],
                    "Pending": [
                        { "name": "key", "type": "string" }, { "name": "size", "type": "number" },
                        { "name": "isPublic", "type": "boolean" }, { "name": "mode", "type": "string" }
                    ],
                    "Batch": [{ "name": "items", "type": "[Item]" }, { "name": "idx", "type": "number" }]
                },
                "traits": [{ "name": "Saver", "linkedEntity": "Thing",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": events,
                        "transitions": transitions
                    } }]
            }]
        }))
        .unwrap()
    }

    fn string_events() -> serde_json::Value {
        serde_json::json!([
            { "key": "DONE", "name": "DONE", "payloadSchema": [
                { "name": "state", "type": "string", "required": true },
                { "name": "other", "type": "string", "required": true }] },
            { "key": "NEXT", "name": "NEXT", "payloadSchema": [{ "name": "batch", "type": "string", "required": true }] }
        ])
    }

    fn parsed_field(payload: &serde_json::Value, field: &str) -> serde_json::Value {
        serde_json::from_str(payload[field].as_str().unwrap_or_else(|| panic!("{field} is text: {payload}")))
            .unwrap_or_else(|e| panic!("{field} is JSON text: {e}: {payload}"))
    }

    #[test]
    fn a_payload_field_read_through_json_parse_is_the_json_text_of_a_minimal_instance() {
        let schema = json_schema(
            serde_json::json!([
                { "from": "idle", "to": "idle", "event": "INIT" },
                { "from": "idle", "to": "idle", "event": "DONE",
                  "effects": [["let", [["u", ["json/parse", "@payload.state", "Pending"]]], ["emit", "SAVED", { "key": "@u.key" }]]] }
            ]),
            string_events(),
        );
        let steps = plan_program(&schema, &PlanOptions::default()).unwrap();
        let payload = payload_of(&steps, "DONE", PayloadCase::Success);
        let instance = parsed_field(&payload, "state");
        assert!(instance["key"].as_str().is_some_and(|k| !k.is_empty()), "{instance}");
        assert!(instance["size"].is_number() && instance["isPublic"].is_boolean(), "{instance}");
        assert!(serde_json::from_str::<serde_json::Value>(payload["other"].as_str().unwrap()).is_err(), "control: a field not parsed stays plain text");
    }

    #[test]
    fn each_arm_json_member_is_solved_from_its_guard() {
        let mode = |value: &str| serde_json::json!(["=", ["object/get", ["json/parse", "@payload.state", "Pending"], "mode", "single"], value]);
        let schema = json_schema(
            serde_json::json!([
                { "from": "idle", "to": "idle", "event": "INIT" },
                { "from": "idle", "to": "idle", "event": "DONE", "guard": mode("single") },
                { "from": "idle", "to": "idle", "event": "DONE", "guard": mode("batch") }
            ]),
            string_events(),
        );
        let steps = plan_program(&schema, &PlanOptions::default()).unwrap();
        for (arm, expected) in [(0, "single"), (1, "batch")] {
            let step = steps
                .iter()
                .find(|s| s.event == "DONE" && s.arm == Some(arm) && s.payload_case == PayloadCase::Success)
                .unwrap();
            assert!(step.solved && step.arm_conflict.is_none(), "arm {arm}: {step:?}");
            assert_eq!(parsed_field(&step.payload, "state")["mode"], expected);
        }
    }

    #[test]
    fn two_json_members_are_solved_together_and_an_unsolvable_guard_stays_unsolved() {
        let items = serde_json::json!(["object/get", ["json/parse", "@payload.batch", "Batch"], "items", []]);
        let idx = serde_json::json!(["object/get", ["json/parse", "@payload.batch", "Batch"], "idx", 0]);
        let schema = json_schema(
            serde_json::json!([
                { "from": "idle", "to": "idle", "event": "INIT" },
                { "from": "idle", "to": "idle", "event": "NEXT", "guard": ["<", idx, ["array/len", items]] }
            ]),
            string_events(),
        );
        let steps = plan_program(&schema, &PlanOptions::default()).unwrap();
        let pass = steps.iter().find(|s| s.event == "NEXT" && s.guard_case == Some(GuardCase::Pass)).unwrap();
        let batch = parsed_field(&pass.payload, "batch");
        assert!(pass.solved, "{pass:?}");
        assert!(batch["items"].as_array().unwrap().len() as f64 > batch["idx"].as_f64().unwrap(), "{batch}");
        let never = json_schema(
            serde_json::json!([
                { "from": "idle", "to": "idle", "event": "INIT" },
                { "from": "idle", "to": "idle", "event": "NEXT",
                  "guard": ["and", ["=", ["object/get", ["json/parse", "@payload.batch", "Batch"], "idx", 0], 7],
                                   ["=", ["object/get", ["json/parse", "@payload.batch", "Batch"], "idx", 0], 9]] }
            ]),
            string_events(),
        );
        let steps = plan_program(&never, &PlanOptions::default()).unwrap();
        let pass = steps.iter().find(|s| s.event == "NEXT" && s.guard_case == Some(GuardCase::Pass)).unwrap();
        assert!(!pass.solved, "control: a contradictory guard has no payload");
    }

    #[test]
    fn a_guard_over_an_array_length_gets_rows_shaped_from_the_declared_entity() {
        let events = serde_json::json!([{ "key": "LOADED", "name": "LOADED", "payloadSchema": [{ "name": "data", "type": "[Thing]" }] }]);
        let schema = json_schema(
            serde_json::json!([
                { "from": "idle", "to": "idle", "event": "INIT" },
                { "from": "idle", "to": "idle", "event": "LOADED", "guard": [">", ["array/len", "@payload.data"], 0] }
            ]),
            events,
        );
        let steps = plan_program(&schema, &PlanOptions::default()).unwrap();
        let pass = payload_of(&steps, "LOADED", PayloadCase::Success);
        let rows = pass["data"].as_array().unwrap_or_else(|| panic!("data is an array: {pass}"));
        assert!(!rows.is_empty() && rows.iter().all(|r| r["id"].is_string()), "{pass}");
        assert_eq!(payload_of(&steps, "LOADED", PayloadCase::GuardFail)["data"], serde_json::json!([]));
    }

    fn sibling_schema(guard: serde_json::Value, setter_effects: serde_json::Value, extra_fields: serde_json::Value) -> OrbitalSchema {
        let mut fields = vec![
            serde_json::json!({ "name": "id", "type": "string" }),
            serde_json::json!({ "name": "activeId", "type": "string", "default": "" }),
        ];
        fields.extend(extra_fields.as_array().unwrap().clone());
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Thing", "persistence": "runtime", "fields": fields },
                "traits": [
                    { "name": "Composer", "linkedEntity": "Thing",
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "sent" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "sent", "event": "SEND", "guard": guard }
                        ] } },
                    { "name": "Picker", "linkedEntity": "Thing",
                      "stateMachine": {
                        "states": [{ "name": "waiting", "isInitial": true }, { "name": "picked" }],
                        "events": [{ "key": "SELECT", "name": "SELECT", "payloadSchema": [{ "name": "id", "type": "string" }] }],
                        "transitions": [
                            { "from": "waiting", "to": "waiting", "event": "INIT" },
                            { "from": "waiting", "to": "picked", "event": "SELECT", "effects": setter_effects }
                        ] } }
                ]
            }]
        }))
        .unwrap()
    }

    fn send_step(schema: &OrbitalSchema) -> StepWire {
        plan_program(schema, &PlanOptions::default())
            .unwrap()
            .into_iter()
            .find(|s| s.event == "SEND" && s.guard_case == Some(GuardCase::Pass))
            .unwrap()
    }

    #[test]
    fn a_guard_reading_an_entity_field_a_sibling_sets_plans_that_sibling_as_the_preamble() {
        let schema = sibling_schema(
            serde_json::json!("@entity.activeId"),
            serde_json::json!([["set", "@entity.activeId", "@payload.id"]]),
            serde_json::json!([]),
        );
        let step = send_step(&schema);
        match step.precondition {
            Some(Precondition::Established { trait_name, event, establish_at_state, proof, payload, .. }) => {
                assert_eq!((trait_name.as_str(), event.as_str(), establish_at_state.as_str()), ("Picker", "SELECT", "waiting"));
                assert_eq!(proof, planner::Proof::Proven);
                assert!(payload["id"].as_str().is_some_and(|id| !id.is_empty()), "{payload}");
            }
            other => panic!("expected an established preamble, got {other:?}"),
        }
    }

    #[test]
    fn control_a_setter_that_writes_an_unsatisfying_value_leaves_the_precondition_unreachable() {
        let schema = sibling_schema(
            serde_json::json!("@entity.activeId"),
            serde_json::json!([["set", "@entity.activeId", ""]]),
            serde_json::json!([]),
        );
        assert_eq!(
            send_step(&schema).precondition,
            Some(Precondition::Unreachable { fields: vec!["activeId".to_string()] })
        );
        let held = sibling_schema(
            serde_json::json!(["=", "@entity.activeId", ""]),
            serde_json::json!([["set", "@entity.activeId", "@payload.id"]]),
            serde_json::json!([]),
        );
        assert_eq!(send_step(&held).precondition, None, "the declared default already satisfies the guard");
    }

    #[test]
    fn a_setter_whose_value_cannot_be_evaluated_is_kept_and_typed_unproven() {
        let schema = sibling_schema(
            serde_json::json!("@entity.activeId"),
            serde_json::json!([["set", "@entity.activeId", ["array/len"]]]),
            serde_json::json!([]),
        );
        match send_step(&schema).precondition {
            Some(Precondition::Established { proof, .. }) => assert_eq!(
                proof,
                planner::Proof::Unproven { reason: planner::UnprovenReason::SetterValueUnresolved }
            ),
            other => panic!("expected an unproven preamble, got {other:?}"),
        }
    }

    #[test]
    fn a_second_unestablished_field_is_a_typed_marker_on_the_preamble() {
        let schema = sibling_schema(
            serde_json::json!(["and", "@entity.activeId", "@entity.mode"]),
            serde_json::json!([["set", "@entity.activeId", "@payload.id"]]),
            serde_json::json!([{ "name": "mode", "type": "string", "default": "" }]),
        );
        match send_step(&schema).precondition {
            Some(Precondition::Established { unplanned_fields, .. }) => assert_eq!(unplanned_fields, vec!["mode".to_string()]),
            other => panic!("expected an established preamble, got {other:?}"),
        }
    }

    #[test]
    fn the_judge_makes_a_failing_step_with_an_unproven_precondition_inconclusive() {
        let mut e = expect(&["b"], false, None, true);
        e.precondition_gap = Some(planner::UnprovenReason::Unreachable);
        assert_eq!(judge(&e, &obs("a", "a", None, vec![])).status, FrameStatus::Inconclusive);
        assert_eq!(judge(&e, &obs("a", "b", None, vec![])).status, FrameStatus::Pass);
        e.precondition_gap = None;
        assert_eq!(judge(&e, &obs("a", "a", None, vec![])).status, FrameStatus::Fail);
    }

    fn config_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Gauge", "linkedEntity": "Job",
                    "config": {
                        "format": { "type": "string", "default": "hearts", "values": ["hearts", "bar", "numeric"] },
                        "animated": { "type": "boolean", "default": true },
                        "current": { "type": "number", "default": 0 },
                        "className": { "type": "string", "default": "" }
                    },
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "TICK" }]
                    } }]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn config_variants_use_only_declared_values_and_report_the_knobs_with_none() {
        let sweep = plan_config_variants(&config_schema(), &PlanOptions::default());
        let [gauge] = &sweep[..] else { panic!("one trait: {sweep:?}") };
        assert_eq!(gauge.baseline, serde_json::json!({ "format": "hearts", "animated": true, "current": 0, "className": "" }));
        let labels: Vec<&str> = gauge.variants.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(labels, vec!["animated = false", "format = bar", "format = numeric"]);
        let bar = gauge.variants.iter().find(|v| v.label == "format = bar").unwrap();
        assert_eq!(bar.config, serde_json::json!({ "format": "bar", "animated": true, "current": 0, "className": "" }));
        assert_eq!(gauge.no_declared_values, vec!["className".to_string(), "current".to_string()]);
    }

    #[test]
    fn control_a_trait_without_config_plans_no_sweep_and_every_trait_is_credited_its_boot() {
        let plain = bare_guard_schema();
        assert!(plan_config_variants(&plain, &PlanOptions::default()).is_empty());
        let credits = plan_init_credits(&config_schema(), &PlanOptions::default());
        assert_eq!(
            credits,
            vec![InitCredit { trait_name: "Gauge".into(), state: "idle".into(), coverage_key: "Gauge:idle+INIT->idle".into() }],
            "credited even though the trait declares no INIT arm"
        );
    }

    fn credit_step(event: &str, arm: Option<usize>, reply: Option<ReplyRoute>, fired: Vec<FiredHop>) -> CreditStep {
        CreditStep { trait_name: "Caller".into(), from: "waiting".into(), event: event.into(), arm, reply, fired }
    }

    fn hop(event: &str, arm: Option<usize>) -> FiredHop {
        FiredHop { trait_name: "Caller".into(), from: "waiting".into(), event: event.into(), to: "failed".into(), arm }
    }

    #[test]
    fn a_failure_route_step_is_credited_from_the_hop_another_dispatch_fired() {
        let steps = vec![
            credit_step("CALL", None, None, vec![hop("CALL_FAILED", None)]),
            credit_step("CALL_FAILED", None, Some(ReplyRoute::Failure), Vec::new()),
        ];
        assert_eq!(
            credit_failure_routes(&steps),
            vec![Credit { index: 1, state_before: "waiting".into(), state_after: "failed".into() }]
        );
    }

    #[test]
    fn control_a_different_event_a_different_arm_or_a_success_route_is_not_credited() {
        let other_event = vec![
            credit_step("CALL", None, None, vec![hop("OTHER", None)]),
            credit_step("CALL_FAILED", None, Some(ReplyRoute::Failure), Vec::new()),
        ];
        assert!(credit_failure_routes(&other_event).is_empty());
        let other_arm = vec![
            credit_step("CALL", None, None, vec![hop("CALL_FAILED", Some(0))]),
            credit_step("CALL_FAILED", Some(1), Some(ReplyRoute::Failure), Vec::new()),
        ];
        assert!(credit_failure_routes(&other_arm).is_empty());
        let success_route = vec![
            credit_step("CALL", None, None, vec![hop("CALLED", None)]),
            credit_step("CALLED", None, Some(ReplyRoute::Other), Vec::new()),
        ];
        assert!(credit_failure_routes(&success_route).is_empty(), "only a failure route is credited");
        let arm_matches = vec![
            credit_step("CALL", None, None, vec![hop("CALL_FAILED", Some(1))]),
            credit_step("CALL_FAILED", Some(1), Some(ReplyRoute::Failure), Vec::new()),
        ];
        assert_eq!(credit_failure_routes(&arm_matches).len(), 1);
    }

    fn replay_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{ "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Flow", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "loading" }, { "name": "ready" }, { "name": "done" }],
                        "events": [{ "key": "FINISH", "name": "FINISH", "payloadSchema": [{ "name": "ok", "type": "boolean" }] }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "loading", "event": "GO", "effects": [["fetch", "Job", { "emit": { "success": "LOADED" } }]] },
                            { "from": "loading", "to": "ready", "event": "LOADED" },
                            { "from": "ready", "to": "done", "event": "FINISH", "guard": ["=", "@payload.ok", true] },
                            { "from": "done", "to": "idle", "event": "RESET" }
                        ] } }] }]
        }))
        .unwrap()
    }

    fn reset_preamble(schema: &OrbitalSchema) -> Vec<PreambleEvent> {
        plan_program(schema, &PlanOptions::default())
            .unwrap()
            .into_iter()
            .find(|s| s.event == "RESET" && s.payload_case == PayloadCase::Success)
            .and_then(|s| s.preamble)
            .expect("RESET is reachable")
    }

    #[test]
    fn a_replay_preamble_skips_effect_emitted_events_and_settles_through_their_closure() {
        let hops = reset_preamble(&replay_schema());
        let events: Vec<&str> = hops.iter().map(|h| h.event.as_str()).collect();
        assert_eq!(events, vec!["GO", "FINISH"], "LOADED is fired by the fetch, never dispatched");
        assert_eq!((hops[0].from.as_str(), hops[0].to.as_str()), ("idle", "loading"));
        assert_eq!(hops[0].accept_states, vec!["loading".to_string(), "ready".to_string()]);
        assert_eq!((hops[1].from.as_str(), hops[1].to.as_str()), ("ready", "done"), "the next hop stands where the closure settled");
        assert_eq!(hops[1].payload["ok"], true, "a guarded hop carries the payload its guard accepts");
        assert!(hops.iter().all(|h| !h.navigates));
    }

    #[test]
    fn control_a_replay_without_effect_emitted_events_takes_the_plain_shortest_path() {
        let mut program = serde_json::to_value(replay_schema()).unwrap();
        let transitions = program["orbitals"][0]["traits"][0]["stateMachine"]["transitions"].as_array_mut().unwrap();
        transitions[1]["effects"] = serde_json::json!([]);
        let plain: OrbitalSchema = serde_json::from_value(program).unwrap();
        let events: Vec<String> = reset_preamble(&plain).into_iter().map(|h| h.event).collect();
        assert_eq!(events, vec!["GO", "LOADED", "FINISH"]);
    }

    /// A trait from `(from, event, to, effect)` rows; an `emits` effect is a fetch whose outcome fires that event.
    fn machine(rows: &[(&str, &str, &str, Option<&str>)]) -> OrbitalSchema {
        let initial = rows[0].0;
        let mut states: Vec<&str> = Vec::new();
        for (from, _, to, _) in rows {
            for state in [*from, *to] {
                if !states.contains(&state) {
                    states.push(state);
                }
            }
        }
        let transitions: Vec<serde_json::Value> = rows
            .iter()
            .map(|(from, event, to, emits)| {
                let effects = match *emits {
                    Some("navigate") => serde_json::json!([["navigate", "/away"]]),
                    Some(event) => serde_json::json!([["fetch", "Job", { "emit": { "success": event } }]]),
                    None => serde_json::json!([]),
                };
                serde_json::json!({ "from": from, "to": to, "event": event, "effects": effects })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{ "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "X", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": states.iter().map(|s| serde_json::json!({ "name": s, "isInitial": *s == initial })).collect::<Vec<_>>(),
                        "transitions": transitions } }] }]
        }))
        .unwrap()
    }

    fn replay(rows: &[(&str, &str, &str, Option<&str>)], target: &str) -> Option<Vec<planner::ReplayStep>> {
        let program = machine(rows);
        let td = planner::inline_traits(&program).into_iter().next().unwrap();
        let owner_fields = Vec::new();
        let ctx = planner::EntityCtx { schema: &program, owner_fields: &owner_fields, viewer_id: None };
        planner::replay_steps(td, target, &ctx, None)
    }

    fn events(path: &Option<Vec<planner::ReplayStep>>) -> Vec<String> {
        path.as_ref().expect("reachable").iter().map(|h| h.event.clone()).collect()
    }

    #[test]
    fn a_replay_is_the_shortest_dispatchable_path_and_none_when_unreachable() {
        let rows = [("a", "GO", "b", None), ("b", "NEXT", "c", None), ("z", "LOST", "a", None)];
        assert_eq!(events(&replay(&rows, "a")), Vec::<String>::new(), "the initial state needs no hop");
        assert_eq!(events(&replay(&rows, "c")), vec!["GO", "NEXT"]);
        assert!(replay(&rows, "z").is_none(), "genuinely unreachable");
    }

    #[test]
    fn a_target_reachable_only_through_a_transient_closure_is_reached_by_the_hop_into_its_source() {
        let rows = [("idle", "GO", "loading", Some("LOADED")), ("loading", "LOADED", "reviewing", None)];
        let path = replay(&rows, "reviewing");
        assert_eq!(events(&path), vec!["GO"]);
        assert_eq!(path.unwrap()[0].accept_states, vec!["loading".to_string(), "reviewing".to_string()]);
    }

    #[test]
    fn a_target_in_the_initial_states_own_closure_needs_zero_hops() {
        let rows = [("idle", "INIT", "idle", Some("SCAN")), ("idle", "SCAN", "scanning", None)];
        assert_eq!(events(&replay(&rows, "scanning")), Vec::<String>::new());
    }

    #[test]
    fn a_transient_settle_chains_into_further_real_hops() {
        let one = [("idle", "GO", "loading", Some("LOADED")), ("loading", "LOADED", "browsing", None), ("browsing", "REPLY", "replying", None)];
        let path = replay(&one, "replying").unwrap();
        assert_eq!(path.iter().map(|h| (h.from.as_str(), h.event.as_str())).collect::<Vec<_>>(), vec![("idle", "GO"), ("browsing", "REPLY")]);
        let two = [
            ("idle", "GO", "loading", Some("LOADED")),
            ("loading", "LOADED", "browsing", None),
            ("browsing", "OPEN", "viewing", None),
            ("viewing", "EDIT", "editing", None),
        ];
        assert_eq!(events(&replay(&two, "editing")), vec!["GO", "OPEN", "EDIT"]);
    }

    #[test]
    fn a_replay_hop_over_a_navigating_edge_is_marked_navigating() {
        let rows = [("idle", "GO", "away", Some("navigate")), ("away", "BACK", "idle", None)];
        let path = replay(&rows, "away").unwrap();
        assert!(path[0].navigates);
        let plain = replay(&[("idle", "GO", "away", None)], "away").unwrap();
        assert!(!plain[0].navigates, "control: a hop with no navigate effect stays unmarked");
    }

    /// One-arm trait whose only guard is `guard`; the event declares the payload fields the guard reads.
    fn guard_schema(guard: serde_json::Value, fields: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{ "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "G", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "open" }],
                        "events": [{ "key": "GO", "name": "GO", "payloadSchema": fields }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "open", "event": "GO", "guard": guard }
                        ] } }] }]
        }))
        .unwrap()
    }

    /// The pass and fail payloads the kernel plans for `guard`, each checked against the real evaluator.
    fn steered(guard: serde_json::Value, fields: serde_json::Value) -> (Option<bool>, Option<bool>) {
        let schema = guard_schema(guard.clone(), fields);
        let steps = plan_program(&schema, &PlanOptions::default()).unwrap();
        let sexpr: orbital_core_base::schema::sexpr::SExpression = serde_json::from_value(guard).unwrap();
        let verdict = |case: PayloadCase| {
            let step = steps.iter().find(|s| s.event == "GO" && s.payload_case == case)?;
            if !step.solved {
                return None;
            }
            planner::guard_passes(&sexpr, &step.payload, "idle").ok()
        };
        (verdict(PayloadCase::Success), verdict(PayloadCase::GuardFail))
    }

    fn text(name: &str) -> serde_json::Value {
        serde_json::json!({ "name": name, "type": "string" })
    }

    #[test]
    fn the_guard_solver_steers_every_canonical_payload_guard_shape_and_the_evaluator_agrees() {
        let map = serde_json::json!({ "a": true, "b": false, "c": true });
        let cases: Vec<(&str, serde_json::Value, serde_json::Value)> = vec![
            ("str/default fallback", serde_json::json!(["!=", ["str/default", "@payload.entityId", ""], ""]), serde_json::json!([text("entityId")])),
            ("== alias", serde_json::json!(["==", "@payload.s", "x"]), serde_json::json!([text("s")])),
            ("object/has", serde_json::json!(["object/has", map, "@payload.k"]), serde_json::json!([text("k")])),
            ("object/get truthy", serde_json::json!(["object/get", map, "@payload.k"]), serde_json::json!([text("k")])),
            ("object/get =", serde_json::json!(["=", ["object/get", { "a": "x", "b": "y" }, "@payload.k"], "y"]), serde_json::json!([text("k")])),
            ("array/includes", serde_json::json!(["array/includes", ["x", "y"], "@payload.v"]), serde_json::json!([text("v")])),
            ("str/startsWith", serde_json::json!(["str/startsWith", "@payload.name", "pre"]), serde_json::json!([text("name")])),
            ("validate/url", serde_json::json!(["validate/url", "@payload.u"]), serde_json::json!([text("u")])),
            ("validate/required", serde_json::json!(["validate/required", "@payload.r"]), serde_json::json!([text("r")])),
            ("and intersects in the other order", serde_json::json!(["and", ["=", ["object/get", { "a": "x", "change": "y" }, "@payload.k"], "y"], ["object/has", { "a": 1, "change": 2 }, "@payload.k"]]), serde_json::json!([text("k")])),
            ("and intersects a shared field", serde_json::json!(["and", ["object/has", { "a": 1, "change": 2 }, "@payload.k"], ["=", ["object/get", { "a": "x", "change": "y" }, "@payload.k"], "y"]]), serde_json::json!([text("k")])),
        ];
        for (name, guard, fields) in cases {
            let (pass, fail) = steered(guard, fields);
            assert_eq!((pass, fail), (Some(true), Some(false)), "{name}");
        }
    }

    #[test]
    fn the_guard_solver_steers_quantifiers_conditionals_bindings_and_grid_bounds() {
        let items = serde_json::json!({ "name": "items", "type": "[object]" });
        let cases: Vec<(&str, serde_json::Value, serde_json::Value)> = vec![
            ("array/some", serde_json::json!(["array/some", "@payload.items", ["fn", "x", ["=", "@x.kind", "a"]]]), serde_json::json!([items.clone()])),
            ("array/every", serde_json::json!(["array/every", "@payload.items", ["fn", "x", ["=", "@x.kind", "a"]]]), serde_json::json!([items])),
            ("if", serde_json::json!(["if", ["=", "@payload.s", "x"], ["=", "@payload.t", 1], true]), serde_json::json!([text("s"), { "name": "t", "type": "number" }])),
            ("let", serde_json::json!(["let", [["v", "@payload.s"]], ["=", "@v", "x"]]), serde_json::json!([text("s")])),
            ("grid/in-bounds", serde_json::json!(["grid/in-bounds", "@payload.pos", 5, 5]), serde_json::json!([{ "name": "pos", "type": "object" }])),
        ];
        for (name, guard, fields) in cases {
            let (pass, fail) = steered(guard, fields);
            assert_eq!((pass, fail), (Some(true), Some(false)), "{name}");
        }
    }

    #[test]
    fn control_a_guard_over_no_payload_stays_unsolved_rather_than_claiming_a_payload() {
        let (pass, fail) = steered(serde_json::json!(["=", "@entity.mode", "on"]), serde_json::json!([]));
        assert_eq!((pass, fail), (None, None));
    }

    #[test]
    fn steps_are_planned_in_the_order_the_transitions_are_declared() {
        let schema = machine(&[("idle", "ZED", "b", None), ("idle", "ALPHA", "c", None), ("b", "MID", "idle", None)]);
        let events: Vec<String> = plan_program(&schema, &PlanOptions::default())
            .unwrap()
            .into_iter()
            .filter(|s| s.payload_case == PayloadCase::Success)
            .map(|s| s.event)
            .collect();
        assert_eq!(events, vec!["ZED", "ALPHA", "MID"], "not sorted by state and event name");
    }

    fn steerable(guard: serde_json::Value) -> Option<bool> {
        let schema = guard_schema(guard, serde_json::json!([text("s"), text("t")]));
        plan_program(&schema, &PlanOptions::default())
            .unwrap()
            .into_iter()
            .find(|s| s.event == "GO" && s.payload_case == PayloadCase::Success)
            .and_then(|s| s.guard_steerable)
    }

    #[test]
    fn a_guard_is_steerable_only_when_every_binding_it_reads_is_a_payload_path() {
        assert_eq!(steerable(serde_json::json!(["=", "@payload.s", "x"])), Some(true));
        assert_eq!(steerable(serde_json::json!(["=", "@entity.mode", "x"])), Some(false), "@entity side unreachable");
        assert_eq!(steerable(serde_json::json!(["=", "@config.mode", "x"])), Some(false));
        assert_eq!(steerable(serde_json::json!("@config.enabled")), Some(false), "a bare config atom");
        assert_eq!(steerable(serde_json::json!(["and", ["=", "@payload.s", "x"], ["=", "@entity.mode", "x"]])), Some(false));
        assert_eq!(steerable(serde_json::json!(["or", true, ["=", "@payload.s", "x"]])), Some(true), "control: a payload binding keeps it steerable");
        let unguarded = guard_schema(serde_json::json!(null), serde_json::json!([]));
        let step = plan_program(&unguarded, &PlanOptions::default()).unwrap().into_iter().find(|s| s.event == "GO").unwrap();
        assert_eq!(step.guard_steerable, None, "an unguarded step carries no steerability stamp");
    }
}
