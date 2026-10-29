//! The shapes the credential store answers with, and the raw store asks the `credential-*` operators make. The host
//! supplies the store and the mode flag; the `std-credential-admin` program owns every rule over what they answer.
use serde::{Deserialize, Serialize};

use orbital_core_base::value::{Value, ValueMap};
use orbital_core_base::credential_admin::CredentialAdminOp;

use crate::resumable::IoPort;

/// One stored credential row as the store reports it: its service and credential name, never its value.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredName {
    pub service: String,
    pub name: String,
}

/// The credential store's state: whether it can encrypt (a master key is present) and the rows it holds.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreListing {
    pub enabled: bool,
    pub entries: Vec<StoredName>,
}

/// What `credential-status` reports of one credential: whether the host holds a value, and the non-secret account label
/// stored beside it (the email it was signed in as), never the value.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialStanding {
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl CredentialStanding {
    /// A standing with no label.
    pub fn of(connected: bool) -> Self {
        Self { connected, label: None }
    }
}

/// What a rotation did to the stored rows.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RotateCounts {
    #[cfg_attr(test, ts(type = "number"))]
    pub rotated: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub already_current: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub unreadable: u64,
}

fn object(pairs: Vec<(&str, Value)>) -> Value {
    Value::object(ValueMap::from_iter(pairs.into_iter().map(|(key, value)| (key.into(), value))))
}

fn text(request: &Value, op: CredentialAdminOp, key: &str) -> Result<String, String> {
    match request {
        Value::Object(fields) => match fields.get(key) {
            Some(Value::String(value)) => Ok(value.to_string()),
            Some(_) => Err(format!("{} needs {key} as text", op.name())),
            None => Err(format!("{} needs {key}", op.name())),
        },
        _ => Err(format!("{} takes a request object", op.name())),
    }
}

/// Ask the host's store or mode flag for `op`, answering the raw reply.
pub(crate) fn ask(port: &IoPort, op: CredentialAdminOp, request: &Value) -> Result<Value, String> {
    match op {
        CredentialAdminOp::List => {
            let listing = port.credential_store_list()?.unwrap_or(StoreListing { enabled: false, entries: Vec::new() });
            let entries = listing
                .entries
                .into_iter()
                .map(|entry| object(vec![("service", Value::String(entry.service.into())), ("name", Value::String(entry.name.into()))]))
                .collect::<Vec<_>>();
            Ok(object(vec![("enabled", Value::Bool(listing.enabled)), ("entries", Value::array(entries))]))
        }
        CredentialAdminOp::Set => {
            port.credential_store_write(&text(request, op, "service")?, &text(request, op, "envVar")?, &text(request, op, "value")?)?;
            Ok(object(vec![("saved", Value::Bool(true))]))
        }
        CredentialAdminOp::Remove => Ok(object(vec![("removed", Value::Bool(port.credential_store_remove(&text(request, op, "envVar")?)?))])),
        CredentialAdminOp::Test => {
            let held = port.secret(&text(request, op, "envVar")?, None)?.is_some_and(|value| !value.is_empty());
            Ok(object(vec![("held", Value::Bool(held))]))
        }
        CredentialAdminOp::Mode => {
            let mode = match request {
                Value::Object(fields) if fields.contains_key("mode") => port.credential_mode_write(&text(request, op, "mode")?)?,
                _ => port.credential_mode_read()?.unwrap_or_default(),
            };
            Ok(object(vec![("mode", Value::String(mode.into()))]))
        }
        CredentialAdminOp::Rotate => {
            let counts = port.credential_store_rotate()?;
            Ok(object(vec![
                ("rotated", Value::Number(counts.rotated as f64)),
                ("alreadyCurrent", Value::Number(counts.already_current as f64)),
                ("unreadable", Value::Number(counts.unreadable as f64)),
            ]))
        }
    }
}
