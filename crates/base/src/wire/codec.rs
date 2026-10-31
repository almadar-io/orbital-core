//! The one set of wire codecs: percent-encoders, base64, SHA-256, MD5, HMAC-SHA256, gzip and brotli. The `http` request builder
//! and the std operators all read these.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use base64::Engine;
use md5::Md5;
use sha2::{Digest, Sha256};

pub fn push_escaped(out: &mut String, byte: u8) {
    out.push('%');
    out.push(char::from_digit(u32::from(byte >> 4), 16).map_or('0', |c| c.to_ascii_uppercase()));
    out.push(char::from_digit(u32::from(byte & 15), 16).map_or('0', |c| c.to_ascii_uppercase()));
}

/// JS `encodeURIComponent`: everything but `A-Za-z0-9 - _ . ! ~ * ' ( )` is escaped.
pub fn encode_uri_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            push_escaped(&mut out, byte);
        }
    }
    out
}

/// JS `decodeURIComponent`: `%XX` runs decode as UTF-8, `+` stays, anything malformed is `URI malformed`.
pub fn decode_uri_component(text: &str) -> Result<String, String> {
    const MALFORMED: &str = "URI malformed";
    fn flush(out: &mut String, pending: &mut Vec<u8>) -> Result<(), String> {
        out.push_str(std::str::from_utf8(pending).map_err(|_| MALFORMED.to_string())?);
        pending.clear();
        Ok(())
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut pending: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes.get(i + 1).and_then(|b| char::from(*b).to_digit(16));
            let lo = bytes.get(i + 2).and_then(|b| char::from(*b).to_digit(16));
            let (Some(hi), Some(lo)) = (hi, lo) else {
                return Err(MALFORMED.to_string());
            };
            pending.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            flush(&mut out, &mut pending)?;
            let width = text[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&text[i..i + width]);
            i += width;
        }
    }
    flush(&mut out, &mut pending)?;
    Ok(out)
}

/// `URLSearchParams` serialization of one value: a space is `+`, everything but `A-Za-z0-9 * - . _` is escaped.
pub fn form_urlencode_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"*-._".contains(&byte) {
            out.push(byte as char);
        } else if byte == b' ' {
            out.push('+');
        } else {
            push_escaped(&mut out, byte);
        }
    }
    out
}

/// `application/x-www-form-urlencoded` body of `key=value` pairs joined by `&`, in the given order.
pub fn form_urlencode(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_urlencode_value(k), form_urlencode_value(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `qs.stringify` (RFC 3986) of one value: everything but `A-Za-z0-9 - . _ ~` is escaped, so `!'()*` are too.
pub fn encode_rfc3986(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            push_escaped(&mut out, byte);
        }
    }
    out
}

/// Standard padded base64.
pub fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// URL-safe base64 without padding (the `random-bytes` value).
pub fn base64url_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Unpadded URL-safe base64 (RFC 4648 section 5), the form every JWS segment and JWK member is written in; padding or a
/// character outside the alphabet is refused.
pub fn base64url_decode(text: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|e| e.to_string())
}

pub fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|e| e.to_string())
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn md5(data: &[u8]) -> [u8; 16] {
    Md5::digest(data).into()
}

/// The scrypt cost of `@almadar/auth`'s `hashApiKey`: N = 2^14, r = 8, p = 1.
pub const SCRYPT_LOG_N: u8 = 14;
pub const SCRYPT_R: u32 = 8;
pub const SCRYPT_P: u32 = 1;
/// The length of the derived key, in bytes.
pub const SCRYPT_KEYLEN: usize = 32;

/// scrypt (RFC 7914) at the fixed `hashApiKey` cost: the 32-byte key of `password` over `salt`.
pub fn scrypt_api_key(password: &[u8], salt: &[u8]) -> Result<[u8; SCRYPT_KEYLEN], String> {
    let params = scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, SCRYPT_KEYLEN)
        .map_err(|e| e.to_string())?;
    let mut out = [0u8; SCRYPT_KEYLEN];
    scrypt::scrypt(password, salt, &params, &mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// The bytes of a non-empty, even-length hex string (either case); `None` for anything else.
pub fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()
        })
        .collect()
}

/// PBKDF2 with HMAC-SHA256 (RFC 8018 section 5.2): `out_len` bytes derived from `password` and `salt` over
/// `iterations` rounds (at least one).
pub fn pbkdf2_hmac_sha256(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    out_len: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(out_len + 32);
    let mut block = 1u32;
    while out.len() < out_len {
        let mut seed = salt.to_vec();
        seed.extend_from_slice(&block.to_be_bytes());
        let mut u = hmac_sha256(password, &seed);
        let mut t = u;
        for _ in 1..iterations.max(1) {
            u = hmac_sha256(password, &u);
            for (acc, byte) in t.iter_mut().zip(u) {
                *acc ^= byte;
            }
        }
        out.extend_from_slice(&t);
        block += 1;
    }
    out.truncate(out_len);
    out
}

