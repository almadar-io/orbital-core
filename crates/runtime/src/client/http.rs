//! HTTP Client for Server Communication
//!
//! The blocking HTTP transport a client renderer posts events through
//! (`SyncServerBridge`, feature `client-http`). All server-side effects (fetch,
//! persist, call-service) are delegated through it.
use orbital_core_base::json_value::from_json_str;
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

    /// The host freed this client's session (EXEC-DIST-9, `409 SESSION_ENDED`): a mount with the frames the client holds
    /// revives it (the web transport's `SessionEndedError`).
    #[error("Session ended: {0}")]
    SessionEnded(String),
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

/// One item of a client's live stream: a dispatch the host ran on its own, or a persist another client made, which
/// reaches this client's declared listeners (Orbital_Core §6; the web twin: `EventTransport.subscribe`).
#[derive(Debug, Clone)]
pub enum FeedItem {
    Dispatch(PushedDispatch),
    Peer(orbital_core_base::bus_event::EmittedEvent),
}

#[cfg(feature = "client-http")]
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum FeedFrame {
    #[serde(rename_all = "camelCase")]
    Dispatch { orbital_name: String, request: EventRequest, response: EventResponse },
    Bus {
        event: String,
        #[serde(default)]
        payload: Option<Value>,
        #[serde(default)]
        source: Option<orbital_core_base::bus_event::BusEventSource>,
    },
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
    frames: std::sync::mpsc::Receiver<FeedItem>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "client-http")]
impl LiveFeed {
    /// The items that arrived since the last call, in arrival order.
    pub fn drain(&self) -> Vec<FeedItem> {
        self.frames.try_iter().collect()
    }
}

/// A client's live stream as a window drains it, whatever transport carries it.
pub trait HostFeed: Send {
    fn drain(&self) -> Vec<FeedItem>;
}

#[cfg(feature = "client-http")]
impl HostFeed for LiveFeed {
    fn drain(&self) -> Vec<FeedItem> {
        LiveFeed::drain(self)
    }
}

