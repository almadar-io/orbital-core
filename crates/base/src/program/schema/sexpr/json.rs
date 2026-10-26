//! The value leaves of a JSON-encoded expression.

use super::*;

pub(super) fn json_head(value: &serde_json::Value) -> Option<(&str, &[serde_json::Value])> {
    let arr = value.as_array()?;
    let (head, args) = arr.split_first()?;
    Some((head.as_str()?, args))
}

pub(super) fn json_lambda_body(value: &serde_json::Value) -> Option<&serde_json::Value> {
    value.as_array().filter(|a| a.len() >= 3)?.last()
}

pub(super) fn json_plan(walk: &ValueWalk<'_>, value: &serde_json::Value) -> Option<Vec<ValueSlot>> {
    let (op, args) = json_head(value)?;
    plan_value_sources(
        walk,
        op,
        args.len(),
        |i| {
            json_lambda_body(&args[i]).is_some()
                && json_head(&args[i])
                    .and_then(|(head, _)| (walk.lookup)(head))
                    .is_some_and(|r| r.returns_function)
        },
        false,
    )
}

/// `serde_json::Value` counterpart of [`SExpression::value_leaves`]. Config
/// values arrive as raw JSON before/without a full `SExpression` parse. The
/// dead-branch fold of [`ValueWalk::skip_dead_branches`] is an `SExpression`
/// notion and does not apply here: every branch contributes.
pub fn json_value_leaves<'a>(
    value: &'a serde_json::Value,
    walk: &ValueWalk<'_>,
) -> Vec<&'a serde_json::Value> {
    let mut out = Vec::new();
    collect_json_value_leaves(value, walk, &mut out);
    out
}

pub(super) fn collect_json_value_leaves<'a>(
    value: &'a serde_json::Value,
    walk: &ValueWalk<'_>,
    out: &mut Vec<&'a serde_json::Value>,
) {
    let Some(slots) = json_plan(walk, value) else {
        out.push(value);
        return;
    };
    let args = &value.as_array().expect("a plan exists only for arrays")[1..];
    for slot in slots {
        match slot {
            ValueSlot::Arg(i) => collect_json_value_leaves(&args[i], walk, out),
            ValueSlot::LambdaBody(i) => {
                if let Some(body) = json_lambda_body(&args[i]) {
                    collect_json_value_leaves(body, walk, out);
                }
            }
        }
    }
}

/// Mutable counterpart of [`json_value_leaves`], for sites that rewrite
/// descriptors in place (renames, codegen qualification).
pub fn json_value_leaves_mut<'a>(
    value: &'a mut serde_json::Value,
    walk: &ValueWalk<'_>,
) -> Vec<&'a mut serde_json::Value> {
    let mut out = Vec::new();
    collect_json_value_leaves_mut(value, walk, &mut out);
    out
}

pub(super) fn collect_json_value_leaves_mut<'a>(
    value: &'a mut serde_json::Value,
    walk: &ValueWalk<'_>,
    out: &mut Vec<&'a mut serde_json::Value>,
) {
    let Some(slots) = json_plan(walk, value) else {
        out.push(value);
        return;
    };
    let arr = value.as_array_mut().expect("a plan exists only for arrays");
    for (i, arg) in arr[1..].iter_mut().enumerate() {
        match slots.iter().find(|slot| slot.arg() == i) {
            Some(ValueSlot::Arg(_)) => collect_json_value_leaves_mut(arg, walk, out),
            Some(ValueSlot::LambdaBody(_)) => {
                if let Some(body) = arg.as_array_mut().and_then(|a| a.last_mut()) {
                    collect_json_value_leaves_mut(body, walk, out);
                }
            }
            None => {}
        }
    }
}
