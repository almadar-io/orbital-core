//! The one Rust definition of the row-query contract: `RowPredicate` / `RowOrder` / `RowQuery`,
//! the conditional writes, the capability shapes and the run of each over rows held in memory.
//!
//! Mirrors `@almadar/core` `service-types.ts` and `row-query.ts`; the serde forms are the TS JSON
//! forms, so `packages/almadar-parity/fixtures/pushdown/*.json` load unchanged. Every evaluation
//! site (the in-kernel evaluator behind [`EntitySource::query_page`](super::EntitySource), the
//! in-memory cache's writes, the differential tests) goes through here.
//!
//! Two-valued null logic: a missing or null field satisfies only `!=` against a non-null value and
//! `isNull: true`. Strings compare by code point (UTF-8 byte order). A comparison relates only two
//! values of one kind. An array or object is never equal to anything.

use orbital_core_eval::evaluator::context::BindingContext;
use orbital_core_base::value::Value;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::sync::Arc;

/// What a host is asked for: whole rows, or only the ids of the window.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowProjection {
    Rows,
    Ids,
}

/// The answer to a `RowQuery`: the window, and `total` = every match before it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowPage {
    #[cfg_attr(test, ts(type = "Array<EntityRow>"))]
    pub rows: Vec<Value>,
    pub total: usize,
}

/// The body of `POST /api/orbitals/resolve-rows`: the reference a server resolves for the requesting viewer. The
/// answer is a [`RowPage`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveRowsRequest {
    #[serde(rename = "ref")]
    pub reference: RowSetRef,
}

/// The answer to a ids-only query.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowIdPage {
    pub ids: Vec<String>,
    pub total: usize,
}

/// The reference a [`row_set_marker`] leaf carries; `None` for any other value (core's `isRowSetMarker`).
pub fn as_row_set_marker(value: &Value) -> Option<RowSetRef> {
    let leaf = value.as_object()?;
    if leaf.get(ROW_SET_MARKER) != Some(&Value::Bool(true)) {
        return None;
    }
    serde_json::from_value(orbital_core_base::json_value::value_to_json(leaf.get("ref")?)).ok()
}

/// Every reference a resolved value carries, in document order.
pub fn row_sets_in(value: &Value) -> Vec<RowSetRef> {
    if let Some(reference) = as_row_set_marker(value) {
        return vec![reference];
    }
    match value {
        Value::Array(items) => items.iter().flat_map(row_sets_in).collect(),
        Value::Object(map) => map.values().flat_map(row_sets_in).collect(),
        _ => Vec::new(),
    }
}

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
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteCapabilities {
    pub guards: GuardCapabilities,
    pub conditional: bool,
    pub returns_row: bool,
    pub stop_runs: bool,
    pub atomic_batch: bool,
}

impl WriteCapabilities {
    /// A store that runs every write form, transactions included.
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
        }
    }
}

fn leaf_kind_of(predicate: &RowPredicate) -> Option<RowLeafKind> {
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
fn guard_unsupported(
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

fn value_kind_of(value: &Value) -> Option<RowValueKind> {
    match value {
        Value::Null => Some(RowValueKind::Null),
        Value::String(_) => Some(RowValueKind::String),
        Value::Number(_) => Some(RowValueKind::Number),
        Value::Bool(_) => Some(RowValueKind::Boolean),
        _ => None,
    }
}

fn value_kind_unsupported(restrictions: &[OperatorValueKinds], predicate: &RowPredicate) -> bool {
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

fn leaf_failure(
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

// ----- ordering -------------------------------------------------------------------------------

/// Rank of a present value's kind, so a column of mixed kinds still has a total order.
fn kind_rank(value: &Value) -> u8 {
    match value {
        Value::Bool(_) => 0,
        Value::Number(_) => 1,
        Value::String(_) => 2,
        _ => 3,
    }
}

fn total_order_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.total_cmp(y),
        _ => order_values(a, b).unwrap_or_else(|| kind_rank(a).cmp(&kind_rank(b))),
    }
}

/// Order of two rows by one field: missing and null last in both directions, no tie-break.
fn compare_by_field(a: &Value, b: &Value, field: &str, direction: RowDirection) -> Ordering {
    let (x, y) = (row_field(a, field), row_field(b, field));
    match (is_missing(x), is_missing(y)) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            let c = total_order_values(x.unwrap_or(&Value::Null), y.unwrap_or(&Value::Null));
            match direction {
                RowDirection::Asc => c,
                RowDirection::Desc => c.reverse(),
            }
        }
    }
}

