//! The one Rust definition of the row-query contract: `RowPredicate` / `RowOrder` / `RowQuery`,
//! the conditional writes, the capability shapes and the run of each over rows held in memory.
//!
//! Mirrors `@almadar/core` `service-types.ts` and `row-query.ts`; the serde forms are the TS JSON
//! forms, so `tests/baked/pushdown/*.json` load unchanged. Every evaluation
//! site (the in-kernel evaluator behind [`EntitySource::query_page`](super::EntitySource), the
//! in-memory cache's writes, the differential tests) goes through here.
//!
//! Two-valued null logic: a missing or null field satisfies only `!=` against a non-null value and
//! `isNull: true`. Strings compare by code point (UTF-8 byte order). A comparison relates only two
//! values of one kind. An array or object is never equal to anything.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_eval::evaluator::context::BindingContext;
use orbital_core_base::value::Value;
use orbital_core_base::collections::IndexMap;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
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

#[allow(unused_imports)]
use orbital_core_base::row_set::{nearest_rows, row_field, is_missing, same_value, order_values, compare_leaf, fold_case};
#[allow(unused_imports)]
pub use orbital_core_base::row_set::{row_id, RowNearest, RowCompareOp, RowPredicate, RowDirection, RowOrder, RowTotalMode, RowQuery, RowSetRef, ROW_SET_MARKER, row_set_marker};

mod capabilities;
mod ordering;
mod run;
mod writes;
mod compile;
pub use capabilities::{
    guard_unsupported_reason, query_unsupported_reason, GuardCapabilities, IndexRealization,
    OperatorValueKinds, QueryCapabilities, QueryUnsupportedReason, RowCombinator, RowLeafKind,
    RowOperator, RowOrderableFields, RowValueKind, WriteCapabilities,
};
pub use compile::{
    compile_predicate, lower_object_filter, order_of, NotExpressible, PredicateRole,
};
pub use ordering::{compare_rows, default_row_order, effective_row_order, DEFAULT_ORDER_FIELD};
pub use run::{and_predicates, resolve_row_set, run_ids, run_query, QueryRun, ScanControl};
pub use writes::{
    apply_write, apply_write_batch, apply_write_batch_across, touched_ids, CreateIds, RowCommit,
    RowCommitWrite, RowTable, RowWriteBatch, RowWriteFailureMode, RowWriteOp, RowWriteOutcome,
};
