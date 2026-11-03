//! `mem/*` pure reads. They see the kernel's memory through [`KernelMem`]; a handle must be a live sealed value.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::bytes::{big_endian_arg, decode_int, decode_uint, width_arg};
use super::macros::*;
use super::*;

fn handle_arg(arg: &SExpression, ctx: &BindingContext, kind: &str) -> Result<Value, OrbitalError> {
    match eval_expr(arg, ctx)? {
        handle @ Value::Sealed(_) => Ok(handle),
        other => Err(other.kind_error(kind).into()),
    }
}

fn int_arg(arg: &SExpression, ctx: &BindingContext) -> Result<i64, OrbitalError> {
    Ok(eval_expr(arg, ctx)?.int()?)
}

/// mem/read (borrow, offset, length) - `length` bytes at `offset` of the block a live borrow holds
pub struct MemReadOp;
impl Operator for MemReadOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "mem/read";
        require_arity!(args, OP, == 3);
        let borrow = handle_arg(&args[0], ctx, "Borrow")?;
        let offset = int_arg(&args[1], ctx)?;
        let length = int_arg(&args[2], ctx)?;
        let data = ctx.kernel_mem(OP)?.read(&borrow, offset, length).map_err(OrbitalError::Eval)?;
        Ok(Value::bytes(data))
    }
}

/// mem/read-uint (borrow, offset, width, endian) - the unsigned integer in width bytes at offset; a width-8 value
/// above i64::MAX answers null
pub struct MemReadUintOp;
impl Operator for MemReadUintOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        read_field(args, ctx, "mem/read-uint", decode_uint)
    }
}

/// mem/read-int (borrow, offset, width, endian) - the two's-complement signed integer in width bytes at offset
pub struct MemReadIntOp;
impl Operator for MemReadIntOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        read_field(args, ctx, "mem/read-int", decode_int)
    }
}

fn read_field(args: &[SExpression], ctx: &BindingContext, op: &str, decode: fn(&[u8], bool) -> Value) -> EvalResult {
    require_arity!(args, op, == 4);
    let borrow = handle_arg(&args[0], ctx, "Borrow")?;
    let offset = int_arg(&args[1], ctx)?;
    let (Some(width), Some(big)) = (width_arg(&args[2], ctx)?, big_endian_arg(&args[3], ctx)?) else {
        return Ok(Value::Null);
    };
    let field = ctx.kernel_mem(op)?.read(&borrow, offset, width as i64).map_err(OrbitalError::Eval)?;
    Ok(decode(&field, big))
}