pub const BLOCK: usize = 64;

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodecFailure {
    Magic, Method, Flags, HeaderCrc, Truncated, Block, Crc, Size, Trailing, OutputLimit,
}

impl CodecFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Magic => "magic", Self::Method => "method", Self::Flags => "flags",
            Self::HeaderCrc => "header-crc", Self::Truncated => "truncated", Self::Block => "block",
            Self::Crc => "crc", Self::Size => "size", Self::Trailing => "trailing", Self::OutputLimit => "output-limit",
        }
    }
}

#[derive(Debug)]
pub struct CodecDecoded {
    pub data: Vec<u8>,
    pub crc: Option<u32>,
}

pub fn gzip_compress(data: &[u8]) -> Vec<u8> {
    gzip_compress_level(data, 6)
}

/// One gzip member: the fixed header (no name or time, OS unknown), the raw deflate stream, CRC-32 and size.
pub fn gzip_compress_level(data: &[u8], level: u32) -> Vec<u8> {
    let extra_flags = if level >= 9 { 2 } else if level <= 1 { 4 } else { 0 };
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, extra_flags, 255];
    out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(data, level.min(10) as u8));
    out.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

pub fn gzip_decompress(data: &[u8], max: usize) -> Result<Vec<u8>, String> {
    gzip_decode(data, max).map(|out| out.data).map_err(|error| error.code().to_string())
}

pub type CodecResult = Result<CodecDecoded, CodecFailure>;

pub fn gzip_decode(data: &[u8], max: usize) -> CodecResult {
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let start = offset;
        if data.len() - start < 18 {
            return Err(if start == 0 { CodecFailure::Truncated } else { CodecFailure::Trailing });
        }
        if data[start..start + 2] != [31, 139] {
            return Err(if start == 0 { CodecFailure::Magic } else { CodecFailure::Trailing });
        }
        if data[start + 2] != 8 { return Err(CodecFailure::Method); }
        let flags = data[start + 3];
        if flags & 224 != 0 { return Err(CodecFailure::Flags); }
        offset += 10;
        if flags & 4 != 0 {
            let extra = data.get(offset..offset + 2).ok_or(CodecFailure::Truncated)?;
            offset += 2 + usize::from(u16::from_le_bytes([extra[0], extra[1]]));
            if offset > data.len() { return Err(CodecFailure::Truncated); }
        }
        for mask in [8, 16] {
            if flags & mask != 0 {
                offset += data[offset..].iter().position(|byte| *byte == 0).ok_or(CodecFailure::Truncated)? + 1;
            }
        }
        if flags & 2 != 0 {
            let header_sum = crc32fast::hash(&data[start..offset]);
            let header_crc = data.get(offset..offset + 2).ok_or(CodecFailure::Truncated)?;
            if u16::from_le_bytes([header_crc[0], header_crc[1]]) != header_sum as u16 { return Err(CodecFailure::HeaderCrc); }
            offset += 2;
        }
        let mut decoder = miniz_oxide::inflate::stream::InflateState::new_boxed(miniz_oxide::DataFormat::Raw);
        let mut crc = crc32fast::Hasher::new();
        let member_start = out.len();
        loop {
            let mut buffer = [0; 8192];
            let step = miniz_oxide::inflate::stream::inflate(&mut decoder, &data[offset..], &mut buffer, miniz_oxide::MZFlush::None);
            let ended = match step.status {
                Ok(status) => status == miniz_oxide::MZStatus::StreamEnd,
                Err(miniz_oxide::MZError::Buf) => false,
                Err(_) => return Err(CodecFailure::Block),
            };
            let (consumed, produced) = (step.bytes_consumed, step.bytes_written);
            offset += consumed;
            if produced > max - out.len() { return Err(CodecFailure::OutputLimit); }
            crc.update(&buffer[..produced]);
            out.extend_from_slice(&buffer[..produced]);
            if ended { break; }
            if consumed == 0 && produced == 0 { return Err(if offset == data.len() { CodecFailure::Truncated } else { CodecFailure::Block }); }
        }
        let sum = crc.finalize();
        let trailer = data.get(offset..offset + 8).ok_or(CodecFailure::Truncated)?;
        let expected_crc = u32::from_le_bytes(trailer[..4].try_into().expect("four bytes"));
        let expected_size = u32::from_le_bytes(trailer[4..].try_into().expect("four bytes"));
        if sum != expected_crc { return Err(CodecFailure::Crc); }
        if (out.len() - member_start) as u32 != expected_size { return Err(CodecFailure::Size); }
        offset += 8;
        if offset == data.len() { return Ok(CodecDecoded { data: out, crc: Some(sum) }); }
    }
}

