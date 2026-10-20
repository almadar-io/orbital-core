//! The one set of wire codecs: percent-encoders, base64, SHA-256, MD5 and HMAC-SHA256. The `http` request builder
//! and the std operators all read these.

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
    let params = scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, SCRYPT_KEYLEN).map_err(|e| e.to_string())?;
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
pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, out_len: usize) -> Vec<u8> {
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

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
