//! Expression evaluation as a host operation: the kernel's one evaluator (`orbital_core_eval`) and
//! row-access owner (`entity_access`) behind JSON strings, so a host renders with the kernel's semantics
//! and never carries a second evaluator. A render hands its binding context once and every expression it
//! binds in one `evaluate_many` crossing.

use crate::entity_access::{apply_read_access, check_mutation_access};
use crate::kernel::envelope::mapped_listen_payload;
use orbital_core_base::effect_types::PersistOperation;
use orbital_core_base::schema::sexpr::{SExpression, SObject};
use orbital_core_base::value::{Value, ValueMap};
use orbital_core_eval::evaluator::trace::{evaluate_traced, PathSeg, TraceStep};
use orbital_core_eval::evaluator::{BindingContext, BindingContextBuilder, Evaluator};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The binding context an expression is evaluated against. Every root a host does not hand over is
/// unbound, as in a dispatch that never set it.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EvalContextWire {
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub entity: Option<Value>,
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub payload: Option<Value>,
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub config: Option<Value>,
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub user: Option<Value>,
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub callsite_payload: Option<Value>,
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub current_theme: Option<Value>,
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub pages: Option<Value>,
    pub state: Option<String>,
    /// The dispatch's `now` stamp (ms epoch); absent = the wall clock.
    pub now: Option<f64>,
    pub locale: Option<String>,
    /// The active locale's merged catalog (qualified key to message) for `i18n/t`.
    pub messages: Option<HashMap<String, String>>,
    /// Local bindings (`@name`), as a `let` would bind them.
    #[cfg_attr(test, ts(type = "JsonObject", optional))]
    pub locals: Option<serde_json::Map<String, serde_json::Value>>,
    /// Entity rows by type, for `count` queries over a store.
    #[cfg_attr(test, ts(type = "{ [key in string]: Array<JsonValue> }", optional))]
    pub entity_stores: Option<HashMap<String, Vec<Value>>>,
}

impl EvalContextWire {
    pub fn into_context(self) -> Result<BindingContext, String> {
        let mut builder = BindingContextBuilder::new();
        if let Some(entity) = self.entity {
            builder = builder.entity(entity);
        }
        if let Some(payload) = self.payload {
            builder = builder.payload(payload);
        }
        if let Some(config) = self.config {
            builder = builder.config(config);
        }
        if let Some(user) = self.user {
            builder = builder.user(user);
        }
        if let Some(theme) = self.current_theme {
            builder = builder.current_theme(theme);
        }
        if let Some(pages) = self.pages {
            builder = builder.pages(pages);
        }
        if let Some(state) = self.state {
            builder = builder.state(Value::String(state));
        }
        if let Some(now) = self.now {
            builder = builder.now(now);
        }
        if let Some(locale) = self.locale {
            builder = builder.locale(Value::String(locale));
        }
        if let Some(messages) = self.messages {
            builder = builder.messages(messages);
        }
        for (name, value) in self.locals.unwrap_or_default() {
            let value = serde_json::from_value::<Value>(value)
                .map_err(|e| format!("local `{name}`: {e}"))?;
            builder = builder.local(&name, value);
        }
        for (entity_type, rows) in self.entity_stores.unwrap_or_default() {
            builder = builder.entity_store(&entity_type, rows);
        }
        let mut ctx = builder.build();
        if let Some(callsite) = self.callsite_payload {
            ctx.set_callsite_payload(callsite);
        }
        Ok(ctx)
    }
}

/// One expression's outcome: its value, or the evaluator's message.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EvalOutcomeWire {
    Value {
        #[cfg_attr(test, ts(type = "JsonValue"))]
        value: Value,
    },
    Error {
        message: String,
    },
}

/// One guard's outcome: its truth, or the evaluator's message.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GuardOutcomeWire {
    Value { value: bool },
    Error { message: String },
}

