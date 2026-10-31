//! `jws/verify` - the one place a compact JWS is checked (EXEC-IDENTITY-2). Pure: the answer is a function of the token,
//! the keys, the options and the dispatch clock, so a replay recomputes it and gets the same value.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::codec;
use orbital_core_base::sealed::VERIFIED;

/// The issuer the Firebase auth emulator (std-dev-personas) writes into its unsigned tokens.
pub const EMULATOR_ISSUER: &str = "firebase-auth-emulator@example.com";

const OPTIONS: [&str; 4] = ["issuer", "audience", "clockSkewMs", "allowUnsigned"];

type Json = serde_json::Map<String, serde_json::Value>;

fn refusal(kind: &str, error: &str) -> EvalResult {
    Ok(Value::Object(
        [("error", Value::string(error)), ("kind", Value::string(kind))]
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    ))
}

fn invalid(message: String) -> OrbitalError {
    OrbitalError::Eval(EvalError::InvalidArgument(format!("jws/verify: {message}")))
}

struct Options {
    issuer: String,
    audience: String,
    skew_ms: f64,
    allow_unsigned: bool,
}

fn options_of(value: &Value) -> Result<Options, OrbitalError> {
    let Value::Object(fields) = value else {
        return Err(OrbitalError::Eval(value.kind_error("object")));
    };
    if let Some(key) = fields.keys().find(|key| !OPTIONS.contains(&key.as_str())) {
        return Err(invalid(format!("unknown option {key}")));
    }
    let text = |key: &str| -> Result<String, OrbitalError> {
        match fields.get(key) {
            Some(Value::String(text)) if !text.is_empty() => Ok(text.to_string()),
            _ => Err(invalid(format!("option {key} must be a non-empty string"))),
        }
    };
    let skew_ms = match fields.get("clockSkewMs") {
        None => 0.0,
        Some(Value::Number(n)) if n.is_finite() && *n >= 0.0 => *n,
        Some(_) => return Err(invalid("option clockSkewMs must be a non-negative number".to_string())),
    };
    let allow_unsigned = match fields.get("allowUnsigned") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(invalid("option allowUnsigned must be a boolean".to_string())),
    };
    Ok(Options { issuer: text("issuer")?, audience: text("audience")?, skew_ms, allow_unsigned })
}

fn segment_json(segment: &str) -> Option<Json> {
    let bytes = codec::base64url_decode(segment).ok()?;
    match serde_json::from_slice::<serde_json::Value>(&bytes).ok()? {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    }
}

fn member<'a>(key: &'a Value, name: &str) -> Option<&'a str> {
    key.get(name).and_then(Value::as_str)
}

fn member_bytes(key: &Value, name: &str) -> Option<Vec<u8>> {
    codec::base64url_decode(member(key, name)?).ok()
}

fn key_verifies(key: &Value, alg: &str, message: &[u8], signature: &[u8]) -> bool {
    match alg {
        "RS256" => match (member_bytes(key, "n"), member_bytes(key, "e")) {
            (Some(n), Some(e)) => codec::rsa_pkcs1_sha256_verify(&n, &e, message, signature) == codec::RsaVerdict::Valid,
            _ => false,
        },
        _ => {
            if member(key, "crv") != Some("P-256") {
                return false;
            }
            match (member_bytes(key, "x"), member_bytes(key, "y")) {
                (Some(x), Some(y)) => {
                    codec::ecdsa_p256_sha256_verify(&x, &y, message, signature) == codec::EcdsaVerdict::Valid
                }
                _ => false,
            }
        }
    }
}

