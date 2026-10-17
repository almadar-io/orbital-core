use crate::value::Value;
use serde::Deserialize;
use serde::Serialize;
use std::cmp::Ordering;
use std::sync::Arc;

/// The comparison operators of a `compare` leaf.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowCompareOp {
    #[serde(rename = "==")]
    #[cfg_attr(feature = "ts-export", ts(rename = "=="))]
    Eq,
    #[serde(rename = "!=")]
    #[cfg_attr(feature = "ts-export", ts(rename = "!="))]
    Ne,
    #[serde(rename = "<")]
    #[cfg_attr(feature = "ts-export", ts(rename = "<"))]
    Lt,
    #[serde(rename = "<=")]
    #[cfg_attr(feature = "ts-export", ts(rename = "<="))]
    Le,
    #[serde(rename = ">")]
    #[cfg_attr(feature = "ts-export", ts(rename = ">"))]
    Gt,
    #[serde(rename = ">=")]
    #[cfg_attr(feature = "ts-export", ts(rename = ">="))]
    Ge,
}

impl RowCompareOp {
    pub fn flipped(self) -> Self {
        match self {
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Gt => Self::Lt,
            Self::Ge => Self::Le,
            other => other,
        }
    }
}

/// A row predicate with two-valued logic. Its TypeScript form is declared by hand in `embed::wire::ts_export`
/// (a self-referential type).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RowPredicate {
    Compare {
        field: String,
        op: RowCompareOp,
        value: Value,
    },
    Member {
        field: String,
        values: Vec<Value>,
    },
    HasElement {
        field: String,
        value: Value,
    },
    Contains {
        field: String,
        text: String,
        case_sensitive: bool,
    },
    IsNull {
        field: String,
        is_null: bool,
    },
    All {
        of: Vec<RowPredicate>,
    },
    Any {
        of: Vec<RowPredicate>,
    },
    Not {
        of: Box<RowPredicate>,
    },
    Const {
        value: bool,
    },
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowDirection {
    Asc,
    Desc,
}

/// One sort key. Missing and null values sort last in both directions; ties break by `id` ascending.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowOrder {
    pub field: String,
    pub direction: RowDirection,
}

/// Whether `total` counts every match or the store may skip the count.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowTotalMode {
    Exact,
    None,
}

/// A pushed-down query: rows matching `policy` AND `filter`, ordered, then windowed. With no `order` the rows are
/// ordered by `createdAt` ascending, then `id` (EXEC-FETCH-1).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RowQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<RowPredicate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<RowPredicate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<RowOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<RowTotalMode>,
    /// The nearest `k` rows by cosine similarity among those `policy` and `filter` admit (EXEC-FETCH-2). Excludes
    /// `order`, `offset` and `limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest: Option<Box<RowNearest>>,
}

/// A vector-similarity query: up to `k` rows ordered by descending cosine similarity of `field` to `vector`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowNearest {
    pub field: String,
    pub vector: Vec<f64>,
    pub k: usize,
}

/// A fetched page by reference; `query` re-counts `total` at resolve time (order and window ignored).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowSetRef {
    pub entity_type: String,
    pub ids: Vec<String>,
    pub total: usize,
    pub query: RowQuery,
}

/// Tag key of a [`RowSetRef`] carried as a render prop value (core's `ROW_SET_MARKER`).
pub const ROW_SET_MARKER: &str = "$rowSet";

/// The render-prop leaf that carries `reference` (core's `RowSetMarker`): `{ "$rowSet": true, "ref": … }`.
pub fn row_set_marker(reference: &RowSetRef) -> Value {
    let mut leaf = crate::value::ValueMap::new();
    leaf.insert(ROW_SET_MARKER.to_string(), Value::Bool(true));
    leaf.insert(
        "ref".to_string(),
        serde_json::to_value(reference)
            .ok()
            .and_then(|json| serde_json::from_value(json).ok())
            .unwrap_or(Value::Null),
    );
    Value::Object(leaf)
}

// ----- predicate evaluation -------------------------------------------------------------------

pub fn row_field<'a>(row: &'a Value, field: &str) -> Option<&'a Value> {
    row.get_path(field)
}

