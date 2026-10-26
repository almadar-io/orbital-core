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
mod tests;
