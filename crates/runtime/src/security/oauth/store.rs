//! The one answerer of every [`OAuthAsk`]. Pending grants and provider connections live in the host's
//! credentials, never in a store of their own: a pending grant is a derived secret under the authorizing
//! principal (`oauth:grant:<state>`, consumed once through [`SecretSource::take_secret_of`]) and a connection is a
//! durable credential of its signed-in owner (`oauth:connection:<provider>`, tokens included, never read back
//! whole). Only the subject index, discovered metadata and the mock token table stay process-local.
//! [`MemoryOAuthStore`] is the same answerer over an in-memory credential set, for a native or test host.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use orbital_core_base::oauth_ask::{
    MockTokenState, OAuthAsk, OAuthConnectionRow, OAuthProvider, OAuthReply, OAuthTokenGrant,
    PendingTake,
};
use orbital_core_base::oauth_wire::Pending;

use crate::http_service::SecretSource;

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// How long past its TTL a pending grant stays held, so an expired state answers `expired` and not `missing`.
pub const PENDING_GRACE_MS: u64 = 60 * 60 * 1000;

const NO_CALLER: &str = "a provider connection needs a signed-in caller";
const NO_CONNECTIONS: &str = "no connection store";
const POISONED: &str = "the OAuth store is poisoned";
const UNREADABLE: &str = "a held OAuth credential is unreadable";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingRecord {
    grant: Pending,
    expires_at: i64,
}

#[derive(Serialize, Deserialize)]
struct ConnectionRecord {
    row: OAuthConnectionRow,
    grant: OAuthTokenGrant,
}

fn pending_name(state: &str) -> String {
    format!("oauth:grant:{state}")
}