#[cfg(feature = "client-http")]
impl Drop for LiveFeed {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Read SSE `data:` events from `stream` and hand each dispatch frame and each `bus` frame (a peer's persist) to
/// `deliver`; any other frame is skipped. A numbered frame (`id:`) of the boot `seen` holds that is not past it is a replay and is
/// dropped; `seen` moves to the last numbered frame read. Returns when the stream ends or `deliver` refuses.
#[cfg(feature = "client-http")]
fn read_frames(
    stream: impl std::io::BufRead,
    seen: &mut Option<FrameNumber>,
    mut deliver: impl FnMut(FeedItem) -> bool,
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
        match from_json_str::<FeedFrame>(&text) {
            Ok(FeedFrame::Dispatch { orbital_name, request, response }) => {
                if !deliver(FeedItem::Dispatch(PushedDispatch { orbital_name, request, response })) {
                    return;
                }
            }
            Ok(FeedFrame::Bus { event, payload, source }) => {
                if !deliver(FeedItem::Peer(orbital_core_base::bus_event::EmittedEvent { event, payload, source })) {
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
    bearer: Bearer,
}

/// The bearer the native host holds for its viewer, shared by every clone of a bridge and read at each request, so a
/// sign-in, a refresh or a sign-out takes hold on the next leg, ticket or stream reconnect. Core knows no provider: the
/// host puts the token here (EXEC-IDENTITY-4).
#[cfg(feature = "client-http")]
type Bearer = std::sync::Arc<std::sync::RwLock<Option<String>>>;

/// A posted event's answer, with the host's `409 { code: "SESSION_ENDED" }` read as [`BridgeError::SessionEnded`] (the web
/// transport's rule) and any other refusal's body kept.
#[cfg(feature = "client-http")]
fn refused(sent: Result<ureq::Response, ureq::Error>) -> Result<ureq::Response, BridgeError> {
    match sent {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(status, response)) => {
            let body = response.into_string().unwrap_or_default();
            let refusal: Option<serde_json::Value> = orbital_core_base::json_value::from_json_str(&body).ok();
            let code = refusal.as_ref().and_then(|r| r.get("code")).and_then(serde_json::Value::as_str);
            if status == 409 && code == Some("SESSION_ENDED") {
                let error = refusal.as_ref().and_then(|r| r.get("error")).and_then(serde_json::Value::as_str).unwrap_or("the session ended");
                return Err(BridgeError::SessionEnded(error.to_string()));
            }
            error!(status, body = %body, "Server refused the event");
            Err(BridgeError::Server(format!("status {status}: {body}")))
        }
        Err(other) => Err(BridgeError::Network(other.to_string())),
    }
}

#[cfg(feature = "client-http")]
fn authorized(request: ureq::Request, bearer: &Bearer) -> ureq::Request {
    match bearer.read().ok().and_then(|held| held.clone()) {
        Some(token) => request.set("authorization", &format!("Bearer {token}")),
        None => request,
    }
}

#[cfg(feature = "client-http")]
impl SyncServerBridge {
    /// Create a new sync server bridge
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        info!(url = %base_url, "Creating SyncServerBridge");

        Self { base_url, bearer: Bearer::default() }
    }

    /// Hold `bearer` for every later request of this bridge and its clones; `None` sends none (signed out).
    pub fn set_bearer(&self, bearer: Option<String>) {
        if let Ok(mut held) = self.bearer.write() {
            *held = bearer;
        }
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
        let (base, client_id, halted, bearer) = (self.base_url.clone(), client_id.to_string(), stop.clone(), self.bearer.clone());
        std::thread::spawn(move || {
            let mut seen = None;
            while !halted.load(std::sync::atomic::Ordering::SeqCst) {
                match stream_once(&base, &client_id, &bearer, &mut seen, &halted, &tx) {
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

        let response = refused(authorized(ureq::post(&url), &self.bearer).send_json(&request))?;

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
        let response = authorized(ureq::post(&url), &self.bearer)
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

        let response = refused(authorized(ureq::post(&url), &self.bearer).send_json(request))?;

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
    bearer: &Bearer,
    seen: &mut Option<FrameNumber>,
    halted: &std::sync::atomic::AtomicBool,
    tx: &std::sync::mpsc::Sender<FeedItem>,
) -> Result<(), BridgeError> {
    #[derive(serde::Deserialize)]
    struct Ticket {
        ticket: String,
    }
    let minted: Ticket = authorized(ureq::post(&format!("{base}/api/events/ticket")), bearer)
        .query("clientId", client_id)
        .call()
        .map_err(|e| BridgeError::Network(e.to_string()))?
        .into_json()
        .map_err(|e| BridgeError::Parse(e.to_string()))?;
    let mut request = authorized(ureq::get(&format!("{base}/api/events")), bearer)
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

        Ok(SyncServerBridge { base_url, bearer: Bearer::default() })
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

    /// The native transport carries the bearer its host holds on every request, as the web transport does
    /// (G-ORBITAL-CORE-234): a leg, the events ticket and the stream.
    #[cfg(feature = "client-http")]
    mod bearer {
        use super::*;
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        /// A host that records each request's first line with its `authorization` header (or `-`).
        fn recording_host() -> (String, Arc<Mutex<Vec<String>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let log = seen.clone();
            std::thread::spawn(move || {
                for connection in listener.incoming() {
                    let Ok(mut connection) = connection else { return };
                    let log = log.clone();
                    std::thread::spawn(move || {
                        let mut reader = BufReader::new(connection.try_clone().unwrap());
                        let mut request = String::new();
                        reader.read_line(&mut request).unwrap();
                        let mut authorization = "-".to_string();
                        let mut length = 0usize;
                        loop {
                            let mut line = String::new();
                            reader.read_line(&mut line).unwrap();
                            if line == "\r\n" || line.is_empty() {
                                break;
                            }
                            let lower = line.to_ascii_lowercase();
                            if let Some(value) = lower.strip_prefix("authorization:") {
                                authorization = line[line.len() - value.len()..].trim().to_string();
                            }
                            if let Some(value) = lower.strip_prefix("content-length:") {
                                length = value.trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body = vec![0u8; length];
                        let _ = std::io::Read::read_exact(&mut reader, &mut body);
                        log.lock().unwrap().push(format!("{} {authorization}", request.split_whitespace().take(2).collect::<Vec<_>>().join(" ")));
                        let (kind, body) = if request.starts_with("POST /api/events/ticket") {
                            ("application/json", r#"{"ticket":"t1"}"#.to_string())
                        } else if request.starts_with("GET /api/events") {
                            ("text/event-stream", String::new())
                        } else {
                            ("application/json", r#"{"success":true,"transitioned":false}"#.to_string())
                        };
                        let _ = write!(connection, "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                    });
                }
            });
            (url, seen)
        }

        fn wait_for(seen: &Arc<Mutex<Vec<String>>>, prefix: &str) -> Option<String> {
            for _ in 0..50 {
                if let Some(line) = seen.lock().unwrap().iter().find(|l| l.starts_with(prefix)) {
                    return Some(line.clone());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            None
        }

        #[test]
        fn a_held_bearer_rides_every_leg_the_ticket_and_the_stream() {
            let (url, seen) = recording_host();
            let bridge = SyncServerBridge::new(url);
            bridge.set_bearer(Some("device-token".to_string()));
            bridge.send_request("Main", &EventRequest { event: "PING".into(), ..Default::default() }).unwrap();
            let _feed = bridge.open_feed("tab-1");
            assert_eq!(wait_for(&seen, "POST /api/orbitals/Main/events").as_deref(), Some("POST /api/orbitals/Main/events Bearer device-token"));
            assert_eq!(wait_for(&seen, "POST /api/events/ticket").map(|l| l.ends_with("Bearer device-token")), Some(true));
            assert_eq!(wait_for(&seen, "GET /api/events").map(|l| l.ends_with("Bearer device-token")), Some(true));
        }

        #[test]
        fn control_with_no_bearer_held_no_request_carries_one() {
            let (url, seen) = recording_host();
            let bridge = SyncServerBridge::new(url);
            bridge.send_request("Main", &EventRequest { event: "PING".into(), ..Default::default() }).unwrap();
            assert_eq!(wait_for(&seen, "POST /api/orbitals/Main/events").as_deref(), Some("POST /api/orbitals/Main/events -"));
        }

        #[test]
        fn control_a_cleared_bearer_stops_riding_later_requests() {
            let (url, seen) = recording_host();
            let bridge = SyncServerBridge::new(url);
            bridge.set_bearer(Some("device-token".to_string()));
            bridge.clone().set_bearer(None);
            bridge.send_request("Main", &EventRequest { event: "PING".into(), ..Default::default() }).unwrap();
            assert_eq!(wait_for(&seen, "POST /api/orbitals/Main/events").as_deref(), Some("POST /api/orbitals/Main/events -"), "edge: a clone shares the one slot");
        }
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

        fn wait_for(feed: &LiveFeed, at_least: usize) -> Vec<FeedItem> {
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
            let got = wait_for(&feed, 2);
            assert_eq!(got.len(), 2, "the bus frame and the dispatch frame each arrive once, in order");
            assert!(matches!(&got[0], FeedItem::Peer(emitted) if emitted.event == "X"));
            let FeedItem::Dispatch(pushed) = &got[1] else { panic!("the second item is the dispatch: {:?}", got[1]) };
            assert_eq!(pushed.orbital_name, "Files");
            assert_eq!(pushed.request.event, "OF_ROOT");
            assert_eq!(pushed.request.target_trait.as_deref(), Some("Reader"));
            assert!(pushed.response.success);
        }

        #[test]
        fn control_a_frame_for_another_client_id_never_reaches_the_feed() {
            let bridge = SyncServerBridge::new(host("tab-1"));
            let feed = bridge.open_feed("tab-2");
            assert!(wait_for(&feed, 1).is_empty());
        }

        #[test]
        fn a_bus_frame_is_handed_on_as_a_peer_emit_with_its_payload_and_source() {
            let mut seen = Vec::new();
            let frame = "data: {\"type\":\"bus\",\"event\":\"SAVED\",\"payload\":{\"title\":\"t\"},\"source\":{\"orbital\":\"Main\",\"trait\":\"Writer\"},\"timestamp\":1}\n\n";
            read_frames(std::io::Cursor::new(frame), &mut None, |item| {
                seen.push(item);
                true
            });
            assert_eq!(seen.len(), 1);
            let FeedItem::Peer(emitted) = &seen[0] else { panic!("a bus frame is a peer emit: {:?}", seen[0]) };
            assert_eq!(emitted.event, "SAVED");
            assert_eq!(emitted.source.as_ref().and_then(|s| s.r#trait.as_deref()), Some("Writer"));
            assert_eq!(emitted.source.as_ref().and_then(|s| s.orbital.as_deref()), Some("Main"));
            assert!(emitted.payload.is_some());
        }

        #[test]
        fn control_a_frame_that_is_neither_a_dispatch_nor_a_bus_emit_is_skipped() {
            let mut seen = Vec::new();
            read_frames(std::io::Cursor::new("data: {\"type\":\"trace\",\"event\":\"X\"}\n\n"), &mut None, |item| {
                seen.push(item);
                true
            });
            assert!(seen.is_empty());
        }
    }
}
