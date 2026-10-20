//! JWS compact tokens checked against a published key set: RS256 (RSASSA-PKCS1-v1_5 over SHA-256, RFC 7518 3.3)
//! and ES256 (ECDSA P-256, RFC 7518 3.4), and the key documents issuers publish (a JWK Set, a `{kid: certificate}`
//! map). Signature only: claim policy (expiry, issuer, audience) belongs to the caller. Pure and wasm-compatible.

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::Signature;
use rsa::sha2::{Digest, Sha256};
use rsa::traits::PublicKeyParts;
use rsa::Pkcs1v15Sign;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::codec::base64_decode_lenient;
use crate::jwt_alg::JwtAlg;

/// Why a token or key document was refused. `reason` is the stable word the shared token fixture names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwsRefusal {
    Malformed(String),
    /// The header names an algorithm other than RS256 or ES256, or one the selected key cannot verify.
    Algorithm(String),
    UnknownKey(Option<String>),
    Signature,
}

impl JwsRefusal {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "malformed",
            Self::Algorithm(_) => "algorithm",
            Self::UnknownKey(_) => "unknown-key",
            Self::Signature => "signature",
        }
    }
}

impl fmt::Display for JwsRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed token: {why}"),
            Self::Algorithm(alg) => write!(f, "algorithm {alg} is not accepted"),
            Self::UnknownKey(Some(kid)) => write!(f, "no key with id {kid}"),
            Self::UnknownKey(None) => f.write_str("no key matches the token"),
            Self::Signature => f.write_str("signature verification failed"),
        }
    }
}

fn malformed(why: impl Into<String>) -> JwsRefusal {
    JwsRefusal::Malformed(why.into())
}

/// `(label, DER)` of the first PEM block.
pub fn pem_der(pem: &str) -> Option<(&str, Vec<u8>)> {
    let start = pem.find("-----BEGIN ")?;
    let label_end = pem[start + 11..].find("-----")? + start + 11;
    let label = &pem[start + 11..label_end];
    let body_end = pem.find("-----END ")?;
    if body_end < label_end + 5 {
        return None;
    }
    let body: String = pem[label_end + 5..body_end].split_whitespace().collect();
    Some((label, base64_decode_lenient(&body)))
}

pub fn b64url_decode(text: &str) -> Result<Vec<u8>, JwsRefusal> {
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|e| malformed(format!("not base64url: {e}")))
}

pub enum VerifyKey {
    Rsa(rsa::RsaPublicKey),
    Ec(p256::ecdsa::VerifyingKey),
}

impl VerifyKey {
    /// The one algorithm this key verifies.
    pub fn alg(&self) -> JwtAlg {
        match self {
            Self::Rsa(_) => JwtAlg::Rs256,
            Self::Ec(_) => JwtAlg::Es256,
        }
    }

    fn rsa(n: &[u8], e: &[u8]) -> Result<Self, JwsRefusal> {
        rsa::RsaPublicKey::new(rsa::BigUint::from_bytes_be(n), rsa::BigUint::from_bytes_be(e))
            .map(Self::Rsa)
            .map_err(|e| malformed(format!("RSA key: {e}")))
    }

    fn ec(x: &[u8], y: &[u8]) -> Result<Self, JwsRefusal> {
        let mut point = vec![0x04];
        point.extend_from_slice(x);
        point.extend_from_slice(y);
        p256::ecdsa::VerifyingKey::from_sec1_bytes(&point)
            .map(Self::Ec)
            .map_err(|e| malformed(format!("EC key: {e}")))
    }

    fn verifies(&self, signing_input: &[u8], signature: &[u8]) -> bool {
        match self {
            Self::Rsa(public) => public
                .verify(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(signing_input), signature)
                .is_ok(),
            Self::Ec(public) => {
                use p256::ecdsa::signature::Verifier;
                Signature::from_slice(signature)
                    .ok()
                    .is_some_and(|signature| public.verify(signing_input, &signature).is_ok())
            }
        }
    }
}

