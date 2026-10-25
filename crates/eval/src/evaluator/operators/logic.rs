//! Comparison, logical, and control-flow operators.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

// ============================================================================
// Comparison Operators
// ============================================================================

pub struct EqOp;
impl Operator for EqOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "=", >= 2);
        let a = eval_expr(&args[0], ctx)?;
        let b = eval_expr(&args[1], ctx)?;
        Ok(Value::Bool(a.loose_eq(&b)))
    }
}

pub struct NeOp;
impl Operator for NeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "!=", >= 2);
        let a = eval_expr(&args[0], ctx)?;
        let b = eval_expr(&args[1], ctx)?;
        Ok(Value::Bool(!a.loose_eq(&b)))
    }
}

pub struct LtOp;
impl Operator for LtOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "<", >= 2);
        let a = eval_expr(&args[0], ctx)?;
        let b = eval_expr(&args[1], ctx)?;
        Ok(Value::Bool(
            a.compare(&b)
                .map(|ord| ord == std::cmp::Ordering::Less)
                .unwrap_or(false),
        ))
    }
}

pub struct GtOp;
impl Operator for GtOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, ">", >= 2);
        let a = eval_expr(&args[0], ctx)?;
        let b = eval_expr(&args[1], ctx)?;
        Ok(Value::Bool(
            a.compare(&b)
                .map(|ord| ord == std::cmp::Ordering::Greater)
                .unwrap_or(false),
        ))
    }
}

pub struct LeOp;
impl Operator for LeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "<=", >= 2);
        let a = eval_expr(&args[0], ctx)?;
        let b = eval_expr(&args[1], ctx)?;
        Ok(Value::Bool(
            a.compare(&b)
                .map(|ord| ord != std::cmp::Ordering::Greater)
                .unwrap_or(false),
        ))
    }
}

pub struct GeOp;
impl Operator for GeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, ">=", >= 2);
        let a = eval_expr(&args[0], ctx)?;
        let b = eval_expr(&args[1], ctx)?;
        Ok(Value::Bool(
            a.compare(&b)
                .map(|ord| ord != std::cmp::Ordering::Less)
                .unwrap_or(false),
        ))
    }
}

// ============================================================================
// Logical Operators
// ============================================================================

/// Operand semantics, as the registry's `last-truthy-of-args` and the JS evaluator: the first falsy value, else the last.
pub struct AndOp;
impl Operator for AndOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let mut last = Value::Bool(true);
        for arg in args {
            last = eval_expr(arg, ctx)?;
            if !last.is_truthy() {
                return Ok(last);
            }
        }
        Ok(last)
    }
}

/// Operand semantics, as the registry's `first-truthy-of-args` and the JS evaluator: the first truthy value, else the last.
pub struct OrOp;
impl Operator for OrOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let mut last = Value::Bool(false);
        for arg in args {
            last = eval_expr(arg, ctx)?;
            if last.is_truthy() {
                return Ok(last);
            }
        }
        Ok(last)
    }
}

pub struct NotOp;
impl Operator for NotOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "not".to_string(),
                expected: 1,
                actual: 0,
            }));
        }
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(!val.is_truthy()))
    }
}

pub struct IfOp;
impl Operator for IfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "if", >= 2);
        let condition = eval_expr(&args[0], ctx)?;
        if condition.is_truthy() {
            eval_expr(&args[1], ctx)
        } else if args.len() > 2 {
            eval_expr(&args[2], ctx)
        } else {
            Ok(Value::Null)
        }
    }
}

// ============================================================================
// Control Flow Operators
// ============================================================================

pub struct DoOp;
impl Operator for DoOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let mut last = Value::Null;
        for arg in args {
            last = eval_expr(arg, ctx)?;
        }
        Ok(last)
    }
}

pub struct WhenOp;
impl Operator for WhenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "when", >= 2);
        let condition = eval_expr(&args[0], ctx)?;
        if condition.is_truthy() {
            eval_expr(&args[1], ctx)
        } else {
            Ok(Value::Null)
        }
    }
}

pub struct LetOp;
impl Operator for LetOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "let", >= 2);

        let Some(scope) = orbital_core_base::schema::sexpr::SequentialBindings::from_args(args)
        else {
            return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "array of bindings".to_string(),
                actual: "non-array".to_string(),
            }));
        };

        let mut child_ctx = ctx.child();

        for (name, initializer) in scope.bindings {
            let value = eval_expr(initializer, &child_ctx)?;
            child_ctx.set_local(name, value);
        }

        eval_expr(scope.body, &child_ctx)
    }
}

/// list - literal array constructor: evaluate each argument, collect to an array.
pub struct ListOp;
impl Operator for ListOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let items: Result<Vec<Value>, _> = args.iter().map(|a| eval_expr(a, ctx)).collect();
        Ok(Value::Array(items?))
    }
}