/// mem/length (handle) - the length in bytes of a live block, or of the block a live borrow holds
pub struct MemLengthOp;
impl Operator for MemLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "mem/length";
        require_arity!(args, OP, == 1);
        let handle = handle_arg(&args[0], ctx, "Block or Borrow")?;
        let length = ctx.kernel_mem(OP)?.length(&handle).map_err(OrbitalError::Eval)?;
        Ok(Value::Int(length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::evaluator::context::KernelMem;
    use orbital_core_base::value::{value_map_with_capacity, ValueMap};

    #[derive(Debug)]
    struct Fake(Vec<u8>);

    impl KernelMem for Fake {
        fn read(&self, borrow: &Value, offset: i64, length: i64) -> Result<Vec<u8>, EvalError> {
            if !borrow.is_sealed_as("Borrow") {
                return Err(borrow.kind_error("Borrow"));
            }
            let start = usize::try_from(offset).map_err(|_| EvalError::InvalidArgument("offset".into()))?;
            let len = usize::try_from(length).map_err(|_| EvalError::InvalidArgument("length".into()))?;
            self.0
                .get(start..start + len)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| EvalError::InvalidArgument("out of bounds".into()))
        }

        fn length(&self, handle: &Value) -> Result<i64, EvalError> {
            if handle.is_sealed_as("Borrow") || handle.is_sealed_as("Block") {
                Ok(self.0.len() as i64)
            } else {
                Err(handle.kind_error("Block or Borrow"))
            }
        }
    }

    fn sealed(kind: &'static str) -> Value {
        let mut fields: ValueMap = value_map_with_capacity(1);
        fields.insert("id".to_string().into(), Value::Int(1));
        Value::sealed(kind, fields)
    }

    fn ctx_with(data: Vec<u8>) -> BindingContext {
        let mut ctx = BindingContext::default();
        ctx.set_kernel_mem(Arc::new(Fake(data)));
        ctx.set_local("b", sealed("Borrow"));
        ctx.set_local("k", sealed("Block"));
        ctx
    }

    fn eval_with(form: serde_json::Value, ctx: &BindingContext) -> EvalResult {
        let expr: SExpression = serde_json::from_value(form).expect("s-expression");
        eval_expr(&expr, ctx)
    }

    #[test]
    fn read_answers_bytes_and_control_out_of_bounds_fails() {
        let ctx = ctx_with(vec![1, 2, 3, 4]);
        assert_eq!(eval_with(serde_json::json!(["mem/read", "@b", 1, 2]), &ctx).unwrap(), Value::bytes(vec![2, 3]));
        assert!(eval_with(serde_json::json!(["mem/read", "@b", 3, 5]), &ctx).is_err(), "control: past the end");
    }

    #[test]
    fn read_uint_decodes_both_endians_and_control_bad_shape_fails() {
        let ctx = ctx_with(vec![0x01, 0x02, 0x03, 0x04]);
        assert_eq!(eval_with(serde_json::json!(["mem/read-uint", "@b", 0, 4, "be"]), &ctx).unwrap(), Value::Int(0x01020304));
        assert_eq!(eval_with(serde_json::json!(["mem/read-uint", "@b", 0, 4, "le"]), &ctx).unwrap(), Value::Int(0x04030201));
        assert_eq!(eval_with(serde_json::json!(["mem/read-uint", "@b", 1, 1, "be"]), &ctx).unwrap(), Value::Int(2));
        assert_eq!(eval_with(serde_json::json!(["mem/read-uint", "@b", 0, 3, "be"]), &ctx).unwrap(), Value::Null, "control: width 3, as bytes/read-uint");
        assert_eq!(eval_with(serde_json::json!(["mem/read-uint", "@b", 0, 4, "xx"]), &ctx).unwrap(), Value::Null, "control: endian, as bytes/read-uint");
    }

    #[test]
    fn read_uint_width_eight_above_i64_max_is_null() {
        let ctx = ctx_with(vec![0xff; 8]);
        assert_eq!(eval_with(serde_json::json!(["mem/read-uint", "@b", 0, 8, "be"]), &ctx).unwrap(), Value::Null);
        let low = ctx_with(vec![0, 0, 0, 0, 0, 0, 1, 0]);
        assert_eq!(eval_with(serde_json::json!(["mem/read-uint", "@b", 0, 8, "be"]), &low).unwrap(), Value::Int(256));
    }

    #[test]
    fn length_answers_for_block_and_borrow() {
        let ctx = ctx_with(vec![9; 6]);
        assert_eq!(eval_with(serde_json::json!(["mem/length", "@k"]), &ctx).unwrap(), Value::Int(6));
        assert_eq!(eval_with(serde_json::json!(["mem/length", "@b"]), &ctx).unwrap(), Value::Int(6));
        let mut other = ctx_with(vec![]);
        other.set_local("a", sealed("Arena"));
        assert!(eval_with(serde_json::json!(["mem/length", "@a"]), &other).is_err(), "control: Arena is not a handle");
    }

    #[test]
    fn no_kernel_memory_names_the_operator() {
        let mut ctx = BindingContext::default();
        ctx.set_local("b", sealed("Borrow"));
        for (form, op) in [
            (serde_json::json!(["mem/read", "@b", 0, 1]), "mem/read"),
            (serde_json::json!(["mem/read-uint", "@b", 0, 1, "be"]), "mem/read-uint"),
            (serde_json::json!(["mem/length", "@b"]), "mem/length"),
        ] {
            let err = eval_with(form, &ctx).unwrap_err().to_string();
            assert!(err.contains(op), "{err}");
        }
    }

    #[test]
    fn non_sealed_handle_is_a_type_error() {
        let ctx = ctx_with(vec![1, 2]);
        for form in [
            serde_json::json!(["mem/read", 5, 0, 1]),
            serde_json::json!(["mem/read-uint", "x", 0, 1, "be"]),
            serde_json::json!(["mem/length", 5]),
        ] {
            assert!(matches!(
                eval_with(form, &ctx),
                Err(OrbitalError::Eval(EvalError::TypeMismatch { .. }))
            ));
        }
    }
}
