//! Purpose-built embedding store: one sorted, int8-quantized file per scope (package snapshot or
//! user store). Replaces the JSON embedding manifests. No network, no mmap, no extra dependency:
//! the file is read into memory once and looked up by binary search over the buffer.
//!
//! Layout (all integers little-endian):
//! `"ORBVEC"` | version u16 | model_len u16 | model utf8 | dims u16 | count u32 |
//! count × record, sorted ascending by key, where record = key[32] | scale f32 | int8 × dims.
//!
//! Quantization is the one in `quantizeInt8` (almadar-calibrate): scale = max|v| (1 when all zero,
//! rounded to 6 significant digits), byte = round_half_up(v / scale × 127), decode = byte × scale ÷ 127.
//!
//! Concurrent writers: `insert_many` takes an exclusive lock file (`<path>.lock`, `create_new`) around
//! its re-read + merge + atomic rename, retrying with backoff until `LockOptions::timeout`
//! (`ORB_VECTOR_LOCKED`). Writers therefore serialise and never drop each other's records. A lock whose
//! owning pid is gone, or older than `stale_after`, is broken by renaming it aside; two breakers racing
//! on the same dead lock can briefly both proceed, in which case the re-read still keeps everything
//! already on disk and only a record landed inside that one window can be dropped. `write` alone (no
//! lock) is last-rename-wins.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAGIC: &[u8; 6] = b"ORBVEC";
pub const FORMAT_VERSION: u16 = 1;
pub const KEY_LEN: usize = 32;
pub type Key = [u8; KEY_LEN];

#[derive(Debug, Error)]
pub enum VectorStoreError {
    #[error("ORB_VECTOR_IO: {path}: {reason}")]
    Io { path: String, reason: String },
    #[error("ORB_VECTOR_BAD_MAGIC: {path} is not an ORBVEC file")]
    BadMagic { path: String },
    #[error("ORB_VECTOR_BAD_VERSION: {path} has format version {found}, this build reads {FORMAT_VERSION}")]
    BadVersion { path: String, found: u16 },
    #[error("ORB_VECTOR_CORRUPT: {path}: {detail}")]
    Corrupt { path: String, detail: String },
    #[error("ORB_VECTOR_MODEL_MISMATCH: {left} vs {right} — vectors from different embedding models are not comparable")]
    ModelMismatch { left: String, right: String },
    #[error("ORB_VECTOR_DIMS_MISMATCH: model {model} has {left} dims in one store and {right} in the other")]
    DimsMismatch {
        model: String,
        left: u16,
        right: u16,
    },
    #[error("ORB_VECTOR_BAD_RECORD: vector has {found} components, store dims is {expected}")]
    BadRecord { expected: u16, found: usize },
    #[error("ORB_VECTOR_LOCKED: {path} stayed locked for {waited_ms} ms (another session is writing; if none is, delete the lock file)")]
    Locked { path: String, waited_ms: u64 },
    #[error("ORB_VECTOR_BAD_MODEL: model name must be non-empty and at most 65535 bytes, dims must be non-zero")]
    BadModel,
}

/// An owned quantized vector. `bytes` are two's-complement int8 components.
#[derive(Debug, Clone, PartialEq)]
pub struct Quantized {
    pub scale: f32,
    pub bytes: Vec<u8>,
}

/// A borrowed quantized vector inside a store buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuantizedRef<'a> {
    pub scale: f32,
    pub bytes: &'a [u8],
}

impl QuantizedRef<'_> {
    pub fn decode(&self) -> Vec<f32> {
        decode_parts(self.scale, self.bytes)
    }
    pub fn to_owned(&self) -> Quantized {
        Quantized {
            scale: self.scale,
            bytes: self.bytes.to_vec(),
        }
    }
}

impl Quantized {
    pub fn decode(&self) -> Vec<f32> {
        decode_parts(self.scale, &self.bytes)
    }
}

/// sha256(model ‖ 0x00 ‖ text).
pub fn text_key(model: &str, text: &str) -> Key {
    let mut h = Sha256::new();
    h.update(model.as_bytes());
    h.update([0u8]);
    h.update(text.as_bytes());
    h.finalize().into()
}

/// Round to 6 significant digits, like JS `Number(x.toPrecision(6))`.
fn round_sig6(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    format!("{:.5e}", x).parse::<f64>().unwrap_or(x)
}

