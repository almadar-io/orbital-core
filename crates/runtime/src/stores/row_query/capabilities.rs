//! What a store declares it can answer, and why a query or guard is unsupported.

use super::*;

// ----- capabilities ---------------------------------------------------------------------------

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowLeafKind {
    Compare,
    Member,
    HasElement,
    Contains,
    IsNull,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowCombinator {
    All,
    Any,
    Not,
}

/// Every operator a `RowPredicate` leaf can carry.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowOperator {
    #[serde(rename = "==")]
    #[cfg_attr(test, ts(rename = "=="))]
    Eq,
    #[serde(rename = "!=")]
    #[cfg_attr(test, ts(rename = "!="))]
    Ne,
    #[serde(rename = "<")]
    #[cfg_attr(test, ts(rename = "<"))]
    Lt,
    #[serde(rename = "<=")]
    #[cfg_attr(test, ts(rename = "<="))]
    Le,
    #[serde(rename = ">")]
    #[cfg_attr(test, ts(rename = ">"))]
    Gt,
    #[serde(rename = ">=")]
    #[cfg_attr(test, ts(rename = ">="))]
    Ge,
    #[serde(rename = "member")]
    #[cfg_attr(test, ts(rename = "member"))]
    Member,
    #[serde(rename = "hasElement")]
    #[cfg_attr(test, ts(rename = "hasElement"))]
    HasElement,
    #[serde(rename = "contains")]
    #[cfg_attr(test, ts(rename = "contains"))]
    Contains,
}

impl From<RowCompareOp> for RowOperator {
    fn from(op: RowCompareOp) -> Self {
        match op {
            RowCompareOp::Eq => Self::Eq,
            RowCompareOp::Ne => Self::Ne,
            RowCompareOp::Lt => Self::Lt,
            RowCompareOp::Le => Self::Le,
            RowCompareOp::Gt => Self::Gt,
            RowCompareOp::Ge => Self::Ge,
        }
    }
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowValueKind {
    String,
    Number,
    Boolean,
    Date,
    Null,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorValueKinds {
    pub operator: RowOperator,
    pub value_kinds: Vec<RowValueKind>,
}

#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RowOrderableFields {
    EveryField,
    Listed { fields: Vec<String> },
}

/// What a host's query push-down honours for one entity type.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryCapabilities {
    pub leaves: Vec<RowLeafKind>,
    pub combinators: Vec<RowCombinator>,
    pub compare_ops: Vec<RowCompareOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_value_kinds: Option<Vec<OperatorValueKinds>>,
    pub orderable_fields: RowOrderableFields,
    pub window: bool,
    pub exact_total: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids_only: Option<bool>,
    /// `nearest` is pushed down: the store ranks by cosine similarity itself (EXEC-FETCH-2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest: Option<bool>,
    /// How the driver realizes the entity's declared `@index` paths; absent is `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexes: Option<IndexRealization>,
}

/// When a store driver creates the access paths an entity declares (EXEC-STORE-6).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexRealization {
    /// The kernel sends `ROWS_ENSURE_INDEXES` and the driver creates the paths while the app runs.
    Runtime,
    /// The kernel sends `ROWS_ENSURE_INDEXES`; the driver records the paths for a deploy step.
    Deploy,
    None,
}

impl IndexRealization {
    /// The driver wants the declared paths sent.
    pub fn wants_paths(self) -> bool {
        matches!(self, Self::Runtime | Self::Deploy)
    }
}

impl QueryCapabilities {
    /// A store that runs every query form.
    pub fn full() -> Self {
        Self {
            leaves: vec![
                RowLeafKind::Compare,
                RowLeafKind::Member,
                RowLeafKind::HasElement,
                RowLeafKind::Contains,
                RowLeafKind::IsNull,
            ],
            combinators: vec![RowCombinator::All, RowCombinator::Any, RowCombinator::Not],
            compare_ops: vec![
                RowCompareOp::Eq,
                RowCompareOp::Ne,
                RowCompareOp::Lt,
                RowCompareOp::Le,
                RowCompareOp::Gt,
                RowCompareOp::Ge,
            ],
            operator_value_kinds: None,
            orderable_fields: RowOrderableFields::EveryField,
            window: true,
            exact_total: true,
            ids_only: Some(true),
            nearest: Some(true),
            indexes: None,
        }
    }
}

