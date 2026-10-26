//! `bytes/*` operators. A bytes value is a standard padded base64 string at run time.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

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

/// The bytes a base64 argument encodes, or `None` when the text is not standard base64; a non-text argument is an error.
fn bytes_arg_or_none(arg: &SExpression, ctx: &BindingContext, op: &str) -> Result<Option<Vec<u8>>, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::String(s) => Ok(codec::base64_decode(&s).ok()),
        other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{op}: expected a bytes argument, got {}",
            other.type_name()
        )))),
    }
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
            Value::String(s) => Ok(Value::string(codec::base64_encode(s.as_bytes()))),
            other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "bytes/from-string: expected a string argument, got {}",
                other.type_name()
            )))),
        }
    }
}

/// bytes/to-string - decode as UTF-8; bytes that are not UTF-8 answer null
pub struct BytesToStringOp;
impl Operator for BytesToStringOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/to-string", == 1);
        let Some(data) = bytes_arg_or_none(&args[0], ctx, "bytes/to-string")? else {
            return Ok(Value::Null);
        };
        Ok(String::from_utf8(data).map_or(Value::Null, Value::string))
    }
}

/// bytes/from-base64 - standard padded base64 text as the bytes it encodes; other text answers null
pub struct BytesFromBase64Op;
impl Operator for BytesFromBase64Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/from-base64", == 1);
        Ok(bytes_arg_or_none(&args[0], ctx, "bytes/from-base64")?
            .map_or(Value::Null, |data| Value::string(codec::base64_encode(&data))))
    }
}

/// bytes/to-base64 - the bytes as their standard padded base64 text
pub struct BytesToBase64Op;
impl Operator for BytesToBase64Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/to-base64", == 1);
        let data = bytes_arg(&args[0], ctx, "bytes/to-base64")?;
        Ok(Value::string(codec::base64_encode(&data)))
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
            return Ok(Value::string(String::new()));
        }
        Ok(Value::string(codec::base64_encode(&data[from..to])))
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
        Ok(Value::string(codec::base64_encode(&joined)))
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

/// A whole-number argument; a number that is not whole is `None`, a non-number is an `invalid` error naming what it is for.
fn whole(arg: &SExpression, ctx: &BindingContext, op: &str, what: &str) -> Result<Option<i64>, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::Number(n) if n.is_finite() && n.fract() == 0.0 && n.abs() <= MAX_SAFE => Ok(Some(n as i64)),
        Value::Number(_) => Ok(None),
        other => Err(invalid(op, format!("{what} {}: expected a whole number", other.to_text()))),
    }
}

/// The largest integer a number holds exactly, 2^53 - 1.
const MAX_SAFE: f64 = 9_007_199_254_740_991.0;

fn width_arg(arg: &SExpression, ctx: &BindingContext, op: &str) -> Result<Option<usize>, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::Number(n) if n == 1.0 || n == 2.0 || n == 4.0 || n == 8.0 => Ok(Some(n as usize)),
        Value::Number(_) => Ok(None),
        other => Err(invalid(op, format!("width {}: expected 1, 2, 4 or 8", other.to_text()))),
    }
}

fn big_endian_arg(arg: &SExpression, ctx: &BindingContext, op: &str) -> Result<Option<bool>, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::String(s) if s == "be" => Ok(Some(true)),
        Value::String(s) if s == "le" => Ok(Some(false)),
        Value::String(_) => Ok(None),
        other => Err(invalid(op, format!("endian {}: expected \"be\" or \"le\"", other.to_text()))),
    }
}

