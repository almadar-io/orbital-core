//! The `redis` effect's protocol, owned by the kernel: a RESP2 client (RESP3 replies decoded where they map onto the
//! reply union) over the shared sans-I/O driver loop. One call is one connection: connect (TLS for `rediss://`),
//! `AUTH` and `SELECT` as one pipeline whose replies are all checked before the command is sent, the command, then
//! close. Every ask is a journaled `os/*` ask of [`ByteStream`], so a replay of the journal is a replay of the call.

use orbital_core_base::emit_contract::REDIS_REPLY_DEPTH;
use orbital_core_base::http_wire::{parse_slot_object, whole_number, CredentialSlot, HttpRefusal};
use orbital_core_base::value::{Value, ValueMap};

use crate::byte_stream::{self, ByteStream, FailureKind, StreamEvent, StreamFailure};
use crate::driver::{Driver, DriverAnswer, DriverAsk, DriverStep};
use crate::http_request::tcp_target_allowed;

const MAX_BULK: usize = 512 * 1024 * 1024;
const MAX_NESTING: usize = 32;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_992;

/// A parsed `redis` call.
#[derive(Debug, Clone, PartialEq)]
pub struct RedisCallSpec {
    pub origin: String,
    pub command: Vec<String>,
    pub auth: Option<CredentialSlot>,
    pub user: Option<String>,
    pub db: Option<u32>,
}

fn invalid<T>(message: impl Into<String>) -> Result<T, HttpRefusal> {
    Err(HttpRefusal::Invalid(message.into()))
}

/// The call's `origin`, `command` and connection `options` (`auth`, `user`, `db`) as a [`RedisCallSpec`].
pub fn parse_redis_call(origin: &Value, command: &Value, options: &Value) -> Result<RedisCallSpec, HttpRefusal> {
    let Value::String(origin) = origin else {
        return invalid("redis: the origin must be text");
    };
    let Value::Array(items) = command else {
        return invalid("redis: the command must be a list of text items");
    };
    let mut words = Vec::with_capacity(items.len());
    for (at, item) in items.iter().enumerate() {
        match item {
            Value::String(word) => words.push(word.clone()),
            _ => return invalid(format!("redis: command item {} must be text", at + 1)),
        }
    }
    if words.first().is_none_or(String::is_empty) {
        return invalid("redis: the command must start with a command name");
    }
    let (auth, user, db) = match options {
        Value::Null => (None, None, None),
        Value::Object(map) => parse_options(map)?,
        _ => return invalid("redis: the options must be an object"),
    };
    Ok(RedisCallSpec { origin: origin.clone(), command: words, auth, user, db })
}

type Connection = (Option<CredentialSlot>, Option<String>, Option<u32>);

fn parse_options(map: &ValueMap) -> Result<Connection, HttpRefusal> {
    if let Some(unknown) = map.keys().find(|k| !matches!(k.as_str(), "auth" | "user" | "db")) {
        return invalid(format!("redis: unknown option `{unknown}` (auth, user, db)"));
    }
    let auth = match map.get("auth") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) if !name.is_empty() => Some(CredentialSlot::from(name.as_str())),
        Some(Value::Object(slot)) => Some(parse_slot_object(slot, "redis options.auth")?),
        Some(_) => return invalid("redis: options.auth must be a credential name or { credential, scope }"),
    };
    let user = match map.get("user") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) if !name.is_empty() => Some(name.clone()),
        Some(_) => return invalid("redis: options.user must be a non-empty string"),
    };
    if user.is_some() && auth.is_none() {
        return invalid("redis: options.user needs options.auth");
    }
    let db = match whole_number(map.get("db"), "redis options.db")? {
        None => None,
        Some(n) => Some(u32::try_from(n).or_else(|_| invalid("redis: options.db is out of range"))?),
    };
    Ok((auth, user, db))
}

/// One RESP frame as it came off the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum Resp {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<Resp>>),
    Null,
    Double(f64),
    Boolean(bool),
    BigNumber(String),
    Verbatim(Vec<u8>),
    BlobError(Vec<u8>),
    Map(Vec<(Resp, Resp)>),
    Set(Vec<Resp>),
    Push(Vec<Resp>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Complete(Resp, usize),
    /// The buffer ends inside a frame.
    Incomplete,
    Invalid(String),
}

/// `command` as a RESP array of bulk strings.
pub fn encode_command(command: &[String]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", command.len()).into_bytes();
    for item in command {
        out.extend_from_slice(format!("${}\r\n", item.len()).as_bytes());
        out.extend_from_slice(item.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// The first frame of `buf`.
pub fn parse(buf: &[u8]) -> Parsed {
    match frame(buf, 0, 0) {
        Ok(Some((resp, end))) => Parsed::Complete(resp, end),
        Ok(None) => Parsed::Incomplete,
        Err(message) => Parsed::Invalid(message),
    }
}

type Step = Result<Option<(Resp, usize)>, String>;

fn line(buf: &[u8], from: usize) -> Option<(&[u8], usize)> {
    let rest = buf.get(from..)?;
    let end = rest.windows(2).position(|pair| pair == b"\r\n")?;
    Some((&rest[..end], from + end + 2))
}

fn text(bytes: &[u8], what: &str) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| format!("{what} is not UTF-8"))
}

fn number(bytes: &[u8], what: &str) -> Result<i64, String> {
    text(bytes, what)?.parse::<i64>().map_err(|_| format!("{what} is not an integer"))
}

fn blob(buf: &[u8], from: usize, len: usize) -> Result<Option<(Vec<u8>, usize)>, String> {
    if len > MAX_BULK {
        return Err(format!("a blob of {len} bytes exceeds the {MAX_BULK} byte limit"));
    }
    let end = from + len;
    if buf.len() < end + 2 {
        return Ok(None);
    }
    if &buf[end..end + 2] != b"\r\n" {
        return Err("a blob is not terminated by CRLF".to_string());
    }
    Ok(Some((buf[from..end].to_vec(), end + 2)))
}

fn elements(buf: &[u8], mut at: usize, count: usize, depth: usize) -> Result<Option<(Vec<Resp>, usize)>, String> {
    let mut items = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        match frame(buf, at, depth + 1)? {
            Some((item, end)) => {
                items.push(item);
                at = end;
            }
            None => return Ok(None),
        }
    }
    Ok(Some((items, at)))
}