/// Rows by `order`, ties by `id` ascending: the shared comparator of every ordered page.
pub fn compare_rows(a: &Value, b: &Value, order: &RowOrder) -> Ordering {
    compare_by_field(a, b, &order.field, order.direction)
        .then_with(|| compare_by_field(a, b, "id", RowDirection::Asc))
}

/// The field a query with no authored `order` sorts by (EXEC-FETCH-1).
pub const DEFAULT_ORDER_FIELD: &str = "createdAt";

/// The order of a query with no authored `order`: `createdAt` ascending, a row without one last, ties by `id`
/// ascending. Every store and both execution paths answer an unordered query this way.
pub fn default_row_order() -> RowOrder {
    RowOrder {
        field: DEFAULT_ORDER_FIELD.to_string(),
        direction: RowDirection::Asc,
    }
}

/// The order a store must apply to `query`: the authored one, else [`default_row_order`].
pub fn effective_row_order(query: &RowQuery) -> RowOrder {
    query.order.clone().unwrap_or_else(default_row_order)
}

/// The row's `id` as text; empty when it has none.
pub fn row_id(row: &Value) -> String {
    match row_field(row, "id") {
        Some(Value::String(s)) => s.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

// ----- running a query ------------------------------------------------------------------------

/// What a scan visitor tells its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanControl {
    Continue,
    Stop,
}

/// One run of a [`RowQuery`] over rows offered one at a time. Keeps the pointers of the matches awaiting selection
/// (every query is ordered, by default `createdAt` then `id`) and counts them for the total.
pub struct QueryRun<'q> {
    query: &'q RowQuery,
    matched: Vec<Arc<Value>>,
    total: usize,
}

impl<'q> QueryRun<'q> {
    pub fn new(query: &'q RowQuery) -> Self {
        Self {
            query,
            matched: Vec::new(),
            total: 0,
        }
    }

    /// Offer one row.
    pub fn visit(&mut self, row: &Arc<Value>) -> ScanControl {
        let hit = self.query.policy.as_ref().is_none_or(|p| p.matches(row))
            && self.query.filter.as_ref().is_none_or(|p| p.matches(row));
        if !hit {
            return ScanControl::Continue;
        }
        self.total += 1;
        self.matched.push(Arc::clone(row));
        ScanControl::Continue
    }

    /// The window's pointers in order, and the total.
    fn window(mut self) -> (Vec<Arc<Value>>, usize) {
        let offset = self.query.offset.unwrap_or(0);
        let order = effective_row_order(self.query);
        let cmp = |a: &Arc<Value>, b: &Arc<Value>| compare_rows(a, b, &order);
        let end = self.query.limit.map(|l| offset.saturating_add(l));
        if let Some(end) = end.filter(|e| *e < self.matched.len()) {
            if end == 0 {
                self.matched.clear();
            } else {
                self.matched.select_nth_unstable_by(end - 1, cmp);
                self.matched.truncate(end);
            }
        }
        self.matched.sort_by(cmp);
        let start = offset.min(self.matched.len());
        let mut window = self.matched.split_off(start);
        if let Some(limit) = self.query.limit {
            window.truncate(limit);
        }
        let total = if self.query.total == Some(RowTotalMode::None) {
            window.len()
        } else {
            self.total
        };
        (window, total)
    }

    /// The page: only the window's rows are cloned.
    pub fn finish(self) -> RowPage {
        let (window, total) = self.window();
        RowPage {
            rows: window.iter().map(|r| (**r).clone()).collect(),
            total,
        }
    }

    /// The ids of the page, no row cloned.
    pub fn finish_ids(self) -> RowIdPage {
        let (window, total) = self.window();
        RowIdPage {
            ids: window.iter().map(|r| row_id(r)).collect(),
            total,
        }
    }
}

/// The reference run of a query over `rows`.
pub fn run_query<'a>(rows: impl IntoIterator<Item = &'a Arc<Value>>, query: &RowQuery) -> RowPage {
    let mut run = QueryRun::new(query);
    for row in rows {
        if run.visit(row) == ScanControl::Stop {
            break;
        }
    }
    run.finish()
}

/// The reference run of an ids-only query over `rows`: the same window as [`run_query`].
pub fn run_ids<'a>(rows: impl IntoIterator<Item = &'a Arc<Value>>, query: &RowQuery) -> RowIdPage {
    let mut run = QueryRun::new(query);
    for row in rows {
        if run.visit(row) == ScanControl::Stop {
            break;
        }
    }
    run.finish_ids()
}