/// One step of a traced evaluation (`EvalStep` in `@almadar/core`).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalStepWire {
    pub kind: EvalStepKind,
    #[cfg_attr(test, ts(type = "Array<number | string>"))]
    pub path: Vec<PathSegWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EvalStepKind {
    Enter,
    Exit,
    Iter,
    Skip,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PathSegWire {
    Index(usize),
    Key(String),
}

/// A traced evaluation: the outcome and every step up to it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalTracedWire {
    pub outcome: EvalOutcomeWire,
    pub trace: Vec<EvalStepWire>,
}

/// What a binding term folds to for a row predicate (`RowBindingResult` in `@almadar/core`).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RowBindingWire {
    Value {
        #[cfg_attr(test, ts(type = "string | number | boolean | null"))]
        value: Value,
    },
    List {
        #[cfg_attr(test, ts(type = "Array<string | number | boolean | null>"))]
        values: Vec<Value>,
    },
    Unspecialised {
        detail: String,
    },
    Inexpressible {
        detail: String,
    },
}

/// The mutation a row-access directive guards.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MutationOpWire {
    Create,
    Update,
    Delete,
}

/// Whether a mutation passed its access directive, and the denial when it did not.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationVerdictWire {
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn parse<T: serde::de::DeserializeOwned>(json: &str, what: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|e| format!("parse {what}: {e}"))
}

fn to_json<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("serialize: {e}"))
}

fn context_of(json: &str) -> Result<BindingContext, String> {
    parse::<EvalContextWire>(json, "context")?.into_context()
}

fn outcome(result: Result<Value, orbital_core_base::error::OrbitalError>) -> EvalOutcomeWire {
    match result {
        Ok(value) => EvalOutcomeWire::Value { value },
        Err(err) => EvalOutcomeWire::Error {
            message: err.to_string(),
        },
    }
}

/// Every expression of one render against one context: `EvalOutcomeWire[]`, in order. A failing
/// expression is its own outcome and never fails its siblings.
pub fn evaluate_many_json(context_json: &str, expressions_json: &str) -> Result<String, String> {
    let ctx = context_of(context_json)?;
    let expressions: Vec<SExpression> = parse(expressions_json, "expressions")?;
    let evaluator = Evaluator::new();
    let outcomes: Vec<EvalOutcomeWire> = expressions
        .iter()
        .map(|expr| outcome(evaluator.evaluate(expr, &ctx)))
        .collect();
    to_json(&outcomes)
}

/// Every value of one render resolved against one context: each `@`-binding in it, whole or embedded in a string at
/// any depth, and each operator call. `EvalOutcomeWire[]`, in order; a failing value never fails its siblings.
pub fn resolve_values_json(context_json: &str, values_json: &str) -> Result<String, String> {
    let ctx = context_of(context_json)?;
    let values: Vec<Value> = parse(values_json, "values")?;
    let outcomes: Vec<EvalOutcomeWire> = values
        .iter()
        .map(|value| match ctx.resolve_value_checked(value) {
            Ok(resolved) => EvalOutcomeWire::Value { value: resolved.into_owned() },
            Err(err) => EvalOutcomeWire::Error { message: err.to_string() },
        })
        .collect();
    to_json(&outcomes)
}

/// Every guard of one render against one context, in one crossing: `GuardOutcomeWire[]`, in order.
pub fn evaluate_guards_json(context_json: &str, guards_json: &str) -> Result<String, String> {
    let ctx = context_of(context_json)?;
    let guards: Vec<SExpression> = parse(guards_json, "guards")?;
    let evaluator = Evaluator::new();
    let verdicts: Vec<GuardOutcomeWire> = guards
        .iter()
        .map(|guard| match evaluator.evaluate_guard(guard, &ctx) {
            Ok(value) => GuardOutcomeWire::Value { value },
            Err(err) => GuardOutcomeWire::Error {
                message: err.to_string(),
            },
        })
        .collect();
    to_json(&verdicts)
}

fn seg(seg: &PathSeg) -> PathSegWire {
    match seg {
        PathSeg::Index(i) => PathSegWire::Index(*i),
        PathSeg::Key(k) => PathSegWire::Key(k.clone()),
    }
}

