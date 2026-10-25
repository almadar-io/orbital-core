//! The native HTTP(S) transport of a server-role host: `ureq` + rustls, redirects never followed, and a connection only to an
//! address that was checked as public (or an IP literal the program lists in `egress`).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
use crate::http_service::{HttpRequest, HttpResponse, Transport, TransportError};
use crate::http_request::is_public_address;
use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};

type Refusals = Arc<Mutex<HashMap<String, String>>>;

/// Resolves a host once, refuses a non-public address (unless the host is an IP literal listed in egress, or
/// the check is off), and hands the checked addresses to ureq, so the connection goes to what was checked.
struct CheckedResolver {
    egress: Vec<String>,
    checked: bool,
    refusals: Refusals,
}

fn netloc_of(origin: &str) -> Option<(IpAddr, u16)> {
    let url = url::Url::parse(origin).ok()?;
    let ip: IpAddr = url.host_str()?.trim_matches(['[', ']']).parse().ok()?;
    Some((ip, url.port_or_known_default()?))
}

impl CheckedResolver {
    fn refuse(&self, netloc: &str, message: String) -> std::io::Error {
        if let Ok(mut refusals) = self.refusals.lock() {
            refusals.insert(netloc.to_string(), message.clone());
        }
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, message)
    }
}

impl ureq::Resolver for CheckedResolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        let addrs: Vec<SocketAddr> = netloc.to_socket_addrs()?.collect();
        if !self.checked {
            return Ok(addrs);
        }
        let literal = netloc
            .rsplit_once(':')
            .and_then(|(host, _)| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some();
        for addr in &addrs {
            if is_public_address(addr.ip()) {
                continue;
            }
            let listed = literal
                && self
                    .egress
                    .iter()
                    .filter_map(|origin| netloc_of(origin))
                    .any(|(ip, port)| ip == addr.ip() && port == addr.port());
            if !listed {
                return Err(self.refuse(
                    netloc,
                    format!("{netloc} resolves to {}, which is not a public address", addr.ip()),
                ));
            }
        }
        Ok(addrs)
    }
}

/// Blocking HTTP(S) over `ureq` + rustls. It never follows a redirect (a 3xx is the reply) and connects only
/// to an address it checked as public, the one it resolved (no second lookup).
pub struct UreqTransport {
    agent: ureq::Agent,
    refusals: Refusals,
}

impl UreqTransport {
    /// Public destinations only.
    pub fn new() -> Self {
        Self::build(Vec::new(), true)
    }

    /// Public destinations, plus the IP-literal origins listed in `egress` (the program's declared egress).
    pub fn with_egress(egress: Vec<String>) -> Self {
        Self::build(egress, true)
    }

    /// No address check: for a host whose providers deliberately talk to local services (and for tests that
    /// run a loopback server). Redirects are still never followed.
    pub fn unchecked() -> Self {
        Self::build(Vec::new(), false)
    }

    fn build(egress: Vec<String>, checked: bool) -> Self {
        let refusals: Refusals = Arc::default();
        let resolver = CheckedResolver {
            egress,
            checked,
            refusals: Arc::clone(&refusals),
        };
        Self {
            agent: ureq::AgentBuilder::new().redirects(0).resolver(resolver).build(),
            refusals,
        }
    }

