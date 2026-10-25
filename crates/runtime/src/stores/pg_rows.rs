//! The Postgres row store: a kernel store driver over a raw socket port, the Rust twin of `@almadar/db/postgres`
//! (`PostgresPersistence`, `rows.ts`). It keeps that adapter's layout exactly, so either side reads the other's rows:
//!
//! - an entity's table is its snake_case name, pluralized (`TimeEntry` -> `time_entries`, `Ticket` -> `tickets`);
//! - a row is one table row: `id text` plus one column per field, and the stamps `createdAt` / `updatedAt` live in
//!   the framework columns `created_at` / `updated_at` unless the table declares a column of the stamp's own name;
//! - an object or list value is stored as JSON text, a `jsonb` column reads back as a value, and a stamp reads back
//!   as an ISO-8601 string with millisecond precision (`Date.prototype.toISOString`).
//!
//! The wire client reuses the `postgres` effect's message codec and SCRAM (`postgres.rs`), but it is a different use:
//! a session pool that runs many statements, with transactions and writes. The `postgres` effect's read-only guard
//! (`sql_guard`) stays that effect's one guard; this store is a kernel-internal use and never takes statements from a
//! program. Every bind value travels as text and the server infers its type, exactly as `postgres.rs` does.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::HashMap;
use std::sync::Arc;

use orbital_core_base::codec;
use orbital_core_base::row_set::{
    RowCompareOp, RowDirection, RowPredicate, RowQuery, RowTotalMode,
};
use orbital_core_base::str_utils::js_number_string;
use orbital_core_base::value::{Value, ValueMap};
use orbital_core_base::sync::{Mutex, RwLock};

use crate::entity_source::EntitySource;
use crate::postgres::{
    cell_value, cstr, md5_password, message, next_message, parse_data_row, parse_error,
    parse_row_description, Column, Scram, SSL_REQUEST,
};
use crate::row_query::{
    effective_row_order, QueryCapabilities, RowIdPage, RowPage, RowProjection, RowWriteBatch,
    RowWriteFailureMode, RowWriteOp, RowWriteOutcome, WriteCapabilities,
};

// ---- the raw port ----

/// One open connection: the bytes the server sends and the bytes sent to it.
pub trait PgConn: Send {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String>;
    /// At least one byte, blocking; an empty reply means the server closed the connection.
    fn recv(&mut self) -> Result<Vec<u8>, String>;
}

/// The host's socket port.
pub trait PgPort: Send + Sync {
    fn connect(&self, host: &str, port: u16) -> Result<Box<dyn PgConn>, String>;
}

/// A plain TCP port (std sockets).
#[derive(Debug, Default, Clone, Copy)]
pub struct TcpPgPort;

struct TcpConn(std::net::TcpStream);

impl PgConn for TcpConn {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        use std::io::Write;
        self.0.write_all(bytes).map_err(|e| e.to_string())
    }

    fn recv(&mut self) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let mut buf = vec![0u8; 16 * 1024];
        let n = self.0.read(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(n);
        Ok(buf)
    }
}

impl PgPort for TcpPgPort {
    fn connect(&self, host: &str, port: u16) -> Result<Box<dyn PgConn>, String> {
        let stream = std::net::TcpStream::connect((host, port))
            .map_err(|e| format!("cannot connect to {host}:{port}: {e}"))?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        Ok(Box::new(TcpConn(stream)))
    }
}

// ---- connection settings ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: String,
    pub ssl: SslMode,
    /// PEM text of the CA the server's certificate must chain to (`sslrootcert`); the web roots when absent. Only
    /// `verify-ca` / `verify-full` read it.
    pub root_cert_pem: Option<String>,
    /// The `sslrootcert` path the connection string named, for the host to read into `root_cert_pem`.
    pub root_cert_path: Option<String>,
}

/// `sslmode`, with libpq's meaning: `prefer` (the default) asks for TLS and falls back to plain when the server
/// answers no; `require` insists on TLS but does not judge the server's certificate; `verify-ca` and `verify-full` check
/// the certificate against the roots and the server's name (`verify-ca` is held to `verify-full`'s check, stricter
/// than libpq).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
    VerifyFull,
}

impl PgConfig {
    /// `postgres://user:password@host:port/database?sslmode=…`, the form `DATABASE_URL` takes.
    pub fn from_url(connection_string: &str) -> Result<Self, String> {
        let url = url::Url::parse(connection_string)
            .map_err(|e| format!("DATABASE_URL is not a URL: {e}"))?;
        if !matches!(url.scheme(), "postgres" | "postgresql") {
            return Err(format!(
                "DATABASE_URL must be postgres:// or postgresql://, not {}://",
                url.scheme()
            ));
        }
        let decode = |text: &str| -> String {
            codec::decode_uri_component(text).unwrap_or_else(|_| text.to_string())
        };
        let user = if url.username().is_empty() {
            "postgres".to_string()
        } else {
            decode(url.username())
        };
        let database = url.path().trim_start_matches('/');
        let query = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        let ssl = match query("sslmode").as_deref() {
            None | Some("prefer") => SslMode::Prefer,
            Some("disable" | "allow") => SslMode::Disable,
            Some("require") => SslMode::Require,
            Some("verify-ca" | "verify-full") => SslMode::VerifyFull,
            Some(other) => {
                return Err(format!(
                    "sslmode `{other}` is not one of disable, allow, prefer, require, verify-ca, verify-full"
                ))
            }
        };
        Ok(Self {
            host: url.host_str().unwrap_or("localhost").to_string(),
            port: url.port().unwrap_or(5432),
            password: url.password().map(decode),
            database: if database.is_empty() {
                user.clone()
            } else {
                decode(database)
            },
            user,
            ssl,
            root_cert_pem: None,
            root_cert_path: query("sslrootcert"),
        })
    }
}

// ---- TLS: the kernel's sans-I/O rustls session over the port's socket ----

/// The port's socket past Postgres' in-band `SSLRequest`, spoken through [`crate::tls::TlsClient`], the same sans-I/O
/// session the `postgres` effect's upgrade uses: this only moves its ciphertext over the blocking socket.
struct TlsConn {
    inner: Box<dyn PgConn>,
    client: crate::tls::TlsClient,
    plain: Vec<u8>,
}

impl TlsConn {
    fn connect(inner: Box<dyn PgConn>, config: &PgConfig) -> Result<Self, String> {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed)
            .map_err(|e| format!("no entropy for the TLS handshake: {e}"))?;
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let tls_config = match config.ssl {
            SslMode::VerifyFull => {
                let roots = match &config.root_cert_pem {
                    Some(pem) => {
                        crate::tls::session::roots_from_pem(pem).map_err(|e| e.to_string())?
                    }
                    None => crate::tls::session::web_roots(),
                };
                crate::tls::session::client_config(roots, now).map_err(|e| e.to_string())?
            }
            _ => crate::tls::session::encrypt_only_config(now).map_err(|e| e.to_string())?,
        };
        let client = crate::tls::TlsClient::with_config(&config.host, seed, tls_config)
            .map_err(|e| e.to_string())?;
        let mut conn = Self {
            inner,
            client,
            plain: Vec::new(),
        };
        conn.handshake()?;
        Ok(conn)
    }

    /// Reads ciphertext from the socket into the session; an empty read is the socket's EOF.
    fn fill(&mut self) -> Result<bool, String> {
        let bytes = self.inner.recv()?;
        if bytes.is_empty() {
            self.client.feed_eof();
            return Ok(false);
        }
        self.client.feed(&bytes);
        Ok(true)
    }

    fn handshake(&mut self) -> Result<(), String> {
        loop {
            match self.client.poll().map_err(|e| e.to_string())? {
                crate::tls::Poll::Transmit(bytes) => self.inner.send(&bytes)?,
                crate::tls::Poll::Plaintext(bytes) => self.plain.extend(bytes),
                crate::tls::Poll::Idle => return Ok(()),
                crate::tls::Poll::NeedInbound => {
                    self.fill()?;
                }
                crate::tls::Poll::Closed => {
                    return Err("tls: the server closed during the handshake".to_string())
                }
            }
        }
    }
}

impl PgConn for TlsConn {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.client.write(bytes);
        loop {
            match self.client.poll().map_err(|e| e.to_string())? {
                crate::tls::Poll::Transmit(out) => self.inner.send(&out)?,
                crate::tls::Poll::Plaintext(more) => self.plain.extend(more),
                crate::tls::Poll::Idle => return Ok(()),
                crate::tls::Poll::NeedInbound => {
                    self.fill()?;
                }
                crate::tls::Poll::Closed => {
                    return Err("tls: the server closed the connection".to_string())
                }
            }
        }
    }

    fn recv(&mut self) -> Result<Vec<u8>, String> {
        if !self.plain.is_empty() {
            return Ok(std::mem::take(&mut self.plain));
        }
        loop {
            match self.client.poll().map_err(|e| e.to_string())? {
                crate::tls::Poll::Plaintext(bytes) => return Ok(bytes),
                crate::tls::Poll::Transmit(out) => self.inner.send(&out)?,
                crate::tls::Poll::Closed => return Ok(Vec::new()),
                crate::tls::Poll::Idle | crate::tls::Poll::NeedInbound => {
                    if !self.fill()? {
                        return Ok(Vec::new());
                    }
                }
            }
        }
    }
}

// ---- a session ----

/// A server error: its SQLSTATE and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgError {
    pub code: String,
    pub message: String,
}

impl PgError {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            code: String::new(),
            message: message.into(),
        }
    }

    fn unique_violation(&self) -> bool {
        self.code == "23505"
    }

    /// A comparison between a column and a value of another type.
    fn type_mismatch(&self) -> bool {
        matches!(
            self.code.as_str(),
            "22P02" | "22007" | "22008" | "22003" | "42883" | "42804"
        )
    }
}

