//! The `postgres` effect's protocol, owned by the kernel: a Postgres v3 client over the shared sans-I/O driver loop. One
//! call is one connection that runs one read-only statement: connect (an in-band `SSLRequest` and the kernel's TLS when
//! the call asks for it), the startup message, authentication (cleartext, MD5 or SCRAM-SHA-256), the extended query
//! protocol (Parse, Bind, Describe, Execute, Sync) with every parameter and result in text format, then Terminate and
//! close. Every ask is a journaled `os/*`, `Secret`, `Random` or clock ask, so a replay of the journal is a replay of
//! the call. The statement passes `sql_guard` (the one read-only guard) before anything connects.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::codec;
use orbital_core_base::emit_contract::POSTGRES_ARRAY_DEPTH;
use orbital_core_base::http_wire::{parse_slot_object, whole_number, CredentialSlot, HttpRefusal};
use orbital_core_base::sql_guard::assert_read_only_select;
use orbital_core_base::str_utils::js_number_string;
use orbital_core_base::value::{Value, ValueMap};

use crate::byte_stream::{self, ByteStream, FailureKind, StreamEvent, StreamFailure};
use crate::driver::{Driver, DriverAnswer, DriverAsk, DriverStep};
use crate::http_request::tcp_target_allowed;
use crate::redis::{failure_payload, refusal_payload};

const MAX_MESSAGE: usize = 1 << 30;
const MAX_SCRAM_ITERATIONS: u32 = 1_000_000;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_992;
const SCRAM: &str = "SCRAM-SHA-256";
pub(crate) const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

/// A parsed `postgres` call.
#[derive(Debug, Clone, PartialEq)]
pub struct PostgresCallSpec {
    pub origin: String,
    pub sql: String,
    /// The bind values as the text the server parses; `None` is SQL NULL.
    pub params: Vec<Option<String>>,
    pub auth: Option<CredentialSlot>,
    pub user: String,
    pub database: Option<String>,
    pub tls: bool,
    pub timeout_ms: Option<u64>,
}

fn invalid<T>(message: impl Into<String>) -> Result<T, HttpRefusal> {
    Err(HttpRefusal::Invalid(message.into()))
}

/// The text a bind value is sent as (the server infers the parameter type): `null` is NULL, a list a Postgres array
/// literal, an object its JSON.
pub fn bind_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(js_number_string(*n)),
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => Some(array_literal(items)),
        Value::Object(_) => serde_json::to_string(value).ok(),
        Value::Lambda { .. } => None,
    }
}

fn holds_lambda(value: &Value) -> bool {
    match value {
        Value::Lambda { .. } => true,
        Value::Array(items) => items.iter().any(holds_lambda),
        Value::Object(fields) => fields.values().any(holds_lambda),
        _ => false,
    }
}

fn array_literal(items: &[Value]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|item| match item {
            Value::Null => "NULL".to_string(),
            Value::Array(inner) => array_literal(inner),
            other => {
                let text = bind_text(other).unwrap_or_default();
                format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// The call's `origin`, `sql`, bind `params` and connection `options` as a [`PostgresCallSpec`]; a statement the
/// read-only guard refuses is refused here, before any ask.
pub fn parse_postgres_call(origin: &Value, sql: &Value, params: &Value, options: &Value) -> Result<PostgresCallSpec, HttpRefusal> {
    let Value::String(origin) = origin else {
        return invalid("postgres: the origin must be text");
    };
    let Value::String(sql) = sql else {
        return invalid("postgres: the statement must be text");
    };
    let guard = assert_read_only_select(sql);
    if !guard.ok {
        return invalid(format!(
            "Read-only violation: {}",
            guard.reason.as_deref().unwrap_or("not a SELECT statement")
        ));
    }
    if holds_lambda(params) {
        return invalid("postgres: a param cannot be a function");
    }
    let binds = match params {
        Value::Null => Vec::new(),
        Value::Array(items) => items.iter().map(bind_text).collect(),
        _ => return invalid("postgres: the params must be a list"),
    };
    let Value::Object(map) = options else {
        return invalid("postgres: the options must name a user");
    };
    if let Some(unknown) = map.keys().find(|k| !matches!(k.as_str(), "auth" | "user" | "database" | "tls" | "timeoutMs")) {
        return invalid(format!("postgres: unknown option `{unknown}` (auth, user, database, tls, timeoutMs)"));
    }
    let auth = match map.get("auth") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) if !name.is_empty() => Some(CredentialSlot::from(name.as_str())),
        Some(Value::Object(slot)) => Some(parse_slot_object(slot, "postgres options.auth")?),
        Some(_) => return invalid("postgres: options.auth must be a credential name or { credential, scope }"),
    };
    let user = match map.get("user") {
        Some(Value::String(name)) if !name.is_empty() => name.clone(),
        _ => return invalid("postgres: options.user must be a non-empty string"),
    };
    let database = match map.get("database") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) if !name.is_empty() => Some(name.clone()),
        Some(_) => return invalid("postgres: options.database must be a non-empty string"),
    };
    let tls = match map.get("tls") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return invalid("postgres: options.tls must be true or false"),
    };
    let timeout_ms = match whole_number(map.get("timeoutMs"), "postgres options.timeoutMs")? {
        Some(0) => return invalid("postgres: options.timeoutMs must be above zero"),
        other => other,
    };
    Ok(PostgresCallSpec { origin: origin.clone(), sql: sql.clone(), params: binds, auth, user, database, tls, timeout_ms })
}

// ---- messages ----

pub(crate) fn message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(kind);
    out.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

pub(crate) fn cstr(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(text.as_bytes());
    out.push(0);
}

/// The startup message: protocol 3.0 and the connection parameters.
pub fn startup_message(spec: &PostgresCallSpec) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&196_608u32.to_be_bytes());
    let mut pairs = vec![("user", spec.user.clone()), ("database", spec.database.clone().unwrap_or_else(|| spec.user.clone())), ("client_encoding", "UTF8".to_string())];
    if let Some(ms) = spec.timeout_ms {
        pairs.push(("options", format!("-c statement_timeout={ms}")));
    }
    for (name, value) in pairs {
        cstr(&mut body, name);
        cstr(&mut body, &value);
    }
    body.push(0);
    let mut out = ((body.len() + 4) as u32).to_be_bytes().to_vec();
    out.extend(body);
    out
}

/// Parse, Bind, Describe, Execute and Sync as one send: every parameter and every result column in text format.
pub fn query_messages(sql: &str, params: &[Option<String>]) -> Vec<u8> {
    let mut parse = vec![0u8];
    cstr(&mut parse, sql);
    parse.extend_from_slice(&0u16.to_be_bytes());
    let mut bind = vec![0u8, 0u8];
    bind.extend_from_slice(&0u16.to_be_bytes());
    bind.extend_from_slice(&(params.len() as u16).to_be_bytes());
    for param in params {
        match param {
            None => bind.extend_from_slice(&(-1i32).to_be_bytes()),
            Some(text) => {
                bind.extend_from_slice(&(text.len() as i32).to_be_bytes());
                bind.extend_from_slice(text.as_bytes());
            }
        }
    }
    bind.extend_from_slice(&0u16.to_be_bytes());
    let mut out = message(b'P', &parse);
    out.extend(message(b'B', &bind));
    out.extend(message(b'D', &[b'P', 0]));
    let mut execute = vec![0u8];
    execute.extend_from_slice(&0u32.to_be_bytes());
    out.extend(message(b'E', &execute));
    out.extend(message(b'S', &[]));
    out
}

/// One backend message: its type byte and body.
#[derive(Debug, Clone, PartialEq)]
pub struct Backend {
    pub kind: u8,
    pub body: Vec<u8>,
}

/// The first whole message of `buf`, removed from it; `None` while it is incomplete.
pub fn next_message(buf: &mut Vec<u8>) -> Result<Option<Backend>, String> {
    if buf.len() < 5 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if len < 4 {
        return Err(format!("a message of length {len} is shorter than its own header"));
    }
    if len > MAX_MESSAGE {
        return Err(format!("a message of {len} bytes exceeds the {MAX_MESSAGE} byte limit"));
    }
    if buf.len() < 1 + len {
        return Ok(None);
    }
    let kind = buf[0];
    let body = buf[5..1 + len].to_vec();
    buf.drain(..1 + len);
    Ok(Some(Backend { kind, body }))
}

fn be_i32(body: &[u8], at: usize) -> Result<i32, String> {
    body.get(at..at + 4).map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| "a message ends inside an integer".to_string())
}