/// bytes/index-of (data, needle, from?) - first byte offset of needle at or after from (negatives count from the end), else -1
pub struct BytesIndexOfOp;
impl Operator for BytesIndexOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/index-of", >= 2);
        if args.len() > 3 {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "bytes/index-of".to_string(),
                expected: 3,
                actual: args.len(),
            }));
        }
        let data = bytes_arg(&args[0], ctx, "bytes/index-of")?;
        let needle = bytes_arg(&args[1], ctx, "bytes/index-of")?;
        let from = if args.len() == 3 {
            match eval_expr(&args[2], ctx)? {
                Value::Null => 0,
                other => index(data.len(), require_number!(other)),
            }
        } else {
            0
        };
        if needle.is_empty() {
            return Ok(Value::Number(from as f64));
        }
        let found = data
            .get(from..)
            .and_then(|tail| tail.windows(needle.len()).position(|w| w == needle.as_slice()));
        Ok(Value::Number(found.map_or(-1.0, |at| (from + at) as f64)))
    }
}

/// bytes/at (data, i) - the byte at i as 0-255; null when i is outside the data
pub struct BytesAtOp;
impl Operator for BytesAtOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/at", == 2);
        let data = bytes_arg(&args[0], ctx, "bytes/at")?;
        let Some(at) = whole(&args[1], ctx, "bytes/at", "index")? else {
            return Ok(Value::Null);
        };
        Ok(usize::try_from(at)
            .ok()
            .and_then(|i| data.get(i))
            .map_or(Value::Null, |b| Value::Number(f64::from(*b))))
    }
}

/// bytes/read-uint (data, offset, width, endian) - the unsigned integer in width bytes at offset; null when they run
/// past the data. A width-8 value above 2^53 - 1 answers null, never rounded.
pub struct BytesReadUintOp;
impl Operator for BytesReadUintOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "bytes/read-uint";
        require_arity!(args, OP, == 4);
        let data = bytes_arg(&args[0], ctx, OP)?;
        let (Some(offset), Some(width), Some(big)) = (
            whole(&args[1], ctx, OP, "offset")?,
            width_arg(&args[2], ctx, OP)?,
            big_endian_arg(&args[3], ctx, OP)?,
        ) else {
            return Ok(Value::Null);
        };
        let Some(field) = usize::try_from(offset)
            .ok()
            .and_then(|start| data.get(start..start.checked_add(width)?))
        else {
            return Ok(Value::Null);
        };
        let fold = |acc: u64, b: &u8| (acc << 8) | u64::from(*b);
        let value = if big {
            field.iter().fold(0, fold)
        } else {
            field.iter().rev().fold(0, fold)
        };
        if value as f64 > MAX_SAFE {
            return Ok(Value::Null);
        }
        Ok(Value::Number(value as f64))
    }
}