pub fn is_missing(value: Option<&Value>) -> bool {
    matches!(value, None | Some(Value::Null))
}

/// Equality of two present values: scalars of one kind that are equal. An array or object is never equal.
pub fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

/// Order of two present values of one kind; `None` when the kinds differ or a value is not scalar.
pub fn order_values(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::String(x), Value::String(y)) => Some(x.as_str().cmp(y.as_str())),
        (Value::Number(x), Value::Number(y)) => Some(if x == y {
            Ordering::Equal
        } else if x < y {
            Ordering::Less
        } else {
            Ordering::Greater
        }),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

pub fn compare_leaf(value: Option<&Value>, op: RowCompareOp, target: &Value) -> bool {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return op == RowCompareOp::Ne && !target.is_null();
    };
    match op {
        RowCompareOp::Eq => same_value(value, target),
        RowCompareOp::Ne => !same_value(value, target),
        _ => match order_values(value, target) {
            None => false,
            Some(c) => match op {
                RowCompareOp::Lt => c == Ordering::Less,
                RowCompareOp::Le => c != Ordering::Greater,
                RowCompareOp::Gt => c == Ordering::Greater,
                _ => c != Ordering::Less,
            },
        },
    }
}

/// Full Unicode case folding as `to_uppercase` then `to_lowercase` gives it (the TS mirror's fold).
pub fn fold_case(text: &str) -> String {
    text.to_uppercase().to_lowercase()
}

