//! Encoding and digest operators. Every byte-level step is `crate::codec`.

use super::macros::*;
use super::*;
use orbital_core_base::codec;

fn digest_text(bytes: &[u8], encoding: &str, op: &str) -> EvalResult {
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
        require_arity!(args, "str/encode-uri-component", == 1);
        let s = string_arg(&args[0], ctx, "str/encode-uri-component")?;
        Ok(Value::String(codec::encode_uri_component(&s)))
    }
}

/// str/decode-uri-component - JS decodeURIComponent; null yields null, malformed input is an evaluation error
pub struct StrDecodeUriComponentOp;
impl Operator for StrDecodeUriComponentOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/decode-uri-component", == 1);
        let s = match eval_expr(&args[0], ctx)? {
            Value::Null => return Ok(Value::Null),
            Value::String(s) => s,
            other => {
                return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "str/decode-uri-component: expected a string argument, got {}",
                    other.type_name()
                ))))
            }
        };
        let decoded = codec::decode_uri_component(&s).map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!("str/decode-uri-component: {e}")))
        })?;
        Ok(Value::String(decoded))
    }
}

/// str/uri-component-valid - true iff str/decode-uri-component would succeed; null is false
pub struct StrUriComponentValidOp;
impl Operator for StrUriComponentValidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/uri-component-valid", == 1);
        match eval_expr(&args[0], ctx)? {
            Value::Null => Ok(Value::Bool(false)),
            Value::String(s) => Ok(Value::Bool(codec::decode_uri_component(&s).is_ok())),
            other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "str/uri-component-valid: expected a string argument, got {}",
                other.type_name()
            )))),
        }
    }
}

/// base64/encode - UTF-8 bytes, standard padded alphabet
pub struct Base64EncodeOp;
impl Operator for Base64EncodeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "base64/encode", == 1);
        let s = string_arg(&args[0], ctx, "base64/encode")?;
        Ok(Value::String(codec::base64_encode(s.as_bytes())))
    }
}

/// The one base64/decode step: standard base64 then UTF-8; the error is the unprefixed reason.
fn decode_base64_text(s: &str) -> Result<String, String> {
    let bytes = codec::base64_decode(s)?;
    String::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))
}

/// base64/decode - to UTF-8; invalid base64 or invalid UTF-8 is an evaluation error
pub struct Base64DecodeOp;
impl Operator for Base64DecodeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "base64/decode", == 1);
        let s = string_arg(&args[0], ctx, "base64/decode")?;
        let text = decode_base64_text(&s).map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!("base64/decode: {e}")))
        })?;
        Ok(Value::String(text))
    }
}

/// base64/valid - true iff base64/decode would succeed; null is false
pub struct Base64ValidOp;
impl Operator for Base64ValidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "base64/valid", == 1);
        match eval_expr(&args[0], ctx)? {
            Value::Null => Ok(Value::Bool(false)),
            Value::String(s) => Ok(Value::Bool(decode_base64_text(&s).is_ok())),
            other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "base64/valid: expected a string argument, got {}",
                other.type_name()
            )))),
        }
    }
}

/// hash/sha256 (s, encoding)
pub struct HashSha256Op;
impl Operator for HashSha256Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hash/sha256", == 2);
        let s = string_arg(&args[0], ctx, "hash/sha256")?;
        let encoding = string_arg(&args[1], ctx, "hash/sha256")?;
        digest_text(&codec::sha256(s.as_bytes()), &encoding, "hash/sha256")
    }
}

/// hash/md5 (base64 bytes, encoding) - the base64 is decoded once and the bytes are hashed
pub struct HashMd5Op;
impl Operator for HashMd5Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hash/md5", == 2);
        let s = string_arg(&args[0], ctx, "hash/md5")?;
        let bytes = codec::base64_decode(&s).map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!("hash/md5: {e}")))
        })?;
        let encoding = string_arg(&args[1], ctx, "hash/md5")?;
        digest_text(&codec::md5(&bytes), &encoding, "hash/md5")
    }
}

/// hmac/sha256 (key, message, encoding)
pub struct HmacSha256Op;
impl Operator for HmacSha256Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hmac/sha256", == 3);
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
