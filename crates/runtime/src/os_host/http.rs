//! `http/listen` and `http/respond`: HTTP/1.1 over the `os/tcp-listen` stream. Each accepted connection is read
//! on a thread of its own and parsed with `httparse`; a complete request is held in the socket table as a
//! [`SockEntry::Request`] and delivered on the call's `onMessage` route as
//! `{ requestId, method, path, query, headers, body, peer }`. `http/respond` answers it once and closes the
//! connection. The host never routes: a head it cannot parse is answered 400, a body framed without
//! `content-length` 411, an oversized head 431 and an oversized body 413, each closed and never delivered.

use super::args::{arg_i64, arg_str};
use super::completion::Completions;
use super::net::{err, io_error, object, SockEntry, MAX_RECV};
use super::{HandleSlab, OsHost};
use crate::resumable::CallSite;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::value::{Value, ValueMap};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::Arc;

const MAX_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 100;
/// Header names whose framing the host writes itself.
const HOST_FRAMING: &[&str] = &["content-length", "transfer-encoding", "connection"];

struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Why a connection ends without reaching the program: answered with a status, or gone before a full request.
enum Unread {
    Refuse(u16),
    Gone,
}

/// The [`super::net::OnAccept`] of `http/listen`: reads the request off the accept loop so a slow client never
/// holds back the next accept.
pub(super) fn serve(
    stream: TcpStream,
    peer: SocketAddr,
    socks: &Arc<HandleSlab<SockEntry>>,
    completions: &Arc<Completions>,
    key: &str,
    site: &CallSite,
) -> bool {
    let (socks, completions, owned_key, site) = (
        socks.clone(),
        completions.clone(),
        key.to_string(),
        site.clone(),
    );
    let spawned = std::thread::Builder::new()
        .name(format!("http-request-{key}"))
        .spawn(move || read_and_deliver(stream, peer, &socks, &completions, &owned_key, &site));
    if let Err(e) = spawned {
        tracing::warn!(key, error = %e, "http/listen: no thread to read a request on; connection dropped");
    }
    true
}

fn read_and_deliver(
    mut stream: TcpStream,
    peer: SocketAddr,
    socks: &HandleSlab<SockEntry>,
    completions: &Completions,
    key: &str,
    site: &CallSite,
) {
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(Unread::Refuse(status)) => {
            let _ = write_response(&mut stream, status, &[], b"");
            return;
        }
        Err(Unread::Gone) => return,
    };
    let (path, query) = match request.target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (request.target.clone(), String::new()),
    };
    let mut headers = ValueMap::new();
    for (name, value) in request.headers {
        let joined = match headers.get(&name) {
            Some(Value::String(prev)) => format!("{prev}, {value}"),
            _ => value,
        };
        headers.insert(name, Value::String(joined));
    }
    let id = socks.insert(SockEntry::Request(stream));
    let event = object([
        ("requestId", Value::Number(id as f64)),
        ("method", Value::String(request.method)),
        ("path", Value::String(path)),
        ("query", Value::String(query)),
        ("headers", Value::Object(headers)),
        (
            "body",
            Value::String(String::from_utf8_lossy(&request.body).into_owned()),
        ),
        ("peer", Value::String(peer.to_string())),
    ]);
    if !completions.message(key, site, event) {
        socks.remove(id);
    }
}

