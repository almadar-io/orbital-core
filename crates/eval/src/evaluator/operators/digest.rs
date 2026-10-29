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

/// hash/pbkdf2 (password, salt, iterations, length) - PBKDF2-HMAC-SHA256 (RFC 8018) of bytes, answering length bytes
pub struct HashPbkdf2Op;
impl Operator for HashPbkdf2Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "hash/pbkdf2";
        require_arity!(args, OP, == 4);
        let password = bytes_in(&args[0], ctx)?;
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
        let key = codec::pbkdf2_hmac_sha256(&password, &salt, iterations, length as usize);
        Ok(Value::string(codec::base64_encode(&key)))
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

/// hmac/sha256 (key, message, encoding)
pub struct HmacSha256Op;
impl Operator for HmacSha256Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "hmac/sha256", == 3);
        let key = string_arg(&args[0], ctx)?;
        let message = string_arg(&args[1], ctx)?;
        let encoding = string_arg(&args[2], ctx)?;
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
        let pw = codec::base64_encode(b"password");
        let salt = codec::base64_encode(b"salt");
        for (iterations, length) in [
            (0.0, 32.0),
            (1.5, 32.0),
            (-1.0, 32.0),
            (1.0, 0.0),
            (1.0, 2.5),
        ] {
            assert_eq!(
                eval(serde_json::json!([
                    "hash/pbkdf2",
                    pw,
                    salt,
                    iterations,
                    length
                ]))
                .unwrap(),
                Value::Null
            );
        }
        assert_eq!(
            eval(serde_json::json!(["hash/pbkdf2", "%%", salt, 1, 32])).unwrap(),
            Value::Null
        );
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
}