/// One public signing key as a language value: the `KeySet` entries `jwk/keyset-from-*` answer and `jws/verify`
/// takes. Key material is unpadded base64url; `alg` is the key's own algorithm, never the token's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kty", deny_unknown_fields)]
pub enum PublicJwk {
    #[serde(rename = "RSA")]
    Rsa { kid: Option<String>, alg: JwtAlg, n: String, e: String },
    #[serde(rename = "EC")]
    Ec { kid: Option<String>, alg: JwtAlg, crv: String, x: String, y: String },
}

/// The public keys an issuer publishes, by `kid`.
#[derive(Default)]
pub struct KeySet {
    keys: Vec<(Option<String>, VerifyKey)>,
}

fn jwk_field(jwk: &Value, name: &str) -> Result<Vec<u8>, JwsRefusal> {
    let text = jwk
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(format!("a JWK has no `{name}`")))?;
    b64url_decode(text.trim_end_matches('='))
}

impl KeySet {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn push(&mut self, kid: Option<String>, key: VerifyKey) {
        self.keys.push((kid, key));
    }

    /// A JWK Set document (`{"keys":[...]}`): RSA and P-256 signing keys; a key of another type or `use` is skipped.
    pub fn from_jwks(json_text: &str) -> Result<Self, JwsRefusal> {
        let doc: Value = serde_json::from_str(json_text).map_err(|e| malformed(format!("key set: {e}")))?;
        let list = doc
            .get("keys")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("a key set has no `keys`"))?;
        let mut set = KeySet::default();
        for jwk in list {
            if jwk.get("use").and_then(Value::as_str).is_some_and(|u| u != "sig") {
                continue;
            }
            let kid = jwk.get("kid").and_then(Value::as_str).map(str::to_string);
            match jwk.get("kty").and_then(Value::as_str) {
                Some("RSA") => set.push(kid, VerifyKey::rsa(&jwk_field(jwk, "n")?, &jwk_field(jwk, "e")?)?),
                Some("EC") if jwk.get("crv").and_then(Value::as_str) == Some("P-256") => {
                    set.push(kid, VerifyKey::ec(&jwk_field(jwk, "x")?, &jwk_field(jwk, "y")?)?)
                }
                _ => {}
            }
        }
        Ok(set)
    }

    /// The `{kid: certificate PEM}` document Firebase publishes for its ID-token signing keys.
    pub fn from_x509_map(json_text: &str) -> Result<Self, JwsRefusal> {
        let doc: Value = serde_json::from_str(json_text).map_err(|e| malformed(format!("certificates: {e}")))?;
        let map = doc.as_object().ok_or_else(|| malformed("certificates are not an object"))?;
        let mut set = KeySet::default();
        for (kid, pem) in map {
            let pem = pem.as_str().ok_or_else(|| malformed("a certificate is not text"))?;
            set.push(Some(kid.clone()), key_of_certificate(pem)?);
        }
        Ok(set)
    }

    /// The keys of the language value `keys` (what `public_jwks` answers).
    pub fn from_public_jwks(keys: &[PublicJwk]) -> Result<Self, JwsRefusal> {
        let mut set = KeySet::default();
        for jwk in keys {
            let (kid, alg, key) = match jwk {
                PublicJwk::Rsa { kid, alg, n, e } => (kid, alg, VerifyKey::rsa(&b64url_decode(n)?, &b64url_decode(e)?)?),
                PublicJwk::Ec { kid, alg, crv, x, y } => {
                    if crv != "P-256" {
                        return Err(malformed(format!("EC curve {crv} is not P-256")));
                    }
                    (kid, alg, VerifyKey::ec(&b64url_decode(x)?, &b64url_decode(y)?)?)
                }
            };
            if *alg != key.alg() {
                return Err(malformed(format!("a {} key declares {}", key.alg().name(), alg.name())));
            }
            set.push(kid.clone(), key);
        }
        Ok(set)
    }

    /// The keys as language values, in document order.
    pub fn public_jwks(&self) -> Vec<PublicJwk> {
        self.keys
            .iter()
            .map(|(kid, key)| match key {
                VerifyKey::Rsa(public) => PublicJwk::Rsa {
                    kid: kid.clone(),
                    alg: JwtAlg::Rs256,
                    n: URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
                    e: URL_SAFE_NO_PAD.encode(public.e().to_bytes_be()),
                },
                VerifyKey::Ec(public) => {
                    let point = public.to_encoded_point(false);
                    PublicJwk::Ec {
                        kid: kid.clone(),
                        alg: JwtAlg::Es256,
                        crv: "P-256".to_string(),
                        x: URL_SAFE_NO_PAD.encode(point.x().map(|x| x.to_vec()).unwrap_or_default()),
                        y: URL_SAFE_NO_PAD.encode(point.y().map(|y| y.to_vec()).unwrap_or_default()),
                    }
                }
            })
            .collect()
    }
}

