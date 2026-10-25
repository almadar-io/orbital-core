//! The encrypted credential store: one AES-256-GCM row per (credential name, principal) kept through a row port, the master
//! key held alone in `ALMADAR_CREDENTIAL_MASTER_KEY`. A row carries `service`, `envVar`, hex `ciphertext` / `iv` / `authTag`,
//! `keyId`, `updatedAt`, `principal` and an optional clear `label`. A stored value shadows its environment counterpart.
//! A one-use value (a pending OAuth grant) is a row of kind `derived` in the same rows: sealed the same way, with an
//! `expiresAt`, consumed by [`EncryptedCredentialStore::take`]. A row that cannot be read is a failure, never an absence.
//! Everything here compiles on wasm32 except the file medium; a host whose rows answer asynchronously keeps the store over
//! [`RecordedCredentialRows`] and applies the recorded writes itself.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::credential_admin::{RotateCounts, StoreListing, StoredName};
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use orbital_core_base::sync::Mutex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// The entity type the credential rows persist under.
pub const CREDENTIAL_ENTITY_TYPE: &str = "AlmadarIntegrationCredential";
pub const CREDENTIAL_MASTER_KEY_ENV: &str = "ALMADAR_CREDENTIAL_MASTER_KEY";
pub const CREDENTIAL_PREVIOUS_MASTER_KEY_ENV: &str = "ALMADAR_CREDENTIAL_MASTER_KEY_PREVIOUS";
/// Where the file-backed rows live when no path is named.
#[cfg(not(target_arch = "wasm32"))]
pub const CREDENTIALS_FILE_ENV: &str = "ALMADAR_CREDENTIALS_FILE";
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_CREDENTIALS_FILE: &str = ".almadar/dev-credentials.json";
/// The `kind` of a row holding a one-use value; a credential row carries no `kind`.
pub const DERIVED_KIND: &str = "derived";

/// The persistence the store uses: whole rows, as JSON objects carrying their `id`.
pub trait CredentialRows: Send + Sync {
    fn list(&self) -> Result<Vec<Value>, String>;
    /// Persist a new row and return its id.
    fn create(&self, data: &Value) -> Result<String, String>;
    /// Merge `data` over the row `id`; a missing row is left missing.
    fn update(&self, id: &str, data: &Value) -> Result<(), String>;
    /// Delete the row `id`; whether this call removed it (false when another taker or writer already had).
    fn delete(&self, id: &str) -> Result<bool, String>;
}

/// Rows kept in memory.
#[derive(Default)]
pub struct MemoryCredentialRows {
    rows: Mutex<Vec<Value>>,
    counter: AtomicU64,
}

impl MemoryCredentialRows {
    /// Replace every row with `rows`, as read from a medium.
    pub fn load(&self, rows: Vec<Value>) {
        *self.rows.lock() = rows;
    }
}

impl CredentialRows for MemoryCredentialRows {
    fn list(&self) -> Result<Vec<Value>, String> {
        Ok(self.rows.lock().clone())
    }
    fn create(&self, data: &Value) -> Result<String, String> {
        let id = format!(
            "{CREDENTIAL_ENTITY_TYPE}-{}-{}",
            chrono_free_now_ms(),
            self.counter.fetch_add(1, Ordering::SeqCst) + 1
        );
        self.rows.lock().push(with_id(data, &id));
        Ok(id)
    }
    fn update(&self, id: &str, data: &Value) -> Result<(), String> {
        let mut rows = self.rows.lock();
        if let Some(row) = rows.iter_mut().find(|row| row["id"] == id) {
            *row = merged(row, data, id);
        }
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<bool, String> {
        let mut rows = self.rows.lock();
        let before = rows.len();
        rows.retain(|row| row["id"] != id);
        Ok(rows.len() != before)
    }
}

/// Rows kept in memory that also record every change as `{ write: "create" | "update" | "delete", id, data? }`, in order.
/// A host whose medium answers asynchronously loads the rows it listed, runs the store, then applies
/// [`Self::take_writes`] to its medium: a created row carries the id the store gave it, an update is a merge.
#[derive(Default)]
pub struct RecordedCredentialRows {
    memory: MemoryCredentialRows,
    writes: Mutex<Vec<Value>>,
}

impl RecordedCredentialRows {
    /// Replace every row with `rows` read from the medium; changes not yet taken are dropped, for they were not applied.
    pub fn load(&self, rows: Vec<Value>) {
        self.memory.load(rows);
        self.writes.lock().clear();
    }

    pub fn take_writes(&self) -> Vec<Value> {
        std::mem::take(&mut *self.writes.lock())
    }
}

impl CredentialRows for RecordedCredentialRows {
    fn list(&self) -> Result<Vec<Value>, String> {
        self.memory.list()
    }
    fn create(&self, data: &Value) -> Result<String, String> {
        let id = self.memory.create(data)?;
        self.writes
            .lock()
            .push(json!({ "write": "create", "id": id, "data": with_id(data, &id) }));
        Ok(id)
    }
    fn update(&self, id: &str, data: &Value) -> Result<(), String> {
        if self.memory.list()?.iter().any(|row| row["id"] == id) {
            self.memory.update(id, data)?;
            self.writes
                .lock()
                .push(json!({ "write": "update", "id": id, "data": data }));
        }
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<bool, String> {
        let removed = self.memory.delete(id)?;
        if removed {
            self.writes
                .lock()
                .push(json!({ "write": "delete", "id": id }));
        }
        Ok(removed)
    }
}

fn with_id(data: &Value, id: &str) -> Value {
    let mut row = data.as_object().cloned().unwrap_or_default();
    row.insert("id".to_string(), json!(id));
    Value::Object(row)
}

fn merged(existing: &Value, data: &Value, id: &str) -> Value {
    let mut row = existing.as_object().cloned().unwrap_or_default();
    for (key, value) in data.as_object().into_iter().flatten() {
        row.insert(key.clone(), value.clone());
    }
    row.insert("id".to_string(), json!(id));
    Value::Object(row)
}

/// The dev credential file of `@almadar/db` (`FileCredentialPersistence`): a JSON array of rows, written atomically
/// (a temporary file, then a rename). A missing file is no rows; an unreadable one, or one that is not an array of
/// rows each carrying an `id`, is a failure.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileCredentialRows {
    path: PathBuf,
    counter: AtomicU64,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileCredentialRows {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            counter: AtomicU64::new(0),
        }
    }