#[cfg(feature = "std")]
pub fn brotli_compress(data: &[u8]) -> Vec<u8> {
    brotli_compress_options(data, 11, 22)
}

#[cfg(feature = "std")]
pub fn brotli_compress_options(data: &[u8], quality: i32, window: i32) -> Vec<u8> {
    let params = brotli::enc::BrotliEncoderParams { quality, lgwin: window, ..Default::default() };
    let mut out = Vec::new();
    brotli::BrotliCompress(&mut &data[..], &mut out, &params).expect("brotli into memory cannot fail");
    out
}

#[cfg(feature = "std")]
pub fn brotli_decompress(data: &[u8], max: usize) -> Result<Vec<u8>, String> {
    brotli_decode(data, max).map(|out| out.data).map_err(|error| error.code().to_string())
}

#[cfg(feature = "std")]
pub fn brotli_decode(data: &[u8], max: usize) -> CodecResult {
    use std::io::Read;
    let mut reader = brotli::Decompressor::new(data, 4096);
    let mut out = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let n = reader.read(&mut buffer).map_err(|error| if error.kind() == std::io::ErrorKind::UnexpectedEof { CodecFailure::Truncated } else { CodecFailure::Block })?;
        if n == 0 { return Ok(CodecDecoded { data: out, crc: None }); }
        if n > max - out.len() { return Err(CodecFailure::OutputLimit); }
        out.extend_from_slice(&buffer[..n]);
    }
}

/// The Ed25519 public key of the 32-byte signing `seed`.
pub fn ed25519_public_key(seed: &[u8; 32]) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

/// The 64-byte Ed25519 signature of `message` under the 32-byte signing `seed`.
pub fn ed25519_sign(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    use ed25519_dalek::Signer;
    ed25519_dalek::SigningKey::from_bytes(seed).sign(message).to_bytes()
}

/// Whether `signature` is `public_key`'s Ed25519 signature of `message`; a key that is not a curve point is not valid.
pub fn ed25519_verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    use ed25519_dalek::Verifier;
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(public_key) else { return false };
    key.verify(message, &ed25519_dalek::Signature::from_bytes(signature)).is_ok()
}

/// The outcome of an RSASSA-PKCS1-v1_5 / SHA-256 verification: a verdict on a well-formed key and signature, or the
/// closed reason the key or signature could not be checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsaVerdict {
    Valid,
    Mismatch,
    KeySize,
    SignatureLength,
    SignatureRange,
}

impl RsaVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            RsaVerdict::Valid => "valid",
            RsaVerdict::Mismatch => "mismatch",
            RsaVerdict::KeySize => "key-size",
            RsaVerdict::SignatureLength => "signature-length",
            RsaVerdict::SignatureRange => "signature-range",
        }
    }
}

/// The smallest accepted RSA modulus, 2048 bits, what JWS RS256 requires.
pub const RSA_MIN_MODULUS_BYTES: usize = 256;

const SHA256_DIGEST_INFO: [u8; 19] = [0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20];

/// RSASSA-PKCS1-v1_5 with SHA-256 (RFC 8017 section 8.2.2) over a public key given as big-endian modulus and exponent: the
/// signature is raised to the exponent modulo the modulus and the block must equal 00 01 FF..FF 00 DigestInfo hash byte for
/// byte. Public inputs only; nothing here is secret.
pub fn rsa_pkcs1_sha256_verify(n: &[u8], e: &[u8], message: &[u8], signature: &[u8]) -> RsaVerdict {
    use rsa::BigUint;
    if n.len() < RSA_MIN_MODULUS_BYTES || n[0] == 0 || e.is_empty() || e.len() >= n.len() {
        return RsaVerdict::KeySize;
    }
    if signature.len() != n.len() {
        return RsaVerdict::SignatureLength;
    }
    let modulus = BigUint::from_bytes_be(n);
    let signature_int = BigUint::from_bytes_be(signature);
    if signature_int >= modulus {
        return RsaVerdict::SignatureRange;
    }
    let recovered = signature_int.modpow(&BigUint::from_bytes_be(e), &modulus).to_bytes_be();
    let k = n.len();
    let mut expected = Vec::with_capacity(k);
    expected.extend_from_slice(&[0x00, 0x01]);
    expected.resize(k - SHA256_DIGEST_INFO.len() - 32 - 1, 0xff);
    expected.push(0x00);
    expected.extend_from_slice(&SHA256_DIGEST_INFO);
    expected.extend_from_slice(&sha256(message));
    let mut block = vec![0u8; k - recovered.len().min(k)];
    block.extend_from_slice(&recovered);
    if block == expected {
        RsaVerdict::Valid
    } else {
        RsaVerdict::Mismatch
    }
}

/// The outcome of an ECDSA P-256 / SHA-256 verification: a verdict on a well-formed key and signature, or the closed reason
/// the key or signature could not be checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcdsaVerdict {
    Valid,
    Mismatch,
    Length,
    Range,
    Point,
}

