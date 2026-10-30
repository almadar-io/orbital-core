//! Encoding and digest operators. Every byte-level step is `crate::codec`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::codec;

fn encode_digest(bytes: &[u8], encoding: &str) -> Option<String> {
    match encoding {
        "hex" => Some(codec::hex(bytes)),
        "base64" => Some(codec::base64_encode(bytes)),
        _ => None,
    }
}

fn digest_text(bytes: &[u8], encoding: &str, op: &str) -> EvalResult {
    encode_digest(bytes, encoding)
        .map(Value::string)
        .ok_or_else(|| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "{op}: encoding must be \"hex\" or \"base64\", got \"{encoding}\""
            )))
        })
}

fn digest_or_null(bytes: &[u8], encoding: &str) -> EvalResult {
    Ok(encode_digest(bytes, encoding).map_or(Value::Null, Value::string))
}

fn string_arg(arg: &SExpression, ctx: &BindingContext) -> Result<String, OrbitalError> {
    Ok(eval_expr(arg, ctx)?.text().map(str::to_string)?)
}

/// str/encode-uri-component
pub struct StrEncodeUriComponentOp;
impl Operator for StrEncodeUriComponentOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/encode-uri-component", == 1);
        let s = string_arg(&args[0], ctx)?;
        Ok(Value::string(codec::encode_uri_component(&s)))
    }
}

/// str/decode-uri-component - JS decodeURIComponent; malformed input answers null, a non-string input (null included) is a type mismatch
pub struct StrDecodeUriComponentOp;
impl Operator for StrDecodeUriComponentOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/decode-uri-component", == 1);
        let s = string_arg(&args[0], ctx)?;
        Ok(codec::decode_uri_component(&s).map_or(Value::Null, Value::string))
    }
}

/// str/uri-component-valid - true iff str/decode-uri-component would succeed; a non-string input is a type mismatch
pub struct StrUriComponentValidOp;
impl Operator for StrUriComponentValidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/uri-component-valid", == 1);
        let s = string_arg(&args[0], ctx)?;
        Ok(Value::Bool(codec::decode_uri_component(&s).is_ok()))
    }
}

/// base64/encode - UTF-8 bytes, standard padded alphabet
pub struct Base64EncodeOp;
impl Operator for Base64EncodeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "base64/encode", == 1);
        let s = string_arg(&args[0], ctx)?;
        Ok(Value::string(codec::base64_encode(s.as_bytes())))
    }
}

/// The one base64/decode step: standard base64 then UTF-8; the error is the unprefixed reason.
fn decode_base64_text(s: &str) -> Result<String, String> {
    let bytes = codec::base64_decode(s)?;
    String::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))
}

/// base64/decode - to UTF-8; invalid base64 or invalid UTF-8 answers null
pub struct Base64DecodeOp;
impl Operator for Base64DecodeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "base64/decode", == 1);
        let s = string_arg(&args[0], ctx)?;
        Ok(decode_base64_text(&s).map_or(Value::Null, Value::string))
    }
}

/// base64/valid - true iff base64/decode would succeed; a non-string input is a type mismatch
pub struct Base64ValidOp;
impl Operator for Base64ValidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "base64/valid", == 1);
        let s = string_arg(&args[0], ctx)?;
        Ok(Value::Bool(decode_base64_text(&s).is_ok()))
    }
}

/// hash/sha256 (data, encoding, input?) - input "text" (default) hashes the UTF-8 of a string, "bytes" decodes
/// standard padded base64 once and hashes the bytes (the hash/md5 reading)
pub struct HashSha256Op;
impl Operator for HashSha256Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "hash/sha256";
        require_arity!(args, OP, >= 2);
        if args.len() > 3 {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: OP.to_string(),
                expected: 3,
                actual: args.len(),
            }));
        }
        let s = string_arg(&args[0], ctx)?;
        let encoding = string_arg(&args[1], ctx)?;
        let input = match args.get(2) {
            Some(arg) => string_arg(arg, ctx)?,
            None => "text".to_string(),
        };
        let data = match input.as_str() {
            "text" => s.into_bytes(),
            "bytes" => match codec::base64_decode(&s) {
                Ok(data) => data,
                Err(_) => return Ok(Value::Null),
            },
            _ => return Ok(Value::Null),
        };
        digest_or_null(&codec::sha256(&data), &encoding)
    }
}

/// hash/md5 (base64 bytes, encoding) - the base64 is decoded once and the bytes are hashed
pub struct HashMd5Op;
impl Operator for HashMd5Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hash/md5", == 2);
        let s = string_arg(&args[0], ctx)?;
        let bytes = codec::base64_decode(&s);
        let encoding = string_arg(&args[1], ctx)?;
        match bytes {
            Ok(bytes) => digest_or_null(&codec::md5(&bytes), &encoding),
            Err(_) => Ok(Value::Null),
        }
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
        let s = string_arg(&args[0], ctx)?;
        let credential = string_arg(&args[1], ctx)?;
        let Some(salt) = ctx.secret(&credential).and_then(codec::hex_decode) else {
            return Ok(Value::Null);
        };
        let key = codec::scrypt_api_key(s.as_bytes(), &salt).map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!("hash/scrypt: {e}")))
        })?;
        Ok(Value::string(codec::hex(&key)))
    }
}

/// hash/pbkdf2 (credential, salt, iterations, length) - PBKDF2-HMAC-SHA256 (RFC 8018) answering length bytes. The password
/// is a credential slot: a secret-typed @config knob naming the credential whose value (its UTF-8 bytes) the kernel
/// resolves into the evaluation. Null when the credential is not held or an operand is not canonical base64 or a count.
pub struct HashPbkdf2Op;
impl Operator for HashPbkdf2Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "hash/pbkdf2";
        require_arity!(args, OP, == 4);
        let password = credential_value(&args[0], ctx)?;
        let salt = bytes_in(&args[1], ctx)?;
        let count = |arg: &SExpression| -> Result<Option<u32>, OrbitalError> {
            let n = eval_expr(arg, ctx)?.number().map_err(OrbitalError::Eval)?;
            Ok((n.fract() == 0.0 && (1.0..=f64::from(u32::MAX)).contains(&n)).then_some(n as u32))
        };
        let iterations = count(&args[2])?;
        let length = count(&args[3])?;
        let (Some(password), Some(salt), Some(iterations), Some(length)) =
            (password, salt, iterations, length)
        else {
            return Ok(Value::Null);
        };
        let key = codec::pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations, length as usize);
        Ok(Value::string(codec::base64_encode(&key)))
    }
}

/// hash/md5-credential (credential, suffix, encoding) - MD5 (RFC 1321) of the credential's value followed by the UTF-8
/// `suffix`, as lower-case hex or standard base64: the first step of the Postgres md5 login, whose password never
/// becomes a program value. The credential is a slot as in hash/pbkdf2. Null when it is not held.
pub struct HashMd5CredentialOp;
impl Operator for HashMd5CredentialOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hash/md5-credential", == 3);
        let suffix = string_arg(&args[1], ctx)?;
        let encoding = string_arg(&args[2], ctx)?;
        let Some(value) = credential_value(&args[0], ctx)? else {
            return Ok(Value::Null);
        };
        let mut data = value.as_bytes().to_vec();
        data.extend_from_slice(suffix.as_bytes());
        digest_or_null(&codec::md5(&data), &encoding)
    }
}