/// `quantizeInt8`: JS `Math.round` rounds half toward +infinity, hence `floor(x + 0.5)`.
pub fn quantize(vector: &[f32]) -> Quantized {
    let mut max = 0f64;
    for v in vector {
        let a = (*v as f64).abs();
        if a > max {
            max = a;
        }
    }
    let scale = if max == 0.0 { 1.0 } else { max };
    let bytes = vector
        .iter()
        .map(|v| {
            let q = (((*v as f64) / scale) * 127.0 + 0.5).floor();
            (q.clamp(-128.0, 127.0) as i8) as u8
        })
        .collect();
    Quantized {
        scale: round_sig6(scale) as f32,
        bytes,
    }
}

fn decode_parts(scale: f32, bytes: &[u8]) -> Vec<f32> {
    bytes
        .iter()
        .map(|b| ((*b as i8) as f32 * scale) / 127.0)
        .collect()
}

/// Cosine of the decoded vectors. Scales are positive, so they cancel: this works on the raw int8s.
pub fn cosine_i8(a: &[u8], b: &[u8]) -> f64 {
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b.iter()) {
        let (x, y) = ((*x as i8) as f64, (*y as i8) as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[derive(Debug, Clone)]
pub struct VectorStore {
    model: String,
    dims: u16,
    buf: Vec<u8>,
    data_off: usize,
    count: usize,
}

fn header_len(model: &str) -> usize {
    MAGIC.len() + 2 + 2 + model.len() + 2 + 4
}

fn io_err(path: &Path, e: &io::Error) -> VectorStoreError {
    VectorStoreError::Io {
        path: path.display().to_string(),
        reason: e.to_string(),
    }
}

impl VectorStore {
    /// A store with no records and no model (what a missing file opens as).
    pub fn empty() -> Self {
        VectorStore {
            model: String::new(),
            dims: 0,
            buf: Vec::new(),
            data_off: 0,
            count: 0,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn dims(&self) -> u16 {
        self.dims
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    fn rec_len(&self) -> usize {
        KEY_LEN + 4 + self.dims as usize
    }

    /// Missing file = empty store. A short, malformed or unsorted file is an error.
    pub fn open(path: &Path) -> Result<Self, VectorStoreError> {
        match fs::read(path) {
            Ok(bytes) => Self::from_bytes(bytes, &path.display().to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(e) => Err(io_err(path, &e)),
        }
    }

    pub fn from_bytes(buf: Vec<u8>, origin: &str) -> Result<Self, VectorStoreError> {
        let corrupt = |detail: &str| VectorStoreError::Corrupt {
            path: origin.to_string(),
            detail: detail.to_string(),
        };
        if buf.len() < MAGIC.len() || &buf[..MAGIC.len()] != MAGIC {
            return Err(VectorStoreError::BadMagic {
                path: origin.to_string(),
            });
        }
        let mut o = MAGIC.len();
        let u16_at = |o: usize| -> Option<u16> {
            buf.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
        };
        let version = u16_at(o).ok_or_else(|| corrupt("truncated header"))?;
        if version != FORMAT_VERSION {
            return Err(VectorStoreError::BadVersion {
                path: origin.to_string(),
                found: version,
            });
        }
        o += 2;
        let mlen = u16_at(o).ok_or_else(|| corrupt("truncated header"))? as usize;
        o += 2;
        let model = buf
            .get(o..o + mlen)
            .and_then(|s| std::str::from_utf8(s).ok())
            .ok_or_else(|| corrupt("truncated or non-utf8 model name"))?
            .to_string();
        o += mlen;
        let dims = u16_at(o).ok_or_else(|| corrupt("truncated header"))?;
        o += 2;
        let count = buf
            .get(o..o + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
            .ok_or_else(|| corrupt("truncated header"))?;
        o += 4;
        if model.is_empty() || dims == 0 {
            return Err(corrupt("empty model name or zero dims"));
        }
        let rec = KEY_LEN + 4 + dims as usize;
        let expected = count
            .checked_mul(rec)
            .and_then(|n| n.checked_add(o))
            .ok_or_else(|| corrupt("record count overflow"))?;
        if buf.len() != expected {
            return Err(corrupt(&format!(
                "expected {expected} bytes for {count} records, found {}",
                buf.len()
            )));
        }
        let store = VectorStore {
            model,
            dims,
            buf,
            data_off: o,
            count,
        };
        for i in 1..count {
            if store.key_at(i - 1) >= store.key_at(i) {
                return Err(corrupt("records are not strictly sorted by key"));
            }
        }
        Ok(store)
    }

    fn key_at(&self, i: usize) -> &[u8] {
        let s = self.data_off + i * self.rec_len();
        &self.buf[s..s + KEY_LEN]
    }

    fn record_at(&self, i: usize) -> QuantizedRef<'_> {
        let s = self.data_off + i * self.rec_len() + KEY_LEN;
        let scale = f32::from_le_bytes([
            self.buf[s],
            self.buf[s + 1],
            self.buf[s + 2],
            self.buf[s + 3],
        ]);
        QuantizedRef {
            scale,
            bytes: &self.buf[s + 4..s + 4 + self.dims as usize],
        }
    }

    pub fn get(&self, key: &Key) -> Option<QuantizedRef<'_>> {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.key_at(mid).cmp(&key[..]) {
                std::cmp::Ordering::Equal => return Some(self.record_at(mid)),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// All records in key order.
    pub fn iter(&self) -> impl Iterator<Item = (Key, QuantizedRef<'_>)> + '_ {
        (0..self.count).map(move |i| {
            let mut k = [0u8; KEY_LEN];
            k.copy_from_slice(self.key_at(i));
            (k, self.record_at(i))
        })
    }

    /// Build a store from records; later duplicates of a key win.
    pub fn from_records(
        model: &str,
        dims: u16,
        records: Vec<(Key, Quantized)>,
    ) -> Result<Self, VectorStoreError> {
        if model.is_empty() || model.len() > u16::MAX as usize || dims == 0 {
            return Err(VectorStoreError::BadModel);
        }
        for (_, q) in &records {
            if q.bytes.len() != dims as usize {
                return Err(VectorStoreError::BadRecord {
                    expected: dims,
                    found: q.bytes.len(),
                });
            }
        }
        let mut indexed: Vec<(usize, Key, Quantized)> = records
            .into_iter()
            .enumerate()
            .map(|(i, (k, q))| (i, k, q))
            .collect();
        indexed.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        let mut dedup: Vec<(Key, Quantized)> = Vec::with_capacity(indexed.len());
        for (_, k, q) in indexed {
            match dedup.last_mut() {
                Some(last) if last.0 == k => last.1 = q,
                _ => dedup.push((k, q)),
            }
        }
        let data_off = header_len(model);
        let rec = KEY_LEN + 4 + dims as usize;
        let mut buf = Vec::with_capacity(data_off + dedup.len() * rec);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&(model.len() as u16).to_le_bytes());
        buf.extend_from_slice(model.as_bytes());
        buf.extend_from_slice(&dims.to_le_bytes());
        buf.extend_from_slice(&(dedup.len() as u32).to_le_bytes());
        for (k, q) in &dedup {
            buf.extend_from_slice(k);
            buf.extend_from_slice(&q.scale.to_le_bytes());
            buf.extend_from_slice(&q.bytes);
        }
        Ok(VectorStore {
            model: model.to_string(),
            dims,
            buf,
            data_off,
            count: dedup.len(),
        })
    }

    /// The records for `keys` that exist in this store, as a new store (for a package snapshot).
    pub fn subset(&self, keys: &[Key]) -> Result<VectorStore, VectorStoreError> {
        let records = keys
            .iter()
            .filter_map(|k| self.get(k).map(|r| (*k, r.to_owned())))
            .collect();
        if self.model.is_empty() {
            return Ok(Self::empty());
        }
        Self::from_records(&self.model, self.dims, records)
    }

    /// Write via temp file + atomic rename.
    pub fn write(&self, path: &Path) -> Result<(), VectorStoreError> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(|e| io_err(dir, &e))?;
        }
        let mut tmp: PathBuf = path.to_path_buf();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        tmp.set_file_name(format!(
            ".{name}.tmp.{}.{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&tmp, &self.buf).map_err(|e| io_err(&tmp, &e))?;
        fs::rename(&tmp, path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            io_err(path, &e)
        })
    }

    /// Take the exclusive writer lock for the store at `path`; released when the guard drops.
    pub fn lock(path: &Path, opts: &LockOptions) -> Result<StoreLock, VectorStoreError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(|e| io_err(dir, &e))?;
        }
        let lock = lock_path(path);
        let started = std::time::Instant::now();
        let mut wait = opts.poll;
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(mut f) => {
                    use io::Write;
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    f.write_all(format!("{} {now}", std::process::id()).as_bytes())
                        .map_err(|e| io_err(&lock, &e))?;
                    return Ok(StoreLock { path: lock });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&lock, opts.stale_after) {
                        break_lock(&lock);
                        continue;
                    }
                }
                Err(e) => return Err(io_err(&lock, &e)),
            }
            if started.elapsed() >= opts.timeout {
                return Err(VectorStoreError::Locked {
                    path: lock.display().to_string(),
                    waited_ms: started.elapsed().as_millis() as u64,
                });
            }
            std::thread::sleep(wait);
            wait = (wait * 2).min(Duration::from_millis(100));
        }
    }

    /// [`Self::insert_many_with`] under the default lock options.
    pub fn insert_many(
        path: &Path,
        model: &str,
        dims: u16,
        records: Vec<(Key, Quantized)>,
    ) -> Result<(), VectorStoreError> {
        Self::insert_many_with(path, model, dims, records, &LockOptions::default())
    }

    /// Merge `records` into the store at `path` (created if missing) under the writer lock. Refuses a
    /// model or dims mismatch with what is on disk. Existing keys are overwritten; all other records
    /// are kept.
    pub fn insert_many_with(
        path: &Path,
        model: &str,
        dims: u16,
        records: Vec<(Key, Quantized)>,
        opts: &LockOptions,
    ) -> Result<(), VectorStoreError> {
        let incoming = Self::from_records(model, dims, records)?;
        let _guard = Self::lock(path, opts)?;
        // Re-read under the lock so records other writers landed meanwhile survive.
        let existing = Self::open(path)?;
        let merged = if existing.model.is_empty() {
            incoming
        } else {
            check_compatible(&existing, &incoming)?;
            let mut all: Vec<(Key, Quantized)> =
                existing.iter().map(|(k, r)| (k, r.to_owned())).collect();
            all.extend(incoming.iter().map(|(k, r)| (k, r.to_owned())));
            Self::from_records(model, dims, all)?
        };
        merged.write(path)
    }
}