impl EcdsaVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            EcdsaVerdict::Valid => "valid",
            EcdsaVerdict::Mismatch => "mismatch",
            EcdsaVerdict::Length => "length",
            EcdsaVerdict::Range => "range",
            EcdsaVerdict::Point => "point",
        }
    }
}

/// ECDSA over P-256 with SHA-256 (FIPS 186-4 section 6.4.2) for a public key given as two 32-byte big-endian coordinates and
/// a raw 64-byte r||s signature. r or s of zero is a range refusal before the key is looked at; a coordinate not below the
/// field prime or a point off the curve is a point refusal; r or s not below the group order is a range refusal.
pub fn ecdsa_p256_sha256_verify(x: &[u8], y: &[u8], message: &[u8], signature: &[u8]) -> EcdsaVerdict {
    use p256::ecdsa::signature::Verifier;
    use p256::elliptic_curve::sec1::FromEncodedPoint;
    use p256::elliptic_curve::PrimeField;
    if x.len() != 32 || y.len() != 32 || signature.len() != 64 {
        return EcdsaVerdict::Length;
    }
    let (r_bytes, s_bytes) = signature.split_at(32);
    if r_bytes.iter().all(|b| *b == 0) || s_bytes.iter().all(|b| *b == 0) {
        return EcdsaVerdict::Range;
    }
    let encoded = p256::EncodedPoint::from_affine_coordinates(p256::FieldBytes::from_slice(x), p256::FieldBytes::from_slice(y), false);
    let Some(point) = Option::<p256::AffinePoint>::from(p256::AffinePoint::from_encoded_point(&encoded)) else {
        return EcdsaVerdict::Point;
    };
    for half in [r_bytes, s_bytes] {
        if Option::<p256::Scalar>::from(p256::Scalar::from_repr(*p256::FieldBytes::from_slice(half))).is_none() {
            return EcdsaVerdict::Range;
        }
    }
    let Ok(signature) = p256::ecdsa::Signature::from_slice(signature) else {
        return EcdsaVerdict::Range;
    };
    let Ok(key) = p256::ecdsa::VerifyingKey::from_affine(point) else {
        return EcdsaVerdict::Point;
    };
    if key.verify(message, &signature).is_ok() {
        EcdsaVerdict::Valid
    } else {
        EcdsaVerdict::Mismatch
    }
}

/// A signing key held as a PEM string, or the closed reason it cannot sign: the PEM is not a PKCS#8 key of the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignFailure {
    Key,
}

/// The 64-byte raw r||s ECDSA P-256 / SHA-256 signature of `message` under a PKCS#8 PEM private key. The nonce is
/// deterministic (RFC 6979), so the same key and message always sign to the same bytes, and the `p256` scalar arithmetic is
/// constant time. The key is zeroized when it drops.
pub fn ecdsa_p256_sha256_sign(pem: &str, message: &[u8]) -> Result<[u8; 64], SignFailure> {
    use p256::ecdsa::signature::Signer;
    use p256::pkcs8::DecodePrivateKey;
    let key = p256::ecdsa::SigningKey::from_pkcs8_pem(pem).map_err(|_| SignFailure::Key)?;
    let signature: p256::ecdsa::Signature = key.sign(message);
    Ok(signature.to_bytes().into())
}

/// RSASSA-PKCS1-v1_5 / SHA-256 (RFC 8017 section 8.2.1) under a PKCS#8 PEM private key. The private operation is blinded
/// with a ChaCha20 stream seeded from `seed`; a modulus under [`RSA_MIN_MODULUS_BYTES`] is refused. The key is zeroized when
/// it drops.
pub fn rsa_pkcs1_sha256_sign(pem: &str, message: &[u8], seed: [u8; 32]) -> Result<Vec<u8>, SignFailure> {
    use rand_chacha::rand_core::SeedableRng;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::signature::{RandomizedSigner, SignatureEncoding};
    use rsa::traits::PublicKeyParts;
    let key = rsa::RsaPrivateKey::from_pkcs8_pem(pem).map_err(|_| SignFailure::Key)?;
    if key.size() < RSA_MIN_MODULUS_BYTES {
        return Err(SignFailure::Key);
    }
    let signing = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(key);
    let mut rng = rand_chacha::ChaCha20Rng::from_seed(seed);
    let signature = signing.try_sign_with_rng(&mut rng, message).map_err(|_| SignFailure::Key)?;
    Ok(signature.to_vec())
}

/// An ephemeral P-256 ECDH exchange: the SEC1 uncompressed public key of the key generated here (65 bytes) and the 32-byte
/// shared x coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EcdhExchange {
    pub public: Vec<u8>,
    pub shared: [u8; 32],
}

