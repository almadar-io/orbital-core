//! The native answerer of the `kv/` asks (`kv/get|put|delete|scan|batch`), reached through the os answerer's family
//! routing. The host owns the namespace `kv:<app>:<locale>:<store>`; the kernel names only the store.
//!
//! ON-DISK LAYOUT, identical to `renderers/node/src/kv-node.ts` (a state directory is portable between the two):
//! `<stateDir>/kv/<app>/<locale>/<store>.json` is ONE JSON object, keys sorted by UTF-8 byte order, no whitespace,
//! JSON escaping the only escaping. Every write serializes the whole store to `<store>.json.tmp-<random>`, fsyncs it
//! and renames it over `<store>.json`, so `kv/batch` is one atomic commit. Without a state directory the store is a
//! memory map.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::os_host::{KvMode, OsHost, OsRequirement};
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::str_utils::js_number_string;
use orbital_core_base::value::{Value, ValueMap};
use orbital_core_base::sync::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One store's content; `String`'s order is UTF-8 byte order.
pub type KvTable = BTreeMap<String, Value>;

enum KvMedium {
    Memory(HashMap<String, KvTable>),
    Directory(PathBuf),
}

/// Where one program instance keeps its kv stores, and the app and locale its namespaces are under.
pub struct KvStores {
    namespace: Option<(String, String)>,
    medium: Mutex<KvMedium>,
}

impl Default for KvStores {
    /// No namespace: every `kv/` ask is `unsupported` until the host names the app and locale.
    fn default() -> Self {
        Self {
            namespace: None,
            medium: Mutex::new(KvMedium::Memory(HashMap::new())),
        }
    }
}

impl KvStores {
    pub fn memory(app: &str, locale: &str) -> Self {
        Self {
            namespace: Some((app.to_string(), locale.to_string())),
            medium: Mutex::new(KvMedium::Memory(HashMap::new())),
        }
    }

    pub fn directory(state_dir: impl Into<PathBuf>, app: &str, locale: &str) -> Self {
        Self {
            namespace: Some((app.to_string(), locale.to_string())),
            medium: Mutex::new(KvMedium::Directory(state_dir.into())),
        }
    }
}

fn fail(op: OsOp, kind: OsErrorKind, what: impl std::fmt::Display) -> OsError {
    OsError::new(kind, format!("{}: {what}", op.name()))
}

fn check_segment(op: OsOp, what: &str, seg: &str) -> Result<(), OsError> {
    let ok = !seg.is_empty()
        && seg != "."
        && seg != ".."
        && seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(fail(
            op,
            OsErrorKind::Invalid,
            format!("kv {what} name {seg:?} is not [A-Za-z0-9._-]+"),
        ))
    }
}

fn string_arg<'a>(op: OsOp, args: &'a [Value], i: usize, what: &str) -> Result<&'a str, OsError> {
    match args.get(i) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(fail(
            op,
            OsErrorKind::Invalid,
            format!("kv {what} must be a string"),
        )),
    }
}

fn check_json(op: OsOp, value: &Value, path: &str) -> Result<(), OsError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(n) if n.is_finite() => Ok(()),
        Value::Number(_) => Err(fail(
            op,
            OsErrorKind::Invalid,
            format!("kv value at {path} is not a finite number"),
        )),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(i, v)| check_json(op, v, &format!("{path}[{i}]"))),
        Value::Object(fields) => fields
            .iter()
            .try_for_each(|(k, v)| check_json(op, v, &format!("{path}.{k}"))),
        Value::Lambda { .. } => Err(fail(
            op,
            OsErrorKind::Invalid,
            format!("kv value at {path} is not JSON"),
        )),
    }
}

/// `JSON.stringify` of a checked JSON value: JS number spelling, JSON escaping, object fields in their order.
pub fn json_text(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, &mut out);
    out
}

fn write_string(s: &str, out: &mut String) {
    out.push_str(&serde_json::to_string(s).unwrap_or_default());
}

fn write_json(value: &Value, out: &mut String) {
    match value {
        Value::Null | Value::Lambda { .. } => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&js_number_string(*n)),
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Value::Object(fields) => {
            out.push('{');
            for (i, (k, v)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_json(v, out);
            }
            out.push('}');
        }
    }
}

