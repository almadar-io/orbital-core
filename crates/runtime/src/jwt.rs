//! JWS compact tokens signed with a PEM private key: RS256 (RSASSA-PKCS1-v1_5 over SHA-256, RFC 7518 3.3) and
//! ES256 (ECDSA P-256, RFC 7518 3.4), plus the Google service-account assertion. Pure and wasm-compatible
//! (RustCrypto `rsa`/`p256`); RS256 is deterministic, ES256 is RFC 6979 deterministic.

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::SecretKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::sha2::{Digest, Sha256};
use rsa::{Pkcs1v15Sign, RsaPrivateKey};
use serde_json::Value;

use crate::lenient::base64_decode_lenient;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtError {
    /// The PEM is missing, malformed, or not a key of the algorithm's type.
    InvalidKey,
    Sign(String),
}

impl fmt::Display for JwtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey => f.write_str("invalid private key"),
            Self::Sign(message) => f.write_str(message),
        }
    }
}

/// The two fields of a Google service-account key file the JWT grant reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccountKey {
    pub client_email: Option<String>,
    pub private_key: Option<String>,
}

impl ServiceAccountKey {
    pub fn parse(json_text: &str) -> Result<Self, String> {
        let key: Value = serde_json::from_str(json_text).map_err(|e| e.to_string())?;
        let field = |name: &str| key.get(name).and_then(Value::as_str).map(str::to_string);
        Ok(Self {
            client_email: field("client_email"),
            private_key: field("private_key"),
        })
    }
}

/// `(label, DER)` of the first PEM block.
fn pem_der(pem: &str) -> Result<(&str, Vec<u8>), JwtError> {
    let start = pem.find("-----BEGIN ").ok_or(JwtError::InvalidKey)?;
    let label_end = pem[start + 11..].find("-----").ok_or(JwtError::InvalidKey)? + start + 11;
    let label = &pem[start + 11..label_end];
    let body_end = pem.find("-----END ").ok_or(JwtError::InvalidKey)?;
    if body_end < label_end + 5 {
        return Err(JwtError::InvalidKey);
    }
    let body: String = pem[label_end + 5..body_end].split_whitespace().collect();
    Ok((label, base64_decode_lenient(&body)))
}

/// RS256 over `message`; `pem` is PKCS#1 (`RSA PRIVATE KEY`) or PKCS#8.
pub fn sign_rs256(pem: &str, message: &[u8]) -> Result<Vec<u8>, JwtError> {
    let (label, der) = pem_der(pem)?;
    let key = if label == "RSA PRIVATE KEY" {
        RsaPrivateKey::from_pkcs1_der(&der)
    } else {
        RsaPrivateKey::from_pkcs8_der(&der).or_else(|_| RsaPrivateKey::from_pkcs1_der(&der))
    }
    .map_err(|_| JwtError::InvalidKey)?;
    sign_rs256_key(&key, message)
}

pub fn sign_rs256_key(key: &RsaPrivateKey, message: &[u8]) -> Result<Vec<u8>, JwtError> {
    key.sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(message))
        .map_err(|e| JwtError::Sign(e.to_string()))
}

/// ES256 over `message`: the 64-byte `r || s` JWS form. `pem` is SEC1 (`EC PRIVATE KEY`) or PKCS#8.
pub fn sign_es256(pem: &str, message: &[u8]) -> Result<Vec<u8>, JwtError> {
    let (label, der) = pem_der(pem)?;
    let secret = if label == "EC PRIVATE KEY" {
        SecretKey::from_sec1_der(&der).map_err(|_| JwtError::InvalidKey)?
    } else {
        SecretKey::from_pkcs8_der(&der).map_err(|_| JwtError::InvalidKey)?
    };
    let signature: Signature = SigningKey::from(secret).sign(message);
    Ok(signature.to_bytes().to_vec())
}

fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// `base64url(header).base64url(claims).base64url(signature)` with the header `{"alg":"<alg>"}`; `claims_json`
/// is the exact payload text.
pub fn compact(alg: JwtAlg, claims_json: &str, pem: &str) -> Result<String, JwtError> {
    let header = format!("{{\"alg\":\"{}\"}}", alg.name());
    let signing_input = format!("{}.{}", b64url(header.as_bytes()), b64url(claims_json.as_bytes()));
    let signature = match alg {
        JwtAlg::Rs256 => sign_rs256(pem, signing_input.as_bytes())?,
        JwtAlg::Es256 => sign_es256(pem, signing_input.as_bytes())?,
    };
    Ok(format!("{signing_input}.{}", b64url(&signature)))
}

pub struct ServiceAccountClaims<'a> {
    pub iss: &'a str,
    pub scope: &'a str,
    pub aud: &'a str,
    /// Issued-at, epoch seconds; `exp` is `iat + 3600`.
    pub iat: u64,
    pub sub: Option<&'a str>,
}

/// The RS256 JWT-bearer assertion of a Google service account (`google.auth.JWT`).
pub fn service_account_assertion(
    claims: &ServiceAccountClaims,
    pem: &str,
) -> Result<String, JwtError> {
    let quoted = |text: &str| Value::String(text.to_string()).to_string();
    let sub = claims
        .sub
        .map_or_else(String::new, |sub| format!(",\"sub\":{}", quoted(sub)));
    let payload = format!(
        "{{\"iss\":{},\"scope\":{},\"aud\":{},\"exp\":{},\"iat\":{}{sub}}}",
        quoted(claims.iss),
        quoted(claims.scope),
        quoted(claims.aud),
        claims.iat + 3600,
        claims.iat,
    );
    compact(JwtAlg::Rs256, &payload, pem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;
    use p256::pkcs8::DecodePublicKey;
    use base64::engine::general_purpose::STANDARD;

    const RS_PKCS8: &str = include_str!("../../../src/codec/fixtures/rs256-pkcs8.pem");
    const RS_PKCS1: &str = include_str!("../../../src/codec/fixtures/rs256-pkcs1.pem");
    const RS_SIG: &str = include_str!("../../../src/codec/fixtures/rs256-sig.b64");
    const EC_PKCS8: &str = include_str!("../../../src/codec/fixtures/es256-pkcs8.pem");
    const EC_SEC1: &str = include_str!("../../../src/codec/fixtures/es256-sec1.pem");
    const EC_PUBLIC: &str = include_str!("../../../src/codec/fixtures/es256-public.pem");
    const EC_OPENSSL_SIG: &str = include_str!("../../../src/codec/fixtures/es256-openssl-sig.b64");
    const MESSAGE: &[u8] = b"eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJhIn0";

    #[test]
    fn rs256_matches_the_openssl_known_answer_for_pkcs8_and_pkcs1() {
        let expected = STANDARD.decode(RS_SIG).unwrap();
        assert_eq!(sign_rs256(RS_PKCS8, MESSAGE).unwrap(), expected);
        assert_eq!(sign_rs256(RS_PKCS1, MESSAGE).unwrap(), expected);
    }

    #[test]
    fn rs256_is_deterministic_and_message_bound() {
        let a = sign_rs256(RS_PKCS8, MESSAGE).unwrap();
        assert_eq!(a, sign_rs256(RS_PKCS8, MESSAGE).unwrap());
        assert_ne!(a, sign_rs256(RS_PKCS8, b"other").unwrap());
    }

    #[test]
    fn rs256_refuses_bad_keys() {
        assert_eq!(sign_rs256("", MESSAGE), Err(JwtError::InvalidKey));
        assert_eq!(sign_rs256("not a pem", MESSAGE), Err(JwtError::InvalidKey));
        assert_eq!(
            sign_rs256("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----", MESSAGE),
            Err(JwtError::InvalidKey)
        );
        assert_eq!(sign_rs256(EC_PKCS8, MESSAGE), Err(JwtError::InvalidKey));
    }

    #[test]
    fn es256_signs_verifiably_in_pkcs8_and_sec1() {
        let public = VerifyingKey::from_public_key_pem(EC_PUBLIC).unwrap();
        for pem in [EC_PKCS8, EC_SEC1] {
            let raw = sign_es256(pem, MESSAGE).unwrap();
            assert_eq!(raw.len(), 64);
            public.verify(MESSAGE, &Signature::from_slice(&raw).unwrap()).unwrap();
            assert!(public.verify(b"other", &Signature::from_slice(&raw).unwrap()).is_err());
        }
        assert_eq!(sign_es256(EC_PKCS8, MESSAGE).unwrap(), sign_es256(EC_SEC1, MESSAGE).unwrap());
    }

    #[test]
    fn es256_verifies_an_independent_openssl_signature_and_refuses_bad_keys() {
        let public = VerifyingKey::from_public_key_pem(EC_PUBLIC).unwrap();
        let der = STANDARD.decode(EC_OPENSSL_SIG).unwrap();
        public.verify(MESSAGE, &Signature::from_der(&der).unwrap()).unwrap();
        assert_eq!(sign_es256(RS_PKCS8, MESSAGE), Err(JwtError::InvalidKey));
        assert_eq!(sign_es256("", MESSAGE), Err(JwtError::InvalidKey));
    }

    #[test]
    fn the_service_account_assertion_has_the_documented_claims() {
        let token = service_account_assertion(
            &ServiceAccountClaims {
                iss: "sa@p.iam.gserviceaccount.com",
                scope: "https://www.googleapis.com/auth/drive",
                aud: "https://www.googleapis.com/oauth2/v4/token",
                iat: 1_700_000_000,
                sub: Some("user@example.com"),
            },
            RS_PKCS8,
        )
        .unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(URL_SAFE_NO_PAD.decode(parts[0]).unwrap(), br#"{"alg":"RS256"}"#);
        assert_eq!(
            String::from_utf8(URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap(),
            r#"{"iss":"sa@p.iam.gserviceaccount.com","scope":"https://www.googleapis.com/auth/drive","aud":"https://www.googleapis.com/oauth2/v4/token","exp":1700003600,"iat":1700000000,"sub":"user@example.com"}"#
        );
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        assert_eq!(
            URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
            sign_rs256(RS_PKCS8, signing_input.as_bytes()).unwrap()
        );
    }

    #[test]
    fn the_assertion_omits_sub_when_absent() {
        let token = service_account_assertion(
            &ServiceAccountClaims { iss: "a", scope: "s", aud: "u", iat: 1, sub: None },
            RS_PKCS8,
        )
        .unwrap();
        let claims = String::from_utf8(URL_SAFE_NO_PAD.decode(token.split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!(claims, r#"{"iss":"a","scope":"s","aud":"u","exp":3601,"iat":1}"#);
    }

    #[test]
    fn es256_compact_token_verifies() {
        let token = compact(JwtAlg::Es256, r#"{"a":1}"#, EC_PKCS8).unwrap();
        let (input, sig) = token.rsplit_once('.').unwrap();
        let public = VerifyingKey::from_public_key_pem(EC_PUBLIC).unwrap();
        let raw = URL_SAFE_NO_PAD.decode(sig).unwrap();
        public.verify(input.as_bytes(), &Signature::from_slice(&raw).unwrap()).unwrap();
        assert!(token.starts_with(&b64url(br#"{"alg":"ES256"}"#)));
    }

    #[test]
    fn service_account_key_parses_the_two_fields() {
        let key = ServiceAccountKey::parse(r#"{"client_email":"e","private_key":"k","x":1}"#).unwrap();
        assert_eq!(key.client_email.as_deref(), Some("e"));
        assert_eq!(key.private_key.as_deref(), Some("k"));
        let none = ServiceAccountKey::parse("{}").unwrap();
        assert!(none.client_email.is_none() && none.private_key.is_none());
        assert!(ServiceAccountKey::parse("nope").is_err());
    }
}

#[allow(unused_imports)]
pub use orbital_core_base::jwt_alg::{JwtAlg};
