//! The native host's [`CredentialTransport`]: one form POST to an https token endpoint over the kernel's sans-I/O TLS
//! stream on a plain `TcpStream`. The endpoint's origin must be in the program's declared `egress`, every address it
//! resolves to must be public, and a redirect is a failure. No error text carries a header, a body or a token.
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use orbital_core_base::codec;
use orbital_core_base::http_wire::{check_header, is_public_address, origin_allowed, split_url};
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::secret::Secret;
use orbital_core_base::value::{Value, ValueMap};

use crate::credential_store::{CredentialTransport, TransportError};
use crate::driver::{DriverAnswer, DriverAsk};
use crate::os_host::net::{outbound, CONNECT_TIMEOUT, IO_TIMEOUT};
use crate::tls::{TlsEvent, TlsStream};

/// The `egress` origins the programs declare, read at each POST (a program may register after the transport is made).
pub type EgressSource = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

pub struct TlsFormTransport {
    egress: EgressSource,
}

impl TlsFormTransport {
    pub fn new(egress: EgressSource) -> Self {
        Self { egress }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Target {
    host: String,
    port: u16,
    path: String,
}

fn refuse(message: impl Into<String>) -> TransportError {
    TransportError(message.into())
}

fn target_of(endpoint: &str, egress: &[String]) -> Result<Target, TransportError> {
    let Some((origin, rest)) = split_url(endpoint) else {
        return Err(refuse("the token endpoint is not an http(s) url"));
    };
    if origin.scheme() != "https" {
        return Err(refuse("the token endpoint must be https"));
    }
    if !origin_allowed(endpoint, egress) {
        return Err(refuse(format!(
            "{}:{} is not an origin the program declares in egress",
            origin.host(),
            origin.port()
        )));
    }
    let path = if rest.is_empty() {
        "/".to_string()
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        format!("/{rest}")
    };
    if path.contains(['#', ' ', '\r', '\n']) {
        return Err(refuse(
            "the token endpoint path is not a valid request target",
        ));
    }
    Ok(Target {
        host: origin.host().to_string(),
        port: origin.port(),
        path,
    })
}

fn public_addresses(target: &Target) -> Result<Vec<SocketAddr>, TransportError> {
    let bare = target.host.trim_start_matches('[').trim_end_matches(']');
    let addrs = outbound(OsOp::TcpConnect, bare, target.port).map_err(|e| refuse(e.message))?;
    match addrs.iter().find(|a| !is_public_address(a.ip())) {
        Some(a) => Err(refuse(format!(
            "{} resolves to {}, not a public address",
            target.host,
            a.ip()
        ))),
        None => Ok(addrs),
    }
}

fn request_bytes(
    target: &Target,
    headers: &[(String, String)],
    body: &str,
) -> Result<Vec<u8>, TransportError> {
    let mut head = format!("POST {} HTTP/1.1\r\n", target.path);
    let authority = if target.port == 443 {
        target.host.clone()
    } else {
        format!("{}:{}", target.host, target.port)
    };
    head.push_str(&format!("Host: {authority}\r\n"));
    for (name, value) in headers {
        check_header(name, value).map_err(|_| refuse("a request header is not valid"))?;
        if ["host", "content-length", "connection", "transfer-encoding"]
            .iter()
            .any(|owned| name.eq_ignore_ascii_case(owned))
        {
            return Err(refuse(format!(
                "the request header `{name}` belongs to the transport"
            )));
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    Ok(bytes)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn decode_chunked(mut rest: &[u8]) -> Result<Option<Vec<u8>>, TransportError> {
    let mut body = Vec::new();
    loop {
        let Some(line_end) = find(rest, b"\r\n") else {
            return Ok(None);
        };
        let size_text = std::str::from_utf8(&rest[..line_end])
            .map_err(|_| refuse("the response chunk size is not text"))?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| refuse("the response chunk size is not a number"))?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok(find(rest, b"\r\n").map(|_| body));
        }
        if rest.len() < size + 2 {
            return Ok(None);
        }
        body.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// `None` while the response is incomplete; a redirect is refused as soon as its status line is complete.
fn parse_response(raw: &[u8]) -> Result<Option<(u16, String)>, TransportError> {
    let Some(head_end) = find(raw, b"\r\n\r\n") else {
        if let Some(line_end) = find(raw, b"\r\n") {
            status_of(&raw[..line_end])?;
        }
        return Ok(None);
    };
    let head = std::str::from_utf8(&raw[..head_end])
        .map_err(|_| refuse("the response head is not text"))?;
    let mut lines = head.split("\r\n");
    let status = status_of(lines.next().unwrap_or_default().as_bytes())?;
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(refuse("the response has a malformed header"));
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| refuse("the response content-length is not a number"))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.eq_ignore_ascii_case("chunked")
        {
            chunked = true;
        }
    }
    let rest = &raw[head_end + 4..];
    let body = if chunked {
        decode_chunked(rest)?
    } else {
        match length {
            Some(n) if rest.len() >= n => Some(rest[..n].to_vec()),
            Some(_) => None,
            None => None,
        }
    };
    body.map(|bytes| {
        String::from_utf8(bytes)
            .map(|text| (status, text))
            .map_err(|_| refuse("the response body is not text"))
    })
    .transpose()
}

fn status_of(line: &[u8]) -> Result<u16, TransportError> {
    let text =
        std::str::from_utf8(line).map_err(|_| refuse("the response status line is not text"))?;
    let mut parts = text.split(' ');
    let status = match (parts.next(), parts.next().map(str::parse::<u16>)) {
        (Some(version), Some(Ok(code))) if version.starts_with("HTTP/1.") => code,
        _ => return Err(refuse("the response status line is malformed")),
    };
    if (300..400).contains(&status) {
        return Err(refuse(format!(
            "the token endpoint answered a redirect ({status}); redirects are not followed"
        )));
    }
    Ok(status)
}

fn io_failure(op: OsOp, error: &std::io::Error) -> OsError {
    let kind = match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => OsErrorKind::Timeout,
        std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::BrokenPipe => OsErrorKind::Closed,
        _ => OsErrorKind::Io,
    };
    OsError::new(kind, format!("os/{}: {error}", op.name()))
}

fn answer(stream: &mut TcpStream, ask: DriverAsk) -> DriverAnswer {
    match ask {
        DriverAsk::Random { bytes } => {
            let mut buf = vec![0u8; bytes as usize];
            DriverAnswer::Random(
                getrandom::getrandom(&mut buf)
                    .map(|()| Secret::new(buf))
                    .map_err(|e| format!("random: {e}")),
            )
        }
        DriverAsk::Clock => DriverAnswer::Clock(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| format!("clock: {e}"))
                .and_then(|elapsed| {
                    i64::try_from(elapsed.as_millis()).map_err(|e| format!("clock: {e}"))
                }),
        ),
        DriverAsk::Os {
            op: OsOp::Send,
            args,
            ..
        } => DriverAnswer::Os(match args.get(1) {
            Some(Value::String(text)) => codec::base64_decode(text)
                .map_err(|m| OsError::new(OsErrorKind::Invalid, m))
                .and_then(|bytes| {
                    stream
                        .write_all(&bytes)
                        .map(|()| Value::Null)
                        .map_err(|e| io_failure(OsOp::Send, &e))
                }),
            _ => Err(OsError::new(
                OsErrorKind::Invalid,
                "os/send: the bytes must be base64 text",
            )),
        }),
        DriverAsk::Os {
            op: OsOp::Recv,
            args,
            ..
        } => DriverAnswer::Os(match args.get(1) {
            Some(Value::Number(count)) if *count >= 1.0 => {
                let mut buf = vec![0u8; *count as usize];
                stream
                    .read(&mut buf)
                    .map_err(|e| io_failure(OsOp::Recv, &e))
                    .map(|n| {
                        let mut fields = ValueMap::default();
                        fields.insert(
                            "data".to_string().into(),
                            Value::string(codec::base64_encode(&buf[..n])),
                        );
                        fields.insert("eof".to_string().into(), Value::Bool(n == 0));
                        Value::object(fields)
                    })
            }
            _ => Err(OsError::new(
                OsErrorKind::Invalid,
                "os/recv: the count must be a positive integer",
            )),
        }),
        DriverAsk::Os {
            op: OsOp::Close, ..
        } => {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            DriverAnswer::Os(Ok(Value::Null))
        }
        _ => DriverAnswer::Os(Err(OsError::new(
            OsErrorKind::Unsupported,
            "the token exchange answers only TLS traffic asks",
        ))),
    }
}

fn settle(
    tls: &mut TlsStream,
    stream: &mut TcpStream,
    mut event: TlsEvent,
) -> Result<TlsEvent, TransportError> {
    loop {
        match event {
            TlsEvent::Ask(ask) => event = tls.resume(answer(stream, ask)),
            TlsEvent::Failed(message) => return Err(refuse(format!("tls: {message}"))),
            other => return Ok(other),
        }
    }
}

fn exchange(
    target: &Target,
    addrs: Vec<SocketAddr>,
    request: &[u8],
) -> Result<(u16, String), TransportError> {
    let mut stream = None;
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let Some(mut stream) = stream else {
        return Err(refuse(match last {
            Some(e) => format!("could not connect to {}: {e}", target.host),
            None => format!("{} has no addresses", target.host),
        }));
    };
    let timeouts = stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)));
    timeouts.map_err(|e| refuse(format!("socket: {e}")))?;
    let server_name = target.host.trim_start_matches('[').trim_end_matches(']');
    let (mut tls, first) = TlsStream::upgrade(server_name, Value::Number(0.0));
    if !matches!(settle(&mut tls, &mut stream, first)?, TlsEvent::Connected) {
        return Err(refuse("tls: the handshake did not complete"));
    }
    let sent = tls.send(request);
    if !matches!(settle(&mut tls, &mut stream, sent)?, TlsEvent::Sent) {
        return Err(refuse("tls: the request was not sent"));
    }
    let mut raw = Vec::new();
    loop {
        let next = tls.recv();
        match settle(&mut tls, &mut stream, next)? {
            TlsEvent::Data(bytes) => {
                raw.extend_from_slice(&bytes);
                if let Some(done) = parse_response(&raw)? {
                    return Ok(done);
                }
            }
            TlsEvent::Eof => {
                return match parse_response(&raw)? {
                    Some(done) => Ok(done),
                    None => Err(refuse(
                        "the token endpoint closed before a complete response",
                    )),
                };
            }
            _ => {
                return Err(refuse(
                    "tls: an unexpected event while reading the response",
                ))
            }
        }
    }
}