    /// `ALMADAR_CREDENTIALS_FILE`, else `.almadar/dev-credentials.json` under the working directory.
    pub fn from_env() -> Self {
        match std::env::var(CREDENTIALS_FILE_ENV) {
            Ok(path) if !path.is_empty() => Self::new(path),
            _ => Self::new(
                std::env::current_dir()
                    .unwrap_or_default()
                    .join(DEFAULT_CREDENTIALS_FILE),
            ),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn load(&self) -> Result<Vec<Value>, String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("credential file {}: {error}", self.path.display())),
        };
        let rows = match serde_json::from_str::<Value>(&text) {
            Ok(Value::Array(rows)) => rows,
            Ok(_) => {
                return Err(format!(
                    "credential file {} is not an array of rows",
                    self.path.display()
                ))
            }
            Err(error) => {
                return Err(format!(
                    "credential file {} is not JSON: {error}",
                    self.path.display()
                ))
            }
        };
        if let Some(position) = rows.iter().position(|row| !row["id"].is_string()) {
            return Err(format!(
                "credential file {}: row {position} carries no id",
                self.path.display()
            ));
        }
        Ok(rows)
    }

    fn save(&self, rows: &[Value]) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("credential file: {e}"))?;
        }
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let text =
            serde_json::to_string_pretty(rows).map_err(|e| format!("credential file: {e}"))?;
        std::fs::write(&tmp, text).map_err(|e| format!("credential file: {e}"))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("credential file: {e}"))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl CredentialRows for FileCredentialRows {
    fn list(&self) -> Result<Vec<Value>, String> {
        self.load()
    }
    fn create(&self, data: &Value) -> Result<String, String> {
        let mut rows = self.load()?;
        let now = chrono_free_now_ms();
        let id = format!(
            "{CREDENTIAL_ENTITY_TYPE}-{now}-{}",
            self.counter.fetch_add(1, Ordering::SeqCst) + 1
        );
        rows.push(with_id(data, &id));
        self.save(&rows)?;
        Ok(id)
    }
    fn update(&self, id: &str, data: &Value) -> Result<(), String> {
        let mut rows = self.load()?;
        if let Some(row) = rows.iter_mut().find(|row| row["id"] == id) {
            *row = merged(row, data, id);
            self.save(&rows)?;
        }
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<bool, String> {
        let mut rows = self.load()?;
        let before = rows.len();
        rows.retain(|row| row["id"] != id);
        let removed = rows.len() != before;
        if removed {
            self.save(&rows)?;
        }
        Ok(removed)
    }
}

/// Rows kept in the app's own store, as the entity [`CREDENTIAL_ENTITY_TYPE`] (`FirestoreCredentialPersistence` in
/// `@almadar/db` keeps them in the app's Firestore the same way).
pub struct StoreCredentialRows(pub Arc<dyn crate::effects::executor::PersistenceHandler>);

impl StoreCredentialRows {
    fn json(value: orbital_core_base::value::Value) -> Result<Value, String> {
        serde_json::to_value(value).map_err(|e| e.to_string())
    }

    fn value(json: &Value) -> Result<orbital_core_base::value::Value, String> {
        serde_json::from_value(json.clone()).map_err(|e| e.to_string())
    }
}

impl CredentialRows for StoreCredentialRows {
    fn list(&self) -> Result<Vec<Value>, String> {
        self.0
            .list(CREDENTIAL_ENTITY_TYPE)?
            .into_iter()
            .map(Self::json)
            .collect()
    }
    fn create(&self, data: &Value) -> Result<String, String> {
        let row = Self::json(self.0.create(CREDENTIAL_ENTITY_TYPE, &Self::value(data)?)?)?;
        row["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "the store created a credential row without an id".to_string())
    }
    fn update(&self, id: &str, data: &Value) -> Result<(), String> {
        if self.0.find(CREDENTIAL_ENTITY_TYPE, id)?.is_none() {
            return Ok(());
        }
        self.0
            .update(CREDENTIAL_ENTITY_TYPE, id, &Self::value(data)?)
            .map(|_| ())
    }
    fn delete(&self, id: &str) -> Result<bool, String> {
        if self.0.find(CREDENTIAL_ENTITY_TYPE, id)?.is_none() {
            return Ok(false);
        }
        self.0.delete(CREDENTIAL_ENTITY_TYPE, id).map(|()| true)
    }
}

fn chrono_free_now_ms() -> i64 {
    orbital_core_base::time_util::now_millis()
}

struct Key {
    bytes: [u8; 32],
}

impl Key {
    fn parse(hex: &str) -> Option<Key> {
        if hex.len() != 64 {
            return None;
        }
        let bytes = unhex(hex)?;
        Some(Key {
            bytes: bytes.try_into().ok()?,
        })
    }

    fn id(&self) -> String {
        hex(&Sha256::digest(self.bytes))[..16].to_string()
    }
}

/// Whether `hex` is a master key: 64 hex digits.
pub fn is_master_key(hex: &str) -> bool {
    Key::parse(hex).is_some()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// `JSON.stringify([principal ?? null, name])`: the identity a row is stored and bound to.
pub fn storage_key(name: &str, principal: Option<&str>) -> String {
    json!([principal, name]).to_string()
}

fn last4(value: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    String::from_utf16_lossy(&units[units.len().saturating_sub(4)..])
}

/// Masked view of one stored credential, safe to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialEntry {
    pub service: String,
    pub env_var: String,
    pub last4: String,
    pub updated_at: i64,
    pub principal: Option<String>,
}

struct Stored {
    id: String,
    entry: CredentialEntry,
    value: String,
    label: Option<String>,
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stored")
            .field("entry", &self.entry)
            .finish_non_exhaustive()
    }
}

struct Sealed {
    ciphertext: String,
    iv: String,
    auth_tag: String,
    key_id: String,
}

pub type IvSource = dyn Fn() -> [u8; 12] + Send + Sync;

pub struct EncryptedCredentialStore {
    rows: Arc<dyn CredentialRows>,
    master: Option<Key>,
    previous: Option<Key>,
    iv: Box<IvSource>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    warmed: bool,
    by_identity: HashMap<String, Stored>,
    /// Held one-use rows found by [`EncryptedCredentialStore::take_begin`] whose delete the taker has not yet answered,
    /// by ticket: two takers of one row each hold their own.
    taking: HashMap<u64, Value>,
    tickets: u64,
}

/// A held one-use row [`EncryptedCredentialStore::take_begin`] found: the row to delete, and the ticket to finish or cancel with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldTake {
    pub ticket: u64,
    pub id: String,
}

impl std::fmt::Debug for EncryptedCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedCredentialStore")
            .field("enabled", &self.enabled())
            .finish_non_exhaustive()
    }
}

fn os_iv() -> [u8; 12] {
    let mut iv = [0u8; 12];
    getrandom::getrandom(&mut iv).expect("the operating system has no random source");
    iv
}

fn text_of<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row[field].as_str().filter(|text| !text.is_empty())
}

impl EncryptedCredentialStore {
    /// A store with the given master key (64 hex digits) and, during a rotation window, the previous one. A key
    /// that is not 64 hex digits is absent: the store then neither encrypts nor decrypts.
    pub fn new(
        rows: Arc<dyn CredentialRows>,
        master_hex: Option<&str>,
        previous_hex: Option<&str>,
    ) -> Self {
        Self::with_iv(rows, master_hex, previous_hex, Box::new(os_iv))
    }

    pub fn with_iv(
        rows: Arc<dyn CredentialRows>,
        master_hex: Option<&str>,
        previous_hex: Option<&str>,
        iv: Box<IvSource>,
    ) -> Self {
        Self {
            rows,
            master: master_hex.and_then(Key::parse),
            previous: previous_hex.and_then(Key::parse),
            iv,
            state: Mutex::new(State::default()),
        }
    }

    /// The keys from the environment. In production (`NODE_ENV=production`) a missing or malformed master key is an error.
    pub fn from_env(rows: Arc<dyn CredentialRows>) -> Result<Self, String> {
        let master = std::env::var(CREDENTIAL_MASTER_KEY_ENV).ok();
        let previous = std::env::var(CREDENTIAL_PREVIOUS_MASTER_KEY_ENV).ok();
        let store = Self::new(rows, master.as_deref(), previous.as_deref());
        if !store.enabled() && std::env::var("NODE_ENV").as_deref() == Ok("production") {
            return Err(format!("{CREDENTIAL_MASTER_KEY_ENV} must be a 64-character hex string (32 bytes) in production"));
        }
        Ok(store)
    }