/// `Err(refusal)` is the typed failure answer; `Ok` carries nothing the caller needs.
fn check_signature(
    keys: &[Value],
    alg: &str,
    kid: Option<&str>,
    message: &str,
    signature: &str,
) -> Result<(), (&'static str, &'static str)> {
    let kty = if alg == "RS256" { "RSA" } else { "EC" };
    let candidates: Vec<&Value> = keys
        .iter()
        .filter(|key| member(key, "kty") == Some(kty))
        .filter(|key| member(key, "alg").is_none_or(|declared| declared == alg))
        .filter(|key| kid.is_none_or(|wanted| member(key, "kid") == Some(wanted)))
        .collect();
    if candidates.is_empty() {
        return Err(("key", "no key of the key set fits the token header"));
    }
    let Ok(signature) = codec::base64url_decode(signature) else {
        return Err(("malformed", "the signature segment is not unpadded base64url"));
    };
    if candidates.iter().any(|key| key_verifies(key, alg, message.as_bytes(), &signature)) {
        Ok(())
    } else {
        Err(("signature", "the signature does not verify under the key"))
    }
}

fn seconds(claims: &Json, name: &str) -> Result<Option<f64>, ()> {
    match claims.get(name) {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => n.as_f64().filter(|n| n.is_finite()).map(Some).ok_or(()),
        Some(_) => Err(()),
    }
}

fn verify(token: &str, keys: &[Value], options: &Options, now_ms: f64) -> EvalResult {
    let parts: Vec<&str> = token.split('.').collect();
    let [header_segment, payload_segment, signature_segment] = parts.as_slice() else {
        return refusal("malformed", "a compact JWS has three dot-separated segments");
    };
    let (Some(header), Some(claims)) = (segment_json(header_segment), segment_json(payload_segment)) else {
        return refusal("malformed", "the header and payload are unpadded base64url JSON objects");
    };
    let Some(alg) = header.get("alg").and_then(serde_json::Value::as_str) else {
        return refusal("malformed", "the header has no alg");
    };
    let kid = match header.get("kid") {
        None => None,
        Some(serde_json::Value::String(kid)) => Some(kid.as_str()),
        Some(_) => return refusal("malformed", "the header kid is not a string"),
    };
    match alg {
        "RS256" | "ES256" => {
            let message = format!("{header_segment}.{payload_segment}");
            if let Err((kind, error)) = check_signature(keys, alg, kid, &message, signature_segment) {
                return refusal(kind, error);
            }
        }
        "none" => {
            let emulator = options.allow_unsigned
                && options.issuer == EMULATOR_ISSUER
                && claims.get("iss").and_then(serde_json::Value::as_str) == Some(EMULATOR_ISSUER);
            if !emulator {
                return refusal("algorithm", "an unsigned token verifies only when the call allows it for the emulator issuer");
            }
            if !signature_segment.is_empty() {
                return refusal("malformed", "an unsigned token has an empty signature segment");
            }
        }
        _ => return refusal("algorithm", "the header alg is not RS256 or ES256"),
    }
    let (Ok(exp), Ok(nbf)) = (seconds(&claims, "exp"), seconds(&claims, "nbf")) else {
        return refusal("malformed", "exp and nbf are numbers");
    };
    let Some(exp) = exp else {
        return refusal("malformed", "the token has no exp");
    };
    let expires_at = exp * 1000.0;
    if now_ms >= expires_at + options.skew_ms {
        return refusal("expired", "the token has expired");
    }
    if nbf.is_some_and(|nbf| now_ms < nbf * 1000.0 - options.skew_ms) {
        return refusal("not-yet-valid", "the token is not valid yet");
    }
    if claims.get("iss").and_then(serde_json::Value::as_str) != Some(options.issuer.as_str()) {
        return refusal("issuer", "the token issuer is not the expected one");
    }
    let audience_ok = match claims.get("aud") {
        Some(serde_json::Value::String(aud)) => *aud == options.audience,
        Some(serde_json::Value::Array(auds)) => auds.iter().any(|aud| aud.as_str() == Some(options.audience.as_str())),
        _ => false,
    };
    if !audience_ok {
        return refusal("audience", "the token audience does not name the expected one");
    }
    let Some(subject) = claims.get("sub").and_then(serde_json::Value::as_str).filter(|sub| !sub.is_empty()) else {
        return refusal("malformed", "the token has no sub");
    };
    let mut list = Vec::with_capacity(claims.len());
    for (name, value) in &claims {
        let value = serde_json::from_value::<Value>(value.clone()).map_err(|error| invalid(error.to_string()))?;
        list.push(Value::Object(
            [("name", Value::string(name.as_str())), ("value", value)]
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        ));
    }
    let fields: ValueMap = [
        ("subject", Value::string(subject)),
        ("issuer", Value::string(options.issuer.as_str())),
        ("audience", Value::string(options.audience.as_str())),
        ("claims", Value::array(list)),
        ("expiresAt", Value::Number(expires_at)),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value))
    .collect();
    Ok(Value::sealed(VERIFIED, fields))
}