/// `a AND b`, either side optional.
pub fn and_predicates(a: Option<RowPredicate>, b: Option<RowPredicate>) -> Option<RowPredicate> {
    match (a, b) {
        (Some(a), Some(b)) => Some(RowPredicate::All { of: vec![a, b] }),
        (a, b) => a.or(b),
    }
}

/// A row set by reference resolved against the current rows: the ids of `reference.ids` that still match
/// the reference's query (and `policy`, the current viewer's `@read`), in the reference's order; `total` is a
/// true re-count of those matches, ignoring order and window.
pub fn resolve_row_set<'a>(
    rows: impl IntoIterator<Item = &'a Arc<Value>>,
    reference: &RowSetRef,
    policy: Option<&RowPredicate>,
) -> RowPage {
    let combined = and_predicates(reference.query.policy.clone(), policy.cloned());
    let query = RowQuery {
        policy: combined,
        filter: reference.query.filter.clone(),
        ..RowQuery::default()
    };
    let matching = run_query(rows, &query);
    let by_id: std::collections::HashMap<String, &Value> =
        matching.rows.iter().map(|r| (row_id(r), r)).collect();
    let resolved = reference
        .ids
        .iter()
        .filter_map(|id| by_id.get(id).map(|r| (*r).clone()))
        .collect();
    RowPage {
        rows: resolved,
        total: matching.total,
    }
}

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
            | Self::Delete { entity_type, .. } => entity_type,
        }
    }

    pub fn guard(&self) -> Option<&RowPredicate> {
        match self {
            Self::Create { .. } | Self::Upsert { .. } => None,
            Self::Update { guard, .. } | Self::Delete { guard, .. } => guard.as_ref(),
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
                _ => orbital_core_base::value::ValueMap::new(),
            };
            fields.insert("id".to_string(), Value::String(id.clone()));
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
                _ => orbital_core_base::value::ValueMap::new(),
            };
            fields.insert("id".to_string(), Value::String(id.clone()));
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
                _ => orbital_core_base::value::ValueMap::new(),
            };
            if let Value::Object(incoming) = data {
                for (k, v) in incoming {
                    fields.insert(k.clone(), v.clone());
                }
            }
            fields.insert("id".to_string(), Value::String(id.clone()));
            let row = Value::Object(fields);
            table.insert(id.clone(), Arc::new(row.clone()));
            RowWriteOutcome::Updated {
                row,
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

// ----- compiling a filter or an `@read` policy -----------------------------------------------

/// Why an S-expression is not a [`RowPredicate`]: the kernel runs it through the interpreter instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotExpressible {
    /// An operator with no predicate form (a function call on a field, `array/some` with a lambda, …).
    Operator(String),
    /// A form of an expressible operator the compiler does not map (two fields compared, a row path, …).
    Shape(String),
    /// A binding that does not resolve for this dispatch (a filter only; a policy compiles to `const false`).
    Binding(String),
    /// The evaluator and a predicate read a missing or null value differently here (an ordering comparison
    /// on a field the entity does not declare required or default, with no presence guard in an enclosing `and`):
    /// the interpreter keeps the result.
    NullSemantics(String),
}

impl NotExpressible {
    /// The `reason` of `kernel.fetch.pushdown-fallback`.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NullSemantics(_) => "null-semantics",
            _ => "not-expressible",
        }
    }
}

impl std::fmt::Display for NotExpressible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operator(op) => write!(f, "operator `{op}` has no predicate form"),
            Self::Shape(what) => write!(f, "{what}"),
            Self::Binding(binding) => write!(f, "binding `{binding}` does not resolve"),
            Self::NullSemantics(field) => write!(
                f,
                "an ordering comparison on `{field}`, which is not declared required and no enclosing `and` guards"
            ),
        }
    }
}

/// Which predicate is being compiled: a policy that cannot be specialised hides every row, a filter is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateRole {
    Policy,
    Filter,
}

enum Fail {
    Not(NotExpressible),
    Unresolved(String),
}

enum Term {
    Field(String),
    Const(Value),
}

fn shape(what: &str) -> Fail {
    Fail::Not(NotExpressible::Shape(what.to_string()))
}

fn row_path_free(field: &str) -> Result<String, Fail> {
    if field.contains('.') || field.contains('[') {
        Err(shape("a row path is not a flat field"))
    } else {
        Ok(field.to_string())
    }
}