/// bytes/write-uint (value, width, endian) - value as width bytes; a value the width (or a number) cannot hold exactly answers null
pub struct BytesWriteUintOp;
impl Operator for BytesWriteUintOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "bytes/write-uint";
        require_arity!(args, OP, == 3);
        let (Some(value), Some(width), Some(big)) = (
            whole(&args[0], ctx, OP, "value")?,
            width_arg(&args[1], ctx, OP)?,
            big_endian_arg(&args[2], ctx, OP)?,
        ) else {
            return Ok(Value::Null);
        };
        let fits = value >= 0 && (width == 8 || value < 1i64 << (8 * width));
        if !fits {
            return Ok(Value::Null);
        }
        let all = (value as u64).to_be_bytes();
        let mut field = all[8 - width..].to_vec();
        if !big {
            field.reverse();
        }
        Ok(Value::string(codec::base64_encode(&field)))
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
            Ok(Value::String(s)) => s.into_inner(),
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
    fn to_string_answers_null_for_bad_base64_and_bad_utf8() {
        assert_eq!(eval(serde_json::json!(["bytes/to-string", "!!notbase64"])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/to-string", "/w=="])).unwrap(), Value::Null);
        assert!(eval(serde_json::json!(["bytes/to-string", 5])).is_err(), "control: a number is a type error, not a bad value");
    }

    #[test]
    fn base64_text_and_bytes_convert_both_ways_unchanged() {
        assert_eq!(text(serde_json::json!(["bytes/from-base64", "aGVsbG8="])), "aGVsbG8=");
        assert_eq!(text(serde_json::json!(["bytes/to-base64", ["bytes/from-string", "hello"]])), "aGVsbG8=");
        assert_eq!(text(serde_json::json!(["bytes/from-base64", ""])), "");
        assert_eq!(
            text(serde_json::json!(["bytes/to-string", ["bytes/from-base64", ["bytes/to-base64", ["bytes/from-string", "é"]]]])),
            "é"
        );
    }

    #[test]
    fn from_base64_answers_null_for_text_that_is_not_standard_base64() {
        assert_eq!(eval(serde_json::json!(["bytes/from-base64", "!!notbase64"])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/from-base64", "aGVsbG8"])).unwrap(), Value::Null);
        assert!(eval(serde_json::json!(["bytes/from-base64", 5])).is_err(), "control: a number is a type error, not a bad value");
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

    fn b64(bytes: &[u8]) -> String {
        codec::base64_encode(bytes)
    }

    #[test]
    fn index_of_finds_the_first_offset_from_the_start_or_from() {
        let hello = b64(b"hello");
        let at = |form: serde_json::Value| eval(form).unwrap();
        assert_eq!(at(serde_json::json!(["bytes/index-of", hello, b64(b"l")])), Value::Number(2.0));
        assert_eq!(at(serde_json::json!(["bytes/index-of", hello, b64(b"lo")])), Value::Number(3.0));
        assert_eq!(at(serde_json::json!(["bytes/index-of", hello, b64(b"l"), 3])), Value::Number(3.0));
        assert_eq!(at(serde_json::json!(["bytes/index-of", hello, b64(b"l"), -2])), Value::Number(3.0));
        assert_eq!(at(serde_json::json!(["bytes/index-of", hello, b64(b"l"), null])), Value::Number(2.0));
        assert_eq!(at(serde_json::json!(["bytes/index-of", hello, "", 2])), Value::Number(2.0));
        assert_eq!(at(serde_json::json!(["bytes/index-of", b64(b"\r\n+OK\r\n"), b64(b"\r\n"), 1])), Value::Number(5.0));
    }

    #[test]
    fn index_of_answers_minus_one_when_absent_and_control_rejects_bad_bytes() {
        let hello = b64(b"hello");
        assert_eq!(eval(serde_json::json!(["bytes/index-of", hello, b64(b"z")])).unwrap(), Value::Number(-1.0));
        assert_eq!(eval(serde_json::json!(["bytes/index-of", hello, b64(b"l"), 4])).unwrap(), Value::Number(-1.0));
        assert_eq!(eval(serde_json::json!(["bytes/index-of", hello, b64(b"hello!")])).unwrap(), Value::Number(-1.0));
        assert_eq!(eval(serde_json::json!(["bytes/index-of", "", b64(b"a")])).unwrap(), Value::Number(-1.0));
        fails_invalid(serde_json::json!(["bytes/index-of", "%%", b64(b"a")]));
        fails_invalid(serde_json::json!(["bytes/index-of", hello, "%%"]));
    }

    #[test]
    fn at_answers_the_byte_value_or_null_out_of_range() {
        let data = b64(&[0, 1, 2, 255]);
        assert_eq!(eval(serde_json::json!(["bytes/at", data, 0])).unwrap(), Value::Number(0.0));
        assert_eq!(eval(serde_json::json!(["bytes/at", data, 3])).unwrap(), Value::Number(255.0));
        assert_eq!(eval(serde_json::json!(["bytes/at", data, 4])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/at", data, -1])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/at", "", 0])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/at", data, 1.5])).unwrap(), Value::Null);
        fails_invalid(serde_json::json!(["bytes/at", data, null]));
        fails_invalid(serde_json::json!(["bytes/at", "%%", 0]));
    }

    #[test]
    fn read_uint_reads_big_and_little_endian_at_each_width() {
        let data = b64(&[0x12, 0x34, 0x56, 0x78, 0x9a]);
        let read = |offset: i64, width: i64, endian: &str| {
            eval(serde_json::json!(["bytes/read-uint", data, offset, width, endian])).unwrap()
        };
        assert_eq!(read(0, 1, "be"), Value::Number(0x12 as f64));
        assert_eq!(read(4, 1, "le"), Value::Number(0x9a as f64));
        assert_eq!(read(0, 2, "be"), Value::Number(0x1234 as f64));
        assert_eq!(read(0, 2, "le"), Value::Number(0x3412 as f64));
        assert_eq!(read(1, 4, "be"), Value::Number(0x3456789a_u32 as f64));
        assert_eq!(read(1, 4, "le"), Value::Number(0x9a785634_u32 as f64));
        let wide = b64(&[0, 0x1f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(
            eval(serde_json::json!(["bytes/read-uint", wide, 0, 8, "be"])).unwrap(),
            Value::Number(9_007_199_254_740_991.0)
        );
    }

    #[test]
    fn read_uint_answers_null_past_the_end_and_refuses_bad_shape() {
        let data = b64(&[1, 2, 3]);
        assert_eq!(eval(serde_json::json!(["bytes/read-uint", data, 2, 2, "be"])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/read-uint", data, -1, 1, "be"])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/read-uint", data, 3, 1, "le"])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/read-uint", data, 0, 3, "be"])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/read-uint", data, 0, 1, "middle"])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["bytes/read-uint", data, 0.5, 1, "be"])).unwrap(), Value::Null);
        let too_wide = b64(&[0, 0x20, 0, 0, 0, 0, 0, 0]);
        assert_eq!(eval(serde_json::json!(["bytes/read-uint", too_wide, 0, 8, "be"])).unwrap(), Value::Null);
        fails_invalid(serde_json::json!(["bytes/read-uint", data, null, 1, "be"]));
    }

    #[test]
    fn write_uint_encodes_each_width_and_round_trips_through_read() {
        assert_eq!(text(serde_json::json!(["bytes/write-uint", 0x1234, 2, "be"])), b64(&[0x12, 0x34]));
        assert_eq!(text(serde_json::json!(["bytes/write-uint", 0x1234, 2, "le"])), b64(&[0x34, 0x12]));
        assert_eq!(text(serde_json::json!(["bytes/write-uint", 255, 1, "be"])), b64(&[255]));
        assert_eq!(text(serde_json::json!(["bytes/write-uint", 0, 4, "le"])), b64(&[0, 0, 0, 0]));
        assert_eq!(
            text(serde_json::json!(["bytes/write-uint", 9_007_199_254_740_991_i64, 8, "be"])),
            b64(&[0, 0x1f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/read-uint", ["bytes/write-uint", 305_419_896, 4, "le"], 0, 4, "le"])).unwrap(),
            Value::Number(305_419_896.0)
        );
    }

    #[test]
    fn write_uint_answers_null_for_values_the_width_cannot_hold() {
        let null = |form: serde_json::Value| assert_eq!(eval(form).unwrap(), Value::Null);
        null(serde_json::json!(["bytes/write-uint", 256, 1, "be"]));
        null(serde_json::json!(["bytes/write-uint", -1, 2, "be"]));
        null(serde_json::json!(["bytes/write-uint", 1.5, 2, "be"]));
        null(serde_json::json!(["bytes/write-uint", 65_536, 2, "le"]));
        null(serde_json::json!(["bytes/write-uint", 9_007_199_254_740_992_i64, 8, "be"]));
        null(serde_json::json!(["bytes/write-uint", 1, 3, "be"]));
        null(serde_json::json!(["bytes/write-uint", 1, 2, "big"]));
        fails_invalid(serde_json::json!(["bytes/write-uint", null, 2, "be"]));
    }
}