    /// Whether a valid master key is present.
    pub fn enabled(&self) -> bool {
        self.master.is_some()
    }

    fn seal(&self, plaintext: &str, name: &str, principal: Option<&str>) -> Result<Sealed, String> {
        let Some(master) = &self.master else {
            return Err(format!(
                "{CREDENTIAL_MASTER_KEY_ENV} is not configured — cannot store credentials"
            ));
        };
        let iv = (self.iv)();
        let aad = principal.map(|_| storage_key(name, principal));
        let cipher = Aes256Gcm::new_from_slice(&master.bytes).map_err(|e| e.to_string())?;
        let mut buffer = plaintext.as_bytes().to_vec();
        let tag = cipher
            .encrypt_in_place_detached(
                Nonce::from_slice(&iv),
                aad.as_deref().unwrap_or("").as_bytes(),
                &mut buffer,
            )
            .map_err(|_| "credential encryption failed".to_string())?;
        Ok(Sealed {
            ciphertext: hex(&buffer),
            iv: hex(&iv),
            auth_tag: hex(&tag),
            key_id: master.id(),
        })
    }

    fn open_with(key: &Key, row: &Sealed, name: &str, principal: Option<&str>) -> Option<String> {
        let iv = unhex(&row.iv).filter(|iv| iv.len() == 12)?;
        let tag = unhex(&row.auth_tag).filter(|tag| tag.len() == 16)?;
        let mut buffer = unhex(&row.ciphertext)?;
        let aad = principal.map(|_| storage_key(name, principal));
        let cipher = Aes256Gcm::new_from_slice(&key.bytes).ok()?;
        cipher
            .decrypt_in_place_detached(
                Nonce::from_slice(&iv),
                aad.as_deref().unwrap_or("").as_bytes(),
                &mut buffer,
                Tag::from_slice(&tag),
            )
            .ok()?;
        Some(String::from_utf8_lossy(&buffer).into_owned())
    }

    /// Decrypt one row, choosing the key by its stamped `keyId`: the current key, then the rotation-window previous key. A row
    /// no held key opens is `None`, never a wrong value.
    fn open(
        &self,
        row: &Sealed,
        row_key_id: Option<&str>,
        name: &str,
        principal: Option<&str>,
    ) -> Option<String> {
        let master = self.master.as_ref()?;
        let mut candidates = Vec::new();
        if row_key_id.is_none() || row_key_id == Some(master.id().as_str()) {
            candidates.push(master);
        }
        if let Some(previous) = &self.previous {
            if row_key_id.is_none() || row_key_id == Some(previous.id().as_str()) {
                candidates.push(previous);
            }
        }
        candidates
            .into_iter()
            .find_map(|key| Self::open_with(key, row, name, principal))
    }

    fn sealed_of(row: &Value) -> Option<Sealed> {
        Some(Sealed {
            ciphertext: row["ciphertext"].as_str()?.to_string(),
            iv: text_of(row, "iv")?.to_string(),
            auth_tag: text_of(row, "authTag")?.to_string(),
            key_id: row["keyId"].as_str().unwrap_or("").to_string(),
        })
    }

    /// The row's identity fields when it is well formed.
    fn identity_of(row: &Value) -> Option<(&str, &str, &str, Option<&str>)> {
        let principal = match row.get("principal") {
            None => None,
            Some(value) => Some(value.as_str().filter(|text| !text.is_empty())?),
        };
        Some((
            text_of(row, "id")?,
            text_of(row, "service")?,
            text_of(row, "envVar")?,
            principal,
        ))
    }

    /// The rows that cannot be read, as `rows` holds them: a row that is malformed, of an unknown kind, or that no held key opens.
    /// A row of kind `derived` is a one-use value, read when it is taken, not here.
    fn scan(&self, rows: &[Value]) -> (HashMap<String, Stored>, Vec<String>) {
        let mut found = HashMap::new();
        let mut unreadable = Vec::new();
        for (position, row) in rows.iter().enumerate() {
            let label = || {
                text_of(row, "id")
                    .map_or_else(|| format!("(row {position}, no id)"), str::to_string)
            };
            match row.get("kind") {
                None => {}
                Some(kind) if kind.as_str() == Some(DERIVED_KIND) => continue,
                Some(_) => {
                    unreadable.push(label());
                    continue;
                }
            }
            let (Some((id, service, env_var, principal)), Some(sealed)) =
                (Self::identity_of(row), Self::sealed_of(row))
            else {
                unreadable.push(label());
                continue;
            };
            let Some(value) = self.open(&sealed, text_of(row, "keyId"), env_var, principal) else {
                unreadable.push(label());
                continue;
            };
            found.insert(
                storage_key(env_var, principal),
                Stored {
                    id: id.to_string(),
                    entry: CredentialEntry {
                        service: service.to_string(),
                        env_var: env_var.to_string(),
                        last4: last4(&value),
                        updated_at: row["updatedAt"].as_f64().map_or(0, |ms| ms as i64),
                        principal: principal.map(str::to_string),
                    },
                    value,
                    label: text_of(row, "label").map(str::to_string),
                },
            );
        }
        (found, unreadable)
    }

    fn unreadable_failure(ids: &[String]) -> String {
        format!(
            "{} credential row(s) cannot be read (malformed, or no held master key opens them): {}",
            ids.len(),
            ids.join(", ")
        )
    }

    /// Load and decrypt every stored row; the number of readable credentials. A row that cannot be read fails the load, naming
    /// the row ids, and leaves the store unwarmed. Without a master key nothing decrypts and the rows are left alone.
    pub fn warm(&self) -> Result<usize, String> {
        let mut state = self.state.lock();
        state.by_identity.clear();
        state.warmed = false;
        if !self.enabled() {
            state.warmed = true;
            return Ok(0);
        }
        let (found, unreadable) = self.scan(&self.rows.list()?);
        if !unreadable.is_empty() {
            return Err(Self::unreadable_failure(&unreadable));
        }
        state.by_identity = found;
        state.warmed = true;
        Ok(state.by_identity.len())
    }

    /// Whether the rows have been read; the reads below answer nothing before that.
    pub fn warmed(&self) -> bool {
        self.state.lock().warmed
    }

    fn ensure_warm(&self) -> Result<(), String> {
        if self.warmed() {
            return Ok(());
        }
        self.warm().map(|_| ())
    }

    /// One warmed credential by name.
    pub fn resolve(&self, name: &str, principal: Option<&str>) -> Option<String> {
        self.state
            .lock()
            .by_identity
            .get(&storage_key(name, principal))
            .map(|stored| stored.value.clone())
    }

    /// The non-secret account label stored beside a warmed credential, `None` when its row carries none.
    pub fn label(&self, name: &str, principal: Option<&str>) -> Option<String> {
        self.state
            .lock()
            .by_identity
            .get(&storage_key(name, principal))
            .and_then(|stored| stored.label.clone())
    }

    /// Every warmed app-wide value as an environment-shaped map.
    pub fn snapshot_env(&self) -> HashMap<String, String> {
        self.state
            .lock()
            .by_identity
            .values()
            .filter(|stored| stored.entry.principal.is_none())
            .map(|stored| (stored.entry.env_var.clone(), stored.value.clone()))
            .collect()
    }

