//! The one set of wire codecs: percent-encoders, base64, SHA-256 and HMAC-SHA256. `orbital-integrations`,
//! the `http` request builder and the std operators all read these.

use base64::Engine;
use sha2::{Digest, Sha256};

pub mod jwt;
pub mod lenient;
pub mod sigv4;
pub mod webpush;

fn push_escaped(out: &mut String, byte: u8) {
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

const BLOCK: usize = 64;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_keeps_the_js_unreserved_set() {
        assert_eq!(
            encode_uri_component("a b&c=d/é!~*'()-_."),
            "a%20b%26c%3Dd%2F%C3%A9!~*'()-_."
        );
    }

    #[test]
    fn form_value_turns_spaces_into_plus_and_escapes_the_rest() {
        assert_eq!(form_urlencode_value("a b!~'()é"), "a+b%21%7E%27%28%29%C3%A9");
        assert_eq!(form_urlencode_value("{\"a\":1}"), "%7B%22a%22%3A1%7D");
    }

    #[test]
    fn form_pairs_keep_order_and_encode_keys_and_values() {
        let pairs = vec![
            ("b k".to_string(), "1&2".to_string()),
            ("a".to_string(), String::new()),
        ];
        assert_eq!(form_urlencode(&pairs), "b+k=1%262&a=");
        assert_eq!(form_urlencode(&[]), "");
    }

    #[test]
    fn rfc3986_also_escapes_the_js_reserved_marks() {
        assert_eq!(
            encode_rfc3986("a!b'c(d)e*f~g_h-i.j k"),
            "a%21b%27c%28d%29e%2Af~g_h-i.j%20k"
        );
    }

    #[test]
    fn base64_matches_rfc_4648_vectors_and_rejects_garbage() {
        for (plain, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64_encode(plain.as_bytes()), enc);
            assert_eq!(base64_decode(enc).unwrap(), plain.as_bytes());
        }
        assert!(base64_decode("!!!").is_err());
    }

    #[test]
    fn base64_decode_is_strict_rfc_4648() {
        for bad in ["Zg", "Zm9", "Zh==", "Zm 9v", "Zm9v\n", "-_8=", "Zg=", "=Zg="] {
            assert!(base64_decode(bad).is_err(), "{bad:?} must be rejected");
        }
        assert_eq!(base64_decode("+/8=").unwrap(), vec![0xfb, 0xff]);
    }

    #[test]
    fn sha256_matches_fips_180_2() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn rfc_4231_test_case_2() {
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn rfc_4231_test_case_6_a_key_longer_than_the_block_is_hashed_first() {
        let long_key = vec![0xaau8; 131];
        let mac = hmac_sha256(&long_key, b"Test Using Larger Than Block-Size Key - Hash Key First");
        assert_eq!(
            hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn control_a_different_key_gives_a_different_mac() {
        assert_ne!(hmac_sha256(b"a", b"m"), hmac_sha256(b"b", b"m"));
    }
}
