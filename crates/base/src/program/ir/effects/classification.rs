//! Classifying an effect for client and server execution, and the types that classification names.

use super::*;

/// Persist operation type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OirPersistOp {
    Create,
    Upsert,
    Read,
    Update,
    Delete,
    List,
    Query,
}

/// Log level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum OirLogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Execution environment for effects
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OirExecutionEnv {
    Client,
    Server,
}

/// Effect execution classification
#[derive(Debug, Clone)]
pub struct EffectClassification {
    pub effect: OirEffect,
    pub client_action: ClientAction,
    pub server_action: ServerAction,
}

/// What the client should do with an effect
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAction {
    /// Execute the effect locally
    Execute,
    /// Ignore (server handles it)
    Ignore,
}

/// What the server should do with an effect
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAction {
    /// Execute the effect on server
    Execute,
    /// Return to client for execution
    ReturnToClient,
}

impl OirEffect {
    /// Classify this effect for client/server execution
    pub fn classify(&self) -> EffectClassification {
        let site = crate::runs_on::effect_runs_on(self);
        let client_action = if site == crate::runs_on::RunsOn::Server {
            ClientAction::Ignore
        } else {
            ClientAction::Execute
        };
        let server_action = if site == crate::runs_on::RunsOn::Client {
            ServerAction::ReturnToClient
        } else {
            ServerAction::Execute
        };

        EffectClassification {
            effect: self.clone(),
            client_action,
            server_action,
        }
    }
}