type PgResultOf<T> = Result<T, PgError>;

#[derive(Debug, Default)]
struct Rows {
    columns: Vec<Column>,
    rows: Vec<Vec<Option<Vec<u8>>>>,
}

/// A bind value as the text the server parses; `None` is SQL NULL.
type Param = Option<String>;

struct Session {
    conn: Box<dyn PgConn>,
    inbox: Vec<u8>,
}

impl Session {
    fn open(port: &dyn PgPort, config: &PgConfig) -> PgResultOf<Self> {
        let mut conn = port
            .connect(&config.host, config.port)
            .map_err(PgError::plain)?;
        if config.ssl != SslMode::Disable {
            conn.send(&SSL_REQUEST).map_err(PgError::plain)?;
            let mut answer = conn.recv().map_err(PgError::plain)?;
            if answer.is_empty() {
                return Err(PgError::plain(
                    "the server closed the connection answering the TLS request",
                ));
            }
            match answer.remove(0) {
                b'S' => conn = Box::new(TlsConn::connect(conn, config).map_err(PgError::plain)?),
                b'N' if config.ssl == SslMode::Prefer => {}
                b'N' => {
                    return Err(PgError::plain(
                        "the server does not support TLS and sslmode requires it",
                    ))
                }
                other => {
                    return Err(PgError::plain(format!(
                        "the answer to the TLS request is {other:#x}, neither S nor N"
                    )))
                }
            }
        }
        let mut session = Session {
            conn,
            inbox: Vec::new(),
        };
        session.startup(config)?;
        Ok(session)
    }

    fn read_message(&mut self) -> PgResultOf<crate::postgres::Backend> {
        loop {
            if let Some(message) = next_message(&mut self.inbox).map_err(PgError::plain)? {
                return Ok(message);
            }
            let more = self.conn.recv().map_err(PgError::plain)?;
            if more.is_empty() {
                return Err(PgError::plain("the server closed the connection"));
            }
            self.inbox.extend(more);
        }
    }

    fn startup(&mut self, config: &PgConfig) -> PgResultOf<()> {
        let mut body = Vec::new();
        body.extend_from_slice(&196_608u32.to_be_bytes());
        for (name, value) in [
            ("user", config.user.as_str()),
            ("database", config.database.as_str()),
            ("client_encoding", "UTF8"),
        ] {
            cstr(&mut body, name);
            cstr(&mut body, value);
        }
        body.push(0);
        let mut out = ((body.len() + 4) as u32).to_be_bytes().to_vec();
        out.extend(body);
        self.conn.send(&out).map_err(PgError::plain)?;
        let mut scram: Option<Scram> = None;
        loop {
            let backend = self.read_message()?;
            match backend.kind {
                b'R' => {
                    let code = backend
                        .body
                        .get(..4)
                        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                        .unwrap_or(u32::MAX);
                    let password = || {
                        config.password.clone().ok_or_else(|| {
                            PgError::plain(
                                "the server asks for a password and DATABASE_URL has none",
                            )
                        })
                    };
                    match code {
                        0 => {}
                        3 => {
                            let mut body = Vec::new();
                            cstr(&mut body, &password()?);
                            self.conn
                                .send(&message(b'p', &body))
                                .map_err(PgError::plain)?;
                        }
                        5 => {
                            let salt = backend
                                .body
                                .get(4..8)
                                .ok_or_else(|| PgError::plain("a short MD5 salt"))?;
                            let mut body = Vec::new();
                            cstr(&mut body, &md5_password(&config.user, &password()?, salt));
                            self.conn
                                .send(&message(b'p', &body))
                                .map_err(PgError::plain)?;
                        }
                        10 => {
                            let nonce = nonce();
                            let (state, first) = Scram::start(&config.user, &nonce);
                            scram = Some(state);
                            let mut body = Vec::new();
                            cstr(&mut body, "SCRAM-SHA-256");
                            body.extend_from_slice(&(first.len() as u32).to_be_bytes());
                            body.extend_from_slice(first.as_bytes());
                            self.conn
                                .send(&message(b'p', &body))
                                .map_err(PgError::plain)?;
                        }
                        11 => {
                            let server_first =
                                String::from_utf8_lossy(&backend.body[4..]).into_owned();
                            let state = scram
                                .as_mut()
                                .ok_or_else(|| PgError::plain("SCRAM continue before start"))?;
                            let proof = state
                                .proof(&password()?, &server_first)
                                .map_err(PgError::plain)?;
                            self.conn
                                .send(&message(b'p', proof.as_bytes()))
                                .map_err(PgError::plain)?;
                        }
                        12 => {
                            let server_final =
                                String::from_utf8_lossy(&backend.body[4..]).into_owned();
                            scram
                                .as_ref()
                                .ok_or_else(|| PgError::plain("SCRAM final before start"))?
                                .verify(&server_final)
                                .map_err(PgError::plain)?;
                        }
                        other => {
                            return Err(PgError::plain(format!(
                                "unsupported authentication method {other}"
                            )))
                        }
                    }
                }
                b'E' => {
                    let error = parse_error(&backend.body);
                    return Err(PgError {
                        code: error.code,
                        message: error.message,
                    });
                }
                b'Z' => return Ok(()),
                _ => {}
            }
        }
    }

    /// One statement through the extended protocol, every bind and result in text format.
    fn query(&mut self, sql: &str, params: &[Param]) -> PgResultOf<Rows> {
        self.conn
            .send(&crate::postgres::query_messages(sql, params))
            .map_err(PgError::plain)?;
        let mut out = Rows::default();
        let mut error: Option<PgError> = None;
        loop {
            let backend = self.read_message()?;
            match backend.kind {
                b'T' => {
                    out.columns = parse_row_description(&backend.body).map_err(PgError::plain)?
                }
                b'D' => out
                    .rows
                    .push(parse_data_row(&backend.body).map_err(PgError::plain)?),
                b'E' => {
                    let e = parse_error(&backend.body);
                    error = Some(PgError {
                        code: e.code,
                        message: e.message,
                    });
                }
                b'Z' => {
                    return match error {
                        Some(e) => Err(e),
                        None => Ok(out),
                    }
                }
                _ => {}
            }
        }
    }
}

fn nonce() -> String {
    let mut bytes = [0u8; 18];
    getrandom::getrandom(&mut bytes).unwrap_or_default();
    codec::base64_encode(&bytes)
}

/// A random UUID v4, the id `PostgresPersistence` mints for an id-less create.
pub(crate) fn mint_id() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).unwrap_or_default();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = codec::hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

// ---- names, values and rows (`rows.ts`) ----

/// `TimeEntry` -> `time_entry`.
pub fn snake_name_for(entity_type: &str) -> String {
    let chars: Vec<char> = entity_type.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0
            && c.is_ascii_uppercase()
            && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit())
        {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// The one deterministic owner of entityType -> table name: snake_case, pluralized (trailing y -> ies, trailing s ->
/// es, else +s). `TimeEntry` -> `time_entries`.
pub fn table_name_for(entity_type: &str) -> String {
    let snake = snake_name_for(entity_type);
    if let Some(stem) = snake.strip_suffix('y') {
        format!("{stem}ies")
    } else if snake.ends_with('s') {
        format!("{snake}es")
    } else {
        format!("{snake}s")
    }
}

/// The declared `[persistent: tbl]` collections of the entities a store serves, by entity type.
pub type Collections = RwLock<HashMap<String, String>>;

/// The table (or database) holding an entity's rows: its declared collection as written, else `table_name_for`.
pub fn storage_name_for(collections: &Collections, entity_type: &str) -> String {
    collections
        .read()
        .get(entity_type)
        .cloned()
        .unwrap_or_else(|| table_name_for(entity_type))
}

pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

const STAMP_COLUMNS: [(&str, &str); 2] = [("createdAt", "created_at"), ("updatedAt", "updated_at")];

fn stamp_column(field: &str) -> Option<&'static str> {
    STAMP_COLUMNS
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, c)| *c)
}

fn is_stamp_field(field: &str) -> bool {
    stamp_column(field).is_some()
}

/// What a column holds, as far as a query translation needs to know. `Absent`: the table has no such column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Text,
    Jsonb,
    Array,
    Other,
    Absent,
}

fn column_kind_of(data_type: &str) -> ColumnKind {
    match data_type {
        "text" | "character varying" | "character" => ColumnKind::Text,
        "jsonb" => ColumnKind::Jsonb,
        "ARRAY" => ColumnKind::Array,
        _ => ColumnKind::Other,
    }
}

type Kinds = HashMap<String, ColumnKind>;

/// The column a row field lives in: the stamp's framework column unless the table declares a column of the field's
/// own name.
fn column_for(kinds: &Kinds, field: &str) -> String {
    let kind = |column: &str| kinds.get(column).copied().unwrap_or(ColumnKind::Absent);
    match stamp_column(field) {
        Some(stamp) if kind(field) == ColumnKind::Absent && kind(stamp) != ColumnKind::Absent => {
            stamp.to_string()
        }
        _ => field.to_string(),
    }
}

fn kind_of_field(kinds: &Kinds, field: &str) -> ColumnKind {
    kinds
        .get(&column_for(kinds, field))
        .copied()
        .unwrap_or(ColumnKind::Absent)
}

fn scalar_text(value: &Value) -> Param {
    match value {
        Value::Null | Value::Lambda { .. } => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(js_number_string(*n)),
        Value::String(s) => Some(s.clone()),
        Value::Array(_) | Value::Object(_) => crate::postgres::bind_text(value),
    }
}