fn step(step: TraceStep) -> EvalStepWire {
    let wire = |kind, path: Vec<PathSeg>, value, error| EvalStepWire {
        kind,
        path: path.iter().map(seg).collect(),
        value,
        error,
    };
    match step {
        TraceStep::Enter { path } => wire(EvalStepKind::Enter, path, None, None),
        TraceStep::Iter { path } => wire(EvalStepKind::Iter, path, None, None),
        TraceStep::Skip { path } => wire(EvalStepKind::Skip, path, None, None),
        TraceStep::Exit {
            path,
            outcome: Ok(value),
        } => wire(EvalStepKind::Exit, path, Some(value), None),
        TraceStep::Exit {
            path,
            outcome: Err(message),
        } => wire(EvalStepKind::Exit, path, None, Some(message)),
    }
}

/// One expression against a context, with every node's entry, value and exit by structural path:
/// an `EvalTracedWire`.
pub fn evaluate_traced_json(context_json: &str, expression_json: &str) -> Result<String, String> {
    let ctx = context_of(context_json)?;
    let expr: SExpression = parse(expression_json, "expression")?;
    let (result, steps) = evaluate_traced(&expr, &ctx);
    to_json(&EvalTracedWire {
        outcome: outcome(result),
        trace: steps.into_iter().map(step).collect(),
    })
}

/// A listen route's `with { field: <expr> }` mapping applied to the source payload (a payload-only context,
/// as the kernel's listener fan-out builds it): the mapped payload as JSON. No mapping passes the payload through.
pub fn map_listen_payload_json(
    mapping_json: Option<&str>,
    payload_json: &str,
) -> Result<String, String> {
    let mapping: Option<SObject> = match mapping_json {
        Some(s) if !s.is_empty() => Some(parse(s, "mapping")?),
        _ => None,
    };
    let payload: Value = parse(payload_json, "payload")?;
    let ctx = BindingContextBuilder::new()
        .payload(payload.clone())
        .build();
    to_json(&mapped_listen_payload(
        &Evaluator::new(),
        &ctx,
        &mapping,
        &payload,
    ))
}

fn optional_value(json: Option<&str>, what: &str) -> Result<Option<Value>, String> {
    match json {
        Some(s) if !s.is_empty() => parse::<Option<Value>>(s, what),
        _ => Ok(None),
    }
}

/// The rows that pass the declared `@read` policy and the call-site `filter:` (`Value[]` as JSON), under the
/// bindings' viewer, payload, config and clock. A predicate that fails to evaluate drops its row.
pub fn scope_rows_json(
    bindings_json: &str,
    rows_json: &str,
    policy_json: Option<&str>,
    filter_json: Option<&str>,
    entity_type: &str,
) -> Result<String, String> {
    let ctx = context_of(bindings_json)?;
    let mut rows: Vec<Value> = parse(rows_json, "rows")?;
    let policy = optional_value(policy_json, "policy")?;
    let filter = optional_value(filter_json, "filter")?;
    apply_read_access(
        &mut rows,
        policy.as_ref(),
        filter.as_ref(),
        &ctx,
        entity_type,
    );
    to_json(&rows)
}

/// Whether `row` may undergo `operation` under the declared directive: a `MutationVerdictWire`. No directive allows.
pub fn check_mutation_json(
    operation: &str,
    bindings_json: &str,
    row_json: &str,
    policy_json: Option<&str>,
    entity_type: &str,
) -> Result<String, String> {
    let operation = match serde_json::from_value::<MutationOpWire>(serde_json::Value::String(
        operation.to_string(),
    ))
    .map_err(|e| format!("parse operation: {e}"))?
    {
        MutationOpWire::Create => PersistOperation::Create,
        MutationOpWire::Update => PersistOperation::Update,
        MutationOpWire::Delete => PersistOperation::Delete,
    };
    let ctx = context_of(bindings_json)?;
    let row: Value = parse(row_json, "row")?;
    let policy = optional_value(policy_json, "policy")?;
    let verdict = match check_mutation_access(operation, &row, policy.as_ref(), &ctx, entity_type) {
        Ok(()) => MutationVerdictWire {
            allowed: true,
            message: None,
        },
        Err(denied) => MutationVerdictWire {
            allowed: false,
            message: Some(denied.into_message()),
        },
    };
    to_json(&verdict)
}

