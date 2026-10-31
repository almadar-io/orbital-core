//! The lambda operator and applying a lambda value.

use super::*;

// ============================================================================
// Lambda Operator (Phase 1)
// ============================================================================

pub struct FnOp;
impl Operator for FnOp {
    fn evaluate(&self, args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        // ["fn", "x", body] or ["fn", ["a", "b"], body]
        require_arity!(args, "fn", >= 2);

        // Parse parameters
        let params = match &args[0] {
            SExpression::String(name) => vec![name.to_string()],
            SExpression::Array(arr) => arr
                .iter()
                .filter_map(|p| {
                    if let SExpression::String(s) = p {
                        Some(s.to_string())
                    } else {
                        None
                    }
                })
                .collect(),
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "string or array of strings".to_string(),
                    actual: "invalid parameter list".to_string(),
                }))
            }
        };

        // Store body as-is (not evaluated yet)
        let body = std::sync::Arc::new(args[1].clone());
        crate::evaluator::trace::adopt(&body, &args[1]);

        Ok(Value::lambda(params, body))
    }
}

/// `(quote x)` — an S-expression held as data (G-CROSS-041): the encoded body
/// (`schema::quote`) decodes to `x`, returned without evaluating or resolving it.
pub struct QuoteOp;
impl Operator for QuoteOp {
    fn evaluate(&self, args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "quote", 1);
        let SExpression::String(body) = &args[0] else {
            return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "an encoded quote body (string)".to_string(),
                actual: "a non-string quote argument".to_string(),
            }));
        };
        orbital_core_base::schema::quote::decode_quote_body(body)
            .map(Value::from)
            .ok_or_else(|| {
                OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "an encoded quote body (JSON)".to_string(),
                    actual: "a malformed quote body".to_string(),
                })
            })
    }
}

/// `(quasiquote x)` — a quoted template whose holes are live: the body
/// (`schema::quote::quasiquote_expr`) decodes to the template, each hole
/// expression after it is evaluated, and marker `["unquote", i]` becomes
/// hole `i`'s value. Twin of `@almadar/core` `instantiateQuasiquote`.
pub struct QuasiquoteOp;

pub(super) fn unquote_marker(value: &Value) -> Option<usize> {
    match value {
        Value::Array(items) if items.len() == 2 => match (&items[0], &items[1]) {
            (Value::String(h), Value::Number(n))
                if h == orbital_core_base::schema::quote::UNQUOTE_HEAD
                    && *n >= 0.0
                    && n.fract() == 0.0 =>
            {
                Some(*n as usize)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The kind of the first value inside `value` a program cannot hold as data
/// (a lambda), matching `@almadar/evaluator`'s `toProgramData` refusal.
pub(super) fn non_data_kind(value: &Value) -> Option<&'static str> {
    match value {
        Value::Array(items) => items.iter().find_map(non_data_kind),
        Value::Object(map) => map.values().find_map(non_data_kind),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
        Value::Lambda(_) => Some("function"),
        Value::Sealed(_) => Some("sealed"),
    }
}

pub(super) fn count_markers(value: &Value) -> usize {
    if unquote_marker(value).is_some() {
        return 1;
    }
    match value {
        Value::Array(items) => items.iter().map(count_markers).sum(),
        Value::Object(map) => map.values().map(count_markers).sum(),
        _ => 0,
    }
}

pub(super) fn fill_markers(value: Value, holes: &[Value]) -> Value {
    if let Some(i) = unquote_marker(&value) {
        return holes[i].clone();
    }
    match value {
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| fill_markers(v, holes)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, fill_markers(v, holes)))
                .collect(),
        ),
        other => other,
    }
}

impl Operator for QuasiquoteOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let Some(SExpression::String(body)) = args.first() else {
            return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "an encoded quasiquote body (string)".to_string(),
                actual: "a non-string quasiquote argument".to_string(),
            }));
        };
        let template = orbital_core_base::schema::quote::decode_quote_body(body)
            .map(Value::from)
            .ok_or_else(|| {
                OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "an encoded quasiquote body (JSON)".to_string(),
                    actual: "a malformed quasiquote body".to_string(),
                })
            })?;
        let holes = args[1..]
            .iter()
            .map(|e| super::eval_expr(e, ctx))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(kind) = holes.iter().find_map(non_data_kind) {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "quasiquote: a hole evaluated to a {kind} value, which cannot be held as program data"
            ))));
        }
        let markers = count_markers(&template);
        if markers != holes.len() {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "quasiquote: template has {markers} hole(s), got {} value(s)",
                holes.len()
            ))));
        }
        Ok(fill_markers(template, &holes))
    }
}

/// `(unquote e)` evaluated on its own: legal only as a hole inside
/// `(quasiquote …)`, which replaces it before evaluation.
pub struct UnquoteOp;
impl Operator for UnquoteOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        Err(OrbitalError::Eval(EvalError::InvalidArgument(
            "unquote: legal only inside a quasiquote".to_string(),
        )))
    }
}

/// `(behavior/apply value overrides)` — records call-site overrides on a trait
/// value (`schema::behavior_value`). Twin of `@almadar/core` `applyTraitOverrides`.
pub struct BehaviorApplyOp;
impl Operator for BehaviorApplyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "behavior/apply", >= 2);
        if args.len() > 2 {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "behavior/apply".to_string(),
                expected: 2,
                actual: args.len(),
            }));
        }
        let value = super::eval_expr(&args[0], ctx)?;
        let overrides = super::eval_expr(&args[1], ctx)?;
        orbital_core_base::schema::behavior_value::apply_behavior_overrides(&value, &overrides)
            .ok_or_else(|| {
                OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "a behavior value and overrides of its kind".to_string(),
                    actual: "a non-behavior value, or overrides with a key its kind lacks"
                        .to_string(),
                })
            })
    }
}

/// `(behavior/ref specifier)` — a whole behavior as a value. Twin of
/// `@almadar/evaluator` `evalBehaviorRef`.
pub struct BehaviorRefOp;
impl Operator for BehaviorRefOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "behavior/ref", >= 1);
        if args.len() > 1 {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "behavior/ref".to_string(),
                expected: 1,
                actual: args.len(),
            }));
        }
        let spec = super::eval_expr(&args[0], ctx)?;
        let spec = match spec {
            Value::String(spec) => spec,
            other => return Err(other.kind_error("string").into()),
        };
        orbital_core_base::schema::behavior_value::behavior_ref(&spec).ok_or_else(|| {
            OrbitalError::Eval(EvalError::TypeMismatch {
                expected:
                    "a behavior specifier (<prefix>/<name>, std/behaviors/<name> or ./<dir>/<name>)"
                        .to_string(),
                actual: spec.into_inner(),
            })
        })
    }
}
