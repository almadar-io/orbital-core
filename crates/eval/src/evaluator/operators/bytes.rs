//! `bytes/*` operators. A bytes value is a standard padded base64 string at run time.

use super::macros::*;
use super::*;
use orbital_core_base::codec;

fn invalid(op: &str, detail: impl std::fmt::Display) -> OrbitalError {
    OrbitalError::Eval(EvalError::InvalidArgument(format!(
        "{op}: invalid {detail}"
    )))
}

fn decode_value(value: Value, op: &str) -> Result<Vec<u8>, OrbitalError> {
    match value {
        Value::String(s) => {
            codec::base64_decode(&s).map_err(|e| invalid(op, format!("base64 ({e})")))
        }
        other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{op}: expected a bytes argument, got {}",
            other.type_name()
        )))),
    }
}

fn bytes_arg(arg: &SExpression, ctx: &BindingContext, op: &str) -> Result<Vec<u8>, OrbitalError> {
    decode_value(eval_expr(arg, ctx)?, op)
}

fn index(len: usize, at: f64) -> usize {
    if at < 0.0 {
        (len as i64 + at as i64).max(0) as usize
    } else {
        (at as usize).min(len)
    }
}

/// bytes/from-string - UTF-8 bytes of the text
pub struct BytesFromStringOp;
impl Operator for BytesFromStringOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/from-string", == 1);
        match eval_expr(&args[0], ctx)? {
            Value::String(s) => Ok(Value::String(codec::base64_encode(s.as_bytes()))),
            other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "bytes/from-string: expected a string argument, got {}",
                other.type_name()
            )))),
        }
    }
}

/// bytes/to-string - decode as UTF-8; anything else is an `invalid` error
pub struct BytesToStringOp;
impl Operator for BytesToStringOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/to-string", == 1);
        let data = bytes_arg(&args[0], ctx, "bytes/to-string")?;
        String::from_utf8(data)
            .map(Value::String)
            .map_err(|e| invalid("bytes/to-string", format!("UTF-8 ({e})")))
    }
}

/// bytes/length - decoded byte count
pub struct BytesLengthOp;
impl Operator for BytesLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/length", == 1);
        let data = bytes_arg(&args[0], ctx, "bytes/length")?;
        Ok(Value::Number(data.len() as f64))
    }
}

/// bytes/slice (data, start, end?) - byte indexes; negatives count from the end, out-of-range clamps
pub struct BytesSliceOp;
impl Operator for BytesSliceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/slice", >= 2);
        if args.len() > 3 {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "bytes/slice".to_string(),
                expected: 3,
                actual: args.len(),
            }));
        }
        let data = bytes_arg(&args[0], ctx, "bytes/slice")?;
        let start = require_number!(eval_expr(&args[1], ctx)?);
        let from = index(data.len(), start);
        let to = if args.len() == 3 {
            match eval_expr(&args[2], ctx)? {
                Value::Null => data.len(),
                other => index(data.len(), require_number!(other)),
            }
        } else {
            data.len()
        };
        if from >= to {
            return Ok(Value::String(String::new()));
        }
        Ok(Value::String(codec::base64_encode(&data[from..to])))
    }
}

/// bytes/concat (parts) - join a list of bytes values in order
pub struct BytesConcatOp;
impl Operator for BytesConcatOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/concat", == 1);
        let parts = require_array!(eval_expr(&args[0], ctx)?);
        let mut joined = Vec::new();
        for part in parts {
            joined.extend(decode_value(part, "bytes/concat")?);
        }
        Ok(Value::String(codec::base64_encode(&joined)))
    }
}

