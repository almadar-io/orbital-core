//! Credential resolution: the tenant credential store first, then the host env, as `configureFromStore`
//! layers `{ ...env, ...store.snapshotEnv() }` in `@almadar/integrations`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use orbital_core_base::oauth_ask::{OAuthAsk, OAuthReply};
use crate::oauth::store::CredentialOAuthStore;
use crate::credential_store::EncryptedCredentialStore;
use crate::http_service::SecretSource;

/// One signed-in user's credentials, keyed by `(name, principal)`: derived secrets that expire, and durable ones
/// that stay until forgotten.
#[derive(Default)]
struct UserSlots {
    derived: HashMap<(String, String), (String, Instant)>,
    durable: HashMap<(String, String), String>,
}

const POISONED: &str = "credential cache lock poisoned";

#[derive(Clone, Default)]
pub struct Credentials {
    env: HashMap<String, String>,
    store: HashMap<String, String>,
    minted: Arc<Mutex<HashMap<String, (String, Instant)>>>,
    users: Arc<Mutex<UserSlots>>,
    oauth: Arc<OnceLock<CredentialOAuthStore>>,
    encrypted: Option<Arc<EncryptedCredentialStore>>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("env_count", &self.env.len())
            .field("store_count", &self.store.len())
            .finish_non_exhaustive()
    }
}

impl Credentials {
    pub fn from_env(env: HashMap<String, String>) -> Self {
        Self {
            env,
            store: HashMap::new(),
            minted: Arc::default(),
            users: Arc::default(),
            oauth: Arc::default(),
            encrypted: None,
        }
    }

    pub fn from_process_env() -> Self {
        Self::from_env(std::env::vars().collect())
    }

    /// The encrypted credential store (the twin of `@almadar/integrations` `CredentialStore`): a stored value shadows its
    /// environment counterpart, and the kernel's credential administration asks reach it. The store is warmed here.
    pub fn with_encrypted_store(mut self, store: Arc<EncryptedCredentialStore>) -> Result<Self, String> {
        store.warm()?;
        self.encrypted = Some(store);
        Ok(self)
    }

    pub fn with_store(mut self, store: HashMap<String, String>) -> Self {
        self.store = store;
        self.minted = Arc::default();
        self.users = Arc::default();
        self.oauth = Arc::default();
        self
    }