/// The guard predicates a store can test: a subset of the query capabilities.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardCapabilities {
    pub leaves: Vec<RowLeafKind>,
    pub combinators: Vec<RowCombinator>,
    pub compare_ops: Vec<RowCompareOp>,
}

/// What a host's `writeWhere` / `writeBatch` honours for one entity type.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteCapabilities {
    pub guards: GuardCapabilities,
    pub conditional: bool,
    pub returns_row: bool,
    pub stop_runs: bool,
    pub atomic_batch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rekey: Option<bool>,
    /// The host commits rows the kernel decided, compare-and-commit in one transaction (EXEC-STORE-5); it then
    /// holds no write semantics and the kernel runs every write form itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_rows: Option<bool>,
}

impl WriteCapabilities {
    /// The existing write forms, without opting into optional capabilities.
    pub fn full() -> Self {
        let q = QueryCapabilities::full();
        Self {
            guards: GuardCapabilities {
                leaves: q.leaves,
                combinators: q.combinators,
                compare_ops: q.compare_ops,
            },
            conditional: true,
            returns_row: true,
            stop_runs: true,
            atomic_batch: true,
            rekey: None,
            commit_rows: None,
        }
    }
}

/// Why a query is not pushed down; the `reason` of `kernel.fetch.pushdown-fallback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryUnsupportedReason {
    LeafUnsupported,
    CombinatorUnsupported,
    ValueKindUnsupported,
    OrderField,
    Window,
    ExactTotal,
    Nearest,
}

impl QueryUnsupportedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeafUnsupported => "leaf-unsupported",
            Self::CombinatorUnsupported => "combinator-unsupported",
            Self::ValueKindUnsupported => "value-kind-unsupported",
            Self::OrderField => "order-field",
            Self::Window => "window",
            Self::ExactTotal => "exact-total",
            Self::Nearest => "nearest",
        }
    }
}

pub(super) fn leaf_kind_of(predicate: &RowPredicate) -> Option<RowLeafKind> {
    match predicate {
        RowPredicate::Compare { .. } => Some(RowLeafKind::Compare),
        RowPredicate::Member { .. } => Some(RowLeafKind::Member),
        RowPredicate::HasElement { .. } => Some(RowLeafKind::HasElement),
        RowPredicate::Contains { .. } => Some(RowLeafKind::Contains),
        RowPredicate::IsNull { .. } => Some(RowLeafKind::IsNull),
        _ => None,
    }
}

/// A leaf-or-combinator check of `predicate` against a guard's capabilities.
pub(super) fn guard_unsupported(
    leaves: &[RowLeafKind],
    combinators: &[RowCombinator],
    compare_ops: &[RowCompareOp],
    predicate: &RowPredicate,
) -> Option<QueryUnsupportedReason> {
    match predicate {
        RowPredicate::Const { .. } => None,
        RowPredicate::All { of } | RowPredicate::Any { of } => {
            let which = if matches!(predicate, RowPredicate::All { .. }) {
                RowCombinator::All
            } else {
                RowCombinator::Any
            };
            if !combinators.contains(&which) {
                return Some(QueryUnsupportedReason::CombinatorUnsupported);
            }
            of.iter()
                .find_map(|p| guard_unsupported(leaves, combinators, compare_ops, p))
        }
        RowPredicate::Not { of } => {
            if !combinators.contains(&RowCombinator::Not) {
                return Some(QueryUnsupportedReason::CombinatorUnsupported);
            }
            guard_unsupported(leaves, combinators, compare_ops, of)
        }
        RowPredicate::Compare { op, .. } => {
            if leaves.contains(&RowLeafKind::Compare) && compare_ops.contains(op) {
                None
            } else {
                Some(QueryUnsupportedReason::LeafUnsupported)
            }
        }
        other => match leaf_kind_of(other) {
            Some(kind) if leaves.contains(&kind) => None,
            _ => Some(QueryUnsupportedReason::LeafUnsupported),
        },
    }
}

pub(super) fn value_kind_of(value: &Value) -> Option<RowValueKind> {
    match value {
        Value::Null => Some(RowValueKind::Null),
        Value::String(_) => Some(RowValueKind::String),
        Value::Number(_) => Some(RowValueKind::Number),
        Value::Bool(_) => Some(RowValueKind::Boolean),
        _ => None,
    }
}