    fn refusal_for(&self, url: &str) -> Option<String> {
        let parsed = url::Url::parse(url).ok()?;
        let netloc = format!("{}:{}", parsed.host_str()?, parsed.port_or_known_default()?);
        self.refusals.lock().ok()?.remove(&netloc)
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_headers(response: &ureq::Response) -> Vec<(String, String)> {
    response
        .headers_names()
        .into_iter()
        .filter_map(|name| {
            response
                .header(&name)
                .map(|v| (name.clone(), v.to_string()))
        })
        .collect()
}

impl UreqTransport {
    fn request(&self, request: &HttpRequest) -> Result<ureq::Response, TransportError> {
        let mut call = self.agent.request(&request.method, &request.url);
        if let Some(timeout) = request.timeout {
            call = call.timeout(timeout);
        }
        for (name, value) in &request.headers {
            call = call.set(name, value);
        }
        let outcome = call.send_bytes(&request.body);
        let response = match outcome {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(t)) => {
                if let Some(refusal) = self.refusal_for(&request.url) {
                    return Err(TransportError::Refused(refusal));
                }
                let text = t.to_string();
                if text.to_ascii_lowercase().contains("timed out") {
                    return Err(TransportError::TimedOut);
                }
                let detail = t.message().unwrap_or("");
                return Err(TransportError::Failed(format!("{}: {detail}", t.kind())));
            }
        };
        Ok(response)
    }
}

impl Transport for UreqTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut body = Vec::new();
        let mut reply = self.stream(request, &mut |chunk| {
            body.extend_from_slice(chunk);
            Ok(())
        })?;
        reply.body = body;
        Ok(reply)
    }

    fn stream(
        &self,
        request: &HttpRequest,
        on_chunk: &mut dyn FnMut(&[u8]) -> Result<(), TransportError>,
    ) -> Result<HttpResponse, TransportError> {
        let response = self.request(request)?;
        let reply = HttpResponse {
            status: response.status(),
            headers: collect_headers(&response),
            body: Vec::new(),
        };
        let mut reader = response.into_reader();
        let mut bytes = [0u8; 8192];
        loop {
            let count = match reader.read(&mut bytes) {
                Ok(0) => return Ok(reply),
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => return Err(TransportError::TimedOut),
                Err(error) => return Err(TransportError::Failed(error.to_string())),
            };
            on_chunk(&bytes[..count])?;
        }
    }
}