/// `(tag, content, rest)` of the DER value at the start of `bytes`.
fn der_next(bytes: &[u8]) -> Result<(u8, &[u8], &[u8]), JwsRefusal> {
    let bad = || malformed("certificate is not DER");
    let (&tag, rest) = bytes.split_first().ok_or_else(bad)?;
    let (&first, rest) = rest.split_first().ok_or_else(bad)?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return Err(bad());
        }
        let length = rest[..count].iter().fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (length, &rest[count..])
    };
    if rest.len() < length {
        return Err(bad());
    }
    Ok((tag, &rest[..length], &rest[length..]))
}

/// The public key of an X.509 certificate PEM: its `SubjectPublicKeyInfo`, read as RSA or P-256.
fn key_of_certificate(pem: &str) -> Result<VerifyKey, JwsRefusal> {
    let (label, der) = pem_der(pem).ok_or_else(|| malformed("certificate PEM"))?;
    if label != "CERTIFICATE" {
        return Err(malformed(format!("expected a CERTIFICATE, found {label}")));
    }
    let (_, certificate, _) = der_next(&der)?;
    let (_, tbs, _) = der_next(certificate)?;
    let mut cursor = tbs;
    let (tag, _, after_version) = der_next(cursor)?;
    if tag == 0xA0 {
        cursor = after_version;
    }
    // serial, signature algorithm, issuer, validity, subject; the key info follows
    for _ in 0..5 {
        cursor = der_next(cursor)?.2;
    }
    let after_spki = der_next(cursor)?.2;
    let spki = &cursor[..cursor.len() - after_spki.len()];
    {
        use rsa::pkcs8::DecodePublicKey;
        if let Ok(key) = rsa::RsaPublicKey::from_public_key_der(spki) {
            return Ok(VerifyKey::Rsa(key));
        }
    }
    use p256::pkcs8::DecodePublicKey as _;
    p256::ecdsa::VerifyingKey::from_public_key_der(spki)
        .map(VerifyKey::Ec)
        .map_err(|_| malformed("the certificate key is neither RSA nor P-256"))
}

/// A JOSE header. `alg` is what the token claims, trusted only once a key of that algorithm verified it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JwsHeader {
    #[serde(default)]
    pub alg: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub typ: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Audience {
    One(String),
    Many(Vec<String>),
}

/// A token payload: the registered claims (RFC 7519 4.1) typed, every other claim in `extra`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JwsClaims {
    #[serde(default)]
    pub iss: Option<String>,
    #[serde(default)]
    pub sub: Option<String>,
    #[serde(default)]
    pub aud: Option<Audience>,
    #[serde(default)]
    pub exp: Option<f64>,
    #[serde(default)]
    pub nbf: Option<f64>,
    #[serde(default)]
    pub iat: Option<f64>,
    #[serde(default)]
    pub jti: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl JwsClaims {
    /// The language value: every registered claim present (`null` when absent), the others under `extra`.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("iss".into(), self.iss.clone().into());
        out.insert("sub".into(), self.sub.clone().into());
        out.insert(
            "aud".into(),
            serde_json::to_value(&self.aud).unwrap_or(Value::Null),
        );
        for (name, seconds) in [("exp", self.exp), ("nbf", self.nbf), ("iat", self.iat)] {
            out.insert(name.into(), seconds.and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number));
        }
        out.insert("jti".into(), self.jti.clone().into());
        out.insert("extra".into(), Value::Object(self.extra.clone()));
        Value::Object(out)
    }
}

