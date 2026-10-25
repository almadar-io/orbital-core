use base64::Engine;
#[allow(unused_imports)]
use orbital_core_base::codec::*;
use sha2::Digest;

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
        #[serde(default)]
        null: bool,
        source: String,
    }

    fn eval_decode(args: &[serde_json::Value]) -> Result<serde_json::Value, String> {
        eval_op("str/decode-uri-component", args)
    }

    fn eval_op(op: &str, args: &[serde_json::Value]) -> Result<serde_json::Value, String> {
        eval_in(op, args, crate::evaluator::context::BindingContextBuilder::new().build())
    }

    fn eval_in(op: &str, args: &[serde_json::Value], ctx: crate::evaluator::context::BindingContext) -> Result<serde_json::Value, String> {
        let mut expr = vec![serde_json::json!(op)];
        expr.extend(args.iter().cloned());
        let sexpr =
            serde_json::from_value(serde_json::Value::Array(expr)).map_err(|e| e.to_string())?;
        crate::evaluator::Evaluator::new()
            .evaluate(&sexpr, &ctx)
            .map(|v| serde_json::to_value(v).unwrap())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn decode_uri_component_runs_every_shared_vector() {
        let cases: Vec<UriDecodeCase> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/operators/uri-decode-vectors.json"
        ))
        .unwrap();
        assert!(cases.iter().any(|c| c.error) && cases.iter().any(|c| c.null));
        assert!(cases.iter().any(|c| c.source.starts_with("round trip")));
        for case in cases.iter().filter(|c| c.op == "str/decode-uri-component") {
            let got = eval_decode(&case.args);
            let valid = eval_op("str/uri-component-valid", &case.args);
            if case.args[0].is_number() {
                assert!(valid.is_err(), "{}", case.source);
            } else {
                let expected = !case.error && !case.null && !case.args[0].is_null();
                assert_eq!(
                    valid,
                    Ok(serde_json::json!(expected)),
                    "{:?} ({})",
                    case.args,
                    case.source
                );
            }
            if case.null {
                assert_eq!(got, Ok(serde_json::Value::Null), "{:?} ({})", case.args, case.source);
            } else if case.error {
                let message =
                    got.expect_err(&format!("{:?} must error ({})", case.args, case.source));
                assert!(
                    message.contains("URI malformed") || message.contains("expected string"),
                    "{message}"
                );
            } else {
                let expected = case.expect.clone().unwrap_or(serde_json::Value::Null);
                assert_eq!(
                    got.as_ref().ok(),
                    Some(&expected),
                    "{:?} ({})",
                    case.args,
                    case.source
                );
            }
        }
    }

    #[test]
    fn uri_component_valid_runs_its_shared_vectors() {
        let cases: Vec<UriDecodeCase> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/operators/uri-decode-vectors.json"
        ))
        .unwrap();
        let own: Vec<_> = cases
            .iter()
            .filter(|c| c.op == "str/uri-component-valid")
            .collect();
        assert!(
            own.iter().any(|c| c.error)
                && own
                    .iter()
                    .any(|c| c.expect == Some(serde_json::json!(false)))
        );
        for case in own {
            let got = eval_op(&case.op, &case.args);
            if case.error {
                assert!(got.is_err(), "{}", case.source);
            } else {
                assert_eq!(
                    got,
                    Ok(case.expect.clone().unwrap()),
                    "{:?} ({})",
                    case.args,
                    case.source
                );
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
        assert_eq!(
            form_urlencode_value("a b!~'()é"),
            "a+b%21%7E%27%28%29%C3%A9"
        );
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
        for (plain, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), enc);
            assert_eq!(base64_decode(enc).unwrap(), plain.as_bytes());
        }
        assert!(base64_decode("!!!").is_err());
    }

    #[test]
    fn base64_decode_is_strict_rfc_4648() {
        for bad in [
            "Zg", "Zm9", "Zh==", "Zm 9v", "Zm9v\n", "-_8=", "Zg=", "=Zg=",
        ] {
            assert!(base64_decode(bad).is_err(), "{bad:?} must be rejected");
        }
        assert_eq!(base64_decode("+/8=").unwrap(), vec![0xfb, 0xff]);
    }

    #[test]
    fn md5_runs_every_shared_vector() {
        let cases: Vec<UriDecodeCase> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/operators/md5-vectors.json"
        ))
        .unwrap();
        assert!(
            cases.iter().any(|c| c.error)
                && cases.iter().any(|c| c.null)
                && cases.iter().any(|c| c.source.starts_with("RFC 1321"))
        );
        for case in &cases {
            assert_eq!(case.op, "hash/md5");
            let got = eval_op(&case.op, &case.args);
            if case.null {
                assert_eq!(got, Ok(serde_json::Value::Null), "{:?} ({})", case.args, case.source);
            } else if case.error {
                assert!(got.is_err(), "{:?} must error ({})", case.args, case.source);
            } else {
                assert_eq!(
                    got.as_ref().ok(),
                    case.expect.as_ref(),
                    "{:?} ({})",
                    case.args,
                    case.source
                );
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
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ] {
            assert_eq!(hex(&md5(input.as_bytes())), expected, "{input:?}");
        }
        assert_eq!(
            hex(&md5(&[0x00, 0xff, 0x80, 0x7f])),
            "35913419231430075897c28e5e159b65"
        );
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
        let mac = hmac_sha256(
            &long_key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn control_a_different_key_gives_a_different_mac() {
        assert_ne!(hmac_sha256(b"a", b"m"), hmac_sha256(b"b", b"m"));
    }

    #[test]
    fn pbkdf2_hmac_sha256_matches_the_known_vectors() {
        for (iterations, want) in [
            (1, "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"),
            (2, "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"),
            (4096, "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"),
        ] {
            assert_eq!(hex(&pbkdf2_hmac_sha256(b"password", b"salt", iterations, 32)), want, "{iterations}");
        }
    }

    #[test]
    fn pbkdf2_longer_output_continues_the_block_sequence_and_control_differs_by_salt() {
        let long = pbkdf2_hmac_sha256(b"password", b"salt", 1, 40);
        assert_eq!(long.len(), 40);
        assert_eq!(&long[..32], &pbkdf2_hmac_sha256(b"password", b"salt", 1, 32)[..]);
        assert_ne!(pbkdf2_hmac_sha256(b"password", b"pepper", 1, 32), pbkdf2_hmac_sha256(b"password", b"salt", 1, 32));
    }

    const API_KEY_VECTORS: [(&str, &str, &str); 4] = [
        ("00112233445566778899", "sk_live_abababababababababababababababababababababababababababababababab", "a492cbeb35eff51937467da2c2ebd3357833764db320d250ab133403456b1537"),
        ("00112233445566778899", "sk_live_", "010bbcf1a211930bca10afa35c740b1c70dcb058711e165c9918f1137c97b940"),
        ("00112233445566778899", "héllo wörld", "5a781e12503077b2d0d1bf683ca2ba77737fdeaca90df36fc5ef9c5fd72dfde7"),
        ("AbCdEf0123456789", "sk_live_0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f", "1dcc68a241b44c82ab715bbab75dc0ab28643e69111a574a65f1f072a0ff47b5"),
    ];

    fn scrypt_of(plaintext: &str, credential: &str, held: Option<(&str, &str)>) -> Result<serde_json::Value, String> {
        let mut ctx = crate::evaluator::context::BindingContextBuilder::new().build();
        if let Some((name, value)) = held {
            ctx = ctx.with_secret(name, value.to_string());
        }
        eval_in("hash/scrypt", &[serde_json::json!(plaintext), serde_json::json!(credential)], ctx)
    }

    #[test]
    fn scrypt_reproduces_every_hash_node_scrypt_sync_made_for_auth_api_key() {
        for (salt, plaintext, want) in API_KEY_VECTORS {
            let key = scrypt_api_key(plaintext.as_bytes(), &hex_decode(salt).unwrap()).unwrap();
            assert_eq!(hex(&key), want, "{plaintext}");
            assert_eq!(scrypt_of(plaintext, "SALT", Some(("SALT", salt))), Ok(serde_json::json!(want)));
        }
        assert_eq!((SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, SCRYPT_KEYLEN), (14, 8, 1, 32));
    }

    #[test]
    fn scrypt_changes_with_the_salt_and_the_input_and_is_null_for_a_credential_the_context_does_not_hold_as_hex() {
        let (salt, plaintext, want) = API_KEY_VECTORS[0];
        assert_ne!(scrypt_of(plaintext, "SALT", Some(("SALT", "ff00"))).unwrap(), serde_json::json!(want));
        assert_ne!(scrypt_of("sk_live_x", "SALT", Some(("SALT", salt))).unwrap(), serde_json::json!(want));
        for bad in ["", "abc", "not-hex", "zz", "0g"] {
            assert_eq!(scrypt_of(plaintext, "SALT", Some(("SALT", bad))), Ok(serde_json::Value::Null), "{bad:?}");
        }
        assert_eq!(scrypt_of(plaintext, "SALT", None), Ok(serde_json::Value::Null), "no value held");
        assert_eq!(scrypt_of(plaintext, "OTHER", Some(("SALT", salt))), Ok(serde_json::Value::Null), "another credential's value is not used");
        assert!(eval_op("hash/scrypt", &[serde_json::json!(plaintext)]).is_err());
        assert!(scrypt_of("x", "SALT", Some(("SALT", salt))).is_ok());
        assert!(eval_op("hash/scrypt", &[serde_json::json!(5), serde_json::json!("SALT")]).is_err());
    }

    #[test]
    fn hex_decode_takes_either_case_and_only_whole_bytes() {
        assert_eq!(hex_decode("00FfaB"), Some(vec![0x00, 0xff, 0xab]));
        for bad in ["", "a", "abc", "gg", "é1"] {
            assert_eq!(hex_decode(bad), None, "{bad:?}");
        }
    }
}
