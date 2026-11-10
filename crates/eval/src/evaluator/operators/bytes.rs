//! `bytes/*` operators. A bytes value is a `Value::Bytes`; standard padded base64 text reads as bytes too.
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
        Value::Bytes(b) => Ok(b.into_inner()),
        Value::String(s) => {
            codec::base64_decode(&s).map_err(|e| invalid(op, format!("base64 ({e})")))
        }
        other => Err(other.kind_error("bytes").into()),
    }
}

fn bytes_arg(arg: &SExpression, ctx: &BindingContext, op: &str) -> Result<Vec<u8>, OrbitalError> {
    decode_value(eval_expr(arg, ctx)?, op)
}

/// The bytes a bytes argument holds, or `None` when base64 text is not standard base64; any other kind is an error.
fn bytes_arg_or_none(
    arg: &SExpression,
    ctx: &BindingContext,
) -> Result<Option<Vec<u8>>, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::Bytes(b) => Ok(Some(b.into_inner())),
        Value::String(s) => Ok(codec::base64_decode(&s).ok()),
        other => Err(other.kind_error("bytes").into()),
    }
}

/// A byte position clamped into `0..=len`; a negative one counts from the end (the table declares both).
fn index(op: &str, len: usize, at: &Value) -> Result<usize, OrbitalError> {
    let Some(at) = at.as_int() else {
        return if at.is_numeric() {
            Err(invalid(op, format!("index {}: expected a whole number", at.to_string_value())))
        } else {
            Err(at.kind_error("number").into())
        };
    };
    Ok(if at < 0 {
        (len as i64).saturating_add(at).max(0) as usize
    } else {
        usize::try_from(at).map_or(len, |at| at.min(len))
    })
}

/// bytes/from-string - UTF-8 bytes of the text
pub struct BytesFromStringOp;
impl Operator for BytesFromStringOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/from-string", == 1);
        match eval_expr(&args[0], ctx)? {
            Value::String(s) => Ok(Value::bytes(s.as_bytes().to_vec())),
            other => Err(other.kind_error("string").into()),
        }
    }
}

/// bytes/to-string - decode as UTF-8; bytes that are not UTF-8 answer null
pub struct BytesToStringOp;
impl Operator for BytesToStringOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/to-string", == 1);
        let Some(data) = bytes_arg_or_none(&args[0], ctx)? else {
            return Ok(Value::Null);
        };
        Ok(String::from_utf8(data).map_or(Value::Null, Value::string))
    }
}

/// bytes/valid-utf8 - whether the bytes are well-formed UTF-8
pub struct BytesValidUtf8Op;
impl Operator for BytesValidUtf8Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/valid-utf8", == 1);
        let data = bytes_arg(&args[0], ctx, "bytes/valid-utf8")?;
        Ok(Value::Bool(std::str::from_utf8(&data).is_ok()))
    }
}

/// bytes/from-base64 - standard padded base64 text as the bytes it encodes; other text answers null
pub struct BytesFromBase64Op;
impl Operator for BytesFromBase64Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/from-base64", == 1);
        match eval_expr(&args[0], ctx)? {
            Value::String(s) => Ok(codec::base64_decode(&s).map_or(Value::Null, Value::bytes)),
            other => Err(other.kind_error("string").into()),
        }
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
        let len = match eval_expr(&args[0], ctx)? {
            Value::Bytes(b) => b.len(),
            other => decode_value(other, "bytes/length")?.len(),
        };
        Ok(super::util::int_of(len))
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
        let from = index("bytes/slice", data.len(), &eval_expr(&args[1], ctx)?)?;
        let to = if args.len() == 3 {
            match eval_expr(&args[2], ctx)? {
                Value::Null => data.len(),
                other => index("bytes/slice", data.len(), &other)?,
            }
        } else {
            data.len()
        };
        if from >= to {
            return Ok(Value::bytes(Vec::new()));
        }
        Ok(Value::bytes(data[from..to].to_vec()))
    }
}

/// bytes/concat (parts) - join a list of bytes values in order
pub struct BytesConcatOp;
impl Operator for BytesConcatOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/concat", == 1);
        let parts = require_array!(eval_expr(&args[0], ctx)?);
        let mut held = Vec::with_capacity(parts.len());
        for part in parts {
            held.push(match part {
                Value::Bytes(b) => b,
                other => orbital_core_base::value::Shared::new(decode_value(other, "bytes/concat")?),
            });
        }
        let mut joined = Vec::with_capacity(held.iter().map(|b| b.len()).sum());
        for part in &held {
            joined.extend_from_slice(part);
        }
        Ok(Value::bytes(joined))
    }
}