fn connection_name(provider: OAuthProvider) -> String {
    format!("oauth:connection:{}", provider.name())
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

#[derive(Default)]
struct Caches {
    subjects: HashMap<String, String>,
    metadata: HashMap<String, Map<String, Value>>,
    /// Mock mode: access token -> (refresh token, expiry, issuer), and refresh token -> (access token, issuer).
    mock_tokens: HashMap<String, (String, i64, OAuthProvider)>,
    mock_refresh: HashMap<String, (String, OAuthProvider)>,
}

pub struct CredentialOAuthStore {
    credentials: Arc<dyn SecretSource>,
    clock: Clock,
    caches: Mutex<Caches>,
}

impl CredentialOAuthStore {
    /// An answerer persisting through `credentials`, with `clock` as the host clock in epoch milliseconds.
    pub fn new(
        credentials: Arc<dyn SecretSource>,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self::from_clock(credentials, Arc::new(clock))
    }

    fn from_clock(credentials: Arc<dyn SecretSource>, clock: Clock) -> Self {
        Self {
            credentials,
            clock,
            caches: Mutex::new(Caches::default()),
        }
    }

    fn caches(&self) -> Result<MutexGuard<'_, Caches>, String> {
        self.caches.lock().map_err(|_| POISONED.to_string())
    }

    /// Answer `ask` for `principal` (the signed-in user; `None` is anonymous and is denied any connection).
    pub fn answer(&self, ask: &OAuthAsk, principal: Option<&str>) -> Result<OAuthReply, String> {
        let now = (self.clock)();
        let scope = principal.filter(|p| !p.is_empty());
        let caller = || scope.ok_or_else(|| NO_CALLER.to_string());
        Ok(match ask {
            OAuthAsk::PendingPut {
                state,
                grant,
                ttl_ms,
            } => {
                let record = PendingRecord {
                    grant: grant.clone(),
                    expires_at: now.saturating_add(i64::try_from(*ttl_ms).unwrap_or(i64::MAX)),
                };
                let text = serde_json::to_string(&record).map_err(|_| UNREADABLE.to_string())?;
                self.credentials.store_secret_of(
                    &pending_name(state),
                    &text,
                    ttl_ms.saturating_add(PENDING_GRACE_MS),
                    None,
                )?;
                OAuthReply::Done
            }
            OAuthAsk::PendingTake { state } => {
                let name = pending_name(state);
                let held = self.credentials.take_secret_of(&name, None)?;
                OAuthReply::Taken {
                    taken: match held {
                        None => PendingTake::Missing,
                        Some(text) => {
                            let record: PendingRecord =
                                serde_json::from_str(&text).map_err(|_| UNREADABLE.to_string())?;
                            if record.expires_at < now {
                                PendingTake::Expired
                            } else {
                                PendingTake::Grant {
                                    grant: record.grant,
                                }
                            }
                        }
                    },
                }
            }
            OAuthAsk::ConnectionAvailable => OAuthReply::Flag {
                value: self.credentials.durable_credentials(),
            },
            OAuthAsk::ConnectionSave { provider, grant } => {
                if !self.credentials.durable_credentials() {
                    return Err(NO_CONNECTIONS.to_string());
                }
                let who = caller()?;
                let row = OAuthConnectionRow {
                    provider: *provider,
                    connected: true,
                    account: grant.subject.clone().unwrap_or_default(),
                    avatar_url: String::new(),
                    scope: grant.scope.clone().unwrap_or_default(),
                    connected_at: iso(now),
                };
                let record = ConnectionRecord {
                    row: row.clone(),
                    grant: grant.clone(),
                };
                let text = serde_json::to_string(&record).map_err(|_| UNREADABLE.to_string())?;
                self.credentials.persist_secret_of(
                    &connection_name(*provider),
                    &text,
                    Some(who),
                )?;
                OAuthReply::Row { row }
            }
            OAuthAsk::ConnectionStatus { provider } => {
                let who = caller()?;
                let row = match self
                    .credentials
                    .secret_of(&connection_name(*provider), Some(who))?
                {
                    None => disconnected(*provider),
                    Some(text) => {
                        serde_json::from_str::<ConnectionRecord>(&text)
                            .map_err(|_| UNREADABLE.to_string())?
                            .row
                    }
                };
                OAuthReply::Row { row }
            }
            OAuthAsk::ConnectionRemove { provider } => {
                let who = caller()?;
                self.credentials
                    .forget_of(&connection_name(*provider), Some(who))?;
                OAuthReply::Row {
                    row: disconnected(*provider),
                }
            }
            OAuthAsk::SubjectRemember {
                access_token,
                subject,
            } => {
                self.caches()?
                    .subjects
                    .insert(access_token.clone(), subject.clone());
                OAuthReply::Done
            }
            OAuthAsk::SubjectLookup { access_token } => OAuthReply::Subject {
                subject: self.caches()?.subjects.get(access_token).cloned(),
            },
            OAuthAsk::SubjectForget { access_token } => {
                self.caches()?.subjects.remove(access_token);
                OAuthReply::Done
            }
            OAuthAsk::MetadataGet { issuer } => OAuthReply::Metadata {
                metadata: self.caches()?.metadata.get(issuer).cloned(),
            },
            OAuthAsk::MetadataPut { issuer, metadata } => {
                self.caches()?
                    .metadata
                    .insert(issuer.clone(), metadata.clone());
                OAuthReply::Done
            }
            OAuthAsk::Clock => OAuthReply::Millis { value: now },
            OAuthAsk::Consent { .. } => OAuthReply::Code { code: None },
            OAuthAsk::MockTokenPut {
                access_token,
                refresh_token,
                expires_at_ms,
                provider,
            } => {
                let mut caches = self.caches()?;
                caches.mock_tokens.insert(
                    access_token.clone(),
                    (refresh_token.clone(), *expires_at_ms, *provider),
                );
                caches
                    .mock_refresh
                    .insert(refresh_token.clone(), (access_token.clone(), *provider));
                OAuthReply::Done
            }
            OAuthAsk::MockTokenRefresh {
                refresh_token,
                new_access_token,
                expires_at_ms,
            } => {
                let mut caches = self.caches()?;
                let Some((old, provider)) = caches.mock_refresh.get(refresh_token).cloned() else {
                    return Ok(OAuthReply::Flag { value: false });
                };
                caches.mock_tokens.remove(&old);
                if let Some(subject) = caches.subjects.remove(&old) {
                    caches.subjects.insert(new_access_token.clone(), subject);
                }
                caches.mock_tokens.insert(
                    new_access_token.clone(),
                    (refresh_token.clone(), *expires_at_ms, provider),
                );
                caches
                    .mock_refresh
                    .insert(refresh_token.clone(), (new_access_token.clone(), provider));
                OAuthReply::Flag { value: true }
            }
            OAuthAsk::MockTokenRevoke { token } => {
                let mut caches = self.caches()?;
                if let Some((refresh, _, _)) = caches.mock_tokens.remove(token) {
                    caches.mock_refresh.remove(&refresh);
                    caches.subjects.remove(token);
                    return Ok(OAuthReply::Flag { value: true });
                }
                if let Some((access, _)) = caches.mock_refresh.remove(token) {
                    caches.mock_tokens.remove(&access);
                    caches.subjects.remove(&access);
                    return Ok(OAuthReply::Flag { value: true });
                }
                OAuthReply::Flag { value: false }
            }
            OAuthAsk::MockTokenCheck { access_token } => {
                let mut caches = self.caches()?;
                let held = caches
                    .mock_tokens
                    .get(access_token)
                    .map(|(_, expires, provider)| (*expires, *provider));
                match held {
                    None => OAuthReply::MockToken {
                        state: MockTokenState::Missing,
                        provider: None,
                    },
                    Some((expires, _)) if now > expires => {
                        caches.mock_tokens.remove(access_token);
                        caches.subjects.remove(access_token);
                        OAuthReply::MockToken {
                            state: MockTokenState::Expired,
                            provider: None,
                        }
                    }
                    Some((_, provider)) => OAuthReply::MockToken {
                        state: MockTokenState::Valid,
                        provider: Some(provider),
                    },
                }
            }
        })
    }
}