/// credential/byte-length (credential) - the length in UTF-8 bytes of the credential's value, so a program can frame a
/// wire message around a segment the host fills in. The credential is a slot as in hash/pbkdf2. Null when it is not held.
pub struct CredentialByteLengthOp;
impl Operator for CredentialByteLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "credential/byte-length", == 1);
        Ok(credential_value(&args[0], ctx)?.map_or(Value::Null, |value| Value::Number(value.len() as f64)))
    }
}

fn bytes_in(arg: &SExpression, ctx: &BindingContext) -> Result<Option<Vec<u8>>, OrbitalError> {
    let s = string_arg(arg, ctx)?;
    Ok(codec::base64_decode(&s).ok())
}

fn codec_options(
    args: &[SExpression],
    index: usize,
    ctx: &BindingContext,
    op: &str,
    keys: &[&str],
) -> Result<Option<Value>, OrbitalError> {
    let value = match args.get(index) {
        Some(arg) => eval_expr(arg, ctx)?,
        None => Value::Object(Default::default()),
    };
    let Value::Object(fields) = &value else {
        return Err(OrbitalError::Eval(value.kind_error("object")));
    };
    for (key, value) in fields {
        if key == "result" && !matches!(value, Value::Bool(_)) {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "{op}: invalid option {key}"
            ))));
        }
        if !keys.contains(&key.as_str()) {
            return Ok(None);
        }
    }
    Ok(Some(value))
}

fn codec_setting(
    options: &Value,
    key: &str,
    default: u32,
    max: u32,
    min: u32,
) -> Result<Option<u32>, OrbitalError> {
    let Some(value) = options.as_object().and_then(|fields| fields.get(key)) else {
        return Ok(Some(default));
    };
    let n = value.number().map_err(OrbitalError::Eval)?;
    Ok((n.fract() == 0.0 && n >= f64::from(min) && n <= f64::from(max)).then_some(n as u32))
}

fn codec_answer(
    answer: Result<codec::CodecDecoded, codec::CodecFailure>,
    options: &Value,
) -> EvalResult {
    if options.as_object().and_then(|fields| fields.get("result")) == Some(&Value::Bool(true)) {
        let (ok, data, crc, code) = match answer {
            Ok(decoded) => (
                true,
                Value::string(codec::base64_encode(&decoded.data)),
                decoded
                    .crc
                    .map(|crc| Value::Number(f64::from(crc)))
                    .unwrap_or(Value::Null),
                Value::Null,
            ),
            Err(error) => (
                false,
                Value::Null,
                Value::Null,
                Value::string(error.code().to_string()),
            ),
        };
        return Ok(Value::Object(
            [
                ("ok", Value::Bool(ok)),
                ("data", data),
                ("crc", crc),
                ("code", code),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
        ));
    }
    Ok(answer.map_or(Value::Null, |decoded| {
        Value::string(codec::base64_encode(&decoded.data))
    }))
}

fn compress(args: &[SExpression], ctx: &BindingContext, op: &str) -> EvalResult {
    require_arity!(args, op, >= 1);
    if args.len() > 2 {
        require_arity!(args, op, == 2);
    }
    let data = bytes_in(&args[0], ctx)?;
    let gzip = op == "gzip/compress";
    let options = codec_options(
        args,
        1,
        ctx,
        op,
        if gzip {
            &["result", "level"]
        } else {
            &["result", "quality", "window"]
        },
    )?;
    let (Some(data), Some(options)) = (data, options) else {
        return Ok(Value::Null);
    };
    let packed = if gzip {
        let Some(level) = codec_setting(&options, "level", 6, 9, 0)? else {
            return Ok(Value::Null);
        };
        codec::gzip_compress_level(&data, level)
    } else {
        let (Some(quality), Some(window)) = (
            codec_setting(&options, "quality", 11, 11, 0)?,
            codec_setting(&options, "window", 22, 24, 10)?,
        ) else {
            return Ok(Value::Null);
        };
        brotli_compress(&data, quality as i32, window as i32, op)?
    };
    let crc = if gzip {
        Some(u32::from_le_bytes(
            packed[packed.len() - 8..packed.len() - 4]
                .try_into()
                .expect("gzip trailer"),
        ))
    } else {
        None
    };
    codec_answer(Ok(codec::CodecDecoded { data: packed, crc }), &options)
}

fn decompress(args: &[SExpression], ctx: &BindingContext, op: &str) -> EvalResult {
    require_arity!(args, op, >= 2);
    if args.len() > 3 {
        require_arity!(args, op, == 3);
    }
    let data = bytes_in(&args[0], ctx)?;
    let n = eval_expr(&args[1], ctx)?
        .number()
        .map_err(OrbitalError::Eval)?;
    let max = (n.fract() == 0.0 && n >= 0.0 && n <= u32::MAX as f64).then_some(n as usize);
    let options = codec_options(args, 2, ctx, op, &["result"])?;
    let (Some(data), Some(max), Some(options)) = (data, max, options) else {
        return Ok(Value::Null);
    };
    let answer = if op == "gzip/decompress" {
        codec::gzip_decode(&data, max)
    } else {
        brotli_decode(&data, max, op)?
    };
    codec_answer(answer, &options)
}

#[cfg(feature = "std")]
fn brotli_compress(
    data: &[u8],
    quality: i32,
    window: i32,
    _op: &str,
) -> Result<Vec<u8>, OrbitalError> {
    Ok(codec::brotli_compress_options(data, quality, window))
}

#[cfg(feature = "std")]
fn brotli_decode(data: &[u8], max: usize, _op: &str) -> Result<codec::CodecResult, OrbitalError> {
    Ok(codec::brotli_decode(data, max))
}

/// Brotli runs through the codec's `std::io` streams, which a bare-metal host does not have.
#[cfg(not(feature = "std"))]
fn brotli_compress(
    _data: &[u8],
    _quality: i32,
    _window: i32,
    op: &str,
) -> Result<Vec<u8>, OrbitalError> {
    Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
        "{op}: brotli is not available on a bare-metal host"
    ))))
}

#[cfg(not(feature = "std"))]
fn brotli_decode(_data: &[u8], _max: usize, op: &str) -> Result<codec::CodecResult, OrbitalError> {
    Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
        "{op}: brotli is not available on a bare-metal host"
    ))))
}

pub struct GzipCompressOp;
impl Operator for GzipCompressOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        compress(args, ctx, "gzip/compress")
    }
}
pub struct GzipDecompressOp;
impl Operator for GzipDecompressOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        decompress(args, ctx, "gzip/decompress")
    }
}
pub struct BrotliCompressOp;
impl Operator for BrotliCompressOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        compress(args, ctx, "brotli/compress")
    }
}
pub struct BrotliDecompressOp;
impl Operator for BrotliDecompressOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        decompress(args, ctx, "brotli/decompress")
    }
}