/// jws/verify - `(jws/verify token keys { issuer, audience, clockSkewMs?, allowUnsigned? })`: a sealed `Verified` when the
/// token is a compact JWS (RS256 or ES256) whose signature verifies under a key of `keys` (JWK records, base64url members;
/// the `kid` header picks the key, none tries each key of the right type) and whose `exp`, `nbf`, `iss` and `aud` hold at
/// the dispatch clock; otherwise `{ error, kind }`, kind one of malformed, algorithm, key, signature, expired,
/// not-yet-valid, issuer, audience.
pub struct JwsVerifyOp;
impl Operator for JwsVerifyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "jws/verify", == 3);
        let token = eval_expr(&args[0], ctx)?;
        let keys = eval_expr(&args[1], ctx)?;
        let options = options_of(&eval_expr(&args[2], ctx)?)?;
        let Value::Array(keys) = &keys else {
            return Err(OrbitalError::Eval(keys.kind_error("array")));
        };
        if let Some(bad) = keys.iter().find(|key| !matches!(key, Value::Object(_))) {
            return Err(OrbitalError::Eval(bad.kind_error("object")));
        }
        verify(token.text()?, keys, &options, ctx.now_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../../../tests/fixtures/security/jws-verify.json");
    const INSIDE_WINDOW_MS: f64 = 1_800_000_000_000.0;

    fn fixture() -> serde_json::Value {
        serde_json::from_str(FIXTURE).expect("fixture")
    }

    fn call_at(now: f64, token: &str, keys: serde_json::Value, options: serde_json::Value) -> EvalResult {
        let form = serde_json::json!(["jws/verify", token, keys, options]);
        let expr: SExpression = serde_json::from_value(form).expect("s-expression");
        let mut ctx = BindingContext::default();
        ctx.now = Some(now);
        eval_expr(&expr, &ctx)
    }

    fn opts() -> serde_json::Value {
        serde_json::json!({ "issuer": "https://issuer.test", "audience": "app-1" })
    }

    fn call(token: &str, options: serde_json::Value) -> Value {
        call_at(INSIDE_WINDOW_MS, token, fixture()["keys"].clone(), options).expect("evaluates")
    }

    fn token(name: &str) -> String {
        fixture()["tokens"][name].as_str().expect("token").to_string()
    }

    fn kind(answer: &Value) -> String {
        assert!(!matches!(answer, Value::Sealed(_)), "refused, not verified: {answer:?}");
        answer.get("kind").and_then(Value::as_str).unwrap_or_else(|| panic!("a refusal: {answer:?}")).to_string()
    }

    #[test]
    fn rs256_and_es256_tokens_verify_to_a_sealed_record_with_every_claim() {
        for name in ["rs256_valid", "es256_valid", "rs256_no_kid"] {
            let answer = call(&token(name), opts());
            assert!(answer.is_sealed_as("Verified"), "{name}: {answer:?}");
            assert_eq!(answer.get_path("subject").and_then(Value::as_str), Some("user-1"));
            assert_eq!(answer.get_path("issuer").and_then(Value::as_str), Some("https://issuer.test"));
            assert_eq!(answer.get_path("audience").and_then(Value::as_str), Some("app-1"));
            assert_eq!(answer.get_path("expiresAt"), Some(&Value::Number(2_000_000_000_000.0)));
            let claims = answer.get_path("claims").and_then(Value::as_array).expect("claims");
            let roles = claims.iter().find(|c| c.get("name").and_then(Value::as_str) == Some("roles")).expect("roles claim");
            assert_eq!(roles.get("value"), Some(&Value::array(vec![Value::string("a"), Value::string("b")])));
        }
    }

    #[test]
    fn a_list_audience_verifies_when_it_names_the_expected_one() {
        assert!(call(&token("rs256_audience_list"), opts()).is_sealed_as("Verified"));
    }

    #[test]
    fn each_failed_check_answers_its_own_kind() {
        assert_eq!(kind(&call("only.two", opts())), "malformed");
        assert_eq!(kind(&call("a.b.c", opts())), "malformed");
        assert_eq!(kind(&call(&token("rs256_unknown_kid"), opts())), "key");
        assert_eq!(kind(&call(&token("rs256_signed_by_other_key"), opts())), "signature");
        assert_eq!(kind(&call(&token("rs256_expired"), opts())), "expired");
        assert_eq!(kind(&call(&token("rs256_not_yet_valid"), opts())), "not-yet-valid");
        assert_eq!(kind(&call(&token("rs256_wrong_issuer"), opts())), "issuer");
        assert_eq!(kind(&call(&token("rs256_wrong_audience"), opts())), "audience");
        assert_eq!(kind(&call(&token("rs256_no_subject"), opts())), "malformed");
        assert_eq!(kind(&call(&token("rs256_no_exp"), opts())), "malformed");
    }

    #[test]
    fn a_tampered_payload_fails_the_signature_and_a_hmac_alg_is_refused() {
        let good = token("rs256_valid");
        let parts: Vec<&str> = good.split('.').collect();
        let forged = codec::base64url_encode(br#"{"iss":"https://issuer.test","aud":"app-1","sub":"admin","exp":2000000000}"#);
        assert_eq!(kind(&call(&format!("{}.{forged}.{}", parts[0], parts[2]), opts())), "signature");
        let header = codec::base64url_encode(br#"{"alg":"HS256","kid":"rs-1"}"#);
        assert_eq!(kind(&call(&format!("{header}.{}.{}", parts[1], parts[2]), opts())), "algorithm");
    }

    #[test]
    fn the_dispatch_clock_and_the_skew_decide_exp_and_nbf() {
        let expired = token("rs256_expired");
        let keys = fixture()["keys"].clone();
        let at = |now: f64, skew: Option<f64>| {
            let mut options = opts();
            if let Some(skew) = skew {
                options["clockSkewMs"] = serde_json::json!(skew);
            }
            call_at(now, &expired, keys.clone(), options).expect("evaluates")
        };
        let exp_ms = 1_700_000_100_000.0;
        assert!(at(exp_ms - 1.0, None).is_sealed_as("Verified"));
        assert_eq!(kind(&at(exp_ms, None)), "expired");
        assert!(at(exp_ms + 999.0, Some(1000.0)).is_sealed_as("Verified"), "skew forgives a late clock");
        assert_eq!(kind(&at(exp_ms + 1000.0, Some(1000.0))), "expired");
        let early = token("rs256_not_yet_valid");
        let nbf_ms = 1_900_000_000_000.0;
        let early_at = |now: f64, skew: f64| {
            let mut options = opts();
            options["clockSkewMs"] = serde_json::json!(skew);
            call_at(now, &early, keys.clone(), options).expect("evaluates")
        };
        assert_eq!(kind(&early_at(nbf_ms - 1001.0, 1000.0)), "not-yet-valid");
        assert!(early_at(nbf_ms - 1000.0, 1000.0).is_sealed_as("Verified"));
    }

    fn unsigned(issuer: &str) -> String {
        let header = codec::base64url_encode(br#"{"alg":"none","typ":"JWT"}"#);
        let claims = format!(r#"{{"iss":"{issuer}","aud":"app-1","sub":"dev-1","exp":2000000000}}"#);
        format!("{header}.{}.", codec::base64url_encode(claims.as_bytes()))
    }

    #[test]
    fn an_unsigned_token_verifies_only_for_the_emulator_issuer_when_the_call_allows_it() {
        let emulator = unsigned(EMULATOR_ISSUER);
        let allow = serde_json::json!({ "issuer": EMULATOR_ISSUER, "audience": "app-1", "allowUnsigned": true });
        assert!(call(&emulator, allow.clone()).is_sealed_as("Verified"));
        let not_allowed = serde_json::json!({ "issuer": EMULATOR_ISSUER, "audience": "app-1" });
        assert_eq!(kind(&call(&emulator, not_allowed)), "algorithm", "control: allowUnsigned is off");
        assert_eq!(kind(&call(&emulator, opts())), "algorithm", "control: a deployment naming its own issuer");
        let other = unsigned("https://issuer.test");
        let other_allow = serde_json::json!({ "issuer": "https://issuer.test", "audience": "app-1", "allowUnsigned": true });
        assert_eq!(kind(&call(&other, other_allow)), "algorithm", "allowUnsigned never lifts a non-emulator issuer");
        let mismatch = serde_json::json!({ "issuer": EMULATOR_ISSUER, "audience": "app-1", "allowUnsigned": true });
        assert_eq!(kind(&call(&other, mismatch)), "algorithm", "the token itself must carry the emulator issuer");
        let with_signature = format!("{}x", emulator);
        assert_eq!(kind(&call(&with_signature, allow)), "malformed");
    }

    #[test]
    fn a_misused_call_is_an_evaluation_error_not_a_refusal() {
        let keys = fixture()["keys"].clone();
        let t = token("rs256_valid");
        assert!(call_at(INSIDE_WINDOW_MS, &t, keys.clone(), serde_json::json!({ "issuer": "i" })).is_err());
        assert!(call_at(INSIDE_WINDOW_MS, &t, keys.clone(), serde_json::json!({ "issuer": "i", "audience": "a", "x": 1 })).is_err());
        assert!(call_at(INSIDE_WINDOW_MS, &t, serde_json::json!("keys"), opts()).is_err());
        assert!(call_at(INSIDE_WINDOW_MS, &t, serde_json::json!([1]), opts()).is_err());
    }

    #[test]
    fn the_answer_is_a_pure_function_of_its_inputs() {
        let t = token("rs256_valid");
        assert_eq!(call(&t, opts()), call(&t, opts()));
    }

    #[test]
    fn a_sealed_value_is_not_built_by_a_literal_a_cast_or_a_wire_round_trip() {
        let answer = call(&token("rs256_valid"), opts());
        let wire = serde_json::to_value(&answer).expect("serializes");
        let back: Value = serde_json::from_value(wire).expect("deserializes");
        assert!(!back.is_sealed_as("Verified"), "a wire round trip never yields a seal");
        assert_ne!(back, answer);
        let forged: SExpression = serde_json::from_value(serde_json::json!(
            { "subject": "root", "issuer": "i", "audience": "a", "claims": [], "expiresAt": 1 }
        ))
        .expect("literal");
        assert!(!eval_expr(&forged, &BindingContext::default()).expect("evaluates").is_sealed_as("Verified"));
        for form in [
            serde_json::json!(["as", { "subject": "root" }, "Verified"]),
            serde_json::json!(["json/parse", "{}", "Verified"]),
        ] {
            let expr: SExpression = serde_json::from_value(form).expect("s-expression");
            assert!(eval_expr(&expr, &BindingContext::default()).is_err());
        }
    }

    #[test]
    fn a_verified_fits_only_its_own_declared_type_at_run_time() {
        use orbital_core_base::ir::types::OirType;
        let declared = OirType::TypeRef { name: "Verified".to_string() };
        let shape = orbital_core_base::ir::types::DeclaredShape { version: None, name: None, ty: declared, named: Default::default() };
        let answer = call(&token("rs256_valid"), opts());
        assert!(super::super::arg_types::misfit_of_shape(&answer, &shape).is_none());
        let forged = Value::Object([("subject".into(), Value::string("root"))].into_iter().collect());
        assert!(super::super::arg_types::misfit_of_shape(&forged, &shape).is_some());
    }
}