/// The three parts of a compact token, its header read.
pub struct CompactParts<'t> {
    pub header_text: &'t str,
    pub claims_text: &'t str,
    pub signature_text: &'t str,
    pub header: JwsHeader,
}

impl CompactParts<'_> {
    /// The payload as JSON.
    pub fn claims_json(&self) -> Result<Value, JwsRefusal> {
        serde_json::from_slice(&b64url_decode(self.claims_text)?).map_err(|e| malformed(format!("claims: {e}")))
    }

    /// The payload typed.
    pub fn claims(&self) -> Result<JwsClaims, JwsRefusal> {
        serde_json::from_value(self.claims_json()?).map_err(|e| malformed(format!("claims: {e}")))
    }
}

/// Split a compact token into its three parts and read its header.
pub fn split_compact(token: &str) -> Result<CompactParts<'_>, JwsRefusal> {
    let mut parts = token.split('.');
    let (Some(header_text), Some(claims_text), Some(signature_text), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(malformed("a compact token has three parts"));
    };
    let header: Value =
        serde_json::from_slice(&b64url_decode(header_text)?).map_err(|e| malformed(format!("header: {e}")))?;
    if !header.is_object() {
        return Err(malformed("the header is not an object"));
    }
    let header: JwsHeader = serde_json::from_value(header).map_err(|e| malformed(format!("header: {e}")))?;
    Ok(CompactParts { header_text, claims_text, signature_text, header })
}

/// Check the signature of a compact token against `keys`: RS256 or ES256 only, the key chosen by `kid` (every key
/// when the header names none), and the header's algorithm must be the chosen key's own, so `none` and the HMAC
/// algorithms are refused and a public key is never an HMAC secret.
pub fn verify_signature<'t>(token: &'t str, keys: &KeySet) -> Result<CompactParts<'t>, JwsRefusal> {
    let parts = split_compact(token)?;
    let Some(alg) = JwtAlg::parse(&parts.header.alg) else {
        return Err(JwsRefusal::Algorithm(parts.header.alg.clone()));
    };
    let kid = &parts.header.kid;
    let chosen: Vec<&VerifyKey> = keys
        .keys
        .iter()
        .filter(|(have, _)| kid.is_none() || have == kid)
        .map(|(_, key)| key)
        .collect();
    if chosen.is_empty() {
        return Err(JwsRefusal::UnknownKey(kid.clone()));
    }
    let Some(key) = chosen.into_iter().find(|key| key.alg() == alg) else {
        return Err(JwsRefusal::Algorithm(parts.header.alg.clone()));
    };
    let signature = b64url_decode(parts.signature_text)?;
    let signing_input = &token[..parts.header_text.len() + 1 + parts.claims_text.len()];
    if !key.verifies(signing_input.as_bytes(), &signature) {
        return Err(JwsRefusal::Signature);
    }
    Ok(parts)
}

/// A token read: its header, typed payload and the signature part as sent.
pub struct DecodedJws {
    pub header: JwsHeader,
    pub claims: JwsClaims,
    pub signature: String,
}

impl DecodedJws {
    fn of(parts: &CompactParts<'_>) -> Result<Self, JwsRefusal> {
        Ok(Self {
            header: parts.header.clone(),
            claims: parts.claims()?,
            signature: parts.signature_text.to_string(),
        })
    }
}

/// Read a compact token WITHOUT checking its signature (the Firebase Auth emulator issues unsigned tokens).
pub fn decode(token: &str) -> Result<DecodedJws, JwsRefusal> {
    DecodedJws::of(&split_compact(token)?)
}