/// A field value as a SQL param: objects and lists go through JSON (`serializeValue`).
fn serialize_value(value: &Value) -> Param {
    match value {
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).ok(),
        other => scalar_text(other),
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

/// `2026-03-05T10:00:00.123456Z` as `Date.prototype.toISOString` writes it: three fraction digits.
fn to_millis_iso(text: &str) -> String {
    let Some((head, tail)) = text.split_once('.') else {
        return match text.strip_suffix('Z') {
            Some(clock) => format!("{clock}.000Z"),
            None => text.to_string(),
        };
    };
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    let rest = &tail[digits.len()..];
    format!("{head}.{:0<3.3}{rest}", digits)
}

/// A result cell as the value the row carries: `jsonb` parses, a stamp's ISO text has millisecond precision.
fn cell(oid: u32, bytes: Option<&[u8]>) -> Result<Value, String> {
    if matches!(oid, 114 | 3802) {
        return Ok(match bytes {
            None => Value::Null,
            Some(raw) => {
                let text = std::str::from_utf8(raw)
                    .map_err(|_| "a jsonb cell is not UTF-8".to_string())?;
                serde_json::from_str::<Value>(text)
                    .map_err(|e| format!("a jsonb cell is not JSON: {e}"))?
            }
        });
    }
    cell_value(oid, bytes).map_err(|p| p.0)
}

/// A result row as the row object `deserializeRow` returns: the stamp columns under the stamp names, as ISO text.
fn row_of(columns: &[Column], cells: &[Option<Vec<u8>>]) -> Result<ValueMap, String> {
    let mut row = ValueMap::default();
    for (column, bytes) in columns.iter().zip(cells) {
        row.insert(
            column.name.clone(),
            cell(column.type_oid, bytes.as_deref())?,
        );
    }
    for (field, column) in STAMP_COLUMNS {
        if !row.contains_key(field) {
            if let Some(value) = row.shift_remove(column) {
                row.insert(field.to_string(), value);
            }
        }
        if let Some(Value::String(text)) = row.get(field) {
            let iso = to_millis_iso(text);
            row.insert(field.to_string(), Value::String(iso));
        }
    }
    Ok(row)
}

fn rows_of(result: &Rows) -> Result<Vec<ValueMap>, String> {
    result
        .rows
        .iter()
        .map(|cells| row_of(&result.columns, cells))
        .collect()
}

// ---- SQL (`rows.ts`) ----

struct Ctx<'a> {
    kinds: &'a Kinds,
    params: Vec<Param>,
}

impl Ctx<'_> {
    fn kind(&self, field: &str) -> ColumnKind {
        kind_of_field(self.kinds, field)
    }

    fn column(&self, field: &str) -> String {
        quote_ident(&column_for(self.kinds, field))
    }

    fn bind(&mut self, param: Param) -> String {
        self.params.push(param);
        format!("${}", self.params.len())
    }
}

fn compare_sql(ctx: &mut Ctx<'_>, field: &str, op: RowCompareOp, value: &Value) -> String {
    let col = ctx.column(field);
    if matches!(value, Value::Null) {
        return if op == RowCompareOp::Ne {
            format!("{col} IS NOT NULL")
        } else {
            "FALSE".to_string()
        };
    }
    if !is_scalar(value) {
        return if op == RowCompareOp::Ne {
            "TRUE".to_string()
        } else {
            "FALSE".to_string()
        };
    }
    match op {
        RowCompareOp::Eq => {
            let p = ctx.bind(scalar_text(value));
            format!("COALESCE({col} = {p}, FALSE)")
        }
        RowCompareOp::Ne => {
            let p = ctx.bind(scalar_text(value));
            format!("{col} IS DISTINCT FROM {p}")
        }
        range => {
            let collate =
                if matches!(value, Value::String(_)) && ctx.kind(field) == ColumnKind::Text {
                    " COLLATE \"C\""
                } else {
                    ""
                };
            let symbol = match range {
                RowCompareOp::Lt => "<",
                RowCompareOp::Le => "<=",
                RowCompareOp::Gt => ">",
                _ => ">=",
            };
            let p = ctx.bind(scalar_text(value));
            format!("COALESCE({col}{collate} {symbol} {p}, FALSE)")
        }
    }
}

fn absent_sql(predicate: &RowPredicate) -> String {
    match predicate {
        RowPredicate::IsNull { is_null, .. } => if *is_null { "TRUE" } else { "FALSE" }.to_string(),
        RowPredicate::Compare { op, value, .. } => {
            if *op == RowCompareOp::Ne && !matches!(value, Value::Null) {
                "TRUE"
            } else {
                "FALSE"
            }
            .to_string()
        }
        _ => "FALSE".to_string(),
    }
}

fn field_of(predicate: &RowPredicate) -> Option<&str> {
    match predicate {
        RowPredicate::Compare { field, .. }
        | RowPredicate::Member { field, .. }
        | RowPredicate::HasElement { field, .. }
        | RowPredicate::Contains { field, .. }
        | RowPredicate::IsNull { field, .. } => Some(field),
        _ => None,
    }
}

fn predicate_sql(ctx: &mut Ctx<'_>, predicate: &RowPredicate) -> String {
    if let Some(field) = field_of(predicate) {
        if ctx.kind(field) == ColumnKind::Absent {
            return absent_sql(predicate);
        }
    }
    match predicate {
        RowPredicate::Const { value } => if *value { "TRUE" } else { "FALSE" }.to_string(),
        RowPredicate::Compare { field, op, value } => compare_sql(ctx, field, *op, value),
        RowPredicate::IsNull { field, is_null } => {
            format!(
                "{} {}",
                ctx.column(field),
                if *is_null { "IS NULL" } else { "IS NOT NULL" }
            )
        }
        RowPredicate::Member { field, values } => {
            let kept: Vec<Value> = values
                .iter()
                .filter(|v| !matches!(v, Value::Null) && is_scalar(v))
                .cloned()
                .collect();
            if kept.is_empty() {
                return "FALSE".to_string();
            }
            let col = ctx.column(field);
            let p = ctx.bind(crate::postgres::bind_text(&Value::Array(kept)));
            format!("COALESCE({col} = ANY({p}), FALSE)")
        }
        RowPredicate::HasElement { field, value } => match ctx.kind(field) {
            ColumnKind::Jsonb => {
                let col = ctx.column(field);
                let json =
                    serde_json::to_string(&Value::Array(vec![value.clone()])).unwrap_or_default();
                let p = ctx.bind(Some(json));
                format!("COALESCE({col} @> {p}::jsonb, FALSE)")
            }
            ColumnKind::Array => {
                let col = ctx.column(field);
                let p = ctx.bind(scalar_text(value));
                format!("COALESCE({p} = ANY({col}), FALSE)")
            }
            _ => "FALSE".to_string(),
        },
        RowPredicate::Contains {
            field,
            text,
            case_sensitive,
        } => {
            if ctx.kind(field) != ColumnKind::Text {
                return "FALSE".to_string();
            }
            let col = ctx.column(field);
            let p = ctx.bind(Some(text.clone()));
            if *case_sensitive {
                format!("COALESCE(position({p}::text in {col}) > 0, FALSE)")
            } else {
                format!("COALESCE(position(lower({p}::text) in lower({col})) > 0, FALSE)")
            }
        }
        RowPredicate::All { of } => {
            if of.is_empty() {
                "TRUE".to_string()
            } else {
                format!(
                    "({})",
                    of.iter()
                        .map(|p| predicate_sql(ctx, p))
                        .collect::<Vec<_>>()
                        .join(" AND ")
                )
            }
        }
        RowPredicate::Any { of } => {
            if of.is_empty() {
                "FALSE".to_string()
            } else {
                format!(
                    "({})",
                    of.iter()
                        .map(|p| predicate_sql(ctx, p))
                        .collect::<Vec<_>>()
                        .join(" OR ")
                )
            }
        }
        RowPredicate::Not { of } => format!("NOT ({})", predicate_sql(ctx, of)),
    }
}

struct QuerySql {
    sql: String,
    params: Vec<Param>,
    count_sql: String,
    count_params: Vec<Param>,
}

/// A `RowQuery` as parameterized SQL with the semantics of `runRowQuery`: two-valued null logic, nulls last in both
/// directions, ties by `id` ascending, binary (`COLLATE "C"`) order on text columns.
fn query_page_sql(
    table: &str,
    query: &RowQuery,
    kinds: &Kinds,
    projection: RowProjection,
) -> QuerySql {
    let mut ctx = Ctx {
        kinds,
        params: Vec::new(),
    };
    let conditions: Vec<String> = [&query.policy, &query.filter]
        .into_iter()
        .flatten()
        .map(|p| predicate_sql(&mut ctx, p))
        .collect();
    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };
    let count_params = ctx.params.clone();
    let collate = |ctx: &Ctx<'_>, field: &str| {
        if ctx.kind(field) == ColumnKind::Text {
            " COLLATE \"C\""
        } else {
            ""
        }
    };
    let tie_break = format!("{}{} ASC", quote_ident("id"), collate(&ctx, "id"));
    let order = effective_row_order(query);
    let order_sql = if ctx.kind(&order.field) != ColumnKind::Absent {
        format!(
            " ORDER BY {}{} {} NULLS LAST, {tie_break}",
            ctx.column(&order.field),
            collate(&ctx, &order.field),
            if order.direction == RowDirection::Desc {
                "DESC"
            } else {
                "ASC"
            }
        )
    } else {
        format!(" ORDER BY {tie_break}")
    };
    let mut window = String::new();
    if let Some(limit) = query.limit {
        let p = ctx.bind(Some(limit.to_string()));
        window.push_str(&format!(" LIMIT {p}"));
    }
    if let Some(offset) = query.offset {
        let p = ctx.bind(Some(offset.to_string()));
        window.push_str(&format!(" OFFSET {p}"));
    }
    let from = quote_ident(table);
    let select = if projection == RowProjection::Ids {
        quote_ident("id")
    } else {
        "*".to_string()
    };
    let total = if query.total == Some(RowTotalMode::None) {
        ""
    } else {
        ", (COUNT(*) OVER ())::int AS \"__total\""
    };
    QuerySql {
        sql: format!("SELECT {select}{total} FROM {from}{where_sql}{order_sql}{window}"),
        params: ctx.params,
        count_sql: format!("SELECT COUNT(*)::int AS total FROM {from}{where_sql}"),
        count_params,
    }
}

