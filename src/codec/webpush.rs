//! The cryptography of web push: RFC 8291 `aes128gcm` content encryption and the VAPID ES256 JWT (RFC 8292).
//! Pure: the ephemeral key, the salt and the expiry are parameters.

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, KeyInit};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdh::diffie_hellman;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};

use super::hmac_sha256;
use super::lenient::base64_decode_lenient;

const RECORD_SIZE: usize = 4096;
const TAG_LENGTH: usize = 16;

/// A message to surface as the SDK's error.
#[derive(Debug, PartialEq, Eq)]
pub struct WebpushError(pub String);

fn fail(message: &str) -> WebpushError {
    WebpushError(message.to_string())
}

/// `Buffer.from(text, 'base64url')`: either alphabet, padding optional, foreign characters skipped.
pub fn decode_base64_any(text: &str) -> Vec<u8> {
    base64_decode_lenient(&text.replace('-', "+").replace('_', "/"))
}

fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    hmac_sha256(salt, ikm)
}

fn hkdf_expand(prk: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(length);
    let mut previous: Vec<u8> = Vec::new();
    let mut counter = 0u8;
    while output.len() < length {
        counter += 1;
        let mut block = previous.clone();
        block.extend_from_slice(info);
        block.push(counter);
        previous = hmac_sha256(prk, &block).to_vec();
        output.extend_from_slice(&previous);
    }
    output.truncate(length);
    output
}

/// `generateNonce(base, counter)` of `http_ece`: the counter XORed into the last six bytes.
fn record_nonce(base: &[u8], counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&base[..12]);
    let tail = u64::from_be_bytes([
        0, 0, nonce[6], nonce[7], nonce[8], nonce[9], nonce[10], nonce[11],
    ]) ^ counter;
    nonce[6..12].copy_from_slice(&tail.to_be_bytes()[2..8]);
    nonce
}

/// `encryptionHelper.encrypt(p256dh, auth, payload, 'aes128gcm').cipherText`.
pub fn encrypt_aes128gcm(
    user_public_key: &str,
    user_auth: &str,
    payload: &[u8],
    ephemeral_private_key: &[u8; 32],
    salt: &[u8; 16],
) -> Result<Vec<u8>, WebpushError> {
    let user_public = decode_base64_any(user_public_key);
    if user_public.len() != 65 {
        return Err(fail(
            "The subscription p256dh value should be 65 bytes long.",
        ));
    }
    let auth_secret = decode_base64_any(user_auth);
    if auth_secret.len() < 16 {
        return Err(fail(
            "The subscription auth key should be at least 16 bytes long",
        ));
    }

    let secret_key = SecretKey::from_slice(ephemeral_private_key)
        .map_err(|_| fail("Private key is not valid for specified curve."))?;
    let sender_public = secret_key.public_key().to_encoded_point(false);
    let receiver = PublicKey::from_sec1_bytes(&user_public)
        .map_err(|_| fail("Public key is not valid for specified curve"))?;
    let shared = diffie_hellman(secret_key.to_nonzero_scalar(), receiver.as_affine());

    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(&user_public);
    key_info.extend_from_slice(sender_public.as_bytes());
    let secret = hkdf_expand(
        &hkdf_extract(&auth_secret, shared.raw_secret_bytes().as_slice()),
        &key_info,
        32,
    );

    let prk = hkdf_extract(salt, &secret);
    let key = hkdf_expand(&prk, b"Content-Encoding: aes128gcm\0", 16);
    let nonce_base = hkdf_expand(&prk, b"Content-Encoding: nonce\0", 12);
    let cipher = Aes128Gcm::new(GenericArray::from_slice(&key));

    let mut out = Vec::with_capacity(payload.len() + 100);
    out.extend_from_slice(salt);
    out.extend_from_slice(&(RECORD_SIZE as u32).to_be_bytes());
    out.push(sender_public.as_bytes().len() as u8);
    out.extend_from_slice(sender_public.as_bytes());

    let data_per_record = RECORD_SIZE - 1 - TAG_LENGTH;
    let mut start = 0usize;
    let mut counter = 0u64;
    loop {
        let end = start + data_per_record;
        let last = end >= payload.len();
        let mut plain = payload[start.min(payload.len())..end.min(payload.len())].to_vec();
        plain.push(if last { 2 } else { 1 });
        let nonce = record_nonce(&nonce_base, counter);
        let block = cipher
            .encrypt(GenericArray::from_slice(&nonce), plain.as_slice())
            .map_err(|_| fail("invalid tag generated"))?;
        out.extend_from_slice(&block);
        if last {
            return Ok(out);
        }
        start = end;
        counter += 1;
    }
}

