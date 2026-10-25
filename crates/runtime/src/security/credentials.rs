//! Credential resolution: the tenant credential store first, then the host env, as `configureFromStore`
//! layers `{ ...env, ...store.snapshotEnv() }` in `@almadar/runtime`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use orbital_core_base::oauth_ask::{OAuthAsk, OAuthReply};
use crate::oauth::store::CredentialOAuthStore;
use crate::credential_store::EncryptedCredentialStore;
use orbital_core_base::credential_admin as admin;
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
    /// `ALMADAR_CREDENTIALS_SOURCE=store`: every credential but a host setting answers from the store alone.
    store_source: bool,
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
        let store_source = admin::source_is_store(env.get(admin::SOURCE_VAR).map(String::as_str));
        Self {
            store_source,
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

    /// The encrypted credential store (the twin of `@almadar/runtime` `CredentialStore`): a stored value shadows its
    /// environment counterpart, and the kernel's credential administration asks reach it. The store is warmed here.
    pub fn with_encrypted_store(mut self, store: Arc<EncryptedCredentialStore>) -> Result<Self, String> {
        store.warm()?;
        self.encrypted = Some(store);
        Ok(self)
    }

    /// Whether `ALMADAR_CREDENTIALS_SOURCE=store` made the store the only source of credentials.
    pub fn store_source(&self) -> bool {
        self.store_source
    }

    /// Copy every declared credential the store lacks and the env holds into the store, as a `store`-source boot does,
    /// and return how many rows it wrote. A host with no enabled store seeds nothing.
    pub fn seed_store_from_env(&self, catalog: &admin::Catalog) -> Result<usize, String> {
        let Some(store) = self.encrypted.as_ref().filter(|store| store.enabled()) else {
            return Ok(0);
        };
        let rows = admin::seed_plan(
            catalog,
            &|name| store.resolve(name, None).is_some(),
            &|name| self.env.get(name).cloned(),
        );
        for row in &rows {
            store.set(&row.service, &row.env_var, &row.value, None)?;
        }
        Ok(rows.len())
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
            store_source: self.store_source,
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

    fn store_mode_env(extra: &[(&str, &str)]) -> HashMap<String, String> {
        let mut pairs = vec![(admin::SOURCE_VAR, "store"), ("STRIPE_KEY", "env-key")];
        pairs.extend_from_slice(extra);
        map(&pairs)
    }

    #[test]
    fn under_store_source_a_credential_answers_from_the_store_and_a_missing_row_is_not_configured_never_the_env() {
        use crate::credential_store::MemoryCredentialRows;
        let creds = Credentials::from_env(store_mode_env(&[]))
            .with_encrypted_store(encrypted(Arc::new(MemoryCredentialRows::default())))
            .unwrap();
        assert!(creds.store_source());
        assert_eq!(creds.secret("STRIPE_KEY").unwrap(), None, "the env value is hidden, not a fallback");
        creds.credential_store_write("stripe", "STRIPE_KEY", "stored-9999").unwrap();
        assert_eq!(creds.secret("STRIPE_KEY").unwrap().as_deref(), Some("stored-9999"));
        assert!(creds.credential_store_remove("STRIPE_KEY").unwrap());
        assert_eq!(creds.secret("STRIPE_KEY").unwrap(), None, "a removed row does not resurrect the env value");
    }

    #[test]
    fn under_store_source_a_host_setting_still_reads_the_env_and_a_host_with_no_store_answers_nothing() {
        let creds = Credentials::from_env(store_mode_env(&[(admin::ADMIN_ROLES_VAR, "admin,owner")]));
        assert_eq!(creds.secret(admin::ADMIN_ROLES_VAR).unwrap().as_deref(), Some("admin,owner"));
        assert_eq!(creds.secret(admin::SOURCE_VAR).unwrap().as_deref(), Some("store"));
        assert_eq!(creds.secret("STRIPE_KEY").unwrap(), None);
        let preloaded = Credentials::from_env(store_mode_env(&[])).with_store(map(&[("STRIPE_KEY", "preloaded")]));
        assert_eq!(preloaded.secret("STRIPE_KEY").unwrap().as_deref(), Some("preloaded"));
    }

    #[test]
    fn the_source_vector_the_js_host_replays_is_answered_by_the_native_host() {
        let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/credentials-source.json")).unwrap();
        let vector: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let text = |value: &serde_json::Value| value.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect::<HashMap<_, _>>();
        let cases = vector["cases"].as_array().unwrap();
        assert!(cases.len() >= 8);
        for case in cases {
            let creds = Credentials::from_env(text(&case["env"])).with_store(text(&case["store"]));
            assert_eq!(creds.secret(case["name"].as_str().unwrap()).unwrap().as_deref(), case["answer"].as_str(), "{}", case["case"]);
        }
    }

    #[test]
    fn control_any_other_source_value_keeps_the_store_then_env_order() {
        for source in ["", "env", "Store"] {
            let creds = Credentials::from_env(map(&[(admin::SOURCE_VAR, source), ("STRIPE_KEY", "env-key")]));
            assert!(!creds.store_source(), "{source}");
            assert_eq!(creds.secret("STRIPE_KEY").unwrap().as_deref(), Some("env-key"), "{source}");
        }
    }

    #[test]
    fn a_store_source_boot_seeds_the_declared_env_credentials_the_store_lacks_and_keeps_every_value_out_of_debug() {
        use crate::credential_store::MemoryCredentialRows;
        let catalog = admin::Catalog::from_registry(
            r#"{ "services": { "stripe": { "backend": "production", "credentials": [{ "envVar": "STRIPE_KEY", "required": true, "description": "k" }, { "envVar": "STRIPE_WEBHOOK", "required": false, "description": "w" }] } } }"#,
        )
        .unwrap();
        let rows = Arc::new(MemoryCredentialRows::default());
        let creds = Credentials::from_env(store_mode_env(&[("STRIPE_WEBHOOK", "whsec_live"), ("UNDECLARED", "x")]))
            .with_encrypted_store(encrypted(rows.clone()))
            .unwrap();
        creds.credential_store_write("stripe", "STRIPE_WEBHOOK", "already-stored").unwrap();
        assert_eq!(creds.seed_store_from_env(&catalog).unwrap(), 1, "only the missing STRIPE_KEY seeds");
        assert_eq!(creds.secret("STRIPE_KEY").unwrap().as_deref(), Some("env-key"));
        assert_eq!(creds.secret("STRIPE_WEBHOOK").unwrap().as_deref(), Some("already-stored"), "a held row is never overwritten by the env");
        assert_eq!(creds.secret("UNDECLARED").unwrap(), None);
        assert_eq!(creds.seed_store_from_env(&catalog).unwrap(), 0, "a second boot seeds nothing");
        assert!(!format!("{creds:?}").contains("env-key"));
        let listing = creds.credential_store_list().unwrap().expect("a store");
        assert!(!format!("{listing:?}").contains("env-key"), "the listing names rows, never values");
    }

    #[test]
    fn control_a_host_with_no_enabled_store_seeds_nothing() {
        let catalog = admin::Catalog::from_registry(
            r#"{ "services": { "stripe": { "backend": "production", "credentials": [{ "envVar": "STRIPE_KEY", "required": true, "description": "k" }] } } }"#,
        )
        .unwrap();
        assert_eq!(Credentials::from_env(store_mode_env(&[])).seed_store_from_env(&catalog).unwrap(), 0);
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
    fn the_native_host_answers_the_label_stored_beside_a_credential_and_none_for_the_rest() {
        use crate::credential_store::MemoryCredentialRows;
        let store = encrypted(Arc::new(MemoryCredentialRows::default()));
        store.set_labeled("kernel", "SESSION", "refresh-SECRET", None, Some("ada@example.com")).unwrap();
        store.set("kernel", "PLAIN", "v", None).unwrap();
        let creds = Credentials::from_env(map(&[("ENV_ONLY", "env-key")])).with_encrypted_store(store).unwrap();
        assert_eq!(creds.label_of("SESSION", None), Ok(Some("ada@example.com".to_string())));
        assert_ne!(creds.label_of("SESSION", None), creds.secret_of("SESSION", None), "the label is never the secret");
        assert_eq!(creds.label_of("PLAIN", None), Ok(None));
        assert_eq!(creds.label_of("ENV_ONLY", None), Ok(None));
        assert_eq!(creds.label_of("SESSION", Some("alice")), Ok(None), "a user slot never borrows the app label");
        assert_eq!(Credentials::default().label_of("SESSION", None), Ok(None));
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
        if self.store_source && admin::store_only_name(name) {
            return Ok(self.store.get(name).filter(|value| !value.is_empty()).cloned());
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

    fn label_of(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
        Ok(self.encrypted.as_ref().and_then(|store| store.label(name, principal)))
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
    let (credentials, failure) = credentials_with_rows(None);
    if let Some(error) = failure {
        tracing::error!(target: orbital_core_base::logging::ns::SERVER_DATA, %error, "credential store unavailable");
    }
    credentials
}

/// The process environment's credentials, with `ALMADAR_CREDENTIAL_MASTER_KEY` set layered under the encrypted store
/// kept in `rows` (else the [`credentials_from_env`] file). A store that cannot be opened leaves the environment alone
/// and names why, for the host to refuse or warn.
pub fn credentials_with_rows(
    rows: Option<Arc<dyn crate::credential_store::CredentialRows>>,
) -> (Credentials, Option<String>) {
    use crate::credential_store::{FileCredentialRows, CREDENTIAL_MASTER_KEY_ENV};
    let base = Credentials::from_process_env();
    if std::env::var(CREDENTIAL_MASTER_KEY_ENV).is_err() {
        return (base, None);
    }
    let rows = rows.unwrap_or_else(|| Arc::new(FileCredentialRows::from_env()));
    match EncryptedCredentialStore::from_env(rows).and_then(|store| base.clone().with_encrypted_store(Arc::new(store))) {
        Ok(stored) => (stored, None),
        Err(error) => (base, Some(error)),
    }
}