fn is_row_binding_term(term: &str) -> bool {
    let root = term.strip_prefix('@');
    match root {
        Some("now") => true,
        Some(rest) => ["user", "payload", "config"].iter().any(|r| {
            rest == *r
                || rest
                    .strip_prefix(r)
                    .is_some_and(|tail| tail.starts_with('.'))
        }),
        None => false,
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

/// What `term` (`@user.*`, `@payload.*`, `@config.*`, `@now`) folds to under the bindings, for a row predicate
/// compiled against a store: a `RowBindingWire`. Any other term is not a binding and stays in the predicate.
pub fn resolve_row_binding_json(term: &str, bindings_json: &str) -> Result<String, String> {
    let ctx = context_of(bindings_json)?;
    let resolved = if !is_row_binding_term(term) {
        RowBindingWire::Inexpressible {
            detail: format!("binding {term}"),
        }
    } else {
        match ctx.resolve_binding(term) {
            None => RowBindingWire::Unspecialised {
                detail: format!("{term} resolves to nothing"),
            },
            Some(value) if is_scalar(&value) => RowBindingWire::Value { value },
            Some(Value::Array(items)) if items.iter().all(is_scalar) => {
                RowBindingWire::List { values: items }
            }
            Some(Value::Array(_)) => RowBindingWire::Inexpressible {
                detail: format!("{term} is not a list of scalars"),
            },
            Some(_) => RowBindingWire::Inexpressible {
                detail: format!("{term} is not a scalar"),
            },
        }
    };
    to_json(&resolved)
}

// ---------------------------------------------------------------------------------------------------------
// Running effects against a row: `traceEffects`
// ---------------------------------------------------------------------------------------------------------

/// What happened to one effect: it ran, it failed, or this operation has no handler for its kind.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectStatusWire {
    Executed,
    Failed,
    Skipped,
}

/// One effect of the list, as `EffectTrace` in `@almadar/core` reports it: its operator head, its arguments, its
/// outcome and the evaluation of the expressions it ran, node by node under the effect's own paths.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectTraceWire {
    #[serde(rename = "type")]
    #[cfg_attr(test, ts(rename = "type"))]
    pub kind: String,
    #[cfg_attr(test, ts(type = "Array<JsonValue>"))]
    pub args: Vec<SExpression>,
    pub status: EffectStatusWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_trace: Option<Vec<EvalStepWire>>,
}

/// An event an `emit` effect raised, with its evaluated payload.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmittedEffectWire {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub payload: Option<Value>,
}

/// A field a `set` effect wrote, in write order (a field set twice appears twice).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityWriteWire {
    pub field: String,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub value: Value,
}

/// The effects of one list run in order against one row: a trace per effect, the events raised, the writes made and
/// the row after them. A later effect reads the writes of the earlier ones.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEffectsWire {
    pub effects: Vec<EffectTraceWire>,
    pub emitted: Vec<EmittedEffectWire>,
    pub writes: Vec<EntityWriteWire>,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub entity: Value,
}

fn under(index: usize, steps: Vec<TraceStep>) -> Vec<TraceStep> {
    let lift = |path: Vec<PathSeg>| {
        let mut lifted = vec![PathSeg::Index(index)];
        lifted.extend(path);
        lifted
    };
    steps
        .into_iter()
        .map(|step| match step {
            TraceStep::Enter { path } => TraceStep::Enter { path: lift(path) },
            TraceStep::Iter { path } => TraceStep::Iter { path: lift(path) },
            TraceStep::Skip { path } => TraceStep::Skip { path: lift(path) },
            TraceStep::Exit { path, outcome } => TraceStep::Exit { path: lift(path), outcome },
        })
        .collect()
}

