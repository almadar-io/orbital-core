//! Conditional writes: ops, outcomes, batches and commits applied to rows in memory.

use super::*;

// ----- writes ---------------------------------------------------------------------------------

/// One write. `create` fails (`duplicate-id`) when its id is taken. `upsert` creates the row or replaces it whole
/// by id, keeping the existing `createdAt`. `update` changes an existing row only (never an upsert); `guard` is
/// tested against the pre-image.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RowWriteOp {
    Create {
        entity_type: String,
        #[cfg_attr(test, ts(type = "EntityRow"))]
        data: Value,
    },
    Upsert {
        entity_type: String,
        #[cfg_attr(test, ts(type = "EntityRow"))]
        data: Value,
    },
    Update {
        entity_type: String,
        id: String,
        #[cfg_attr(test, ts(type = "EntityRow"))]
        data: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guard: Option<RowPredicate>,
    },
    Rekey {
        entity_type: String,
        id: String,
        new_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guard: Option<RowPredicate>,
    },
    Delete {
        entity_type: String,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guard: Option<RowPredicate>,
    },
}

impl RowWriteOp {
    pub fn entity_type(&self) -> &str {
        match self {
            Self::Create { entity_type, .. }
            | Self::Upsert { entity_type, .. }
            | Self::Update { entity_type, .. }
            | Self::Delete { entity_type, .. }
            | Self::Rekey { entity_type, .. } => entity_type,
        }
    }

    pub fn guard(&self) -> Option<&RowPredicate> {
        match self {
            Self::Create { .. } | Self::Upsert { .. } => None,
            Self::Update { guard, .. } | Self::Delete { guard, .. } | Self::Rekey { guard, .. } => {
                guard.as_ref()
            }
        }
    }
}

/// What one write did. `before` is present exactly when the op carried a guard; `rolled-back` only
/// inside an atomic batch that failed.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RowWriteOutcome {
    Created {
        #[cfg_attr(test, ts(type = "EntityRow"))]
        row: Value,
    },
    Updated {
        #[cfg_attr(test, ts(type = "EntityRow"))]
        row: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(type = "EntityRow"))]
        before: Option<Value>,
    },
    Deleted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(type = "EntityRow"))]
        before: Option<Value>,
    },
    NotFound,
    DuplicateId,
    Refused,
    Failed {
        message: String,
    },
    RolledBack,
}

impl RowWriteOutcome {
    /// The write took effect.
    pub fn applied(&self) -> bool {
        matches!(
            self,
            Self::Created { .. } | Self::Updated { .. } | Self::Deleted { .. }
        )
    }
}

/// `atomic`: all ops apply or none do. `stop`: ops apply in order and stop at the first failure.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowWriteFailureMode {
    Atomic,
    Stop,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowWriteBatch {
    pub ops: Vec<RowWriteOp>,
    pub on_failure: RowWriteFailureMode,
}

/// How a `create` that names no id is handled; a create on a taken id fails (`duplicate-id`) under both.
pub enum CreateIds<'a> {
    /// The reference semantics: a create needs a string id.
    Strict,
    /// A store that mints an id for a create that names none (the in-memory cache).
    Mint(&'a mut dyn FnMut() -> String),
}

/// A table of rows by id, in insertion order.
pub type RowTable = IndexMap<String, Arc<Value>>;

