//! The one set of wire codecs: percent-encoders, base64, SHA-256, MD5, HMAC-SHA256, gzip and brotli. The `http` request builder
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

pub fn gzip_compress_level(data: &[u8], level: u32) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
    encoder.write_all(data).and_then(|()| encoder.finish()).expect("gzip into memory cannot fail")
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
            let mut crc = flate2::Crc::new(); crc.update(&data[start..offset]);
            let header_crc = data.get(offset..offset + 2).ok_or(CodecFailure::Truncated)?;
            if u16::from_le_bytes([header_crc[0], header_crc[1]]) != crc.sum() as u16 { return Err(CodecFailure::HeaderCrc); }
            offset += 2;
        }
        let mut decoder = flate2::Decompress::new(false);
        let mut crc = flate2::Crc::new();
        let member_start = out.len();
        loop {
            let mut buffer = [0; 8192];
            let before_in = decoder.total_in();
            let before_out = decoder.total_out();
            let status = decoder.decompress(&data[offset..], &mut buffer, flate2::FlushDecompress::None).map_err(|_| CodecFailure::Block)?;
            let consumed = (decoder.total_in() - before_in) as usize;
            let produced = (decoder.total_out() - before_out) as usize;
            offset += consumed;
            if produced > max - out.len() { return Err(CodecFailure::OutputLimit); }
            crc.update(&buffer[..produced]);
            out.extend_from_slice(&buffer[..produced]);
            if status == flate2::Status::StreamEnd { break; }
            if consumed == 0 && produced == 0 { return Err(if offset == data.len() { CodecFailure::Truncated } else { CodecFailure::Block }); }
        }
        let trailer = data.get(offset..offset + 8).ok_or(CodecFailure::Truncated)?;
        let expected_crc = u32::from_le_bytes(trailer[..4].try_into().expect("four bytes"));
        let expected_size = u32::from_le_bytes(trailer[4..].try_into().expect("four bytes"));
        if crc.sum() != expected_crc { return Err(CodecFailure::Crc); }
        if (out.len() - member_start) as u32 != expected_size { return Err(CodecFailure::Size); }
        offset += 8;
        if offset == data.len() { return Ok(CodecDecoded { data: out, crc: Some(crc.sum()) }); }
    }
}

pub fn brotli_compress(data: &[u8]) -> Vec<u8> {
    brotli_compress_options(data, 11, 22)
}

pub fn brotli_compress_options(data: &[u8], quality: i32, window: i32) -> Vec<u8> {
    let params = brotli::enc::BrotliEncoderParams { quality, lgwin: window, ..Default::default() };
    let mut out = Vec::new();
    brotli::BrotliCompress(&mut &data[..], &mut out, &params).expect("brotli into memory cannot fail");
    out
}

pub fn brotli_decompress(data: &[u8], max: usize) -> Result<Vec<u8>, String> {
    brotli_decode(data, max).map(|out| out.data).map_err(|error| error.code().to_string())
}

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

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod structured_codec_tests {
    use super::*;

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
        let mut crc = flate2::Crc::new(); crc.update(&bad[..10]);
        bad[10..12].copy_from_slice(&(crc.sum() as u16).to_le_bytes());
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
}
