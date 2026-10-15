//! Encoding and digest operators. Every byte-level step is `crate::codec`.

use super::macros::*;
use super::*;
use crate::codec;

fn digest_text(bytes: &[u8; 32], encoding: &str, op: &str) -> EvalResult {
    match encoding {
        "hex" => Ok(Value::String(codec::hex(bytes))),
        "base64" => Ok(Value::String(codec::base64_encode(bytes))),
        other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{op}: encoding must be \"hex\" or \"base64\", got \"{other}\""
        )))),
    }
}

fn string_arg(arg: &SExpression, ctx: &BindingContext, op: &str) -> Result<String, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::String(s) => Ok(s),
        other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{op}: expected a string argument, got {}",
            other.type_name()
        )))),
    }
}

/// str/encode-uri-component
pub struct StrEncodeUriComponentOp;
impl Operator for StrEncodeUriComponentOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/encode-uri-component", 1);
        let s = string_arg(&args[0], ctx, "str/encode-uri-component")?;
        Ok(Value::String(codec::encode_uri_component(&s)))
    }
}

/// base64/encode - UTF-8 bytes, standard padded alphabet
pub struct Base64EncodeOp;
impl Operator for Base64EncodeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "base64/encode", 1);
        let s = string_arg(&args[0], ctx, "base64/encode")?;
        Ok(Value::String(codec::base64_encode(s.as_bytes())))
    }
}

/// base64/decode - to UTF-8; invalid base64 or invalid UTF-8 is an evaluation error
pub struct Base64DecodeOp;
impl Operator for Base64DecodeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "base64/decode", 1);
        let s = string_arg(&args[0], ctx, "base64/decode")?;
        let bytes = codec::base64_decode(&s).map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!("base64/decode: {e}")))
        })?;
        let text = String::from_utf8(bytes).map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "base64/decode: not UTF-8: {e}"
            )))
        })?;
        Ok(Value::String(text))
    }
}

/// hash/sha256 (s, encoding)
pub struct HashSha256Op;
impl Operator for HashSha256Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hash/sha256", >= 2);
        let s = string_arg(&args[0], ctx, "hash/sha256")?;
        let encoding = string_arg(&args[1], ctx, "hash/sha256")?;
        digest_text(&codec::sha256(s.as_bytes()), &encoding, "hash/sha256")
    }
}

/// hmac/sha256 (key, message, encoding)
pub struct HmacSha256Op;
impl Operator for HmacSha256Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hmac/sha256", >= 3);
        let key = string_arg(&args[0], ctx, "hmac/sha256")?;
        let message = string_arg(&args[1], ctx, "hmac/sha256")?;
        let encoding = string_arg(&args[2], ctx, "hmac/sha256")?;
        digest_text(
            &codec::hmac_sha256(key.as_bytes(), message.as_bytes()),
            &encoding,
            "hmac/sha256",
        )
    }
}
