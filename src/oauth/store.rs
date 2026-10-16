//! An in-memory answerer of every [`OAuthAsk`]: the store a native or test host embeds. Pending grants are
//! consumed once under a TTL checked against the host clock (inclusive), connections are kept per principal and
//! provider, the subject index and discovered metadata are shared.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Map, Value};

use super::ask::{MockTokenState, OAuthAsk, OAuthConnectionRow, OAuthProvider, OAuthReply, PendingTake};
use super::Pending;

type Clock = Box<dyn Fn() -> i64 + Send + Sync>;

#[derive(Default)]
struct State {
    pending: HashMap<String, (Pending, i64)>,
    connections: HashMap<(String, OAuthProvider), OAuthConnectionRow>,
    subjects: HashMap<String, String>,
    metadata: HashMap<String, Map<String, Value>>,
    /// Mock mode: access token -> (refresh token, expiry), and refresh token -> access token.
    mock_tokens: HashMap<String, (String, i64, OAuthProvider)>,
    mock_refresh: HashMap<String, (String, OAuthProvider)>,
}

pub struct MemoryOAuthStore {
    state: Mutex<State>,
    clock: Clock,
    connections_available: bool,
}

fn disconnected(provider: OAuthProvider) -> OAuthConnectionRow {
    OAuthConnectionRow {
        provider,
        connected: false,
        account: String::new(),
        avatar_url: String::new(),
        scope: Vec::new(),
        connected_at: String::new(),
    }
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

impl MemoryOAuthStore {
    pub fn new(clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self { state: Mutex::new(State::default()), clock: Box::new(clock), connections_available: true }
    }

    /// A store with no connection persistence (`ConnectionAvailable` answers false).
    pub fn without_connections(mut self) -> Self {
        self.connections_available = false;
        self
    }

    /// Answer `ask` for `principal` (the signed-in user; `None` is anonymous and is denied any connection).
    pub fn answer(&self, ask: &OAuthAsk, principal: Option<&str>) -> Result<OAuthReply, String> {
        let now = (self.clock)();
        let mut state = self.state.lock().map_err(|_| "the OAuth store is poisoned".to_string())?;
        let caller = || principal.filter(|p| !p.is_empty()).ok_or_else(|| "a provider connection needs a signed-in caller".to_string());
        Ok(match ask {
            OAuthAsk::PendingPut { state: key, grant, ttl_ms } => {
                state.pending.retain(|_, (_, expires)| *expires >= now);
                state.pending.insert(key.clone(), (grant.clone(), now.saturating_add(i64::try_from(*ttl_ms).unwrap_or(i64::MAX))));
                OAuthReply::Done
            }
            OAuthAsk::PendingTake { state: key } => OAuthReply::Taken {
                taken: match state.pending.remove(key) {
                    None => PendingTake::Missing,
                    Some((_, expires)) if expires < now => PendingTake::Expired,
                    Some((grant, _)) => PendingTake::Grant { grant },
                },
            },
            OAuthAsk::ConnectionAvailable => OAuthReply::Flag { value: self.connections_available },
            OAuthAsk::ConnectionSave { provider, grant } => {
                if !self.connections_available {
                    return Err("no connection store".to_string());
                }
                let who = caller()?.to_string();
                let row = OAuthConnectionRow {
                    provider: *provider,
                    connected: true,
                    account: grant.subject.clone().unwrap_or_default(),
                    avatar_url: String::new(),
                    scope: grant.scope.clone().unwrap_or_default(),
                    connected_at: iso(now),
                };
                state.connections.insert((who, *provider), row.clone());
                OAuthReply::Row { row }
            }
            OAuthAsk::ConnectionStatus { provider } => {
                let who = caller()?.to_string();
                OAuthReply::Row { row: state.connections.get(&(who, *provider)).cloned().unwrap_or_else(|| disconnected(*provider)) }
            }
            OAuthAsk::ConnectionRemove { provider } => {
                let who = caller()?.to_string();
                state.connections.remove(&(who, *provider));
                OAuthReply::Row { row: disconnected(*provider) }
            }
            OAuthAsk::SubjectRemember { access_token, subject } => {
                state.subjects.insert(access_token.clone(), subject.clone());
                OAuthReply::Done
            }
            OAuthAsk::SubjectLookup { access_token } => OAuthReply::Subject { subject: state.subjects.get(access_token).cloned() },
            OAuthAsk::SubjectForget { access_token } => {
                state.subjects.remove(access_token);
                OAuthReply::Done
            }
            OAuthAsk::MetadataGet { issuer } => OAuthReply::Metadata { metadata: state.metadata.get(issuer).cloned() },
            OAuthAsk::MetadataPut { issuer, metadata } => {
                state.metadata.insert(issuer.clone(), metadata.clone());
                OAuthReply::Done
            }
            OAuthAsk::Clock => OAuthReply::Millis { value: now },
            OAuthAsk::MockTokenPut { access_token, refresh_token, expires_at_ms, provider } => {
                state.mock_tokens.insert(access_token.clone(), (refresh_token.clone(), *expires_at_ms, *provider));
                state.mock_refresh.insert(refresh_token.clone(), (access_token.clone(), *provider));
                OAuthReply::Done
            }
            OAuthAsk::MockTokenRefresh { refresh_token, new_access_token, expires_at_ms } => {
                let Some((old, provider)) = state.mock_refresh.get(refresh_token).cloned() else {
                    return Ok(OAuthReply::Flag { value: false });
                };
                state.mock_tokens.remove(&old);
                if let Some(subject) = state.subjects.remove(&old) {
                    state.subjects.insert(new_access_token.clone(), subject);
                }
                state.mock_tokens.insert(new_access_token.clone(), (refresh_token.clone(), *expires_at_ms, provider));
                state.mock_refresh.insert(refresh_token.clone(), (new_access_token.clone(), provider));
                OAuthReply::Flag { value: true }
            }
            OAuthAsk::MockTokenRevoke { token } => {
                if let Some((refresh, _, _)) = state.mock_tokens.remove(token) {
                    state.mock_refresh.remove(&refresh);
                    state.subjects.remove(token);
                    return Ok(OAuthReply::Flag { value: true });
                }
                if let Some((access, _)) = state.mock_refresh.remove(token) {
                    state.mock_tokens.remove(&access);
                    state.subjects.remove(&access);
                    return Ok(OAuthReply::Flag { value: true });
                }
                OAuthReply::Flag { value: false }
            }
            OAuthAsk::MockTokenCheck { access_token } => {
                let held = state.mock_tokens.get(access_token).map(|(_, expires, provider)| (*expires, *provider));
                match held {
                    None => OAuthReply::MockToken { state: MockTokenState::Missing, provider: None },
                    Some((expires, _)) if now > expires => {
                        state.mock_tokens.remove(access_token);
                        state.subjects.remove(access_token);
                        OAuthReply::MockToken { state: MockTokenState::Expired, provider: None }
                    }
                    Some((_, provider)) => OAuthReply::MockToken { state: MockTokenState::Valid, provider: Some(provider) },
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    fn grant() -> Pending {
        Pending { provider: "google".into(), redirect_uri: "https://app/cb".into(), verifier: "v".into(), subject: Some("u1".into()), scopes: Vec::new() }
    }

    fn store() -> (MemoryOAuthStore, Arc<AtomicI64>) {
        let now = Arc::new(AtomicI64::new(1_000));
        let seen = now.clone();
        (MemoryOAuthStore::new(move || seen.load(Ordering::SeqCst)), now)
    }

    fn put(store: &MemoryOAuthStore) {
        let ask = OAuthAsk::PendingPut { state: "s".into(), grant: grant(), ttl_ms: 600_000 };
        assert_eq!(store.answer(&ask, None), Ok(OAuthReply::Done));
    }

    fn take(store: &MemoryOAuthStore) -> PendingTake {
        match store.answer(&OAuthAsk::PendingTake { state: "s".into() }, None) {
            Ok(OAuthReply::Taken { taken }) => taken,
            other => panic!("not a take: {other:?}"),
        }
    }

    #[test]
    fn a_pending_grant_is_taken_once_and_lives_exactly_its_ttl() {
        let (store, now) = store();
        put(&store);
        now.store(1_000 + 600_000, Ordering::SeqCst);
        assert_eq!(take(&store), PendingTake::Grant { grant: grant() });
        assert_eq!(take(&store), PendingTake::Missing);
        put(&store);
        now.store(1_000 + 600_001 + 600_000, Ordering::SeqCst);
        assert_eq!(take(&store), PendingTake::Expired);
        assert_eq!(take(&store), PendingTake::Missing);
    }

    #[test]
    fn connections_are_per_principal_and_anonymous_is_denied() {
        let (store, _) = store();
        let save = OAuthAsk::ConnectionSave {
            provider: OAuthProvider::Google,
            grant: super::super::ask::OAuthTokenGrant {
                access_token: "a".into(),
                refresh_token: None,
                expires_in: 1.0,
                token_type: "bearer".into(),
                scope: Some(vec!["x".into()]),
                subject: Some("acct".into()),
            },
        };
        assert!(matches!(store.answer(&save, Some("u1")), Ok(OAuthReply::Row { row }) if row.connected && row.account == "acct"));
        let status = OAuthAsk::ConnectionStatus { provider: OAuthProvider::Google };
        assert!(matches!(store.answer(&status, Some("u1")), Ok(OAuthReply::Row { row }) if row.connected));
        assert!(matches!(store.answer(&status, Some("u2")), Ok(OAuthReply::Row { row }) if !row.connected));
        assert!(store.answer(&status, None).is_err());
        assert!(store.answer(&save, None).is_err());
        let forget = OAuthAsk::ConnectionRemove { provider: OAuthProvider::Google };
        assert!(matches!(store.answer(&forget, Some("u2")), Ok(OAuthReply::Row { row }) if !row.connected));
        assert!(matches!(store.answer(&status, Some("u1")), Ok(OAuthReply::Row { row }) if row.connected));
    }
}
