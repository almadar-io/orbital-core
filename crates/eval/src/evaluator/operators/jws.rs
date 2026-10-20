//! JWS and JWK primitives over `orbital_core_base::jws`: signature checks and key documents only. Claim policy
//! (expiry, issuer, audience, tenant) is the calling `.lolo` atom's.

use super::macros::*;
use super::*;
use orbital_core_base::json_value::value_to_json;
use orbital_core_base::jws::{self, DecodedJws, JwsRefusal, KeySet, PublicJwk};

fn text_arg(arg: &SExpression, ctx: &BindingContext, op: &str) -> Result<String, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::String(s) => Ok(s),
        other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{op}: expected a string argument, got {}",
            other.type_name()
        )))),
    }
}

fn to_value(json: serde_json::Value, op: &str) -> EvalResult {
    serde_json::from_value(json)
        .map_err(|e| OrbitalError::Eval(EvalError::InvalidArgument(format!("{op}: {e}"))))
}

/// `{ok: true, header, claims, signature}` or `{ok: false, reason, message}`.
fn verdict(read: Result<DecodedJws, JwsRefusal>, op: &str) -> EvalResult {
    let json = match read {
        Ok(read) => serde_json::json!({
            "ok": true,
            "header": read.header,
            "claims": read.claims.to_json(),
            "signature": read.signature,
        }),
        Err(refusal) => serde_json::json!({
            "ok": false,
            "reason": refusal.reason(),
            "message": refusal.to_string(),
        }),
    };
    to_value(json, op)
}

fn keyset_value(keys: Result<KeySet, JwsRefusal>, op: &str) -> EvalResult {
    match keys {
        Ok(keys) => to_value(serde_json::to_value(keys.public_jwks()).unwrap_or_default(), op),
        Err(_) => Ok(Value::Null),
    }
}

/// jws/verify (token, keyset) - the signature checked against the key `kid` selects; the token read, or a refusal
pub struct JwsVerifyOp;
impl Operator for JwsVerifyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "jws/verify", == 2);
        let token = text_arg(&args[0], ctx, "jws/verify")?;
        let keys = eval_expr(&args[1], ctx)?;
        if !matches!(keys, Value::Array(_)) {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "jws/verify: expected a KeySet, got {}",
                keys.type_name()
            ))));
        }
        let keys: Vec<PublicJwk> = serde_json::from_value(value_to_json(&keys)).map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!("jws/verify: not a KeySet: {e}")))
        })?;
        let read = KeySet::from_public_jwks(&keys).and_then(|keys| jws::verify(&token, &keys));
        verdict(read, "jws/verify")
    }
}

/// jws/decode (token) - the token read WITHOUT a signature check (an emulator's unsigned token)
pub struct JwsDecodeOp;
impl Operator for JwsDecodeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "jws/decode", == 1);
        let token = text_arg(&args[0], ctx, "jws/decode")?;
        verdict(jws::decode(&token), "jws/decode")
    }
}

/// jwk/keyset-from-jwks (text) - the RSA and P-256 signing keys of a JWK Set document; null when it is malformed
pub struct JwkKeysetFromJwksOp;
impl Operator for JwkKeysetFromJwksOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "jwk/keyset-from-jwks", == 1);
        let text = text_arg(&args[0], ctx, "jwk/keyset-from-jwks")?;
        keyset_value(KeySet::from_jwks(&text), "jwk/keyset-from-jwks")
    }
}