/// Generate an ephemeral P-256 key from `seed` and derive the shared secret with `peer`, a SEC1 point (compressed or not).
/// `None` when `peer` is not a point on the curve. The ephemeral secret never leaves this function.
pub fn ecdh_p256_ephemeral(peer: &[u8], seed: [u8; 32]) -> Option<EcdhExchange> {
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use rand_chacha::rand_core::SeedableRng;
    let peer = p256::PublicKey::from_sec1_bytes(peer).ok()?;
    let mut rng = rand_chacha::ChaCha20Rng::from_seed(seed);
    let secret = p256::ecdh::EphemeralSecret::random(&mut rng);
    let public = p256::PublicKey::from(&secret).to_encoded_point(false).as_bytes().to_vec();
    let shared = secret.diffie_hellman(&peer);
    let mut out = [0u8; 32];
    out.copy_from_slice(shared.raw_secret_bytes());
    Some(EcdhExchange { public, shared: out })
}

/// AES-128-GCM seal: ciphertext followed by the 16-byte tag. `None` when the key is not 16 bytes or the nonce not 12.
pub fn aes128_gcm_seal(key: &[u8], nonce: &[u8], plaintext: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    if key.len() != 16 || nonce.len() != 12 {
        return None;
    }
    let cipher = aes_gcm::Aes128Gcm::new_from_slice(key).ok()?;
    cipher.encrypt(aes_gcm::Nonce::from_slice(nonce), Payload { msg: plaintext, aad }).ok()
}

/// AES-128-GCM open of ciphertext followed by its tag. `None` when the key or nonce is the wrong size or the tag does not
/// verify.
pub fn aes128_gcm_open(key: &[u8], nonce: &[u8], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    if key.len() != 16 || nonce.len() != 12 {
        return None;
    }
    let cipher = aes_gcm::Aes128Gcm::new_from_slice(key).ok()?;
    cipher.decrypt(aes_gcm::Nonce::from_slice(nonce), Payload { msg: sealed, aad }).ok()
}

/// One regular file of a tar archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarFile {
    pub path: String,
    pub data: Vec<u8>,
    pub executable: bool,
}