/// bytes/xor (data, key) - data XOR key repeated cyclically; an empty key answers null
pub struct BytesXorOp;
impl Operator for BytesXorOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/xor", == 2);
        let mut data = bytes_arg(&args[0], ctx, "bytes/xor")?;
        let key = bytes_arg(&args[1], ctx, "bytes/xor")?;
        if key.is_empty() {
            return Ok(Value::Null);
        }
        for (byte, k) in data.iter_mut().zip(key.iter().cycle()) {
            *byte ^= k;
        }
        Ok(Value::bytes(data))
    }
}

/// bytes/equals - same decoded bytes, compared in constant time (the one `constant_time_eq_bytes`)
pub struct BytesEqualsOp;
impl Operator for BytesEqualsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/equals", == 2);
        let a = bytes_arg(&args[0], ctx, "bytes/equals")?;
        let b = bytes_arg(&args[1], ctx, "bytes/equals")?;
        Ok(Value::Bool(orbital_core_base::inbound::constant_time_eq_bytes(&a, &b)))
    }
}

/// A whole-number argument; a number that is not whole is `None`, a non-number is a type mismatch.
fn whole(arg: &SExpression, ctx: &BindingContext) -> Result<Option<i64>, OrbitalError> {
    let value = eval_expr(arg, ctx)?;
    if value.is_numeric() {
        Ok(value.as_int())
    } else {
        Err(value.kind_error("number").into())
    }
}

pub(super) fn width_arg(arg: &SExpression, ctx: &BindingContext) -> Result<Option<usize>, OrbitalError> {
    Ok(whole(arg, ctx)?.filter(|n| matches!(n, 1 | 2 | 4 | 8)).map(|n| n as usize))
}

pub(super) fn big_endian_arg(arg: &SExpression, ctx: &BindingContext) -> Result<Option<bool>, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::String(s) if s == "be" => Ok(Some(true)),
        Value::String(s) if s == "le" => Ok(Some(false)),
        Value::String(_) => Ok(None),
        other => Err(other.kind_error("string").into()),
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
                other => index("bytes/index-of", data.len(), &other)?,
            }
        } else {
            0
        };
        if needle.is_empty() {
            return Ok(super::util::int_of(from));
        }
        let found = data.get(from..).and_then(|tail| {
            tail.windows(needle.len())
                .position(|w| w == needle.as_slice())
        });
        Ok(found.map_or(Value::Int(-1), |at| super::util::int_of(from + at)))
    }
}

/// bytes/at (data, i) - the byte at i as 0-255; null when i is outside the data
pub struct BytesAtOp;
impl Operator for BytesAtOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bytes/at", == 2);
        let data = bytes_arg(&args[0], ctx, "bytes/at")?;
        let Some(at) = whole(&args[1], ctx)? else {
            return Ok(Value::Null);
        };
        Ok(usize::try_from(at)
            .ok()
            .and_then(|i| data.get(i))
            .map_or(Value::Null, |b| Value::Int(i64::from(*b))))
    }
}

/// bytes/read-uint (data, offset, width, endian) - the unsigned integer in width bytes at offset; null when they run
/// past the data. A width-8 value above i64::MAX answers null.
pub struct BytesReadUintOp;
impl Operator for BytesReadUintOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        read_field(args, ctx, "bytes/read-uint", decode_uint)
    }
}

/// bytes/read-int (data, offset, width, endian) - the two's-complement signed integer in width bytes at offset; null
/// when they run past the data
pub struct BytesReadIntOp;
impl Operator for BytesReadIntOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        read_field(args, ctx, "bytes/read-int", decode_int)
    }
}

fn read_field(args: &[SExpression], ctx: &BindingContext, op: &str, decode: fn(&[u8], bool) -> Value) -> EvalResult {
    require_arity!(args, op, == 4);
    let data = bytes_arg(&args[0], ctx, op)?;
    let (Some(offset), Some(width), Some(big)) = (
        whole(&args[1], ctx)?,
        width_arg(&args[2], ctx)?,
        big_endian_arg(&args[3], ctx)?,
    ) else {
        return Ok(Value::Null);
    };
    let Some(field) = usize::try_from(offset)
        .ok()
        .and_then(|start| data.get(start..start.checked_add(width)?))
    else {
        return Ok(Value::Null);
    };
    Ok(decode(field, big))
}