struct EffectRun {
    ctx: BindingContext,
    entity: ValueMap,
    emitted: Vec<EmittedEffectWire>,
    writes: Vec<EntityWriteWire>,
}

/// How one effect (or a control form's parts) ended.
enum Ended {
    Executed,
    Failed(String),
    Skipped(String),
}

impl EffectRun {
    fn mirror(&mut self) {
        self.ctx.set_entity(Value::Object(self.entity.clone()));
    }

    /// A leaf effect's value expression, traced under the effect's own paths: the root entered, the expression at
    /// `at`, every argument before it not run, the root exited.
    fn leaf(&mut self, expr: &SExpression, at: usize, skipped: &[usize], then: impl FnOnce(&mut Self, Value) -> Ended) -> (Ended, Vec<TraceStep>) {
        let mut steps = vec![TraceStep::Enter { path: Vec::new() }];
        let (outcome, inner) = evaluate_traced(expr, &self.ctx);
        steps.extend(under(at, inner));
        steps.extend(skipped.iter().map(|i| TraceStep::Skip { path: vec![PathSeg::Index(*i)] }));
        let ended = match outcome {
            Ok(value) => then(self, value),
            Err(error) => Ended::Failed(error.to_string()),
        };
        let exit = match &ended {
            Ended::Executed => Ok(Value::Null),
            Ended::Failed(message) | Ended::Skipped(message) => Err(message.clone()),
        };
        steps.push(TraceStep::Exit { path: Vec::new(), outcome: exit });
        (ended, steps)
    }

    fn run(&mut self, effect: &SExpression) -> (Ended, Vec<TraceStep>) {
        let SExpression::Array(items) = effect else {
            return (Ended::Skipped("not an effect: an effect is a list headed by its operator".to_string()), Vec::new());
        };
        let Some(SExpression::String(head)) = items.first() else {
            return (Ended::Skipped("not an effect: an effect is a list headed by its operator".to_string()), Vec::new());
        };
        let rest = &items[1..];
        match head.as_str() {
            "set" if rest.len() == 2 => {
                let SExpression::String(target) = &rest[0] else {
                    return (Ended::Failed("set: the target is an @entity binding".to_string()), Vec::new());
                };
                let Some(field) = target.strip_prefix("@entity.") else {
                    return (Ended::Failed(format!("set only supports @entity bindings, got: {target}")), Vec::new());
                };
                let field = field.to_string();
                self.leaf(&rest[1], 2, &[1], move |run, value| {
                    run.entity.insert(field.clone(), value.clone());
                    run.writes.push(EntityWriteWire { field, value });
                    run.mirror();
                    Ended::Executed
                })
            }
            "emit" if !rest.is_empty() && rest.len() <= 2 => {
                let SExpression::String(event) = &rest[0] else {
                    return (Ended::Failed("emit: the event is a name".to_string()), Vec::new());
                };
                let event = event.clone();
                match rest.get(1) {
                    Some(payload) => self.leaf(payload, 2, &[1], move |run, value| {
                        run.emitted.push(EmittedEffectWire { event, payload: Some(value) });
                        Ended::Executed
                    }),
                    None => {
                        self.emitted.push(EmittedEffectWire { event, payload: None });
                        (
                            Ended::Executed,
                            vec![
                                TraceStep::Enter { path: Vec::new() },
                                TraceStep::Skip { path: vec![PathSeg::Index(1)] },
                                TraceStep::Exit { path: Vec::new(), outcome: Ok(Value::Null) },
                            ],
                        )
                    }
                }
            }
            "do" => (self.sequence(rest), Vec::new()),
            "when" if !rest.is_empty() => match Evaluator::new().evaluate(&rest[0], &self.ctx) {
                Ok(condition) if condition.is_truthy() => (self.sequence(&rest[1..]), Vec::new()),
                Ok(_) => (Ended::Executed, Vec::new()),
                Err(error) => (Ended::Failed(error.to_string()), Vec::new()),
            },
            "if" if rest.len() >= 2 => match Evaluator::new().evaluate(&rest[0], &self.ctx) {
                Ok(condition) => {
                    let branch = if condition.is_truthy() { rest.get(1) } else { rest.get(2) };
                    (branch.map_or(Ended::Executed, |b| self.run(b).0), Vec::new())
                }
                Err(error) => (Ended::Failed(error.to_string()), Vec::new()),
            },
            "let" if rest.len() >= 2 => (self.scoped(rest), Vec::new()),
            other => (Ended::Skipped(format!("`{other}` has no handler in traceEffects; only set and emit, and the do/when/if/let forms around them, run")), Vec::new()),
        }
    }