fn validate_subject(subject: &str) -> Result<(), WebpushError> {
    if subject.is_empty() {
        return Err(fail("No subject set in vapidDetails.subject."));
    }
    let parsed = url::Url::parse(subject)
        .map_err(|_| WebpushError(format!("Vapid subject is not a valid URL. {subject}")))?;
    if !matches!(parsed.scheme(), "https" | "mailto") {
        return Err(WebpushError(format!(
            "Vapid subject is not an https: or mailto: URL. {subject}"
        )));
    }
    Ok(())
}

fn is_url_safe_base64(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn validate_key(key: &str, missing: &str, label: &str, length: usize) -> Result<(), WebpushError> {
    if key.is_empty() {
        return Err(fail(missing));
    }
    if !is_url_safe_base64(key) {
        return Err(WebpushError(format!(
            "Vapid {label} key must be a URL safe Base 64 (without \"=\")"
        )));
    }
    if decode_base64_any(key).len() != length {
        return Err(WebpushError(format!(
            "Vapid {label} key should be {length} bytes long when decoded."
        )));
    }
    Ok(())
}

/// The compact ES256 JWT of `getVapidHeaders` (after the audience, subject and key checks of the SDK).
pub fn vapid_jwt(
    audience: &str,
    subject: &str,
    public_key: &str,
    private_key: &str,
    expiration: i64,
) -> Result<String, WebpushError> {
    if url::Url::parse(audience).is_err() {
        return Err(WebpushError(format!(
            "VAPID audience is not a url. {audience}"
        )));
    }
    validate_subject(subject)?;
    validate_key(
        public_key,
        "No key set vapidDetails.publicKey",
        "public",
        65,
    )?;
    validate_key(
        private_key,
        "No key set in vapidDetails.privateKey",
        "private",
        32,
    )?;

    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = format!(
        "{{\"aud\":{},\"exp\":{expiration},\"sub\":{}}}",
        serde_json::Value::String(audience.to_string()),
        serde_json::Value::String(subject.to_string())
    );
    let signing_input = format!("{header}.{}", URL_SAFE_NO_PAD.encode(claims.as_bytes()));
    let signing_key = SigningKey::from_slice(&decode_base64_any(private_key))
        .map_err(|_| fail("Vapid private key is not a valid P-256 scalar."))?;
    let signature: Signature = signing_key.sign(signing_input.as_bytes());
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    ))
}


#[cfg(test)]
mod tests {
    use super::*;

    fn b(text: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD.decode(text).unwrap()
    }

    fn key32(last: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[31] = last;
        k
    }

    #[test]
    fn hkdf_matches_rfc_5869_test_case_1() {
        let ikm = [0x0bu8; 22];
        let salt: Vec<u8> = (0u8..=0x0c).collect();
        let info: Vec<u8> = (0xf0u8..=0xf9).collect();
        let okm = hkdf_expand(&hkdf_extract(&salt, &ikm), &info, 42);
        let hex: String = okm.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
    }

    #[test]
    fn the_record_nonce_xors_the_counter_into_the_last_six_bytes() {
        let base: Vec<u8> = (1u8..=12).collect();
        assert_eq!(record_nonce(&base, 0).to_vec(), base);
        let n = record_nonce(&base, 1);
        assert_eq!(n[11], base[11] ^ 1);
        assert_eq!(&n[..11], &base[..11]);
    }