/// The value of the credential an operator operand names, held in the evaluation by the kernel; `None` when it holds none.
fn credential_value<'a>(arg: &SExpression, ctx: &'a BindingContext) -> Result<Option<&'a str>, OrbitalError> {
    let name = string_arg(arg, ctx)?;
    Ok(ctx.secret(&name))
}

/// hmac/sha256 (credential, message, encoding) - the key is a credential slot: a secret-typed @config knob naming the
/// credential whose value the kernel resolves into the evaluation. Null when the credential is not held.
pub struct HmacSha256Op;
impl Operator for HmacSha256Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hmac/sha256", == 3);
        let message = string_arg(&args[1], ctx)?;
        let encoding = string_arg(&args[2], ctx)?;
        let Some(key) = credential_value(&args[0], ctx)? else {
            return Ok(Value::Null);
        };
        digest_text(
            &codec::hmac_sha256(key.as_bytes(), message.as_bytes()),
            &encoding,
            "hmac/sha256",
        )
    }
}

/// hmac/sha256-bytes (key, message, encoding) - HMAC-SHA256 under a bytes key that is a program value (a derived key such
/// as a SigV4 chain step); a non-canonical base64 key answers null
pub struct HmacSha256BytesOp;
impl Operator for HmacSha256BytesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hmac/sha256-bytes", == 3);
        let key = bytes_in(&args[0], ctx)?;
        let message = string_arg(&args[1], ctx)?;
        let encoding = string_arg(&args[2], ctx)?;
        let Some(key) = key else {
            return Ok(Value::Null);
        };
        digest_text(
            &codec::hmac_sha256(&key, message.as_bytes()),
            &encoding,
            "hmac/sha256-bytes",
        )
    }
}

/// ecdsa/p256-sign (credential, message) - ECDSA P-256 / SHA-256, RFC 6979 deterministic, 64 raw r||s bytes. The key is a
/// credential slot holding a PKCS#8 PEM private key. Null when the credential is not held or is not such a key.
pub struct EcdsaP256SignOp;
impl Operator for EcdsaP256SignOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "ecdsa/p256-sign", == 2);
        let message = string_arg(&args[1], ctx)?;
        let Some(pem) = credential_value(&args[0], ctx)? else {
            return Ok(Value::Null);
        };
        Ok(codec::ecdsa_p256_sha256_sign(pem, message.as_bytes())
            .map_or(Value::Null, |signature| Value::string(codec::base64_encode(&signature))))
    }
}

/// rsa/sign (credential, message) - RSASSA-PKCS1-v1_5 / SHA-256, blinded; the signature is as long as the modulus. The key
/// is a credential slot holding a PKCS#8 PEM private key of at least 2048 bits. Null when the credential is not held or is
/// not such a key.
pub struct RsaSignOp;
impl Operator for RsaSignOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "rsa/sign", == 2);
        let name = string_arg(&args[0], ctx)?;
        let message = string_arg(&args[1], ctx)?;
        let Some(pem) = ctx.secret(&name) else {
            return Ok(Value::Null);
        };
        ctx.sealed("rsa/sign", &[Value::string(name.clone()), Value::string(message.clone())], |seed| {
            codec::rsa_pkcs1_sha256_sign(pem, message.as_bytes(), seed)
                .map_or(Value::Null, |signature| Value::string(codec::base64_encode(&signature)))
        })
    }
}

/// ecdh/p256-ephemeral (peer) - generate an ephemeral P-256 key inside the operator and answer { public, shared }: the
/// 65-byte SEC1 public key and the 32-byte shared x coordinate with the SEC1 `peer` point. The ephemeral secret never
/// leaves the operator. Null when the peer is not a point on the curve.
pub struct EcdhP256EphemeralOp;
impl Operator for EcdhP256EphemeralOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "ecdh/p256-ephemeral", == 1);
        let Some(peer) = bytes_in(&args[0], ctx)? else {
            return Ok(Value::Null);
        };
        ctx.sealed("ecdh/p256-ephemeral", &[Value::string(codec::base64_encode(&peer))], |seed| {
            codec::ecdh_p256_ephemeral(&peer, seed).map_or(Value::Null, |exchange| {
                Value::Object(
                    [
                        ("public", Value::string(codec::base64_encode(&exchange.public))),
                        ("shared", Value::string(codec::base64_encode(&exchange.shared))),
                    ]
                    .into_iter()
                    .map(|(key, value)| (key.into(), value))
                    .collect(),
                )
            })
        })
    }
}

fn gcm(args: &[SExpression], ctx: &BindingContext, op: &str, seal: bool) -> EvalResult {
    require_arity!(args, op, == 4);
    let (key, nonce, data, aad) = (
        bytes_in(&args[0], ctx)?,
        bytes_in(&args[1], ctx)?,
        bytes_in(&args[2], ctx)?,
        bytes_in(&args[3], ctx)?,
    );
    let (Some(key), Some(nonce), Some(data), Some(aad)) = (key, nonce, data, aad) else {
        return Ok(Value::Null);
    };
    let out = if seal {
        codec::aes128_gcm_seal(&key, &nonce, &data, &aad)
    } else {
        codec::aes128_gcm_open(&key, &nonce, &data, &aad)
    };
    Ok(out.map_or(Value::Null, |bytes| Value::string(codec::base64_encode(&bytes))))
}

/// aes-gcm/seal (key, nonce, plaintext, aad) - AES-128-GCM; ciphertext followed by the 16-byte tag. Null when the key is not
/// 16 bytes, the nonce not 12, or an operand is not canonical base64.
pub struct AesGcmSealOp;
impl Operator for AesGcmSealOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        gcm(args, ctx, "aes-gcm/seal", true)
    }
}

/// aes-gcm/open (key, nonce, sealed, aad) - AES-128-GCM open of ciphertext followed by its tag. Null when the tag does not
/// verify or an operand is malformed.
pub struct AesGcmOpenOp;
impl Operator for AesGcmOpenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        gcm(args, ctx, "aes-gcm/open", false)
    }
}

/// rsa/verify (n, e, message, signature) - RSASSA-PKCS1-v1_5 SHA-256; answers the verdict code, a non-canonical base64 operand is invalid
pub struct RsaVerifyOp;
impl Operator for RsaVerifyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "rsa/verify", == 4);
        let n = bytes_in(&args[0], ctx)?;
        let e = bytes_in(&args[1], ctx)?;
        let message = string_arg(&args[2], ctx)?;
        let signature = bytes_in(&args[3], ctx)?;
        let (Some(n), Some(e), Some(signature)) = (n, e, signature) else {
            return Ok(Value::string("invalid"));
        };
        Ok(Value::string(codec::rsa_pkcs1_sha256_verify(&n, &e, message.as_bytes(), &signature).as_str()))
    }
}