    fn sequence(&mut self, effects: &[SExpression]) -> Ended {
        let mut ran = false;
        let mut skipped = None;
        for effect in effects {
            match self.run(effect).0 {
                Ended::Failed(message) => return Ended::Failed(message),
                Ended::Skipped(message) => skipped = Some(message),
                Ended::Executed => ran = true,
            }
        }
        match (ran, skipped) {
            (false, Some(message)) => Ended::Skipped(message),
            _ => Ended::Executed,
        }
    }

    fn scoped(&mut self, rest: &[SExpression]) -> Ended {
        let outer = self.ctx.clone();
        let mut inner = outer.child();
        if let SExpression::Array(bindings) = &rest[0] {
            for binding in bindings {
                if let SExpression::Array(pair) = binding {
                    if let (Some(SExpression::String(name)), Some(value)) = (pair.first(), pair.get(1)) {
                        match Evaluator::new().evaluate(value, &inner) {
                            Ok(value) => {
                                inner.set_local(name, value);
                            }
                            Err(error) => return Ended::Failed(error.to_string()),
                        }
                    }
                }
            }
        }
        self.ctx = inner;
        let ended = self.sequence(&rest[1..]);
        let entity = self.entity.clone();
        self.ctx = outer;
        self.entity = entity;
        self.mirror();
        ended
    }
}

/// Run `effects` in order against the row in the context's `entity`, with the context's other bindings: the
/// `TraceEffectsWire` as JSON. `set` writes the row (under the flat key the kernel writes) and `emit` raises an event,
/// each with the evaluation of its value expression traced under the effect's own paths; `do`, `when`, `if` and `let`
/// run their parts. Every other effect is reported `skipped`: this operation has no host to run it.
pub fn trace_effects_json(context_json: &str, effects_json: &str) -> Result<String, String> {
    let ctx = context_of(context_json)?;
    let effects: Vec<SExpression> = parse(effects_json, "effects")?;
    let entity = match &*ctx.entity {
        Value::Object(map) => map.clone(),
        _ => ValueMap::new(),
    };
    let mut run = EffectRun { ctx, entity, emitted: Vec::new(), writes: Vec::new() };
    run.mirror();
    let mut traced = Vec::with_capacity(effects.len());
    for effect in &effects {
        let (ended, steps) = run.run(effect);
        let (status, error) = match ended {
            Ended::Executed => (EffectStatusWire::Executed, None),
            Ended::Failed(message) => (EffectStatusWire::Failed, Some(message)),
            Ended::Skipped(message) => (EffectStatusWire::Skipped, Some(message)),
        };
        let (kind, args) = match effect {
            SExpression::Array(items) => (
                match items.first() {
                    Some(SExpression::String(head)) => head.clone(),
                    _ => "unknown".to_string(),
                },
                items.iter().skip(1).cloned().collect(),
            ),
            _ => ("unknown".to_string(), Vec::new()),
        };
        let trace: Vec<EvalStepWire> = steps
            .into_iter()
            .map(step)
            .map(|mut wire| {
                if matches!(wire.kind, EvalStepKind::Exit) && wire.path.is_empty() {
                    wire.value = None;
                }
                wire
            })
            .collect();
        traced.push(EffectTraceWire { kind, args, status, error, eval_trace: (!trace.is_empty()).then_some(trace) });
    }
    to_json(&TraceEffectsWire { effects: traced, emitted: run.emitted, writes: run.writes, entity: Value::Object(run.entity) })
}

