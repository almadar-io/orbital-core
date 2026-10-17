//! Expression evaluation as a host operation: the kernel's one evaluator (`orbital_core_eval`) and
//! row-access owner (`entity_access`) behind JSON strings, so a host renders with the kernel's semantics
//! and never carries a second evaluator. A render hands its binding context once and every expression it
//! binds in one `evaluate_many` crossing.

use crate::entity_access::{apply_read_access, check_mutation_access};
use crate::kernel::envelope::mapped_listen_payload;
use orbital_core_base::effect_types::PersistOperation;
use orbital_core_base::schema::sexpr::{SExpression, SObject};
use orbital_core_base::value::Value;
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