/// How a writer waits for the lock and when it treats one as abandoned.
#[derive(Debug, Clone)]
pub struct LockOptions {
    pub stale_after: Duration,
    pub timeout: Duration,
    pub poll: Duration,
}

impl Default for LockOptions {
    fn default() -> Self {
        LockOptions {
            stale_after: Duration::from_secs(120),
            timeout: Duration::from_secs(180),
            poll: Duration::from_millis(5),
        }
    }
}

/// `<path>.lock`
pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".lock");
    path.with_file_name(name)
}

/// The held lock; the file is removed on drop.
#[derive(Debug)]
pub struct StoreLock {
    path: PathBuf,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_is_stale(lock: &Path, stale_after: Duration) -> bool {
    let Ok(meta) = fs::metadata(lock) else {
        // Vanished between create_new and here: not stale, just retry.
        return false;
    };
    let aged = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age >= stale_after);
    if aged {
        return true;
    }
    let pid = fs::read_to_string(lock).ok().and_then(|c| {
        c.split_whitespace()
            .next()
            .and_then(|p| p.parse::<u32>().ok())
    });
    pid.is_some_and(|p| !pid_alive(p))
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

fn break_lock(lock: &Path) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut aside = lock.to_path_buf();
    let name = lock
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    aside.set_file_name(format!(
        ".{name}.stale.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if fs::rename(lock, &aside).is_ok() {
        let _ = fs::remove_file(&aside);
    }
}

fn check_compatible(a: &VectorStore, b: &VectorStore) -> Result<(), VectorStoreError> {
    if a.model != b.model {
        return Err(VectorStoreError::ModelMismatch {
            left: a.model.clone(),
            right: b.model.clone(),
        });
    }
    if a.dims != b.dims {
        return Err(VectorStoreError::DimsMismatch {
            model: a.model.clone(),
            left: a.dims,
            right: b.dims,
        });
    }
    Ok(())
}

/// Ordered lookup across stores: first hit wins (package snapshots first, user store last).
#[derive(Debug, Clone)]
pub struct Layered {
    stores: Vec<VectorStore>,
}

impl Layered {
    /// Empty (model-less) stores are skipped; every other store must share model and dims.
    pub fn new(stores: Vec<VectorStore>) -> Result<Self, VectorStoreError> {
        let stores: Vec<VectorStore> = stores.into_iter().filter(|s| !s.model.is_empty()).collect();
        for s in stores.iter().skip(1) {
            check_compatible(&stores[0], s)?;
        }
        Ok(Layered { stores })
    }

    pub fn model(&self) -> Option<&str> {
        self.stores.first().map(|s| s.model())
    }
    pub fn dims(&self) -> Option<u16> {
        self.stores.first().map(|s| s.dims())
    }

    pub fn get(&self, key: &Key) -> Option<QuantizedRef<'_>> {
        self.stores.iter().find_map(|s| s.get(key))
    }
}