/// Apply one write to `table`.
pub fn apply_write(
    table: &mut RowTable,
    op: &RowWriteOp,
    ids: &mut CreateIds<'_>,
) -> RowWriteOutcome {
    match op {
        RowWriteOp::Create { data, .. } => {
            let given = data
                .as_object()
                .and_then(|o| o.get("id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let id = match (given, &mut *ids) {
                (Some(id), _) => {
                    if table.contains_key(&id) {
                        return RowWriteOutcome::DuplicateId;
                    }
                    id
                }
                (None, CreateIds::Strict) => {
                    return RowWriteOutcome::Failed {
                        message: "create needs a string id".to_string(),
                    }
                }
                (None, CreateIds::Mint(mint)) => mint(),
            };
            let mut fields = match data {
                Value::Object(o) => o.clone(),
                _ => orbital_core_base::value::ValueMap::default().into(),
            };
            fields.insert("id".to_string(), Value::string(id.clone()));
            let row = Value::Object(fields);
            table.insert(id, Arc::new(row.clone()));
            RowWriteOutcome::Created { row }
        }
        RowWriteOp::Upsert { data, .. } => {
            let Some(id) = data
                .as_object()
                .and_then(|o| o.get("id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
            else {
                return RowWriteOutcome::Failed {
                    message: "upsert needs a string id".to_string(),
                };
            };
            let mut fields = match data {
                Value::Object(o) => o.clone(),
                _ => orbital_core_base::value::ValueMap::default().into(),
            };
            fields.insert("id".to_string(), Value::string(id.clone()));
            let kept = table.get(&id).map(|existing| {
                existing
                    .as_object()
                    .and_then(|o| o.get("createdAt"))
                    .cloned()
            });
            if let Some(Some(created_at)) = &kept {
                fields.insert("createdAt".to_string(), created_at.clone());
            }
            let row = Value::Object(fields);
            table.insert(id, Arc::new(row.clone()));
            match kept {
                Some(_) => RowWriteOutcome::Updated {
                    row,
                    before: None,
                },
                None => RowWriteOutcome::Created { row },
            }
        }
        RowWriteOp::Update {
            id, data, guard, ..
        } => {
            let Some(before) = table.get(id).cloned() else {
                return RowWriteOutcome::NotFound;
            };
            if guard.as_ref().is_some_and(|g| !g.matches(&before)) {
                return RowWriteOutcome::Refused;
            }
            let mut fields = match &*before {
                Value::Object(o) => o.clone(),
                _ => orbital_core_base::value::ValueMap::default().into(),
            };
            if let Value::Object(incoming) = data {
                for (k, v) in incoming {
                    fields.insert(k.clone(), v.clone());
                }
            }
            fields.insert("id".to_string(), Value::string(id.clone()));
            let row = Value::Object(fields);
            table.insert(id.clone(), Arc::new(row.clone()));
            RowWriteOutcome::Updated {
                row,
                before: guard.as_ref().map(|_| (*before).clone()),
            }
        }
        RowWriteOp::Rekey {
            id, new_id, guard, ..
        } => {
            if new_id.is_empty() {
                return RowWriteOutcome::Failed {
                    message: "rekey needs a nonempty string destination id".to_string(),
                };
            }
            let Some(before) = table.get(id).cloned() else {
                return RowWriteOutcome::NotFound;
            };
            if guard.as_ref().is_some_and(|g| !g.matches(&before)) {
                return RowWriteOutcome::Refused;
            }
            if id != new_id {
                let Value::Object(mut moved) = (*before).clone() else {
                    return RowWriteOutcome::Failed {
                        message: "rekey needs an object row".to_string(),
                    };
                };
                moved.insert("id".to_string(), Value::string(new_id.clone()));
                let moved = Arc::new(Value::Object(moved));
                let original = std::mem::take(table);
                for (key, row) in original {
                    if key == *id {
                        table.insert(new_id.clone(), moved.clone());
                    } else {
                        table.insert(key, row);
                    }
                }
            }
            let row = table
                .get(new_id)
                .expect("the source row mapped to the destination");
            RowWriteOutcome::Updated {
                row: (**row).clone(),
                before: guard.as_ref().map(|_| (*before).clone()),
            }
        }
        RowWriteOp::Delete { id, guard, .. } => {
            let Some(before) = table.get(id).cloned() else {
                return RowWriteOutcome::NotFound;
            };
            if guard.as_ref().is_some_and(|g| !g.matches(&before)) {
                return RowWriteOutcome::Refused;
            }
            table.shift_remove(id);
            RowWriteOutcome::Deleted {
                before: guard.as_ref().map(|_| (*before).clone()),
            }
        }
    }
}

/// What a compare-and-commit does to one row the kernel read (EXEC-STORE-5).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RowCommitWrite {
    /// Left as it is; it is still checked against the version read.
    Keep,
    Put {
        #[cfg_attr(test, ts(type = "EntityRow"))]
        row: Value,
    },
    Delete,
}

/// One row of a compare-and-commit: the version the kernel read (`null` when absent) and what it writes.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowCommit {
    pub entity_type: String,
    pub id: String,
    #[cfg_attr(test, ts(type = "EntityRow | null"))]
    pub expected: Option<Value>,
    pub write: RowCommitWrite,
}

/// The ids of rows `op` reads or writes.
pub fn touched_ids(op: &RowWriteOp) -> Vec<String> {
    let given = |data: &Value| data.as_object().and_then(|o| o.get("id")).and_then(|v| v.as_str()).map(str::to_string);
    match op {
        RowWriteOp::Create { data, .. } | RowWriteOp::Upsert { data, .. } => given(data).into_iter().collect(),
        RowWriteOp::Update { id, .. } | RowWriteOp::Delete { id, .. } => vec![id.clone()],
        RowWriteOp::Rekey { id, new_id, .. } => vec![id.clone(), new_id.clone()],
    }
}

/// [`apply_write_batch`] over one table per entity type: ops in order, each seeing the earlier ones; `atomic`
/// restores every table on a failure.
pub fn apply_write_batch_across(
    tables: &mut HashMap<String, RowTable>,
    batch: &RowWriteBatch,
    ids: &mut dyn FnMut(&str) -> CreateIds<'static>,
) -> Vec<RowWriteOutcome> {
    let snapshot = (batch.on_failure == RowWriteFailureMode::Atomic && batch.ops.len() > 1)
        .then(|| tables.clone());
    let mut outcomes: Vec<RowWriteOutcome> = Vec::with_capacity(batch.ops.len());
    for op in &batch.ops {
        let table = tables.entry(op.entity_type().to_string()).or_default();
        let outcome = apply_write(table, op, &mut ids(op.entity_type()));
        let applied = outcome.applied();
        outcomes.push(outcome);
        if !applied {
            if batch.on_failure == RowWriteFailureMode::Atomic {
                if let Some(snapshot) = snapshot {
                    *tables = snapshot;
                }
                let failing = outcomes.pop().unwrap_or(RowWriteOutcome::RolledBack);
                let mut undone: Vec<RowWriteOutcome> = outcomes.iter().map(|_| RowWriteOutcome::RolledBack).collect();
                undone.push(failing);
                return undone;
            }
            break;
        }
    }
    outcomes
}

/// Apply a batch: ops in order, each seeing the earlier ones. `Stop` ends at the first failure and keeps what
/// was applied (later ops have no outcome); `Atomic` undoes everything on a failure and reports `rolled-back` for
/// each earlier op, then the failing outcome.
pub fn apply_write_batch(
    table: &mut RowTable,
    batch: &RowWriteBatch,
    ids: &mut CreateIds<'_>,
) -> Vec<RowWriteOutcome> {
    let snapshot = (batch.on_failure == RowWriteFailureMode::Atomic && batch.ops.len() > 1)
        .then(|| table.clone());
    let mut outcomes: Vec<RowWriteOutcome> = Vec::with_capacity(batch.ops.len());
    for op in &batch.ops {
        let outcome = apply_write(table, op, ids);
        let applied = outcome.applied();
        outcomes.push(outcome);
        if !applied {
            if batch.on_failure == RowWriteFailureMode::Atomic {
                if let Some(snapshot) = snapshot {
                    *table = snapshot;
                }
                let failing = outcomes.pop().unwrap_or(RowWriteOutcome::RolledBack);
                let mut undone: Vec<RowWriteOutcome> = outcomes
                    .iter()
                    .map(|_| RowWriteOutcome::RolledBack)
                    .collect();
                undone.push(failing);
                return undone;
            }
            break;
        }
    }
    outcomes
}