fn as_field(value: &Value) -> Result<Option<String>, Fail> {
    match value {
        Value::String(s) if s == "@entity" => Err(shape("the whole row is not a field")),
        Value::String(s) => match s.strip_prefix("@entity.") {
            Some(rest) => row_path_free(rest).map(Some),
            None => Ok(None),
        },
        Value::Array(items)
            if items.first().and_then(Value::as_str) == Some("object/get")
                && items.get(1).and_then(Value::as_str) == Some("@entity") =>
        {
            match (items.len(), items.get(2).and_then(Value::as_str)) {
                (3, Some(field)) => row_path_free(field).map(Some),
                _ => Err(shape("object/get on the row is not a single flat field")),
            }
        }
        _ => Ok(None),
    }
}

fn scalar_const(value: Value) -> Result<Value, Fail> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(value),
        _ => Err(shape("the value is not a scalar")),
    }
}

fn term(value: &Value, ctx: &BindingContext) -> Result<Term, Fail> {
    if let Some(field) = as_field(value)? {
        return Ok(Term::Field(field));
    }
    match value {
        Value::String(s) if s.starts_with('@') => match ctx.resolve_binding(s) {
            Some(resolved) => scalar_const(resolved).map(Term::Const),
            None => Err(Fail::Unresolved(s.clone())),
        },
        Value::String(s) if s.contains('@') => Err(shape("a string with an embedded binding")),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            Ok(Term::Const(value.clone()))
        }
        Value::Array(items) => Err(Fail::Not(NotExpressible::Operator(
            items
                .first()
                .and_then(Value::as_str)
                .unwrap_or("[…]")
                .to_string(),
        ))),
        _ => Err(shape("a value with no predicate form")),
    }
}

fn list_of(value: &Value, ctx: &BindingContext) -> Result<Vec<Value>, Fail> {
    let resolved = match value {
        Value::Array(items) => items.clone(),
        Value::String(s) if s.starts_with('@') => match ctx.resolve_binding(s) {
            Some(Value::Array(items)) => items,
            Some(_) => return Err(shape("the list binding is not a list")),
            None => return Err(Fail::Unresolved(s.clone())),
        },
        _ => return Err(shape("the list is not a list")),
    };
    resolved
        .iter()
        .map(|item| match item {
            Value::String(s) if s.starts_with('@') => match ctx.resolve_binding(s) {
                Some(v) => scalar_const(v),
                None => Err(Fail::Unresolved(s.clone())),
            },
            Value::String(s) if s.contains('@') => Err(shape("a string with an embedded binding")),
            other => scalar_const(other.clone()),
        })
        .collect()
}

/// The evaluator fails on a missing or null operand of `array/includes` and `str/includes`, which drops the row:
/// that equals a failing leaf only where a failure makes the whole predicate false (a top-level `and` chain).
/// Under `or`/`not`, or anywhere the field may be missing, the interpreter keeps the result.
fn erroring_leaf(field: &str, required: &dyn Fn(&str) -> bool, exact: bool) -> Result<(), Fail> {
    if exact || required(field) {
        Ok(())
    } else {
        Err(Fail::Not(NotExpressible::NullSemantics(field.to_string())))
    }
}

/// What the enclosing `and` chains establish: fields shown present, and fields ordered against a number (where
/// `validate/required` is a null check, since a number has no empty form).
#[derive(Clone, Default)]
struct Scope {
    guarded: std::collections::HashSet<String>,
    numeric: std::collections::HashSet<String>,
}

/// The fields among the direct conjuncts that an ordering compares with a number constant.
fn numeric_ordered(args: &[Value], ctx: &BindingContext) -> std::collections::HashSet<String> {
    let mut fields = std::collections::HashSet::new();
    for arg in args {
        let Value::Array(items) = arg else { continue };
        let head = items.first().and_then(Value::as_str).unwrap_or("");
        if items.len() != 3 || !matches!(head, "<" | "<=" | ">" | ">=") {
            continue;
        }
        match (term(&items[1], ctx), term(&items[2], ctx)) {
            (Ok(Term::Field(field)), Ok(Term::Const(Value::Number(_))))
            | (Ok(Term::Const(Value::Number(_))), Ok(Term::Field(field))) => {
                fields.insert(field);
            }
            _ => {}
        }
    }
    fields
}

/// The field a null comparison `(op f null)` names.
fn null_compared_field(expr: &Value, op: &str) -> Option<String> {
    let Value::Array(items) = expr else {
        return None;
    };
    if items.len() != 3 || items[0].as_str() != Some(op) {
        return None;
    }
    let other = match (&items[1], &items[2]) {
        (left, Value::Null) => left,
        (Value::Null, right) => right,
        _ => return None,
    };
    as_field(other).ok().flatten()
}

