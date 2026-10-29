//! The working types of a persist run and a batch, and the row-lambda helpers a `where` reads through.

use super::*;

/// The rows one run of persists has written so far, by (entity type, id); `None` is a row the run deleted. A
/// `@derived` recompute reads these before the store, so the pushed path sees the run's own earlier writes.
pub(super) type RunRows = HashMap<(String, String), Option<Value>>;

/// Why a derived-field step stopped a write: the expression failed (`derived-failed`), or the store read it needed did.
pub(super) enum DerivedFault {
    Failed(String),
    Store(String),
}

/// The access directive, required columns and `@intrinsic` fields that govern one persist.
#[derive(Default)]
pub(super) struct PersistScope {
    pub(super) policy: Option<Value>,
    pub(super) required_fields: Vec<String>,
    pub(super) declared_required: Vec<String>,
    pub(super) intrinsic: Vec<String>,
    pub(super) derived: Vec<(String, orbital_core_base::schema::sexpr::SExpression)>,
}

/// One evaluated persist, ready to be sent to the store as a conditional write.
pub(super) struct PreparedWrite {
    pub(super) effect: Effect,
    pub(super) operation: PersistOperation,
    pub(super) entity_type: String,
    pub(super) id: Option<String>,
    pub(super) resolved_data: Value,
    pub(super) row_id: String,
    pub(super) op: RowWriteOp,
    pub(super) guard: Option<RowPredicate>,
}

/// How one evaluated persist reaches the store.
pub(super) enum PersistPrep {
    Push(Box<PreparedWrite>),
    Refused(Box<EffectResult>),
    Legacy,
}

/// One parsed `persist batch` op tuple.
pub(super) struct BatchOp {
    pub(super) action: String,
    pub(super) entity: String,
    pub(super) operation: PersistOperation,
    pub(super) row_id: Option<String>,
    pub(super) data: Value,
}

/// The `operations` entry a finished batch op contributes.
pub(super) fn batch_entry(b: &BatchOp, result: &EffectResult) -> Value {
    let mut entry: ValueMap = match (&result.return_value, b.operation) {
        (
            Some(Value::Object(row)),
            PersistOperation::Create | PersistOperation::Upsert | PersistOperation::Update,
        ) => row.clone().into_inner(),
        (_, PersistOperation::Create | PersistOperation::Upsert | PersistOperation::Update) => {
            b.data.as_object().cloned().unwrap_or_default()
        }
        _ => ValueMap::default(),
    };
    entry.insert("action".into(), Value::string(b.action.clone()));
    entry.insert("entityType".into(), Value::string(b.entity.clone()));
    if let Some(id) = &b.row_id {
        entry.insert("id".into(), Value::string(id.clone()));
    }
    if b.operation == PersistOperation::Delete {
        entry.insert("deleted".into(), Value::Bool(true));
    }
    Value::object(entry)
}

pub(super) fn batch_summary(effect: Effect, completed: Vec<Value>, total: usize) -> EffectResult {
    let mut summary = ValueMap::default();
    summary.insert(
        "completedCount".into(),
        Value::Number(completed.len() as f64),
    );
    summary.insert("totalCount".into(), Value::Number(total as f64));
    summary.insert("operations".into(), Value::array(completed));
    EffectResult::executed_with_value(effect, Value::object(summary))
}

/// Order rows by `order` (missing values last in both directions, ties by `id` ascending, as the compiled
/// path and `@almadar/db`), then cut the page with the same `> 0` offset/limit rule.
pub(crate) fn page_ordered_rows(
    mut rows: Vec<Value>,
    order: &RowOrder,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Vec<Value> {
    rows.sort_by(|a, b| compare_rows(a, b, order));
    let start = offset.unwrap_or(0).min(rows.len());
    let rows = rows.split_off(start);
    match limit {
        Some(n) => rows.into_iter().take(n).collect(),
        None => rows,
    }
}


/// `body` with the lambda's row parameter (`@row`, `@row.f`) read as the row (`@entity`, `@entity.f`).
pub(super) fn bind_row_param(body: &Value, param: &str) -> Value {
    match body {
        Value::String(s) if s == param => Value::string("@entity"),
        Value::String(s) => match s.strip_prefix(param).and_then(|rest| rest.strip_prefix('.')) {
            Some(path) => Value::string(format!("@entity.{path}")),
            None => body.clone(),
        },
        Value::Array(items) => Value::Array(items.iter().map(|item| bind_row_param(item, param)).collect()),
        other => other.clone(),
    }
}

pub(super) fn reads_entity(body: &Value) -> bool {
    match body {
        Value::String(s) => s == "@entity" || s.starts_with("@entity."),
        Value::Array(items) => items.iter().any(reads_entity),
        _ => false,
    }
}
