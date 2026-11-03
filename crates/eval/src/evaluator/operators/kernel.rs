//! `kernel/*` pure operators. They see the running kernel's queue, frame and stamp through [`KernelIntrospect`], and
//! `kernel/apply` calls a pure operator the grant names.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::schema::quote::quote_expr;


fn readable<'a>(ctx: &'a BindingContext, op: &str) -> Result<&'a dyn crate::evaluator::context::KernelIntrospect, OrbitalError> {
    let kernel = ctx.kernel_introspect(op)?;
    if kernel.may_read() {
        Ok(kernel)
    } else {
        Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{op}: denied: no `kernel: {{ \"introspect\": [read] }}` grant covers it"
        ))))
    }
}

/// kernel/worklist () - the pending worklist at step start: `[{ event, trait, orbital }]`
pub struct KernelWorklistOp;
impl Operator for KernelWorklistOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "kernel/worklist";
        require_arity!(args, OP, == 0);
        Ok(readable(ctx, OP)?.worklist())
    }
}

/// kernel/frame () - the step's own position: `{ trait, state, event, entityId }`
pub struct KernelFrameOp;
impl Operator for KernelFrameOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "kernel/frame";
        require_arity!(args, OP, == 0);
        Ok(readable(ctx, OP)?.frame())
    }
}

/// kernel/stamp () - `{ now, dispatch, step, journal }`
pub struct KernelStampOp;
impl Operator for KernelStampOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "kernel/stamp";
        require_arity!(args, OP, == 0);
        Ok(readable(ctx, OP)?.stamp())
    }
}

fn refuse(message: String) -> OrbitalError {
    OrbitalError::Eval(EvalError::InvalidArgument(message))
}