fn be_i16(body: &[u8], at: usize) -> Result<i16, String> {
    body.get(at..at + 2).map(|b| i16::from_be_bytes([b[0], b[1]])).ok_or_else(|| "a message ends inside an integer".to_string())
}

pub(crate) fn read_cstr(body: &[u8], at: usize) -> Result<(String, usize), String> {
    let rest = body.get(at..).ok_or_else(|| "a message ends inside a string".to_string())?;
    let end = rest.iter().position(|b| *b == 0).ok_or_else(|| "a string is not terminated".to_string())?;
    let text = String::from_utf8(rest[..end].to_vec()).map_err(|_| "a string is not UTF-8".to_string())?;
    Ok((text, at + end + 1))
}

/// An ErrorResponse: its SQLSTATE and message.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerError {
    pub code: String,
    pub message: String,
}

pub fn parse_error(body: &[u8]) -> ServerError {
    let (mut code, mut message) = (String::new(), String::new());
    let mut at = 0;
    while let Some(&field) = body.get(at) {
        if field == 0 {
            break;
        }
        let Ok((text, next)) = read_cstr(body, at + 1) else { break };
        match field {
            b'C' => code = text,
            b'M' => message = text,
            _ => {}
        }
        at = next;
    }
    ServerError { code, message }
}

/// A result column.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub type_oid: u32,
}

pub(crate) fn parse_row_description(body: &[u8]) -> Result<Vec<Column>, String> {
    let count = usize::try_from(be_i16(body, 0)?).map_err(|_| "a row description has a negative column count".to_string())?;
    let mut at = 2;
    let mut columns = Vec::with_capacity(count);
    for _ in 0..count {
        let (name, next) = read_cstr(body, at)?;
        let type_oid = be_i32(body, next + 6)? as u32;
        columns.push(Column { name, type_oid });
        at = next + 18;
    }
    Ok(columns)
}

pub(crate) fn parse_data_row(body: &[u8]) -> Result<Vec<Option<Vec<u8>>>, String> {
    let count = usize::try_from(be_i16(body, 0)?).map_err(|_| "a data row has a negative column count".to_string())?;
    let mut at = 2;
    let mut cells = Vec::with_capacity(count);
    for _ in 0..count {
        let len = be_i32(body, at)?;
        at += 4;
        if len < 0 {
            cells.push(None);
        } else {
            let len = len as usize;
            cells.push(Some(body.get(at..at + len).ok_or_else(|| "a data row ends inside a value".to_string())?.to_vec()));
            at += len;
        }
    }
    Ok(cells)
}

// ---- values ----

/// Why a result cannot become reply values.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueProblem(pub String);

fn problem<T>(message: impl Into<String>) -> Result<T, ValueProblem> {
    Err(ValueProblem(message.into()))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `timestamptz` text (`2025-10-09 08:53:20.123456+02`) as UTC ISO text (`2025-10-09T06:53:20.123456Z`).
fn timestamptz_iso(text: &str) -> Result<String, ValueProblem> {
    let bad = || ValueProblem(format!("`{text}` is not a timestamptz this driver reads"));
    let (date, rest) = text.split_once(' ').ok_or_else(bad)?;
    let split = rest.rfind(['+', '-']).ok_or_else(bad)?;
    let (time, offset) = rest.split_at(split);
    let mut ymd = date.split('-');
    let (y, mo, d) = (
        ymd.next().and_then(|v| v.parse::<i64>().ok()).ok_or_else(bad)?,
        ymd.next().and_then(|v| v.parse::<i64>().ok()).ok_or_else(bad)?,
        ymd.next().and_then(|v| v.parse::<i64>().ok()).ok_or_else(bad)?,
    );
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, fraction),
        None => (time, ""),
    };
    let mut hms = clock.split(':');
    let (h, mi, s) = (
        hms.next().and_then(|v| v.parse::<i64>().ok()).ok_or_else(bad)?,
        hms.next().and_then(|v| v.parse::<i64>().ok()).ok_or_else(bad)?,
        hms.next().and_then(|v| v.parse::<i64>().ok()).ok_or_else(bad)?,
    );
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let mut parts = offset[1..].split(':');
    let oh = parts.next().and_then(|v| v.parse::<i64>().ok()).ok_or_else(bad)?;
    let om = parts.next().map_or(Some(0), |v| v.parse::<i64>().ok()).ok_or_else(bad)?;
    let os = parts.next().map_or(Some(0), |v| v.parse::<i64>().ok()).ok_or_else(bad)?;
    let seconds = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s - sign * (oh * 3600 + om * 60 + os);
    let (ry, rm, rd) = civil_from_days(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400);
    let fraction = if fraction.is_empty() { String::new() } else { format!(".{fraction}") };
    Ok(format!("{ry:04}-{rm:02}-{rd:02}T{:02}:{:02}:{:02}{fraction}Z", of_day / 3600, of_day / 60 % 60, of_day % 60))
}

fn element_oid(array_oid: u32) -> Option<u32> {
    Some(match array_oid {
        1000 => 16,
        1001 => 17,
        1005 => 21,
        1007 => 23,
        1009 => 25,
        1014 => 1042,
        1015 => 1043,
        1016 => 20,
        1021 => 700,
        1022 => 701,
        1115 => 1114,
        1182 => 1082,
        1185 => 1184,
        1231 => 1700,
        2951 => 2950,
        199 => 114,
        3807 => 3802,
        _ => return None,
    })
}

fn scalar_value(oid: u32, text: &str) -> Result<Value, ValueProblem> {
    Ok(match oid {
        16 => match text {
            "t" => Value::Bool(true),
            "f" => Value::Bool(false),
            other => return problem(format!("`{other}` is not a boolean")),
        },
        21 | 23 | 26 => Value::Number(text.parse::<i64>().map_err(|_| ValueProblem(format!("`{text}` is not an integer")))? as f64),
        20 => {
            let n = text.parse::<i64>().map_err(|_| ValueProblem(format!("`{text}` is not an integer")))?;
            if n.abs() <= MAX_SAFE_INTEGER {
                Value::Number(n as f64)
            } else {
                Value::String(text.to_string())
            }
        }
        700 | 701 => match text.parse::<f64>() {
            Ok(n) if n.is_finite() => Value::Number(n),
            _ => Value::Null,
        },
        17 => {
            let hex = text.strip_prefix("\\x").ok_or_else(|| ValueProblem("a bytea column is not in hex output format".to_string()))?;
            if hex.len() % 2 != 0 {
                return problem("a bytea value has an odd number of hex digits");
            }
            let bytes: Result<Vec<u8>, _> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16)).collect();
            Value::String(codec::base64_encode(&bytes.map_err(|_| ValueProblem("a bytea value is not hex".to_string()))?))
        }
        1184 => {
            if text.ends_with("infinity") {
                Value::String(text.to_string())
            } else {
                Value::String(timestamptz_iso(text)?)
            }
        }
        1114 => Value::String(text.replacen(' ', "T", 1)),
        _ => Value::String(text.to_string()),
    })
}

/// A one- or many-dimensional array literal (`{1,2}`, `{{"a b",NULL}}`) as nested lists of `element` values.
fn parse_array(element: u32, text: &str, depth: usize) -> Result<Value, ValueProblem> {
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0;
    if chars.first() == Some(&'[') {
        at = chars.iter().position(|c| *c == '=').map_or(0, |p| p + 1);
    }
    let value = array_level(element, &chars, &mut at, depth)?;
    if at != chars.len() {
        return problem("an array literal has trailing text");
    }
    Ok(value)
}

fn array_level(element: u32, chars: &[char], at: &mut usize, depth: usize) -> Result<Value, ValueProblem> {
    if depth > POSTGRES_ARRAY_DEPTH {
        return problem(format!("an array is nested deeper than {POSTGRES_ARRAY_DEPTH} lists"));
    }
    if chars.get(*at) != Some(&'{') {
        return problem("an array literal does not start with {");
    }
    *at += 1;
    let mut items = Vec::new();
    loop {
        match chars.get(*at) {
            Some('}') => {
                *at += 1;
                return Ok(Value::Array(items));
            }
            Some(',') => *at += 1,
            Some('{') => items.push(array_level(element, chars, at, depth + 1)?),
            Some('"') => {
                *at += 1;
                let mut text = String::new();
                loop {
                    match chars.get(*at) {
                        Some('\\') => {
                            text.push(*chars.get(*at + 1).ok_or_else(|| ValueProblem("an array literal ends inside an escape".to_string()))?);
                            *at += 2;
                        }
                        Some('"') => {
                            *at += 1;
                            break;
                        }
                        Some(c) => {
                            text.push(*c);
                            *at += 1;
                        }
                        None => return problem("an array literal ends inside a quoted element"),
                    }
                }
                items.push(scalar_value(element, &text)?);
            }
            Some(_) => {
                let start = *at;
                while chars.get(*at).is_some_and(|c| *c != ',' && *c != '}') {
                    *at += 1;
                }
                let text: String = chars[start..*at].iter().collect();
                items.push(if text == "NULL" { Value::Null } else { scalar_value(element, &text)? });
            }
            None => return problem("an array literal is not closed"),
        }
    }
}

