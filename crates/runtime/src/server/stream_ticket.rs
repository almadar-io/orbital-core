//! Single-use tickets for the live-event stream (`POST /api/events/ticket`, then `GET /api/events?ticket=`): an
//! `EventSource` sends no `Authorization` header, so the viewer is bound to a ticket minted on an authenticated request.
//! A ticket is 32 bytes from the host's CSPRNG, base64url, kept as a TTL secret holding the viewer's JSON.

use crate::http_service::SecretSource;
use orbital_core_base::value::Value;

/// How long a minted ticket stays valid when the host names no other lifetime.
pub const DEFAULT_TTL_MS: u64 = 60_000;

fn slot(ticket: &str) -> String {
    format!("auth:stream-ticket:{ticket}")
}

/// Mint a ticket for `viewer` from 32 `random` bytes, held in `secrets` for `ttl_ms`.
pub fn mint(random: &[u8], secrets: &dyn SecretSource, viewer: &Value, ttl_ms: u64) -> Result<String, String> {
    if random.len() != 32 {
        return Err(format!("a stream ticket takes 32 random bytes, got {}", random.len()));
    }
    let ticket = orbital_core_base::codec::base64url_encode(random);
    let held = serde_json::to_string(viewer).map_err(|e| e.to_string())?;
    secrets.store_secret(&slot(&ticket), &held, ttl_ms)?;
    Ok(ticket)
}

/// The viewer `ticket` was minted for, once: a forged, expired or already used ticket is `None`.
pub fn take(secrets: &dyn SecretSource, ticket: &str) -> Option<Value> {
    let held = secrets.take_secret_of(&slot(ticket), None).ok().flatten()?;
    serde_json::from_str(&held).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::Credentials;

    #[test]
    fn a_ticket_names_its_viewer_once() {
        let secrets = Credentials::from_env(Default::default());
        let viewer: Value = serde_json::from_str(r#"{"id":"u1"}"#).unwrap();
        let ticket = mint(&[7; 32], &secrets, &viewer, DEFAULT_TTL_MS).unwrap();
        assert_eq!(take(&secrets, &ticket), Some(viewer));
        assert_eq!(take(&secrets, &ticket), None, "a used ticket is gone");
    }

    #[test]
    fn control_a_forged_ticket_and_short_random_bytes_are_refused() {
        let secrets = Credentials::from_env(Default::default());
        assert_eq!(take(&secrets, "forged"), None);
        assert!(mint(&[1; 8], &secrets, &Value::Null, DEFAULT_TTL_MS).is_err());
    }
}