fn frame(buf: &[u8], at: usize, depth: usize) -> Step {
    if depth > MAX_NESTING {
        return Err(format!("frames are nested deeper than {MAX_NESTING}"));
    }
    let Some(&kind) = buf.get(at) else {
        return Ok(None);
    };
    let Some((head, next)) = line(buf, at + 1) else {
        return Ok(None);
    };
    let count = |what: &str| -> Result<Option<usize>, String> {
        let n = number(head, what)?;
        if n == -1 {
            return Ok(None);
        }
        usize::try_from(n).map(Some).map_err(|_| format!("{what} is negative"))
    };
    Ok(match kind {
        b'+' => Some((Resp::Simple(text(head, "a status reply")?), next)),
        b'-' => Some((Resp::Error(text(head, "an error reply")?), next)),
        b':' => Some((Resp::Integer(number(head, "an integer reply")?), next)),
        b'$' => match count("a bulk length")? {
            None => Some((Resp::Bulk(None), next)),
            Some(len) => blob(buf, next, len)?.map(|(bytes, end)| (Resp::Bulk(Some(bytes)), end)),
        },
        b'*' => match count("an array length")? {
            None => Some((Resp::Array(None), next)),
            Some(n) => elements(buf, next, n, depth)?.map(|(items, end)| (Resp::Array(Some(items)), end)),
        },
        b'_' => Some((Resp::Null, next)),
        b'#' => match head {
            b"t" => Some((Resp::Boolean(true), next)),
            b"f" => Some((Resp::Boolean(false), next)),
            _ => return Err("a boolean reply is not t or f".to_string()),
        },
        b',' => {
            let digits = text(head, "a double reply")?;
            let value = digits.parse::<f64>().map_err(|_| "a double reply is not a number".to_string())?;
            Some((Resp::Double(value), next))
        }
        b'(' => Some((Resp::BigNumber(text(head, "a big number reply")?), next)),
        b'=' => match count("a verbatim length")? {
            None => return Err("a verbatim string has no length".to_string()),
            Some(len) => blob(buf, next, len)?.map(|(bytes, end)| (Resp::Verbatim(bytes), end)),
        },
        b'!' => match count("a blob error length")? {
            None => return Err("a blob error has no length".to_string()),
            Some(len) => blob(buf, next, len)?.map(|(bytes, end)| (Resp::BlobError(bytes), end)),
        },
        b'%' => match count("a map length")? {
            None => return Err("a map has no length".to_string()),
            Some(n) => elements(buf, next, n * 2, depth)?.map(|(items, end)| {
                let mut pairs = Vec::with_capacity(items.len() / 2);
                let mut items = items.into_iter();
                while let (Some(k), Some(v)) = (items.next(), items.next()) {
                    pairs.push((k, v));
                }
                (Resp::Map(pairs), end)
            }),
        },
        b'~' | b'>' => match count("a collection length")? {
            None => return Err("a collection has no length".to_string()),
            Some(n) => elements(buf, next, n, depth)?
                .map(|(items, end)| (if kind == b'~' { Resp::Set(items) } else { Resp::Push(items) }, end)),
        },
        b'|' => match count("an attribute length")? {
            None => return Err("an attribute has no length".to_string()),
            Some(n) => match elements(buf, next, n * 2, depth)? {
                Some((_, end)) => frame(buf, end, depth)?,
                None => None,
            },
        },
        other => return Err(format!("unknown RESP type byte 0x{other:02x}")),
    })
}

/// Why a reply cannot become a reply value.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyProblem {
    /// The server answered an error.
    Server(String),
    /// The reply is valid RESP the reply union cannot hold.
    Unsupported(String),
}

fn integer_value(n: i64) -> Result<Value, ReplyProblem> {
    if n.abs() > MAX_SAFE_INTEGER {
        return Err(ReplyProblem::Unsupported(format!("the integer {n} does not fit a number exactly")));
    }
    Ok(Value::Number(n as f64))
}

fn utf8(bytes: &[u8]) -> Result<String, ReplyProblem> {
    String::from_utf8(bytes.to_vec()).map_err(|_| ReplyProblem::Unsupported("the reply holds bytes that are not UTF-8 text".to_string()))
}

fn list_value(items: &[Resp], depth: usize) -> Result<Value, ReplyProblem> {
    if depth > REDIS_REPLY_DEPTH {
        return Err(ReplyProblem::Unsupported(format!("the reply is nested deeper than {REDIS_REPLY_DEPTH} lists")));
    }
    items.iter().map(|item| reply_value_at(item, depth)).collect::<Result<Vec<_>, _>>().map(Value::Array)
}

