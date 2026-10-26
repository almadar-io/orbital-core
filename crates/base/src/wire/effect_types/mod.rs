//! Effect Types
//!
//! Core types for effect execution.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::value::Value;
use crate::value::ValueMap;
use crate::error::EvalError;
use crate::http_wire::HttpMethod;
use crate::os::{accepts_arity, split_envelope, OsOp};
use crate::schema::sexpr::SExpression;
use serde::{Deserialize, Serialize};

pub fn call_key_text(value: &Value) -> Result<&str, EvalError> {
    match value {
        Value::String(text) if !text.is_empty() => Ok(text),
        _ => Err(EvalError::InvalidArgument(format!(
            "call key must evaluate to a non-empty string, got {}",
            value.to_string_value()
        ))),
    }
}

/// Runtime environment for effect execution
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum RuntimeEnvironment {
    /// Server-side environment (Node.js, Rust server)
    #[default]
    Server,
    /// Client-side environment (Browser, Bevy)
    Client,
}

impl RuntimeEnvironment {
    /// Check if this is the server environment
    pub fn is_server(&self) -> bool {
        matches!(self, RuntimeEnvironment::Server)
    }

    /// Check if this is the client environment
    pub fn is_client(&self) -> bool {
        matches!(self, RuntimeEnvironment::Client)
    }
}

/// Completion events declared on a `fetch` (or `persist`) effect's options
/// object (`["fetch", X, { "emit": { "success": "...", "failure": "..." } }]`).
///
/// After the fetch resolves, the runtime fires `success` (carrying the fetched rows
/// as `{ "data": [...] }`, so a `@payload.data`-bound table renders) or `failure` on
/// error. This is how a data-loading trait advances from its `loading` state to its
/// `loaded` state — without it, the table never renders (RS-G16). `persist` reuses
/// the same shape for its own `emit.failure` (denial / required-column-missing).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FetchEmit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// A keyed `call-service` aborted by `cancel-call`: fires with `{ key }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<String>,
    /// A running `call-service`'s live messages; delivered by the host, never in-band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_message: Option<String>,
}

impl FetchEmit {
    /// True when no completion event is declared (nothing to emit when the effect finishes).
    pub fn is_empty(&self) -> bool {
        self.success.is_none() && self.failure.is_none() && self.cancelled.is_none()
    }
}

/// What a `fetch` hands its success event: the rows, or a [`RowSetRef`](crate::runtime::row_query::RowSetRef)
/// to them (`deliver: ids`, Almadar_Embedded_Kernel_Speed_Design.md section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FetchDeliver {
    #[default]
    Rows,
    Ids,
}

impl FetchDeliver {
    fn is_rows(&self) -> bool {
        *self == FetchDeliver::Rows
    }
}

mod effect;
mod result;
mod parse;
pub use effect::{Effect, CLIENT_EFFECT_KINDS};
pub use parse::{is_known_effect_name, parse_effect, strip_trailing_emit_options};
pub use result::{BatchFailure, EffectOutcome, EffectResult, LogLevel, PersistOperation};

#[cfg(test)]
mod tests;
