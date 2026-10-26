//! Ordering rows: kinds, comparison and the default order.

use super::*;

// ----- ordering -------------------------------------------------------------------------------

/// Rank of a present value's kind, so a column of mixed kinds still has a total order.
pub(super) fn kind_rank(value: &Value) -> u8 {
    match value {
        Value::Bool(_) => 0,
        Value::Number(_) => 1,
        Value::String(_) => 2,
        _ => 3,
    }
}

pub(super) fn total_order_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.total_cmp(y),
        _ => order_values(a, b).unwrap_or_else(|| kind_rank(a).cmp(&kind_rank(b))),
    }
}

/// Order of two rows by one field: missing and null last in both directions, no tie-break.
pub(super) fn compare_by_field(a: &Value, b: &Value, field: &str, direction: RowDirection) -> Ordering {
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
