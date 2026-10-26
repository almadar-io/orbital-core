//! Argument readers and the bytes encoding shared by every family of the native `os/` answerer.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::value::Value;

pub(super) fn denied(op: OsOp, what: &str) -> OsError {
    OsError::new(OsErrorKind::Denied, format!("{}: {what}", op.name()))
}

fn invalid(op: OsOp, i: usize, expected: &str) -> OsError {
    OsError::new(
        OsErrorKind::Invalid,
        format!("{}: argument {} must be {expected}", op.name(), i + 1),
    )
}

fn arg<'a>(args: &'a [Value], i: usize, op: OsOp, expected: &str) -> Result<&'a Value, OsError> {
    args.get(i).ok_or_else(|| {
        OsError::new(
            OsErrorKind::Invalid,
            format!("{}: missing argument {} ({expected})", op.name(), i + 1),
        )
    })
}

/// The argument at `i` unless it is absent or null.
pub(super) fn arg_opt(args: &[Value], i: usize) -> Option<&Value> {
    args.get(i).filter(|v| !matches!(v, Value::Null))
}

pub(super) fn arg_str(args: &[Value], i: usize, op: OsOp) -> Result<&str, OsError> {
    match arg(args, i, op, "a string")? {
        Value::String(s) => Ok(s),
        _ => Err(invalid(op, i, "a string")),
    }
}

/// An integral number that fits an `i64`.
pub(super) fn arg_i64(args: &[Value], i: usize, op: OsOp) -> Result<i64, OsError> {
    match arg(args, i, op, "an integer")? {
        Value::Number(n)
            if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 =>
        {
            Ok(*n as i64)
        }
        _ => Err(invalid(op, i, "an integer")),
    }
}

pub(super) fn arg_bool(args: &[Value], i: usize, op: OsOp) -> Result<bool, OsError> {
    match arg(args, i, op, "a boolean")? {
        Value::Bool(b) => Ok(*b),
        _ => Err(invalid(op, i, "a boolean")),
    }
}

/// A `bytes` argument: a standard padded base64 string.
pub(super) fn arg_bytes(args: &[Value], i: usize, op: OsOp) -> Result<Vec<u8>, OsError> {
    let text = arg_str(args, i, op)?;
    STANDARD.decode(text).map_err(|e| {
        OsError::new(
            OsErrorKind::Invalid,
            format!(
                "{}: argument {} is not standard base64: {e}",
                op.name(),
                i + 1
            ),
        )
    })
}

/// The run-time value of `bytes`: standard padded base64.
pub(super) fn bytes_value(bytes: &[u8]) -> Value {
    Value::string(STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OP: OsOp = OsOp::ReadFile;

    fn s(text: &str) -> Value {
        Value::string(text.to_string())
    }

    #[test]
    fn a_string_argument_is_read_and_a_wrong_type_or_missing_one_is_invalid() {
        let args = [s("/a"), Value::Number(1.0)];
        assert_eq!(arg_str(&args, 0, OP), Ok("/a"));
        for bad in [arg_str(&args, 1, OP), arg_str(&args, 2, OP)] {
            assert_eq!(bad.expect_err("invalid").kind, OsErrorKind::Invalid);
        }
        assert!(arg_str(&args, 2, OP)
            .expect_err("missing")
            .message
            .contains("missing argument 3"));
        assert!(arg_str(&[Value::Null], 0, OP).is_err());
    }

    #[test]
    fn an_integer_argument_rejects_fractions_non_finite_and_huge_numbers() {
        let args = [
            Value::Number(7.0),
            Value::Number(-3.0),
            Value::Number(1.5),
            Value::Number(f64::NAN),
            Value::Number(f64::INFINITY),
            Value::Number(1e300),
            s("7"),
        ];
        assert_eq!(arg_i64(&args, 0, OP), Ok(7));
        assert_eq!(arg_i64(&args, 1, OP), Ok(-3));
        for i in 2..=6 {
            assert_eq!(
                arg_i64(&args, i, OP).expect_err("invalid").kind,
                OsErrorKind::Invalid,
                "{i}"
            );
        }
        assert!(arg_i64(&args, 9, OP).is_err());
    }

    #[test]
    fn bool_arguments_are_typed() {
        let args = [Value::Bool(true), Value::Null, s("x")];
        assert_eq!(arg_bool(&args, 0, OP), Ok(true));
        assert!(arg_bool(&args, 2, OP).is_err());
    }

    #[test]
    fn an_optional_argument_is_absent_when_missing_or_null() {
        let args = [Value::Null, s("x")];
        assert!(arg_opt(&args, 0).is_none());
        assert!(arg_opt(&args, 1).is_some());
        assert!(arg_opt(&args, 2).is_none());
    }

    #[test]
    fn bytes_round_trip_through_standard_padded_base64() {
        assert_eq!(bytes_value(b"hi"), s("aGk="));
        assert_eq!(bytes_value(b""), s(""));
        assert_eq!(arg_bytes(&[s("aGk=")], 0, OP), Ok(b"hi".to_vec()));
        assert_eq!(arg_bytes(&[s("")], 0, OP), Ok(Vec::new()));
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(arg_bytes(&[bytes_value(&all)], 0, OP), Ok(all));
    }

    #[test]
    fn non_utf8_bytes_survive_the_encoding() {
        let raw: Vec<u8> = vec![0xff, 0xfe, 0x00, 0xc3, 0x28];
        assert!(std::str::from_utf8(&raw).is_err());
        assert_eq!(arg_bytes(&[bytes_value(&raw)], 0, OP), Ok(raw.to_vec()));
    }

    #[test]
    fn invalid_base64_is_invalid_not_a_panic() {
        for text in ["aGk", "a", "!!!!", "aGk=aGk=", "aGk-", "aG k="] {
            let error = arg_bytes(&[s(text)], 0, OP).expect_err(text);
            assert_eq!(error.kind, OsErrorKind::Invalid, "{text}");
        }
        assert!(arg_bytes(&[Value::Number(1.0)], 0, OP).is_err());
        assert!(arg_bytes(&[], 0, OP).is_err());
    }

    #[test]
    fn denied_names_the_op_and_the_refusal() {
        let error = denied(OsOp::Spawn, "command not granted");
        assert_eq!(error.kind, OsErrorKind::Denied);
        assert_eq!(error.message, "os/spawn: command not granted");
    }
}