/// The fields the direct conjuncts of one `and` show present: `(!= f null)`, `(not (= f null))`, and
/// `(validate/required f)` where `f` is ordered against a number in the chain.
fn present_fields(
    args: &[Value],
    numeric: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let mut fields = std::collections::HashSet::new();
    for arg in args {
        let not_null = null_compared_field(arg, "!=").or_else(|| match arg {
            Value::Array(items) if items.len() == 2 && items[0].as_str() == Some("not") => {
                null_compared_field(&items[1], "=")
            }
            _ => None,
        });
        fields.extend(not_null);
        if let Value::Array(items) = arg {
            if items.len() == 2 && items[0].as_str() == Some("validate/required") {
                if let Ok(Some(field)) = as_field(&items[1]) {
                    if numeric.contains(&field) {
                        fields.insert(field);
                    }
                }
            }
        }
    }
    fields
}

fn compare_leaf_of(
    field: String,
    op: RowCompareOp,
    value: Value,
    required: &dyn Fn(&str) -> bool,
    scope: &Scope,
) -> Result<RowPredicate, Fail> {
    let ordering = matches!(
        op,
        RowCompareOp::Lt | RowCompareOp::Le | RowCompareOp::Gt | RowCompareOp::Ge
    );
    if ordering && !required(&field) && !scope.guarded.contains(&field) {
        return Err(Fail::Not(NotExpressible::NullSemantics(field)));
    }
    Ok(match (&value, op) {
        (Value::Null, RowCompareOp::Eq) => RowPredicate::IsNull {
            field,
            is_null: true,
        },
        (Value::Null, RowCompareOp::Ne) => RowPredicate::IsNull {
            field,
            is_null: false,
        },
        _ => RowPredicate::Compare { field, op, value },
    })
}

fn compare_op_of(head: &str) -> Option<RowCompareOp> {
    match head {
        "=" | "==" | "eq" => Some(RowCompareOp::Eq),
        "!=" | "!==" | "neq" => Some(RowCompareOp::Ne),
        "<" => Some(RowCompareOp::Lt),
        "<=" => Some(RowCompareOp::Le),
        ">" => Some(RowCompareOp::Gt),
        ">=" => Some(RowCompareOp::Ge),
        _ => None,
    }
}

