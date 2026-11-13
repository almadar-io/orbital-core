//! HTTP Client for Server Communication
//!
//! The blocking HTTP transport a client renderer posts events through
//! (`SyncServerBridge`, feature `client-http`). All server-side effects (fetch,
//! persist, call-service) are delegated through it.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::effects::response::{EventRequest, EventResponse};
#[cfg(feature = "client-http")]
use crate::effects::response::TraitFrom;
#[cfg(feature = "client-http")]
use orbital_core_base::value::Value;
use thiserror::Error;
#[cfg(feature = "client-http")]
use tracing::{debug, error, info, warn};

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

/// A dispatch the host ran on its own, as a `{ type: dispatch }` `/api/events` frame carries it: the client folds its
/// response and never runs it again (the web twin: `EventTransport.subscribeHostDispatches`).
#[derive(Debug, Clone)]
pub struct PushedDispatch {
    pub orbital_name: String,
    pub request: EventRequest,
    pub response: EventResponse,
}

#[cfg(feature = "client-http")]
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum FeedFrame {
    #[serde(rename_all = "camelCase")]
    Dispatch { orbital_name: String, request: EventRequest, response: EventResponse },
    #[serde(other)]
    Other,
}

/// A numbered frame's id, `<server boot>-<n>` (the web's `FrameNumber`): a reopened stream resumes after the last one
/// applied, and a replay of one already applied is dropped.
#[cfg(feature = "client-http")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameNumber {
    boot: String,
    n: u64,
}

#[cfg(feature = "client-http")]
fn parse_frame_number(id: &str) -> Option<FrameNumber> {
    let dash = id.rfind('-').filter(|dash| *dash > 0)?;
    Some(FrameNumber { boot: id[..dash].to_string(), n: id[dash + 1..].parse().ok()? })
}

/// The delay before a dropped stream is opened again.
#[cfg(feature = "client-http")]
const FEED_REOPEN: std::time::Duration = std::time::Duration::from_secs(1);

/// One client's `/api/events` stream, read on its own thread: the host-run dispatches addressed to this client id
/// wait here until the window drains them. Dropping the feed ends the thread at its next frame or reopen.
#[cfg(feature = "client-http")]
pub struct LiveFeed {
    frames: std::sync::mpsc::Receiver<PushedDispatch>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "client-http")]
impl LiveFeed {
    /// The dispatches that arrived since the last call, in arrival order.
    pub fn drain(&self) -> Vec<PushedDispatch> {
        self.frames.try_iter().collect()
    }
}

/// A client's live stream as a window drains it, whatever transport carries it.
pub trait HostFeed: Send {
    fn drain(&self) -> Vec<PushedDispatch>;
}

#[cfg(feature = "client-http")]
impl HostFeed for LiveFeed {
    fn drain(&self) -> Vec<PushedDispatch> {
        LiveFeed::drain(self)
    }
}