/// The store file's bytes: one JSON object, keys in UTF-8 byte order.
pub fn serialize_table(table: &KvTable) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in table.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_string(k, &mut out);
        out.push(':');
        write_json(v, &mut out);
    }
    out.push('}');
    out
}

fn store_file(state_dir: &Path, app: &str, locale: &str, store: &str) -> (PathBuf, PathBuf) {
    let dir = state_dir.join("kv").join(app).join(locale);
    let file = dir.join(format!("{store}.json"));
    (dir, file)
}

fn load_file(op: OsOp, file: &Path) -> Result<KvTable, OsError> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(KvTable::new()),
        Err(e) => return Err(fail(op, OsErrorKind::Io, format!("kv read failed: {e}"))),
    };
    let parsed: Value = serde_json::from_str(&text).map_err(|e| {
        fail(
            op,
            OsErrorKind::Io,
            format!("kv store file is corrupt: {e}"),
        )
    })?;
    let Value::Object(fields) = parsed else {
        return Err(fail(
            op,
            OsErrorKind::Io,
            "kv store file is not a JSON object",
        ));
    };
    Ok(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn commit_file(op: OsOp, dir: &Path, file: &Path, table: &KvTable) -> Result<(), OsError> {
    let mut suffix = [0u8; 6];
    getrandom::getrandom(&mut suffix)
        .map_err(|e| fail(op, OsErrorKind::Io, format!("kv write failed: {e}")))?;
    let hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".tmp-{hex}"));
    let tmp = PathBuf::from(tmp);
    let written = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut handle = std::fs::File::create(&tmp)?;
        handle.write_all(serialize_table(table).as_bytes())?;
        handle.sync_all()?;
        drop(handle);
        std::fs::rename(&tmp, file)
    })();
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(op, OsErrorKind::Io, format!("kv write failed: {e}"))
    })
}

impl KvStores {
    /// Run `f` on the store's table under the one lock; `commit` decides whether the changed table is written back.
    fn with_table<R>(
        &self,
        op: OsOp,
        store: &str,
        f: impl FnOnce(&mut KvTable) -> Result<(R, bool), OsError>,
    ) -> Result<R, OsError> {
        let Some((app, locale)) = &self.namespace else {
            return Err(fail(
                op,
                OsErrorKind::Unsupported,
                "no kv namespace is configured on this host",
            ));
        };
        check_segment(op, "app", app)?;
        check_segment(op, "locale", locale)?;
        let mut medium = self.medium.lock();
        match &mut *medium {
            KvMedium::Memory(tables) => {
                let id = format!("kv:{app}:{locale}:{store}");
                let mut table = tables.get(&id).cloned().unwrap_or_default();
                let (out, commit) = f(&mut table)?;
                if commit {
                    tables.insert(id, table);
                }
                Ok(out)
            }
            KvMedium::Directory(state_dir) => {
                let (dir, file) = store_file(state_dir, app, locale, store);
                let mut table = load_file(op, &file)?;
                let (out, commit) = f(&mut table)?;
                if commit {
                    commit_file(op, &dir, &file, &table)?;
                }
                Ok(out)
            }
        }
    }
}

enum BatchOp {
    Put(String, Value),
    Delete(String),
}

fn batch_ops(op: OsOp, args: &[Value]) -> Result<Vec<BatchOp>, OsError> {
    let Some(Value::Array(items)) = args.get(1) else {
        return Err(fail(
            op,
            OsErrorKind::Invalid,
            "kv batch ops must be a list",
        ));
    };
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let Value::Object(fields) = item else {
                return Err(fail(
                    op,
                    OsErrorKind::Invalid,
                    format!("kv batch op {i} is not an object"),
                ));
            };
            let Some(Value::String(key)) = fields.get("key") else {
                return Err(fail(
                    op,
                    OsErrorKind::Invalid,
                    format!("kv batch op {i} key must be a string"),
                ));
            };
            match fields.get("kind") {
                Some(Value::String(kind)) if kind == "put" => {
                    let value = fields.get("value").ok_or_else(|| {
                        fail(
                            op,
                            OsErrorKind::Invalid,
                            format!("kv value at ops[{i}].value is not JSON"),
                        )
                    })?;
                    check_json(op, value, &format!("ops[{i}].value"))?;
                    Ok(BatchOp::Put(key.clone().into_inner(), value.clone()))
                }
                Some(Value::String(kind)) if kind == "delete" => Ok(BatchOp::Delete(key.clone().into_inner())),
                _ => Err(fail(
                    op,
                    OsErrorKind::Invalid,
                    format!("kv batch op {i} has unknown kind"),
                )),
            }
        })
        .collect()
}