type Slot = (String, Option<String>);

/// An in-memory credential set: derived secrets that expire by the host clock, and durable credentials that stay
/// until forgotten. A slot is a name under one principal, or under none for the app-wide credential.
pub struct MemoryCredentials {
    clock: Clock,
    durable: AtomicBool,
    derived: Mutex<HashMap<Slot, (String, i64)>>,
    stored: Mutex<HashMap<Slot, String>>,
}

impl MemoryCredentials {
    pub fn new(clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self::from_clock(Arc::new(clock))
    }

    fn from_clock(clock: Clock) -> Self {
        Self {
            clock,
            durable: AtomicBool::new(true),
            derived: Mutex::new(HashMap::new()),
            stored: Mutex::new(HashMap::new()),
        }
    }

    /// A set that keeps no durable credential (`durable_credentials` answers false).
    pub fn without_durable(self) -> Self {
        self.durable.store(false, Ordering::SeqCst);
        self
    }

    fn slot(name: &str, principal: Option<&str>) -> Slot {
        (name.to_string(), principal.map(str::to_string))
    }

    fn derived(&self) -> Result<MutexGuard<'_, HashMap<Slot, (String, i64)>>, String> {
        self.derived.lock().map_err(|_| POISONED.to_string())
    }

    fn stored(&self) -> Result<MutexGuard<'_, HashMap<Slot, String>>, String> {
        self.stored.lock().map_err(|_| POISONED.to_string())
    }
}

impl SecretSource for MemoryCredentials {
    fn secret(&self, name: &str) -> Result<Option<String>, String> {
        self.secret_of(name, None)
    }

    fn secret_of(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
        let slot = Self::slot(name, principal);
        let now = (self.clock)();
        {
            let mut derived = self.derived()?;
            if let Some((value, expires_at)) = derived.get(&slot).cloned() {
                if expires_at > now {
                    return Ok(Some(value));
                }
                derived.remove(&slot);
            }
        }
        Ok(self.stored()?.get(&slot).cloned())
    }

    fn store_secret(&self, name: &str, value: &str, ttl_ms: u64) -> Result<(), String> {
        self.store_secret_of(name, value, ttl_ms, None)
    }

    fn store_secret_of(
        &self,
        name: &str,
        value: &str,
        ttl_ms: u64,
        principal: Option<&str>,
    ) -> Result<(), String> {
        let slot = Self::slot(name, principal);
        let mut derived = self.derived()?;
        if ttl_ms == 0 {
            derived.remove(&slot);
        } else {
            let expires_at =
                (self.clock)().saturating_add(i64::try_from(ttl_ms).unwrap_or(i64::MAX));
            derived.insert(slot, (value.to_string(), expires_at));
        }
        Ok(())
    }

    fn persist_secret_of(
        &self,
        name: &str,
        value: &str,
        principal: Option<&str>,
    ) -> Result<(), String> {
        if !self.durable_credentials() {
            return Err("this host keeps no durable credentials".to_string());
        }
        self.stored()?
            .insert(Self::slot(name, principal), value.to_string());
        Ok(())
    }

    fn take_secret_of(
        &self,
        name: &str,
        principal: Option<&str>,
    ) -> Result<Option<String>, String> {
        let now = (self.clock)();
        Ok(self
            .derived()?
            .remove(&Self::slot(name, principal))
            .filter(|(_, expires_at)| *expires_at > now)
            .map(|(value, _)| value))
    }

    fn forget_of(&self, name: &str, principal: Option<&str>) -> Result<(), String> {
        let slot = Self::slot(name, principal);
        self.derived()?.remove(&slot);
        self.stored()?.remove(&slot);
        Ok(())
    }

    fn durable_credentials(&self) -> bool {
        self.durable.load(Ordering::SeqCst)
    }
}