struct WriteSql {
    sql: String,
    params: Vec<Param>,
}

fn data_map(data: &Value) -> ValueMap {
    data.as_object().cloned().unwrap_or_default()
}

/// One statement for an `update` / `delete`: the pre-image is selected `FOR UPDATE`, the write runs only when the
/// guard holds on it, and the result rows are tagged `before` / `after`. `$1` is the id.
fn write_sql(table: &str, op: &RowWriteOp, kinds: &Kinds) -> WriteSql {
    let (id, guard) = match op {
        RowWriteOp::Update { id, guard, .. } | RowWriteOp::Delete { id, guard, .. } => {
            (id.clone(), guard.as_ref())
        }
        _ => (String::new(), None),
    };
    let mut ctx = Ctx {
        kinds,
        params: vec![Some(id)],
    };
    let t = quote_ident(table);
    let id_col = quote_ident("id");
    let write = match op {
        RowWriteOp::Update { data, .. } => {
            let mut sets: Vec<String> = Vec::new();
            for (key, value) in data_map(data) {
                if key == "id" {
                    continue;
                }
                let col = ctx.column(&key);
                let p = ctx.bind(serialize_value(&value));
                sets.push(format!("{col} = {p}"));
            }
            if sets.is_empty() {
                sets.push(format!("{id_col} = {id_col}"));
            }
            format!("UPDATE {t} SET {}", sets.join(", "))
        }
        _ => format!("DELETE FROM {t}"),
    };
    let guard_sql = guard
        .map(|g| format!(" AND {}", predicate_sql(&mut ctx, g)))
        .unwrap_or_default();
    WriteSql {
        sql: format!(
            "WITH \"pre\" AS (SELECT * FROM {t} WHERE {id_col} = $1 FOR UPDATE), \"post\" AS ({write} WHERE {id_col} = $1{guard_sql} RETURNING *) SELECT 'before' AS \"__part\", \"pre\".* FROM \"pre\" UNION ALL SELECT 'after' AS \"__part\", \"post\".* FROM \"post\""
        ),
        params: ctx.params,
    }
}

fn defined_keys(data: &ValueMap) -> Vec<String> {
    data.keys().cloned().collect()
}

/// `INSERT` that leaves an existing id alone, so a duplicate is an empty result and not an aborted transaction.
fn create_sql(table: &str, data: &ValueMap, kinds: &Kinds) -> WriteSql {
    let keys = defined_keys(data);
    let columns = keys
        .iter()
        .map(|k| quote_ident(&column_for(kinds, k)))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=keys.len())
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(", ");
    WriteSql {
        sql: format!(
            "INSERT INTO {} ({columns}) VALUES ({placeholders}) ON CONFLICT ({}) DO NOTHING RETURNING *",
            quote_ident(table),
            quote_ident("id")
        ),
        params: keys.iter().map(|k| data.get(k).and_then(serialize_value)).collect(),
    }
}

/// Create-or-replace by id in one statement: the given columns are written, every other column goes back to its
/// default, the stored `created_at` is kept and `updated_at` is refreshed unless the row supplies one.
fn upsert_sql(table: &str, data: &ValueMap, kinds: &Kinds) -> WriteSql {
    let keys = defined_keys(data);
    let written: Vec<String> = keys.iter().map(|k| column_for(kinds, k)).collect();
    let created = column_for(kinds, "createdAt");
    let updated = column_for(kinds, "updatedAt");
    let mut sets: Vec<String> = Vec::new();
    for column in &written {
        if column == "id" || *column == created {
            continue;
        }
        sets.push(format!("{0} = EXCLUDED.{0}", quote_ident(column)));
    }
    let mut table_columns: Vec<&String> = kinds.keys().collect();
    table_columns.sort();
    for column in table_columns {
        if column == "id" || *column == created || written.contains(column) {
            continue;
        }
        if column == "updated_at" && *column == updated {
            sets.push(format!("{} = now()", quote_ident(column)));
        } else {
            sets.push(format!("{} = DEFAULT", quote_ident(column)));
        }
    }
    if sets.is_empty() {
        sets.push(format!("{0} = EXCLUDED.{0}", quote_ident("id")));
    }
    let columns = written
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=keys.len())
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(", ");
    WriteSql {
        sql: format!(
            "INSERT INTO {} ({columns}) VALUES ({placeholders}) ON CONFLICT ({}) DO UPDATE SET {} RETURNING *, (xmax = 0) AS \"__inserted\"",
            quote_ident(table),
            quote_ident("id"),
            sets.join(", ")
        ),
        params: keys.iter().map(|k| data.get(k).and_then(serialize_value)).collect(),
    }
}

/// Every field a query names, including the order field (`createdAt` by default) and the `id` tie-break.
fn query_fields(query: &RowQuery) -> Vec<String> {
    fn walk(predicate: &RowPredicate, out: &mut Vec<String>) {
        if let Some(field) = field_of(predicate) {
            out.push(field.to_string());
        }
        match predicate {
            RowPredicate::All { of } | RowPredicate::Any { of } => {
                of.iter().for_each(|p| walk(p, out))
            }
            RowPredicate::Not { of } => walk(of, out),
            _ => {}
        }
    }
    let mut fields = vec!["id".to_string()];
    for predicate in [&query.policy, &query.filter].into_iter().flatten() {
        walk(predicate, &mut fields);
    }
    fields.push(effective_row_order(query).field);
    fields.sort();
    fields.dedup();
    fields
}

// ---- the row store ----

const IDLE_SESSIONS: usize = 4;

pub struct PostgresRows {
    port: Arc<dyn PgPort>,
    config: PgConfig,
    idle: Mutex<Vec<Session>>,
    kinds: Mutex<HashMap<String, Kinds>>,
    collections: Collections,
}

fn err_string(error: PgError) -> String {
    error.message
}

/// What a read-or-write needs from the session it borrowed: handed back to the pool when it ends cleanly.
struct Lease<'a> {
    store: &'a PostgresRows,
    session: Option<Session>,
}

impl Lease<'_> {
    fn query(&mut self, sql: &str, params: &[Param]) -> PgResultOf<Rows> {
        match self.session.as_mut() {
            Some(session) => {
                let out = session.query(sql, params);
                if matches!(&out, Err(e) if e.code.is_empty()) {
                    self.session = None;
                }
                out
            }
            None => Err(PgError::plain("the session is gone")),
        }
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let mut idle = self.store.idle.lock();
            if idle.len() < IDLE_SESSIONS {
                idle.push(session);
            }
        }
    }
}

impl PostgresRows {
    pub fn new(config: PgConfig, port: Arc<dyn PgPort>) -> Self {
        Self {
            port,
            config,
            idle: Mutex::new(Vec::new()),
            kinds: Mutex::new(HashMap::new()),
            collections: RwLock::new(HashMap::new()),
        }
    }

    fn table_of(&self, entity_type: &str) -> String {
        storage_name_for(&self.collections, entity_type)
    }

    /// `DATABASE_URL` over the plain TCP port.
    pub fn from_url(connection_string: &str) -> Result<Self, String> {
        let mut config = PgConfig::from_url(connection_string)?;
        if let Some(path) = &config.root_cert_path {
            config.root_cert_pem = Some(
                std::fs::read_to_string(path)
                    .map_err(|e| format!("sslrootcert {path} cannot be read: {e}"))?,
            );
        }
        Ok(Self::new(config, Arc::new(TcpPgPort)))
    }