pub(super) fn value_kind_unsupported(restrictions: &[OperatorValueKinds], predicate: &RowPredicate) -> bool {
    let refuses = |operator: RowOperator, values: &[&Value]| -> bool {
        let Some(found) = restrictions.iter().find(|r| r.operator == operator) else {
            return false;
        };
        values
            .iter()
            .any(|v| value_kind_of(v).is_some_and(|kind| !found.value_kinds.contains(&kind)))
    };
    match predicate {
        RowPredicate::Const { .. } | RowPredicate::IsNull { .. } => false,
        RowPredicate::Compare { op, value, .. } => refuses((*op).into(), &[value]),
        RowPredicate::Member { values, .. } => {
            refuses(RowOperator::Member, &values.iter().collect::<Vec<_>>())
        }
        RowPredicate::HasElement { value, .. } => refuses(RowOperator::HasElement, &[value]),
        RowPredicate::Contains { text, .. } => {
            refuses(RowOperator::Contains, &[&Value::String(text.clone())])
        }
        RowPredicate::All { of } | RowPredicate::Any { of } => {
            of.iter().any(|p| value_kind_unsupported(restrictions, p))
        }
        RowPredicate::Not { of } => value_kind_unsupported(restrictions, of),
    }
}

pub(super) fn leaf_failure(
    leaves: &[RowLeafKind],
    combinators: &[RowCombinator],
    compare_ops: &[RowCompareOp],
    predicate: &RowPredicate,
) -> bool {
    match predicate {
        RowPredicate::All { of } | RowPredicate::Any { of } => of
            .iter()
            .any(|c| leaf_failure(leaves, combinators, compare_ops, c)),
        RowPredicate::Not { of } => leaf_failure(leaves, combinators, compare_ops, of),
        other => guard_unsupported(leaves, combinators, compare_ops, other).is_some(),
    }
}

/// Why a host with `capabilities` cannot run `query` as a push-down, or `None` when it can. The first
/// failing check wins: leaves, combinators, value kinds, order field (`createdAt` when none is authored), window,
/// exact total.
pub fn query_unsupported_reason(
    capabilities: &QueryCapabilities,
    query: &RowQuery,
) -> Option<QueryUnsupportedReason> {
    let used: Vec<&RowPredicate> = query.policy.iter().chain(query.filter.iter()).collect();
    let (leaves, combinators, ops) = (
        capabilities.leaves.as_slice(),
        capabilities.combinators.as_slice(),
        capabilities.compare_ops.as_slice(),
    );
    if used
        .iter()
        .any(|p| leaf_failure(leaves, combinators, ops, p))
    {
        return Some(QueryUnsupportedReason::LeafUnsupported);
    }
    if let Some(reason) = used
        .iter()
        .find_map(|p| guard_unsupported(leaves, combinators, ops, p))
    {
        return Some(reason);
    }
    if let Some(restrictions) = &capabilities.operator_value_kinds {
        if used.iter().any(|p| value_kind_unsupported(restrictions, p)) {
            return Some(QueryUnsupportedReason::ValueKindUnsupported);
        }
    }
    if query.nearest.is_some() {
        return (capabilities.nearest != Some(true)).then_some(QueryUnsupportedReason::Nearest);
    }
    if let RowOrderableFields::Listed { fields } = &capabilities.orderable_fields {
        if !fields.contains(&effective_row_order(query).field) {
            return Some(QueryUnsupportedReason::OrderField);
        }
    }
    if (query.offset.is_some() || query.limit.is_some()) && !capabilities.window {
        return Some(QueryUnsupportedReason::Window);
    }
    if !capabilities.exact_total && query.total != Some(RowTotalMode::None) {
        return Some(QueryUnsupportedReason::ExactTotal);
    }
    None
}

/// Why a guard cannot be pushed to a host with `capabilities`, or `None` when it can.
pub fn guard_unsupported_reason(
    capabilities: &WriteCapabilities,
    guard: &RowPredicate,
) -> Option<QueryUnsupportedReason> {
    let g = &capabilities.guards;
    if leaf_failure(&g.leaves, &g.combinators, &g.compare_ops, guard) {
        return Some(QueryUnsupportedReason::LeafUnsupported);
    }
    guard_unsupported(&g.leaves, &g.combinators, &g.compare_ops, guard)
}