/// The bits of `field` (at most 8 bytes) in the given byte order.
fn fold_field(field: &[u8], big: bool) -> u64 {
    let fold = |acc: u64, b: &u8| (acc << 8) | u64::from(*b);
    if big {
        field.iter().fold(0, fold)
    } else {
        field.iter().rev().fold(0, fold)
    }
}

/// The unsigned integer in `field` (at most 8 bytes); a value above i64::MAX answers null.
pub(super) fn decode_uint(field: &[u8], big: bool) -> Value {
    i64::try_from(fold_field(field, big)).map_or(Value::Null, Value::Int)
}

/// The two's-complement signed integer in `field` (1 to 8 bytes), sign-extended from its top bit.
pub(super) fn decode_int(field: &[u8], big: bool) -> Value {
    let shift = 64 - 8 * field.len() as u32;
    Value::Int(((fold_field(field, big) << shift) as i64) >> shift)
}

/// bytes/write-uint (value, width, endian) - value as width bytes; a value the width cannot hold answers null
pub struct BytesWriteUintOp;
impl Operator for BytesWriteUintOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "bytes/write-uint";
        require_arity!(args, OP, == 3);
        let (Some(value), Some(width), Some(big)) = (
            whole(&args[0], ctx)?,
            width_arg(&args[1], ctx)?,
            big_endian_arg(&args[2], ctx)?,
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
        Ok(Value::bytes(field))
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

    fn wire(form: serde_json::Value) -> String {
        match eval(form) {
            Ok(Value::Bytes(b)) => codec::base64_encode(&b),
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    fn fails_kind(form: serde_json::Value) {
        match eval(form) {
            Err(e) => assert!(e.to_string().contains("Type mismatch"), "{e}"),
            Ok(v) => panic!("expected an error, got {v:?}"),
        }
    }

    fn fails_invalid(form: serde_json::Value) {
        match eval(form) {
            Err(e) => assert!(e.to_string().contains("invalid"), "{e}"),
            Ok(v) => panic!("expected an error, got {v:?}"),
        }
    }

    #[test]
    fn xor_cycles_the_key_over_the_data() {
        assert_eq!(wire(serde_json::json!(["bytes/xor", "", "AQID"])), "", "empty data");
        assert_eq!(
            wire(serde_json::json!(["bytes/xor", "AQI=", "/w=="])),
            "/v0=",
            "a key shorter than the data repeats; [1,2] ^ [255]"
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/xor", "AQ==", "AQIDBA=="])),
            "AA==",
            "a key longer than the data uses its head"
        );
        for tail in 1..=3 {
            let data: Vec<u8> = (0..(8 + tail)).map(|i| (i * 37 + 5) as u8).collect();
            let key = [0xA5u8, 0x5A, 0x0F, 0xF0];
            let expected: Vec<u8> = data.iter().enumerate().map(|(i, b)| b ^ key[i % 4]).collect();
            let form = serde_json::json!(["bytes/xor", codec::base64_encode(&data), codec::base64_encode(&key)]);
            assert_eq!(wire(form), codec::base64_encode(&expected), "tail of {tail}");
        }
    }

    #[test]
    fn xor_twice_with_a_four_byte_key_is_the_identity() {
        let data = codec::base64_encode(b"a WebSocket payload of 23");
        let key = codec::base64_encode(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let once = wire(serde_json::json!(["bytes/xor", data, key]));
        assert_ne!(once, data);
        assert_eq!(wire(serde_json::json!(["bytes/xor", once, key])), data);
    }

    #[test]
    fn xor_with_an_empty_key_answers_null_and_bad_arguments_are_refused() {
        assert_eq!(eval(serde_json::json!(["bytes/xor", "AQID", ""])).unwrap(), Value::Null);
        fails_invalid(serde_json::json!(["bytes/xor", "%%", "AQ=="]));
        fails_kind(serde_json::json!(["bytes/xor", "AQID", 7]));
    }

    #[test]
    fn valid_utf8_tells_text_from_binary() {
        assert_eq!(
            eval(serde_json::json!(["bytes/valid-utf8", "aGVsbG8="])).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/valid-utf8", ""])).unwrap(),
            Value::Bool(true),
            "empty is valid text"
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/valid-utf8", "w6k="])).unwrap(),
            Value::Bool(true),
            "a two-byte scalar"
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/valid-utf8", "/w=="])).unwrap(),
            Value::Bool(false),
            "0xFF is never UTF-8"
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/valid-utf8", "wQ=="])).unwrap(),
            Value::Bool(false),
            "a truncated sequence"
        );
        fails_invalid(serde_json::json!(["bytes/valid-utf8", "%%"]));
    }

    #[test]
    fn from_string_encodes_utf8() {
        assert_eq!(
            wire(serde_json::json!(["bytes/from-string", "hello"])),
            "aGVsbG8="
        );
        assert_eq!(wire(serde_json::json!(["bytes/from-string", ""])), "");
        assert_eq!(wire(serde_json::json!(["bytes/from-string", "é"])), "w6k=");
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
        assert_eq!(
            eval(serde_json::json!(["bytes/to-string", "!!notbase64"])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/to-string", "/w=="])).unwrap(),
            Value::Null
        );
        assert!(
            eval(serde_json::json!(["bytes/to-string", 5])).is_err(),
            "control: a number is a type error, not a bad value"
        );
    }

    #[test]
    fn base64_text_and_bytes_convert_both_ways_unchanged() {
        assert_eq!(
            wire(serde_json::json!(["bytes/from-base64", "aGVsbG8="])),
            "aGVsbG8="
        );
        assert_eq!(
            text(serde_json::json!([
                "bytes/to-base64",
                ["bytes/from-string", "hello"]
            ])),
            "aGVsbG8="
        );
        assert_eq!(wire(serde_json::json!(["bytes/from-base64", ""])), "");
        assert_eq!(
            text(serde_json::json!([
                "bytes/to-string",
                [
                    "bytes/from-base64",
                    ["bytes/to-base64", ["bytes/from-string", "é"]]
                ]
            ])),
            "é"
        );
    }

    #[test]
    fn from_base64_answers_null_for_text_that_is_not_standard_base64() {
        assert_eq!(
            eval(serde_json::json!(["bytes/from-base64", "!!notbase64"])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/from-base64", "aGVsbG8"])).unwrap(),
            Value::Null
        );
        assert!(
            eval(serde_json::json!(["bytes/from-base64", 5])).is_err(),
            "control: a number is a type error, not a bad value"
        );
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
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", 1, 3])),
            "ZWw="
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", 1])),
            "ZWxsbw=="
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", 1, null])),
            "ZWxsbw=="
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", 0, 5])),
            "aGVsbG8="
        );
    }

    #[test]
    fn slice_edges_clamp_and_negatives_count_from_the_end() {
        assert_eq!(
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", 3, 1])),
            ""
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", 2, 2])),
            ""
        );
        assert_eq!(wire(serde_json::json!(["bytes/slice", "aGVsbG8=", 9])), "");
        assert_eq!(
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", 3, 99])),
            "bG8="
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", -2])),
            "bG8="
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", 0, -1])),
            "aGVsbA=="
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/slice", "aGVsbG8=", -99, 2])),
            "aGU="
        );
        assert_eq!(wire(serde_json::json!(["bytes/slice", "", 0, 3])), "");
        fails_invalid(serde_json::json!(["bytes/slice", "%%", 0, 1]));
    }

    #[test]
    fn concat_joins_in_order() {
        assert_eq!(
            wire(serde_json::json!([
                "bytes/concat",
                ["list", "aGk=", "IQ=="]
            ])),
            "aGkh"
        );
        assert_eq!(wire(serde_json::json!(["bytes/concat", ["list"]])), "");
        assert_eq!(
            wire(serde_json::json!(["bytes/concat", ["list", "", "aGk="]])),
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
        assert_eq!(
            at(serde_json::json!(["bytes/index-of", hello, b64(b"l")])),
            Value::Number(2.0)
        );
        assert_eq!(
            at(serde_json::json!(["bytes/index-of", hello, b64(b"lo")])),
            Value::Number(3.0)
        );
        assert_eq!(
            at(serde_json::json!(["bytes/index-of", hello, b64(b"l"), 3])),
            Value::Number(3.0)
        );
        assert_eq!(
            at(serde_json::json!(["bytes/index-of", hello, b64(b"l"), -2])),
            Value::Number(3.0)
        );
        assert_eq!(
            at(serde_json::json!([
                "bytes/index-of",
                hello,
                b64(b"l"),
                null
            ])),
            Value::Number(2.0)
        );
        assert_eq!(
            at(serde_json::json!(["bytes/index-of", hello, "", 2])),
            Value::Number(2.0)
        );
        assert_eq!(
            at(serde_json::json!([
                "bytes/index-of",
                b64(b"\r\n+OK\r\n"),
                b64(b"\r\n"),
                1
            ])),
            Value::Number(5.0)
        );
    }

    #[test]
    fn index_of_answers_minus_one_when_absent_and_control_rejects_bad_bytes() {
        let hello = b64(b"hello");
        assert_eq!(
            eval(serde_json::json!(["bytes/index-of", hello, b64(b"z")])).unwrap(),
            Value::Number(-1.0)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/index-of", hello, b64(b"l"), 4])).unwrap(),
            Value::Number(-1.0)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/index-of", hello, b64(b"hello!")])).unwrap(),
            Value::Number(-1.0)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/index-of", "", b64(b"a")])).unwrap(),
            Value::Number(-1.0)
        );
        fails_invalid(serde_json::json!(["bytes/index-of", "%%", b64(b"a")]));
        fails_invalid(serde_json::json!(["bytes/index-of", hello, "%%"]));
    }

    #[test]
    fn at_answers_the_byte_value_or_null_out_of_range() {
        let data = b64(&[0, 1, 2, 255]);
        assert_eq!(
            eval(serde_json::json!(["bytes/at", data, 0])).unwrap(),
            Value::Number(0.0)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/at", data, 3])).unwrap(),
            Value::Number(255.0)
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/at", data, 4])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/at", data, -1])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/at", "", 0])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/at", data, 1.5])).unwrap(),
            Value::Null
        );
        fails_kind(serde_json::json!(["bytes/at", data, null]));
        fails_kind(serde_json::json!(["bytes/at", data, "0"]));
        fails_invalid(serde_json::json!(["bytes/at", "%%", 0]));
    }

    #[test]
    fn read_uint_reads_big_and_little_endian_at_each_width() {
        let data = b64(&[0x12, 0x34, 0x56, 0x78, 0x9a]);
        let read = |offset: i64, width: i64, endian: &str| {
            eval(serde_json::json!([
                "bytes/read-uint",
                data,
                offset,
                width,
                endian
            ]))
            .unwrap()
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
    fn read_int_sign_extends_each_width_in_both_byte_orders() {
        let read = |bytes: &[u8], width: i64, endian: &str, op: &str| {
            eval(serde_json::json!([op, b64(bytes), 0, width, endian])).unwrap()
        };
        assert_eq!(read(&[0xfe], 1, "be", "bytes/read-int"), Value::Int(-2));
        assert_eq!(read(&[0xfe, 0xff], 2, "le", "bytes/read-int"), Value::Int(-2));
        assert_eq!(read(&[0xff, 0xfe], 2, "be", "bytes/read-int"), Value::Int(-2));
        assert_eq!(read(&[0x00, 0x00, 0x00, 0x80], 4, "le", "bytes/read-int"), Value::Int(i32::MIN.into()));
        assert_eq!(read(&[0x80, 0, 0, 0, 0, 0, 0, 0], 8, "be", "bytes/read-int"), Value::Int(i64::MIN));
        assert_eq!(read(&[0x7f], 1, "be", "bytes/read-int"), Value::Int(127), "a clear top bit stays positive");
        assert_eq!(read(&[0xfe, 0xff], 2, "le", "bytes/read-uint"), Value::Int(0xfffe), "control: the unsigned read");
        assert_eq!(read(&[0xfe], 2, "le", "bytes/read-int"), Value::Null, "control: past the data");
    }

    #[test]
    fn read_uint_answers_null_past_the_end_and_refuses_bad_shape() {
        let data = b64(&[1, 2, 3]);
        assert_eq!(
            eval(serde_json::json!(["bytes/read-uint", data, 2, 2, "be"])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/read-uint", data, -1, 1, "be"])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/read-uint", data, 3, 1, "le"])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/read-uint", data, 0, 3, "be"])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/read-uint", data, 0, 1, "middle"])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["bytes/read-uint", data, 0.5, 1, "be"])).unwrap(),
            Value::Null
        );
        let too_wide = b64(&[0x80, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            eval(serde_json::json!(["bytes/read-uint", too_wide, 0, 8, "be"])).unwrap(),
            Value::Null,
            "above i64::MAX has no int"
        );
        fails_kind(serde_json::json!(["bytes/read-uint", data, null, 1, "be"]));
        fails_kind(serde_json::json!(["bytes/read-uint", data, 0, 1, 5]));
        fails_invalid(serde_json::json!(["bytes/slice", "aGk=", 0.5, 1]));
    }

    #[test]
    fn write_uint_encodes_each_width_and_round_trips_through_read() {
        assert_eq!(
            wire(serde_json::json!(["bytes/write-uint", 0x1234, 2, "be"])),
            b64(&[0x12, 0x34])
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/write-uint", 0x1234, 2, "le"])),
            b64(&[0x34, 0x12])
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/write-uint", 255, 1, "be"])),
            b64(&[255])
        );
        assert_eq!(
            wire(serde_json::json!(["bytes/write-uint", 0, 4, "le"])),
            b64(&[0, 0, 0, 0])
        );
        assert_eq!(
            wire(serde_json::json!([
                "bytes/write-uint",
                9_007_199_254_740_991_i64,
                8,
                "be"
            ])),
            b64(&[0, 0x1f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])
        );
        assert_eq!(
            eval(serde_json::json!([
                "bytes/read-uint",
                ["bytes/write-uint", 305_419_896, 4, "le"],
                0,
                4,
                "le"
            ]))
            .unwrap(),
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
        null(serde_json::json!(["bytes/write-uint", 1.0e300, 8, "be"]));
        null(serde_json::json!(["bytes/write-uint", 1, 3, "be"]));
        null(serde_json::json!(["bytes/write-uint", 1, 2, "big"]));
        fails_kind(serde_json::json!(["bytes/write-uint", null, 2, "be"]));
        fails_kind(serde_json::json!(["bytes/write-uint", 1, "2", "be"]));
    }

    fn eval_with(form: serde_json::Value, ctx: &BindingContext) -> EvalResult {
        let expr: SExpression = serde_json::from_value(form).expect("s-expression");
        eval_expr(&expr, ctx)
    }

    #[test]
    fn bytes_results_are_bytes_and_base64_text_is_accepted_as_bytes() {
        assert_eq!(eval(serde_json::json!(["bytes/from-string", "hi"])).unwrap(), Value::bytes(b"hi".to_vec()));
        assert_eq!(eval(serde_json::json!(["bytes/from-base64", "aGk="])).unwrap(), Value::bytes(b"hi".to_vec()));
        assert!(matches!(eval(serde_json::json!(["bytes/to-base64", "aGk="])).unwrap(), Value::String(_)), "control: to-base64 is text");
        let mut ctx = BindingContext::default();
        ctx.set_local("b", Value::bytes(b"hello".to_vec()));
        assert_eq!(eval_with(serde_json::json!(["bytes/length", "@b"]), &ctx).unwrap(), Value::Int(5));
        assert!(matches!(eval_with(serde_json::json!(["bytes/length", "@b"]), &ctx).unwrap(), Value::Int(5)));
        assert_eq!(
            eval_with(serde_json::json!(["bytes/slice", "@b", 1, 3]), &ctx).unwrap(),
            Value::bytes(b"el".to_vec())
        );
    }

    #[test]
    fn read_uint_width_eight_above_two_to_the_fifty_third_is_exact() {
        let data = b64(&[0x10, 0, 0, 0, 0, 0, 0, 1]);
        let read = eval(serde_json::json!(["bytes/read-uint", data, 0, 8, "be"])).unwrap();
        assert!(matches!(read, Value::Int(1152921504606846977)));
        let mut ctx = BindingContext::default();
        ctx.set_local("v", read);
        assert_eq!(
            eval_with(serde_json::json!(["bytes/write-uint", "@v", 8, "be"]), &ctx).unwrap(),
            Value::bytes(vec![0x10, 0, 0, 0, 0, 0, 0, 1])
        );
        assert!(matches!(eval(serde_json::json!(["bytes/at", data, 0])).unwrap(), Value::Int(16)));
    }
}