/// ecdsa/p256-verify (x, y, message, signature) - ECDSA P-256 SHA-256 over a raw r||s signature; answers the verdict code
pub struct EcdsaP256VerifyOp;
impl Operator for EcdsaP256VerifyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "ecdsa/p256-verify", == 4);
        let x = bytes_in(&args[0], ctx)?;
        let y = bytes_in(&args[1], ctx)?;
        let message = string_arg(&args[2], ctx)?;
        let signature = bytes_in(&args[3], ctx)?;
        let (Some(x), Some(y), Some(signature)) = (x, y, signature) else {
            return Ok(Value::string("invalid"));
        };
        Ok(Value::string(codec::ecdsa_p256_sha256_verify(&x, &y, message.as_bytes(), &signature).as_str()))
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
            "PW",
            codec::base64_encode(salt),
            iterations,
            length
        ]);
        let ctx = held("PW", std::str::from_utf8(password).expect("text"));
        match eval_in(&ctx, form) {
            Ok(Value::String(b64)) => codec::hex(&codec::base64_decode(&b64).expect("bytes")),
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    fn sha256(form: serde_json::Value) -> String {
        match eval(form) {
            Ok(Value::String(s)) => s.into_inner(),
            other => panic!("expected a digest, got {other:?}"),
        }
    }

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn sha256_bytes_input_hashes_the_decoded_bytes() {
        assert_eq!(
            sha256(serde_json::json!(["hash/sha256", "YWJj", "hex", "bytes"])),
            ABC
        );
        assert_eq!(
            sha256(serde_json::json!(["hash/sha256", "abc", "hex"])),
            ABC
        );
        assert_eq!(
            sha256(serde_json::json!(["hash/sha256", "abc", "hex", "text"])),
            ABC
        );
        assert_eq!(
            sha256(serde_json::json!(["hash/sha256", "", "hex", "bytes"])),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(serde_json::json!([
                "hash/sha256",
                "//4AgA==",
                "hex",
                "bytes"
            ])),
            "5a741968f40e57485ed6e1a1af381adeb2714223c35acedf1ad0670e42df2eb5"
        );
        assert_eq!(
            sha256(serde_json::json!([
                "hash/sha256",
                "YWJj",
                "base64",
                "bytes"
            ])),
            codec::base64_encode(&codec::hex_decode(ABC).expect("hex"))
        );
    }

    #[test]
    fn sha256_control_text_reading_and_refusals() {
        assert_ne!(
            sha256(serde_json::json!(["hash/sha256", "YWJj", "hex"])),
            ABC
        );
        for form in [
            serde_json::json!(["hash/sha256", 42, "hex"]),
            serde_json::json!(["hash/sha256", 42, "hex", "bytes"]),
        ] {
            let err = eval(form).expect_err("refused");
            assert!(err.to_string().contains("expected string, got number"), "{err}");
        }
        for form in [
            serde_json::json!(["hash/sha256", "abc", "hex", "utf16"]),
            serde_json::json!(["hash/sha256", "%%not base64", "hex", "bytes"]),
            serde_json::json!(["hash/sha256", "abc", "octal"]),
        ] {
            assert_eq!(eval(form).unwrap(), Value::Null);
        }
    }

    const HELLO3: &[u8] = b"hello hello hello";
    const GZIP_CLI: &str = "H4sIAAAAAAAAA8tIzcnJV8hAkACAiPnlEQAAAA==";
    const BROTLI_CLI: &str = "HxAA+I2Ubt5EVYaWbCBvAU8cYBw=";

    fn bytes_of(form: serde_json::Value) -> Vec<u8> {
        match eval(form) {
            Ok(Value::String(b64)) => codec::base64_decode(&b64).expect("bytes"),
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    fn compression_cases(algo: &str, cli: &str) {
        let (compress, decompress) = (format!("{algo}/compress"), format!("{algo}/decompress"));
        let text = codec::base64_encode(HELLO3);
        let packed = codec::base64_encode(&bytes_of(serde_json::json!([compress, text])));
        assert_eq!(
            bytes_of(serde_json::json!([decompress, packed, 17])),
            HELLO3,
            "round trip"
        );
        assert_eq!(
            bytes_of(serde_json::json!([decompress, cli, 17])),
            HELLO3,
            "the CLI vector"
        );
        let empty = codec::base64_encode(&bytes_of(serde_json::json!([compress, ""])));
        assert!(
            bytes_of(serde_json::json!([decompress, empty, 0])).is_empty(),
            "edge: empty input"
        );
        assert_eq!(
            eval(serde_json::json!([decompress, cli, 16])).unwrap(),
            Value::Null
        );
        let mut corrupt = codec::base64_decode(cli).unwrap();
        corrupt.truncate(corrupt.len() / 2);
        assert_eq!(
            eval(serde_json::json!([
                decompress,
                codec::base64_encode(&corrupt),
                1024
            ]))
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!([
                decompress,
                codec::base64_encode(b"not compressed at all"),
                1024
            ]))
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!([decompress, "%%", 1024])).unwrap(),
            Value::Null
        );
        for max in [serde_json::json!(-1), serde_json::json!(1.5)] {
            assert_eq!(
                eval(serde_json::json!([decompress, cli, max])).unwrap(),
                Value::Null
            );
        }
        assert!(
            eval(serde_json::json!([decompress, cli])).is_err(),
            "no implicit limit"
        );
        for typed in [
            serde_json::json!([decompress, cli, "17"]),
            serde_json::json!([compress, 42]),
        ] {
            assert!(
                eval(typed.clone()).is_err(),
                "the signature refuses {typed}"
            );
        }
    }

    #[test]
    fn gzip_round_trips_reads_the_cli_vector_and_refuses_past_the_limit() {
        compression_cases("gzip", GZIP_CLI);
        let twice = [
            codec::base64_decode(GZIP_CLI).unwrap(),
            codec::base64_decode(GZIP_CLI).unwrap(),
        ]
        .concat();
        assert_eq!(
            bytes_of(serde_json::json!([
                "gzip/decompress",
                codec::base64_encode(&twice),
                34
            ])),
            [HELLO3, HELLO3].concat(),
            "edge: two members"
        );
    }

    #[test]
    fn brotli_round_trips_reads_the_cli_vector_and_refuses_past_the_limit() {
        compression_cases("brotli", BROTLI_CLI);
    }

    #[test]
    fn control_the_two_formats_do_not_read_each_other() {
        assert_eq!(
            eval(serde_json::json!(["gzip/decompress", BROTLI_CLI, 1024])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["brotli/decompress", GZIP_CLI, 1024])).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn structured_codec_results_keep_plain_bytes_and_failure_codes() {
        for family in ["gzip", "brotli"] {
            let packed = eval(serde_json::json!([
                format!("{family}/compress"),
                "aGVsbG8="
            ]))
            .unwrap();
            let structured = eval(
                serde_json::json!([format!("{family}/compress"), "aGVsbG8=", { "result": true }]),
            )
            .unwrap();
            assert_eq!(structured.as_object().unwrap().get("data"), Some(&packed));
            let refused = eval(
                serde_json::json!([format!("{family}/decompress"), packed, 4, { "result": true }]),
            )
            .unwrap();
            assert_eq!(
                refused.as_object().unwrap().get("ok"),
                Some(&Value::Bool(false))
            );
            assert_eq!(
                refused.as_object().unwrap().get("code"),
                Some(&Value::string("output-limit"))
            );
            assert_eq!(refused.as_object().unwrap().get("data"), Some(&Value::Null));
        }
        for call in [
            serde_json::json!(["gzip/compress", "", { "level": 10 }]),
            serde_json::json!(["brotli/compress", "", { "quality": 12 }]),
            serde_json::json!(["brotli/compress", "", { "window": 9 }]),
            serde_json::json!(["gzip/compress", "", { "unknown": 1 }]),
            serde_json::json!(["gzip/compress", "%%"]),
        ] {
            assert_eq!(eval(call).unwrap(), Value::Null);
        }
        assert!(
            eval(serde_json::json!(["gzip/decompress", GZIP_CLI, 100, { "result": "true" }]))
                .is_err()
        );
    }

    #[test]
    fn pbkdf2_answers_the_rfc_vectors_as_bytes() {
        assert_eq!(
            pbkdf2_hex(b"password", b"salt", 1, 32),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            pbkdf2_hex(b"password", b"salt", 4096, 32),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
        assert_eq!(pbkdf2_hex(b"password", b"salt", 1, 40).len(), 80);
        assert_eq!(pbkdf2_hex(b"password", b"salt", 1, 1), "12");
    }

    #[test]
    fn pbkdf2_control_differs_by_salt_and_answers_null_for_bad_counts() {
        assert_ne!(
            pbkdf2_hex(b"password", b"pepper", 1, 32),
            pbkdf2_hex(b"password", b"salt", 1, 32)
        );
        let salt = codec::base64_encode(b"salt");
        let ctx = held("PW", "password");
        for (iterations, length) in [
            (0.0, 32.0),
            (1.5, 32.0),
            (-1.0, 32.0),
            (1.0, 0.0),
            (1.0, 2.5),
        ] {
            assert_eq!(
                eval_in(&ctx, serde_json::json!(["hash/pbkdf2", "PW", salt, iterations, length])).unwrap(),
                Value::Null
            );
        }
        assert_eq!(
            eval_in(&ctx, serde_json::json!(["hash/pbkdf2", "PW", "%%", 1, 32])).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(serde_json::json!(["hash/pbkdf2", "PW", salt, 1, 32])).unwrap(),
            Value::Null,
            "an evaluation holding no credential never derives from the name"
        );
        assert!(eval_in(&ctx, serde_json::json!(["hash/pbkdf2", serde_json::Value::Null, salt, 1, 32])).is_err());
    }

    #[test]
    fn md5_credential_hashes_the_held_value_then_the_suffix_and_is_null_without_it() {
        let ctx = held("PW", "pass");
        let form = |suffix: &str| serde_json::json!(["hash/md5-credential", "PW", suffix, "hex"]);
        let md5_hex = |bytes: &[u8]| codec::hex(&codec::md5(bytes));
        assert_eq!(text(eval_in(&ctx, form("word"))), md5_hex(b"password"));
        assert_ne!(text(eval_in(&ctx, form("word"))), text(eval_in(&ctx, form("words"))), "control: the suffix is hashed");
        assert_eq!(text(eval_in(&ctx, form(""))), md5_hex(b"pass"));
        assert_eq!(eval_in(&held("OTHER", "pass"), form("word")).unwrap(), Value::Null);
        assert_eq!(eval(form("word")).unwrap(), Value::Null);
        assert_eq!(eval_in(&ctx, serde_json::json!(["hash/md5-credential", "PW", "word", "base32"])).unwrap(), Value::Null, "as hash/md5 answers an unknown encoding");
        assert!(eval_in(&ctx, serde_json::json!(["hash/md5-credential", serde_json::Value::Null, "word", "hex"])).is_err());
    }

    #[test]
    fn credential_byte_length_counts_utf8_bytes_of_the_held_value_and_is_null_without_it() {
        let form = serde_json::json!(["credential/byte-length", "PW"]);
        assert_eq!(eval_in(&held("PW", "pass"), form.clone()).unwrap(), Value::Number(4.0));
        assert_eq!(eval_in(&held("PW", "pässwörd"), form.clone()).unwrap(), Value::Number(10.0), "bytes, not characters");
        assert_eq!(eval_in(&held("PW", ""), form.clone()).unwrap(), Value::Number(0.0));
        assert_eq!(eval_in(&held("OTHER", "pass"), form.clone()).unwrap(), Value::Null);
        assert_eq!(eval(form).unwrap(), Value::Null);
        assert!(eval_in(&held("PW", "pass"), serde_json::json!(["credential/byte-length", serde_json::Value::Null])).is_err());
    }

    #[test]
    fn a_null_text_operand_is_a_type_mismatch_and_a_text_one_is_answered() {
        for op in [
            "str/decode-uri-component",
            "str/uri-component-valid",
            "base64/valid",
        ] {
            assert!(eval(serde_json::json!([op, null])).is_err(), "{op}");
            assert!(eval(serde_json::json!([op, "abc"])).is_ok(), "{op}");
        }
        assert_eq!(
            eval(serde_json::json!(["str/uri-component-valid", "%E0%A4%A"])).unwrap(),
            Value::Bool(false)
        );
    }

    const RSA_N: &str = "pCJTqfijo+dyUDx+uHmap/Y5yXaRg99QNf+AUbAjWRKsgBhvqMknk2SpB9A40VwydKdtoHbWp0z4bHSNmlHooqeFd7mAxpmtsuumAiE+ICdkcPZ63j2OvqlrJna3xB4PX677BZhFYhDhYCjjlLTDwmZRBymA8vSCtDLQZ7MRkX1fxVHML+wJi1LzTwniItB/hoSKx8b4i0w4ZuesAB+/RPTjR8ktRGsUWyzW83Bcfg5IR+8bZLMNyx6XwsAuZ7nQ2+qJXSFXoq2O/a+jdtfagyJBGAD26wx6bnuReRg2x57t6nB3CYYUREhcAJAJR9BLKkNZgk/peUKeb8QHwQBgPw==";
    const RSA_SIGNATURE: &str = "RA4mgYzrg/dfjqlRbUcGMN4Vr7k7ChWG3Jsbpkz93OKGJfIJ8D8zpGCOvaMfc1WN0pWnwuPuSxHgsOMaC4bh1qH5tS1FJWFxL//Lg28IIrWHM/olBCAUXbhsqNvf3g7m50XNVmM8RvmMDZOXuBJkLm4GLAptJaSgUyK2pPFV0CCKx4zIFjtoL1ZvtZvwgT6er9VWheX+3wNm7MQwtHKYTH77z4XVi2K/+rkY7QNdh9GohExwK6cDy8sCuLG7Kjb1YykNxEcZxR8k/1X6NqZC1svDj1ghejKGi+Tkkf7nCfVH7lDIzE+FisrCetfZG8eNQ4ROWAmAA4gWv2Hjd+isWg==";
    const EC_X: &str = "DWCBVhKCGPAV0CVSa7Nu/svWxAWs1ejOo2PZPNjNAHE=";
    const EC_Y: &str = "786l0mh1J2aMm6STYLEz60poSi5F6aNJi4BnV6yCOMY=";
    const EC_SIGNATURE: &str = "eAQcvqokeZHzlZ0shvZjYSxtc12OnlR9IZgH9nCRSoqlNKyYQx0YNoLL0SpkJkGsbGfmF1a4OIfG0VncqUiwgQ==";

    fn verdict(form: serde_json::Value) -> String {
        match eval(form) {
            Ok(Value::String(s)) => s.into_inner(),
            other => panic!("expected a verdict, got {other:?}"),
        }
    }

    fn flipped(signature: &str) -> String {
        let mut bytes = codec::base64_decode(signature).expect("bytes");
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        codec::base64_encode(&bytes)
    }

    #[test]
    fn rsa_verify_answers_the_verdict_code() {
        let call = |n: &str, e: &str, message: &str, signature: &str| verdict(serde_json::json!(["rsa/verify", n, e, message, signature]));
        assert_eq!(call(RSA_N, "AQAB", "header.payload", RSA_SIGNATURE), "valid");
        assert_eq!(call(RSA_N, "AQAB", "header.payload", &flipped(RSA_SIGNATURE)), "mismatch");
        assert_eq!(call(RSA_N, "AQAB", "header.payloaD", RSA_SIGNATURE), "mismatch");
        assert_eq!(call("AQAB", "AQAB", "header.payload", RSA_SIGNATURE), "key-size");
        assert_eq!(call(RSA_N, "AQAB", "header.payload", "c2ln"), "signature-length");
        assert_eq!(call(RSA_N, "AQAB", "header.payload", RSA_N), "signature-range");
    }

    #[test]
    fn rsa_verify_refuses_an_operand_that_is_not_canonical_base64_and_raises_on_null() {
        for form in [
            serde_json::json!(["rsa/verify", "%%not base64", "AQAB", "header.payload", RSA_SIGNATURE]),
            serde_json::json!(["rsa/verify", RSA_N, "AR==", "header.payload", RSA_SIGNATURE]),
            serde_json::json!(["rsa/verify", RSA_N, "AQAB", "header.payload", &RSA_SIGNATURE[..RSA_SIGNATURE.len() - 1]]),
        ] {
            assert_eq!(verdict(form.clone()), "invalid", "{form}");
        }
        for position in 1..=4 {
            let mut form = serde_json::json!(["rsa/verify", RSA_N, "AQAB", "header.payload", RSA_SIGNATURE]);
            form[position] = serde_json::Value::Null;
            assert!(eval(form.clone()).is_err(), "{form}");
        }
        assert!(eval(serde_json::json!(["rsa/verify", RSA_N, "AQAB", "header.payload"])).is_err());
    }

    #[test]
    fn ecdsa_p256_verify_answers_the_verdict_code() {
        let call = |x: &str, y: &str, message: &str, signature: &str| verdict(serde_json::json!(["ecdsa/p256-verify", x, y, message, signature]));
        assert_eq!(call(EC_X, EC_Y, "header.payload", EC_SIGNATURE), "valid");
        assert_eq!(call(EC_X, EC_Y, "header.payload", &flipped(EC_SIGNATURE)), "mismatch");
        assert_eq!(call(EC_X, EC_Y, "header.payloaD", EC_SIGNATURE), "mismatch");
        assert_eq!(call("AQAB", EC_Y, "header.payload", EC_SIGNATURE), "length");
        assert_eq!(call(EC_X, EC_Y, "header.payload", "c2ln"), "length");
        assert_eq!(call(EC_Y, EC_X, "header.payload", EC_SIGNATURE), "point");
        assert_eq!(call(EC_X, EC_Y, "header.payload", &codec::base64_encode(&[0u8; 64])), "range");
    }

    #[test]
    fn ecdsa_p256_verify_refuses_an_operand_that_is_not_canonical_base64_and_raises_on_null() {
        assert_eq!(
            verdict(serde_json::json!(["ecdsa/p256-verify", "%%not base64", EC_Y, "header.payload", EC_SIGNATURE])),
            "invalid"
        );
        for position in 1..=4 {
            let mut form = serde_json::json!(["ecdsa/p256-verify", EC_X, EC_Y, "header.payload", EC_SIGNATURE]);
            form[position] = serde_json::Value::Null;
            assert!(eval(form.clone()).is_err(), "{form}");
        }
        assert!(eval(serde_json::json!(["ecdsa/p256-verify", EC_X, EC_Y, "header.payload"])).is_err());
    }

    const SIGN_RSA_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDBmj4cs/1X1KKS\nd7w4cQLXwY2/zQSivWFyZTFem1GZVnlg223x1KbrF+XtLGmIlWw4U6Ily/uiz645\nZFKwDJ4IRAGs89pnrAB8gR46c0aLRNYKfOpsu0ox0QZre8hgBQF5oUTdGXFQW4Sv\ndGVkM4zMtWCcb+nWWeM1+G86ghU57hjMj/5cnYEwv+waqAOnC7aMGQBNfol9lk4r\n48QKWa5TCibIbo3Hcs5UkqNZYsX+rJrUBtOk1f1/1fhXTAvWsMFudkcYH+M5hmif\nI+O20LVfOV7C2N4DhuWuk/sHHNyJVtwBnZjo3uAdEH2z1lK09JJSFwxkza6QYSRI\ndLC56SAlAgMBAAECggEALGnQhvAplc0TcItkVARk9Q8TkCUdw6GzabVaMoxqF0W1\nuakNG1W/5x3H4vV1zl4NAgz5nTci5pJDMypXJ8j1Ngs34S/d8juMrwNF5n3MeKWV\nfrlyY/hBSJVdpimy6YgscvUyoZA1JQyhr0iB/A8xZULUw36PA9wCGj8AcUknSm70\npQZHB+58X0JG+M+XlU6xW+Tb0V8r6CFGWSxhdOYaXwIBhdBJKvh1KiIHkCDPqX5i\nPsP09EsX7oDsS/+S/INVgqX/q89+ehPdzY19hXgWRD9sbH2h75ImIh/Axy52tJl/\nKCFptgFIf7WBuzJ93iMOVriCR+vncNSj1Gdm0pBq3wKBgQDx7kUsc23zRs8ca+30\n8OpMc81q/qcWBGzs+Fj941hdMRzYzUpk1+Re36Dj5PH1HPc6/h1c/Ip7c8dN1zds\nnJjbhCTpOMuLxhXy81jtWwVr9O4arc0rKH5ZjrDAmTeGtoPMhFjxKdWuljrB/1VA\ndZVhp2S4DPzPJcYu9RUs0JkkvwKBgQDM3H0Z6qiJwUSXVka9kY8rv0ntjr76UUWI\n+E7aHvENElt/YYFeaAG+Suib+bEJ6yqfJlrfF2Gut5PrC14jc9heO2CKkHa5R/8M\nzNALNZ3YSYwAaGlRLrAlRhc7Xe2uPww+Nev4X3O+NUO5MKgLzxgBzWjl+ND6K9Ck\nrsOon43AGwKBgQCsz5MPvmASf0jft2ShKxlhhPJ9c+sr+ftIhdkjeaqb3sIJRxLV\n17Tj4/+L6fS/LnKW6/9TdX0B0+DQhGOtrGovm0O2aABVFiiFgzfJS9QimjEeU3mT\nU3h1RkyRNqCIFIUuqHbTxRtCe0L7YgSmZ5dNywFRys0mtofK8PuwKEzMBwKBgQDJ\nP20SEajBsv/r35Cc16qbIk7AtTM9eqthk/T6Z90Lp1YFO8c389qKktIRr+fQQ5pa\nP/aTJEgMuEDybPdswnfykkgTtzk6aNTQQT+7U0+d0xpaqDeVzdSyuTlHU82TGFKx\npo0KMNLmLzM5cOgUubvuwVhkWtsIeBR+Q0TomN1KswKBgGtczq6Jm7Ic4+WZGzh7\nNVqwdthUdR5EdBjxi5I9SVIS1Rb6pH4Y2cUOBGCz55STj4heV3d0IRS0QCw3TtFi\n0coDp2D6a2eEZDeqdywBQVg8NYpKJI74XyXu+Ogkn0T3vQZtN+BH1fHrTfbSdTa/\nAM812eZIeiLOTXTbKy88Y2Xy\n-----END PRIVATE KEY-----\n";
    const SIGN_RSA_N: &str = "wZo+HLP9V9Sikne8OHEC18GNv80Eor1hcmUxXptRmVZ5YNtt8dSm6xfl7SxpiJVsOFOiJcv7os+uOWRSsAyeCEQBrPPaZ6wAfIEeOnNGi0TWCnzqbLtKMdEGa3vIYAUBeaFE3RlxUFuEr3RlZDOMzLVgnG/p1lnjNfhvOoIVOe4YzI/+XJ2BML/sGqgDpwu2jBkATX6JfZZOK+PEClmuUwomyG6Nx3LOVJKjWWLF/qya1AbTpNX9f9X4V0wL1rDBbnZHGB/jOYZonyPjttC1XzlewtjeA4blrpP7BxzciVbcAZ2Y6N7gHRB9s9ZStPSSUhcMZM2ukGEkSHSwuekgJQ==";
    const SIGN_RSA_SIGNATURE: &str = "ZNheFwSQUoc3hijIwW3RCdNxljDrGANFey5HEtyMvYys8RMSrC3Unkq7ya6CpBJEltkF6/tpaBoglMlH9TSxZolEThukPFL8+vtR7EEch6I4oceaDqjOE6lxhKJfbU3v8elFyaMaxByOxklNcoKnqzNLBdH/waaJhkyLHg9gQ0beE5L1aKe90US3pjdAO3RCPp3kWUHeQSEm697MS5DJjlW/m1G8292iJaYf53B9hPPP9XcC9EVvRRf57ZwJJ3/RnVexo4UFEPkyqvmaPzL6h324ArZdV2veCtloWOG+fhOWM2LzDKRJwUS/7u8mdcmX8Z/ETOkbXQ946nfuXHOXLQ==";
    const SIGN_EC_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgmw9yTiz7OigR+moX\newgXsK22MQNuX+kaIXWFYCkTrWOhRANCAAQR4vtdQBhGwJ5fkkPa7e+gusRThJWP\nfyd8sRcpFm95DUKDGQqcgdOUFry+OqdrGs+pLubBE9jFwRtIf63TxheS\n-----END PRIVATE KEY-----\n";
    const SIGN_EC_PUBLIC: &str = "BBHi+11AGEbAnl+SQ9rt76C6xFOElY9/J3yxFykWb3kNQoMZCpyB05QWvL46p2saz6ku5sET2MXBG0h/rdPGF5I=";

    fn held(name: &str, value: &str) -> BindingContext {
        let mut ctx = BindingContext::default().with_secret(name, value.to_string());
        ctx.set_sealed_outputs(seeded(0x5e));
        ctx
    }

    fn seeded(byte: u8) -> crate::evaluator::context::SealedOutputs {
        crate::evaluator::context::SealedOutputs::new(move |_, _, compute| Ok(compute([byte; 32])))
    }

    fn eval_in(ctx: &BindingContext, form: serde_json::Value) -> EvalResult {
        let expr: SExpression = serde_json::from_value(form).expect("s-expression");
        eval_expr(&expr, ctx)
    }

    fn text(result: EvalResult) -> String {
        match result {
            Ok(Value::String(s)) => s.into_inner(),
            other => panic!("expected a string, got {other:?}"),
        }
    }

    #[test]
    fn hmac_sha256_reads_its_key_from_the_held_credential_and_is_null_without_it() {
        let form = serde_json::json!(["hmac/sha256", "SIGNING", "what do ya want for nothing?", "hex"]);
        assert_eq!(
            text(eval_in(&held("SIGNING", "Jefe"), form.clone())),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(eval_in(&held("OTHER", "Jefe"), form.clone()).unwrap(), Value::Null);
        assert_eq!(eval(form).unwrap(), Value::Null, "an evaluation holding no credential never signs with the name as a key");
        let bad = serde_json::json!(["hmac/sha256", "SIGNING", "m", "base32"]);
        assert!(eval_in(&held("SIGNING", "Jefe"), bad).is_err());
    }

    #[test]
    fn hmac_sha256_bytes_takes_a_bytes_key_program_value() {
        let jefe = codec::base64_encode(b"Jefe");
        let form = serde_json::json!(["hmac/sha256-bytes", jefe, "what do ya want for nothing?", "hex"]);
        assert_eq!(text(eval(form)), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert_eq!(eval(serde_json::json!(["hmac/sha256-bytes", "%not base64", "m", "hex"])).unwrap(), Value::Null);
        assert!(eval(serde_json::json!(["hmac/sha256-bytes", serde_json::Value::Null, "m", "hex"])).is_err());
    }

    #[test]
    fn ecdsa_p256_sign_matches_rfc6979_and_verifies_under_the_public_key() {
        const RFC6979_KEY: &str = "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";
        let mut der = codec::hex_decode("3041020100301306072a8648ce3d020106082a8648ce3d030107042730250201010420").expect("hex");
        der.extend(codec::hex_decode(RFC6979_KEY).expect("hex"));
        let body = codec::base64_encode(&der);
        let (first, rest) = body.split_at(64);
        let pem = format!("-----BEGIN PRIVATE KEY-----\n{first}\n{rest}\n-----END PRIVATE KEY-----\n");
        let signature = text(eval_in(&held("EC", &pem), serde_json::json!(["ecdsa/p256-sign", "EC", "sample"])));
        let raw = codec::base64_decode(&signature).expect("base64");
        assert_eq!(
            codec::hex(&raw),
            "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8"
        );
        let public = codec::base64_decode(SIGN_EC_PUBLIC).expect("base64");
        let (x, y) = (codec::base64_encode(&public[1..33]), codec::base64_encode(&public[33..65]));
        let ctx = held("EC", SIGN_EC_PEM);
        let first = text(eval_in(&ctx, serde_json::json!(["ecdsa/p256-sign", "EC", "header.payload"])));
        assert_eq!(first, text(eval_in(&ctx, serde_json::json!(["ecdsa/p256-sign", "EC", "header.payload"]))), "RFC 6979 is deterministic");
        assert_eq!(verdict(serde_json::json!(["ecdsa/p256-verify", x, y, "header.payload", first])), "valid");
        assert_eq!(verdict(serde_json::json!(["ecdsa/p256-verify", x, y, "header.payloaD", first])), "mismatch");
    }

    #[test]
    fn ecdsa_p256_sign_is_null_without_a_key_and_for_a_key_that_is_not_pkcs8_ec() {
        let form = serde_json::json!(["ecdsa/p256-sign", "EC", "m"]);
        assert_eq!(eval(form.clone()).unwrap(), Value::Null);
        assert_eq!(eval_in(&held("EC", "not a pem"), form.clone()).unwrap(), Value::Null);
        assert_eq!(eval_in(&held("EC", SIGN_RSA_PEM), form.clone()).unwrap(), Value::Null, "an RSA key is not an EC key");
        assert!(eval_in(&held("EC", SIGN_EC_PEM), serde_json::json!(["ecdsa/p256-sign", serde_json::Value::Null, "m"])).is_err());
    }

    #[test]
    fn rsa_sign_matches_openssl_and_verifies_under_the_public_key() {
        let ctx = held("RSA", SIGN_RSA_PEM);
        let signature = text(eval_in(&ctx, serde_json::json!(["rsa/sign", "RSA", "sign me"])));
        assert_eq!(signature, SIGN_RSA_SIGNATURE, "PKCS#1 v1.5 is deterministic: blinding must not change the bytes");
        let mut other = held("RSA", SIGN_RSA_PEM);
        other.set_sealed_outputs(seeded(1));
        assert_eq!(text(eval_in(&other, serde_json::json!(["rsa/sign", "RSA", "sign me"]))), SIGN_RSA_SIGNATURE);
        assert_eq!(verdict(serde_json::json!(["rsa/verify", SIGN_RSA_N, "AQAB", "sign me", signature])), "valid");
        assert_eq!(verdict(serde_json::json!(["rsa/verify", SIGN_RSA_N, "AQAB", "sign mE", signature])), "mismatch");
    }

    #[test]
    fn rsa_sign_is_null_without_a_key_and_for_a_key_that_is_not_pkcs8_rsa_and_needs_an_entropy_source() {
        let form = serde_json::json!(["rsa/sign", "RSA", "m"]);
        assert_eq!(eval(form.clone()).unwrap(), Value::Null);
        assert_eq!(eval_in(&held("RSA", SIGN_EC_PEM), form.clone()).unwrap(), Value::Null);
        let no_entropy = BindingContext::default().with_secret("RSA", SIGN_RSA_PEM.to_string());
        assert!(eval_in(&no_entropy, form).is_err(), "blinding needs the kernel's sealed boundary");
        assert!(eval_in(&held("RSA", SIGN_RSA_PEM), serde_json::json!(["rsa/sign", "RSA", serde_json::Value::Null])).is_err());
    }

    #[test]
    fn ecdh_p256_ephemeral_answers_the_public_key_and_the_shared_secret_from_one_seed() {
        let ctx = held("UNUSED", "x");
        let form = serde_json::json!(["ecdh/p256-ephemeral", SIGN_EC_PUBLIC]);
        let Ok(Value::Object(fields)) = eval_in(&ctx, form.clone()) else { panic!("expected an exchange") };
        let field = |name: &str| match fields.get(name) {
            Some(Value::String(s)) => codec::base64_decode(s.as_str()).expect("base64"),
            other => panic!("{name}: {other:?}"),
        };
        let (public, shared) = (field("public"), field("shared"));
        assert_eq!((public.len(), public[0], shared.len()), (65, 4, 32));
        let again = eval_in(&ctx, form.clone()).unwrap();
        assert_eq!(again, Value::Object(fields.clone()), "one seed, one ephemeral key");
        let mut other = held("UNUSED", "x");
        other.set_sealed_outputs(seeded(2));
        assert_ne!(eval_in(&other, form).unwrap(), again, "another seed, another key");
        let unseeded = BindingContext::default();
        assert!(eval_in(&unseeded, serde_json::json!(["ecdh/p256-ephemeral", SIGN_EC_PUBLIC])).is_err());
    }

    #[test]
    fn ecdh_p256_ephemeral_is_null_for_a_peer_that_is_not_a_curve_point_or_not_base64() {
        let ctx = held("UNUSED", "x");
        let off_curve = codec::base64_encode(&[4u8; 65]);
        assert_eq!(eval_in(&ctx, serde_json::json!(["ecdh/p256-ephemeral", off_curve])).unwrap(), Value::Null);
        assert_eq!(eval_in(&ctx, serde_json::json!(["ecdh/p256-ephemeral", "%not base64"])).unwrap(), Value::Null);
        assert!(eval_in(&ctx, serde_json::json!(["ecdh/p256-ephemeral", serde_json::Value::Null])).is_err());
    }

    #[test]
    fn aes_gcm_seal_and_open_match_the_nist_vector_and_refuse_a_forged_tag() {
        let zero16 = codec::base64_encode(&[0u8; 16]);
        let zero12 = codec::base64_encode(&[0u8; 12]);
        let empty = codec::base64_encode(&[]);
        let sealed = text(eval(serde_json::json!(["aes-gcm/seal", zero16, zero12, zero16, empty])));
        assert_eq!(
            codec::hex(&codec::base64_decode(&sealed).expect("base64")),
            "0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf"
        );
        assert_eq!(text(eval(serde_json::json!(["aes-gcm/open", zero16, zero12, sealed, empty]))), zero16);
        let mut forged = codec::base64_decode(&sealed).expect("base64");
        forged[0] ^= 1;
        assert_eq!(
            eval(serde_json::json!(["aes-gcm/open", zero16, zero12, codec::base64_encode(&forged), empty])).unwrap(),
            Value::Null
        );
        let aad = codec::base64_encode(b"a");
        assert_eq!(eval(serde_json::json!(["aes-gcm/open", zero16, zero12, sealed, aad])).unwrap(), Value::Null, "the aad is bound");
        let short = codec::base64_encode(&[0u8; 15]);
        assert_eq!(eval(serde_json::json!(["aes-gcm/seal", short, zero12, zero16, empty])).unwrap(), Value::Null);
        assert_eq!(eval(serde_json::json!(["aes-gcm/seal", zero16, short, zero16, empty])).unwrap(), Value::Null);
        for position in 1..=4 {
            let mut form = serde_json::json!(["aes-gcm/seal", zero16, zero12, zero16, empty]);
            form[position] = serde_json::Value::Null;
            assert!(eval(form.clone()).is_err(), "{form}");
        }
    }
}
