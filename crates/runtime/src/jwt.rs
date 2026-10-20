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

pub use orbital_core_base::jws::{is_unsigned, JwsRefusal, KeySet, VerifyKey};

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

fn pem_der(pem: &str) -> Result<(&str, Vec<u8>), JwtError> {
    orbital_core_base::jws::pem_der(pem).ok_or(JwtError::InvalidKey)
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

// ----- verification --------------------------------------------------------------------------------

/// Why a token was refused. `reason` is the stable word the shared token fixture (`@almadar/auth` twin) names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtVerifyError {
    Malformed(String),
    /// The header names an algorithm other than RS256 or ES256, or one the key at hand cannot verify.
    Algorithm(String),
    UnknownKey(Option<String>),
    Signature,
    Expired,
    NotYetValid,
    Issuer,
    Audience,
    Subject,
}

impl JwtVerifyError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "malformed",
            Self::Algorithm(_) => "algorithm",
            Self::UnknownKey(_) => "unknown-key",
            Self::Signature => "signature",
            Self::Expired => "expired",
            Self::NotYetValid => "not-yet-valid",
            Self::Issuer => "issuer",
            Self::Audience => "audience",
            Self::Subject => "subject",
        }
    }
}

impl fmt::Display for JwtVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed token: {why}"),
            Self::Algorithm(alg) => write!(f, "algorithm {alg} is not accepted"),
            Self::UnknownKey(Some(kid)) => write!(f, "no key with id {kid}"),
            Self::UnknownKey(None) => f.write_str("no key matches the token"),
            Self::Signature => f.write_str("signature verification failed"),
            Self::Expired => f.write_str("token has expired"),
            Self::NotYetValid => f.write_str("token is not valid yet"),
            Self::Issuer => f.write_str("unexpected issuer"),
            Self::Audience => f.write_str("unexpected audience"),
            Self::Subject => f.write_str("token has no subject"),
        }
    }
}

impl From<JwsRefusal> for JwtVerifyError {
    fn from(refusal: JwsRefusal) -> Self {
        match refusal {
            JwsRefusal::Malformed(why) => Self::Malformed(why),
            JwsRefusal::Algorithm(alg) => Self::Algorithm(alg),
            JwsRefusal::UnknownKey(kid) => Self::UnknownKey(kid),
            JwsRefusal::Signature => Self::Signature,
        }
    }
}

/// What a verified token has to satisfy beyond its signature.
pub struct VerifyPolicy<'a> {
    pub issuer: &'a str,
    pub audience: &'a str,
    /// The clock, epoch seconds.
    pub now: i64,
}

fn claim_seconds(claims: &Value, name: &str) -> Option<f64> {
    claims.get(name).and_then(Value::as_f64)
}

/// Verify a compact JWS (RS256 or ES256 only) against `keys`, then its `exp`, `nbf`, `iss`, `aud` and `sub`; the claims
/// as JSON. The algorithm comes from the header but must be one the selected key can verify, so `none` and the HMAC
/// algorithms are refused and a public key is never an HMAC secret.
pub fn verify_compact(token: &str, keys: &KeySet, policy: &VerifyPolicy<'_>) -> Result<Value, JwtVerifyError> {
    let parts = orbital_core_base::jws::verify_signature(token, keys)?;
    check_claims(parts.claims_json()?, policy)
}

/// The claims of an UNSIGNED token (`alg: none`) after the same claim checks as a signed one. Only a host that has been told
/// it talks to the Firebase Auth emulator may call this: nothing about the token vouches for it.
pub fn verify_unsigned(token: &str, policy: &VerifyPolicy<'_>) -> Result<Value, JwtVerifyError> {
    if !is_unsigned(token) {
        return Err(JwtVerifyError::Algorithm("a signed token".to_string()));
    }
    let parts = orbital_core_base::jws::split_compact(token)?;
    check_claims(parts.claims_json()?, policy)
}

/// `exp` (required), `nbf`, `iss`, `aud` and `sub` of `claims`.
fn check_claims(claims: Value, policy: &VerifyPolicy<'_>) -> Result<Value, JwtVerifyError> {
    let now = policy.now as f64;
    let exp = claim_seconds(&claims, "exp").ok_or_else(|| JwtVerifyError::Malformed("no `exp`".to_string()))?;
    if exp <= now {
        return Err(JwtVerifyError::Expired);
    }
    if claim_seconds(&claims, "nbf").is_some_and(|nbf| nbf > now) {
        return Err(JwtVerifyError::NotYetValid);
    }
    if claims.get("iss").and_then(Value::as_str) != Some(policy.issuer) {
        return Err(JwtVerifyError::Issuer);
    }
    let audience_ok = match claims.get("aud") {
        Some(Value::String(aud)) => aud == policy.audience,
        Some(Value::Array(list)) => list.iter().any(|aud| aud.as_str() == Some(policy.audience)),
        _ => false,
    };
    if !audience_ok {
        return Err(JwtVerifyError::Audience);
    }
    if claims.get("sub").and_then(Value::as_str).map_or(true, str::is_empty) {
        return Err(JwtVerifyError::Subject);
    }
    Ok(claims)
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
