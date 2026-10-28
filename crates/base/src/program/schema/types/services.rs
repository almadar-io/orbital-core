//! Service definitions (REST, socket, MCP) and references to them.

use super::*;

// =============================================================================
// Service Definition
// =============================================================================

/// REST authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestAuthConfig {
    /// Authentication type
    #[serde(rename = "type")]
    pub auth_type: RestAuthType,

    /// For api-key: the query parameter or header name
    #[serde(rename = "keyName", skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,

    /// For api-key: where to place the key
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<AuthKeyLocation>,

    /// Environment variable name containing the secret
    #[serde(rename = "secretEnv", skip_serializing_if = "Option::is_none")]
    pub secret_env: Option<String>,
}

/// REST authentication type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum RestAuthType {
    ApiKey,
    Bearer,
    Basic,
    OAuth2,
}

/// Where to place the API key
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AuthKeyLocation {
    Query,
    Header,
}

/// REST service definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestServiceDef {
    /// Service name
    pub name: String,

    /// Service type (must be "rest")
    #[serde(rename = "type")]
    pub service_type: String,

    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Base URL for the API
    #[serde(rename = "baseUrl")]
    pub base_url: String,

    /// Default headers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<crate::collections::IndexMap<String, String>>,

    /// Authentication configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<RestAuthConfig>,

    /// Timeout in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
}

/// Socket event definitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketEvents {
    /// Events received from server
    pub inbound: Vec<String>,

    /// Events sent to server
    pub outbound: Vec<String>,
}

/// Socket reconnection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketReconnect {
    /// Enable automatic reconnection
    pub enabled: bool,

    /// Maximum reconnection attempts
    #[serde(rename = "maxAttempts", skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,

    /// Delay between attempts in ms
    #[serde(rename = "delayMs", skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u32>,
}

/// Socket service definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketServiceDef {
    /// Service name
    pub name: String,

    /// Service type (must be "socket")
    #[serde(rename = "type")]
    pub service_type: String,

    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// WebSocket URL
    pub url: String,

    /// Event definitions
    pub events: SocketEvents,

    /// Reconnection configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconnect: Option<SocketReconnect>,
}

/// MCP service definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServiceDef {
    /// Service name
    pub name: String,

    /// Service type (must be "mcp")
    #[serde(rename = "type")]
    pub service_type: String,

    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Path to MCP server executable
    #[serde(rename = "serverPath")]
    pub server_path: String,

    /// Capabilities exposed by the MCP server
    pub capabilities: Vec<String>,

    /// Environment variables to pass to the MCP server
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<crate::collections::IndexMap<String, String>>,
}

/// Service definition - discriminated union by type field
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServiceDefinition {
    #[serde(rename = "rest")]
    Rest(RestServiceDefInner),
    #[serde(rename = "socket")]
    Socket(SocketServiceDefInner),
    #[serde(rename = "mcp")]
    Mcp(McpServiceDefInner),
}

/// Inner REST service definition (without type field, used in tagged enum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestServiceDefInner {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<crate::collections::IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<RestAuthConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
}

/// Inner Socket service definition (without type field, used in tagged enum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketServiceDefInner {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub url: String,
    pub events: SocketEvents,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconnect: Option<SocketReconnect>,
}

/// Inner MCP service definition (without type field, used in tagged enum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServiceDefInner {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "serverPath")]
    pub server_path: String,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<crate::collections::IndexMap<String, String>>,
}

impl ServiceDefinition {
    /// Get the service name
    pub fn name(&self) -> &str {
        match self {
            ServiceDefinition::Rest(s) => &s.name,
            ServiceDefinition::Socket(s) => &s.name,
            ServiceDefinition::Mcp(s) => &s.name,
        }
    }

    /// Get the service type as string
    pub fn service_type(&self) -> &str {
        match self {
            ServiceDefinition::Rest(_) => "rest",
            ServiceDefinition::Socket(_) => "socket",
            ServiceDefinition::Mcp(_) => "mcp",
        }
    }
}

/// ServiceRefObject - Reference to imported service with optional overrides.
///
/// Mirrors `PageRefObject` for services. The override fields are a curated
/// subset of what `ServiceDefinition` exposes — only fields that make sense
/// to customize at the reference site (per-environment endpoints, header
/// tweaks, descriptions). No config-merging logic; each override directly
/// replaces the corresponding field on the resolved inline service.
///
/// The overrides are variant-aware: `base_url` and `headers` only apply to
/// `ServiceDefinition::Rest`, `url` only to `Socket`, `server_path` only to
/// `Mcp`. `description` applies to all three. Overrides that don't match
/// the resolved service variant are silently ignored (mirroring how
/// `PageRefObject::path` only applies when the resolved page is inline).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRefObject {
    /// Reference to imported service: "Alias.services.ServiceName"
    #[serde(rename = "ref")]
    pub reference: String,

    /// Optional description override (applies to Rest, Socket, Mcp).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Optional REST `baseUrl` override.
    #[serde(rename = "baseUrl", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Optional REST `headers` override (full replacement, not merge).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<crate::collections::IndexMap<String, String>>,

    /// Optional Socket `url` override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Optional MCP `serverPath` override.
    #[serde(rename = "serverPath", skip_serializing_if = "Option::is_none")]
    pub server_path: Option<String>,
}

/// ServiceRef - Service can be inline definition OR reference to imported service.
///
/// Reference formats:
/// - "Alias.services.ServiceName" - Simple reference (uses upstream defaults)
/// - { ref: "Alias.services.ServiceName", baseUrl: "https://prod.api" } - Reference with overrides
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServiceRef {
    /// Inline service definition (must be FIRST — most specific pattern)
    Inline(ServiceDefinition),
    /// Reference object with optional override fields
    Object(ServiceRefObject),
    /// Simple reference string: "Alias.services.ServiceName"
    Reference(String),
}

impl ServiceRef {
    /// Check if this is a reference (string or object)
    pub fn is_reference(&self) -> bool {
        !matches!(self, ServiceRef::Inline(_))
    }

    /// Get the reference string (works for both Reference and Object forms)
    pub fn as_reference(&self) -> Option<&str> {
        match self {
            ServiceRef::Reference(s) => Some(s),
            ServiceRef::Object(o) => Some(&o.reference),
            _ => None,
        }
    }

    /// Get the inline service if this is inline
    pub fn as_inline(&self) -> Option<&ServiceDefinition> {
        match self {
            ServiceRef::Inline(s) => Some(s),
            _ => None,
        }
    }
}