/// kernel/apply (name, args) - the pure operator `name` over the list `args`. Refused: a `kernel/*` name, a name the
/// grant does not cover, an unknown operator, an operator with side effects or a host. Arity and kind errors are the
/// operator's own, raised by the same call path any written call takes.
pub struct KernelApplyOp;
impl Operator for KernelApplyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "kernel/apply";
        require_arity!(args, OP, == 2);
        let introspect = ctx.kernel_introspect(OP)?;
        let name_value = eval_expr(&args[0], ctx)?;
        let name = name_value.text()?;
        let operands = require_array!(eval_expr(&args[1], ctx)?);

        if name.starts_with("kernel/") {
            return Err(refuse(format!("{OP}: {name} is a kernel operator and cannot be applied")));
        }
        if !introspect.may_apply(name) {
            return Err(refuse(format!("{OP}: the kernel grant does not cover {name}")));
        }
        let declared = orbital_core_base::language::registry::lookup_operator(name)
            .map_err(|error| refuse(format!("{OP}: {error}")))?
            .ok_or_else(|| refuse(format!("{OP}: unknown operator {name}")))?;
        if declared.has_side_effects || declared.host.is_some() {
            return Err(refuse(format!("{OP}: {name} is not a pure operator")));
        }

        let mut call = Vec::with_capacity(operands.len() + 1);
        call.push(SExpression::string(name));
        for operand in operands.iter() {
            let literal = SExpression::try_from(operand.clone()).map_err(OrbitalError::Eval)?;
            call.push(quote_expr(&literal));
        }
        eval_node(&SExpression::Array(call.into()), ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::evaluator::context::KernelIntrospect;

    #[derive(Debug)]
    struct Fake {
        allow: bool,
    }

    impl KernelIntrospect for Fake {
        fn worklist(&self) -> Value {
            Value::array(vec![Value::Int(1), Value::Int(2)])
        }

        fn frame(&self) -> Value {
            Value::string("frame".to_string())
        }

        fn stamp(&self) -> Value {
            Value::Int(42)
        }

        fn may_apply(&self, _operator: &str) -> bool {
            self.allow
        }

        fn may_read(&self) -> bool {
            self.allow
        }
    }

    fn ctx_with(allow: bool) -> BindingContext {
        let mut ctx = BindingContext::default();
        ctx.set_kernel_introspect(Arc::new(Fake { allow }));
        ctx
    }

    fn eval_with(form: serde_json::Value, ctx: &BindingContext) -> EvalResult {
        let expr: SExpression = serde_json::from_value(form).expect("s-expression");
        eval_expr(&expr, ctx)
    }

    fn refusal(form: serde_json::Value, ctx: &BindingContext) -> String {
        eval_with(form, ctx).expect_err("refused").to_string()
    }

    #[test]
    fn introspection_answers_the_kernels_values() {
        let ctx = ctx_with(true);
        assert_eq!(eval_with(serde_json::json!(["kernel/worklist"]), &ctx).unwrap(), Value::array(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(eval_with(serde_json::json!(["kernel/frame"]), &ctx).unwrap(), Value::string("frame".to_string()));
        assert_eq!(eval_with(serde_json::json!(["kernel/stamp"]), &ctx).unwrap(), Value::Int(42));
        assert!(eval_with(serde_json::json!(["kernel/stamp", 1]), &ctx).is_err(), "control: takes no arguments");
    }

    #[test]
    fn introspection_outside_a_kernel_step_names_the_operator() {
        let message = refusal(serde_json::json!(["kernel/stamp"]), &BindingContext::default());
        assert!(message.contains("kernel/stamp"), "{message}");
        let message = refusal(serde_json::json!(["kernel/apply", "+", ["list", 1, 2]]), &BindingContext::default());
        assert!(message.contains("kernel/apply"), "{message}");
    }

    #[test]
    fn apply_calls_a_granted_pure_operator() {
        let ctx = ctx_with(true);
        let got = eval_with(serde_json::json!(["kernel/apply", "+", ["list", 2, 3]]), &ctx).unwrap();
        assert_eq!(got.number().unwrap(), 5.0);
    }

    #[test]
    fn apply_refuses_what_the_grant_does_not_cover() {
        let message = refusal(serde_json::json!(["kernel/apply", "+", ["list", 2, 3]]), &ctx_with(false));
        assert!(message.contains("grant"), "{message}");
    }

    #[test]
    fn apply_refuses_impure_unknown_and_kernel_operators() {
        let ctx = ctx_with(true);
        let message = refusal(serde_json::json!(["kernel/apply", "os/read-file", ["list", "/etc/hosts"]]), &ctx);
        assert!(message.contains("not a pure operator"), "{message}");
        let message = refusal(serde_json::json!(["kernel/apply", "math/no-such-operator", ["list"]]), &ctx);
        assert!(message.contains("unknown operator"), "{message}");
        let message = refusal(serde_json::json!(["kernel/apply", "kernel/apply", ["list", "+", ["list", 1, 2]]]), &ctx);
        assert!(message.contains("cannot be applied"), "{message}");
        let message = refusal(serde_json::json!(["kernel/apply", "kernel/stamp", ["list"]]), &ctx);
        assert!(message.contains("cannot be applied"), "{message}");
    }

    #[test]
    fn apply_leaves_arity_and_kind_errors_to_the_operator() {
        let ctx = ctx_with(true);
        assert!(eval_with(serde_json::json!(["kernel/apply", "math/abs", ["list", "x"]]), &ctx).is_err());
        assert!(eval_with(serde_json::json!(["kernel/apply", "+", 5]), &ctx).is_err(), "control: args must be a list");
    }

    #[test]
    fn apply_does_not_evaluate_its_operands_again() {
        let mut ctx = ctx_with(true);
        ctx.set_local("x", Value::Int(9));
        ctx.set_local("y", Value::string("@x".to_string()));
        let got = eval_with(serde_json::json!(["kernel/apply", "str/concat", ["list", "@y", "a"]]), &ctx).unwrap();
        assert_eq!(got, Value::string("@xa".to_string()));
        let once = eval_with(serde_json::json!(["kernel/apply", "str/concat", ["list", "@x", "a"]]), &ctx).unwrap();
        assert_eq!(once, Value::string("9a".to_string()), "control: the list argument itself is evaluated once");
    }
}