fn compile_expr(
    expr: &Value,
    ctx: &BindingContext,
    required: &dyn Fn(&str) -> bool,
    exact: bool,
    scope: &Scope,
) -> Result<RowPredicate, Fail> {
    match expr {
        Value::Bool(value) => Ok(RowPredicate::Const { value: *value }),
        Value::Array(items) => {
            let head = items.first().and_then(Value::as_str).unwrap_or("");
            let args = items.get(1..).unwrap_or(&[]);
            match head {
                "and" | "or" => {
                    let exact = exact && head == "and";
                    let inner = if head == "and" {
                        let mut numeric = scope.numeric.clone();
                        numeric.extend(numeric_ordered(args, ctx));
                        let mut guarded = scope.guarded.clone();
                        guarded.extend(present_fields(args, &numeric));
                        Scope { guarded, numeric }
                    } else {
                        scope.clone()
                    };
                    let of = args
                        .iter()
                        .map(|a| compile_expr(a, ctx, required, exact, &inner))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(if head == "and" {
                        RowPredicate::All { of }
                    } else {
                        RowPredicate::Any { of }
                    })
                }
                "not" if args.len() == 1 => Ok(RowPredicate::Not {
                    of: Box::new(compile_expr(&args[0], ctx, required, false, scope)?),
                }),
                "validate/required" if args.len() == 1 => {
                    // Equal to "not null" only where the field is ordered against a number in the same chain: a
                    // string or a list is also "required" only when non-empty.
                    match as_field(&args[0])? {
                        Some(field) if scope.numeric.contains(&field) => Ok(RowPredicate::IsNull {
                            field,
                            is_null: false,
                        }),
                        _ => Err(shape(
                            "validate/required is a null check only on a field ordered against a number",
                        )),
                    }
                }
                "array/includes" if args.len() == 2 => {
                    if let Some(field) = as_field(&args[1])? {
                        erroring_leaf(&field, required, exact)?;
                        Ok(RowPredicate::Member {
                            field,
                            values: list_of(&args[0], ctx)?,
                        })
                    } else if let Some(field) = as_field(&args[0])? {
                        erroring_leaf(&field, required, exact)?;
                        match term(&args[1], ctx)? {
                            Term::Const(value) => Ok(RowPredicate::HasElement { field, value }),
                            Term::Field(_) => Err(shape("two row fields")),
                        }
                    } else {
                        Err(shape("array/includes names no row field"))
                    }
                }
                "str/includes" if args.len() == 2 => {
                    let Term::Const(Value::String(text)) = term(&args[1], ctx)? else {
                        return Err(shape("str/includes needle is not a constant string"));
                    };
                    if let Some(field) = as_field(&args[0])? {
                        erroring_leaf(&field, required, exact)?;
                        return Ok(RowPredicate::Contains {
                            field,
                            text,
                            case_sensitive: true,
                        });
                    }
                    if let Value::Array(lower) = &args[0] {
                        if lower.len() == 2 && lower[0].as_str() == Some("str/lower") {
                            if let Some(field) = as_field(&lower[1])? {
                                erroring_leaf(&field, required, exact)?;
                                if text.to_lowercase() != text {
                                    return Err(shape(
                                        "case-insensitive needle is not already lower case",
                                    ));
                                }
                                return Ok(RowPredicate::Contains {
                                    field,
                                    text,
                                    case_sensitive: false,
                                });
                            }
                        }
                    }
                    Err(shape("str/includes haystack is not a row field"))
                }
                other => match compare_op_of(other) {
                    Some(op) if args.len() == 2 => {
                        match (term(&args[0], ctx)?, term(&args[1], ctx)?) {
                            (Term::Field(field), Term::Const(value)) => {
                                compare_leaf_of(field, op, value, required, scope)
                            }
                            (Term::Const(value), Term::Field(field)) => {
                                compare_leaf_of(field, op.flipped(), value, required, scope)
                            }
                            _ => Err(shape("a comparison needs one row field and one constant")),
                        }
                    }
                    _ => Err(Fail::Not(NotExpressible::Operator(other.to_string()))),
                },
            }
        }
        _ => Err(shape("not a predicate form")),
    }
}

/// Compile an S-expression (as a `Value`) over `@entity` fields and literals into a [`RowPredicate`].
/// `@user`, `@payload`, `@config`, `@now` and the like are specialised to their value in `ctx`. A policy
/// that cannot be specialised compiles to `const false` (a row whose predicate fails is hidden); a filter
/// that cannot is [`NotExpressible::Binding`]. The compiler never drops a conjunct.
///
/// The evaluator reads a missing or null value as below a number while a predicate fails the comparison.
/// `orb validate` rejects an ordering over a field that can be missing unless the program guards it (ORB_X_ORDERING_OPTIONAL_OPERAND),
/// so an ordering comparison compiles on a field `required` says the entity declares required or defaulted, or when
/// a direct conjunct of an enclosing `and` is a presence guard on it: a null field then makes the whole
/// conjunction false in both worlds.
pub fn compile_predicate(
    expr: &Value,
    ctx: &BindingContext,
    role: PredicateRole,
    required: &dyn Fn(&str) -> bool,
) -> Result<RowPredicate, NotExpressible> {
    match compile_expr(expr, ctx, required, true, &Scope::default()) {
        Ok(predicate) => Ok(predicate),
        Err(Fail::Not(why)) => Err(why),
        Err(Fail::Unresolved(binding)) => match role {
            PredicateRole::Policy => Ok(RowPredicate::Const { value: false }),
            PredicateRole::Filter => Err(NotExpressible::Binding(binding)),
        },
    }
}

/// The `RowOrder` of a fetch's `(field, descending)` spec.
pub fn order_of(field: &str, descending: bool) -> Result<RowOrder, NotExpressible> {
    if field.contains('.') || field.contains('[') {
        return Err(NotExpressible::Shape(
            "an order field that is a path".to_string(),
        ));
    }
    Ok(RowOrder {
        field: field.to_string(),
        direction: if descending {
            RowDirection::Desc
        } else {
            RowDirection::Asc
        },
    })
}

#[allow(unused_imports)]
use orbital_core_base::row_set::{row_field, is_missing, same_value, order_values, compare_leaf, fold_case};
#[allow(unused_imports)]
pub use orbital_core_base::row_set::{RowCompareOp, RowPredicate, RowDirection, RowOrder, RowTotalMode, RowQuery, RowSetRef, ROW_SET_MARKER, row_set_marker};