    /// Masked entries of `principal`'s credentials (or the app-wide ones), ordered by service then name.
    pub fn entries(&self, principal: Option<&str>) -> Vec<CredentialEntry> {
        let mut found: Vec<CredentialEntry> = self
            .state
            .lock()
            .by_identity
            .values()
            .filter(|stored| stored.entry.principal.as_deref() == principal)
            .map(|stored| stored.entry.clone())
            .collect();
        found.sort_by(|a, b| {
            a.service
                .cmp(&b.service)
                .then_with(|| a.env_var.cmp(&b.env_var))
        });
        found
    }

    /// Upsert one credential: encrypt, persist, keep the warmed entry. A row's label is left as it is.
    pub fn set(
        &self,
        service: &str,
        env_var: &str,
        value: &str,
        principal: Option<&str>,
    ) -> Result<CredentialEntry, String> {
        self.set_labeled(service, env_var, value, principal, None)
    }

    /// [`Self::set`] that also stores `label`, a non-secret account name kept in the clear in the row beside the
    /// ciphertext. `None` leaves the row's label as it is; an empty label is refused.
    pub fn set_labeled(
        &self,
        service: &str,
        env_var: &str,
        value: &str,
        principal: Option<&str>,
        label: Option<&str>,
    ) -> Result<CredentialEntry, String> {
        if principal.is_some_and(str::is_empty) {
            return Err("credential principal must be a nonempty string".to_string());
        }
        if label.is_some_and(str::is_empty) {
            return Err("credential label must be a nonempty string".to_string());
        }
        self.ensure_warm()?;
        let sealed = self.seal(value, env_var, principal)?;
        let updated_at = orbital_core_base::time_util::now_millis();
        let key = storage_key(env_var, principal);
        let mut data = Map::new();
        data.insert("service".into(), json!(service));
        data.insert("envVar".into(), json!(env_var));
        data.insert("ciphertext".into(), json!(sealed.ciphertext));
        data.insert("iv".into(), json!(sealed.iv));
        data.insert("authTag".into(), json!(sealed.auth_tag));
        data.insert("keyId".into(), json!(sealed.key_id));
        data.insert("updatedAt".into(), json!(updated_at));
        if let Some(who) = principal {
            data.insert("principal".into(), json!(who));
        }
        if let Some(label) = label {
            data.insert("label".into(), json!(label));
        }
        let data = Value::Object(data);
        let mut state = self.state.lock();
        let (id, kept_label) = match state.by_identity.get(&key) {
            Some(existing) => {
                self.rows.update(&existing.id, &data)?;
                (existing.id.clone(), existing.label.clone())
            }
            None => (self.rows.create(&data)?, None),
        };
        let entry = CredentialEntry {
            service: service.to_string(),
            env_var: env_var.to_string(),
            last4: last4(value),
            updated_at,
            principal: principal.map(str::to_string),
        };
        state.by_identity.insert(
            key,
            Stored {
                id,
                entry: entry.clone(),
                value: value.to_string(),
                label: label.map(str::to_string).or(kept_label),
            },
        );
        Ok(entry)
    }

    /// Delete one credential row; whether one existed.
    pub fn remove(&self, env_var: &str, principal: Option<&str>) -> Result<bool, String> {
        self.ensure_warm()?;
        let key = storage_key(env_var, principal);
        let mut state = self.state.lock();
        let Some(existing) = state.by_identity.get(&key) else {
            return Ok(false);
        };
        self.rows.delete(&existing.id)?;
        state.by_identity.remove(&key);
        Ok(true)
    }

    /// Re-encrypt every row under the current master key. A row that cannot be read fails the rotation, naming the row ids,
    /// before any row is touched; `unreadable` in the counts is therefore always 0.
    pub fn rotate(&self) -> Result<RotateCounts, String> {
        let Some(master) = &self.master else {
            return Err(format!(
                "{CREDENTIAL_MASTER_KEY_ENV} is not configured — cannot rotate"
            ));
        };
        let current = master.id();
        let rows = self.rows.list()?;
        let (_, unreadable) = self.scan(&rows);
        if !unreadable.is_empty() {
            return Err(Self::unreadable_failure(&unreadable));
        }
        let (mut rotated, mut already_current) = (0u64, 0u64);
        for row in rows {
            if row.get("kind").is_some() {
                continue;
            }
            let (Some((id, service, env_var, principal)), Some(sealed)) =
                (Self::identity_of(&row), Self::sealed_of(&row))
            else {
                return Err("a credential row changed while it was rotated".to_string());
            };
            let row_key_id = text_of(&row, "keyId");
            let Some(value) = self.open(&sealed, row_key_id, env_var, principal) else {
                return Err("a credential row changed while it was rotated".to_string());
            };
            if row_key_id == Some(current.as_str()) {
                already_current += 1;
                continue;
            }
            if row_key_id.is_none()
                && self
                    .open(&sealed, Some(&current), env_var, principal)
                    .is_some()
            {
                already_current += 1;
                let mut stamped = row.clone();
                stamped["keyId"] = json!(current);
                self.rows.update(id, &stamped)?;
                continue;
            }
            let fresh = self.seal(&value, env_var, principal)?;
            let mut data = Map::new();
            data.insert("service".into(), json!(service));
            data.insert("envVar".into(), json!(env_var));
            data.insert("ciphertext".into(), json!(fresh.ciphertext));
            data.insert("iv".into(), json!(fresh.iv));
            data.insert("authTag".into(), json!(fresh.auth_tag));
            data.insert("keyId".into(), json!(fresh.key_id));
            data.insert(
                "updatedAt".into(),
                json!(orbital_core_base::time_util::now_millis()),
            );
            if let Some(who) = principal {
                data.insert("principal".into(), json!(who));
            }
            self.rows.update(id, &Value::Object(data))?;
            rotated += 1;
        }
        self.warm()?;
        Ok(RotateCounts {
            rotated,
            already_current,
            unreadable: 0,
        })
    }

    fn held_row(rows: &[Value], name: &str, principal: Option<&str>) -> Option<Value> {
        rows.iter()
            .find(|row| {
                row.get("kind").and_then(Value::as_str) == Some(DERIVED_KIND)
                    && row["envVar"].as_str() == Some(name)
                    && row.get("principal").and_then(Value::as_str) == principal
            })
            .cloned()
    }

    /// Keep `value` sealed in the rows as a one-use value of `name` (for `principal`) until `expires_at` (epoch ms),
    /// replacing one already held. Held values that expired by `now` are deleted on the way.
    pub fn hold(
        &self,
        name: &str,
        value: &str,
        expires_at: i64,
        now: i64,
        principal: Option<&str>,
    ) -> Result<(), String> {
        if principal.is_some_and(str::is_empty) {
            return Err("credential principal must be a nonempty string".to_string());
        }
        let sealed = self.seal(value, name, principal)?;
        let mut live = Vec::new();
        for row in self.rows.list()? {
            let expired = row.get("kind").and_then(Value::as_str) == Some(DERIVED_KIND)
                && row["expiresAt"]
                    .as_f64()
                    .is_some_and(|at| (at as i64) <= now);
            let id = text_of(&row, "id").map(str::to_string);
            match (expired, id) {
                (true, Some(id)) => {
                    self.rows.delete(&id)?;
                }
                _ => live.push(row),
            }
        }
        let mut data = Map::new();
        data.insert("kind".into(), json!(DERIVED_KIND));
        data.insert("envVar".into(), json!(name));
        data.insert("ciphertext".into(), json!(sealed.ciphertext));
        data.insert("iv".into(), json!(sealed.iv));
        data.insert("authTag".into(), json!(sealed.auth_tag));
        data.insert("keyId".into(), json!(sealed.key_id));
        data.insert("expiresAt".into(), json!(expires_at));
        if let Some(who) = principal {
            data.insert("principal".into(), json!(who));
        }
        let data = Value::Object(data);
        match Self::held_row(&live, name, principal)
            .as_ref()
            .and_then(|row| text_of(row, "id"))
        {
            Some(id) => self.rows.update(id, &data),
            None => self.rows.create(&data).map(|_| ()),
        }
    }