fn obj<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(
        (fields
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect::<ValueMap>()).into(),
    )
}

/// Answer one `kv/` ask: arguments are checked (`invalid`), then the store's grant (`denied`), then the store is used.
pub fn answer(op: OsOp, args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let store = string_arg(op, args, 0, "store")?;
    check_segment(op, "store", store)?;
    let mode = match op {
        OsOp::KvGet | OsOp::KvScan => KvMode::Read,
        _ => KvMode::Write,
    };
    let checked = match op {
        OsOp::KvGet | OsOp::KvDelete => string_arg(op, args, 1, "key").map(|_| ()),
        OsOp::KvPut => string_arg(op, args, 1, "key").and_then(|_| match args.get(2) {
            Some(value) => check_json(op, value, "value"),
            None => Err(fail(
                op,
                OsErrorKind::Invalid,
                "kv value at value is not JSON",
            )),
        }),
        OsOp::KvScan => string_arg(op, args, 1, "prefix").and_then(|_| match args.get(2) {
            None => Ok(()),
            Some(Value::Number(n)) if n.is_finite() && n.fract() == 0.0 && *n >= 0.0 => Ok(()),
            Some(_) => Err(fail(
                op,
                OsErrorKind::Invalid,
                "kv scan limit must be a non-negative integer",
            )),
        }),
        OsOp::KvBatch => batch_ops(op, args).map(|_| ()),
        _ => Err(fail(op, OsErrorKind::Unsupported, "not a kv operation")),
    };
    checked?;
    if !host.grants.allows(&OsRequirement::Kv {
        store: store.to_string(),
        mode,
    }) {
        return Err(fail(
            op,
            OsErrorKind::Denied,
            format!("no kv grant covers {} on store {store:?}", mode.name()),
        ));
    }
    let stores = &host.handles.kv;
    match op {
        OsOp::KvGet => {
            let key = string_arg(op, args, 1, "key")?;
            stores.with_table(op, store, |table| {
                Ok((
                    match table.get(key) {
                        Some(value) => {
                            obj([("found", Value::Bool(true)), ("value", value.clone())])
                        }
                        None => obj([("found", Value::Bool(false))]),
                    },
                    false,
                ))
            })
        }
        OsOp::KvPut => {
            let key = string_arg(op, args, 1, "key")?.to_string();
            let value = args[2].clone();
            stores.with_table(op, store, |table| {
                table.insert(key.clone(), value);
                Ok((obj([("storedKey", Value::string(key))]), true))
            })
        }
        OsOp::KvDelete => {
            let key = string_arg(op, args, 1, "key")?;
            stores.with_table(op, store, |table| {
                let existed = table.remove(key).is_some();
                Ok((obj([("existed", Value::Bool(existed))]), existed))
            })
        }
        OsOp::KvScan => {
            let prefix = string_arg(op, args, 1, "prefix")?;
            let limit = match args.get(2) {
                Some(Value::Number(n)) => Some(*n as usize),
                _ => None,
            };
            stores.with_table(op, store, |table| {
                let entries: Vec<Value> = table
                    .range(prefix.to_string()..)
                    .take_while(|(k, _)| k.as_bytes().starts_with(prefix.as_bytes()))
                    .take(limit.unwrap_or(usize::MAX))
                    .map(|(k, v)| obj([("key", Value::string(k.clone())), ("value", v.clone())]))
                    .collect();
                Ok((obj([("entries", Value::array(entries))]), false))
            })
        }
        OsOp::KvBatch => {
            let ops = batch_ops(op, args)?;
            let applied = ops.len();
            stores.with_table(op, store, |table| {
                for item in ops {
                    match item {
                        BatchOp::Put(key, value) => {
                            table.insert(key, value);
                        }
                        BatchOp::Delete(key) => {
                            table.remove(&key);
                        }
                    }
                }
                Ok((obj([("applied", Value::Number(applied as f64))]), true))
            })
        }
        _ => Err(fail(op, OsErrorKind::Unsupported, "not a kv operation")),
    }
}