/// [`CredentialOAuthStore`] over an in-memory credential set: the answerer a native or test host embeds.
pub struct MemoryOAuthStore {
    credentials: Arc<MemoryCredentials>,
    store: CredentialOAuthStore,
}

impl MemoryOAuthStore {
    pub fn new(clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        let clock: Clock = Arc::new(clock);
        let credentials = Arc::new(MemoryCredentials::from_clock(clock.clone()));
        Self {
            store: CredentialOAuthStore::from_clock(credentials.clone(), clock),
            credentials,
        }
    }

    /// A store with no connection persistence (`ConnectionAvailable` answers false).
    pub fn without_connections(self) -> Self {
        self.credentials.durable.store(false, Ordering::SeqCst);
        self
    }

    /// The credentials this store persists into.
    pub fn credentials(&self) -> &MemoryCredentials {
        &self.credentials
    }

    pub fn answer(&self, ask: &OAuthAsk, principal: Option<&str>) -> Result<OAuthReply, String> {
        self.store.answer(ask, principal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn grant() -> Pending {
        Pending {
            provider: "google".into(),
            redirect_uri: "https://app/cb".into(),
            verifier: "v".into(),
            subject: Some("u1".into()),
            scopes: Vec::new(),
        }
    }

    fn token_grant() -> OAuthTokenGrant {
        OAuthTokenGrant {
            access_token: "AT-SECRET".into(),
            refresh_token: Some("RT-SECRET".into()),
            expires_in: 3600.0,
            token_type: "bearer".into(),
            scope: Some(vec!["x".into()]),
            subject: Some("acct".into()),
        }
    }

    fn store() -> (MemoryOAuthStore, Arc<AtomicI64>) {
        let now = Arc::new(AtomicI64::new(1_000));
        let seen = now.clone();
        (
            MemoryOAuthStore::new(move || seen.load(Ordering::SeqCst)),
            now,
        )
    }

    fn put_as(store: &MemoryOAuthStore, principal: Option<&str>) {
        let ask = OAuthAsk::PendingPut {
            state: "s".into(),
            grant: grant(),
            ttl_ms: 600_000,
        };
        assert_eq!(store.answer(&ask, principal), Ok(OAuthReply::Done));
    }

    fn put(store: &MemoryOAuthStore) {
        put_as(store, None);
    }

    fn take_as(store: &MemoryOAuthStore, principal: Option<&str>) -> PendingTake {
        match store.answer(&OAuthAsk::PendingTake { state: "s".into() }, principal) {
            Ok(OAuthReply::Taken { taken }) => taken,
            other => panic!("not a take: {other:?}"),
        }
    }

    fn take(store: &MemoryOAuthStore) -> PendingTake {
        take_as(store, None)
    }

    fn save() -> OAuthAsk {
        OAuthAsk::ConnectionSave {
            provider: OAuthProvider::Google,
            grant: token_grant(),
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
    fn a_pending_grant_is_forgotten_once_its_grace_has_passed() {
        let (store, now) = store();
        put(&store);
        now.store(
            1_000 + 600_000 + PENDING_GRACE_MS as i64 + 1,
            Ordering::SeqCst,
        );
        assert_eq!(take(&store), PendingTake::Missing);
    }

    #[test]
    fn a_pending_grant_is_one_app_slot_keyed_by_state_that_every_principal_resolves_alike() {
        let (store, _) = store();
        put_as(&store, Some("alice"));
        let credentials = store.credentials();
        let held = credentials
            .secret_of("oauth:grant:s", None)
            .expect("read")
            .expect("held in the app slot");
        assert!(held.contains("\"verifier\":\"v\"") && held.contains("\"subject\":\"u1\""));
        assert_eq!(
            credentials.secret_of("oauth:grant:s", Some("alice")),
            Ok(None)
        );
        assert_eq!(
            take_as(&store, Some("bob")),
            PendingTake::Grant { grant: grant() }
        );
        assert_eq!(take_as(&store, Some("alice")), PendingTake::Missing);
        assert_eq!(credentials.secret_of("oauth:grant:s", None), Ok(None));
    }

    #[test]
    fn control_an_anonymous_authorization_is_taken_by_a_signed_in_callback_once() {
        let (store, _) = store();
        put_as(&store, None);
        assert_eq!(
            take_as(&store, Some("alice")),
            PendingTake::Grant { grant: grant() }
        );
        assert_eq!(take_as(&store, None), PendingTake::Missing);
    }

    #[test]
    fn connections_are_per_principal_and_anonymous_is_denied() {
        let (store, _) = store();
        assert!(
            matches!(store.answer(&save(), Some("u1")), Ok(OAuthReply::Row { row }) if row.connected && row.account == "acct")
        );
        let status = OAuthAsk::ConnectionStatus {
            provider: OAuthProvider::Google,
        };
        assert!(
            matches!(store.answer(&status, Some("u1")), Ok(OAuthReply::Row { row }) if row.connected)
        );
        assert!(
            matches!(store.answer(&status, Some("u2")), Ok(OAuthReply::Row { row }) if !row.connected)
        );
        assert!(store.answer(&status, None).is_err());
        assert!(store.answer(&save(), None).is_err());
        let forget = OAuthAsk::ConnectionRemove {
            provider: OAuthProvider::Google,
        };
        assert!(
            matches!(store.answer(&forget, Some("u2")), Ok(OAuthReply::Row { row }) if !row.connected)
        );
        assert!(
            matches!(store.answer(&status, Some("u1")), Ok(OAuthReply::Row { row }) if row.connected)
        );
        assert!(
            matches!(store.answer(&forget, Some("u1")), Ok(OAuthReply::Row { row }) if !row.connected)
        );
        assert!(
            matches!(store.answer(&status, Some("u1")), Ok(OAuthReply::Row { row }) if !row.connected)
        );
    }

    #[test]
    fn a_connection_is_the_owners_durable_credential_and_its_tokens_never_come_back() {
        let (store, _) = store();
        let reply = store.answer(&save(), Some("u1")).expect("saved");
        let credentials = store.credentials();
        let held = credentials
            .secret_of("oauth:connection:google", Some("u1"))
            .expect("read")
            .expect("held for u1");
        assert!(held.contains("AT-SECRET") && held.contains("RT-SECRET"));
        assert_eq!(
            credentials.secret_of("oauth:connection:google", Some("u2")),
            Ok(None)
        );
        assert_eq!(
            credentials.secret_of("oauth:connection:google", None),
            Ok(None)
        );
        let status = store
            .answer(
                &OAuthAsk::ConnectionStatus {
                    provider: OAuthProvider::Google,
                },
                Some("u1"),
            )
            .expect("status");
        for answered in [
            format!("{reply:?}"),
            serde_json::to_string(&status).expect("json"),
        ] {
            assert!(
                !answered.contains("AT-SECRET") && !answered.contains("RT-SECRET"),
                "{answered}"
            );
        }
        store
            .answer(
                &OAuthAsk::ConnectionRemove {
                    provider: OAuthProvider::Google,
                },
                Some("u1"),
            )
            .expect("removed");
        assert_eq!(
            credentials.secret_of("oauth:connection:google", Some("u1")),
            Ok(None)
        );
    }

    #[test]
    fn a_host_without_durable_credentials_has_no_connection_store() {
        let (store, _) = store();
        let store = store.without_connections();
        assert_eq!(
            store.answer(&OAuthAsk::ConnectionAvailable, None),
            Ok(OAuthReply::Flag { value: false })
        );
        assert_eq!(
            store.answer(&save(), Some("u1")),
            Err(NO_CONNECTIONS.to_string())
        );
        let (store, _) = self::store();
        assert_eq!(
            store.answer(&OAuthAsk::ConnectionAvailable, None),
            Ok(OAuthReply::Flag { value: true })
        );
    }

    #[test]
    fn a_host_that_keeps_no_credentials_refuses_pending_grants_and_connections() {
        struct Bare;
        impl SecretSource for Bare {
            fn secret(&self, _: &str) -> Result<Option<String>, String> {
                Ok(None)
            }
        }
        let bare = CredentialOAuthStore::new(Arc::new(Bare), || 0);
        let put = OAuthAsk::PendingPut {
            state: "s".into(),
            grant: grant(),
            ttl_ms: 1,
        };
        assert!(bare.answer(&put, None).is_err());
        assert!(bare
            .answer(&OAuthAsk::PendingTake { state: "s".into() }, None)
            .is_err());
        assert_eq!(
            bare.answer(&OAuthAsk::ConnectionAvailable, None),
            Ok(OAuthReply::Flag { value: false })
        );
        assert!(bare.answer(&save(), Some("u1")).is_err());
    }

    #[test]
    fn an_unreadable_held_grant_fails_without_echoing_it() {
        let (store, _) = store();
        store
            .credentials()
            .store_secret_of("oauth:grant:s", "TOKEN-SHAPED-GARBAGE", 60_000, None)
            .expect("stored");
        let error = store
            .answer(&OAuthAsk::PendingTake { state: "s".into() }, None)
            .expect_err("unreadable");
        assert_eq!(error, UNREADABLE);
    }
}