/// jwk/keyset-from-x509 (text) - the keys of a `{kid: certificate PEM}` document; null when it is malformed
pub struct JwkKeysetFromX509Op;
impl Operator for JwkKeysetFromX509Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "jwk/keyset-from-x509", == 1);
        let text = text_arg(&args[0], ctx, "jwk/keyset-from-x509")?;
        keyset_value(KeySet::from_x509_map(&text), "jwk/keyset-from-x509")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOKENS: &str = include_str!("../../../../runtime/tests/fixtures/auth/tokens.json");

    fn fixture() -> serde_json::Value {
        serde_json::from_str(TOKENS).unwrap()
    }

    fn token(provider: &str, name: &str) -> String {
        fixture()[provider]["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn run(expr: serde_json::Value) -> Result<Value, String> {
        let expr: SExpression = serde_json::from_value(expr).unwrap();
        eval_expr(&expr, &BindingContext::default()).map_err(|e| e.to_string())
    }

    fn jwks_text() -> String {
        fixture()["oidc"]["jwks"].to_string()
    }

    fn certs_text() -> String {
        fixture()["firebase"]["certs"].to_string()
    }

    fn field<'v>(value: &'v Value, path: &[&str]) -> &'v Value {
        path.iter().fold(value, |v, key| match v {
            Value::Object(map) => map.get(*key).unwrap_or(&Value::Null),
            _ => &Value::Null,
        })
    }

    fn verify(token: &str, keys: &str, from: &str) -> Value {
        run(json!(["jws/verify", token, [from, keys]])).unwrap()
    }

    #[test]
    fn verify_answers_the_typed_header_and_claims() {
        let ok = verify(&token("oidc", "valid RS256 with email, name and a role claim"), &jwks_text(), "jwk/keyset-from-jwks");
        assert_eq!(field(&ok, &["ok"]), &Value::Bool(true));
        assert_eq!(field(&ok, &["header", "alg"]), &Value::String("RS256".into()));
        assert_eq!(field(&ok, &["header", "kid"]), &Value::String("k-rsa".into()));
        assert_eq!(field(&ok, &["header", "typ"]), &Value::Null);
        assert_eq!(field(&ok, &["claims", "sub"]), &Value::String("auth0|ada".into()));
        assert_eq!(field(&ok, &["claims", "exp"]), &Value::Number(4102444800.0));
        assert_eq!(field(&ok, &["claims", "nbf"]), &Value::Null);
        assert_eq!(field(&ok, &["claims", "extra", "role"]), &Value::String("teacher".into()));
        let es = verify(&token("oidc", "valid ES256"), &jwks_text(), "jwk/keyset-from-jwks");
        assert_eq!(field(&es, &["claims", "sub"]), &Value::String("auth0|bob".into()));
        let fb = verify(&token("firebase", "project token"), &certs_text(), "jwk/keyset-from-x509");
        assert_eq!(field(&fb, &["claims", "sub"]), &Value::String("uid-1".into()));
    }

    #[test]
    fn verify_refuses_with_a_reason_and_leaves_claims_to_policy() {
        let reason = |name: &str| field(&verify(&token("oidc", name), &jwks_text(), "jwk/keyset-from-jwks"), &["reason"]).clone();
        assert_eq!(reason("bad signature"), Value::String("signature".into()));
        assert_eq!(reason("unknown kid"), Value::String("unknown-key".into()));
        assert_eq!(reason("alg none"), Value::String("algorithm".into()));
        assert_eq!(reason("HS256 keyed with the public key text"), Value::String("algorithm".into()));
        assert_eq!(reason("expired"), Value::Null, "control: an expired token's signature is good");
        let refused = verify("not-a-token", &jwks_text(), "jwk/keyset-from-jwks");
        assert_eq!(field(&refused, &["ok"]), &Value::Bool(false));
        assert_eq!(field(&refused, &["reason"]), &Value::String("malformed".into()));
    }

    #[test]
    fn decode_reads_without_a_key() {
        let unsigned = format!(
            "{}.{}.",
            orbital_core_base::codec::base64url_encode(br#"{"alg":"none"}"#),
            orbital_core_base::codec::base64url_encode(br#"{"sub":"u","auth_time":1}"#)
        );
        let read = run(json!(["jws/decode", unsigned])).unwrap();
        assert_eq!(field(&read, &["header", "alg"]), &Value::String("none".into()));
        assert_eq!(field(&read, &["signature"]), &Value::String(String::new()));
        assert_eq!(field(&read, &["claims", "extra", "auth_time"]), &Value::Number(1.0));
        let signed = run(json!(["jws/decode", token("oidc", "bad signature")])).unwrap();
        assert_eq!(field(&signed, &["ok"]), &Value::Bool(true), "control: decode never checks a signature");
    }

    #[test]
    fn keysets_are_typed_values_and_a_broken_document_is_null() {
        let keys = run(json!(["jwk/keyset-from-jwks", jwks_text()])).unwrap();
        let Value::Array(keys) = keys else { panic!("a KeySet is a list") };
        assert_eq!(keys.len(), 2);
        assert_eq!(field(&keys[0], &["kty"]), &Value::String("RSA".into()));
        assert_eq!(field(&keys[0], &["alg"]), &Value::String("RS256".into()));
        assert_eq!(field(&keys[1], &["crv"]), &Value::String("P-256".into()));
        assert_eq!(run(json!(["jwk/keyset-from-jwks", "{\"keys\":[{\"kty\":\"RSA\"}]}"])).unwrap(), Value::Null);
        assert_eq!(run(json!(["jwk/keyset-from-x509", "[]"])).unwrap(), Value::Null);
        assert!(run(json!(["jws/verify", "a.b.c", [{"kty": "RSA"}]])).is_err(), "a value that is not a KeySet");
        assert!(run(json!(["jws/decode", null])).is_err(), "null is refused, not read");
    }
}