/// The host a sign-in verifier fetches its keys through: a native host that does nothing else, over a transport that connects
/// to public addresses and to the IP-literal origins in `allowed` only. It is the verifier's, not the program's: the
/// program's `egress` neither widens nor narrows it.
pub fn feed_host(allowed: &[String]) -> crate::auth::HostAnswer {
    use crate::credentials::Credentials;
    use crate::entity_source::InMemoryEntityCache;
    use crate::native_host::NativeHost;
    use crate::os_host::{OsGrants, OsHandles};
    let host = NativeHost::new(
        Arc::new(InMemoryEntityCache::new()),
        Arc::new(UreqTransport::with_egress(allowed.to_vec())),
        Arc::new(Credentials::default()),
        allowed.to_vec(),
        OsGrants::default(),
        Arc::new(OsHandles::default()),
        Arc::new(|_, _| {}),
    );
    Arc::new(move |request| host.answer(request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    fn get(url: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".into(),
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout: Some(Duration::from_secs(5)),
        }
    }

    fn serve(reply: &'static str, connections: usize) -> (u16, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        listener.set_nonblocking(true).expect("nonblocking");
        let handle = thread::spawn(move || {
            let mut served = 0;
            let deadline = std::time::Instant::now() + Duration::from_millis(1500);
            while served < connections && std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).expect("blocking");
                        let mut buf = [0u8; 2048];
                        let _ = stream.read(&mut buf);
                        stream.write_all(reply.as_bytes()).expect("write");
                        served += 1;
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
            served
        });
        (port, handle)
    }

    #[test]
    fn http_stream_routes_a_frame_before_the_server_closes() {
        use crate::http_request::StreamFraming;
        use crate::resumable::{answer_http, HttpHost, IoAnswer, IoRequest};
        use std::sync::mpsc;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        let (seen, receive) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0; 2048];
            stream.read(&mut request).expect("request");
            stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\nD\r\ndata: first\n\n\r\n").expect("first frame");
            let before_eof = receive.recv_timeout(Duration::from_secs(2)).is_ok();
            stream.write_all(b"B\r\ndata: final\r\n0\r\n\r\n").expect("tail");
            before_eof
        });
        let transport = UreqTransport::unchecked();
        let secrets = crate::credentials::Credentials::default();
        let egress = vec![origin.clone()];
        let events = Mutex::new(Vec::new());
        let on_event = |_: Option<&crate::resumable::CallSite>, event| {
            events.lock().expect("events").push(event);
            let _ = seen.send(());
        };
        let host = HttpHost { transport: &transport, secrets: &secrets, egress: &egress, on_event: &on_event, os: None };
        let ask = IoRequest::HttpStream { request: get(&origin), key: Some("trace".into()), framing: StreamFraming::Sse, site: None, continuation: None };
        let IoAnswer::Http(Ok(reply)) = answer_http(Some(&host), &ask) else { panic!("expected response") };
        assert!(server.join().expect("server"), "the first frame must be delivered before EOF");
        assert_eq!(reply.status, 200);
        assert_eq!(reply.header("content-type"), Some("text/event-stream"));
        assert!(reply.body.is_empty());
        let events = events.lock().expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(serde_json::to_value(&events[0]).expect("event"), serde_json::json!({"data": "first", "event": null, "id": null, "key": "trace"}));
        assert_eq!(serde_json::to_value(&events[1]).expect("event"), serde_json::json!({"data": "final", "event": null, "id": null, "key": "trace"}));
    }

    #[test]
    fn native_stream_defers_dispatch_and_cancel_closes_on_the_next_heartbeat() {
        use crate::driver::{DriverAsk, DriverStep};
        use crate::http_request::{parse_http_call, HttpCallDriver, HttpMethod, StreamFraming};
        use crate::native_host::NativeHost;
        use crate::os_host::{OsGrants, OsHandles};
        use crate::os_host::completion::OsCompletionSink;
        use crate::resumable::{CallRoutes, CallSite, IoAnswer, IoRequest};
        use orbital_core_base::value::Value;
        use std::sync::mpsc;

        struct Sink(Mutex<mpsc::Sender<&'static str>>);
        impl OsCompletionSink for Sink {
            fn complete(&self, _: &CallSite, _: &str, _: Result<Value, orbital_core_base::os::OsError>) {}
            fn message(&self, _: &CallSite, _: &str, _: Value) { self.0.lock().expect("sink").send("message").expect("message"); }
            fn settle(&self, _: &CallSite, _: &str, _: Result<Value, Value>) { self.0.lock().expect("sink").send("settled").expect("settled"); }
        }
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        let (heartbeat, wait) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_read_timeout(Some(Duration::from_secs(2))).expect("timeout");
            let mut buffer = [0; 2048];
            stream.read(&mut buffer).expect("request");
            stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: first\n\n").expect("frame");
            wait.recv_timeout(Duration::from_secs(2)).expect("cancel before heartbeat");
            stream.write_all(b": hb\n\n").expect("heartbeat");
            matches!(stream.read(&mut buffer), Ok(0))
        });
        let handles = Arc::new(OsHandles::default());
        let (message, events) = mpsc::channel();
        handles.completions.set_sink(Arc::new(Sink(Mutex::new(message))));
        let egress = vec![origin.clone()];
        let host = NativeHost::new(Arc::new(crate::entity_source::InMemoryEntityCache::new()), Arc::new(UreqTransport::unchecked()), Arc::new(crate::credentials::Credentials::default()), egress.clone(), OsGrants::default(), handles, Arc::new(|_, _| {}));
        let spec: Value = serde_json::from_value(serde_json::json!({"stream": "sse"})).expect("spec");
        let spec = parse_http_call(HttpMethod::Get, &origin, "/trace", &spec).expect("parse");
        let (driver, step) = HttpCallDriver::start(spec, egress, Some("live".into()));
        let DriverStep::Ask(DriverAsk::HttpStream { request, .. }) = step else { panic!("stream") };
        let site = CallSite { orbital: None, trait_name: "Viewer".into(), follow_on: false, entity_id: "singleton".into(), emit: CallRoutes { on_message: Some("FRAME".into()), success: Some("DONE".into()), ..Default::default() } };
        let ask = IoRequest::HttpStream { request, key: Some("live".into()), framing: StreamFraming::Sse, site: Some(site), continuation: Some(Box::new(driver)) };
        assert!(matches!(host.answer(&ask), IoAnswer::ServiceDeferred));
        assert_eq!(events.recv_timeout(Duration::from_secs(2)).expect("first frame"), "message");
        assert!(matches!(host.answer(&IoRequest::CancelCall { key: "live".into() }), IoAnswer::Cancelled(Ok(()))));
        heartbeat.send(()).expect("heartbeat");
        assert!(server.join().expect("server"), "the reader must close the socket on the next heartbeat");
        assert!(events.try_recv().is_err(), "cancelled streams emit neither another message nor success");
    }

    #[test]
    fn stream_consumer_error_stops_the_reader_without_waiting_for_eof() {
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let (finished, receive) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0; 2048];
            stream.read(&mut request).expect("request");
            stream.write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\nfirst").expect("body");
            receive.recv_timeout(Duration::from_secs(2)).is_ok()
        });
        let expected = TransportError::Failed("consumer closed".into());
        let outcome = UreqTransport::unchecked().stream(&get(&url), &mut |_| Err(expected.clone()));
        finished.send(()).expect("finished");
        assert!(matches!(outcome, Err(error) if error == expected));
        assert!(server.join().expect("server"));
    }

    #[test]
    fn stream_read_error_is_reported_after_delivering_received_bytes() {
        let (port, server) = serve("HTTP/1.1 200 OK\r\ncontent-length: 100\r\nconnection: close\r\n\r\npartial", 1);
        let mut body = Vec::new();
        let outcome = UreqTransport::unchecked().stream(&get(&format!("http://127.0.0.1:{port}")), &mut |bytes| {
            body.extend_from_slice(bytes);
            Ok(())
        });
        assert_eq!(body, b"partial");
        assert!(matches!(outcome, Err(TransportError::Failed(_))), "{outcome:?}");
        server.join().expect("server");
    }

    const REDIRECT: &str = "HTTP/1.1 302 Found\r\nlocation: /elsewhere\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    const OK: &str = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";

    #[test]
    fn a_redirect_is_the_reply_and_is_not_followed() {
        let (port, server) = serve(REDIRECT, 2);
        let transport = UreqTransport::with_egress(vec![format!("http://127.0.0.1:{port}")]);
        let reply = transport.send(&get(&format!("http://127.0.0.1:{port}/start"))).expect("reply");
        assert_eq!(reply.status, 302);
        assert_eq!(reply.header("location"), Some("/elsewhere"));
        assert_eq!(server.join().expect("server"), 1, "the Location was not requested");
    }

    #[test]
    fn control_the_unchecked_transport_returns_a_plain_reply() {
        let (port, server) = serve(OK, 1);
        let reply = UreqTransport::unchecked()
            .send(&get(&format!("http://127.0.0.1:{port}/x")))
            .expect("reply");
        assert_eq!((reply.status, reply.text().as_ref()), (200, "ok"));
        server.join().expect("server");
    }

    #[test]
    fn a_hostname_resolving_to_loopback_is_refused_and_never_connected() {
        let (port, server) = serve(OK, 1);
        let outcome = UreqTransport::new().send(&get(&format!("http://localhost:{port}/x")));
        assert!(matches!(outcome, Err(TransportError::Refused(_))), "{outcome:?}");
        assert_eq!(server.join().expect("server"), 0);
    }

    #[test]
    fn a_loopback_literal_is_refused_unless_egress_lists_it() {
        let (port, server) = serve(OK, 1);
        let url = format!("http://127.0.0.1:{port}/x");
        assert!(matches!(UreqTransport::new().send(&get(&url)), Err(TransportError::Refused(_))));
        let other = UreqTransport::with_egress(vec!["http://127.0.0.1:1".to_string()]);
        assert!(matches!(other.send(&get(&url)), Err(TransportError::Refused(_))));
        let listed = UreqTransport::with_egress(vec![format!("http://127.0.0.1:{port}")]);
        assert_eq!(listed.send(&get(&url)).expect("reply").status, 200);
        server.join().expect("server");
    }

    #[test]
    fn a_listed_hostname_is_still_checked() {
        let (port, server) = serve(OK, 1);
        let transport = UreqTransport::with_egress(vec![format!("http://localhost:{port}")]);
        let outcome = transport.send(&get(&format!("http://localhost:{port}/x")));
        assert!(matches!(outcome, Err(TransportError::Refused(_))), "{outcome:?}");
        assert_eq!(server.join().expect("server"), 0);
    }
}
