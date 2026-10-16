//! The one set of wire codecs: percent-encoders, base64, SHA-256, MD5 and HMAC-SHA256. `orbital-integrations`,
//! the `http` request builder and the std operators all read these.

use base64::Engine;
use md5::Md5;
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

    #[derive(serde::Deserialize)]
    struct UriDecodeCase {
        op: String,
        args: Vec<serde_json::Value>,
        #[serde(default)]
        expect: Option<serde_json::Value>,
        #[serde(default)]
        error: bool,
        source: String,
    }

    fn eval_decode(args: &[serde_json::Value]) -> Result<serde_json::Value, String> {
        eval_op("str/decode-uri-component", args)
    }

    fn eval_op(op: &str, args: &[serde_json::Value]) -> Result<serde_json::Value, String> {
        let mut expr = vec![serde_json::json!(op)];
        expr.extend(args.iter().cloned());
        let sexpr = serde_json::from_value(serde_json::Value::Array(expr)).map_err(|e| e.to_string())?;
        let ctx = crate::BindingContextBuilder::new().build();
        crate::Evaluator::new()
            .evaluate(&sexpr, &ctx)
            .map(|v| serde_json::to_value(v).unwrap())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn decode_uri_component_runs_every_shared_vector() {
        let cases: Vec<UriDecodeCase> =
            serde_json::from_str(include_str!("../tests/fixtures/operators/uri-decode-vectors.json")).unwrap();
        assert!(cases.iter().any(|c| c.error));
        assert!(cases.iter().any(|c| c.source.starts_with("round trip")));
        for case in cases.iter().filter(|c| c.op == "str/decode-uri-component") {
            let got = eval_decode(&case.args);
            let valid = eval_op("str/uri-component-valid", &case.args);
            if case.args[0].is_number() {
                assert!(valid.is_err(), "{}", case.source);
            } else {
                let expected = !case.error && !case.args[0].is_null();
                assert_eq!(valid, Ok(serde_json::json!(expected)), "{:?} ({})", case.args, case.source);
            }
            if case.error {
                let message = got.expect_err(&format!("{:?} must error ({})", case.args, case.source));
                assert!(message.contains("URI malformed") || message.contains("expected string"), "{message}");
            } else {
                let expected = case.expect.clone().unwrap_or(serde_json::Value::Null);
                assert_eq!(got.as_ref().ok(), Some(&expected), "{:?} ({})", case.args, case.source);
            }
        }
    }

    #[test]
    fn uri_component_valid_runs_its_shared_vectors() {
        let cases: Vec<UriDecodeCase> =
            serde_json::from_str(include_str!("../tests/fixtures/operators/uri-decode-vectors.json")).unwrap();
        let own: Vec<_> = cases.iter().filter(|c| c.op == "str/uri-component-valid").collect();
        assert!(own.iter().any(|c| c.error) && own.iter().any(|c| c.expect == Some(serde_json::json!(false))));
        for case in own {
            let got = eval_op(&case.op, &case.args);
            if case.error {
                assert!(got.is_err(), "{}", case.source);
            } else {
                assert_eq!(got, Ok(case.expect.clone().unwrap()), "{:?} ({})", case.args, case.source);
            }
        }
        assert!(eval_op("str/uri-component-valid", &[]).is_err());
    }

    #[test]
    fn decode_uri_component_arity_and_control() {
        assert!(eval_decode(&[]).is_err());
        assert!(eval_decode(&[serde_json::json!("a"), serde_json::json!("b")]).is_err());
        assert_eq!(decode_uri_component("%25").unwrap(), "%");
        assert_eq!(decode_uri_component("%C3%A9+").unwrap(), "é+");
        assert_eq!(decode_uri_component("%C3").unwrap_err(), "URI malformed");
    }

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
    fn md5_runs_every_shared_vector() {
        let cases: Vec<UriDecodeCase> =
            serde_json::from_str(include_str!("../tests/fixtures/operators/md5-vectors.json")).unwrap();
        assert!(cases.iter().any(|c| c.error) && cases.iter().any(|c| c.source.starts_with("RFC 1321")));
        for case in &cases {
            assert_eq!(case.op, "hash/md5");
            let got = eval_op(&case.op, &case.args);
            if case.error {
                assert!(got.is_err(), "{:?} must error ({})", case.args, case.source);
            } else {
                assert_eq!(got.as_ref().ok(), case.expect.as_ref(), "{:?} ({})", case.args, case.source);
            }
        }
        assert!(eval_op("hash/md5", &[serde_json::json!("YWJj")]).is_err());
    }

    #[test]
    fn md5_matches_rfc_1321() {
        for (input, expected) in [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            ("abcdefghijklmnopqrstuvwxyz", "c3fcd3d76192e4007dfb496cca67e13b"),
            ("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789", "d174ab98d277d9f5a5611c2c9f419d9f"),
            ("12345678901234567890123456789012345678901234567890123456789012345678901234567890", "57edf4a22be3c955ac49da2e2107b67a"),
        ] {
            assert_eq!(hex(&md5(input.as_bytes())), expected, "{input:?}");
        }
        assert_eq!(hex(&md5(&[0x00, 0xff, 0x80, 0x7f])), "35913419231430075897c28e5e159b65");
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