    /// The value for `name`: a store entry wins over the env; an empty value counts as absent.
    /// This host's credentials with no OAuth state of their own: the set the OAuth store persists into.
    fn without_oauth(&self) -> Self {
        Self {
            env: self.env.clone(),
            store: self.store.clone(),
            minted: self.minted.clone(),
            users: self.users.clone(),
            oauth: Arc::default(),
            encrypted: self.encrypted.clone(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        let value = self.store.get(name).or_else(|| self.env.get(name))?;
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn encrypted(rows: Arc<dyn crate::credential_store::CredentialRows>) -> Arc<EncryptedCredentialStore> {
        Arc::new(EncryptedCredentialStore::new(rows, Some(&"ab".repeat(32)), None))
    }

    #[test]
    fn the_encrypted_store_shadows_the_env_and_answers_the_kernels_administration_asks() {
        use crate::credential_store::MemoryCredentialRows;
        let creds = Credentials::from_env(map(&[("STRIPE_KEY", "env-key")]))
            .with_encrypted_store(encrypted(Arc::new(MemoryCredentialRows::default())))
            .unwrap();
        assert_eq!(creds.secret("STRIPE_KEY").unwrap().as_deref(), Some("env-key"));
        creds.credential_store_write("stripe", "STRIPE_KEY", "stored-9999").unwrap();
        assert_eq!(creds.secret("STRIPE_KEY").unwrap().as_deref(), Some("stored-9999"));
        assert_eq!(creds.credential_store_read("STRIPE_KEY").unwrap().as_deref(), Some("stored-9999"));
        let listing = creds.credential_store_list().unwrap().expect("a store");
        assert!(listing.enabled);
        assert_eq!(listing.entries.len(), 1);
        assert!(creds.credential_store_remove("STRIPE_KEY").unwrap());
        assert_eq!(creds.secret("STRIPE_KEY").unwrap().as_deref(), Some("env-key"));
    }

    #[test]
    fn control_a_host_with_no_encrypted_store_reports_none_and_refuses_writes() {
        let creds = Credentials::from_env(map(&[("STRIPE_KEY", "env-key")]));
        assert_eq!(creds.credential_store_list().unwrap(), None);
        assert!(creds.credential_store_write("stripe", "STRIPE_KEY", "x").is_err());
        assert_eq!(creds.secret("STRIPE_KEY").unwrap().as_deref(), Some("env-key"));
    }

    #[test]
    fn store_wins_over_env() {
        let creds = Credentials::from_env(map(&[("OPENAI_API_KEY", "env-key")]))
            .with_store(map(&[("OPENAI_API_KEY", "store-key")]));
        assert_eq!(creds.get("OPENAI_API_KEY"), Some("store-key"));
    }

    #[test]
    fn control_env_answers_when_the_store_has_no_entry() {
        let creds = Credentials::from_env(map(&[("OPENAI_API_KEY", "env-key")]))
            .with_store(map(&[("OTHER", "x")]));
        assert_eq!(creds.get("OPENAI_API_KEY"), Some("env-key"));
    }

    #[test]
    fn an_empty_store_entry_revokes_the_env_value() {
        let creds = Credentials::from_env(map(&[("OPENAI_API_KEY", "env-key")]))
            .with_store(map(&[("OPENAI_API_KEY", "")]));
        assert_eq!(creds.get("OPENAI_API_KEY"), None);
    }

    #[test]
    fn missing_everywhere_is_none() {
        assert_eq!(Credentials::default().get("OPENAI_API_KEY"), None);
    }

    #[test]
    fn native_host_keeps_derived_tokens_until_expiry() {
        let creds = Credentials::from_env(map(&[("KEY", "original")]));
        creds.store_secret("KEY", "token", 60_000).unwrap();
        assert_eq!(creds.secret("KEY").unwrap().as_deref(), Some("token"));
        creds.store_secret("KEY", "expired", 0).unwrap();
        assert_eq!(creds.secret("KEY").unwrap().as_deref(), Some("original"));
        assert_eq!(creds.secret("MISSING").unwrap(), None);
    }

    #[test]
    fn user_credentials_are_isolated_per_principal_and_never_fall_back_to_the_app_value() {
        let creds = Credentials::from_env(map(&[("KEY", "app-value")]));
        creds
            .persist_secret_of("KEY", "alice-value", Some("alice"))
            .unwrap();
        assert_eq!(
            creds.secret_of("KEY", Some("alice")),
            Ok(Some("alice-value".to_string()))
        );
        assert_eq!(creds.secret_of("KEY", Some("bob")), Ok(None));
        assert_eq!(
            creds.secret_of("KEY", None),
            Ok(Some("app-value".to_string()))
        );
        creds.forget_of("KEY", Some("alice")).unwrap();
        assert_eq!(creds.secret_of("KEY", Some("alice")), Ok(None));
        assert!(creds.persist_secret_of("KEY", "x", None).is_err());
        assert!(creds.durable_credentials());
    }

    #[test]
    fn a_derived_secret_is_taken_once_in_its_own_scope() {
        let creds = Credentials::default();
        creds
            .store_secret_of("grant", "v", 60_000, Some("alice"))
            .unwrap();
        assert_eq!(creds.take_secret_of("grant", Some("bob")), Ok(None));
        assert_eq!(creds.take_secret_of("grant", None), Ok(None));
        assert_eq!(
            creds.take_secret_of("grant", Some("alice")),
            Ok(Some("v".to_string()))
        );
        assert_eq!(creds.take_secret_of("grant", Some("alice")), Ok(None));
        creds.store_secret_of("grant", "app", 60_000, None).unwrap();
        assert_eq!(
            creds.take_secret_of("grant", None),
            Ok(Some("app".to_string()))
        );
        assert_eq!(creds.take_secret_of("grant", None), Ok(None));
    }

    #[test]
    fn control_an_expired_derived_secret_is_not_taken() {
        let creds = Credentials::default();
        creds
            .store_secret_of("grant", "v", 1, Some("alice"))
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(creds.take_secret_of("grant", Some("alice")), Ok(None));
    }

    #[test]
    fn the_native_host_keeps_oauth_grants_and_connections_as_credentials() {
        use orbital_core_base::oauth_ask::{OAuthProvider, OAuthTokenGrant, PendingTake};
        use orbital_core_base::oauth_wire::Pending;
        let creds = Credentials::default();
        let grant = Pending {
            provider: "google".into(),
            redirect_uri: "https://app/cb".into(),
            verifier: "v".into(),
            subject: None,
            scopes: Vec::new(),
        };
        let put = OAuthAsk::PendingPut {
            state: "s".into(),
            grant: grant.clone(),
            ttl_ms: 600_000,
        };
        assert_eq!(creds.oauth_of(&put, Some("alice")), Ok(OAuthReply::Done));
        let take = OAuthAsk::PendingTake { state: "s".into() };
        assert_eq!(
            creds.oauth_of(&take, Some("bob")),
            Ok(OAuthReply::Taken {
                taken: PendingTake::Grant {
                    grant: grant.clone()
                }
            })
        );
        assert_eq!(
            creds.oauth_of(&take, Some("alice")),
            Ok(OAuthReply::Taken {
                taken: PendingTake::Missing
            })
        );
        assert_eq!(
            creds.oauth_of(&take, Some("alice")),
            Ok(OAuthReply::Taken {
                taken: PendingTake::Missing
            })
        );
        let save = OAuthAsk::ConnectionSave {
            provider: OAuthProvider::Google,
            grant: OAuthTokenGrant {
                access_token: "AT".into(),
                refresh_token: None,
                expires_in: 1.0,
                token_type: "bearer".into(),
                scope: None,
                subject: Some("acct".into()),
            },
        };
        assert!(
            matches!(creds.oauth_of(&save, Some("alice")), Ok(OAuthReply::Row { row }) if row.connected)
        );
        let status = OAuthAsk::ConnectionStatus {
            provider: OAuthProvider::Google,
        };
        assert!(
            matches!(creds.oauth_of(&status, Some("alice")), Ok(OAuthReply::Row { row }) if row.connected && row.account == "acct")
        );
        assert!(
            matches!(creds.oauth_of(&status, Some("bob")), Ok(OAuthReply::Row { row }) if !row.connected)
        );
        assert!(creds.oauth_of(&status, None).is_err());
    }

    #[test]
    fn debug_never_exposes_credential_values() {
        let creds = Credentials::from_env(map(&[("KEY", "env-secret")]))
            .with_store(map(&[("KEY", "stored-secret")]));
        creds
            .store_secret("TOKEN", "minted-secret", 60_000)
            .unwrap();
        let debug = format!("{creds:?}");
        assert!(!debug.contains("env-secret"));
        assert!(!debug.contains("stored-secret"));
        assert!(!debug.contains("minted-secret"));
    }

    #[test]
    fn native_token_cache_follows_credential_authority() {
        let first = Credentials::from_env(map(&[("KEY", "first-original")]));
        first.store_secret("KEY", "first-token", 60_000).unwrap();
        let shared = first.clone();
        assert_eq!(
            shared.secret("KEY").unwrap().as_deref(),
            Some("first-token")
        );
        let second = first.clone().with_store(map(&[("KEY", "second-original")]));
        assert_eq!(
            second.secret("KEY").unwrap().as_deref(),
            Some("second-original")
        );
        second.store_secret("KEY", "second-token", 60_000).unwrap();
        assert_eq!(first.secret("KEY").unwrap().as_deref(), Some("first-token"));
        assert_eq!(
            second.secret("KEY").unwrap().as_deref(),
            Some("second-token")
        );
        first.minted.lock().unwrap().insert(
            "KEY".into(),
            ("expired".into(), Instant::now() - Duration::from_millis(1)),
        );
        assert_eq!(
            shared.secret("KEY").unwrap().as_deref(),
            Some("first-original")
        );
        assert_eq!(
            second.secret("KEY").unwrap().as_deref(),
            Some("second-token")
        );
        assert_eq!(first.secret_of("KEY", Some("user")), Ok(None));
    }
}

impl SecretSource for Credentials {
    fn secret(&self, name: &str) -> Result<Option<String>, String> {
        let mut minted = self
            .minted
            .lock()
            .map_err(|_| "credential cache lock poisoned")?;
        if let Some((value, expires_at)) = minted.get(name) {
            if *expires_at > Instant::now() {
                return Ok(Some(value.clone()));
            }
            minted.remove(name);
        }
        if let Some(stored) = self.encrypted.as_ref().and_then(|store| store.resolve(name, None)) {
            return Ok(Some(stored));
        }
        Ok(self.get(name).map(str::to_string))
    }

    fn store_secret(&self, name: &str, value: &str, ttl_ms: u64) -> Result<(), String> {
        let mut minted = self
            .minted
            .lock()
            .map_err(|_| "credential cache lock poisoned")?;
        if ttl_ms == 0 {
            minted.remove(name);
        } else {
            let expires_at = Instant::now()
                .checked_add(Duration::from_millis(ttl_ms))
                .ok_or("credential cache lifetime exceeds host clock range")?;
            minted.insert(name.to_string(), (value.to_string(), expires_at));
        }
        Ok(())
    }

    fn secret_of(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
        let Some(who) = principal else {
            return self.secret(name);
        };
        let key = (name.to_string(), who.to_string());
        let mut users = self.users.lock().map_err(|_| POISONED)?;
        let live = users
            .derived
            .get(&key)
            .map(|(value, expires_at)| (value.clone(), *expires_at));
        if let Some((value, expires_at)) = live {
            if expires_at > Instant::now() {
                return Ok(Some(value));
            }
            users.derived.remove(&key);
        }
        if let Some(held) = users.durable.get(&key) {
            return Ok(Some(held.clone()));
        }
        Ok(self.encrypted.as_ref().and_then(|store| store.resolve(name, Some(who))))
    }

    fn store_secret_of(
        &self,
        name: &str,
        value: &str,
        ttl_ms: u64,
        principal: Option<&str>,
    ) -> Result<(), String> {
        let Some(who) = principal else {
            return self.store_secret(name, value, ttl_ms);
        };
        let key = (name.to_string(), who.to_string());
        let mut users = self.users.lock().map_err(|_| POISONED)?;
        if ttl_ms == 0 {
            users.derived.remove(&key);
        } else {
            let expires_at = Instant::now()
                .checked_add(Duration::from_millis(ttl_ms))
                .ok_or("credential cache lifetime exceeds host clock range")?;
            users.derived.insert(key, (value.to_string(), expires_at));
        }
        Ok(())
    }

    fn persist_secret_of(
        &self,
        name: &str,
        value: &str,
        principal: Option<&str>,
    ) -> Result<(), String> {
        let Some(who) = principal else {
            return Err("this host keeps no app-wide durable credentials".to_string());
        };
        self.users
            .lock()
            .map_err(|_| POISONED)?
            .durable
            .insert((name.to_string(), who.to_string()), value.to_string());
        Ok(())
    }

    fn take_secret_of(
        &self,
        name: &str,
        principal: Option<&str>,
    ) -> Result<Option<String>, String> {
        let now = Instant::now();
        let taken = match principal {
            None => self.minted.lock().map_err(|_| POISONED)?.remove(name),
            Some(who) => self
                .users
                .lock()
                .map_err(|_| POISONED)?
                .derived
                .remove(&(name.to_string(), who.to_string())),
        };
        Ok(taken
            .filter(|(_, expires_at)| *expires_at > now)
            .map(|(value, _)| value))
    }

    fn forget_of(&self, name: &str, principal: Option<&str>) -> Result<(), String> {
        match principal {
            None => {
                self.minted.lock().map_err(|_| POISONED)?.remove(name);
            }
            Some(who) => {
                let key = (name.to_string(), who.to_string());
                let mut users = self.users.lock().map_err(|_| POISONED)?;
                users.derived.remove(&key);
                users.durable.remove(&key);
            }
        }
        Ok(())
    }

    fn durable_credentials(&self) -> bool {
        true
    }

    fn oauth_of(&self, ask: &OAuthAsk, principal: Option<&str>) -> Result<OAuthReply, String> {
        self.oauth
            .get_or_init(|| {
                CredentialOAuthStore::new(Arc::new(self.without_oauth()), || {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |elapsed| {
                            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
                        })
                })
            })
            .answer(ask, principal)
    }

    fn credential_store_list(&self) -> Result<Option<crate::credential_admin::StoreListing>, String> {
        self.encrypted.as_ref().map(|store| store.listing()).transpose()
    }

    fn credential_store_read(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.encrypted.as_ref().and_then(|store| store.resolve(name, None)))
    }

    fn credential_store_write(&self, service: &str, name: &str, value: &str) -> Result<(), String> {
        let store = self.encrypted.as_ref().ok_or(orbital_core_base::credential_admin::NO_STORE)?;
        store.set(service, name, value, None).map(|_| ())
    }

    fn credential_store_remove(&self, name: &str) -> Result<bool, String> {
        let store = self.encrypted.as_ref().ok_or(orbital_core_base::credential_admin::NO_STORE)?;
        store.remove(name, None)
    }

    fn credential_store_rotate(&self) -> Result<crate::credential_admin::RotateCounts, String> {
        let store = self.encrypted.as_ref().ok_or(orbital_core_base::credential_admin::NO_STORE)?;
        store.rotate()
    }
}

/// The process environment's credentials; with `ALMADAR_CREDENTIAL_MASTER_KEY` set, the encrypted store in
/// `ALMADAR_CREDENTIALS_FILE` (else `.almadar/dev-credentials.json`) answers first, the same file the JS host reads.
pub fn credentials_from_env() -> Credentials {
    use crate::credential_store::{EncryptedCredentialStore, FileCredentialRows, CREDENTIAL_MASTER_KEY_ENV};
    let base = Credentials::from_process_env();
    if std::env::var(CREDENTIAL_MASTER_KEY_ENV).is_err() {
        return base;
    }
    let stored = EncryptedCredentialStore::from_env(std::sync::Arc::new(FileCredentialRows::from_env()))
        .and_then(|store| base.clone().with_encrypted_store(std::sync::Arc::new(store)));
    stored.unwrap_or_else(|error| {
        tracing::error!(target: orbital_core_base::logging::ns::SERVER_DATA, %error, "credential store unavailable");
        base
    })
}