#[cfg(test)]
mod trace_effects_tests {
    use super::trace_effects_json;
    use serde_json::{json, Value as Json};

    /// Whole floats compare as integers: the wire text `5.0` and `5` are one number.
    fn norm(value: Json) -> Json {
        match value {
            Json::Number(n) => match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => json!(f as i64),
                _ => Json::Number(n),
            },
            Json::Array(items) => Json::Array(items.into_iter().map(norm).collect()),
            Json::Object(map) => Json::Object(map.into_iter().map(|(k, v)| (k, norm(v))).collect()),
            other => other,
        }
    }

    macro_rules! same {
        ($left:expr, $right:expr) => {
            assert_eq!(norm($left.clone()), norm($right))
        };
    }

    fn trace(entity: Json, effects: Json) -> Json {
        let context = json!({ "entity": entity, "state": "idle" });
        serde_json::from_str(&trace_effects_json(&context.to_string(), &effects.to_string()).unwrap()).unwrap()
    }

    #[test]
    fn a_set_traces_its_value_under_the_effects_own_paths_exactly_as_the_js_interpreter_did() {
        let out = trace(json!({ "x": 1 }), json!([["set", "@entity.x", ["+", "@entity.x", 2]]]));
        same!(
            out["effects"][0]["evalTrace"],
            json!([
                { "kind": "enter", "path": [] },
                { "kind": "enter", "path": [2] },
                { "kind": "enter", "path": [2, 1] },
                { "kind": "exit", "path": [2, 1], "value": 1 },
                { "kind": "enter", "path": [2, 2] },
                { "kind": "exit", "path": [2, 2], "value": 2 },
                { "kind": "exit", "path": [2], "value": 3 },
                { "kind": "skip", "path": [1] },
                { "kind": "exit", "path": [] }
            ])
        );
        same!(out["effects"][0]["type"], json!("set"));
        same!(out["effects"][0]["status"], json!("executed"));
        same!(out["entity"], json!({ "x": 3 }));
    }

    #[test]
    fn a_later_effect_reads_the_writes_of_the_earlier_ones_and_writes_keep_their_order() {
        let out = trace(
            json!({ "x": 1 }),
            json!([["set", "@entity.x", ["+", "@entity.x", 2]], ["set", "@entity.y", ["*", "@entity.x", 10]], ["set", "@entity.x", 0]]),
        );
        same!(out["entity"], json!({ "x": 0, "y": 30 }));
        same!(out["writes"], json!([{ "field": "x", "value": 3 }, { "field": "y", "value": 30 }, { "field": "x", "value": 0 }]));
    }

    #[test]
    fn an_emit_reports_its_evaluated_payload_and_a_bare_emit_reports_none() {
        let out = trace(json!({}), json!([["emit", "PING", { "a": ["+", 1, 1] }], ["emit", "BARE"]]));
        same!(out["emitted"], json!([{ "event": "PING", "payload": { "a": 2 } }, { "event": "BARE" }]));
        same!(out["effects"][1]["evalTrace"], json!([{ "kind": "enter", "path": [] }, { "kind": "skip", "path": [1] }, { "kind": "exit", "path": [] }]));
    }

    #[test]
    fn a_failing_effect_is_failed_with_its_message_leaves_the_row_alone_and_does_not_stop_the_list() {
        let out = trace(json!({ "x": 1 }), json!([["set", "@entity.y", ["/", 1, 0]], ["set", "@entity.x", 5]]));
        same!(out["effects"][0]["status"], json!("failed"));
        assert!(out["effects"][0]["error"].as_str().unwrap().contains("Division by zero"));
        same!(out["effects"][1]["status"], json!("executed"));
        same!(out["entity"], json!({ "x": 5 }));
    }

    #[test]
    fn an_effect_with_no_handler_is_skipped_and_named_not_reported_executed() {
        let out = trace(json!({}), json!([["render-ui", "main", { "type": "x" }], ["persist", "update", "Job", {}]]));
        for index in 0..2 {
            same!(out["effects"][index]["status"], json!("skipped"));
            assert!(out["effects"][index]["error"].as_str().unwrap().contains("no handler in traceEffects"));
        }
        same!(out["effects"][0]["type"], json!("render-ui"));
        same!(out["effects"][0]["args"], json!(["main", { "type": "x" }]));
    }

    #[test]
    fn do_when_if_and_let_run_their_parts_against_the_same_row() {
        let out = trace(
            json!({ "n": 1 }),
            json!([
                ["when", ["=", "@entity.n", 1], ["set", "@entity.a", 1], ["emit", "A"]],
                ["when", ["=", "@entity.n", 2], ["set", "@entity.skipped", 1]],
                ["if", ["=", "@entity.a", 1], ["set", "@entity.b", 2], ["set", "@entity.c", 3]],
                ["let", [["k", 10]], ["do", ["set", "@entity.d", "@k"]]]
            ]),
        );
        same!(out["entity"], json!({ "n": 1, "a": 1, "b": 2, "d": 10 }));
        same!(out["emitted"], json!([{ "event": "A" }]));
        assert!(out["effects"].as_array().unwrap().iter().all(|e| e["status"] == "executed"));
    }

    #[test]
    fn control_a_bad_target_fails_the_effect_and_a_malformed_wire_is_an_error() {
        let out = trace(json!({}), json!([["set", "@payload.x", 1]]));
        same!(out["effects"][0]["status"], json!("failed"));
        assert!(out["effects"][0]["error"].as_str().unwrap().contains("only supports @entity"));
        assert!(trace_effects_json("{}", "not json").unwrap_err().contains("parse effects"));
        same!(trace(Json::Null, json!([]))["entity"], json!({}));
    }
}