    /// Find the one-use value held for `name` (for `principal`) and answer its row id and a ticket, without opening it. The
    /// taker then deletes that row, and only the delete that removes it goes on to [`Self::take_finish`]; one that finds it
    /// gone calls [`Self::take_cancel`]. Without a master key nothing is held.
    pub fn take_begin(
        &self,
        name: &str,
        principal: Option<&str>,
    ) -> Result<Option<HeldTake>, String> {
        if !self.enabled() {
            return Ok(None);
        }
        let Some(row) = Self::held_row(&self.rows.list()?, name, principal) else {
            return Ok(None);
        };
        let id = text_of(&row, "id")
            .ok_or_else(|| format!("a held one-use credential row of {name} carries no id"))?
            .to_string();
        let mut state = self.state.lock();
        state.tickets += 1;
        let ticket = state.tickets;
        state.taking.insert(ticket, row);
        Ok(Some(HeldTake { ticket, id }))
    }

    /// Open the row [`Self::take_begin`] found, after the taker's delete removed it. A value that expired by `now` is
    /// consumed and not returned; a row that cannot be read is a failure.
    pub fn take_finish(&self, ticket: u64, now: i64) -> Result<Option<String>, String> {
        let row = self.state.lock().taking.remove(&ticket).ok_or_else(|| {
            format!("no held one-use credential row is being taken under ticket {ticket}")
        })?;
        let id = text_of(&row, "id").unwrap_or_default();
        let expires_at = row["expiresAt"]
            .as_f64()
            .ok_or_else(|| format!("held one-use credential row {id} carries no expiry"))?;
        if (expires_at as i64) <= now {
            return Ok(None);
        }
        let name = text_of(&row, "envVar")
            .ok_or_else(|| format!("held one-use credential row {id} carries no name"))?;
        let principal = row.get("principal").and_then(Value::as_str);
        let sealed = Self::sealed_of(&row)
            .ok_or_else(|| format!("held one-use credential row {id} is malformed"))?;
        self.open(&sealed, text_of(&row, "keyId"), name, principal)
            .map(Some)
            .ok_or_else(|| {
                format!("held one-use credential row {id} cannot be opened by the held keys")
            })
    }

    /// Forget a row [`Self::take_begin`] found whose delete another taker won.
    pub fn take_cancel(&self, ticket: u64) {
        self.state.lock().taking.remove(&ticket);
    }

    /// Remove and return the one-use value held for `name` (for `principal`): the single read of it, over this store's own
    /// rows. Of several takers racing for one row only the one whose delete removes it gets the value.
    pub fn take(
        &self,
        name: &str,
        now: i64,
        principal: Option<&str>,
    ) -> Result<Option<String>, String> {
        let Some(held) = self.take_begin(name, principal)? else {
            return Ok(None);
        };
        match self.rows.delete(&held.id) {
            Ok(true) => self.take_finish(held.ticket, now),
            Ok(false) => {
                self.take_cancel(held.ticket);
                Ok(None)
            }
            Err(error) => {
                self.take_cancel(held.ticket);
                Err(error)
            }
        }
    }