/// The regular files of a ustar archive, in archive order; directories, links and extended headers are skipped.
pub fn tar_files(archive: &[u8]) -> Result<Vec<TarFile>, String> {
    fn field(block: &[u8]) -> &[u8] {
        let end = block.iter().position(|b| *b == 0).unwrap_or(block.len());
        &block[..end]
    }
    fn octal(block: &[u8]) -> Result<usize, String> {
        let text = core::str::from_utf8(field(block)).map_err(|_| "a tar header number is not text".to_string())?;
        usize::from_str_radix(text.trim(), 8).map_err(|_| format!("a tar header number is not octal: {text:?}"))
    }
    let mut files = Vec::new();
    let mut offset = 0;
    while offset + 512 <= archive.len() {
        let header = &archive[offset..offset + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let size = octal(&header[124..136])?;
        let mode = octal(&header[100..108])?;
        let name = core::str::from_utf8(field(&header[0..100])).map_err(|_| "a tar path is not text".to_string())?;
        let prefix = core::str::from_utf8(field(&header[345..500])).map_err(|_| "a tar path is not text".to_string())?;
        let path = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
        let start = offset + 512;
        let end = start.checked_add(size).filter(|end| *end <= archive.len()).ok_or_else(|| "a tar entry runs past the archive".to_string())?;
        if matches!(header[156], 0 | b'0') {
            files.push(TarFile { path, data: archive[start..end].to_vec(), executable: mode & 0o111 != 0 });
        }
        offset = start + size.div_ceil(512) * 512;
    }
    Ok(files)
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod key_exchange_tests {
    use super::*;

    // EXEC-CRYPTO-2: a sealed operator draws its entropy from the kernel (the seed), never from the program.
    #[test]
    fn the_ephemeral_exchange_derives_the_secret_the_peer_derives() {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let peer = p256::SecretKey::from_slice(&[7u8; 32]).expect("scalar");
        let peer_public = peer.public_key().to_encoded_point(false);
        let exchange = ecdh_p256_ephemeral(peer_public.as_bytes(), [9u8; 32]).expect("exchange");
        assert_eq!(exchange.public.len(), 65);
        let theirs = p256::PublicKey::from_sec1_bytes(&exchange.public).expect("point");
        let derived = p256::ecdh::diffie_hellman(peer.to_nonzero_scalar(), theirs.as_affine());
        assert_eq!(derived.raw_secret_bytes().as_slice(), exchange.shared.as_slice());
        assert_eq!(ecdh_p256_ephemeral(peer_public.as_bytes(), [9u8; 32]), Some(exchange.clone()));
        assert_ne!(ecdh_p256_ephemeral(peer_public.as_bytes(), [8u8; 32]).expect("exchange").public, exchange.public);
        assert_eq!(ecdh_p256_ephemeral(&[4u8; 65], [9u8; 32]), None);
        assert_eq!(ecdh_p256_ephemeral(&[], [9u8; 32]), None);
    }

    // EXEC-CRYPTO-3: AES-128-GCM seal/open; null on a bad key, nonce or tag.
    #[test]
    fn aes_gcm_refuses_a_wrong_sized_key_or_nonce_and_a_forged_tag() {
        let sealed = aes128_gcm_seal(&[1u8; 16], &[2u8; 12], b"hello", b"aad").expect("sealed");
        assert_eq!(aes128_gcm_open(&[1u8; 16], &[2u8; 12], &sealed, b"aad").as_deref(), Some(&b"hello"[..]));
        assert_eq!(aes128_gcm_open(&[1u8; 16], &[2u8; 12], &sealed, b"bad"), None);
        assert_eq!(aes128_gcm_open(&[3u8; 16], &[2u8; 12], &sealed, b"aad"), None);
        assert_eq!(aes128_gcm_seal(&[1u8; 32], &[2u8; 12], b"x", b""), None);
        assert_eq!(aes128_gcm_open(&[1u8; 16], &[2u8; 8], &sealed, b"aad"), None);
    }
}

#[cfg(test)]
mod structured_codec_tests {
    use super::*;

    #[test]
    fn gzip_compression_writes_the_bytes_flate2_wrote() {
        use std::io::Write;
        let inputs: [&[u8]; 4] = [b"", b"hello", &[7u8; 70_000], b"the quick brown fox jumps over the lazy dog, again and again and again"];
        for data in inputs {
            for level in [0, 1, 6, 9] {
                let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
                encoder.write_all(data).unwrap();
                assert_eq!(gzip_compress_level(data, level), encoder.finish().unwrap(), "level {level}, {} bytes", data.len());
                assert_eq!(gzip_decode(&gzip_compress_level(data, level), data.len()).unwrap().data, data, "round trip level {level}");
            }
        }
    }

    #[test]
    fn gzip_refusal_codes_are_structural_and_complete() {
        let valid = gzip_compress(b"hello");
        let code = |bytes: &[u8]| gzip_decode(bytes, 100).unwrap_err();
        assert_eq!(code(&valid[..4]), CodecFailure::Truncated);
        let mut bad = valid.clone(); bad[0] = 0;
        assert_eq!(code(&bad), CodecFailure::Magic);
        bad = valid.clone(); bad[2] = 7;
        assert_eq!(code(&bad), CodecFailure::Method);
        bad = valid.clone(); bad[3] = 32;
        assert_eq!(code(&bad), CodecFailure::Flags);
        bad = valid.clone(); bad[10] = 7;
        assert_eq!(code(&bad), CodecFailure::Block);
        bad = valid.clone(); let n = bad.len(); bad[n - 8] ^= 1;
        assert_eq!(code(&bad), CodecFailure::Crc);
        bad = valid.clone(); bad[n - 4] ^= 1;
        assert_eq!(code(&bad), CodecFailure::Size);
        bad = valid.clone(); bad.extend_from_slice(b"junk");
        assert_eq!(code(&bad), CodecFailure::Trailing);
        bad = valid.clone(); bad[3] = 2; bad.splice(10..10, [0, 0]);
        assert_eq!(code(&bad), CodecFailure::HeaderCrc);
        let header_sum = crc32fast::hash(&bad[..10]) as u16;
        bad[10..12].copy_from_slice(&header_sum.to_le_bytes());
        assert_eq!(gzip_decode(&bad, 5).unwrap().data, b"hello");
        assert_eq!(gzip_decode(&valid, 4).unwrap_err(), CodecFailure::OutputLimit);
        assert_eq!(gzip_decode(&valid, 5).unwrap().data, b"hello");
        assert_eq!(gzip_decode(&[valid.clone(), valid].concat(), 10).unwrap().data, b"hellohello");
    }

    #[test]
    fn brotli_structured_failure_and_exact_limit_control() {
        let valid = brotli_compress(b"hello");
        assert_eq!(brotli_decode(&valid, 4).unwrap_err(), CodecFailure::OutputLimit);
        assert_eq!(brotli_decode(&valid, 5).unwrap().data, b"hello");
        assert_eq!(brotli_decode(&[255; 8], 100).unwrap_err(), CodecFailure::Block);
    }

    #[test]
    fn ed25519_signs_and_verifies_rfc8032_vector_and_refuses_a_tampered_message() {
        let seed: [u8; 32] = hex_decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60").unwrap().try_into().unwrap();
        let public = ed25519_public_key(&seed);
        assert_eq!(hex(&public), "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let signature = ed25519_sign(&seed, b"");
        assert_eq!(hex(&signature), "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b");
        assert!(ed25519_verify(&public, b"", &signature));
        assert!(!ed25519_verify(&public, b"x", &signature));
        let mut bent = signature;
        bent[0] ^= 1;
        assert!(!ed25519_verify(&public, b"", &bent));
        let mut other = seed;
        other[0] ^= 1;
        assert!(!ed25519_verify(&ed25519_public_key(&other), b"", &signature));
    }

    #[test]
    fn tar_files_reads_regular_files_with_their_mode_and_refuses_a_truncated_entry() {
        fn entry(name: &str, mode: u32, kind: u8, data: &[u8]) -> Vec<u8> {
            let mut header = vec![0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..107].copy_from_slice(format!("{mode:07o}").as_bytes());
            header[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
            header[156] = kind;
            let mut out = header;
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
            out
        }
        let mut archive = [entry("dir/", 0o755, b'5', b""), entry("dir/a.txt", 0o644, b'0', b"hello"), entry("dir/run", 0o755, b'0', b"#!")].concat();
        archive.extend_from_slice(&[0u8; 1024]);
        let files = tar_files(&archive).unwrap();
        assert_eq!(files, vec![
            TarFile { path: "dir/a.txt".into(), data: b"hello".to_vec(), executable: false },
            TarFile { path: "dir/run".into(), data: b"#!".to_vec(), executable: true },
        ]);
        assert!(tar_files(&archive[..1026]).is_err());
        assert!(tar_files(&[]).unwrap().is_empty());
    }
}

#[cfg(test)]
mod signature_verification_tests {
    use super::*;

    const RSA_N: &str = "pCJTqfijo+dyUDx+uHmap/Y5yXaRg99QNf+AUbAjWRKsgBhvqMknk2SpB9A40VwydKdtoHbWp0z4bHSNmlHooqeFd7mAxpmtsuumAiE+ICdkcPZ63j2OvqlrJna3xB4PX677BZhFYhDhYCjjlLTDwmZRBymA8vSCtDLQZ7MRkX1fxVHML+wJi1LzTwniItB/hoSKx8b4i0w4ZuesAB+/RPTjR8ktRGsUWyzW83Bcfg5IR+8bZLMNyx6XwsAuZ7nQ2+qJXSFXoq2O/a+jdtfagyJBGAD26wx6bnuReRg2x57t6nB3CYYUREhcAJAJR9BLKkNZgk/peUKeb8QHwQBgPw==";
    const RSA_E: &str = "AQAB";
    const RSA_SIGNATURE: &str = "RA4mgYzrg/dfjqlRbUcGMN4Vr7k7ChWG3Jsbpkz93OKGJfIJ8D8zpGCOvaMfc1WN0pWnwuPuSxHgsOMaC4bh1qH5tS1FJWFxL//Lg28IIrWHM/olBCAUXbhsqNvf3g7m50XNVmM8RvmMDZOXuBJkLm4GLAptJaSgUyK2pPFV0CCKx4zIFjtoL1ZvtZvwgT6er9VWheX+3wNm7MQwtHKYTH77z4XVi2K/+rkY7QNdh9GohExwK6cDy8sCuLG7Kjb1YykNxEcZxR8k/1X6NqZC1svDj1ghejKGi+Tkkf7nCfVH7lDIzE+FisrCetfZG8eNQ4ROWAmAA4gWv2Hjd+isWg==";
    const EC_X: &str = "DWCBVhKCGPAV0CVSa7Nu/svWxAWs1ejOo2PZPNjNAHE=";
    const EC_Y: &str = "786l0mh1J2aMm6STYLEz60poSi5F6aNJi4BnV6yCOMY=";
    const EC_SIGNATURE: &str = "eAQcvqokeZHzlZ0shvZjYSxtc12OnlR9IZgH9nCRSoqlNKyYQx0YNoLL0SpkJkGsbGfmF1a4OIfG0VncqUiwgQ==";
    const MESSAGE: &str = "header.payload";
    const P256_P: &str = "/////wAAAAEAAAAAAAAAAAAAAAD///////////////8=";
    const P256_N: &str = "/////wAAAAD//////////7zm+q2nF56E87nKwvxjJVE=";

    fn bytes(text: &str) -> Vec<u8> {
        base64_decode(text).unwrap()
    }

    fn flip_last_bit(mut data: Vec<u8>) -> Vec<u8> {
        let last = data.len() - 1;
        data[last] ^= 1;
        data
    }

    fn rsa(n: &[u8], e: &[u8], message: &str, signature: &[u8]) -> RsaVerdict {
        rsa_pkcs1_sha256_verify(n, e, message.as_bytes(), signature)
    }

    fn ec(x: &[u8], y: &[u8], message: &str, signature: &[u8]) -> EcdsaVerdict {
        ecdsa_p256_sha256_verify(x, y, message.as_bytes(), signature)
    }

    #[test]
    fn a_real_rs256_signature_verifies_and_its_controls_do_not() {
        let (n, e, signature) = (bytes(RSA_N), bytes(RSA_E), bytes(RSA_SIGNATURE));
        assert_eq!(rsa(&n, &e, MESSAGE, &signature), RsaVerdict::Valid);
        assert_eq!(rsa(&n, &e, MESSAGE, &flip_last_bit(signature.clone())), RsaVerdict::Mismatch);
        assert_eq!(rsa(&n, &e, "header.payloaD", &signature), RsaVerdict::Mismatch);
        assert_eq!(rsa(&n, &[3], MESSAGE, &signature), RsaVerdict::Mismatch);
    }

    #[test]
    fn an_rsa_key_or_signature_that_cannot_be_checked_is_refused_by_a_closed_reason() {
        let (n, e, signature) = (bytes(RSA_N), bytes(RSA_E), bytes(RSA_SIGNATURE));
        assert_eq!(rsa(&n[1..], &e, MESSAGE, &signature[1..]), RsaVerdict::KeySize);
        let mut padded = vec![0u8];
        padded.extend_from_slice(&n[1..]);
        assert_eq!(rsa(&padded, &e, MESSAGE, &signature), RsaVerdict::KeySize);
        assert_eq!(rsa(&n, &[], MESSAGE, &signature), RsaVerdict::KeySize);
        assert_eq!(rsa(&n, &n, MESSAGE, &signature), RsaVerdict::KeySize);
        assert_eq!(rsa(&n, &e, MESSAGE, &signature[1..]), RsaVerdict::SignatureLength);
        assert_eq!(rsa(&n, &e, MESSAGE, &signature), RsaVerdict::Valid);
        assert_eq!(rsa(&n, &e, MESSAGE, &n), RsaVerdict::SignatureRange);
        assert_eq!(rsa(&n, &e, MESSAGE, &vec![0xff; n.len()]), RsaVerdict::SignatureRange);
        assert_eq!(rsa(&n, &e, MESSAGE, &flip_last_bit(n.clone())), RsaVerdict::Mismatch);
    }

    #[test]
    fn a_real_es256_signature_verifies_and_its_controls_do_not() {
        let (x, y, signature) = (bytes(EC_X), bytes(EC_Y), bytes(EC_SIGNATURE));
        assert_eq!(ec(&x, &y, MESSAGE, &signature), EcdsaVerdict::Valid);
        assert_eq!(ec(&x, &y, MESSAGE, &flip_last_bit(signature.clone())), EcdsaVerdict::Mismatch);
        assert_eq!(ec(&x, &y, "header.payloaD", &signature), EcdsaVerdict::Mismatch);
        assert_eq!(ec(&y, &x, MESSAGE, &signature), EcdsaVerdict::Point);
    }

    #[test]
    fn an_ecdsa_key_or_signature_that_cannot_be_checked_is_refused_by_a_closed_reason() {
        let (x, y, signature) = (bytes(EC_X), bytes(EC_Y), bytes(EC_SIGNATURE));
        let (p, order) = (bytes(P256_P), bytes(P256_N));
        assert_eq!(ec(&x[1..], &y, MESSAGE, &signature), EcdsaVerdict::Length);
        assert_eq!(ec(&x, &y[1..], MESSAGE, &signature), EcdsaVerdict::Length);
        assert_eq!(ec(&x, &y, MESSAGE, &signature[1..]), EcdsaVerdict::Length);
        let mut zero_r = signature.clone();
        zero_r[..32].fill(0);
        assert_eq!(ec(&x, &y, MESSAGE, &zero_r), EcdsaVerdict::Range);
        assert_eq!(ec(&y, &x, MESSAGE, &zero_r), EcdsaVerdict::Range);
        let mut zero_s = signature.clone();
        zero_s[32..].fill(0);
        assert_eq!(ec(&x, &y, MESSAGE, &zero_s), EcdsaVerdict::Range);
        let mut big_r = signature.clone();
        big_r[..32].copy_from_slice(&order);
        assert_eq!(ec(&x, &y, MESSAGE, &big_r), EcdsaVerdict::Range);
        let mut big_s = signature.clone();
        big_s[32..].fill(0xff);
        assert_eq!(ec(&x, &y, MESSAGE, &big_s), EcdsaVerdict::Range);
        assert_eq!(ec(&p, &y, MESSAGE, &signature), EcdsaVerdict::Point);
        assert_eq!(ec(&x, &flip_last_bit(y.clone()), MESSAGE, &signature), EcdsaVerdict::Point);
        assert_eq!(ec(&x, &flip_last_bit(y.clone()), MESSAGE, &big_r), EcdsaVerdict::Point);
        assert_eq!(ec(&x, &y, MESSAGE, &signature), EcdsaVerdict::Valid);
    }
}