fn reply_value_at(resp: &Resp, depth: usize) -> Result<Value, ReplyProblem> {
    match resp {
        Resp::Simple(s) | Resp::BigNumber(s) => Ok(Value::String(s.clone())),
        Resp::Error(message) => Err(ReplyProblem::Server(message.clone())),
        Resp::BlobError(bytes) => Err(ReplyProblem::Server(utf8(bytes)?)),
        Resp::Integer(n) => integer_value(*n),
        Resp::Bulk(None) | Resp::Array(None) | Resp::Null => Ok(Value::Null),
        Resp::Bulk(Some(bytes)) => Ok(Value::String(utf8(bytes)?)),
        Resp::Verbatim(bytes) => {
            let body = bytes.get(4..).ok_or_else(|| ReplyProblem::Unsupported("a verbatim string is shorter than its format prefix".to_string()))?;
            Ok(Value::String(utf8(body)?))
        }
        Resp::Double(d) => Ok(Value::Number(*d)),
        Resp::Boolean(b) => Ok(Value::Number(if *b { 1.0 } else { 0.0 })),
        Resp::Array(Some(items)) | Resp::Set(items) | Resp::Push(items) => list_value(items, depth + 1),
        Resp::Map(pairs) => {
            let flat: Vec<Resp> = pairs.iter().flat_map(|(k, v)| [k.clone(), v.clone()]).collect();
            list_value(&flat, depth + 1)
        }
    }
}

/// The reply as the value the success payload carries: text, a number, `null`, or a list of those nested to
/// [`REDIS_REPLY_DEPTH`] levels. A RESP3 boolean is `1` or `0`, a map its keys and values in one flat list.
pub fn reply_value(resp: &Resp) -> Result<Value, ReplyProblem> {
    reply_value_at(resp, 0)
}