impl CredentialTransport for TlsFormTransport {
    fn post_form(
        &self,
        endpoint: &str,
        headers: &[(String, String)],
        body: &str,
    ) -> Result<(u16, String), TransportError> {
        let target = target_of(endpoint, &(self.egress)())?;
        let request = request_bytes(&target, headers, body)?;
        let addrs = public_addresses(&target)?;
        exchange(&target, addrs, &request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn egress(origins: &[&str]) -> EgressSource {
        let owned: Vec<String> = origins.iter().map(|o| o.to_string()).collect();
        Arc::new(move || owned.clone())
    }

    fn post(origins: &[&str], endpoint: &str) -> Result<(u16, String), TransportError> {
        TlsFormTransport::new(egress(origins)).post_form(endpoint, &[], "grant_type=refresh_token")
    }

    fn refused(result: Result<(u16, String), TransportError>) -> String {
        result.expect_err("the transport must refuse").0
    }

    #[test]
    fn an_http_endpoint_is_refused_even_when_declared() {
        assert!(refused(post(
            &["http://auth.example.com"],
            "http://auth.example.com/token"
        ))
        .contains("must be https"));
    }

    #[test]
    fn an_origin_the_program_does_not_declare_is_refused() {
        assert!(refused(post(
            &["https://other.example.com"],
            "https://auth.example.com/token"
        ))
        .contains("egress"));
        assert!(refused(post(&[], "https://auth.example.com/token")).contains("egress"));
    }

    #[test]
    fn a_declared_origin_at_another_port_is_refused() {
        assert!(refused(post(
            &["https://auth.example.com"],
            "https://auth.example.com:8443/token"
        ))
        .contains("egress"));
    }

    #[test]
    fn a_declared_private_address_is_refused_before_any_connection() {
        for literal in [
            "https://10.0.0.1",
            "https://127.0.0.1",
            "https://[::1]",
            "https://169.254.169.254",
        ] {
            let message = refused(post(&[literal], &format!("{literal}/token")));
            assert!(
                message.contains("not a public address"),
                "{literal}: {message}"
            );
        }
    }

    #[test]
    fn a_name_that_resolves_to_loopback_is_refused() {
        assert!(
            refused(post(&["https://localhost"], "https://localhost/token"))
                .contains("not a public address")
        );
    }

    #[test]
    fn control_a_declared_https_origin_yields_its_target_with_the_default_port() {
        let target = target_of(
            "https://auth.example.com/oauth/token?x=1",
            &["https://auth.example.com".to_string()],
        )
        .unwrap();
        assert_eq!(
            target,
            Target {
                host: "auth.example.com".into(),
                port: 443,
                path: "/oauth/token?x=1".into()
            }
        );
    }

    #[test]
    fn a_header_the_transport_owns_is_refused() {
        let target = Target {
            host: "a.example".into(),
            port: 443,
            path: "/t".into(),
        };
        let owned = [("Content-Length".to_string(), "1".to_string())];
        assert!(request_bytes(&target, &owned, "")
            .unwrap_err()
            .0
            .contains("belongs to the transport"));
        let bad = [("X".to_string(), "a\r\nb".to_string())];
        assert!(request_bytes(&target, &bad, "").is_err());
    }

    #[test]
    fn a_redirect_is_a_transport_failure_that_names_no_location() {
        let raw = b"HTTP/1.1 302 Found\r\nLocation: https://evil.example/steal\r\n\r\n";
        let message = parse_response(raw).unwrap_err().0;
        assert!(
            message.contains("redirect (302)") && !message.contains("evil"),
            "{message}"
        );
        assert!(parse_response(b"HTTP/1.1 307 Temporary Redirect\r\n").is_err());
    }

    #[test]
    fn control_a_content_length_and_a_chunked_response_both_complete() {
        let plain =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(
            parse_response(plain).unwrap(),
            Some((200, "{}".to_string()))
        );
        let chunked = b"HTTP/1.1 400 Bad Request\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nabcd\r\n2\r\nef\r\n0\r\n\r\n";
        assert_eq!(
            parse_response(chunked).unwrap(),
            Some((400, "abcdef".to_string()))
        );
    }

    #[test]
    fn an_incomplete_response_waits_for_more_bytes() {
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nab").unwrap(),
            None
        );
        assert_eq!(parse_response(b"HTTP/1.1 200").unwrap(), None);
        assert!(parse_response(b"garbage\r\n\r\n").is_err());
    }
}
