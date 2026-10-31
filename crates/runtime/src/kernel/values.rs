//! Converting between values and S-expressions, and the pairs a client dispatch already delivered.

use super::*;

/// The `(trait, event)` pairs a [`ClientDispatch`] already realized LOCALLY —
/// every event its own outcomes emitted (`response.emitted_events`), plus the
/// seed dispatch itself, read off `server_leg` (a leg's `target_trait`/`event`
/// ARE the seed trait's own dispatch — see `dispatch_with_server_leg`; `None`
/// when no leg was produced, so only the local emits are known). Pass the
/// result to [`RuntimeKernel::apply_server_response`] so a live-push echo of
/// an event this client already ran doesn't run it again — the Rust twin of
/// `useTraitStateMachine.ts`'s `stampLocallyDeliveredEchoes`.
pub fn already_delivered_from(dispatch: &ClientDispatch) -> HashSet<(String, String)> {
    let mut set: HashSet<(String, String)> = dispatch
        .response
        .emitted_events
        .iter()
        .map(|e| {
            (
                e.source
                    .as_ref()
                    .and_then(|s| s.r#trait.clone())
                    .unwrap_or_default(),
                e.event.clone(),
            )
        })
        .collect();
    if !dispatch.server_legs.is_empty() {
        set.insert(dispatch.seed.clone());
    }
    for leg in &dispatch.server_legs {
        if let Some(target) = &leg.target_trait {
            set.insert((target.clone(), leg.event.clone()));
        }
    }
    set
}

/// A runtime value as the expression it denotes. Equals the JSON round trip this replaces:
/// JSON has no NaN or infinity, so a non-finite number reads back as `null`.
pub(super) fn value_to_sexpr(value: &Value) -> SExpression {
    fn finite(expr: &mut SExpression) {
        match expr {
            SExpression::Number(n) if !n.is_finite() => *expr = SExpression::Null,
            SExpression::Array(items) => items.iter_mut().for_each(finite),
            SExpression::Object(obj) => obj.values_mut().for_each(finite),
            _ => {}
        }
    }
    let mut expr: SExpression = value.clone().into();
    finite(&mut expr);
    expr
}

/// Convert an SExpression to a runtime Value for instance seeding.
pub(crate) fn sexpr_to_value(sexpr: &SExpression) -> Value {
    match sexpr {
        SExpression::String(s) => Value::string(s.clone()),
        SExpression::Number(n) => Value::Number(*n),
        SExpression::Bool(b) => Value::Bool(*b),
        SExpression::Null => Value::Null,
        SExpression::Array(items) => Value::Array(items.iter().map(sexpr_to_value).collect()),
        SExpression::Object(obj) => {
            let map: ValueMap = obj
                .iter()
                .map(|(k, v)| (k.clone(), sexpr_to_value(v)))
                .collect();
            Value::object(map)
        }
    }
}