#[cfg(test)]
mod resolve_values_tests {
    use super::resolve_values_json;
    use serde_json::{json, Value as Json};

    fn resolve(context: Json, values: Json) -> Json {
        serde_json::from_str(&resolve_values_json(&context.to_string(), &values.to_string()).unwrap()).unwrap()
    }

    #[test]
    fn interpolates_an_embedded_binding_and_resolves_a_whole_one_to_its_typed_value() {
        let out = resolve(json!({ "entity": { "hp": 7, "maxHp": 10 } }), json!(["HP: @entity.hp / @entity.maxHp", "@entity.hp"]));
        assert_eq!(out[0], json!({ "kind": "value", "value": "HP: 7 / 10" }));
        assert_eq!(out[1]["value"].as_f64(), Some(7.0));
    }

    #[test]
    fn resolves_bindings_at_any_depth_of_a_prop_tree_and_evaluates_an_operator_call() {
        let out = resolve(
            json!({ "entity": { "name": "Ada", "n": 2 } }),
            json!([{ "type": "typography", "content": "Hi @entity.name", "items": [["+", "@entity.n", 1]] }]),
        );
        assert_eq!(out[0]["value"]["content"], json!("Hi Ada"));
        assert_eq!(out[0]["value"]["items"][0].as_f64(), Some(3.0));
    }

    #[test]
    fn control_a_binding_free_value_comes_back_unchanged() {
        let out = resolve(json!({ "entity": {} }), json!([{ "type": "typography", "content": "plain" }, 3]));
        assert_eq!(out[0]["value"], json!({ "type": "typography", "content": "plain" }));
        assert_eq!(out[1]["value"].as_f64(), Some(3.0));
    }

    #[test]
    fn edge_a_failing_operator_call_is_that_values_error_and_never_fails_its_siblings() {
        let out = resolve(json!({ "entity": {} }), json!([["i18n/t", "greet:hello"], "ok"]));
        assert_eq!(out[0]["kind"], json!("error"));
        assert_eq!(out[1], json!({ "kind": "value", "value": "ok" }));
    }
}
