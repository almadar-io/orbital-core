//! The `.lolo` surface form of an S-expression: the twin of `@almadar/core`
//! `formatSExpr` (parity fixture `tests/fixtures/format-sexpr`).

use super::quote::{decode_quote_body, quasiquote_of, quote_body_of, QUASIQUOTE_HEAD, QUOTE_HEAD};
use super::SExpression;
use crate::str_utils::js_number_string;

const UNQUOTE_HEAD: &str = "unquote";

/// Calls as `(op args…)`, bindings bare, strings quoted, objects `{ k: v }`
/// with keys sorted, literal arrays `[a b]`. A string-headed array is a call.
pub fn format_sexpr(expr: &SExpression) -> String {
    match expr {
        SExpression::Array(items) => {
            if let Some(body) = quote_body_of(expr) {
                let inner =
                    decode_quote_body(body).map_or_else(|| raw_body(body), |e| format_sexpr(&e));
                return format!("({QUOTE_HEAD} {inner})");
            }
            if let Some((body, holes)) = quasiquote_of(expr) {
                let inner = decode_quote_body(body)
                    .map_or_else(|| raw_body(body), |e| format_template(&e, holes));
                return format!("({QUASIQUOTE_HEAD} {inner})");
            }
            format_list(items, &|e| format_sexpr(e))
        }
        SExpression::Object(map) => format_object(map, &|e| format_sexpr(e)),
        _ => format_atom(expr),
    }
}

/// A body that is not JSON (never written by the encoder) prints as its string.
fn raw_body(body: &str) -> String {
    format_atom(&SExpression::String(body.to_string()))
}

fn format_template(expr: &SExpression, holes: &[SExpression]) -> String {
    match expr {
        SExpression::Array(items) => {
            if let [SExpression::String(head), SExpression::Number(i)] = items.as_slice() {
                if head == UNQUOTE_HEAD {
                    let hole = holes
                        .get(*i as usize)
                        .map_or_else(|| "null".to_string(), format_sexpr);
                    return format!("({UNQUOTE_HEAD} {hole})");
                }
            }
            format_list(items, &|e| format_template(e, holes))
        }
        SExpression::Object(map) => format_object(map, &|e| format_template(e, holes)),
        _ => format_atom(expr),
    }
}

fn format_list(items: &[SExpression], each: &dyn Fn(&SExpression) -> String) -> String {
    match items.first() {
        Some(SExpression::String(op)) => {
            let parts: Vec<String> = std::iter::once(op.clone())
                .chain(items[1..].iter().map(each))
                .collect();
            format!("({})", parts.join(" "))
        }
        _ => format!("[{}]", items.iter().map(each).collect::<Vec<_>>().join(" ")),
    }
}

fn format_object(
    map: &std::collections::HashMap<String, SExpression>,
    each: &dyn Fn(&SExpression) -> String,
) -> String {
    if map.is_empty() {
        return "{}".to_string();
    }
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let entries: Vec<String> = keys
        .into_iter()
        .map(|k| format!("{k}: {}", each(&map[k])))
        .collect();
    format!("{{ {} }}", entries.join(", "))
}

fn format_atom(expr: &SExpression) -> String {
    match expr {
        SExpression::Null => "null".to_string(),
        SExpression::Bool(b) => b.to_string(),
        SExpression::Number(n) => js_number_string(*n),
        SExpression::String(s) if s.starts_with('@') => s.clone(),
        SExpression::String(s) => serde_json::to_string(s).expect("a string always serializes"),
        SExpression::Array(_) | SExpression::Object(_) => format_sexpr(expr),
    }
}
