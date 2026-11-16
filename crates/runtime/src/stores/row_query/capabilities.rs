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
    /// The most values one `member` leaf may carry; the kernel asks a larger conjunct member in chunks. Absent is no limit.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "positive_whole_count")]
    #[cfg_attr(test, ts(type = "number"))]
    pub member_limit: Option<usize>,
}

/// A count declared in `.lolo` config: the evaluator's numbers are floats, so a whole float is the count it names.
fn positive_whole_count<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<usize>, D::Error> {
    let Some(n) = Option::<f64>::deserialize(deserializer)? else {
        return Ok(None);
    };
    if n.fract() == 0.0 && n >= 1.0 && n <= u32::MAX as f64 {
        Ok(Some(n as usize))
    } else {
        Err(serde::de::Error::custom(format!("a count must be a whole number of at least 1, not {n}")))
    }
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
            member_limit: None,
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
    MemberLimit,
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
            Self::MemberLimit => "member-limit",
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
        Value::Number(_) | Value::Int(_) => Some(RowValueKind::Number),
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
            refuses(RowOperator::Contains, &[&Value::string(text.clone())])
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

/// A declined `query` reduced to its predicates, ordered by `id`: what a store that filters can answer when it
/// cannot order, window, rank or count as asked. The kernel orders, windows and ranks the rows it returns.
pub fn narrowed_query(query: &RowQuery) -> RowQuery {
    RowQuery {
        policy: query.policy.clone(),
        filter: query.filter.clone(),
        order: Some(RowOrder { field: "id".to_string(), direction: RowDirection::Asc }),
        offset: None,
        limit: None,
        total: Some(RowTotalMode::None),
        nearest: None,
    }
}

pub(super) fn widest_member(predicate: &RowPredicate) -> usize {
    match predicate {
        RowPredicate::Member { values, .. } => values.len(),
        RowPredicate::All { of } | RowPredicate::Any { of } => of.iter().map(widest_member).max().unwrap_or(0),
        RowPredicate::Not { of } => widest_member(of),
        _ => 0,
    }
}

/// The narrowing of `query` asked in parts when its one oversized member is the filter or a conjunct of a top-level
/// `all`: each part carries at most `limit` distinct values, so a row matches one part only. `None` when the member
/// sits anywhere else, more than one is oversized, or a part is still not runnable.
pub fn member_chunks(capabilities: &QueryCapabilities, query: &RowQuery) -> Option<Vec<RowQuery>> {
    let limit = capabilities.member_limit.filter(|l| *l > 0)?;
    let narrowed = narrowed_query(query);
    let filter = narrowed.filter.as_ref()?;
    let (field, values, rest): (&str, &[Value], Vec<RowPredicate>) = match filter {
        RowPredicate::Member { field, values } => (field, values, Vec::new()),
        RowPredicate::All { of } => {
            let mut wide = of.iter().enumerate().filter(|(_, p)| widest_member(p) > limit);
            let (at, RowPredicate::Member { field, values }) = wide.next()? else {
                return None;
            };
            if wide.next().is_some() {
                return None;
            }
            let rest = of.iter().enumerate().filter(|(i, _)| *i != at).map(|(_, p)| p.clone()).collect();
            (field, values, rest)
        }
        _ => return None,
    };
    let mut distinct: Vec<Value> = Vec::with_capacity(values.len());
    for value in values {
        if !distinct.iter().any(|seen| same_value(seen, value)) {
            distinct.push(value.clone());
        }
    }
    let parts: Vec<RowQuery> = distinct
        .chunks(limit)
        .map(|chunk| {
            let member = RowPredicate::Member { field: field.to_string(), values: chunk.to_vec() };
            let filter = if rest.is_empty() {
                member
            } else {
                let mut of = rest.clone();
                of.push(member);
                RowPredicate::All { of }
            };
            RowQuery { filter: Some(filter), ..narrowed.clone() }
        })
        .collect();
    parts.iter().all(|p| query_unsupported_reason(capabilities, p).is_none()).then_some(parts)
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
    if let Some(limit) = capabilities.member_limit {
        if used.iter().any(|p| widest_member(p) > limit) {
            return Some(QueryUnsupportedReason::MemberLimit);
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