    fn lease(&self) -> PgResultOf<Lease<'_>> {
        let idle = self.idle.lock().pop();
        let session = match idle {
            Some(session) => session,
            None => Session::open(self.port.as_ref(), &self.config)?,
        };
        Ok(Lease {
            store: self,
            session: Some(session),
        })
    }

    /// Column kinds of `table`, read from the database; re-read once when a queried field is not among them (a column
    /// added since).
    fn kinds_for(
        &self,
        lease: &mut Lease<'_>,
        table: &str,
        fields: &[String],
    ) -> PgResultOf<Kinds> {
        if let Some(known) = self.kinds.lock().get(table) {
            if fields
                .iter()
                .all(|f| known.contains_key(f) || is_stamp_field(f))
            {
                return Ok(known.clone());
            }
        }
        let result = lease.query(
            "SELECT column_name, data_type FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = $1",
            &[Some(table.to_string())],
        )?;
        let mut kinds = Kinds::new();
        for cells in &result.rows {
            let text = |at: usize| {
                cells
                    .get(at)
                    .and_then(|c| c.as_ref())
                    .map(|b| String::from_utf8_lossy(b).into_owned())
            };
            if let (Some(name), Some(data_type)) = (text(0), text(1)) {
                kinds.insert(name, column_kind_of(&data_type));
            }
        }
        self.kinds.lock().insert(table.to_string(), kinds.clone());
        Ok(kinds)
    }

    fn kinds_when_needed(
        &self,
        lease: &mut Lease<'_>,
        table: &str,
        fields: &[String],
        needed: bool,
    ) -> PgResultOf<Kinds> {
        if needed {
            self.kinds_for(lease, table, fields)
        } else {
            Ok(Kinds::new())
        }
    }

    fn select_one(&self, entity_type: &str, id: &str) -> PgResultOf<Option<Value>> {
        let mut lease = self.lease()?;
        let result = lease.query(
            &format!(
                "SELECT * FROM {} WHERE {} = $1",
                quote_ident(&self.table_of(entity_type)),
                quote_ident("id")
            ),
            &[Some(id.to_string())],
        )?;
        rows_of(&result).map_err(PgError::plain).map(|mut rows| {
            rows.pop()
                .map(Value::Object)
                .filter(|_| !result.rows.is_empty())
        })
    }

    fn run_query(
        &self,
        entity_type: &str,
        query: &RowQuery,
        projection: RowProjection,
    ) -> Option<Result<(Vec<Value>, Vec<String>, usize), String>> {
        if query.nearest.is_some() {
            return None;
        }
        let table = self.table_of(entity_type);
        let outcome = (|| -> PgResultOf<(Vec<Value>, Vec<String>, usize)> {
            let mut lease = self.lease()?;
            let kinds = self.kinds_for(&mut lease, &table, &query_fields(query))?;
            let built = query_page_sql(&table, query, &kinds, projection);
            let result = lease.query(&built.sql, &built.params)?;
            let mut maps = rows_of(&result).map_err(PgError::plain)?;
            let total_of = |map: &mut ValueMap| map.shift_remove("__total");
            let mut ids = Vec::with_capacity(maps.len());
            let mut first_total: Option<usize> = None;
            for map in &mut maps {
                if let Some(Value::Number(n)) = total_of(map) {
                    first_total.get_or_insert(n as usize);
                }
                ids.push(
                    map.get("id")
                        .map(Value::to_string_value)
                        .unwrap_or_default(),
                );
            }
            let rows: Vec<Value> = if projection == RowProjection::Rows {
                maps.into_iter().map(Value::Object).collect()
            } else {
                Vec::new()
            };
            if query.total == Some(RowTotalMode::None) {
                let total = ids.len();
                return Ok((rows, ids, total));
            }
            if let Some(total) = first_total {
                return Ok((rows, ids, total));
            }
            if query.offset.unwrap_or(0) == 0 && query.limit != Some(0) {
                return Ok((rows, ids, 0));
            }
            let counted = lease.query(&built.count_sql, &built.count_params)?;
            let total = counted
                .rows
                .first()
                .and_then(|r| r.first())
                .and_then(|c| c.as_ref())
                .and_then(|b| String::from_utf8_lossy(b).parse::<usize>().ok())
                .unwrap_or(0);
            Ok((rows, ids, total))
        })();
        match outcome {
            Ok(done) => Some(Ok(done)),
            Err(e) if e.type_mismatch() => None,
            Err(e) => Some(Err(e.message)),
        }
    }

    /// One write over `lease`'s session. `Err(None)` declines a guard whose value type does not fit its column.
    fn run_write(
        &self,
        lease: &mut Lease<'_>,
        op: &RowWriteOp,
    ) -> Result<RowWriteOutcome, WriteFault> {
        let table = self.table_of(op.entity_type());
        match op {
            RowWriteOp::Rekey { .. } => Err(WriteFault::Failed("rekey is unsupported by this store".to_string())),
            RowWriteOp::Create { data, .. } => {
                let map = data_map(data);
                if !matches!(map.get("id"), Some(Value::String(_))) {
                    return Ok(RowWriteOutcome::Failed {
                        message: "create needs a string id".to_string(),
                    });
                }
                let fields = defined_keys(&map);
                let kinds = self.kinds_when_needed(
                    lease,
                    &table,
                    &fields,
                    fields.iter().any(|f| is_stamp_field(f)),
                )?;
                let built = create_sql(&table, &map, &kinds);
                match lease.query(&built.sql, &built.params) {
                    Ok(result) => {
                        match rows_of(&result).map_err(PgError::plain)?.into_iter().next() {
                            Some(row) => Ok(RowWriteOutcome::Created {
                                row: Value::Object(row),
                            }),
                            None => Ok(RowWriteOutcome::DuplicateId),
                        }
                    }
                    Err(e) => Ok(RowWriteOutcome::Failed { message: e.message }),
                }
            }
            RowWriteOp::Upsert { data, .. } => {
                let map = data_map(data);
                if !matches!(map.get("id"), Some(Value::String(_))) {
                    return Ok(RowWriteOutcome::Failed {
                        message: "upsert needs a string id".to_string(),
                    });
                }
                let attempt = (|| -> PgResultOf<RowWriteOutcome> {
                    let kinds = self.kinds_for(lease, &table, &defined_keys(&map))?;
                    let built = upsert_sql(&table, &map, &kinds);
                    let result = lease.query(&built.sql, &built.params)?;
                    let mut rows = rows_of(&result).map_err(PgError::plain)?;
                    let Some(mut row) = rows.pop() else {
                        return Ok(RowWriteOutcome::Failed {
                            message: format!(
                                "{} {}: the upsert returned no row",
                                op.entity_type(),
                                map.get("id")
                                    .map(Value::to_string_value)
                                    .unwrap_or_default()
                            ),
                        });
                    };
                    let inserted =
                        matches!(row.shift_remove("__inserted"), Some(Value::Bool(true)));
                    let row = Value::Object(row);
                    Ok(if inserted {
                        RowWriteOutcome::Created { row }
                    } else {
                        RowWriteOutcome::Updated { row, before: None }
                    })
                })();
                match attempt {
                    Ok(outcome) => Ok(outcome),
                    Err(e) => Ok(RowWriteOutcome::Failed { message: e.message }),
                }
            }
            RowWriteOp::Update { .. } | RowWriteOp::Delete { .. } => {
                let written: Vec<String> = match op {
                    RowWriteOp::Update { data, .. } => defined_keys(&data_map(data)),
                    _ => Vec::new(),
                };
                let mut fields: Vec<String> = Vec::new();
                if let Some(guard) = op.guard() {
                    fields.extend(query_fields(&RowQuery {
                        filter: Some(guard.clone()),
                        ..RowQuery::default()
                    }));
                }
                fields.extend(written.iter().cloned());
                let needed = op.guard().is_some() || written.iter().any(|f| is_stamp_field(f));
                let kinds = self.kinds_when_needed(lease, &table, &fields, needed)?;
                let built = write_sql(&table, op, &kinds);
                let result = match lease.query(&built.sql, &built.params) {
                    Ok(result) => result,
                    Err(e) if op.guard().is_some() && e.type_mismatch() => {
                        return Err(WriteFault::Declined)
                    }
                    Err(e) => return Ok(RowWriteOutcome::Failed { message: e.message }),
                };
                let mut before: Option<Value> = None;
                let mut after: Option<Value> = None;
                for mut row in rows_of(&result).map_err(PgError::plain)? {
                    let part = row.shift_remove("__part");
                    match part {
                        Some(Value::String(p)) if p == "before" => {
                            before = Some(Value::Object(row))
                        }
                        Some(Value::String(p)) if p == "after" => after = Some(Value::Object(row)),
                        _ => {}
                    }
                }
                let Some(before) = before else {
                    return Ok(RowWriteOutcome::NotFound);
                };
                let Some(after) = after else {
                    return Ok(RowWriteOutcome::Refused);
                };
                let guarded = op.guard().map(|_| before);
                Ok(match op {
                    RowWriteOp::Delete { .. } => RowWriteOutcome::Deleted { before: guarded },
                    _ => RowWriteOutcome::Updated {
                        row: after,
                        before: guarded,
                    },
                })
            }
        }
    }

    fn write_ops(&self, batch: &RowWriteBatch) -> Result<Vec<RowWriteOutcome>, WriteFault> {
        if batch
            .ops
            .iter()
            .any(|op| matches!(op, RowWriteOp::Rekey { .. }))
        {
            return Err(WriteFault::Failed(
                "rekey is unsupported by this store".to_string(),
            ));
        }
        let mut lease = self.lease()?;
        if batch.on_failure == RowWriteFailureMode::Stop {
            let mut outcomes = Vec::new();
            for op in &batch.ops {
                let outcome = self.run_write(&mut lease, op)?;
                let applied = outcome.applied();
                outcomes.push(outcome);
                if !applied {
                    break;
                }
            }
            return Ok(outcomes);
        }
        lease.query("BEGIN", &[])?;
        let mut outcomes: Vec<RowWriteOutcome> = Vec::new();
        for op in &batch.ops {
            let outcome = match self.run_write(&mut lease, op) {
                Ok(outcome) => outcome,
                Err(fault) => {
                    let _ = lease.query("ROLLBACK", &[]);
                    return Err(fault);
                }
            };
            if !outcome.applied() {
                lease.query("ROLLBACK", &[])?;
                let mut undone: Vec<RowWriteOutcome> = outcomes
                    .iter()
                    .map(|_| RowWriteOutcome::RolledBack)
                    .collect();
                undone.push(outcome);
                return Ok(undone);
            }
            outcomes.push(outcome);
        }
        lease.query("COMMIT", &[])?;
        Ok(outcomes)
    }

    /// Runs one statement that returns no rows (schema setup); the store never takes statements from a program.
    pub fn run_ddl(&self, sql: &str) -> Result<(), String> {
        let mut lease = self.lease().map_err(err_string)?;
        lease.query(sql, &[]).map(|_| ()).map_err(err_string)
    }

    /// What the store honours for queries: every form but `nearest`, ids-only included.
    pub fn query_capabilities(&self) -> QueryCapabilities {
        QueryCapabilities {
            ids_only: Some(true),
            nearest: None,
            ..QueryCapabilities::full()
        }
    }

    pub fn write_capabilities(&self) -> WriteCapabilities {
        WriteCapabilities::full()
    }
}

/// Why a write produced no outcome: the guard's value does not fit its column (declined, nothing written), or the
/// connection failed.
enum WriteFault {
    Declined,
    Failed(String),
}

