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
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "str/decode-uri-component: {e}"
            )))
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

/// hash/scrypt (s, salt credential) - the `@almadar/auth` api-key hash: scrypt N=2^14 r=8 p=1 over the hex-decoded salt,
/// as lower-case hex. The salt argument names a credential (a `secret` knob); the kernel resolves its value into the
/// context, so the value never enters program data. The result is null when the context holds no value for that
/// credential or the value is not non-empty hex, so a program can route the failure.
pub struct HashScryptOp;
impl Operator for HashScryptOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hash/scrypt", == 2);
        let s = string_arg(&args[0], ctx, "hash/scrypt")?;
        let credential = string_arg(&args[1], ctx, "hash/scrypt")?;
        let Some(salt) = ctx.secret(&credential).and_then(codec::hex_decode) else {
            return Ok(Value::Null);
        };
        let key = codec::scrypt_api_key(s.as_bytes(), &salt)
            .map_err(|e| OrbitalError::Eval(EvalError::InvalidArgument(format!("hash/scrypt: {e}"))))?;
        Ok(Value::String(codec::hex(&key)))
    }
}

/// hash/pbkdf2 (password, salt, iterations, length) - PBKDF2-HMAC-SHA256 (RFC 8018) of bytes, answering length bytes
pub struct HashPbkdf2Op;
impl Operator for HashPbkdf2Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "hash/pbkdf2";
        require_arity!(args, OP, == 4);
        let bytes = |arg: &SExpression| -> Result<Vec<u8>, OrbitalError> {
            let s = string_arg(arg, ctx, OP)?;
            codec::base64_decode(&s)
                .map_err(|e| OrbitalError::Eval(EvalError::InvalidArgument(format!("{OP}: invalid bytes ({e})"))))
        };
        let password = bytes(&args[0])?;
        let salt = bytes(&args[1])?;
        let count = |arg: &SExpression, what: &str| -> Result<u32, OrbitalError> {
            match eval_expr(arg, ctx)? {
                Value::Number(n) if n.fract() == 0.0 && (1.0..=f64::from(u32::MAX)).contains(&n) => Ok(n as u32),
                other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "{OP}: {what} {}: expected a whole number from 1",
                    other.to_text()
                )))),
            }
        };
        let iterations = count(&args[2], "iterations")?;
        let length = count(&args[3], "length")?;
        let key = codec::pbkdf2_hmac_sha256(&password, &salt, iterations, length as usize);
        Ok(Value::String(codec::base64_encode(&key)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(form: serde_json::Value) -> EvalResult {
        let expr: SExpression = serde_json::from_value(form).expect("s-expression");
        eval_expr(&expr, &BindingContext::default())
    }

    fn pbkdf2_hex(password: &[u8], salt: &[u8], iterations: i64, length: i64) -> String {
        let form = serde_json::json!([
            "hash/pbkdf2",
            codec::base64_encode(password),
            codec::base64_encode(salt),
            iterations,
            length
        ]);
        match eval(form) {
            Ok(Value::String(b64)) => codec::hex(&codec::base64_decode(&b64).expect("bytes")),
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[test]
    fn pbkdf2_answers_the_rfc_vectors_as_bytes() {
        assert_eq!(pbkdf2_hex(b"password", b"salt", 1, 32), "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b");
        assert_eq!(pbkdf2_hex(b"password", b"salt", 4096, 32), "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a");
        assert_eq!(pbkdf2_hex(b"password", b"salt", 1, 40).len(), 80);
        assert_eq!(pbkdf2_hex(b"password", b"salt", 1, 1), "12");
    }

    #[test]
    fn pbkdf2_control_differs_by_salt_and_refuses_bad_counts() {
        assert_ne!(pbkdf2_hex(b"password", b"pepper", 1, 32), pbkdf2_hex(b"password", b"salt", 1, 32));
        let pw = codec::base64_encode(b"password");
        let salt = codec::base64_encode(b"salt");
        for (iterations, length) in [(0.0, 32.0), (1.5, 32.0), (-1.0, 32.0), (1.0, 0.0), (1.0, 2.5)] {
            let err = eval(serde_json::json!(["hash/pbkdf2", pw, salt, iterations, length])).expect_err("refused");
            assert!(err.to_string().contains("hash/pbkdf2"), "{err}");
        }
        assert!(eval(serde_json::json!(["hash/pbkdf2", "%%", salt, 1, 32])).is_err());
    }
}