fn read_request(stream: &mut TcpStream) -> Result<Request, Unread> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let (head_len, method, target, headers) = loop {
        let read = stream.read(&mut chunk).map_err(|_| Unread::Gone)?;
        if read == 0 {
            return Err(Unread::Gone);
        }
        buf.extend_from_slice(&chunk[..read]);
        let mut slots = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut parsed = httparse::Request::new(&mut slots);
        match parsed.parse(&buf) {
            Ok(httparse::Status::Complete(head_len)) => {
                let (Some(method), Some(target)) = (parsed.method, parsed.path) else {
                    return Err(Unread::Refuse(400));
                };
                let mut headers = Vec::with_capacity(parsed.headers.len());
                for header in parsed.headers.iter() {
                    let value =
                        std::str::from_utf8(header.value).map_err(|_| Unread::Refuse(400))?;
                    headers.push((header.name.to_ascii_lowercase(), value.to_string()));
                }
                break (head_len, method.to_string(), target.to_string(), headers);
            }
            Ok(httparse::Status::Partial) if buf.len() > MAX_HEAD => {
                return Err(Unread::Refuse(431))
            }
            Ok(httparse::Status::Partial) => {}
            Err(httparse::Error::TooManyHeaders) => return Err(Unread::Refuse(431)),
            Err(_) => return Err(Unread::Refuse(400)),
        }
    };
    if headers.iter().any(|(name, _)| name == "transfer-encoding") {
        return Err(Unread::Refuse(411));
    }
    let mut lengths = headers
        .iter()
        .filter(|(name, _)| name == "content-length")
        .map(|(_, v)| v.trim());
    let length = match lengths.next() {
        None => 0,
        Some(first) => {
            if lengths.any(|other| other != first) {
                return Err(Unread::Refuse(400));
            }
            first.parse::<usize>().map_err(|_| Unread::Refuse(400))?
        }
    };
    if length > MAX_RECV {
        return Err(Unread::Refuse(413));
    }
    let mut body = buf.split_off(head_len);
    while body.len() < length {
        let read = stream.read(&mut chunk).map_err(|_| Unread::Gone)?;
        if read == 0 {
            return Err(Unread::Gone);
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Ok(Request {
        method,
        target,
        headers,
        body,
    })
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> std::io::Result<usize> {
    let reason = http::StatusCode::from_u16(status)
        .ok()
        .and_then(|code| code.canonical_reason())
        .unwrap_or("");
    let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    if !matches!(status, 204 | 304) {
        out.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    out.push_str("connection: close\r\n\r\n");
    let mut wire = out.into_bytes();
    wire.extend_from_slice(body);
    stream.write_all(&wire)?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Write);
    Ok(wire.len())
}

fn header_args(args: &[Value], op: OsOp) -> Result<Vec<(String, String)>, OsError> {
    let Some(Value::Object(map)) = args.get(2) else {
        return Err(err(
            op,
            OsErrorKind::Invalid,
            "argument 3 must be an object of header names to strings",
        ));
    };
    let mut headers = Vec::with_capacity(map.len());
    for (name, value) in map {
        let Value::String(value) = value else {
            return Err(err(
                op,
                OsErrorKind::Invalid,
                format!("header {name} must be a string"),
            ));
        };
        let name = http::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            err(
                op,
                OsErrorKind::Invalid,
                format!("{name} is not a header name"),
            )
        })?;
        if HOST_FRAMING.contains(&name.as_str()) {
            return Err(err(
                op,
                OsErrorKind::Invalid,
                format!("{name} is written by the host"),
            ));
        }
        http::HeaderValue::from_str(value).map_err(|_| {
            err(
                op,
                OsErrorKind::Invalid,
                format!("header {name} has a value that is not allowed"),
            )
        })?;
        headers.push((name.as_str().to_string(), value.clone()));
    }
    Ok(headers)
}

/// `http/respond requestId status headers body`: everything is checked before the request is taken, so a refused
/// answer leaves it pending.
pub(super) fn respond(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::HttpRespond;
    let id = arg_i64(args, 0, op)?;
    let status = u16::try_from(arg_i64(args, 1, op)?)
        .ok()
        .filter(|status| (200..=599).contains(status))
        .ok_or_else(|| err(op, OsErrorKind::Invalid, "the status must be 200 to 599"))?;
    let headers = header_args(args, op)?;
    let body = arg_str(args, 3, op)?;
    if matches!(status, 204 | 304) && !body.is_empty() {
        return Err(err(
            op,
            OsErrorKind::Invalid,
            format!("a {status} reply has no body"),
        ));
    }
    let socks = &host.handles.net.socks;
    match socks.with(id, |entry| matches!(entry, SockEntry::Request(_))) {
        None => {
            return Err(err(
                op,
                OsErrorKind::Closed,
                "no pending request has this id",
            ))
        }
        Some(false) => return Err(err(op, OsErrorKind::Invalid, "not an http request")),
        Some(true) => {}
    }
    let Some(SockEntry::Request(mut stream)) = socks.remove(id) else {
        return Err(err(
            op,
            OsErrorKind::Closed,
            "no pending request has this id",
        ));
    };
    let written = write_response(&mut stream, status, &headers, body.as_bytes())
        .map_err(|e| io_error(op, &e))?;
    Ok(object([("bytes", Value::Number(written as f64))]))
}