impl From<PgError> for WriteFault {
    fn from(error: PgError) -> Self {
        WriteFault::Failed(error.message)
    }
}

impl EntitySource for PostgresRows {
    fn bind_collection(&self, entity_type: &str, collection: &str) {
        self.collections
            .write()
            .insert(entity_type.to_string(), collection.to_string());
    }

    fn read(&self, entity_type: &str, id: &str) -> Result<Arc<Value>, String> {
        Ok(self
            .find(entity_type, id)?
            .unwrap_or_else(|| Arc::new(Value::Null)))
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Arc<Value>>, String> {
        self.select_one(entity_type, id)
            .map_err(err_string)
            .map(|row| row.map(Arc::new))
    }

    /// A merge write; a missing row is created with its id.
    fn write(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        let table = self.table_of(entity_type);
        let mut map = data_map(data);
        map.shift_remove("id");
        let mut lease = self.lease().map_err(err_string)?;
        if !map.is_empty() {
            let fields = defined_keys(&map);
            let kinds = self
                .kinds_when_needed(
                    &mut lease,
                    &table,
                    &fields,
                    fields.iter().any(|f| is_stamp_field(f)),
                )
                .map_err(err_string)?;
            let mut ctx = Ctx {
                kinds: &kinds,
                params: Vec::new(),
            };
            let sets: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let col = ctx.column(k);
                    let p = ctx.bind(serialize_value(v));
                    format!("{col} = {p}")
                })
                .collect();
            let p = ctx.bind(Some(id.to_string()));
            let sql = format!(
                "UPDATE {} SET {} WHERE {} = {p} RETURNING *",
                quote_ident(&table),
                sets.join(", "),
                quote_ident("id")
            );
            let result = lease.query(&sql, &ctx.params).map_err(err_string)?;
            if let Some(row) = rows_of(&result)?.into_iter().next() {
                return Ok(Value::Object(row));
            }
        } else if let Some(row) = self.select_one(entity_type, id).map_err(err_string)? {
            return Ok(row);
        }
        let mut with_id = data_map(data);
        with_id.insert("id".to_string(), Value::String(id.to_string()));
        drop(lease);
        self.create(entity_type, &Value::Object(with_id))
    }

    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        let table = self.table_of(entity_type);
        let mut map = data_map(data);
        let id = match map.get("id") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => mint_id(),
        };
        map.insert("id".to_string(), Value::String(id.clone()));
        let mut lease = self.lease().map_err(err_string)?;
        let fields = defined_keys(&map);
        let kinds = self
            .kinds_when_needed(
                &mut lease,
                &table,
                &fields,
                fields.iter().any(|f| is_stamp_field(f)),
            )
            .map_err(err_string)?;
        let columns = fields
            .iter()
            .map(|k| quote_ident(&column_for(&kinds, k)))
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = (1..=fields.len())
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let params: Vec<Param> = fields
            .iter()
            .map(|k| map.get(k).and_then(serialize_value))
            .collect();
        let sql = format!(
            "INSERT INTO {} ({columns}) VALUES ({placeholders}) RETURNING *",
            quote_ident(&table)
        );
        match lease.query(&sql, &params) {
            Ok(result) => rows_of(&result)?
                .into_iter()
                .next()
                .map(Value::Object)
                .ok_or_else(|| format!("{entity_type} {id}: the insert returned no row")),
            Err(e) if e.unique_violation() => {
                Err(format!("Entity {entity_type} with id {id} already exists"))
            }
            Err(e) => Err(e.message),
        }
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        let mut lease = self.lease().map_err(err_string)?;
        lease
            .query(
                &format!(
                    "DELETE FROM {} WHERE {} = $1",
                    quote_ident(&self.table_of(entity_type)),
                    quote_ident("id")
                ),
                &[Some(id.to_string())],
            )
            .map(|_| ())
            .map_err(err_string)
    }

    /// A remote store is not cleared by the kernel.
    fn clear(&self) {}

    fn count(&self) -> usize {
        0
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Arc<Value>>, String> {
        let mut lease = self.lease().map_err(err_string)?;
        let result = lease
            .query(
                &format!("SELECT * FROM {}", quote_ident(&self.table_of(entity_type))),
                &[],
            )
            .map_err(err_string)?;
        Ok(rows_of(&result)?
            .into_iter()
            .map(|row| Arc::new(Value::Object(row)))
            .collect())
    }

    fn query_page(&self, entity_type: &str, query: &RowQuery) -> Option<Result<RowPage, String>> {
        self.run_query(entity_type, query, RowProjection::Rows)
            .map(|run| run.map(|(rows, _, total)| RowPage { rows, total }))
    }

    fn query_ids(&self, entity_type: &str, query: &RowQuery) -> Option<Result<RowIdPage, String>> {
        self.run_query(entity_type, query, RowProjection::Ids)
            .map(|run| run.map(|(_, ids, total)| RowIdPage { ids, total }))
    }

    fn write_where(&self, op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        let batch = RowWriteBatch {
            ops: vec![op.clone()],
            on_failure: RowWriteFailureMode::Stop,
        };
        match self.write_ops(&batch) {
            Ok(mut outcomes) => Some(
                outcomes
                    .pop()
                    .ok_or_else(|| "the write produced no outcome".to_string()),
            ),
            Err(WriteFault::Declined) => None,
            Err(WriteFault::Failed(message)) => Some(Err(message)),
        }
    }

    fn write_batch(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        match self.write_ops(batch) {
            Ok(outcomes) => Some(Ok(outcomes)),
            Err(WriteFault::Declined) => None,
            Err(WriteFault::Failed(message)) => Some(Err(message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(pairs: &[(&str, ColumnKind)]) -> Kinds {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn query(json: serde_json::Value) -> RowQuery {
        serde_json::from_value(json).unwrap()
    }

    fn ticket_kinds() -> Kinds {
        kinds(&[
            ("id", ColumnKind::Text),
            ("created_at", ColumnKind::Other),
            ("updated_at", ColumnKind::Other),
            ("title", ColumnKind::Text),
            ("status", ColumnKind::Text),
            ("priority", ColumnKind::Other),
            ("tags", ColumnKind::Jsonb),
        ])
    }

    #[test]
    fn a_table_is_the_snake_case_name_pluralized_as_the_ts_adapter_names_it() {
        for (entity, table) in [
            ("Ticket", "tickets"),
            ("TimeEntry", "time_entries"),
            ("Address", "addresses"),
            ("Category", "categories"),
            ("Class2Student", "class2_students"),
            ("Boss", "bosses"),
        ] {
            assert_eq!(table_name_for(entity), table, "{entity}");
        }
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn a_declared_collection_is_the_table_as_written_and_an_undeclared_entity_keeps_the_pluralized_name(
    ) {
        let collections: Collections = RwLock::new(HashMap::new());
        collections
            .write()
            .insert("TimeEntry".to_string(), "timesheet".to_string());
        assert_eq!(storage_name_for(&collections, "TimeEntry"), "timesheet");
        assert_eq!(
            storage_name_for(&collections, "Category"),
            "categories",
            "control: no declared collection"
        );
    }

    #[test]
    fn the_stamps_live_in_the_framework_columns_unless_the_table_declares_its_own() {
        let framework = ticket_kinds();
        assert_eq!(column_for(&framework, "createdAt"), "created_at");
        assert_eq!(column_for(&framework, "title"), "title");
        let own = kinds(&[
            ("id", ColumnKind::Text),
            ("createdAt", ColumnKind::Other),
            ("created_at", ColumnKind::Other),
        ]);
        assert_eq!(
            column_for(&own, "createdAt"),
            "createdAt",
            "control: a declared createdAt column keeps its name"
        );
        assert_eq!(
            column_for(&Kinds::new(), "createdAt"),
            "createdAt",
            "no stamp column to fall back on"
        );
    }

    #[test]
    fn a_millisecond_iso_stamp_is_what_the_ts_adapter_returns() {
        assert_eq!(
            to_millis_iso("2026-03-05T10:00:00.123456Z"),
            "2026-03-05T10:00:00.123Z"
        );
        assert_eq!(
            to_millis_iso("2026-03-05T10:00:00.5Z"),
            "2026-03-05T10:00:00.500Z"
        );
        assert_eq!(
            to_millis_iso("2026-03-05T10:00:00Z"),
            "2026-03-05T10:00:00.000Z"
        );
    }

    #[test]
    fn a_query_is_parameterized_sql_with_two_valued_logic_binary_text_order_and_the_id_tie_break() {
        let built = query_page_sql(
            "tickets",
            &query(serde_json::json!({
                "filter": { "kind": "all", "of": [
                    { "kind": "compare", "field": "status", "op": "==", "value": "open" },
                    { "kind": "compare", "field": "priority", "op": ">=", "value": 3 },
                    { "kind": "member", "field": "title", "values": ["a", "b"] } ] },
                "order": { "field": "title", "direction": "desc" }, "offset": 2, "limit": 3
            })),
            &ticket_kinds(),
            RowProjection::Rows,
        );
        assert_eq!(
            built.sql,
            "SELECT *, (COUNT(*) OVER ())::int AS \"__total\" FROM \"tickets\" WHERE (COALESCE(\"status\" = $1, FALSE) AND COALESCE(\"priority\" >= $2, FALSE) AND COALESCE(\"title\" = ANY($3), FALSE)) ORDER BY \"title\" COLLATE \"C\" DESC NULLS LAST, \"id\" COLLATE \"C\" ASC LIMIT $4 OFFSET $5"
        );
        assert_eq!(
            built.params,
            vec![
                Some("open".into()),
                Some("3".into()),
                Some("{\"a\",\"b\"}".into()),
                Some("3".into()),
                Some("2".into())
            ]
        );
        assert_eq!(
            built.count_params.len(),
            3,
            "the count takes the filter's binds and no window"
        );
        let default = query_page_sql(
            "tickets",
            &query(serde_json::json!({})),
            &ticket_kinds(),
            RowProjection::Ids,
        );
        assert!(default.sql.starts_with("SELECT \"id\", (COUNT(*) OVER ())::int AS \"__total\" FROM \"tickets\" ORDER BY \"created_at\" ASC NULLS LAST"), "{}", default.sql);
    }

    #[test]
    fn a_missing_column_is_absent_on_every_row_and_a_list_leaf_depends_on_the_column_kind() {
        let missing = query_page_sql(
            "tickets",
            &query(
                serde_json::json!({ "filter": { "kind": "isNull", "field": "nowhere", "isNull": true } }),
            ),
            &ticket_kinds(),
            RowProjection::Rows,
        );
        assert!(missing.sql.contains("WHERE TRUE"), "{}", missing.sql);
        let jsonb = query_page_sql(
            "tickets",
            &query(
                serde_json::json!({ "filter": { "kind": "hasElement", "field": "tags", "value": "a" } }),
            ),
            &ticket_kinds(),
            RowProjection::Rows,
        );
        assert!(jsonb.sql.contains("\"tags\" @> $1::jsonb"), "{}", jsonb.sql);
        assert_eq!(jsonb.params[0], Some("[\"a\"]".to_string()));
        let on_text = query_page_sql(
            "tickets",
            &query(
                serde_json::json!({ "filter": { "kind": "hasElement", "field": "title", "value": "a" } }),
            ),
            &ticket_kinds(),
            RowProjection::Rows,
        );
        assert!(
            on_text.sql.contains("WHERE FALSE"),
            "control: a text column has no elements: {}",
            on_text.sql
        );
    }

    #[test]
    fn a_guarded_update_is_one_statement_that_reads_the_pre_image_for_update() {
        let op: RowWriteOp = serde_json::from_value(serde_json::json!({
            "kind": "update", "entityType": "Ticket", "id": "t1", "data": { "title": "x", "tags": ["a"] },
            "guard": { "kind": "compare", "field": "priority", "op": ">", "value": 0 }
        }))
        .unwrap();
        let built = write_sql("tickets", &op, &ticket_kinds());
        assert_eq!(
            built.sql,
            "WITH \"pre\" AS (SELECT * FROM \"tickets\" WHERE \"id\" = $1 FOR UPDATE), \"post\" AS (UPDATE \"tickets\" SET \"title\" = $2, \"tags\" = $3 WHERE \"id\" = $1 AND COALESCE(\"priority\" > $4, FALSE) RETURNING *) SELECT 'before' AS \"__part\", \"pre\".* FROM \"pre\" UNION ALL SELECT 'after' AS \"__part\", \"post\".* FROM \"post\""
        );
        assert_eq!(
            built.params,
            vec![
                Some("t1".into()),
                Some("x".into()),
                Some("[\"a\"]".into()),
                Some("0".into())
            ]
        );
    }

    #[test]
    fn a_create_leaves_a_taken_id_alone_and_an_upsert_replaces_the_row_keeping_created_at() {
        let data: ValueMap = serde_json::from_value(serde_json::json!({ "id": "t1", "title": "x", "createdAt": "2026-01-01T00:00:00.000Z" })).unwrap();
        let created = create_sql("tickets", &data, &ticket_kinds());
        assert_eq!(
            created.sql,
            "INSERT INTO \"tickets\" (\"id\", \"title\", \"created_at\") VALUES ($1, $2, $3) ON CONFLICT (\"id\") DO NOTHING RETURNING *"
        );
        let upserted = upsert_sql("tickets", &data, &ticket_kinds());
        assert!(
            upserted
                .sql
                .contains("ON CONFLICT (\"id\") DO UPDATE SET \"title\" = EXCLUDED.\"title\""),
            "{}",
            upserted.sql
        );
        assert!(
            !upserted.sql.contains("\"created_at\" = EXCLUDED"),
            "the stored created_at is kept: {}",
            upserted.sql
        );
        assert!(
            upserted.sql.contains("\"updated_at\" = now()"),
            "{}",
            upserted.sql
        );
        assert!(
            upserted.sql.contains("\"status\" = DEFAULT"),
            "every other column goes back to its default: {}",
            upserted.sql
        );
        assert!(upserted
            .sql
            .ends_with("RETURNING *, (xmax = 0) AS \"__inserted\""));
    }

    #[test]
    fn a_connection_string_gives_the_connection_settings() {
        let config =
            PgConfig::from_url("postgres://app:p%40ss@db.internal:6543/main?sslmode=require")
                .unwrap();
        assert_eq!(
            config,
            PgConfig {
                host: "db.internal".into(),
                port: 6543,
                user: "app".into(),
                password: Some("p@ss".into()),
                database: "main".into(),
                ssl: SslMode::Require,
                root_cert_pem: None,
                root_cert_path: None,
            }
        );
        let plain = PgConfig::from_url("postgresql://u@h/").unwrap();
        assert_eq!(
            (
                plain.port,
                plain.database.as_str(),
                plain.ssl,
                plain.password
            ),
            (5432, "u", SslMode::Prefer, None)
        );
        assert!(
            PgConfig::from_url("mysql://x").is_err(),
            "control: another scheme is refused"
        );
        let modes = |mode: &str| PgConfig::from_url(&format!("postgres://u@h/d?sslmode={mode}"));
        assert_eq!(modes("disable").unwrap().ssl, SslMode::Disable);
        assert_eq!(
            modes("verify-ca").unwrap().ssl,
            SslMode::VerifyFull,
            "verify-ca is held to verify-full's check"
        );
        assert_eq!(modes("verify-full").unwrap().ssl, SslMode::VerifyFull);
        assert!(
            modes("sometimes").is_err(),
            "control: an unknown mode is refused"
        );
        let pinned =
            PgConfig::from_url("postgres://u@h/d?sslmode=verify-full&sslrootcert=/etc/ca.pem")
                .unwrap();
        assert_eq!(pinned.root_cert_path.as_deref(), Some("/etc/ca.pem"));
    }

    #[test]
    fn a_minted_id_is_a_uuid_v4() {
        let id = mint_id();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert_ne!(id, mint_id());
    }
    #[test]
    fn rekey_is_refused_before_any_database_request_or_successful_prefix() {
        struct NoConnection(std::sync::atomic::AtomicUsize);
        impl PgPort for NoConnection {
            fn connect(&self, _: &str, _: u16) -> Result<Box<dyn PgConn>, String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("unexpected connection".into())
            }
        }
        let port = Arc::new(NoConnection(std::sync::atomic::AtomicUsize::new(0)));
        let rows = PostgresRows::new(
            PgConfig::from_url("postgres://u@localhost/db").unwrap(),
            port.clone(),
        );
        let rekey: RowWriteOp = serde_json::from_value(
            serde_json::json!({"kind":"rekey","entityType":"Item","id":"a","newId":"b"}),
        )
        .unwrap();
        assert!(
            matches!(rows.write_where(&rekey), Some(Err(ref message)) if message == "rekey is unsupported by this store")
        );
        for on_failure in [RowWriteFailureMode::Stop, RowWriteFailureMode::Atomic] {
            let create = RowWriteOp::Create {
                entity_type: "Item".into(),
                data: serde_json::from_value(serde_json::json!({"id":"new"})).unwrap(),
            };
            let result = rows.write_batch(&RowWriteBatch {
                ops: vec![create, rekey.clone()],
                on_failure,
            });
            assert!(
                matches!(result, Some(Err(ref message)) if message == "rekey is unsupported by this store")
            );
        }
        assert_eq!(port.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(rows.write_capabilities().rekey, None);
    }

    mod twin {
        use super::super::*;
        use crate::firestore::to_json;
        use serde_json::{json, Value as Json};
        use std::collections::VecDeque;

        struct Wire {
            replies: VecDeque<Result<Vec<u8>, String>>,
            sent: Vec<Vec<u8>>,
            connect_error: Option<String>,
            connects: Vec<String>,
        }

        struct ScriptedPort(Arc<Mutex<Wire>>);
        struct ScriptedConn(Arc<Mutex<Wire>>);

        impl PgPort for ScriptedPort {
            fn connect(&self, host: &str, port: u16) -> Result<Box<dyn PgConn>, String> {
                if let Some(error) = self.0.lock().connect_error.take() {
                    return Err(format!("cannot connect to {host}:{port}: {error}"));
                }
                self.0.lock().connects.push(format!("{host}:{port}"));
                Ok(Box::new(ScriptedConn(self.0.clone())))
            }
        }

        impl PgConn for ScriptedConn {
            fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
                self.0.lock().sent.push(bytes.to_vec());
                Ok(())
            }
            fn recv(&mut self) -> Result<Vec<u8>, String> {
                self.0
                    .lock()
                    .replies
                    .pop_front()
                    .unwrap_or_else(|| Err("script exhausted".into()))
            }
        }

        fn panic_text(id: &str, panic: &Box<dyn std::any::Any + Send>) -> String {
            let text = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            format!("{id}: {text}")
        }

        fn subset(actual: &Json, expected: &Json) -> bool {
            match (actual, expected) {
                (Json::Object(a), Json::Object(e)) => e.iter().all(|(k, v)| {
                    a.get(k).is_some_and(|x| subset(x, v)) || (v.is_null() && !a.contains_key(k))
                }),
                (Json::Array(a), Json::Array(e)) => {
                    a.len() == e.len() && a.iter().zip(e).all(|(x, y)| subset(x, y))
                }
                (Json::Number(a), Json::Number(e)) => a.as_f64() == e.as_f64(),
                _ => actual == expected,
            }
        }

        fn kind_named(word: &str) -> ColumnKind {
            match word {
                "text" => ColumnKind::Text,
                "jsonb" => ColumnKind::Jsonb,
                "array" => ColumnKind::Array,
                "other" => ColumnKind::Other,
                other => panic!("column kind {other}"),
            }
        }

        fn answer(rows: &PostgresRows, event: &str, payload: &Json) -> Json {
            let key = payload["key"].clone();
            let failed = |message: String| json!({ "event": "STORE_FAILED", "payload": { "key": key, "message": message } });
            let unsupported = || json!({ "event": "STORE_UNSUPPORTED", "payload": { "key": key, "reason": "" } });
            let entity = payload["entityType"].as_str().unwrap_or_default();
            match event {
                "ROW_READ" => match rows.find(entity, payload["id"].as_str().unwrap()) {
                    Ok(row) => json!({ "event": "ROW_FOUND", "payload": { "key": key, "row": row.map_or(Json::Null, |r| to_json(&r)) } }),
                    Err(message) => failed(message),
                },
                "ROW_LIST" => match rows.list(entity) {
                    Ok(list) => json!({ "event": "ROWS_LISTED", "payload": { "key": key, "rows": list.iter().map(|r| to_json(r)).collect::<Vec<_>>() } }),
                    Err(message) => failed(message),
                },
                "ROWS_QUERY" => {
                    let query: RowQuery = serde_json::from_value(payload["query"].clone()).unwrap();
                    if payload["project"] == "ids" {
                        match rows.query_ids(entity, &query) {
                            None => unsupported(),
                            Some(Ok(page)) => json!({ "event": "ROWS_IDS", "payload": { "key": key, "ids": page.ids, "total": page.total } }),
                            Some(Err(message)) => failed(message),
                        }
                    } else {
                        match rows.query_page(entity, &query) {
                            None => unsupported(),
                            Some(Ok(page)) => json!({ "event": "ROWS_PAGE", "payload": { "key": key, "rows": page.rows.iter().map(to_json).collect::<Vec<_>>(), "total": page.total } }),
                            Some(Err(message)) => failed(message),
                        }
                    }
                }
                "ROWS_WRITE" => {
                    let batch: RowWriteBatch =
                        serde_json::from_value(payload["batch"].clone()).unwrap();
                    match rows.write_batch(&batch) {
                        None => unsupported(),
                        Some(Ok(outcomes)) => json!({ "event": "ROWS_WRITTEN", "payload": { "key": key, "outcomes": serde_json::to_value(&outcomes).unwrap() } }),
                        Some(Err(message)) => failed(message),
                    }
                }
                other => panic!("unknown verb {other}"),
            }
        }

        fn params_of(tap: &Json) -> Vec<Option<String>> {
            tap["payload"]["params"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| match p["kind"].as_str().unwrap() {
                    "null" => None,
                    _ => Some(p["text"].as_str().unwrap().to_string()),
                })
                .collect()
        }

        #[test]
        fn the_std_twin_replay_cases_run_over_the_same_scripted_postgres_wire() {
            let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
                .join("tests/baked/store/postgres-twin.replay.goldens.json");
            let file: Json =
                serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json");
            let mut ran = 0;
            let mut skipped = 0;
            let mut failures: Vec<String> = Vec::new();
            for case in file["cases"].as_array().unwrap() {
                let id = case["id"].as_str().unwrap();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> bool {
                // the std atom has no case-level config here (G-ORB-460), Rust draws its SCRAM client nonce from
                // getrandom with no seam to pin it, and Rust upgrades to TLS on an S answer where std refuses
                if case.get("skip").is_some()
                    || id.contains("scram")
                    || id == "bad-tls-answer-s-is-refused-because-no-tls-over-os-sockets"
                {
                    return false;
                }
                let steps = case["steps"].as_array().unwrap();
                let verb = &steps[0];
                let entity = &verb["entity"];
                let wire = Arc::new(Mutex::new(Wire {
                    replies: VecDeque::new(),
                    sent: Vec::new(),
                    connect_error: None,
                    connects: Vec::new(),
                }));
                let rows = PostgresRows::new(
                    PgConfig::from_url("postgres://app:secret@postgres.example.test:5432/app")
                        .unwrap(),
                    Arc::new(ScriptedPort(wire.clone())),
                );
                if let Some(schema) = entity["schema"].as_object() {
                    for (table, columns) in schema {
                        let kinds: Kinds = columns
                            .as_object()
                            .unwrap()
                            .iter()
                            .map(|(column, word)| (column.clone(), kind_named(word.as_str().unwrap())))
                            .collect();
                        rows.kinds.lock().insert(table.clone(), kinds);
                    }
                }
                for (entity_type, table) in entity["collections"].as_object().into_iter().flatten() {
                    rows.bind_collection(entity_type, table.as_str().unwrap());
                }
                let mut base = 0;
                if entity["sock"].as_i64().is_some_and(|sock| sock >= 0) {
                    let mut login = message(b'R', &0u32.to_be_bytes());
                    login.extend(message(b'Z', b"I"));
                    {
                        let mut script = wire.lock();
                        script.replies.push_back(Ok(b"N".to_vec()));
                        script.replies.push_back(Ok(login));
                    }
                    drop(rows.lease().unwrap_or_else(|e| panic!("{id}: {}", e.message)));
                    base = wire.lock().sent.len();
                }
                let mut connected = entity["sock"].as_i64().is_some_and(|sock| sock >= 0);
                for step in &steps[1..] {
                    let mut script = wire.lock();
                    match step["event"].as_str().unwrap() {
                        "PG_CONNECTED" => connected = true,
                        "PG_RECEIVED" => {
                            let payload = &step["payload"];
                            script.replies.push_back(Ok(if payload["eof"] == true {
                                Vec::new()
                            } else {
                                codec::base64_decode(payload["data"].as_str().unwrap()).unwrap()
                            }));
                        }
                        "PG_IO_FAILED" => {
                            let error = step["payload"]["error"].as_str().unwrap().to_string();
                            if connected {
                                script.replies.push_back(Err(error));
                            } else {
                                script.connect_error = Some(error);
                            }
                        }
                        other => panic!("{id}: unexpected {other}"),
                    }
                }
                let got = answer(&rows, verb["event"].as_str().unwrap(), &verb["payload"]);
                let sent: Vec<Vec<u8>> = wire.lock().sent[base..].to_vec();
                let has_terminal = steps.iter().any(|s| {
                    s["expect"]["emits"]
                        .as_array()
                        .is_some_and(|emits| emits.iter().any(|e| e["event"] != "PG_WIRE_SENT"))
                });
                if !has_terminal {
                    let origins: Vec<String> = steps
                        .iter()
                        .flat_map(|s| s["expect"]["effects"].as_array().into_iter().flatten())
                        .filter(|effect| effect["type"] == "os/tcp-connect")
                        .map(|effect| format!("{}:{}", effect["arg0"].as_str().unwrap(), effect["arg1"]))
                        .collect();
                    let made = wire.lock().connects.clone();
                    assert!(made.len() >= origins.len(), "{id}: connect origins {origins:?} vs {made:?}");
                    assert_eq!(&made[..origins.len()], &origins[..], "{id}: connect origin");
                }
                let taps: Vec<&Json> = steps
                    .iter()
                    .flat_map(|s| s["expect"]["emits"].as_array().into_iter().flatten())
                    .filter(|e| e["event"] == "PG_WIRE_SENT")
                    .collect();
                for (n, tap) in taps.iter().enumerate() {
                    let bytes = sent
                        .get(n)
                        .unwrap_or_else(|| panic!("{id}: send {n} ({}) was not made", tap["payload"]["stage"]));
                    let want = codec::base64_decode(tap["payload"]["frame"].as_str().unwrap()).unwrap();
                    assert_eq!(bytes, &want, "{id}: send {n} ({}) frame", tap["payload"]["stage"]);
                    if matches!(
                        tap["payload"]["stage"].as_str().unwrap(),
                        "statement" | "discover" | "begin" | "count" | "commit" | "rollback"
                    ) {
                        let sql = tap["payload"]["sql"].as_str().unwrap();
                        assert_eq!(
                            bytes,
                            &crate::postgres::query_messages(sql, &params_of(tap)),
                            "{id}: send {n} sql {sql} and its binds"
                        );
                    }
                }
                let last = steps.last().unwrap();
                let exhausted = !has_terminal
                    && got["event"] == "STORE_FAILED"
                    && got["payload"]["message"] == "script exhausted";
                let terminal = last["expect"]["emits"]
                    .as_array()
                    .and_then(|emits| emits.iter().find(|e| e["event"] != "PG_WIRE_SENT"));
                if let Some(expected) = terminal {
                    assert_eq!(got["event"], expected["event"], "{id}: {got}");
                    let mut have = got["payload"].clone();
                    let mut want = expected["payload"].clone();
                    if expected["event"] == "STORE_UNSUPPORTED" {
                        have.as_object_mut().unwrap().remove("reason");
                        want.as_object_mut().unwrap().remove("reason");
                    }
                    assert!(subset(&have, &want), "{id}: {have} vs {want}");
                    assert_eq!(sent.len(), taps.len(), "{id}: no send beyond the expected ones");
                }
                if !exhausted {
                    for never in last["expect"]["neverEmits"].as_array().into_iter().flatten() {
                        assert_ne!(got["event"], *never, "{id}: {got}");
                    }
                }
                true
                }));
                match outcome {
                    Ok(true) => ran += 1,
                    Ok(false) => skipped += 1,
                    Err(panic) => failures.push(panic_text(id, &panic)),
                }
            }
            assert!(failures.is_empty(), "{} failing cases:\n{}", failures.len(), failures.join("\n"));
            assert!(ran > 60, "ran {ran} cases, skipped {skipped}");
        }
    }
}