/// bytes/equals - same decoded bytes
pub struct BytesEqualsOp;
impl Operator for BytesEqualsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/equals", == 2);
        let a = bytes_arg(&args[0], ctx, "bytes/equals")?;
        let b = bytes_arg(&args[1], ctx, "bytes/equals")?;
        Ok(Value::Bool(a == b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(form: serde_json::Value) -> EvalResult {
        let expr: SExpression = serde_json::from_value(form).expect("s-expression");
        eval_expr(&expr, &BindingContext::default())
    }

    fn text(form: serde_json::Value) -> String {
        match eval(form) {
            Ok(Value::String(s)) => s,
            other => panic!("expected a string, got {other:?}"),
        }
    }

    fn fails_invalid(form: serde_json::Value) {
        match eval(form) {
            Err(e) => assert!(e.to_string().contains("invalid"), "{e}"),
            Ok(v) => panic!("expected an error, got {v:?}"),
        }
    }

    #[test]
    fn from_string_encodes_utf8() {
        assert_eq!(
            text(serde_json::json!(["bytes/from-string", "hello"])),
            "aGVsbG8="
        );
        assert_eq!(text(serde_json::json!(["bytes/from-string", ""])), "");
        assert_eq!(text(serde_json::json!(["bytes/from-string", "é"])), "w6k=");
    }

    #[test]
    fn to_string_round_trips_non_ascii_and_empty() {
        assert_eq!(
            text(serde_json::json!(["bytes/to-string", "aGVsbG8="])),
            "hello"
        );
        assert_eq!(text(serde_json::json!(["bytes/to-string", ""])), "");
        assert_eq!(
            text(serde_json::json!([
                "bytes/to-string",
                ["bytes/from-string", "héllo 世界"]
            ])),
            "héllo 世界"
        );
    }

    #[test]
    fn to_string_rejects_bad_base64_and_bad_utf8() {
        fails_invalid(serde_json::json!(["bytes/to-string", "!!notbase64"]));
        fails_invalid(serde_json::json!(["bytes/to-string", "/w=="]));
    }

    #[test]
    fn length_counts_bytes_not_base64_chars() {
        assert_eq!(
            eval(serde_json::json!(["bytes/length", "aGVsbG8="])).unwrap(),
            Value::Number(5.0)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/length", ""])).unwrap(),
            Value::Number(0.0)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/length", "w6k="])).unwrap(),
            Value::Number(2.0)
        );
        fails_invalid(serde_json::json!(["bytes/length", "***"]));
    }

    #[test]
    fn slice_uses_byte_indexes() {
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", 1, 3])),
            "ZWw="
        );
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", 1])),
            "ZWxsbw=="
        );
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", 1, null])),
            "ZWxsbw=="
        );
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", 0, 5])),
            "aGVsbG8="
        );
    }

    #[test]
    fn slice_edges_clamp_and_negatives_count_from_the_end() {
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", 3, 1])),
            ""
        );
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", 2, 2])),
            ""
        );
        assert_eq!(text(serde_json::json!(["bytes/slice", "aGVsbG8=", 9])), "");
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", 3, 99])),
            "bG8="
        );
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", -2])),
            "bG8="
        );
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", 0, -1])),
            "aGVsbA=="
        );
        assert_eq!(
            text(serde_json::json!(["bytes/slice", "aGVsbG8=", -99, 2])),
            "aGU="
        );
        assert_eq!(text(serde_json::json!(["bytes/slice", "", 0, 3])), "");
        fails_invalid(serde_json::json!(["bytes/slice", "%%", 0, 1]));
    }

    #[test]
    fn concat_joins_in_order() {
        assert_eq!(
            text(serde_json::json!([
                "bytes/concat",
                ["list", "aGk=", "IQ=="]
            ])),
            "aGkh"
        );
        assert_eq!(text(serde_json::json!(["bytes/concat", ["list"]])), "");
        assert_eq!(
            text(serde_json::json!(["bytes/concat", ["list", "", "aGk="]])),
            "aGk="
        );
        fails_invalid(serde_json::json!(["bytes/concat", ["list", "aGk=", "%%"]]));
    }

    #[test]
    fn equals_compares_decoded_bytes() {
        assert_eq!(
            eval(serde_json::json!(["bytes/equals", "aGk=", "aGk="])).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/equals", "aGk=", "aGkh"])).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/equals", "", ""])).unwrap(),
            Value::Bool(true)
        );
        fails_invalid(serde_json::json!(["bytes/equals", "aGk=", "%%"]));
    }
}