fn object(entries: Vec<(&str, Value)>) -> Value {
    Value::Object(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn key_value(key: &Option<String>) -> Value {
    key.clone().map_or(Value::Null, Value::String)
}

/// A failure payload: `{ error, kind, code, credential, key }`.
pub fn failure_payload(error: &str, kind: &str, code: Option<&str>, credential: Option<&str>, key: &Option<String>) -> Value {
    object(vec![
        ("error", Value::String(error.to_string())),
        ("kind", Value::String(kind.to_string())),
        ("code", code.map_or(Value::Null, |c| Value::String(c.to_string()))),
        ("credential", credential.map_or(Value::Null, |c| Value::String(c.to_string()))),
        ("key", key_value(key)),
    ])
}

/// A call refused before any byte moved: a missing credential is `unconfigured` and names it, the rest `refused`.
pub fn refusal_payload(refusal: &HttpRefusal, key: &Option<String>) -> Value {
    match refusal {
        HttpRefusal::CredentialMissing(name) => failure_payload(&refusal.message(), "unconfigured", None, Some(name), key),
        _ => failure_payload(&refusal.message(), "refused", None, None, key),
    }
}

fn stream_failure_payload(failure: &StreamFailure, key: &Option<String>) -> Value {
    let kind = match failure.kind {
        FailureKind::Refused => "refused",
        FailureKind::Timeout => "timeout",
        FailureKind::Failed => "failed",
    };
    failure_payload(&failure.message, kind, None, None, key)
}

fn problem_payload(problem: &ReplyProblem, key: &Option<String>) -> Value {
    match problem {
        ReplyProblem::Server(message) => {
            let code = message.split_whitespace().next();
            failure_payload(message, "redis", code, None, key)
        }
        ReplyProblem::Unsupported(message) => failure_payload(message, "unsupported", None, None, key),
    }
}

enum Phase {
    Secret,
    Connecting,
    HandshakeSend,
    HandshakeRecv,
    CommandSend,
    CommandRecv,
    Closing(Result<Value, Value>),
}

enum Flow {
    Event(StreamEvent),
    Step(DriverStep),
}

pub struct RedisDriver {
    spec: RedisCallSpec,
    egress: Vec<String>,
    key: Option<String>,
    stream: Option<Box<dyn ByteStream>>,
    password: Option<String>,
    phase: Phase,
    connected: bool,
    inbox: Vec<u8>,
    expected: usize,
    replies: Vec<Resp>,
}

impl Driver for RedisDriver {
    fn resume(&mut self, answer: DriverAnswer) -> DriverStep {
        if matches!(self.phase, Phase::Secret) {
            return self.credential(answer);
        }
        let Some(stream) = self.stream.as_mut() else {
            return self.done_err(failure_payload("redis: an answer arrived with no connection", "failed", None, None, &self.key));
        };
        let event = stream.resume(answer);
        self.run(event)
    }
}

impl RedisDriver {
    /// Begin `spec`; the call is refused at once unless `egress` lists its origin.
    pub fn start(spec: RedisCallSpec, egress: Vec<String>, key: Option<String>) -> (RedisDriver, DriverStep) {
        let mut driver = RedisDriver {
            spec,
            egress,
            key,
            stream: None,
            password: None,
            phase: Phase::Connecting,
            connected: false,
            inbox: Vec::new(),
            expected: 0,
            replies: Vec::new(),
        };
        let step = match tcp_target_allowed(&driver.spec.origin, &driver.egress) {
            Err(refusal) => driver.done_err(refusal_payload(&refusal, &driver.key)),
            Ok(_) => match driver.spec.auth.clone() {
                Some(slot) => {
                    driver.phase = Phase::Secret;
                    DriverStep::Ask(DriverAsk::Secret { name: slot.name, scope: slot.scope })
                }
                None => driver.connect(),
            },
        };
        (driver, step)
    }

    fn done_err(&self, payload: Value) -> DriverStep {
        DriverStep::Done(Err(payload))
    }

    fn credential(&mut self, answer: DriverAnswer) -> DriverStep {
        let name = self.spec.auth.as_ref().map(|slot| slot.name.clone()).unwrap_or_default();
        match answer {
            DriverAnswer::Secret(Ok(Some(password))) => {
                self.password = Some(password);
                self.connect()
            }
            DriverAnswer::Secret(Ok(None)) => self.done_err(refusal_payload(&HttpRefusal::CredentialMissing(name), &self.key)),
            DriverAnswer::Secret(Err(message)) => self.done_err(failure_payload(&message, "failed", None, None, &self.key)),
            other => self.done_err(failure_payload(&format!("redis: unexpected answer {other:?}"), "failed", None, None, &self.key)),
        }
    }

    fn connect(&mut self) -> DriverStep {
        self.phase = Phase::Connecting;
        match byte_stream::open(&self.spec.origin, &self.egress) {
            Ok((stream, event)) => {
                self.stream = Some(stream);
                self.run(event)
            }
            Err(failure) => self.done_err(stream_failure_payload(&failure, &self.key)),
        }
    }

    fn run(&mut self, mut event: StreamEvent) -> DriverStep {
        loop {
            match self.on_event(event) {
                Flow::Step(step) => return step,
                Flow::Event(next) => event = next,
            }
        }
    }

    fn stream(&mut self) -> &mut dyn ByteStream {
        self.stream.as_deref_mut().expect("a stream exists once connecting began")
    }

    fn fail(&mut self, payload: Value) -> Flow {
        if self.connected && !matches!(self.phase, Phase::Closing(_)) {
            self.phase = Phase::Closing(Err(payload));
            self.connected = false;
            return Flow::Event(self.stream().close());
        }
        Flow::Step(DriverStep::Done(Err(payload)))
    }

    fn finish(&mut self, outcome: Result<Value, Value>) -> Flow {
        self.phase = Phase::Closing(outcome);
        self.connected = false;
        Flow::Event(self.stream().close())
    }

    fn handshake(&self) -> Vec<Vec<String>> {
        let mut commands = Vec::new();
        if let Some(password) = &self.password {
            let mut auth = vec!["AUTH".to_string()];
            auth.extend(self.spec.user.clone());
            auth.push(password.clone());
            commands.push(auth);
        }
        if let Some(db) = self.spec.db {
            commands.push(vec!["SELECT".to_string(), db.to_string()]);
        }
        commands
    }

    fn send_commands(&mut self, commands: &[Vec<String>], next: Phase) -> Flow {
        let mut bytes = Vec::new();
        for command in commands {
            bytes.extend(encode_command(command));
        }
        self.phase = next;
        self.expected = commands.len();
        self.replies.clear();
        self.inbox.clear();
        Flow::Event(self.stream().send(&bytes))
    }

    fn on_event(&mut self, event: StreamEvent) -> Flow {
        match event {
            StreamEvent::Ask(ask) => Flow::Step(DriverStep::Ask(ask)),
            StreamEvent::Failed(failure) => {
                let payload = stream_failure_payload(&failure, &self.key);
                match self.phase {
                    Phase::Closing(ref outcome) => Flow::Step(DriverStep::Done(outcome.clone())),
                    _ => self.fail(payload),
                }
            }
            StreamEvent::Connected => {
                self.connected = true;
                let handshake = self.handshake();
                if handshake.is_empty() {
                    let command = self.spec.command.clone();
                    self.send_commands(&[command], Phase::CommandSend)
                } else {
                    self.send_commands(&handshake, Phase::HandshakeSend)
                }
            }
            StreamEvent::Sent => {
                self.phase = match self.phase {
                    Phase::HandshakeSend => Phase::HandshakeRecv,
                    _ => Phase::CommandRecv,
                };
                Flow::Event(self.stream().recv())
            }
            StreamEvent::Data(bytes) => self.on_data(bytes),
            StreamEvent::Eof => match self.phase {
                Phase::Closing(ref outcome) => Flow::Step(DriverStep::Done(outcome.clone())),
                _ => {
                    let payload = failure_payload("redis: the server closed the connection before it replied in full", "failed", None, None, &self.key);
                    self.fail(payload)
                }
            },
            StreamEvent::Closed => match std::mem::replace(&mut self.phase, Phase::Connecting) {
                Phase::Closing(outcome) => Flow::Step(DriverStep::Done(outcome)),
                _ => {
                    let payload = failure_payload("redis: the connection closed unexpectedly", "failed", None, None, &self.key);
                    Flow::Step(DriverStep::Done(Err(payload)))
                }
            },
        }
    }

    fn on_data(&mut self, bytes: Vec<u8>) -> Flow {
        self.inbox.extend(bytes);
        while self.replies.len() < self.expected {
            match parse(&self.inbox) {
                Parsed::Complete(resp, used) => {
                    self.inbox.drain(..used);
                    self.replies.push(resp);
                }
                Parsed::Incomplete => return Flow::Event(self.stream().recv()),
                Parsed::Invalid(message) => {
                    let payload = failure_payload(&format!("redis: {message}"), "protocol", None, None, &self.key);
                    return self.fail(payload);
                }
            }
        }
        let replies = std::mem::take(&mut self.replies);
        match self.phase {
            Phase::HandshakeRecv => {
                for reply in &replies {
                    if let Err(problem) = reply_value(reply) {
                        let payload = problem_payload(&problem, &self.key);
                        return self.fail(payload);
                    }
                }
                let command = self.spec.command.clone();
                self.send_commands(&[command], Phase::CommandSend)
            }
            _ => {
                let outcome = match replies.first().map(reply_value) {
                    Some(Ok(reply)) => Ok(object(vec![("reply", reply), ("key", key_value(&self.key))])),
                    Some(Err(problem)) => Err(problem_payload(&problem, &self.key)),
                    None => Err(failure_payload("redis: no reply", "protocol", None, None, &self.key)),
                };
                self.finish(outcome)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::codec;
    use orbital_core_base::os::{OsError, OsErrorKind, OsOp};

    fn text(s: &str) -> Value {
        Value::String(s.to_string())
    }

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn list(items: &[&str]) -> Value {
        Value::Array(items.iter().map(|s| text(s)).collect())
    }

    fn egress() -> Vec<String> {
        vec!["redis://cache.internal:6379".to_string(), "rediss://secure.internal".to_string()]
    }

    fn spec(command: &[&str]) -> RedisCallSpec {
        RedisCallSpec { origin: "redis://cache.internal".to_string(), command: words(command), auth: None, user: None, db: None }
    }

    fn sock() -> DriverAnswer {
        DriverAnswer::Os(Ok(Value::Object(ValueMap::from([("sock".to_string(), Value::Number(5.0))]))))
    }

    fn ok() -> DriverAnswer {
        DriverAnswer::Os(Ok(Value::Null))
    }

    fn data(bytes: &[u8]) -> DriverAnswer {
        DriverAnswer::Os(Ok(Value::Object(ValueMap::from([
            ("data".to_string(), text(&codec::base64_encode(bytes))),
            ("eof".to_string(), Value::Bool(false)),
        ]))))
    }

    fn eof() -> DriverAnswer {
        DriverAnswer::Os(Ok(Value::Object(ValueMap::from([
            ("data".to_string(), text("")),
            ("eof".to_string(), Value::Bool(true)),
        ]))))
    }

    fn os_ask(op: OsOp, args: Vec<Value>) -> DriverAsk {
        DriverAsk::Os { op, args, key: None }
    }

    fn connect() -> DriverAsk {
        os_ask(OsOp::TcpConnect, vec![text("cache.internal"), Value::Number(6379.0)])
    }

    fn send(bytes: &[u8]) -> DriverAsk {
        os_ask(OsOp::Send, vec![Value::Number(5.0), text(&codec::base64_encode(bytes))])
    }

    fn recv() -> DriverAsk {
        os_ask(OsOp::Recv, vec![Value::Number(5.0), Value::Number(16384.0)])
    }

    fn close() -> DriverAsk {
        os_ask(OsOp::Close, vec![Value::Number(5.0)])
    }

    fn run(spec: RedisCallSpec, key: Option<&str>, answers: Vec<DriverAnswer>) -> (Vec<DriverAsk>, DriverStep) {
        let (mut driver, mut step) = RedisDriver::start(spec, egress(), key.map(str::to_string));
        let mut asks = Vec::new();
        let mut answers = answers.into_iter();
        loop {
            let DriverStep::Ask(ask) = step.clone() else {
                return (asks, step);
            };
            asks.push(ask);
            let Some(answer) = answers.next() else {
                return (asks, step);
            };
            step = driver.resume(answer);
        }
    }

    fn reply_payload(reply: Value, key: Value) -> Value {
        object(vec![("reply", reply), ("key", key)])
    }

    fn failed(step: &DriverStep) -> &Value {
        match step {
            DriverStep::Done(Err(payload)) => payload,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn field<'a>(payload: &'a Value, name: &str) -> &'a Value {
        payload.get_path(name).unwrap_or_else(|| panic!("no `{name}` in {payload:?}"))
    }

    #[test]
    fn a_command_is_a_resp_array_of_bulk_strings() {
        assert_eq!(encode_command(&words(&["GET", "k"])), b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n".to_vec());
        assert_eq!(encode_command(&words(&["SET", "k", ""])), b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$0\r\n\r\n".to_vec());
        assert!(encode_command(&words(&["SET", "k", "h\u{e9}llo"])).ends_with(b"$6\r\nh\xc3\xa9llo\r\n"));
    }

    #[test]
    fn every_resp2_frame_parses_and_reports_its_length() {
        let cases: Vec<(&[u8], Resp)> = vec![
            (b"+OK\r\n", Resp::Simple("OK".to_string())),
            (b"-ERR bad\r\n", Resp::Error("ERR bad".to_string())),
            (b":-42\r\n", Resp::Integer(-42)),
            (b"$3\r\nbar\r\n", Resp::Bulk(Some(b"bar".to_vec()))),
            (b"$0\r\n\r\n", Resp::Bulk(Some(Vec::new()))),
            (b"$-1\r\n", Resp::Bulk(None)),
            (b"*-1\r\n", Resp::Array(None)),
            (b"*0\r\n", Resp::Array(Some(Vec::new()))),
            (b"$4\r\na\r\nb\r\n", Resp::Bulk(Some(b"a\r\nb".to_vec()))),
            (
                b"*3\r\n$1\r\na\r\n$-1\r\n*1\r\n:7\r\n",
                Resp::Array(Some(vec![Resp::Bulk(Some(b"a".to_vec())), Resp::Bulk(None), Resp::Array(Some(vec![Resp::Integer(7)]))])),
            ),
        ];
        for (bytes, expected) in cases {
            assert_eq!(parse(bytes), Parsed::Complete(expected.clone(), bytes.len()), "{expected:?}");
            let mut trailing = bytes.to_vec();
            trailing.extend_from_slice(b"+NEXT\r\n");
            assert_eq!(parse(&trailing), Parsed::Complete(expected, bytes.len()));
        }
    }

    #[test]
    fn a_frame_cut_at_any_byte_is_incomplete_and_the_whole_frame_completes() {
        let whole: &[u8] = b"*3\r\n$1\r\na\r\n$-1\r\n*1\r\n:7\r\n";
        for cut in 0..whole.len() {
            assert_eq!(parse(&whole[..cut]), Parsed::Incomplete, "cut at {cut}");
        }
        assert!(matches!(parse(whole), Parsed::Complete(_, n) if n == whole.len()));
        assert_eq!(parse(b"$5\r\nab"), Parsed::Incomplete);
        assert_eq!(parse(b"$2\r\nab\r"), Parsed::Incomplete);
    }

    #[test]
    fn malformed_frames_are_invalid_not_incomplete() {
        for bad in [&b"?x\r\n"[..], b":abc\r\n", b"$2\r\nabcd\r\n", b"$x\r\n", b"$-5\r\n", b"#x\r\n", b"$999999999999\r\n"] {
            assert!(matches!(parse(bad), Parsed::Invalid(_)), "{:?}", String::from_utf8_lossy(bad));
        }
        let deep = "*1\r\n".repeat(40);
        assert!(matches!(parse(deep.as_bytes()), Parsed::Invalid(_)));
    }

    #[test]
    fn resp3_frames_parse_where_they_map_onto_the_reply_union() {
        assert_eq!(parse(b"_\r\n"), Parsed::Complete(Resp::Null, 3));
        assert_eq!(parse(b"#t\r\n"), Parsed::Complete(Resp::Boolean(true), 4));
        assert_eq!(parse(b",1.5\r\n"), Parsed::Complete(Resp::Double(1.5), 6));
        assert_eq!(parse(b"(123456789012345678901\r\n"), Parsed::Complete(Resp::BigNumber("123456789012345678901".to_string()), 24));
        assert_eq!(parse(b"=7\r\ntxt:abc\r\n"), Parsed::Complete(Resp::Verbatim(b"txt:abc".to_vec()), 13));
        assert_eq!(parse(b"!3\r\nbad\r\n"), Parsed::Complete(Resp::BlobError(b"bad".to_vec()), 9));
        let map = b"%1\r\n+k\r\n:1\r\n";
        assert_eq!(parse(map), Parsed::Complete(Resp::Map(vec![(Resp::Simple("k".to_string()), Resp::Integer(1))]), map.len()));
        let attributed = b"|1\r\n+ttl\r\n:9\r\n+OK\r\n";
        assert_eq!(parse(attributed), Parsed::Complete(Resp::Simple("OK".to_string()), attributed.len()));
        assert_eq!(parse(b"~2\r\n:1\r\n:2\r\n"), Parsed::Complete(Resp::Set(vec![Resp::Integer(1), Resp::Integer(2)]), 12));
    }

    #[test]
    fn replies_become_text_numbers_null_and_nested_lists() {
        assert_eq!(reply_value(&Resp::Simple("OK".to_string())), Ok(text("OK")));
        assert_eq!(reply_value(&Resp::Bulk(Some(b"v".to_vec()))), Ok(text("v")));
        assert_eq!(reply_value(&Resp::Integer(3)), Ok(Value::Number(3.0)));
        assert_eq!(reply_value(&Resp::Bulk(None)), Ok(Value::Null));
        assert_eq!(reply_value(&Resp::Array(None)), Ok(Value::Null));
        assert_eq!(reply_value(&Resp::Boolean(true)), Ok(Value::Number(1.0)));
        let nested = Resp::Array(Some(vec![Resp::Bulk(None), Resp::Array(Some(vec![Resp::Integer(1)]))]));
        assert_eq!(
            reply_value(&nested),
            Ok(Value::Array(vec![Value::Null, Value::Array(vec![Value::Number(1.0)])]))
        );
        assert_eq!(
            reply_value(&Resp::Map(vec![(Resp::Simple("k".to_string()), Resp::Integer(1))])),
            Ok(Value::Array(vec![text("k"), Value::Number(1.0)]))
        );
    }

    #[test]
    fn controls_for_the_reply_value_limits() {
        let at_the_limit = Resp::Array(Some(vec![Resp::Array(Some(vec![Resp::Array(Some(vec![Resp::Integer(1)]))]))]));
        assert!(reply_value(&at_the_limit).is_ok());
        let past = Resp::Array(Some(vec![at_the_limit]));
        assert!(matches!(reply_value(&past), Err(ReplyProblem::Unsupported(_))));
        assert!(matches!(reply_value(&Resp::Integer(1 << 60)), Err(ReplyProblem::Unsupported(_))));
        assert!(matches!(reply_value(&Resp::Bulk(Some(vec![0xff, 0xfe]))), Err(ReplyProblem::Unsupported(_))));
        assert_eq!(reply_value(&Resp::Error("ERR x".to_string())), Err(ReplyProblem::Server("ERR x".to_string())));
        let buried = Resp::Array(Some(vec![Resp::Integer(1), Resp::Error("EXECABORT no".to_string())]));
        assert_eq!(reply_value(&buried), Err(ReplyProblem::Server("EXECABORT no".to_string())));
    }

    #[test]
    fn a_call_parses_its_command_and_connection_options() {
        let options = Value::Object(ValueMap::from([
            ("auth".to_string(), text("REDIS_PASSWORD")),
            ("user".to_string(), text("app")),
            ("db".to_string(), Value::Number(2.0)),
        ]));
        let parsed = parse_redis_call(&text("redis://cache.internal"), &list(&["GET", "k"]), &options).expect("parses");
        assert_eq!(parsed.command, words(&["GET", "k"]));
        assert_eq!(parsed.auth, Some(CredentialSlot::from("REDIS_PASSWORD")));
        assert_eq!((parsed.user.as_deref(), parsed.db), (Some("app"), Some(2)));
        let scoped = Value::Object(ValueMap::from([(
            "auth".to_string(),
            Value::Object(ValueMap::from([("credential".to_string(), text("TOKEN")), ("scope".to_string(), text("user"))])),
        )]));
        let parsed = parse_redis_call(&text("redis://h"), &list(&["PING"]), &scoped).expect("parses");
        assert_eq!(parsed.auth.map(|slot| slot.scope), Some(orbital_core_base::http_wire::SecretScope::User));
        assert!(parse_redis_call(&text("redis://h"), &list(&["PING"]), &Value::Null).is_ok());
    }

    #[test]
    fn controls_a_malformed_call_is_refused() {
        let origin = text("redis://h");
        let nothing = Value::Null;
        for command in [Value::Array(Vec::new()), list(&[""]), Value::Array(vec![text("GET"), Value::Number(1.0)]), text("GET k")] {
            assert!(parse_redis_call(&origin, &command, &nothing).is_err(), "{command:?}");
        }
        assert!(parse_redis_call(&Value::Number(1.0), &list(&["PING"]), &nothing).is_err());
        let user_alone = Value::Object(ValueMap::from([("user".to_string(), text("app"))]));
        assert!(parse_redis_call(&origin, &list(&["PING"]), &user_alone).is_err());
        let unknown = Value::Object(ValueMap::from([("timeout".to_string(), Value::Number(1.0))]));
        assert!(parse_redis_call(&origin, &list(&["PING"]), &unknown).is_err());
        let fractional = Value::Object(ValueMap::from([("db".to_string(), Value::Number(1.5))]));
        assert!(parse_redis_call(&origin, &list(&["PING"]), &fractional).is_err());
    }

    #[test]
    fn get_connects_sends_one_command_reads_the_reply_and_closes() {
        let (asks, step) = run(spec(&["GET", "k"]), None, vec![sock(), ok(), data(b"$3\r\nbar\r\n"), ok()]);
        assert_eq!(asks, vec![connect(), send(b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n"), recv(), close()]);
        assert_eq!(step, DriverStep::Done(Ok(reply_payload(text("bar"), Value::Null))));
    }

    #[test]
    fn the_key_rides_the_success_and_the_failure() {
        let (_, step) = run(spec(&["GET", "k"]), Some("call-1"), vec![sock(), ok(), data(b"$-1\r\n"), ok()]);
        assert_eq!(step, DriverStep::Done(Ok(reply_payload(Value::Null, text("call-1")))));
        let (_, step) = run(spec(&["INCR", "k"]), Some("call-1"), vec![sock(), ok(), data(b"-ERR value is not an integer\r\n"), ok()]);
        assert_eq!(field(failed(&step), "key"), &text("call-1"));
    }

    #[test]
    fn a_reply_split_across_reads_asks_for_more_until_it_is_whole() {
        let (asks, step) = run(
            spec(&["GET", "k"]),
            None,
            vec![sock(), ok(), data(b"$11\r\nhello"), data(b" wor"), data(b"ld\r\n"), ok()],
        );
        assert_eq!(asks.iter().filter(|a| **a == recv()).count(), 3);
        assert_eq!(step, DriverStep::Done(Ok(reply_payload(text("hello world"), Value::Null))));
    }

    #[test]
    fn auth_and_select_go_out_as_one_pipeline_whose_replies_are_checked_before_the_command() {
        let mut call = spec(&["GET", "k"]);
        call.auth = Some(CredentialSlot::from("REDIS_PASSWORD"));
        call.user = Some("app".to_string());
        call.db = Some(2);
        let pipeline = b"*3\r\n$4\r\nAUTH\r\n$3\r\napp\r\n$2\r\npw\r\n*2\r\n$6\r\nSELECT\r\n$1\r\n2\r\n";
        let (asks, step) = run(
            call,
            None,
            vec![
                DriverAnswer::Secret(Ok(Some("pw".to_string()))),
                sock(),
                ok(),
                data(b"+OK\r\n"),
                data(b"+OK\r\n"),
                ok(),
                data(b"$1\r\nv\r\n"),
                ok(),
            ],
        );
        assert_eq!(
            asks,
            vec![
                DriverAsk::Secret { name: "REDIS_PASSWORD".to_string(), scope: orbital_core_base::http_wire::SecretScope::App },
                connect(),
                send(pipeline),
                recv(),
                recv(),
                send(b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n"),
                recv(),
                close(),
            ]
        );
        assert_eq!(step, DriverStep::Done(Ok(reply_payload(text("v"), Value::Null))));
    }

    #[test]
    fn auth_without_a_user_sends_the_password_alone() {
        let mut call = spec(&["PING"]);
        call.auth = Some(CredentialSlot::from("P"));
        let (asks, _) = run(call, None, vec![DriverAnswer::Secret(Ok(Some("pw".to_string()))), sock(), ok(), data(b"+OK\r\n")]);
        assert_eq!(asks[2], send(b"*2\r\n$4\r\nAUTH\r\n$2\r\npw\r\n"));
    }

    #[test]
    fn control_a_call_with_no_auth_and_no_db_sends_no_handshake() {
        let (asks, _) = run(spec(&["PING"]), None, vec![sock(), ok(), data(b"+PONG\r\n"), ok()]);
        assert_eq!(asks.iter().filter(|a| matches!(a, DriverAsk::Os { op: OsOp::Send, .. })).count(), 1);
        assert!(!asks.iter().any(|a| matches!(a, DriverAsk::Secret { .. })));
    }

    #[test]
    fn a_wrong_password_fails_the_call_before_the_command_is_sent() {
        let mut call = spec(&["SET", "k", "v"]);
        call.auth = Some(CredentialSlot::from("P"));
        call.db = Some(1);
        let (asks, step) = run(
            call,
            None,
            vec![
                DriverAnswer::Secret(Ok(Some("bad".to_string()))),
                sock(),
                ok(),
                data(b"-WRONGPASS invalid username-password pair or user is disabled.\r\n+OK\r\n"),
                ok(),
            ],
        );
        let payload = failed(&step);
        assert_eq!(field(payload, "kind"), &text("redis"));
        assert_eq!(field(payload, "code"), &text("WRONGPASS"));
        assert_eq!(asks.iter().filter(|a| matches!(a, DriverAsk::Os { op: OsOp::Send, .. })).count(), 1);
        assert_eq!(asks.last(), Some(&close()));
        assert!(!format!("{payload:?}").contains("bad"));
    }

    #[test]
    fn a_failed_select_does_not_run_the_command_on_the_wrong_database() {
        let mut call = spec(&["SET", "k", "v"]);
        call.db = Some(99);
        let (asks, step) = run(call, None, vec![sock(), ok(), data(b"-ERR DB index is out of range\r\n"), ok()]);
        assert_eq!(field(failed(&step), "code"), &text("ERR"));
        assert_eq!(asks.iter().filter(|a| matches!(a, DriverAsk::Os { op: OsOp::Send, .. })).count(), 1);
    }

    #[test]
    fn a_server_error_reply_is_a_typed_failure_with_its_code() {
        let (asks, step) = run(spec(&["INCR", "k"]), None, vec![sock(), ok(), data(b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"), ok()]);
        let payload = failed(&step);
        assert_eq!(field(payload, "kind"), &text("redis"));
        assert_eq!(field(payload, "code"), &text("WRONGTYPE"));
        assert_eq!(field(payload, "error"), &text("WRONGTYPE Operation against a key holding the wrong kind of value"));
        assert_eq!(asks.last(), Some(&close()));
    }

    #[test]
    fn a_missing_credential_is_unconfigured_and_never_connects() {
        let mut call = spec(&["PING"]);
        call.auth = Some(CredentialSlot::from("REDIS_PASSWORD"));
        let (asks, step) = run(call, None, vec![DriverAnswer::Secret(Ok(None))]);
        assert_eq!(asks.len(), 1);
        let payload = failed(&step);
        assert_eq!(field(payload, "kind"), &text("unconfigured"));
        assert_eq!(field(payload, "credential"), &text("REDIS_PASSWORD"));
    }

    #[test]
    fn a_target_outside_egress_is_refused_before_any_ask() {
        let mut call = spec(&["PING"]);
        call.origin = "redis://other.internal:6379".to_string();
        let (asks, step) = run(call, None, Vec::new());
        assert!(asks.is_empty());
        assert_eq!(field(failed(&step), "kind"), &text("refused"));
        assert_eq!(field(failed(&step), "error"), &text("egress denied: redis://other.internal:6379"));
    }

    #[test]
    fn a_server_that_closes_mid_reply_fails_and_the_socket_is_closed() {
        let (asks, step) = run(spec(&["GET", "k"]), None, vec![sock(), ok(), data(b"$10\r\nabc"), eof(), ok()]);
        assert_eq!(field(failed(&step), "kind"), &text("failed"));
        assert_eq!(asks.last(), Some(&close()));
    }

    #[test]
    fn a_read_timeout_is_a_timeout_failure() {
        let slow = DriverAnswer::Os(Err(OsError::new(OsErrorKind::Timeout, "os/recv: timed out")));
        let (asks, step) = run(spec(&["GET", "k"]), None, vec![sock(), ok(), slow, ok()]);
        assert_eq!(field(failed(&step), "kind"), &text("timeout"));
        assert_eq!(asks.last(), Some(&close()));
    }

    #[test]
    fn a_refused_connect_is_a_refused_failure_with_nothing_to_close() {
        let denied = DriverAnswer::Os(Err(OsError::new(OsErrorKind::Denied, "os/tcp-connect: cache.internal:6379 is not granted")));
        let (asks, step) = run(spec(&["GET", "k"]), None, vec![denied]);
        assert_eq!(asks, vec![connect()]);
        assert_eq!(field(failed(&step), "kind"), &text("refused"));
    }

    #[test]
    fn a_garbled_reply_is_a_protocol_failure() {
        let (_, step) = run(spec(&["GET", "k"]), None, vec![sock(), ok(), data(b"?what\r\n"), ok()]);
        assert_eq!(field(failed(&step), "kind"), &text("protocol"));
    }

    #[test]
    fn an_array_reply_with_nulls_comes_back_whole() {
        let (_, step) = run(spec(&["MGET", "a", "b"]), None, vec![sock(), ok(), data(b"*2\r\n$1\r\nx\r\n$-1\r\n"), ok()]);
        assert_eq!(
            step,
            DriverStep::Done(Ok(reply_payload(Value::Array(vec![text("x"), Value::Null]), Value::Null)))
        );
    }

    #[test]
    fn a_rediss_origin_opens_the_tls_stream_after_the_same_egress_check() {
        let mut call = spec(&["PING"]);
        call.origin = "rediss://secure.internal".to_string();
        let (asks, _) = run(call.clone(), None, Vec::new());
        assert_eq!(asks, vec![os_ask(OsOp::TcpConnect, vec![text("secure.internal"), Value::Number(6379.0)])]);
        call.origin = "rediss://elsewhere.internal".to_string();
        let (asks, step) = run(call, None, Vec::new());
        assert!(asks.is_empty());
        assert_eq!(field(failed(&step), "kind"), &text("refused"));
    }
}