    /// RFC 8291 section 5: the worked example (plaintext, sender key, receiver key, auth secret, salt). The
    /// expected body is also what `http_ece` 1.2.0 produces for the same inputs.
    #[test]
    fn rfc_8291_section_5_example() {
        let body = encrypt_aes128gcm(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            "BTBZMqHH6r4Tts7J_aSIgg",
            b"When I grow up, I want to be a watermelon",
            &b("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw").try_into().unwrap(),
            &b("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap(),
        )
        .unwrap();
        assert_eq!(
            URL_SAFE_NO_PAD.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn a_payload_over_one_record_is_split_and_the_control_differs() {
        let ua = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        let auth = "BTBZMqHH6r4Tts7J_aSIgg";
        let big = vec![7u8; 4079 * 2 + 5];
        let long = encrypt_aes128gcm(ua, auth, &big, &key32(7), &[3u8; 16]).unwrap();
        // header (16 salt + 4 rs + 1 idlen + 65 key) + 3 records
        assert_eq!(long.len(), 86 + big.len() + 3 * (1 + TAG_LENGTH));
        let short = encrypt_aes128gcm(ua, auth, &big[..10], &key32(7), &[3u8; 16]).unwrap();
        assert_ne!(long[86..96], short[86..96]);
        let other = encrypt_aes128gcm(ua, auth, &big[..10], &key32(8), &[3u8; 16]).unwrap();
        assert_ne!(short, other);
    }

    #[test]
    fn short_keys_are_refused_with_the_sdk_messages() {
        let ok = URL_SAFE_NO_PAD.encode([4u8; 65]);
        let auth = URL_SAFE_NO_PAD.encode([1u8; 16]);
        let (k, s) = (key32(7), [3u8; 16]);
        assert_eq!(
            encrypt_aes128gcm("AAAA", &auth, b"x", &k, &s),
            Err(fail("The subscription p256dh value should be 65 bytes long."))
        );
        assert_eq!(
            encrypt_aes128gcm(&ok, "AAAA", b"x", &k, &s),
            Err(fail("The subscription auth key should be at least 16 bytes long"))
        );
        assert_eq!(
            encrypt_aes128gcm(&ok, &auth, b"x", &k, &s),
            Err(fail("Public key is not valid for specified curve"))
        );
        assert_eq!(
            encrypt_aes128gcm(&ok, &auth, b"x", &[0u8; 32], &s),
            Err(fail("Private key is not valid for specified curve."))
        );
    }

    #[test]
    fn vapid_checks_run_in_the_sdk_order() {
        let public = URL_SAFE_NO_PAD.encode([4u8; 65]);
        let private = URL_SAFE_NO_PAD.encode([9u8; 32]);
        let a = "https://push.example.test";
        assert_eq!(vapid_jwt(a, "", &public, &private, 1), Err(fail("No subject set in vapidDetails.subject.")));
        assert_eq!(vapid_jwt(a, "mailto:a@b.c", "", &private, 1), Err(fail("No key set vapidDetails.publicKey")));
        assert_eq!(vapid_jwt(a, "mailto:a@b.c", &public, "", 1), Err(fail("No key set in vapidDetails.privateKey")));
        assert!(vapid_jwt("nope", "mailto:a@b.c", &public, &private, 1).is_err());
        assert!(vapid_jwt(a, "http://x.test", &public, &private, 1).is_err());
        assert!(vapid_jwt(a, "mailto:a@b.c", &public, &private, 1).is_ok());
    }

    /// RFC 8292: the JWT is ES256 over `aud`/`exp`/`sub` and verifies under the matching public key.
    #[test]
    fn the_vapid_jwt_verifies_under_its_public_key() {
        use p256::ecdsa::signature::Verifier;
        use p256::ecdsa::VerifyingKey;
        let private = URL_SAFE_NO_PAD.encode(key32(9));
        let signing = SigningKey::from_slice(&key32(9)).unwrap();
        let public_point = signing.verifying_key().to_encoded_point(false);
        let public = URL_SAFE_NO_PAD.encode(public_point.as_bytes());
        let jwt = vapid_jwt("https://push.example.test", "mailto:a@b.c", &public, &private, 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(b(parts[0]), br#"{"typ":"JWT","alg":"ES256"}"#);
        assert_eq!(
            b(parts[1]),
            br#"{"aud":"https://push.example.test","exp":1700000000,"sub":"mailto:a@b.c"}"#
        );
        let input = format!("{}.{}", parts[0], parts[1]);
        let verifying = VerifyingKey::from_sec1_bytes(public_point.as_bytes()).unwrap();
        verifying
            .verify(input.as_bytes(), &Signature::from_slice(&b(parts[2])).unwrap())
            .unwrap();
        let other = vapid_jwt("https://push.example.test", "mailto:a@b.c", &public, &private, 1_700_000_001).unwrap();
        assert_ne!(jwt, other);
    }
}