    /// The store's state for the kernel's `credentialStoreList` ask: whether it can encrypt and the service and name of every
    /// app-wide row, never a value.
    pub fn listing(&self) -> Result<StoreListing, String> {
        self.ensure_warm()?;
        Ok(StoreListing {
            enabled: self.enabled(),
            entries: self
                .entries(None)
                .into_iter()
                .map(|entry| StoredName {
                    service: entry.service,
                    name: entry.env_var,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/credential-store.json"
        );
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture"))
            .expect("fixture json")
    }

    struct Fixed(Vec<Value>);

    impl CredentialRows for Fixed {
        fn list(&self) -> Result<Vec<Value>, String> {
            Ok(self.0.clone())
        }
        fn create(&self, _: &Value) -> Result<String, String> {
            Err("read only".to_string())
        }
        fn update(&self, _: &str, _: &Value) -> Result<(), String> {
            Err("read only".to_string())
        }
        fn delete(&self, _: &str) -> Result<bool, String> {
            Err("read only".to_string())
        }
    }

    fn js_store(fx: &Value) -> EncryptedCredentialStore {
        let rows = fx["rows"].as_array().expect("rows").clone();
        EncryptedCredentialStore::new(
            Arc::new(Fixed(rows)),
            fx["masterKey"].as_str(),
            fx["previousKey"].as_str(),
        )
    }

    fn principal_of(case: &Value) -> Option<&str> {
        case["principal"].as_str()
    }

    #[test]
    fn rows_written_by_the_js_store_open_to_the_plaintexts_the_js_store_resolves() {
        let fx = fixture();
        let store = js_store(&fx);
        let expected = fx["resolves"].as_array().expect("resolves");
        assert_eq!(store.warm().expect("warm"), expected.len());
        for case in expected {
            let name = case["name"].as_str().expect("name");
            assert_eq!(
                store.resolve(name, principal_of(case)).as_deref(),
                case["value"].as_str(),
                "{name}"
            );
        }
        for case in expected {
            let name = case["name"].as_str().expect("name");
            assert_eq!(
                store.label(name, principal_of(case)).as_deref(),
                case["label"].as_str(),
                "label of {name}"
            );
        }
        assert_eq!(store.label("GITHUB_TOKEN", None), None);
        let app = store.entries(None);
        let stripe = app
            .iter()
            .find(|e| e.env_var == "STRIPE_KEY")
            .expect("stripe");
        assert_eq!(
            (stripe.service.as_str(), stripe.last4.as_str()),
            ("stripe", "1234")
        );
        let user = store.entries(Some("user-7"));
        assert_eq!(user.len(), 1);
        assert_eq!(user[0].last4, "9876");
    }

    #[test]
    fn a_label_is_kept_in_the_clear_beside_the_ciphertext_and_is_never_the_secret() {
        let fx = fixture();
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = EncryptedCredentialStore::new(rows.clone(), fx["masterKey"].as_str(), None);
        let secret = "1//refresh-token-SECRET-4242";
        store
            .set_labeled(
                "kflow",
                "KNOWLEDGE_REFRESH_TOKEN",
                secret,
                None,
                Some("ada@example.com"),
            )
            .expect("set");
        assert_eq!(
            store.label("KNOWLEDGE_REFRESH_TOKEN", None).as_deref(),
            Some("ada@example.com")
        );
        assert_eq!(
            store.resolve("KNOWLEDGE_REFRESH_TOKEN", None).as_deref(),
            Some(secret)
        );
        let row = rows.list().expect("rows").pop().expect("row");
        assert_eq!(row["label"], "ada@example.com");
        assert!(
            !row.to_string().contains(secret),
            "the stored row never carries the secret in the clear"
        );
        assert!(!row["ciphertext"]
            .as_str()
            .expect("ciphertext")
            .contains("ada@example.com"));
        let reopened = EncryptedCredentialStore::new(rows.clone(), fx["masterKey"].as_str(), None);
        reopened.warm().expect("warm");
        assert_eq!(
            reopened.label("KNOWLEDGE_REFRESH_TOKEN", None).as_deref(),
            Some("ada@example.com")
        );
        assert_ne!(
            reopened.label("KNOWLEDGE_REFRESH_TOKEN", None).as_deref(),
            Some(secret)
        );
    }

    #[test]
    fn control_a_plain_set_keeps_the_label_and_rotation_keeps_it_and_remove_drops_it() {
        let fx = fixture();
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = EncryptedCredentialStore::new(rows.clone(), fx["masterKey"].as_str(), None);
        store
            .set_labeled("kflow", "T", "first", Some("u1"), Some("ada@example.com"))
            .expect("set");
        store
            .set("kflow", "T", "second", Some("u1"))
            .expect("rotate the value");
        assert_eq!(store.resolve("T", Some("u1")).as_deref(), Some("second"));
        assert_eq!(
            store.label("T", Some("u1")).as_deref(),
            Some("ada@example.com")
        );
        store.rotate().expect("rotate keys");
        let again = EncryptedCredentialStore::new(rows.clone(), fx["masterKey"].as_str(), None);
        again.warm().expect("warm");
        assert_eq!(
            again.label("T", Some("u1")).as_deref(),
            Some("ada@example.com")
        );
        assert_eq!(
            again.label("T", None),
            None,
            "a user row has no app-wide label"
        );
        store
            .set_labeled("kflow", "T", "third", Some("u1"), Some("grace@example.com"))
            .expect("relabel");
        assert_eq!(
            store.label("T", Some("u1")).as_deref(),
            Some("grace@example.com")
        );
        assert!(store.remove("T", Some("u1")).expect("remove"));
        assert_eq!(store.label("T", Some("u1")), None);
    }

    #[test]
    fn control_a_row_without_a_label_reads_as_none_and_an_empty_label_is_refused() {
        let fx = fixture();
        let store = EncryptedCredentialStore::new(
            Arc::new(MemoryCredentialRows::default()),
            fx["masterKey"].as_str(),
            None,
        );
        store.set("kflow", "K", "v", None).expect("set");
        assert_eq!(store.label("K", None), None);
        assert!(store
            .set_labeled("kflow", "K", "v", None, Some(""))
            .is_err());
    }

    fn row_id(fx: &Value, env_var: &str) -> String {
        fx["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .chain(fx["unreadableRows"].as_array().expect("unreadable rows"))
            .find(|row| row["envVar"] == env_var)
            .expect("row")["id"]
            .as_str()
            .expect("id")
            .to_string()
    }

    fn rows_with_unreadable(fx: &Value) -> Vec<Value> {
        let mut rows = fx["rows"].as_array().expect("rows").clone();
        rows.extend(
            fx["unreadableRows"]
                .as_array()
                .expect("unreadable rows")
                .clone(),
        );
        rows
    }

    #[test]
    fn a_row_no_held_key_opens_fails_the_warm_naming_its_id_and_the_store_reads_nothing() {
        let fx = fixture();
        let store = EncryptedCredentialStore::new(
            Arc::new(Fixed(rows_with_unreadable(&fx))),
            fx["masterKey"].as_str(),
            fx["previousKey"].as_str(),
        );
        let failure = store.warm().expect_err("an unreadable row fails the warm");
        assert!(failure.contains(&row_id(&fx, "TWILIO_TOKEN")), "{failure}");
        assert!(!failure.contains(&row_id(&fx, "STRIPE_KEY")), "{failure}");
        assert!(!store.warmed());
        assert_eq!(
            store.resolve("STRIPE_KEY", None),
            None,
            "a failed warm leaves nothing to resolve"
        );
        assert!(
            store.set("stripe", "K", "v", None).is_err(),
            "a write waits on a good warm"
        );
    }

    #[test]
    fn a_missing_previous_key_fails_the_warm_naming_the_rows_it_hides() {
        let fx = fixture();
        let store = EncryptedCredentialStore::new(
            Arc::new(Fixed(fx["rows"].as_array().expect("rows").clone())),
            fx["masterKey"].as_str(),
            None,
        );
        let failure = store
            .warm()
            .expect_err("a row under the retired key is unreadable");
        assert!(failure.contains(&row_id(&fx, "GITHUB_TOKEN")), "{failure}");
        assert!(!failure.contains(&row_id(&fx, "STRIPE_KEY")), "{failure}");
    }

    #[test]
    fn control_malformed_and_unknown_kind_rows_fail_the_warm_and_a_derived_row_is_not_one() {
        let fx = fixture();
        let readable = fx["rows"].as_array().expect("rows").clone();
        let with = |change: &dyn Fn(&mut Value)| {
            let mut rows = readable.clone();
            change(&mut rows[0]);
            let id = rows[0]["id"].as_str().expect("id").to_string();
            let store = EncryptedCredentialStore::new(
                Arc::new(Fixed(rows)),
                fx["masterKey"].as_str(),
                fx["previousKey"].as_str(),
            );
            (store.warm(), id)
        };
        for change in [
            (&|row: &mut Value| row["iv"] = json!("")) as &dyn Fn(&mut Value),
            &|row: &mut Value| row["authTag"] = json!("zz"),
            &|row: &mut Value| row["ciphertext"] = json!(7),
            &|row: &mut Value| row["principal"] = json!(""),
            &|row: &mut Value| row["kind"] = json!("mystery"),
            &|row: &mut Value| row["id"] = json!(""),
        ] {
            let (warm, id) = with(change);
            let failure = warm.expect_err("a malformed row fails the warm");
            assert!(failure.contains("1 credential row(s)"), "{failure}");
            if !id.is_empty() {
                assert!(failure.contains(&id), "{failure}");
            }
        }
        let (derived, _) = with(&|row: &mut Value| row["kind"] = json!(DERIVED_KIND));
        assert_eq!(
            derived.expect("a derived row is a one-use value, not a credential"),
            readable.len() - 1
        );
    }

    #[test]
    fn control_a_user_row_is_bound_to_its_principal_and_stripped_of_it_it_fails_the_warm() {
        let fx = fixture();
        let mut rows = fx["rows"].as_array().expect("rows").clone();
        let mine = rows
            .iter()
            .position(|r| r["envVar"] == "OPENAI_KEY")
            .expect("row");
        rows[mine].as_object_mut().expect("row").remove("principal");
        let store = EncryptedCredentialStore::new(
            Arc::new(Fixed(rows)),
            fx["masterKey"].as_str(),
            fx["previousKey"].as_str(),
        );
        let failure = store
            .warm()
            .expect_err("the row no longer opens as the app credential");
        assert!(failure.contains(&row_id(&fx, "OPENAI_KEY")), "{failure}");
    }

    #[test]
    fn fixed_iv_seals_are_byte_for_byte_the_ones_node_crypto_makes() {
        let fx = fixture();
        for case in fx["seals"].as_array().expect("seals") {
            let iv: [u8; 12] = unhex(case["ivHex"].as_str().expect("iv"))
                .expect("hex")
                .try_into()
                .expect("12 bytes");
            let store = EncryptedCredentialStore::with_iv(
                Arc::new(MemoryCredentialRows::default()),
                fx["masterKey"].as_str(),
                None,
                Box::new(move || iv),
            );
            let sealed = store
                .seal(
                    case["plaintext"].as_str().expect("plain"),
                    case["name"].as_str().expect("name"),
                    principal_of(case),
                )
                .expect("seal");
            assert_eq!(
                sealed.ciphertext,
                case["ciphertext"].as_str().expect("ct"),
                "{}",
                case["name"]
            );
            assert_eq!(sealed.auth_tag, case["authTag"].as_str().expect("tag"));
            assert_eq!(sealed.key_id, case["keyId"].as_str().expect("key id"));
        }
    }

    fn temp_file(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("credential-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("creds.json")
    }

    #[test]
    fn the_file_store_round_trips_and_keeps_the_js_row_shape() {
        let fx = fixture();
        let path = temp_file("roundtrip");
        let store = EncryptedCredentialStore::new(
            Arc::new(FileCredentialRows::new(&path)),
            fx["masterKey"].as_str(),
            None,
        );
        let entry = store
            .set("stripe", "STRIPE_KEY", "fake-stripe-4321", None)
            .expect("set");
        assert_eq!(entry.last4, "4321");
        store
            .set("openai", "OPENAI_KEY", "per-user-0001", Some("user-9"))
            .expect("set user");
        let again = EncryptedCredentialStore::new(
            Arc::new(FileCredentialRows::new(&path)),
            fx["masterKey"].as_str(),
            None,
        );
        assert_eq!(again.warm().expect("warm"), 2);
        assert_eq!(
            again.resolve("STRIPE_KEY", None).as_deref(),
            Some("fake-stripe-4321")
        );
        assert_eq!(
            again.resolve("OPENAI_KEY", Some("user-9")).as_deref(),
            Some("per-user-0001")
        );
        let text = std::fs::read_to_string(&path).expect("file");
        let rows: Vec<Value> = serde_json::from_str(&text).expect("rows");
        let row = rows
            .iter()
            .find(|r| r["envVar"] == "STRIPE_KEY")
            .expect("row");
        for field in [
            "id",
            "service",
            "envVar",
            "ciphertext",
            "iv",
            "authTag",
            "keyId",
            "updatedAt",
        ] {
            assert!(!row[field].is_null(), "{field}");
        }
        assert!(row.get("principal").is_none());
        assert!(!text.contains("fake-stripe-4321"));
        assert!(again.remove("STRIPE_KEY", None).expect("remove"));
        assert!(!again.remove("STRIPE_KEY", None).expect("remove twice"));
    }

    #[test]
    fn rotation_re_encrypts_rows_under_the_previous_key_and_counts_the_rest() {
        let fx = fixture();
        let rows = Arc::new(MemoryCredentialRows::default());
        rows.load(fx["rows"].as_array().expect("rows").clone());
        let store = EncryptedCredentialStore::new(
            rows.clone(),
            fx["masterKey"].as_str(),
            fx["previousKey"].as_str(),
        );
        let counts = store.rotate().expect("rotate");
        assert_eq!(
            counts,
            RotateCounts {
                rotated: 1,
                already_current: 3,
                unreadable: 0
            }
        );
        let retired = EncryptedCredentialStore::new(rows, fx["masterKey"].as_str(), None);
        retired.warm().expect("warm");
        assert_eq!(
            retired.resolve("GITHUB_TOKEN", None).as_deref(),
            Some("fake-github-5555")
        );
    }

    #[test]
    fn control_a_rotation_over_an_unreadable_row_fails_naming_it_and_touches_no_row() {
        let fx = fixture();
        let rows = Arc::new(MemoryCredentialRows::default());
        rows.load(rows_with_unreadable(&fx));
        let before = rows.list().expect("rows");
        let store = EncryptedCredentialStore::new(
            rows.clone(),
            fx["masterKey"].as_str(),
            fx["previousKey"].as_str(),
        );
        let failure = store
            .rotate()
            .expect_err("an unreadable row fails the rotation");
        assert!(failure.contains(&row_id(&fx, "TWILIO_TOKEN")), "{failure}");
        assert_eq!(
            rows.list().expect("rows"),
            before,
            "no row was re-encrypted before the failure"
        );
    }

    #[test]
    fn without_a_master_key_nothing_is_stored_or_read() {
        let fx = fixture();
        let store = EncryptedCredentialStore::new(
            Arc::new(MemoryCredentialRows::default()),
            Some("not-hex"),
            None,
        );
        assert!(!store.enabled());
        assert_eq!(store.warm().expect("warm"), 0);
        assert!(store.set("stripe", "K", "v", None).is_err());
        assert!(store.rotate().is_err());
        let listing = store.listing().expect("listing");
        assert!(!listing.enabled && listing.entries.is_empty());
        assert!(js_store(&fx).enabled());
    }

    #[test]
    fn rows_kept_in_the_apps_store_survive_a_new_store_and_resolve_the_same_value() {
        let fx = fixture();
        let persistence = Arc::new(crate::effects::executor::InMemoryPersistence::new());
        let rows = || Arc::new(StoreCredentialRows(persistence.clone())) as Arc<dyn CredentialRows>;
        let store = EncryptedCredentialStore::new(rows(), fx["masterKey"].as_str(), None);
        store
            .set("stripe", "STRIPE_KEY", "sk_live_1234", None)
            .expect("set");
        let reopened = EncryptedCredentialStore::new(rows(), fx["masterKey"].as_str(), None);
        reopened.warm().expect("warm");
        assert_eq!(
            reopened.resolve("STRIPE_KEY", None).as_deref(),
            Some("sk_live_1234")
        );
        let kept = crate::effects::executor::PersistenceHandler::list(
            persistence.as_ref(),
            CREDENTIAL_ENTITY_TYPE,
        )
        .unwrap();
        let row = serde_json::to_value(&kept[0]).unwrap();
        assert!(
            row["ciphertext"].is_string() && row.get("value").is_none(),
            "the store keeps ciphertext, never the value: {row}"
        );
        assert!(reopened.remove("STRIPE_KEY", None).expect("remove"));
        assert!(crate::effects::executor::PersistenceHandler::list(
            persistence.as_ref(),
            CREDENTIAL_ENTITY_TYPE
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn control_an_update_of_a_missing_row_leaves_it_missing() {
        let persistence = Arc::new(crate::effects::executor::InMemoryPersistence::new());
        let rows = StoreCredentialRows(persistence.clone());
        rows.update("absent", &json!({ "envVar": "X" }))
            .expect("a missing row is not an error");
        assert!(rows.list().unwrap().is_empty());
    }

    fn store_over(rows: Arc<dyn CredentialRows>) -> EncryptedCredentialStore {
        EncryptedCredentialStore::new(rows, fixture()["masterKey"].as_str(), None)
    }

    #[test]
    fn a_held_one_use_value_is_sealed_in_the_rows_survives_a_new_store_and_is_taken_once() {
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = store_over(rows.clone());
        store
            .hold("oauth:grant:S1", "GRANT-SECRET", 2_000, 1_000, None)
            .expect("hold");
        let kept = rows.list().expect("rows");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["kind"], DERIVED_KIND);
        assert!(
            !kept[0].to_string().contains("GRANT-SECRET"),
            "the row never carries the value in the clear"
        );
        let restarted = store_over(rows.clone());
        assert_eq!(
            restarted.warm().expect("a derived row is not a credential"),
            0
        );
        assert_eq!(
            restarted
                .take("oauth:grant:S1", 1_500, None)
                .expect("take")
                .as_deref(),
            Some("GRANT-SECRET")
        );
        assert!(
            rows.list().expect("rows").is_empty(),
            "a take consumes the row"
        );
        assert_eq!(
            restarted
                .take("oauth:grant:S1", 1_500, None)
                .expect("take again"),
            None
        );
    }

    #[test]
    fn control_an_expired_value_is_consumed_and_not_returned_and_a_held_value_is_scoped_to_its_principal(
    ) {
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = store_over(rows.clone());
        store
            .hold("grant", "late", 2_000, 1_000, None)
            .expect("hold");
        assert_eq!(store.take("grant", 2_000, None).expect("take"), None);
        assert!(
            rows.list().expect("rows").is_empty(),
            "an expired value is gone"
        );

        store
            .hold("grant", "alice-value", 9_000, 1_000, Some("alice"))
            .expect("hold");
        assert_eq!(store.take("grant", 2_000, Some("bob")).expect("take"), None);
        assert_eq!(store.take("grant", 2_000, None).expect("take"), None);
        assert_eq!(
            rows.list().expect("rows").len(),
            1,
            "another scope does not consume it"
        );
        assert_eq!(
            store
                .take("grant", 2_000, Some("alice"))
                .expect("take")
                .as_deref(),
            Some("alice-value")
        );
    }

    #[test]
    fn holding_again_replaces_the_value_and_sweeps_the_expired_ones() {
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = store_over(rows.clone());
        store.hold("old", "x", 1_500, 1_000, None).expect("hold");
        store
            .hold("grant", "first", 9_000, 1_000, None)
            .expect("hold");
        store
            .hold("grant", "second", 9_000, 2_000, None)
            .expect("hold again");
        let kept = rows.list().expect("rows");
        assert_eq!(
            kept.len(),
            1,
            "the expired row was swept and the replaced one is a single row"
        );
        assert_eq!(
            store.take("grant", 3_000, None).expect("take").as_deref(),
            Some("second")
        );
    }

    #[test]
    fn control_a_held_row_that_cannot_be_opened_fails_the_take_and_is_consumed() {
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = store_over(rows.clone());
        store.hold("grant", "v", 9_000, 1_000, None).expect("hold");
        let id = rows.list().expect("rows")[0]["id"]
            .as_str()
            .expect("id")
            .to_string();
        rows.update(&id, &json!({ "authTag": "00".repeat(16) }))
            .expect("tamper");
        let failure = store
            .take("grant", 2_000, None)
            .expect_err("a tampered row is a failure, not an absence");
        assert!(failure.contains(&id), "{failure}");
        assert!(rows.list().expect("rows").is_empty());
    }

    #[test]
    fn control_without_a_master_key_nothing_is_held_and_nothing_is_taken() {
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = EncryptedCredentialStore::new(rows.clone(), None, None);
        assert!(store.hold("grant", "v", 9_000, 1_000, None).is_err());
        assert_eq!(store.take("grant", 2_000, None).expect("take"), None);
        assert!(store.hold("grant", "v", 9_000, 1_000, Some("")).is_err());
    }

    #[test]
    fn recorded_rows_hand_a_host_the_writes_to_apply_and_a_second_store_over_them_reads_the_same_credentials(
    ) {
        let recorded = Arc::new(RecordedCredentialRows::default());
        let store = store_over(recorded.clone());
        store.warm().expect("warm");
        store
            .set("stripe", "STRIPE_KEY", "sk-1111", None)
            .expect("set");
        store
            .set("stripe", "STRIPE_KEY", "sk-2222", None)
            .expect("rotate the value");
        store
            .set("openai", "OPENAI_KEY", "per-user-0001", Some("u1"))
            .expect("set user");
        assert!(store.remove("OPENAI_KEY", Some("u1")).expect("remove"));
        let writes = recorded.take_writes();
        let kinds: Vec<&str> = writes
            .iter()
            .map(|w| w["write"].as_str().expect("write"))
            .collect();
        assert_eq!(kinds, ["create", "update", "create", "delete"]);
        let medium = Arc::new(MemoryCredentialRows::default());
        for write in &writes {
            let id = write["id"].as_str().expect("id");
            match write["write"].as_str().expect("write") {
                "create" => {
                    medium.load({
                        let mut held = medium.list().expect("rows");
                        held.push(write["data"].clone());
                        held
                    });
                }
                "update" => medium.update(id, &write["data"]).expect("update"),
                _ => {
                    medium.delete(id).expect("delete");
                }
            }
        }
        let reopened = store_over(medium);
        assert_eq!(reopened.warm().expect("warm"), 1);
        assert_eq!(
            reopened.resolve("STRIPE_KEY", None).as_deref(),
            Some("sk-2222")
        );
        assert!(
            recorded.take_writes().is_empty(),
            "taking the writes empties the record"
        );
    }

    #[test]
    fn control_loading_rows_drops_writes_the_host_never_applied_and_an_update_of_a_missing_row_records_nothing(
    ) {
        let recorded = Arc::new(RecordedCredentialRows::default());
        recorded.create(&json!({ "envVar": "A" })).expect("create");
        recorded.load(Vec::new());
        assert!(recorded.take_writes().is_empty());
        recorded
            .update("absent", &json!({ "envVar": "B" }))
            .expect("update");
        assert!(!recorded.delete("absent").expect("delete"));
        assert!(recorded.take_writes().is_empty());
    }

    #[test]
    fn two_takers_racing_for_one_held_row_get_exactly_one_value_and_a_lone_taker_gets_it() {
        for _ in 0..100 {
            let rows = Arc::new(MemoryCredentialRows::default());
            let store = Arc::new(store_over(rows.clone()));
            store.hold("grant", "v", 9_000, 1_000, None).expect("hold");
            let taken: Vec<Option<String>> = std::thread::scope(|scope| {
                let takers: Vec<_> = (0..2)
                    .map(|_| {
                        let store = store.clone();
                        scope.spawn(move || store.take("grant", 2_000, None).expect("take"))
                    })
                    .collect();
                takers
                    .into_iter()
                    .map(|taker| taker.join().expect("taker"))
                    .collect()
            });
            assert_eq!(
                taken.iter().filter(|value| value.is_some()).count(),
                1,
                "{taken:?}"
            );
            assert!(taken.contains(&Some("v".to_string())));
            assert!(rows.list().expect("rows").is_empty());
        }
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = store_over(rows);
        store.hold("grant", "v", 9_000, 1_000, None).expect("hold");
        assert_eq!(
            store.take("grant", 2_000, None).expect("take").as_deref(),
            Some("v")
        );
    }

    #[test]
    fn a_taker_whose_delete_lost_gets_nothing_and_each_ticket_is_its_own() {
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = store_over(rows.clone());
        store.hold("grant", "v", 9_000, 1_000, None).expect("hold");
        let first = store
            .take_begin("grant", None)
            .expect("begin")
            .expect("held");
        let second = store
            .take_begin("grant", None)
            .expect("begin")
            .expect("held");
        assert_eq!(first.id, second.id);
        assert_ne!(first.ticket, second.ticket);
        assert_eq!(
            rows.list().expect("rows").len(),
            1,
            "begin deletes nothing and opens nothing"
        );
        assert!(rows.delete(&first.id).expect("delete"));
        assert!(!rows.delete(&second.id).expect("delete"));
        store.take_cancel(second.ticket);
        assert_eq!(
            store
                .take_finish(first.ticket, 2_000)
                .expect("finish")
                .as_deref(),
            Some("v")
        );
        assert!(
            store.take_finish(second.ticket, 2_000).is_err(),
            "a cancelled ticket opens nothing"
        );
        assert_eq!(store.take_begin("grant", None).expect("begin"), None);
    }
}