/// [`verify_signature`], then the typed payload.
pub fn verify(token: &str, keys: &KeySet) -> Result<DecodedJws, JwsRefusal> {
    DecodedJws::of(&verify_signature(token, keys)?)
}

/// Whether a token is unsigned (`alg` is `none` and the signature part is empty): what the Firebase Auth emulator issues.
pub fn is_unsigned(token: &str) -> bool {
    split_compact(token).is_ok_and(|parts| parts.signature_text.is_empty() && parts.header.alg == "none")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKENS: &str = include_str!("../../runtime/tests/fixtures/auth/tokens.json");

    fn fixture() -> Value {
        serde_json::from_str(TOKENS).unwrap()
    }

    fn case(provider: &str, name: &str) -> String {
        fixture()[provider]["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no case {name}"))["token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn jwks() -> KeySet {
        KeySet::from_jwks(&fixture()["oidc"]["jwks"].to_string()).unwrap()
    }

    fn certs() -> KeySet {
        KeySet::from_x509_map(&fixture()["firebase"]["certs"].to_string()).unwrap()
    }

    fn b64(json: &str) -> String {
        URL_SAFE_NO_PAD.encode(json)
    }

    #[test]
    fn valid_rs256_and_es256_verify_to_typed_claims() {
        let ada = verify(&case("oidc", "valid RS256 with email, name and a role claim"), &jwks()).unwrap();
        assert_eq!(ada.header.alg, "RS256");
        assert_eq!(ada.header.kid.as_deref(), Some("k-rsa"));
        assert_eq!(ada.claims.sub.as_deref(), Some("auth0|ada"));
        assert_eq!(ada.claims.aud, Some(Audience::One("app-client".into())));
        assert_eq!(ada.claims.exp, Some(4102444800.0));
        assert_eq!(ada.claims.extra["role"], "teacher");
        assert_eq!(ada.claims.extra["email"], "ada@example.com");
        assert!(!ada.claims.extra.contains_key("sub"), "a registered claim is not repeated in extra");
        let bob = verify(&case("oidc", "valid ES256"), &jwks()).unwrap();
        assert_eq!(bob.header.alg, "ES256");
        assert_eq!(bob.claims.sub.as_deref(), Some("auth0|bob"));
    }

    #[test]
    fn claims_are_not_checked_only_the_signature() {
        for name in ["expired", "wrong audience", "wrong issuer", "no subject"] {
            assert!(verify(&case("oidc", name), &jwks()).is_ok(), "{name}: the claim policy is the caller's");
        }
    }

    #[test]
    fn refusals_carry_their_reason() {
        let reason = |name: &str| verify(&case("oidc", name), &jwks()).err().map(|e| e.reason());
        assert_eq!(reason("bad signature"), Some("signature"));
        assert_eq!(reason("signed by a key the issuer does not publish"), Some("signature"));
        assert_eq!(reason("unknown kid"), Some("unknown-key"));
        assert_eq!(reason("alg none"), Some("algorithm"));
        assert_eq!(reason("HS256 keyed with the public key text"), Some("algorithm"));
        assert_eq!(verify("a.b", &jwks()).err().map(|e| e.reason()), Some("malformed"));
        assert_eq!(verify("a.b.c.d", &jwks()).err().map(|e| e.reason()), Some("malformed"));
    }

    #[test]
    fn a_header_alg_that_does_not_fit_the_kid_key_is_refused() {
        let token = case("oidc", "valid ES256");
        let (_, rest) = token.split_once('.').unwrap();
        let forged = format!("{}.{rest}", b64(r#"{"alg":"RS256","kid":"k-ec"}"#));
        assert_eq!(verify(&forged, &jwks()).err().map(|e| e.reason()), Some("algorithm"));
    }

    #[test]
    fn x509_certificates_verify_firebase_tokens() {
        let token = case("firebase", "project token");
        let ok = verify(&token, &certs()).unwrap();
        assert_eq!(ok.claims.sub.as_deref(), Some("uid-1"));
        assert_eq!(
            verify(&case("firebase", "unknown kid"), &certs()).err().map(|e| e.reason()),
            Some("unknown-key")
        );
        assert_eq!(
            verify(&case("firebase", "bad signature"), &certs()).err().map(|e| e.reason()),
            Some("signature")
        );
    }

    #[test]
    fn public_jwks_round_trip_and_still_verify() {
        for set in [jwks(), certs()] {
            let values = set.public_jwks();
            let json = serde_json::to_value(&values).unwrap();
            let back: Vec<PublicJwk> = serde_json::from_value(json).unwrap();
            assert_eq!(back, values);
            let rebuilt = KeySet::from_public_jwks(&back).unwrap();
            assert_eq!(rebuilt.public_jwks(), values);
        }
        let rebuilt = KeySet::from_public_jwks(&jwks().public_jwks()).unwrap();
        assert!(verify(&case("oidc", "valid RS256 with email, name and a role claim"), &rebuilt).is_ok());
        assert!(verify(&case("oidc", "valid ES256"), &rebuilt).is_ok());
        let fb = KeySet::from_public_jwks(&certs().public_jwks()).unwrap();
        assert!(verify(&case("firebase", "project token"), &fb).is_ok());
    }

    #[test]
    fn a_public_jwk_whose_alg_does_not_fit_its_key_is_refused() {
        let PublicJwk::Rsa { kid, n, e, .. } = jwks().public_jwks().remove(0) else { panic!("rsa first") };
        let lying = PublicJwk::Rsa { kid, alg: JwtAlg::Es256, n, e };
        assert!(matches!(KeySet::from_public_jwks(&[lying]), Err(JwsRefusal::Malformed(_))));
    }

    #[test]
    fn key_documents_skip_foreign_keys_and_refuse_broken_ones() {
        let set = KeySet::from_jwks(
            r#"{"keys":[{"kty":"oct","k":"AA"},{"kty":"EC","crv":"P-384","x":"AA","y":"AA"},{"kty":"RSA","use":"enc","n":"AQAB","e":"AQAB"}]}"#,
        )
        .unwrap();
        assert!(set.is_empty());
        assert!(KeySet::from_jwks(r#"{"keys":[{"kty":"RSA","e":"AQAB"}]}"#).is_err());
        assert!(KeySet::from_jwks(r#"{"nokeys":[]}"#).is_err());
        assert!(KeySet::from_x509_map(r#"{"k":"-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----"}"#).is_err());
        assert!(KeySet::from_x509_map("[]").is_err());
    }

    #[test]
    fn decode_reads_an_unsigned_token_and_types_its_claims() {
        let token = format!("{}.{}.", b64(r#"{"alg":"none","typ":"JWT"}"#), b64(r#"{"sub":"u","aud":["a","b"],"exp":5,"firebase":{"tenant":"t1"}}"#));
        assert!(is_unsigned(&token));
        let read = decode(&token).unwrap();
        assert_eq!(read.header.alg, "none");
        assert_eq!(read.header.typ.as_deref(), Some("JWT"));
        assert_eq!(read.signature, "");
        assert_eq!(read.claims.aud, Some(Audience::Many(vec!["a".into(), "b".into()])));
        let json = read.claims.to_json();
        assert_eq!(json["exp"], 5.0);
        assert_eq!(json["iss"], Value::Null);
        assert_eq!(json["extra"]["firebase"]["tenant"], "t1");
        assert!(!is_unsigned(&case("oidc", "valid ES256")), "control: a signed token");
        assert_eq!(decode("x.y").err().map(|e| e.reason()), Some("malformed"));
        let wrong = format!("{}.{}.", b64(r#"{"alg":"none"}"#), b64(r#"{"exp":"soon"}"#));
        assert_eq!(decode(&wrong).err().map(|e| e.reason()), Some("malformed"), "a registered claim of the wrong type");
    }
}