/// One result cell as the value a row carries: booleans, integers (an `int8` past 2^53 as text), floats (NaN and the
/// infinities as `null`), `timestamptz` as UTC ISO text, `bytea` as base64, arrays of those as lists, and every other
/// type (numeric, json, jsonb, uuid, text, date, time, timestamp, interval, inet, enums, ...) as the text Postgres prints.
pub fn cell_value(oid: u32, cell: Option<&[u8]>) -> Result<Value, ValueProblem> {
    let Some(bytes) = cell else {
        return Ok(Value::Null);
    };
    let text = std::str::from_utf8(bytes).map_err(|_| ValueProblem("a result value is not UTF-8".to_string()))?;
    match element_oid(oid) {
        Some(element) => parse_array(element, text, 1),
        None => scalar_value(oid, text),
    }
}

/// The result object: `rows`, `rowCount`, `columns`.
pub fn result_value(columns: &[Column], rows: &[Vec<Option<Vec<u8>>>], tag: Option<&str>) -> Result<Value, ValueProblem> {
    let mut out_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let mut object = ValueMap::default();
        for (column, cell) in columns.iter().zip(row) {
            object.insert(column.name.clone(), cell_value(column.type_oid, cell.as_deref())?);
        }
        out_rows.push(Value::Object(object));
    }
    let counted = tag.and_then(|t| t.rsplit(' ').next()).and_then(|n| n.parse::<f64>().ok()).unwrap_or(rows.len() as f64);
    let columns_value = columns
        .iter()
        .map(|c| Value::Object(ValueMap::from_iter([("name".to_string(), Value::String(c.name.clone())), ("typeOid".to_string(), Value::Number(f64::from(c.type_oid)))])))
        .collect();
    Ok(Value::Object(ValueMap::from_iter([
        ("rows".to_string(), Value::Array(out_rows)),
        ("rowCount".to_string(), Value::Number(counted)),
        ("columns".to_string(), Value::Array(columns_value)),
    ])))
}

// ---- authentication ----

fn scram_escape(user: &str) -> String {
    user.replace('=', "=3D").replace(',', "=2C")
}

pub(crate) struct Scram {
    client_first_bare: String,
    nonce: String,
    expected_server_signature: Vec<u8>,
}

fn xor(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b).map(|(x, y)| x ^ y).collect()
}

impl Scram {
    pub(crate) fn start(user: &str, nonce: &str) -> (Scram, String) {
        let bare = format!("n={},r={nonce}", scram_escape(user));
        let first = format!("n,,{bare}");
        (Scram { client_first_bare: bare, nonce: nonce.to_string(), expected_server_signature: Vec::new() }, first)
    }

    /// The client-final message for the server-first message, remembering the signature the server must answer.
    pub(crate) fn proof(&mut self, password: &str, server_first: &str) -> Result<String, String> {
        let (mut combined, mut salt, mut iterations) = (None, None, None);
        for part in server_first.split(',') {
            match part.split_once('=') {
                Some(("r", v)) => combined = Some(v),
                Some(("s", v)) => salt = Some(v),
                Some(("i", v)) => iterations = v.parse::<u32>().ok(),
                _ => {}
            }
        }
        let (Some(combined), Some(salt), Some(iterations)) = (combined, salt, iterations) else {
            return Err("the SCRAM server-first message lacks r, s or i".to_string());
        };
        if !combined.starts_with(&self.nonce) {
            return Err("the SCRAM server nonce does not extend the client nonce".to_string());
        }
        if iterations == 0 || iterations > MAX_SCRAM_ITERATIONS {
            return Err(format!("the SCRAM iteration count {iterations} is outside 1 to {MAX_SCRAM_ITERATIONS}"));
        }
        let salt = codec::base64_decode(salt).map_err(|e| format!("the SCRAM salt is not base64: {e}"))?;
        let salted = codec::pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations, 32);
        let client_key = codec::hmac_sha256(&salted, b"Client Key");
        let stored_key = codec::sha256(&client_key);
        let without_proof = format!("c=biws,r={combined}");
        let auth_message = format!("{},{},{}", self.client_first_bare, server_first, without_proof);
        let client_signature = codec::hmac_sha256(&stored_key, auth_message.as_bytes());
        let proof = xor(&client_key, &client_signature);
        let server_key = codec::hmac_sha256(&salted, b"Server Key");
        self.expected_server_signature = codec::hmac_sha256(&server_key, auth_message.as_bytes()).to_vec();
        Ok(format!("{without_proof},p={}", codec::base64_encode(&proof)))
    }

    pub(crate) fn verify(&self, server_final: &str) -> Result<(), String> {
        let signature = server_final.strip_prefix("v=").ok_or_else(|| "the SCRAM server-final message carries no signature".to_string())?;
        let got = codec::base64_decode(signature).map_err(|e| format!("the SCRAM server signature is not base64: {e}"))?;
        if got == self.expected_server_signature {
            Ok(())
        } else {
            Err("the SCRAM server signature does not match: the server does not know the password".to_string())
        }
    }
}

pub(crate) fn md5_password(user: &str, password: &str, salt: &[u8]) -> String {
    let inner = codec::hex(&codec::md5(format!("{password}{user}").as_bytes()));
    let mut outer = inner.into_bytes();
    outer.extend_from_slice(salt);
    format!("md5{}", codec::hex(&codec::md5(&outer)))
}

// ---- driver ----

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Secret,
    Connecting,
    SslSend,
    SslRecv,
    Upgrading,
    StartupSend,
    Startup,
    Nonce,
    AuthSend,
    QuerySend,
    Query,
    Terminating,
    Closing,
}

enum Flow {
    Event(StreamEvent),
    Step(DriverStep),
}

pub struct PostgresDriver {
    spec: PostgresCallSpec,
    egress: Vec<String>,
    key: Option<String>,
    stream: Option<Box<dyn ByteStream>>,
    password: Option<String>,
    phase: Phase,
    connected: bool,
    inbox: Vec<u8>,
    scram: Option<Scram>,
    columns: Vec<Column>,
    rows: Vec<Vec<Option<Vec<u8>>>>,
    tag: Option<String>,
    error: Option<ServerError>,
    outcome: Option<Result<Value, Value>>,
}

impl Driver for PostgresDriver {
    fn resume(&mut self, answer: DriverAnswer) -> DriverStep {
        match self.phase {
            Phase::Secret => return self.credential(answer),
            Phase::Nonce => return self.nonce(answer),
            _ => {}
        }
        let Some(stream) = self.stream.as_mut() else {
            return DriverStep::Done(Err(self.failure("postgres: an answer arrived with no connection", "failed")));
        };
        let event = stream.resume(answer);
        self.run(event)
    }
}