#[cfg(feature = "client-http")]
impl Drop for LiveFeed {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Read SSE `data:` events from `stream` and hand each dispatch frame to `deliver`; other frames (`bus`) are not
/// dispatches and are skipped. A numbered frame (`id:`) of the boot `seen` holds that is not past it is a replay and is
/// dropped; `seen` moves to the last numbered frame read. Returns when the stream ends or `deliver` refuses.
#[cfg(feature = "client-http")]
fn read_frames(
    stream: impl std::io::BufRead,
    seen: &mut Option<FrameNumber>,
    mut deliver: impl FnMut(PushedDispatch) -> bool,
) {
    let mut data = String::new();
    let mut id: Option<FrameNumber> = None;
    for line in stream.lines() {
        let Ok(line) = line else { return };
        if let Some(raw) = line.strip_prefix("id:") {
            id = parse_frame_number(raw.strip_prefix(' ').unwrap_or(raw));
            continue;
        }
        if let Some(chunk) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(chunk.strip_prefix(' ').unwrap_or(chunk));
            continue;
        }
        if !line.is_empty() {
            continue;
        }
        let text = std::mem::take(&mut data);
        let number = id.take();
        if text.is_empty() {
            continue;
        }
        if let Some(number) = number {
            if seen.as_ref().is_some_and(|last| last.boot == number.boot && number.n <= last.n) {
                continue;
            }
            *seen = Some(number);
        }
        match serde_json::from_str::<FeedFrame>(&text) {
            Ok(FeedFrame::Dispatch { orbital_name, request, response }) => {
                if !deliver(PushedDispatch { orbital_name, request, response }) {
                    return;
                }
            }
            Ok(FeedFrame::Other) => {}
            Err(e) => warn!(error = %e, "live feed: a frame was not understood"),
        }
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

    /// Open `client_id`'s live stream (`POST /api/events/ticket`, then `GET /api/events`, the web transport's own
    /// handshake): the host-run dispatches addressed to that client id arrive on the returned feed, and a stream that
    /// drops is opened again.
    pub fn open_feed(&self, client_id: &str) -> LiveFeed {
        let (tx, frames) = std::sync::mpsc::channel();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (base, client_id, halted) = (self.base_url.clone(), client_id.to_string(), stop.clone());
        std::thread::spawn(move || {
            let mut seen = None;
            while !halted.load(std::sync::atomic::Ordering::SeqCst) {
                match stream_once(&base, &client_id, &mut seen, &halted, &tx) {
                    Ok(()) => {}
                    Err(e) => warn!(error = %e, "live feed: the stream could not be opened"),
                }
                std::thread::sleep(FEED_REOPEN);
            }
        });
        LiveFeed { frames, stop }
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

    /// Ask the server to resolve `reference` for the viewer it serves (`POST /api/orbitals/resolve-rows`).
    pub fn resolve_rows(
        &self,
        reference: &orbital_core_base::row_set::RowSetRef,
    ) -> Result<crate::row_query::RowPage, BridgeError> {
        let url = format!("{}/api/orbitals/resolve-rows", self.base_url);
        let request = crate::row_query::ResolveRowsRequest { reference: reference.clone() };
        let response = ureq::post(&url)
            .send_json(&request)
            .map_err(|e| BridgeError::Network(e.to_string()))?;
        if response.status() != 200 {
            let body = response.into_string().unwrap_or_default();
            error!(body = %body, "Server refused to resolve a row set");
            return Err(BridgeError::Server(body));
        }
        response.into_json().map_err(|e| BridgeError::Parse(e.to_string()))
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

/// One connection of a client's stream: mint a ticket, open the stream with it, deliver its dispatch frames until it ends.
#[cfg(feature = "client-http")]
fn stream_once(
    base: &str,
    client_id: &str,
    seen: &mut Option<FrameNumber>,
    halted: &std::sync::atomic::AtomicBool,
    tx: &std::sync::mpsc::Sender<PushedDispatch>,
) -> Result<(), BridgeError> {
    #[derive(serde::Deserialize)]
    struct Ticket {
        ticket: String,
    }
    let minted: Ticket = ureq::post(&format!("{base}/api/events/ticket"))
        .query("clientId", client_id)
        .call()
        .map_err(|e| BridgeError::Network(e.to_string()))?
        .into_json()
        .map_err(|e| BridgeError::Parse(e.to_string()))?;
    let mut request = ureq::get(&format!("{base}/api/events"))
        .query("clientId", client_id)
        .query("ticket", &minted.ticket);
    if let Some(last) = seen.as_ref() {
        request = request.query("since", &format!("{}-{}", last.boot, last.n));
    }
    let stream = request.call().map_err(|e| BridgeError::Network(e.to_string()))?;
    read_frames(std::io::BufReader::new(stream.into_reader()), seen, |frame| {
        tx.send(frame).is_ok() && !halted.load(std::sync::atomic::Ordering::SeqCst)
    });
    Ok(())
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

    #[cfg(feature = "client-http")]
    mod feed {
        use super::*;
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        const DISPATCH: &str = r#"{"type":"dispatch","orbitalName":"Files","request":{"event":"OF_ROOT","targetTrait":"Reader","entityId":"singleton"},"response":{"success":true,"transitioned":true},"timestamp":1}"#;

        /// A host that serves the stream only to `owner`: the ticket route mints for any client id, and the stream
        /// carries one `bus` frame and one `dispatch` frame when the query names `owner`, and nothing otherwise.
        fn host(owner: &'static str) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            std::thread::spawn(move || {
                for connection in listener.incoming() {
                    let Ok(mut connection) = connection else { return };
                    std::thread::spawn(move || {
                        let mut reader = BufReader::new(connection.try_clone().unwrap());
                        let mut request = String::new();
                        reader.read_line(&mut request).unwrap();
                        loop {
                            let mut line = String::new();
                            reader.read_line(&mut line).unwrap();
                            if line == "\r\n" || line.is_empty() {
                                break;
                            }
                        }
                        let body = if request.starts_with("POST /api/events/ticket") {
                            r#"{"ticket":"t1"}"#.to_string()
                        } else if request.contains(&format!("clientId={owner}")) && request.contains("ticket=t1") {
                            format!("data: {{\"type\":\"bus\",\"event\":\"X\"}}\n\ndata: {DISPATCH}\n\n")
                        } else {
                            String::new()
                        };
                        let kind = if request.starts_with("POST") { "application/json" } else { "text/event-stream" };
                        let _ = write!(connection, "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                    });
                }
            });
            url
        }

        fn wait_for(feed: &LiveFeed, at_least: usize) -> Vec<PushedDispatch> {
            let mut got = Vec::new();
            for _ in 0..50 {
                got.extend(feed.drain());
                if got.len() >= at_least {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            got
        }

        #[test]
        fn a_dispatch_frame_pushed_to_this_client_reaches_its_feed() {
            let bridge = SyncServerBridge::new(host("tab-1"));
            let feed = bridge.open_feed("tab-1");
            let got = wait_for(&feed, 1);
            assert_eq!(got.len(), 1, "the bus frame is skipped and the dispatch frame arrives once");
            assert_eq!(got[0].orbital_name, "Files");
            assert_eq!(got[0].request.event, "OF_ROOT");
            assert_eq!(got[0].request.target_trait.as_deref(), Some("Reader"));
            assert!(got[0].response.success);
        }

        #[test]
        fn control_a_frame_for_another_client_id_never_reaches_the_feed() {
            let bridge = SyncServerBridge::new(host("tab-1"));
            let feed = bridge.open_feed("tab-2");
            assert!(wait_for(&feed, 1).is_empty());
        }

        #[test]
        fn control_a_frame_that_is_not_a_dispatch_is_skipped() {
            let mut seen = Vec::new();
            read_frames(std::io::Cursor::new("data: {\"type\":\"bus\",\"event\":\"X\"}\n\n"), &mut None, |frame| {
                seen.push(frame);
                true
            });
            assert!(seen.is_empty());
        }
    }
}
