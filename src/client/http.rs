//! HTTP Client for Server Communication
//!
//! The blocking HTTP transport a client renderer posts events through
//! (`SyncServerBridge`, feature `client-http`). All server-side effects (fetch,
//! persist, call-service) are delegated through it.

#[cfg(feature = "client-http")]
use crate::{EventRequest, EventResponse, TraitFrom, Value};
use thiserror::Error;
#[cfg(feature = "client-http")]
use tracing::{debug, error, info};

/// Errors that can occur during server communication
#[derive(Debug, Error)]
pub enum BridgeError {
    /// Network error (connection failed, timeout, etc.)
    #[error("Network error: {0}")]
    Network(String),

    /// Error parsing server response
    #[error("Parse error: {0}")]
    Parse(String),

    /// Server returned an error
    #[error("Server error: {0}")]
    Server(String),

    /// Request was invalid
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
}

/// Build the canonical `EventRequest` (`@almadar/core`'s `OrbitalEventRequest` mirror)
/// from this bridge's `(entity_id, trait_name)` call shape — `targetTrait` addresses the
/// trait (scoped-listen delivery), `traits`/`entityByTrait` fill the stateless addressing
/// fields the compiled server also reads (`handlers.rs:510-532`).
#[cfg(feature = "client-http")]
fn build_event_request(event: &str, entity_id: Option<&str>, trait_name: Option<&str>, payload: Value) -> EventRequest {
    EventRequest {
        event: event.to_string(),
        entity_id: Some(entity_id.unwrap_or("singleton").to_string()),
        target_trait: trait_name.map(str::to_string),
        traits: trait_name.map(|t| vec![TraitFrom { r#trait: t.to_string(), from: t.to_string(), event: None }]),
        payload: if payload.is_null() { None } else { Some(payload) },
        ..Default::default()
    }
}

/// Synchronous HTTP client wrapper for non-async contexts
///
/// Use this in synchronous contexts like egui update functions
/// where async is not available.
#[cfg(feature = "client-http")]
#[derive(Clone)]
pub struct SyncServerBridge {
    base_url: String,
}

#[cfg(feature = "client-http")]
impl SyncServerBridge {
    /// Create a new sync server bridge
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        info!(url = %base_url, "Creating SyncServerBridge");

        Self { base_url }
    }

    /// Get the server base URL
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Check server health
    pub fn health_check(&self) -> Result<bool, BridgeError> {
        let url = format!("{}/api/health", self.base_url);
        debug!(url = %url, "Health check (sync)");

        let response = ureq::get(&url)
            .call()
            .map_err(|e| BridgeError::Network(e.to_string()))?;

        Ok(response.status() == 200)
    }

    /// Send an event to the server (synchronous)
    pub fn send_event(
        &self,
        orbital: &str,
        event: &str,
        payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        self.send_event_with_options(orbital, event, None, None, payload)
    }

    /// Send an event for a specific entity
    pub fn send_event_for_entity(
        &self,
        orbital: &str,
        event: &str,
        entity_id: &str,
        payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        self.send_event_with_options(orbital, event, Some(entity_id), None, payload)
    }

    /// Send an event with full options
    pub fn send_event_with_options(
        &self,
        orbital: &str,
        event: &str,
        entity_id: Option<&str>,
        trait_name: Option<&str>,
        payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        let url = format!("{}/api/orbitals/{}/events", self.base_url, orbital);

        debug!(
            url = %url,
            event = %event,
            entity_id = ?entity_id,
            "Sending event to server (sync)"
        );

        let request = build_event_request(event, entity_id, trait_name, payload);

        let response = ureq::post(&url)
            .send_json(&request)
            .map_err(|e| BridgeError::Network(e.to_string()))?;

        if response.status() != 200 {
            let body = response.into_string().unwrap_or_default();
            error!(body = %body, "Server returned error");
            return Err(BridgeError::Server(body));
        }

        let event_response: EventResponse = response
            .into_json()
            .map_err(|e| BridgeError::Parse(e.to_string()))?;

        debug!(
            success = event_response.success,
            transitioned = event_response.transitioned,
            "Received server response (sync)"
        );

        Ok(event_response)
    }

    /// Post a full `EventRequest` AS-IS (the client-kernel "server leg" — carries
    /// `traits`/`entityByTrait`/`clientId` that `send_event_with_options`'s
    /// narrower `(event, entity_id, trait_name, payload)` signature can't express)
    /// to `/api/orbitals/{orbital}/events`.
    pub fn send_request(&self, orbital: &str, request: &EventRequest) -> Result<EventResponse, BridgeError> {
        let url = format!("{}/api/orbitals/{}/events", self.base_url, orbital);

        debug!(url = %url, event = %request.event, "Sending server leg (sync)");

        let response = ureq::post(&url)
            .send_json(request)
            .map_err(|e| BridgeError::Network(e.to_string()))?;

        if response.status() != 200 {
            let body = response.into_string().unwrap_or_default();
            error!(body = %body, "Server returned error");
            return Err(BridgeError::Server(body));
        }

        let event_response: EventResponse = response
            .into_json()
            .map_err(|e| BridgeError::Parse(e.to_string()))?;

        debug!(
            success = event_response.success,
            transitioned = event_response.transitioned,
            "Received server response (sync leg)"
        );

        Ok(event_response)
    }
}

/// Builder for configuring a server bridge with runtime
#[derive(Debug, Default)]
pub struct ServerBridgeBuilder {
    base_url: Option<String>,
}

impl ServerBridgeBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self { base_url: None }
    }

    /// Set the server URL
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Build the sync server bridge
    #[cfg(feature = "client-http")]
    pub fn build_sync(self) -> Result<SyncServerBridge, BridgeError> {
        let base_url = self
            .base_url
            .ok_or_else(|| BridgeError::InvalidRequest("Server URL is required".to_string()))?;

        Ok(SyncServerBridge { base_url })
    }
}

#[cfg(all(test, feature = "client-http"))]
mod tests {
    use super::*;

    #[test]
    fn the_builder_keeps_the_url() {
        let bridge = ServerBridgeBuilder::new().url("http://localhost:3030").build_sync().unwrap();
        assert_eq!(bridge.base_url, "http://localhost:3030");
    }

    #[test]
    fn control_the_builder_without_a_url_is_refused() {
        assert!(matches!(ServerBridgeBuilder::new().build_sync(), Err(BridgeError::InvalidRequest(_))));
    }
}