impl PostgresDriver {
    /// Begin `spec`; the call is refused at once unless `egress` lists its origin (a `postgres://` or
    /// `postgresql://` origin).
    pub fn start(spec: PostgresCallSpec, egress: Vec<String>, key: Option<String>) -> (PostgresDriver, DriverStep) {
        let mut driver = PostgresDriver {
            spec,
            egress,
            key,
            stream: None,
            password: None,
            phase: Phase::Connecting,
            connected: false,
            inbox: Vec::new(),
            scram: None,
            columns: Vec::new(),
            rows: Vec::new(),
            tag: None,
            error: None,
            outcome: None,
        };
        let step = match tcp_target_allowed(&driver.spec.origin, &driver.egress) {
            Err(refusal) => DriverStep::Done(Err(refusal_payload(&refusal, &driver.key))),
            Ok(target) if !matches!(target.scheme(), "postgres" | "postgresql") => DriverStep::Done(Err(refusal_payload(
                &HttpRefusal::Invalid(format!("postgres: the origin must be postgres:// or postgresql://, not {}://", target.scheme())),
                &driver.key,
            ))),
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

    fn failure(&self, message: &str, kind: &str) -> Value {
        failure_payload(message, kind, None, None, &self.key)
    }

    fn credential(&mut self, answer: DriverAnswer) -> DriverStep {
        let name = self.spec.auth.as_ref().map(|slot| slot.name.clone()).unwrap_or_default();
        match answer {
            DriverAnswer::Secret(Ok(Some(password))) => {
                self.password = Some(password);
                self.connect()
            }
            DriverAnswer::Secret(Ok(None)) => DriverStep::Done(Err(refusal_payload(&HttpRefusal::CredentialMissing(name), &self.key))),
            DriverAnswer::Secret(Err(message)) => DriverStep::Done(Err(self.failure(&message, "failed"))),
            other => DriverStep::Done(Err(self.failure(&format!("postgres: unexpected answer {other:?}"), "failed"))),
        }
    }

    fn connect(&mut self) -> DriverStep {
        self.phase = Phase::Connecting;
        match byte_stream::open(&self.spec.origin, &self.egress) {
            Ok((stream, event)) => {
                self.stream = Some(stream);
                self.run(event)
            }
            Err(failure) => DriverStep::Done(Err(stream_failure_payload(&failure, &self.key))),
        }
    }

    fn nonce(&mut self, answer: DriverAnswer) -> DriverStep {
        let bytes = match answer {
            DriverAnswer::Random(Ok(bytes)) => bytes,
            DriverAnswer::Random(Err(message)) => {
                let payload = self.failure(&message, "failed");
                return self.fail_step(payload);
            }
            other => {
                let payload = self.failure(&format!("postgres: unexpected answer {other:?}"), "failed");
                return self.fail_step(payload);
            }
        };
        let (scram, first) = Scram::start(&self.spec.user, &codec::base64_encode(&bytes));
        self.scram = Some(scram);
        let mut body = Vec::new();
        cstr(&mut body, SCRAM);
        body.extend_from_slice(&(first.len() as i32).to_be_bytes());
        body.extend_from_slice(first.as_bytes());
        let event = self.send_phase(message(b'p', &body), Phase::AuthSend);
        self.run(event)
    }

    fn fail_step(&mut self, payload: Value) -> DriverStep {
        let flow = self.fail(payload);
        match flow {
            Flow::Step(step) => step,
            Flow::Event(event) => self.run(event),
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

    fn send_phase(&mut self, bytes: Vec<u8>, next: Phase) -> StreamEvent {
        self.phase = next;
        self.stream().send(&bytes)
    }

    fn fail(&mut self, payload: Value) -> Flow {
        self.outcome = Some(Err(payload.clone()));
        if self.connected && self.phase != Phase::Closing {
            self.phase = Phase::Closing;
            self.connected = false;
            return Flow::Event(self.stream().close());
        }
        Flow::Step(DriverStep::Done(Err(payload)))
    }

    fn server_failure(&mut self) -> Flow {
        let error = self.error.take().unwrap_or(ServerError { code: String::new(), message: "the server reported an error".to_string() });
        let code = (!error.code.is_empty()).then_some(error.code.as_str());
        let payload = failure_payload(&error.message, "postgres", code, None, &self.key);
        self.fail(payload)
    }

    fn on_event(&mut self, event: StreamEvent) -> Flow {
        match event {
            StreamEvent::Ask(ask) => Flow::Step(DriverStep::Ask(ask)),
            StreamEvent::Failed(failure) => {
                let payload = stream_failure_payload(&failure, &self.key);
                match (self.phase, self.outcome.clone()) {
                    (Phase::Closing | Phase::Terminating, Some(outcome)) => Flow::Step(DriverStep::Done(outcome)),
                    _ => self.fail(payload),
                }
            }
            StreamEvent::Connected => {
                self.connected = true;
                if self.phase == Phase::Upgrading {
                    return Flow::Event(self.send_phase(startup_message(&self.spec), Phase::StartupSend));
                }
                if self.spec.tls {
                    return Flow::Event(self.send_phase(SSL_REQUEST.to_vec(), Phase::SslSend));
                }
                Flow::Event(self.send_phase(startup_message(&self.spec), Phase::StartupSend))
            }
            StreamEvent::Sent => match self.phase {
                Phase::SslSend => {
                    self.phase = Phase::SslRecv;
                    Flow::Event(self.stream().recv())
                }
                Phase::StartupSend | Phase::AuthSend => {
                    self.phase = Phase::Startup;
                    self.pump()
                }
                Phase::QuerySend => {
                    self.phase = Phase::Query;
                    self.pump()
                }
                Phase::Terminating => {
                    self.phase = Phase::Closing;
                    self.connected = false;
                    Flow::Event(self.stream().close())
                }
                _ => {
                    let payload = self.failure("postgres: a send completed in an unexpected phase", "protocol");
                    self.fail(payload)
                }
            },
            StreamEvent::Data(bytes) => self.on_data(bytes),
            StreamEvent::Eof => match (self.phase, self.outcome.clone()) {
                (Phase::Closing | Phase::Terminating, Some(outcome)) => Flow::Step(DriverStep::Done(outcome)),
                _ => {
                    let payload = self.failure("postgres: the server closed the connection before the statement finished", "failed");
                    self.fail(payload)
                }
            },
            StreamEvent::Closed => {
                let outcome = self.outcome.take().unwrap_or_else(|| Err(self.failure("postgres: the connection closed unexpectedly", "failed")));
                Flow::Step(DriverStep::Done(outcome))
            }
        }
    }

    fn on_data(&mut self, bytes: Vec<u8>) -> Flow {
        if self.phase == Phase::SslRecv {
            return match bytes.first() {
                Some(b'S') => {
                    let Some(sock) = self.stream().socket() else {
                        let payload = self.failure("postgres: the plain stream holds no socket to upgrade", "failed");
                        return self.fail(payload);
                    };
                    let Ok(target) = tcp_target_allowed(&self.spec.origin, &self.egress) else {
                        let payload = self.failure("postgres: the origin left egress", "refused");
                        return self.fail(payload);
                    };
                    let (tls, event) = crate::tls::TlsStream::upgrade(target.host(), sock);
                    self.stream = Some(Box::new(tls));
                    self.phase = Phase::Upgrading;
                    Flow::Event(byte_stream::of_tls(event))
                }
                Some(b'N') => {
                    let payload = self.failure("postgres: the server does not support TLS", "unsupported");
                    self.fail(payload)
                }
                _ => {
                    let payload = self.failure("postgres: the answer to the TLS request is neither S nor N", "protocol");
                    self.fail(payload)
                }
            };
        }
        self.inbox.extend(bytes);
        self.pump()
    }

    fn pump(&mut self) -> Flow {
        loop {
            match next_message(&mut self.inbox) {
                Err(message) => {
                    let payload = self.failure(&format!("postgres: {message}"), "protocol");
                    return self.fail(payload);
                }
                Ok(None) => return Flow::Event(self.stream().recv()),
                Ok(Some(msg)) => {
                    if let Some(flow) = self.handle(msg) {
                        return flow;
                    }
                }
            }
        }
    }

    fn protocol(&mut self, message: &str) -> Option<Flow> {
        let payload = self.failure(&format!("postgres: {message}"), "protocol");
        Some(self.fail(payload))
    }

    fn handle(&mut self, msg: Backend) -> Option<Flow> {
        match (self.phase, msg.kind) {
            (_, b'N' | b'S' | b'K' | b'A') => None,
            (Phase::Startup, b'R') => self.authentication(&msg.body),
            (Phase::Startup, b'E') => {
                self.error = Some(parse_error(&msg.body));
                Some(self.server_failure())
            }
            (Phase::Startup, b'Z') => {
                let bytes = query_messages(&self.spec.sql, &self.spec.params);
                Some(Flow::Event(self.send_phase(bytes, Phase::QuerySend)))
            }
            (Phase::Query, b'1' | b'2' | b'n' | b'I' | b's') => None,
            (Phase::Query, b'T') => match parse_row_description(&msg.body) {
                Ok(columns) => {
                    self.columns = columns;
                    None
                }
                Err(message) => self.protocol(&message),
            },
            (Phase::Query, b'D') => match parse_data_row(&msg.body) {
                Ok(row) => {
                    self.rows.push(row);
                    None
                }
                Err(message) => self.protocol(&message),
            },
            (Phase::Query, b'C') => {
                self.tag = read_cstr(&msg.body, 0).ok().map(|(tag, _)| tag);
                None
            }
            (Phase::Query, b'E') => {
                self.error = Some(parse_error(&msg.body));
                None
            }
            (Phase::Query, b'Z') => Some(self.finish()),
            (phase, kind) => self.protocol(&format!("unexpected message `{}` while {phase:?}", char::from(kind))),
        }
    }

    fn finish(&mut self) -> Flow {
        if self.error.is_some() {
            return self.server_failure();
        }
        let outcome = match result_value(&self.columns, &self.rows, self.tag.as_deref()) {
            Ok(Value::Object(mut result)) => {
                result.insert("key".to_string(), self.key.clone().map_or(Value::Null, Value::String));
                Ok(Value::Object(result))
            }
            Ok(other) => Ok(other),
            Err(ValueProblem(message)) => Err(failure_payload(&message, "unsupported", None, None, &self.key)),
        };
        self.outcome = Some(outcome);
        Flow::Event(self.send_phase(message(b'X', &[]), Phase::Terminating))
    }

    fn need_password(&mut self) -> Result<String, Flow> {
        match self.password.clone() {
            Some(password) => Ok(password),
            None => {
                let payload = failure_payload("postgres: the server asks for a password and the call names no auth credential", "unconfigured", None, None, &self.key);
                Err(self.fail(payload))
            }
        }
    }

    fn authentication(&mut self, body: &[u8]) -> Option<Flow> {
        let code = match be_i32(body, 0) {
            Ok(code) => code,
            Err(message) => return self.protocol(&message),
        };
        match code {
            0 => None,
            3 => {
                let password = match self.need_password() {
                    Ok(p) => p,
                    Err(flow) => return Some(flow),
                };
                let mut reply = Vec::new();
                cstr(&mut reply, &password);
                Some(Flow::Event(self.send_phase(message(b'p', &reply), Phase::AuthSend)))
            }
            5 => {
                let password = match self.need_password() {
                    Ok(p) => p,
                    Err(flow) => return Some(flow),
                };
                let Some(salt) = body.get(4..8) else {
                    return self.protocol("an MD5 authentication request carries no salt");
                };
                let mut reply = Vec::new();
                cstr(&mut reply, &md5_password(&self.spec.user, &password, salt));
                Some(Flow::Event(self.send_phase(message(b'p', &reply), Phase::AuthSend)))
            }
            10 => {
                if let Err(flow) = self.need_password() {
                    return Some(flow);
                }
                let mut at = 4;
                let mut offered = Vec::new();
                while let Ok((name, next)) = read_cstr(body, at) {
                    if name.is_empty() {
                        break;
                    }
                    offered.push(name);
                    at = next;
                }
                if !offered.iter().any(|m| m == SCRAM) {
                    let payload = self.failure(&format!("postgres: the server offers only {offered:?}; this driver speaks {SCRAM}"), "unsupported");
                    return Some(self.fail(payload));
                }
                self.phase = Phase::Nonce;
                Some(Flow::Step(DriverStep::Ask(DriverAsk::Random { bytes: 18 })))
            }
            11 => {
                let password = match self.need_password() {
                    Ok(p) => p,
                    Err(flow) => return Some(flow),
                };
                let server_first = String::from_utf8_lossy(&body[4..]).into_owned();
                let Some(scram) = self.scram.as_mut() else {
                    return self.protocol("a SCRAM continuation arrived before the SCRAM start");
                };
                match scram.proof(&password, &server_first) {
                    Ok(final_message) => Some(Flow::Event(self.send_phase(message(b'p', final_message.as_bytes()), Phase::AuthSend))),
                    Err(message) => {
                        let payload = self.failure(&format!("postgres: {message}"), "protocol");
                        Some(self.fail(payload))
                    }
                }
            }
            12 => {
                let server_final = String::from_utf8_lossy(&body[4..]).into_owned();
                let verdict = self.scram.as_ref().map(|s| s.verify(&server_final));
                match verdict {
                    Some(Ok(())) => None,
                    Some(Err(message)) => {
                        let payload = self.failure(&format!("postgres: {message}"), "protocol");
                        Some(self.fail(payload))
                    }
                    None => self.protocol("a SCRAM final message arrived before the SCRAM start"),
                }
            }
            other => {
                let payload = self.failure(&format!("postgres: authentication method {other} is not supported (cleartext, md5 and {SCRAM} are)"), "unsupported");
                Some(self.fail(payload))
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::os::{OsError, OsErrorKind, OsOp};

    fn text(s: &str) -> Value {
        Value::String(s.to_string())
    }

    fn egress() -> Vec<String> {
        vec!["postgres://db.internal:5432".to_string(), "postgresql://alt.internal".to_string()]
    }

    fn spec(sql: &str) -> PostgresCallSpec {
        PostgresCallSpec {
            origin: "postgres://db.internal".to_string(),
            sql: sql.to_string(),
            params: Vec::new(),
            auth: None,
            user: "app".to_string(),
            database: Some("main".to_string()),
            tls: false,
            timeout_ms: None,
        }
    }

    fn msg(kind: u8, body: &[u8]) -> Vec<u8> {
        message(kind, body)
    }

    fn auth(code: i32, extra: &[u8]) -> Vec<u8> {
        let mut body = code.to_be_bytes().to_vec();
        body.extend_from_slice(extra);
        msg(b'R', &body)
    }

    fn ready() -> Vec<u8> {
        msg(b'Z', b"I")
    }

    fn parameter_status() -> Vec<u8> {
        let mut body = Vec::new();
        cstr(&mut body, "server_version");
        cstr(&mut body, "16.4");
        msg(b'S', &body)
    }

    fn server_error(code: &str, message_text: &str) -> Vec<u8> {
        let mut body = vec![b'S'];
        cstr(&mut body, "ERROR");
        body.push(b'C');
        cstr(&mut body, code);
        body.push(b'M');
        cstr(&mut body, message_text);
        body.push(0);
        msg(b'E', &body)
    }

    fn row_description(columns: &[(&str, u32)]) -> Vec<u8> {
        let mut body = (columns.len() as i16).to_be_bytes().to_vec();
        for (name, oid) in columns {
            cstr(&mut body, name);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&0i16.to_be_bytes());
            body.extend_from_slice(&oid.to_be_bytes());
            body.extend_from_slice(&(-1i16).to_be_bytes());
            body.extend_from_slice(&(-1i32).to_be_bytes());
            body.extend_from_slice(&0i16.to_be_bytes());
        }
        msg(b'T', &body)
    }

    fn data_row(cells: &[Option<&str>]) -> Vec<u8> {
        let mut body = (cells.len() as i16).to_be_bytes().to_vec();
        for cell in cells {
            match cell {
                None => body.extend_from_slice(&(-1i32).to_be_bytes()),
                Some(t) => {
                    body.extend_from_slice(&(t.len() as i32).to_be_bytes());
                    body.extend_from_slice(t.as_bytes());
                }
            }
        }
        msg(b'D', &body)
    }

    fn complete(tag: &str) -> Vec<u8> {
        let mut body = Vec::new();
        cstr(&mut body, tag);
        msg(b'C', &body)
    }

    fn query_reply(columns: &[(&str, u32)], rows: &[Vec<Option<&str>>], tag: &str) -> Vec<u8> {
        let mut out = msg(b'1', &[]);
        out.extend(msg(b'2', &[]));
        out.extend(row_description(columns));
        for row in rows {
            out.extend(data_row(row));
        }
        out.extend(complete(tag));
        out.extend(ready());
        out
    }

    fn trusted_startup() -> Vec<u8> {
        let mut out = auth(0, &[]);
        out.extend(parameter_status());
        out.extend(msg(b'K', &[0, 0, 0, 1, 0, 0, 0, 2]));
        out.extend(ready());
        out
    }

    type Reply = Box<dyn Fn(&[Vec<u8>]) -> Vec<u8>>;

    fn bytes(b: Vec<u8>) -> Reply {
        Box::new(move |_| b.clone())
    }

    /// Runs the driver against scripted server replies: each `os/recv` takes the next reply, built from the bytes sent so far.
    fn run(call: PostgresCallSpec, password: Option<&str>, replies: Vec<Reply>) -> (Vec<DriverAsk>, DriverStep, Vec<Vec<u8>>) {
        use orbital_core_base::codec::{base64_decode, base64_encode};
        let (mut driver, mut step) = PostgresDriver::start(call, egress(), None);
        let mut asks = Vec::new();
        let mut sent: Vec<Vec<u8>> = Vec::new();
        let mut replies = replies.into_iter();
        loop {
            let DriverStep::Ask(ask) = step.clone() else {
                return (asks, step, sent);
            };
            asks.push(ask.clone());
            let answer = match &ask {
                DriverAsk::Secret { .. } => DriverAnswer::Secret(Ok(password.map(str::to_string))),
                DriverAsk::Random { bytes } => DriverAnswer::Random(Ok(vec![7u8; *bytes as usize])),
                DriverAsk::Os { op: OsOp::TcpConnect, .. } => DriverAnswer::Os(Ok(Value::Object(ValueMap::from_iter([("sock".to_string(), Value::Number(5.0))])))),
                DriverAsk::Os { op: OsOp::Send, args, .. } => {
                    let Value::String(b64) = &args[1] else { panic!("send data") };
                    sent.push(base64_decode(b64).expect("base64"));
                    DriverAnswer::Os(Ok(Value::Null))
                }
                DriverAsk::Os { op: OsOp::Recv, .. } => {
                    let Some(reply) = replies.next() else { return (asks, step, sent) };
                    DriverAnswer::Os(Ok(Value::Object(ValueMap::from_iter([
                        ("data".to_string(), text(&base64_encode(&reply(&sent)))),
                        ("eof".to_string(), Value::Bool(false)),
                    ]))))
                }
                DriverAsk::Os { op: OsOp::Close, .. } => DriverAnswer::Os(Ok(Value::Object(ValueMap::default()))),
                DriverAsk::Clock => DriverAnswer::Clock(Ok(1_760_000_000_000)),
                other => panic!("unexpected ask {other:?}"),
            };
            step = driver.resume(answer);
        }
    }

    fn field<'a>(payload: &'a Value, name: &str) -> &'a Value {
        payload.get_path(name).unwrap_or_else(|| panic!("no `{name}` in {payload:?}"))
    }

    fn done_ok(step: &DriverStep) -> &Value {
        match step {
            DriverStep::Done(Ok(v)) => v,
            other => panic!("expected success, got {other:?}"),
        }
    }

    fn done_err(step: &DriverStep) -> &Value {
        match step {
            DriverStep::Done(Err(v)) => v,
            other => panic!("expected failure, got {other:?}"),
        }
    }

    fn ops(asks: &[DriverAsk]) -> Vec<&'static str> {
        asks.iter()
            .map(|a| match a {
                DriverAsk::Os { op, .. } => op.name(),
                DriverAsk::Secret { .. } => "secret",
                DriverAsk::Random { .. } => "random",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn the_startup_message_names_the_user_the_database_and_the_statement_timeout() {
        let mut call = spec("SELECT 1");
        call.timeout_ms = Some(2500);
        let m = startup_message(&call);
        assert_eq!(u32::from_be_bytes([m[0], m[1], m[2], m[3]]) as usize, m.len());
        assert_eq!(&m[4..8], &196_608u32.to_be_bytes());
        let body = String::from_utf8_lossy(&m[8..]).into_owned();
        assert_eq!(body, "user\0app\0database\0main\0client_encoding\0UTF8\0options\0-c statement_timeout=2500\0\0");
        let mut plain = spec("SELECT 1");
        plain.database = None;
        assert!(String::from_utf8_lossy(&startup_message(&plain)).contains("database\0app\0"), "the database defaults to the user");
        assert!(!String::from_utf8_lossy(&startup_message(&plain)).contains("statement_timeout"));
    }

    #[test]
    fn a_trusted_server_runs_one_statement_with_text_parameters_and_returns_typed_rows() {
        let mut call = spec("SELECT $1::int + 1 AS n, $2::text AS t, NULL::int AS z");
        call.params = vec![Some("41".to_string()), None];
        let reply = query_reply(&[("n", 23), ("t", 25), ("z", 23)], &[vec![Some("42"), None, None]], "SELECT 1");
        let (asks, step, sent) = run(call.clone(), None, vec![bytes(trusted_startup()), bytes(reply)]);
        assert_eq!(
            ops(&asks),
            ["os/tcp-connect", "os/send", "os/recv", "os/send", "os/recv", "os/send", "os/close"],
            "connect, startup, query, terminate, close"
        );
        assert_eq!(sent[0], startup_message(&call));
        assert_eq!(sent[1], query_messages(&call.sql, &call.params));
        assert_eq!(sent[2], msg(b'X', &[]));
        let result = done_ok(&step);
        assert_eq!(field(result, "rowCount"), &Value::Number(1.0));
        let row = field(result, "rows");
        assert_eq!(row, &Value::Array(vec![Value::Object(ValueMap::from_iter([
            ("n".to_string(), Value::Number(42.0)),
            ("t".to_string(), Value::Null),
            ("z".to_string(), Value::Null),
        ]))]));
        assert_eq!(
            field(result, "columns"),
            &Value::Array(vec!["n", "t", "z"].into_iter().zip([23.0, 25.0, 23.0]).map(|(n, o)| Value::Object(ValueMap::from_iter([
                ("name".to_string(), text(n)),
                ("typeOid".to_string(), Value::Number(o)),
            ]))).collect())
        );
    }

    #[test]
    fn the_query_message_parses_binds_describes_executes_and_syncs_in_text_format() {
        let m = query_messages("SELECT $1", &[Some("a".to_string()), None]);
        let kinds: Vec<u8> = {
            let mut buf = m.clone();
            let mut kinds = Vec::new();
            while let Some(b) = next_message(&mut buf).expect("frames") {
                kinds.push(b.kind);
            }
            kinds
        };
        assert_eq!(kinds, b"PBDES");
        let bind = &m[m.windows(2).position(|w| w == [b'B', 0]).expect("bind")..];
        assert!(bind.windows(5).any(|w| w == [0, 0, 0, 1, b'a']), "the text value is length-prefixed");
        assert!(bind.windows(4).any(|w| w == [0xff, 0xff, 0xff, 0xff]), "NULL is length -1");
    }

    #[test]
    fn a_statement_cut_across_reads_asks_for_more_until_the_ready_message() {
        let reply = query_reply(&[("n", 23)], &[vec![Some("1")], vec![Some("2")]], "SELECT 2");
        let (first, second) = reply.split_at(reply.len() / 2);
        let (asks, step, _) = run(spec("SELECT n"), None, vec![bytes(trusted_startup()), bytes(first.to_vec()), bytes(second.to_vec())]);
        assert_eq!(asks.iter().filter(|a| matches!(a, DriverAsk::Os { op: OsOp::Recv, .. })).count(), 3);
        assert_eq!(field(done_ok(&step), "rowCount"), &Value::Number(2.0));
    }

    #[test]
    fn a_server_error_is_a_typed_failure_with_its_sqlstate_and_the_connection_still_closes() {
        let mut reply = msg(b'1', &[]);
        reply.extend(server_error("42P01", "relation \"nope\" does not exist"));
        reply.extend(ready());
        let (asks, step, _) = run(spec("SELECT * FROM nope"), None, vec![bytes(trusted_startup()), bytes(reply)]);
        let payload = done_err(&step);
        assert_eq!(field(payload, "kind"), &text("postgres"));
        assert_eq!(field(payload, "code"), &text("42P01"));
        assert_eq!(field(payload, "error"), &text("relation \"nope\" does not exist"));
        assert_eq!(asks.last().map(|a| matches!(a, DriverAsk::Os { op: OsOp::Close, .. })), Some(true));
    }

    #[test]
    fn an_error_during_startup_is_the_failure_and_the_statement_is_never_sent() {
        let (asks, step, sent) = run(spec("SELECT 1"), None, vec![bytes(server_error("3D000", "database \"main\" does not exist"))]);
        assert_eq!(field(done_err(&step), "code"), &text("3D000"));
        assert_eq!(sent.len(), 1, "only the startup message went out");
        assert_eq!(asks.last().map(|a| matches!(a, DriverAsk::Os { op: OsOp::Close, .. })), Some(true));
    }

    #[test]
    fn a_cleartext_password_goes_out_in_a_password_message() {
        let mut call = spec("SELECT 1");
        call.auth = Some(CredentialSlot::from("PG_PASSWORD"));
        let (_, step, sent) = run(call, Some("secret"), vec![bytes(auth(3, &[])), bytes(trusted_startup()), bytes(query_reply(&[("a", 23)], &[vec![Some("1")]], "SELECT 1"))]);
        assert_eq!(sent[1], msg(b'p', b"secret\0"));
        assert!(matches!(step, DriverStep::Done(Ok(_))));
    }

    #[test]
    fn an_md5_password_is_the_salted_double_hash() {
        assert_eq!(md5_password("postgres", "secret", &[1, 2, 3, 4]), "md5bb41a296aab6baccb36ff243a562abff");
        let mut call = spec("SELECT 1");
        call.auth = Some(CredentialSlot::from("PG_PASSWORD"));
        let (_, _, sent) = run(call, Some("secret"), vec![bytes(auth(5, &[1, 2, 3, 4])), bytes(trusted_startup()), bytes(query_reply(&[("a", 23)], &[vec![Some("1")]], "SELECT 1"))]);
        assert_eq!(sent[1], msg(b'p', format!("{}\0", md5_password("app", "secret", &[1, 2, 3, 4])).as_bytes()));
    }

    #[test]
    fn scram_sha_256_matches_the_rfc_7677_example() {
        let mut scram = Scram { client_first_bare: "n=user,r=rOprNGfwEbeRWgbNEkqO".to_string(), nonce: "rOprNGfwEbeRWgbNEkqO".to_string(), expected_server_signature: Vec::new() };
        let final_message = scram
            .proof("pencil", "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096")
            .expect("proof");
        assert_eq!(final_message, "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=");
        assert!(scram.verify("v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=").is_ok());
        assert!(scram.verify("v=AAAA").is_err(), "a wrong server signature is refused");
    }

    #[test]
    fn controls_scram_refuses_a_foreign_nonce_and_an_absurd_iteration_count() {
        let mut scram = Scram::start("u", "abc").0;
        assert!(scram.proof("pw", "r=xyz,s=c2FsdA==,i=4096").is_err());
        assert!(scram.proof("pw", "r=abcdef,s=c2FsdA==,i=0").is_err());
        assert!(scram.proof("pw", "r=abcdef,s=c2FsdA==,i=99999999").is_err());
        assert!(scram.proof("pw", "r=abcdef,s=c2FsdA==").is_err());
    }

    /// A server that speaks SCRAM-SHA-256 for `password`: it reads the client's messages from what was sent so far.
    fn scram_server(password: &'static str, wrong_signature: bool) -> Vec<Reply> {
        let salt = b"0123456789abcdef";
        let iterations = 4096;
        let server_nonce = "SERVERPART";
        let first: Reply = Box::new(|_| auth(10, b"SCRAM-SHA-256\0\0"));
        let second: Reply = Box::new(move |sent| {
            let body = &sent[1][5..];
            let nul = body.iter().position(|b| *b == 0).expect("mechanism");
            let client_first = String::from_utf8_lossy(&body[nul + 5..]).into_owned();
            let bare = client_first.strip_prefix("n,,").expect("gs2 header").to_string();
            let client_nonce = bare.split("r=").nth(1).expect("nonce").to_string();
            let server_first = format!("r={client_nonce}{server_nonce},s={},i={iterations}", codec::base64_encode(salt));
            auth(11, server_first.as_bytes())
        });
        let third: Reply = Box::new(move |sent| {
            let first_body = &sent[1][5..];
            let nul = first_body.iter().position(|b| *b == 0).expect("mechanism");
            let bare = String::from_utf8_lossy(&first_body[nul + 5..]).trim_start_matches("n,,").to_string();
            let client_nonce = bare.split("r=").nth(1).expect("nonce").to_string();
            let server_first = format!("r={client_nonce}SERVERPART,s={},i=4096", codec::base64_encode(b"0123456789abcdef"));
            let client_final = String::from_utf8_lossy(&sent[2][5..]).into_owned();
            let without_proof = client_final.rsplit_once(",p=").expect("proof").0.to_string();
            let auth_message = format!("{bare},{server_first},{without_proof}");
            let salted = codec::pbkdf2_hmac_sha256(password.as_bytes(), b"0123456789abcdef", 4096, 32);
            let server_key = codec::hmac_sha256(&salted, b"Server Key");
            let mut signature = codec::hmac_sha256(&server_key, auth_message.as_bytes()).to_vec();
            if wrong_signature {
                signature[0] ^= 1;
            }
            let mut out = auth(12, format!("v={}", codec::base64_encode(&signature)).as_bytes());
            out.extend(trusted_startup());
            out
        });
        vec![first, second, third]
    }

    #[test]
    fn a_scram_login_proves_the_password_and_verifies_the_server_before_the_statement_runs() {
        let mut call = spec("SELECT 1");
        call.auth = Some(CredentialSlot::from("PG_PASSWORD"));
        let mut replies = scram_server("pencil", false);
        replies.push(bytes(query_reply(&[("a", 23)], &[vec![Some("1")]], "SELECT 1")));
        let (asks, step, _) = run(call, Some("pencil"), replies);
        assert!(ops(&asks).contains(&"random"), "the client nonce is a journaled Random ask");
        assert_eq!(field(done_ok(&step), "rowCount"), &Value::Number(1.0));
    }

    #[test]
    fn control_a_server_that_does_not_know_the_password_is_refused_and_no_statement_is_sent() {
        let mut call = spec("SELECT 1");
        call.auth = Some(CredentialSlot::from("PG_PASSWORD"));
        let (_, step, sent) = run(call, Some("pencil"), scram_server("pencil", true));
        assert_eq!(field(done_err(&step), "kind"), &text("protocol"));
        assert_eq!(sent.len(), 3, "startup, SASL initial response and SASL response only");
    }

    #[test]
    fn a_server_that_wants_a_password_the_call_does_not_name_is_unconfigured() {
        let (_, step, _) = run(spec("SELECT 1"), None, vec![bytes(auth(3, &[]))]);
        assert_eq!(field(done_err(&step), "kind"), &text("unconfigured"));
        let (_, step, _) = run(spec("SELECT 1"), None, vec![bytes(auth(10, b"SCRAM-SHA-256\0\0"))]);
        assert_eq!(field(done_err(&step), "kind"), &text("unconfigured"));
    }

    #[test]
    fn an_authentication_method_the_driver_does_not_speak_is_unsupported() {
        let (_, step, _) = run(spec("SELECT 1"), None, vec![bytes(auth(7, &[]))]);
        assert_eq!(field(done_err(&step), "kind"), &text("unsupported"));
        let mut call = spec("SELECT 1");
        call.auth = Some(CredentialSlot::from("P"));
        let (_, step, _) = run(call, Some("pw"), vec![bytes(auth(10, b"SCRAM-SHA-256-PLUS\0\0"))]);
        assert_eq!(field(done_err(&step), "kind"), &text("unsupported"));
    }

    #[test]
    fn a_tls_call_sends_the_ssl_request_first_and_upgrades_the_same_socket_on_s() {
        let mut call = spec("SELECT 1");
        call.tls = true;
        let (asks, _, sent) = run(call.clone(), None, vec![bytes(b"S".to_vec())]);
        assert_eq!(sent[0], SSL_REQUEST.to_vec());
        let after_request: Vec<&DriverAsk> = asks.iter().skip_while(|a| !matches!(a, DriverAsk::Random { bytes: 32 })).collect();
        assert!(matches!(after_request.as_slice(), [DriverAsk::Random { bytes: 32 }, DriverAsk::Clock, DriverAsk::Os { op: OsOp::Send, .. }, ..]), "the TLS seed, clock and ClientHello follow the S: {asks:?}");
        let (_, step, _) = run(call, None, vec![bytes(b"N".to_vec())]);
        assert_eq!(field(done_err(&step), "kind"), &text("unsupported"));
    }

    #[test]
    fn control_a_plain_call_never_sends_an_ssl_request() {
        let (_, _, sent) = run(spec("SELECT 1"), None, vec![bytes(trusted_startup())]);
        assert_ne!(sent[0], SSL_REQUEST.to_vec());
    }

    #[test]
    fn an_origin_outside_egress_a_wrong_scheme_and_a_missing_credential_end_before_the_first_data_ask() {
        let mut outside = spec("SELECT 1");
        outside.origin = "postgres://other.internal".to_string();
        let (asks, step, _) = run(outside, None, Vec::new());
        assert!(asks.is_empty());
        assert_eq!(field(done_err(&step), "kind"), &text("refused"));
        let mut redis = spec("SELECT 1");
        redis.origin = "redis://db.internal:5432".to_string();
        let (asks, step, _) = run(redis, None, Vec::new());
        assert!(asks.is_empty());
        assert_eq!(field(done_err(&step), "kind"), &text("refused"));
        let mut named = spec("SELECT 1");
        named.auth = Some(CredentialSlot::from("PG_PASSWORD"));
        let (asks, step, _) = run(named, None, Vec::new());
        assert_eq!(ops(&asks), ["secret"]);
        assert_eq!(field(done_err(&step), "kind"), &text("unconfigured"));
        assert_eq!(field(done_err(&step), "credential"), &text("PG_PASSWORD"));
    }

    #[test]
    fn the_postgresql_scheme_is_accepted_like_postgres() {
        let mut call = spec("SELECT 1");
        call.origin = "postgresql://alt.internal".to_string();
        let (asks, _, _) = run(call, None, Vec::new());
        assert_eq!(ops(&asks)[0], "os/tcp-connect");
    }

    #[test]
    fn a_read_timeout_and_a_server_that_hangs_up_are_typed_failures_that_close() {
        let (mut driver, mut step) = PostgresDriver::start(spec("SELECT 1"), egress(), None);
        let mut closed = false;
        for _ in 0..8 {
            let DriverStep::Ask(ask) = step.clone() else { break };
            let answer = match ask {
                DriverAsk::Os { op: OsOp::TcpConnect, .. } => DriverAnswer::Os(Ok(Value::Object(ValueMap::from_iter([("sock".to_string(), Value::Number(5.0))])))),
                DriverAsk::Os { op: OsOp::Recv, .. } => DriverAnswer::Os(Err(OsError::new(OsErrorKind::Timeout, "os/recv: timed out"))),
                DriverAsk::Os { op: OsOp::Close, .. } => {
                    closed = true;
                    DriverAnswer::Os(Ok(Value::Null))
                }
                _ => DriverAnswer::Os(Ok(Value::Null)),
            };
            step = driver.resume(answer);
        }
        assert_eq!(field(done_err(&step), "kind"), &text("timeout"));
        assert!(closed);
    }

    #[test]
    fn a_garbled_message_is_a_protocol_failure() {
        let (_, step, _) = run(spec("SELECT 1"), None, vec![bytes(vec![b'R', 0, 0, 0, 2, 0])]);
        assert_eq!(field(done_err(&step), "kind"), &text("protocol"));
    }

    #[test]
    fn the_call_parses_its_options_and_refuses_what_the_guard_or_the_shape_refuses() {
        let options = |entries: Vec<(&str, Value)>| Value::Object(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
        let ok = parse_postgres_call(
            &text("postgres://h"),
            &text("SELECT $1"),
            &Value::Array(vec![Value::Number(1.5), Value::Bool(true), Value::Null, text("s"), Value::Array(vec![text("a\"b"), Value::Null])]),
            &options(vec![("user", text("app")), ("auth", text("PW")), ("database", text("d")), ("tls", Value::Bool(true)), ("timeoutMs", Value::Number(5000.0))]),
        )
        .expect("parses");
        assert_eq!(ok.params, vec![Some("1.5".to_string()), Some("true".to_string()), None, Some("s".to_string()), Some("{\"a\\\"b\",NULL}".to_string())]);
        assert!(ok.tls && ok.timeout_ms == Some(5000) && ok.auth.is_some());
        let call = |sql: &str, params: Value, opts: Value| parse_postgres_call(&text("postgres://h"), &text(sql), &params, &opts);
        let user = options(vec![("user", text("app"))]);
        for (sql, why) in [("DELETE FROM t", "Only SELECT"), ("SELECT 1; SELECT 2", "single statement"), ("SELECT * INTO c FROM t", "INTO")] {
            let error = call(sql, Value::Array(Vec::new()), user.clone()).expect_err(sql);
            assert!(error.message().contains("Read-only violation") && error.message().contains(why), "{}", error.message());
        }
        assert!(call("SELECT 1", Value::Array(Vec::new()), user.clone()).is_ok());
        assert!(call("SELECT 1", text("x"), user.clone()).is_err(), "params are a list");
        assert!(call("SELECT 1", Value::Array(Vec::new()), options(vec![])).is_err(), "a user is required");
        assert!(call("SELECT 1", Value::Array(Vec::new()), options(vec![("user", text("a")), ("timeout", Value::Number(1.0))])).is_err());
        assert!(call("SELECT 1", Value::Array(Vec::new()), options(vec![("user", text("a")), ("timeoutMs", Value::Number(0.0))])).is_err());
        assert!(call("SELECT 1", Value::Array(Vec::new()), options(vec![("user", text("a")), ("tls", text("yes"))])).is_err());
    }

    #[test]
    fn cells_decode_by_type_oid() {
        let cell = |oid: u32, t: &str| cell_value(oid, Some(t.as_bytes())).expect(t);
        assert_eq!(cell(16, "t"), Value::Bool(true));
        assert_eq!(cell(21, "-7"), Value::Number(-7.0));
        assert_eq!(cell(23, "42"), Value::Number(42.0));
        assert_eq!(cell(20, "123"), Value::Number(123.0));
        assert_eq!(cell(20, "123456789012345678"), text("123456789012345678"), "an int8 past 2^53 stays text");
        assert_eq!(cell(700, "0.1"), Value::Number(0.1));
        assert_eq!(cell(701, "2.25"), Value::Number(2.25));
        assert_eq!(cell(701, "NaN"), Value::Null);
        assert_eq!(cell(701, "-Infinity"), Value::Null);
        assert_eq!(cell(1700, "-12.50"), text("-12.50"));
        assert_eq!(cell(25, "h\u{e9}llo"), text("h\u{e9}llo"));
        assert_eq!(cell(114, "{\"b\": 1}"), text("{\"b\": 1}"));
        assert_eq!(cell(2950, "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"), text("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"));
        assert_eq!(cell(17, "\\x0102"), text("AQI="));
        assert_eq!(cell(1114, "2025-10-09 08:53:20"), text("2025-10-09T08:53:20"));
        assert_eq!(cell(1082, "2025-10-09"), text("2025-10-09"));
        assert_eq!(cell(1186, "1 day"), text("1 day"), "an unmapped type is the text Postgres prints");
        assert_eq!(cell_value(23, None), Ok(Value::Null));
    }

    #[test]
    fn timestamptz_is_normalised_to_utc_iso_text() {
        let iso = |t: &str| match cell_value(1184, Some(t.as_bytes())).expect(t) {
            Value::String(s) => s,
            other => panic!("{other:?}"),
        };
        assert_eq!(iso("2025-10-09 08:53:20.123456+00"), "2025-10-09T08:53:20.123456Z");
        assert_eq!(iso("2025-10-09 08:53:20+02"), "2025-10-09T06:53:20Z");
        assert_eq!(iso("2025-10-09 00:30:00+05:30"), "2025-10-08T19:00:00Z");
        assert_eq!(iso("2025-01-01 01:00:00-03"), "2025-01-01T04:00:00Z");
        assert_eq!(iso("2024-02-29 23:59:59.5+00"), "2024-02-29T23:59:59.5Z");
        assert_eq!(iso("1999-12-31 23:59:59.999999+00"), "1999-12-31T23:59:59.999999Z");
        assert_eq!(iso("infinity"), "infinity");
        assert!(cell_value(1184, Some(b"garbled")).is_err());
    }

    #[test]
    fn arrays_decode_to_nested_lists_of_typed_elements() {
        let array = |oid: u32, t: &str| cell_value(oid, Some(t.as_bytes())).expect(t);
        assert_eq!(array(1007, "{1,2,NULL}"), Value::Array(vec![Value::Number(1.0), Value::Number(2.0), Value::Null]));
        assert_eq!(array(1009, "{\"a b\",NULL,\"q\\\"x\",plain}"), Value::Array(vec![text("a b"), Value::Null, text("q\"x"), text("plain")]));
        assert_eq!(array(1000, "{t,f}"), Value::Array(vec![Value::Bool(true), Value::Bool(false)]));
        assert_eq!(array(1007, "{{1,2},{3,4}}"), Value::Array(vec![Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]), Value::Array(vec![Value::Number(3.0), Value::Number(4.0)])]));
        assert_eq!(array(1007, "{}"), Value::Array(Vec::new()));
        assert_eq!(array(1016, "{1,2}"), Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]));
        assert!(cell_value(1007, Some(b"{{{{1}}}}")).is_err(), "deeper than the limit");
        assert!(cell_value(1007, Some(b"{1,2")).is_err());
        assert_eq!(array(1007, "[0:1]={5,6}"), Value::Array(vec![Value::Number(5.0), Value::Number(6.0)]), "the bounds prefix is dropped");
    }

    #[test]
    fn the_command_tag_gives_the_row_count_and_a_missing_tag_falls_back_to_the_rows_held() {
        let columns = [Column { name: "a".to_string(), type_oid: 23 }];
        let rows = vec![vec![Some(b"1".to_vec())], vec![Some(b"2".to_vec())]];
        assert_eq!(result_value(&columns, &rows, Some("SELECT 2")).expect("ok").get_path("rowCount"), Some(&Value::Number(2.0)));
        assert_eq!(result_value(&columns, &rows, None).expect("ok").get_path("rowCount"), Some(&Value::Number(2.0)));
    }
}