impl RowPredicate {
    /// Whether `row` satisfies the predicate. Borrows the row; allocates only for the case-folded `contains`.
    pub fn matches(&self, row: &Value) -> bool {
        match self {
            Self::Const { value } => *value,
            Self::All { of } => of.iter().all(|p| p.matches(row)),
            Self::Any { of } => of.iter().any(|p| p.matches(row)),
            Self::Not { of } => !of.matches(row),
            Self::Compare { field, op, value } => compare_leaf(row_field(row, field), *op, value),
            Self::IsNull { field, is_null } => is_missing(row_field(row, field)) == *is_null,
            Self::Member { field, values } => match row_field(row, field) {
                Some(v) if !v.is_null() => values.iter().any(|t| same_value(t, v)),
                _ => false,
            },
            Self::HasElement { field, value } => match row_field(row, field) {
                Some(Value::Array(items)) => items.iter().any(|v| same_value(v, value)),
                _ => false,
            },
            Self::Contains {
                field,
                text,
                case_sensitive,
            } => match row_field(row, field) {
                Some(Value::String(s)) => {
                    if *case_sensitive {
                        s.contains(text.as_str())
                    } else {
                        fold_case(s).contains(&fold_case(text))
                    }
                }
                _ => false,
            },
        }
    }
}
/// The row's `id` as text; empty when it has none.
pub fn row_id(row: &Value) -> String {
    match row_field(row, "id") {
        Some(Value::String(s)) => s.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

/// The one `nearest` selection (EXEC-FETCH-2), run by the kernel and every in-process store: `rows` are already
/// admitted by the policy and filter. Cosine similarity `dot / (|q| * |r|)`, descending, ties by `id` ascending,
/// the first `k`. A row whose field is absent, null or of zero norm is skipped; a stored vector that is not a
/// `[number]` of the query's length, a zero-norm query or `k = 0` fails.
pub fn nearest_rows(
    rows: Vec<Arc<Value>>,
    nearest: &RowNearest,
) -> Result<Vec<Arc<Value>>, String> {
    if nearest.k == 0 {
        return Err("nearest: k must be a positive integer".to_string());
    }
    let query_norm = nearest.vector.iter().map(|x| x * x).sum::<f64>().sqrt();
    if query_norm == 0.0 {
        return Err("nearest: the query vector has zero norm".to_string());
    }
    let mut scored: Vec<(f64, String, Arc<Value>)> = Vec::new();
    for row in rows {
        let stored = match row_field(&row, &nearest.field) {
            None | Some(Value::Null) => continue,
            Some(Value::Array(items)) => items,
            Some(_) => {
                return Err(format!(
                    "nearest: field `{}` of row `{}` is not a [number]",
                    nearest.field,
                    row_id(&row)
                ))
            }
        };
        if stored.len() != nearest.vector.len() {
            return Err(format!(
                "nearest: field `{}` of row `{}` has {} dimensions, the query has {}",
                nearest.field,
                row_id(&row),
                stored.len(),
                nearest.vector.len()
            ));
        }
        let (mut dot, mut sum_squares) = (0.0_f64, 0.0_f64);
        for (item, q) in stored.iter().zip(&nearest.vector) {
            let Value::Number(x) = item else {
                return Err(format!(
                    "nearest: field `{}` of row `{}` is not a [number]",
                    nearest.field,
                    row_id(&row)
                ));
            };
            dot += x * q;
            sum_squares += x * x;
        }
        let row_norm = sum_squares.sqrt();
        if row_norm == 0.0 {
            continue;
        }
        let id = row_id(&row);
        scored.push((dot / (query_norm * row_norm), id, row));
    }
    scored.sort_by(|a, b| {
        if a.0 == b.0 {
            a.1.cmp(&b.1)
        } else {
            b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal)
        }
    });
    scored.truncate(nearest.k);
    Ok(scored.into_iter().map(|(_, _, row)| row).collect())
}

#[cfg(test)]
mod nearest_tests {
    use super::*;

    fn row(id: &str, embedding: Option<Value>) -> Arc<Value> {
        let mut map = crate::value::ValueMap::new();
        map.insert("id".to_string(), Value::String(id.to_string()));
        if let Some(v) = embedding {
            map.insert("embedding".to_string(), v);
        }
        Arc::new(Value::Object(map))
    }

    fn vec_of(xs: &[f64]) -> Value {
        Value::Array(xs.iter().map(|x| Value::Number(*x)).collect())
    }

    fn ask(vector: &[f64], k: usize) -> RowNearest {
        RowNearest {
            field: "embedding".to_string(),
            vector: vector.to_vec(),
            k,
        }
    }

    fn ids(rows: &[Arc<Value>]) -> Vec<String> {
        rows.iter().map(|r| row_id(r)).collect()
    }

    #[test]
    fn orders_by_descending_cosine_then_id_and_keeps_k() {
        let rows = vec![
            row("c", Some(vec_of(&[0.0, 1.0]))),
            row("b", Some(vec_of(&[2.0, 0.0]))),
            row("a", Some(vec_of(&[1.0, 0.0]))),
            row("d", Some(vec_of(&[1.0, 1.0]))),
        ];
        let got = nearest_rows(rows.clone(), &ask(&[1.0, 0.0], 3)).unwrap();
        assert_eq!(ids(&got), ["a", "b", "d"]);
        let all = nearest_rows(rows, &ask(&[1.0, 0.0], 10)).unwrap();
        assert_eq!(ids(&all), ["a", "b", "d", "c"]);
    }

    #[test]
    fn skips_absent_null_and_zero_norm_rows() {
        let rows = vec![
            row("a", None),
            row("b", Some(Value::Null)),
            row("z", Some(vec_of(&[0.0, 0.0]))),
            row("ok", Some(vec_of(&[1.0, 1.0]))),
        ];
        let got = nearest_rows(rows, &ask(&[1.0, 0.0], 5)).unwrap();
        assert_eq!(ids(&got), ["ok"]);
    }

    #[test]
    fn fails_on_dimension_mismatch_zero_query_and_zero_k() {
        let rows = vec![row("a", Some(vec_of(&[1.0, 0.0, 0.0])))];
        assert!(nearest_rows(rows.clone(), &ask(&[1.0, 0.0], 1))
            .unwrap_err()
            .contains("3 dimensions"));
        assert!(nearest_rows(rows.clone(), &ask(&[0.0, 0.0, 0.0], 1))
            .unwrap_err()
            .contains("zero norm"));
        assert!(nearest_rows(rows.clone(), &ask(&[1.0, 0.0, 0.0], 0)).is_err());
        assert!(nearest_rows(rows, &ask(&[1.0, 0.0, 0.0], 1)).is_ok());
    }
}
