//! Structured, namespaced logging and span timing — the Rust mirror of `@almadar/logger`.
//!
//! Same contract as the TypeScript stack so the Rust shell's logs read the same:
//!
//! - **Namespaces** are `:`-delimited and hierarchical, e.g. `almadar:runtime:fetch`,
//!   `almadar:server:data`, `almadar:bridge`, `almadar:ui:data-grid`. In Rust they are
//!   the tracing event `target`. Use the [`ns`] constants or pass `target:` directly.
//! - **Levels**: `error`/`warn`/`info`/`debug`/`trace` (`tracing` macros).
//! - **Structured fields**: `debug!(target: ns::RUNTIME_FETCH, entity = %e, count = n, "fetch:list")`.
//! - **Enable/disable** via the `ALMADAR_DEBUG` env var — an ALLOWLIST of namespace patterns,
//!   comma-separated, each either exact (`almadar:runtime:fetch`), a prefix wildcard
//!   (`almadar:runtime:*`), or `*` (everything). `WARN`/`ERROR` ALWAYS fire regardless of
//!   the allowlist (only `DEBUG`/`INFO`/`TRACE` are namespace-gated), matching `@almadar/logger`.
//!   An allowlisted namespace fires at every level.
//! - `LOG_LEVEL` (default `warn`) sets the global floor. With no allowlist the floor applies to
//!   every namespace; with one, levels below `WARN` fire only for allowlisted namespaces.
//! - `ALMADAR_LOG_LEVELS="almadar:lolo:*=info,almadar:runtime:sm=trace"` gives a namespace its
//!   own floor: an exact namespace beats every `prefix:*`, and the longest prefix wins.
//! - `ALMADAR_LOG_FORMAT=json` writes one [`StructuredLogEntry`] per line (the
//!   `@almadar/core` observability shape) instead of console lines.
//! - **Timing** is spans: a closed span's wall time is its duration. [`Timings`] records every
//!   closed span for a timing tree and a Chrome trace; in json format a closed span the filter
//!   admits is a `[PROFILE] <name>` entry carrying `elapsedMs`, as `profile()` writes in TS.
//!
//! Example: `ALMADAR_DEBUG='almadar:runtime:*,almadar:bridge' orb verify <app> --screenshot …`

/// Canonical namespace constants (mirror the TS `createLogger('…')` namespaces).
pub mod ns {
    pub const RUNTIME_KERNEL: &str = "almadar:runtime:kernel";
    pub const RUNTIME_FETCH: &str = "almadar:runtime:fetch";
    pub const RUNTIME_EFFECTS: &str = "almadar:runtime:effects";
    pub const RUNTIME_PERSIST: &str = "almadar:runtime:persist";
    pub const RUNTIME_SM: &str = "almadar:runtime:sm";
    pub const RUNTIME_OS: &str = "almadar:runtime:os";
    pub const SERVER_DATA: &str = "almadar:server:data";
    pub const BRIDGE: &str = "almadar:bridge";
    pub const SEED: &str = "almadar:runtime:seed";
    pub const UI_DATA_GRID: &str = "almadar:ui:data-grid";
    pub const CLI: &str = "almadar:cli";
    pub const COMPILER: &str = "almadar:compiler";
    pub const LOLO: &str = "almadar:lolo";
    pub const SHELL: &str = "almadar:shell";
    /// A running program's own `(log …)` lines.
    pub const PROGRAM: &str = "almadar:program";
}

/// The span field a phase records its outcome in (`"ok"` or `"failed"`), see [`record_outcome`].
pub const OUTCOME: &str = "outcome";

/// Record `result` as the outcome of `span`, which declares `outcome = tracing::field::Empty`.
pub fn record_outcome<T, E>(span: &tracing::Span, result: &Result<T, E>) {
    span.record(OUTCOME, if result.is_ok() { "ok" } else { "failed" });
}

#[cfg(feature = "std")]
pub use hosted::*;

/// The subscriber, its configuration and the timing tree: a hosted process's logging, absent on bare metal.
#[cfg(feature = "std")]
mod hosted {
use crate::sync::Mutex;
use serde::Serialize;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Filter, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;


/// One parsed namespace pattern: exact, `prefix*`, or `*`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pattern {
    All,
    Prefix(String),
    Exact(String),
}

impl Pattern {
    fn parse(raw: &str) -> Option<Self> {
        let p = raw.trim();
        if p.is_empty() {
            None
        } else if p == "*" {
            Some(Pattern::All)
        } else if let Some(prefix) = p.strip_suffix('*') {
            // `almadar:runtime:*` → prefix `almadar:runtime:`
            Some(Pattern::Prefix(prefix.to_string()))
        } else {
            Some(Pattern::Exact(p.to_string()))
        }
    }

    fn matches(&self, target: &str) -> bool {
        match self {
            Pattern::All => true,
            Pattern::Prefix(prefix) => target.starts_with(prefix.as_str()),
            Pattern::Exact(exact) => target == exact,
        }
    }

    fn spelling(&self) -> String {
        match self {
            Pattern::All => "*".to_string(),
            Pattern::Prefix(prefix) => format!("{prefix}*"),
            Pattern::Exact(exact) => exact.clone(),
        }
    }

    /// Exact beats every wildcard; among wildcards the longer prefix wins.
    fn specificity(&self) -> usize {
        match self {
            Pattern::All => 0,
            Pattern::Prefix(prefix) => prefix.len() + 1,
            Pattern::Exact(_) => usize::MAX,
        }
    }
}

fn parse_level(raw: &str) -> Option<Level> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "error" => Some(Level::ERROR),
        "warn" => Some(Level::WARN),
        "info" => Some(Level::INFO),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => None,
    }
}

/// Which lines fire: the global floor, the `ALMADAR_DEBUG` allowlist and the
/// `ALMADAR_LOG_LEVELS` per-namespace floors.
#[derive(Clone, Debug)]
pub struct LogConfig {
    floor: Level,
    allow: Vec<Pattern>,
    levels: Vec<(Pattern, Level)>,
    skipped: Vec<String>,
}

impl Default for LogConfig {
    fn default() -> Self {
        LogConfig {
            floor: Level::WARN,
            allow: Vec::new(),
            levels: Vec::new(),
            skipped: Vec::new(),
        }
    }
}

impl LogConfig {
    /// `LOG_LEVEL`, `ALMADAR_DEBUG` and `ALMADAR_LOG_LEVELS` from the environment.
    pub fn from_env() -> Self {
        Self::parse(
            std::env::var("LOG_LEVEL").ok().as_deref(),
            std::env::var("ALMADAR_DEBUG").ok().as_deref(),
            std::env::var("ALMADAR_LOG_LEVELS").ok().as_deref(),
        )
    }

    pub fn parse(log_level: Option<&str>, allowlist: Option<&str>, levels: Option<&str>) -> Self {
        let mut config = LogConfig::default();
        if let Some(raw) = log_level {
            match parse_level(raw) {
                Some(level) => config.floor = level,
                None => config
                    .skipped
                    .push(format!("LOG_LEVEL: skipped \"{raw}\" (expected error|warn|info|debug|trace)")),
            }
        }
        config.allow = allowlist
            .unwrap_or_default()
            .split(',')
            .filter_map(Pattern::parse)
            .collect();
        for entry in levels.unwrap_or_default().split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let parsed = entry
                .rsplit_once('=')
                .and_then(|(namespace, level)| Some((Pattern::parse(namespace)?, parse_level(level)?)));
            match parsed {
                Some(pair) => config.levels.push(pair),
                None => config.skipped.push(format!(
                    "ALMADAR_LOG_LEVELS: skipped \"{entry}\" (expected <namespace>=error|warn|info|debug|trace)"
                )),
            }
        }
        config
    }

    /// The same config with `floor` as the global floor.
    pub fn with_floor(mut self, floor: Level) -> Self {
        self.floor = floor;
        self
    }

    pub fn floor(&self) -> Level {
        self.floor
    }

    /// The env variables that reproduce this config and `format` in a child process's
    /// [`init`]: `LOG_LEVEL`, `ALMADAR_DEBUG`, `ALMADAR_LOG_LEVELS`, `ALMADAR_LOG_FORMAT`.
    pub fn env(&self, format: LogFormat) -> [(&'static str, String); 4] {
        let join = |items: Vec<String>| items.join(",");
        [
            ("LOG_LEVEL", self.floor.as_str().to_ascii_lowercase()),
            ("ALMADAR_DEBUG", join(self.allow.iter().map(Pattern::spelling).collect())),
            (
                "ALMADAR_LOG_LEVELS",
                join(
                    self.levels
                        .iter()
                        .map(|(p, l)| format!("{}={}", p.spelling(), l.as_str().to_ascii_lowercase()))
                        .collect(),
                ),
            ),
            (
                "ALMADAR_LOG_FORMAT",
                match format {
                    LogFormat::Console => "console".to_string(),
                    LogFormat::Json => "json".to_string(),
                },
            ),
        ]
    }

    /// The env entries that did not parse, one message each.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    fn allowlisted(&self, namespace: &str) -> bool {
        self.allow.iter().any(|p| p.matches(namespace))
    }

    /// The most verbose level `namespace` fires at: its own level, else every level when
    /// allowlisted, else the global floor.
    pub fn namespace_level(&self, namespace: &str) -> Level {
        let own = self
            .levels
            .iter()
            .filter(|(pattern, _)| pattern.matches(namespace))
            .max_by_key(|(pattern, _)| pattern.specificity())
            .map(|(_, level)| *level);
        match own {
            Some(level) => level,
            None if self.allowlisted(namespace) => Level::TRACE,
            None => self.floor,
        }
    }

    pub fn enabled(&self, level: Level, namespace: &str) -> bool {
        // tracing Ord: ERROR < WARN < INFO < DEBUG < TRACE (more verbose is "greater").
        if level > self.namespace_level(namespace) {
            return false;
        }
        level <= Level::WARN || self.allow.is_empty() || self.allowlisted(namespace)
    }
}

/// A per-layer filter applying a [`LogConfig`] to every event, and to spans unless it is
/// for events only.
pub struct NamespaceFilter {
    config: LogConfig,
    spans: bool,
}

impl NamespaceFilter {
    pub fn new(config: LogConfig) -> Self {
        NamespaceFilter { config, spans: true }
    }

    /// The same filter refusing every span.
    pub fn events_only(mut self) -> Self {
        self.spans = false;
        self
    }
}

impl<S> Filter<S> for NamespaceFilter {
    fn enabled(&self, meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        (self.spans || !meta.is_span()) && self.config.enabled(*meta.level(), meta.target())
    }
}

/// How log lines are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// Human console lines.
    Console,
    /// One [`StructuredLogEntry`] JSON object per line.
    Json,
}

impl LogFormat {
    /// `ALMADAR_LOG_FORMAT=json`, else console.
    pub fn from_env() -> Self {
        match std::env::var("ALMADAR_LOG_FORMAT").ok().as_deref() {
            Some("json") => LogFormat::Json,
            _ => LogFormat::Console,
        }
    }
}

/// Console line options.
#[derive(Clone, Copy, Debug)]
pub struct ConsoleOptions {
    /// Colored levels and dimmed targets.
    pub ansi: bool,
    /// Stamp lines with the time since start instead of the wall clock.
    pub uptime: bool,
    /// Prefix each line with its enclosing spans and their fields.
    pub spans: bool,
}

impl ConsoleOptions {
    /// Color when stderr is a terminal and `NO_COLOR` is unset; wall-clock stamps.
    pub fn for_stderr() -> Self {
        ConsoleOptions {
            ansi: stderr_supports_color(),
            uptime: false,
            spans: true,
        }
    }
}

/// stderr is a terminal and `NO_COLOR` (https://no-color.org) is unset.
pub fn stderr_supports_color() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

/// The layer that writes log lines in `format` to `writer`, filtered by `config`.
pub fn event_layer<S, W>(
    format: LogFormat,
    config: LogConfig,
    console: ConsoleOptions,
    writer: W,
) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let filter = NamespaceFilter::new(config);
    match format {
        LogFormat::Console => {
            let filter = if console.spans { filter } else { filter.events_only() };
            let fmt = tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_target(true)
                .with_level(true)
                .with_ansi(console.ansi)
                .compact();
            if console.uptime {
                fmt.with_timer(tracing_subscriber::fmt::time::uptime())
                    .with_filter(filter)
                    .boxed()
            } else {
                fmt.with_filter(filter).boxed()
            }
        }
        LogFormat::Json => JsonLines { writer }.with_filter(filter).boxed(),
    }
}

/// Report the env entries `config` skipped, once a subscriber is installed.
pub fn report_skipped(config: &LogConfig) {
    for message in config.skipped() {
        tracing::warn!(target: "almadar:logger", "{message}");
    }
}

static INIT: Once = Once::new();

/// Install the namespaced tracing subscriber (idempotent; safe to call from every binary
/// entry point). Reads `ALMADAR_DEBUG`, `LOG_LEVEL`, `ALMADAR_LOG_LEVELS` and
/// `ALMADAR_LOG_FORMAT`. No-op if a subscriber is already set.
pub fn init() {
    INIT.call_once(|| {
        let config = LogConfig::from_env();
        // stdout is the data channel (`--json`); logs never share it.
        let layer = event_layer(
            LogFormat::from_env(),
            config.clone(),
            ConsoleOptions::for_stderr(),
            std::io::stderr,
        );
        if tracing_subscriber::registry().with(layer).try_init().is_ok() {
            report_skipped(&config);
        }
    });
}

/// Cloud Logging severities, as `@almadar/core` `LogSeverity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum LogSeverity {
    #[serde(rename = "DEBUG")]
    Debug,
    #[serde(rename = "INFO")]
    Info,
    #[serde(rename = "WARNING")]
    Warning,
    #[serde(rename = "ERROR")]
    Error,
}

impl LogSeverity {
    /// TRACE has no Cloud Logging severity and is written as DEBUG.
    pub fn of(level: Level) -> Self {
        match level {
            Level::ERROR => LogSeverity::Error,
            Level::WARN => LogSeverity::Warning,
            Level::INFO => LogSeverity::Info,
            _ => LogSeverity::Debug,
        }
    }
}

/// One structured log line, as `@almadar/core` `StructuredLogEntry`.
#[derive(Clone, Debug, Serialize)]
pub struct StructuredLogEntry {
    pub severity: LogSeverity,
    pub time: String,
    pub namespace: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Collects tracing fields as JSON; `message` is kept apart.
#[derive(Default)]
struct JsonFields {
    message: Option<String>,
    data: serde_json::Map<String, serde_json::Value>,
}

impl JsonFields {
    fn put(&mut self, field: &Field, value: serde_json::Value) {
        if field.name() == "message" {
            self.message = Some(match value {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            });
        } else {
            self.data.insert(field.name().to_string(), value);
        }
    }
}

impl Visit for JsonFields {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, serde_json::json!(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.into());
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.into());
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, value.to_string().into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field, format!("{value:?}").into());
    }
}

/// A span's start and fields, kept in its extensions by the layers that time it.
struct SpanStart {
    at: Instant,
    fields: serde_json::Map<String, serde_json::Value>,
}

fn span_fields(attrs: &Attributes<'_>) -> serde_json::Map<String, serde_json::Value> {
    let mut fields = JsonFields::default();
    attrs.record(&mut fields);
    fields.data
}

fn elapsed_ms(elapsed: Duration) -> f64 {
    (elapsed.as_secs_f64() * 10_000.0).round() / 10.0
}

/// The `ALMADAR_LOG_FORMAT=json` writer.
struct JsonLines<W> {
    writer: W,
}

impl<W> JsonLines<W>
where
    W: for<'w> MakeWriter<'w>,
{
    fn write(&self, entry: &StructuredLogEntry) {
        let mut line = match serde_json::to_vec(entry) {
            Ok(line) => line,
            Err(_) => return,
        };
        line.push(b'\n');
        // A failed write to the log channel has nowhere else to be reported.
        let _ = self.writer.make_writer().write_all(&line);
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl<S, W> Layer<S> for JsonLines<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + 'static,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanStart {
                at: Instant::now(),
                fields: span_fields(attrs),
            });
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(start) = span.extensions_mut().get_mut::<SpanStart>() {
                let mut fields = JsonFields::default();
                values.record(&mut fields);
                start.fields.extend(fields.data);
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = JsonFields::default();
        event.record(&mut fields);
        let meta = event.metadata();
        self.write(&StructuredLogEntry {
            severity: LogSeverity::of(*meta.level()),
            time: now_iso(),
            namespace: meta.target().to_string(),
            message: fields.message.unwrap_or_default(),
            data: (!fields.data.is_empty()).then_some(fields.data),
        });
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(start) = span.extensions_mut().remove::<SpanStart>() else { return };
        let mut data = start.fields;
        data.insert("elapsedMs".to_string(), serde_json::json!(elapsed_ms(start.at.elapsed())));
        let meta = span.metadata();
        self.write(&StructuredLogEntry {
            severity: LogSeverity::of(*meta.level()),
            time: now_iso(),
            namespace: meta.target().to_string(),
            message: format!("[PROFILE] {}", meta.name()),
            data: Some(data),
        });
    }
}

/// One closed span.
#[derive(Clone, Debug)]
pub struct SpanRecord {
    pub name: &'static str,
    pub target: &'static str,
    /// Span names from the root down to this span.
    pub path: Vec<&'static str>,
    /// When the span opened, from when recording began.
    pub start: Duration,
    pub elapsed: Duration,
    /// A small per-process number for the thread the span opened on.
    pub thread: u64,
    pub fields: serde_json::Map<String, serde_json::Value>,
}

struct TimingsState {
    origin: Instant,
    closed: Vec<SpanRecord>,
}

/// Records every closed span (any level, any namespace) for [`Timings::tree`] and
/// [`Timings::chrome_trace`]. Clone it to keep a handle; install [`Timings::layer`].
#[derive(Clone)]
pub struct Timings {
    state: Arc<Mutex<TimingsState>>,
}

impl Default for Timings {
    fn default() -> Self {
        Self::new()
    }
}

struct RecordedStart {
    at: Instant,
    path: Vec<&'static str>,
    thread: u64,
    fields: serde_json::Map<String, serde_json::Value>,
}

fn thread_number() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static NUMBER: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    NUMBER.with(|n| *n)
}

impl Timings {
    pub fn new() -> Self {
        Timings {
            state: Arc::new(Mutex::new(TimingsState {
                origin: Instant::now(),
                closed: Vec::new(),
            })),
        }
    }

    /// The recording layer; it sees spans only, so events cost it nothing.
    pub fn layer<S>(&self) -> impl Layer<S> + Send + Sync
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        TimingsLayer {
            state: self.state.clone(),
        }
        .with_filter(tracing_subscriber::filter::filter_fn(|meta| meta.is_span()))
    }

    /// Every span closed so far, in closing order.
    pub fn records(&self) -> Vec<SpanRecord> {
        self.state.lock().closed.clone()
    }

    pub fn tree(&self) -> TimingTree {
        TimingTree::of(&self.records())
    }

    /// The closed spans as a Chrome trace (`chrome://tracing`, ui.perfetto.dev): one complete
    /// (`"X"`) event per span, microseconds from when recording began.
    pub fn chrome_trace(&self) -> serde_json::Value {
        let events: Vec<serde_json::Value> = self
            .records()
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "cat": r.target,
                    "ph": "X",
                    "ts": r.start.as_micros() as u64,
                    "dur": r.elapsed.as_micros() as u64,
                    "pid": std::process::id(),
                    "tid": r.thread,
                    "args": r.fields,
                })
            })
            .collect();
        serde_json::json!({ "traceEvents": events, "displayTimeUnit": "ms" })
    }
}

struct TimingsLayer {
    state: Arc<Mutex<TimingsState>>,
}

impl<S> Layer<S> for TimingsLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut path = span
            .parent()
            .and_then(|parent| parent.extensions().get::<RecordedStart>().map(|p| p.path.clone()))
            .unwrap_or_default();
        path.push(span.name());
        span.extensions_mut().insert(RecordedStart {
            at: Instant::now(),
            path,
            thread: thread_number(),
            fields: span_fields(attrs),
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(start) = span.extensions_mut().get_mut::<RecordedStart>() {
                let mut fields = JsonFields::default();
                values.record(&mut fields);
                start.fields.extend(fields.data);
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(start) = span.extensions_mut().remove::<RecordedStart>() else { return };
        let mut state = self.state.lock();
        let record = SpanRecord {
            name: span.name(),
            target: span.metadata().target(),
            path: start.path,
            start: start.at.saturating_duration_since(state.origin),
            elapsed: start.at.elapsed(),
            thread: start.thread,
            fields: start.fields,
        };
        state.closed.push(record);
    }
}

/// Closed spans merged by path: a node per distinct root-to-span name path, children in the
/// order they first opened.
#[derive(Clone, Debug, Default)]
pub struct TimingTree {
    pub roots: Vec<TimingNode>,
}

#[derive(Clone, Debug)]
pub struct TimingNode {
    pub name: &'static str,
    pub calls: usize,
    /// Summed wall time of every call.
    pub total: Duration,
    first_start: Duration,
    pub children: Vec<TimingNode>,
}

impl TimingNode {
    /// `total` less the children's totals; zero when children ran in parallel past it.
    pub fn self_time(&self) -> Duration {
        let children: Duration = self.children.iter().map(|c| c.total).sum();
        self.total.saturating_sub(children)
    }
}

fn insert_record(nodes: &mut Vec<TimingNode>, path: &[&'static str], record: &SpanRecord) {
    let Some((head, rest)) = path.split_first() else { return };
    let index = match nodes.iter().position(|n| n.name == *head) {
        Some(index) => index,
        None => {
            nodes.push(TimingNode {
                name: head,
                calls: 0,
                total: Duration::ZERO,
                first_start: record.start,
                children: Vec::new(),
            });
            nodes.len() - 1
        }
    };
    let node = &mut nodes[index];
    node.first_start = node.first_start.min(record.start);
    if rest.is_empty() {
        node.calls += 1;
        node.total += record.elapsed;
    } else {
        insert_record(&mut node.children, rest, record);
    }
}

fn sort_nodes(nodes: &mut [TimingNode]) {
    nodes.sort_by_key(|n| n.first_start);
    for node in nodes {
        sort_nodes(&mut node.children);
    }
}

impl TimingTree {
    pub fn of(records: &[SpanRecord]) -> Self {
        let mut roots = Vec::new();
        for record in records {
            insert_record(&mut roots, &record.path, record);
        }
        sort_nodes(&mut roots);
        TimingTree { roots }
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

/// `0.42ms`, `41.3ms`, `1.42s`.
pub fn format_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 10.0 {
        format!("{ms:.2}ms")
    } else if ms < 1000.0 {
        format!("{ms:.1}ms")
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

impl std::fmt::Display for TimingTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut rows: Vec<(String, &TimingNode)> = Vec::new();
        fn walk<'a>(nodes: &'a [TimingNode], prefix: &str, top: bool, rows: &mut Vec<(String, &'a TimingNode)>) {
            for (i, node) in nodes.iter().enumerate() {
                let last = i + 1 == nodes.len();
                let (branch, carry) = match (top, last) {
                    (true, _) => ("", ""),
                    (false, true) => ("└─ ", "   "),
                    (false, false) => ("├─ ", "│  "),
                };
                rows.push((format!("{prefix}{branch}{}", node.name), node));
                walk(&node.children, &format!("{prefix}{carry}"), false, rows);
            }
        }
        walk(&self.roots, "", true, &mut rows);
        let width = rows.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0);
        let mut out = String::new();
        let _ = writeln!(out, "{:<width$}  {:>9}  {:>9}  {:>6}", "span", "total", "self", "calls");
        for (label, node) in &rows {
            let pad = width - label.chars().count();
            let _ = writeln!(
                out,
                "{label}{:pad$}  {:>9}  {:>9}  {:>6}",
                "",
                format_duration(node.total),
                format_duration(node.self_time()),
                node.calls,
            );
        }
        f.write_str(&out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::{record_outcome, OUTCOME};

    #[test]
    fn default_config_fires_warn_and_error_only() {
        let config = LogConfig::parse(None, None, None);
        assert!(config.enabled(Level::ERROR, "almadar:cli"));
        assert!(config.enabled(Level::WARN, "almadar:cli"));
        assert!(!config.enabled(Level::INFO, "almadar:cli"));
        assert!(!config.enabled(Level::DEBUG, "orbital_compiler::phases"));
    }

    #[test]
    fn an_allowlisted_namespace_fires_every_level_and_the_rest_keep_the_floor() {
        let config = LogConfig::parse(None, Some("almadar:runtime:*"), None);
        assert!(config.enabled(Level::TRACE, "almadar:runtime:sm"));
        assert!(!config.enabled(Level::INFO, "almadar:cli"));
        assert!(config.enabled(Level::WARN, "almadar:cli"));
    }

    #[test]
    fn a_raised_floor_reaches_every_namespace_without_an_allowlist() {
        let config = LogConfig::parse(None, None, None).with_floor(Level::DEBUG);
        assert!(config.enabled(Level::DEBUG, "orbital_compiler::phases"));
        assert!(!config.enabled(Level::TRACE, "orbital_compiler::phases"));
    }

    #[test]
    fn with_an_allowlist_a_raised_floor_reaches_only_the_allowlisted() {
        let config = LogConfig::parse(Some("debug"), Some("almadar:cli"), None);
        assert!(config.enabled(Level::DEBUG, "almadar:cli"));
        assert!(!config.enabled(Level::DEBUG, "almadar:runtime:sm"));
    }

    #[test]
    fn a_quiet_floor_still_fires_errors() {
        let config = LogConfig::parse(None, None, None).with_floor(Level::ERROR);
        assert!(config.enabled(Level::ERROR, "almadar:cli"));
        assert!(!config.enabled(Level::WARN, "almadar:cli"));
    }

    #[test]
    fn exact_namespace_level_beats_every_prefix_and_the_longest_prefix_wins() {
        let config = LogConfig::parse(
            Some("warn"),
            None,
            Some("almadar:*=info, almadar:runtime:*=debug, almadar:runtime:sm=error"),
        );
        assert_eq!(config.namespace_level("almadar:runtime:sm"), Level::ERROR);
        assert_eq!(config.namespace_level("almadar:runtime:fetch"), Level::DEBUG);
        assert_eq!(config.namespace_level("almadar:cli"), Level::INFO);
        assert_eq!(config.namespace_level("orbital_lolo"), Level::WARN);
        assert!(!config.enabled(Level::WARN, "almadar:runtime:sm"));
    }

    #[test]
    fn malformed_env_entries_are_skipped_and_reported() {
        let config = LogConfig::parse(Some("loud"), None, Some("almadar:cli=loud,=info,almadar:lolo=info"));
        assert_eq!(config.floor(), Level::WARN);
        assert_eq!(config.skipped().len(), 3, "{:?}", config.skipped());
        assert_eq!(config.namespace_level("almadar:lolo"), Level::INFO);
    }

    #[test]
    fn a_config_written_to_env_reads_back_the_same() {
        let config = LogConfig::parse(Some("warn"), Some("almadar:runtime:*,almadar:cli"), Some("almadar:lolo=trace,*=error"))
            .with_floor(Level::DEBUG);
        let env = config.env(LogFormat::Json);
        let get = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str());
        let back = LogConfig::parse(get("LOG_LEVEL"), get("ALMADAR_DEBUG"), get("ALMADAR_LOG_LEVELS"));
        assert_eq!(back.floor(), Level::DEBUG);
        assert!(back.skipped().is_empty(), "{:?}", back.skipped());
        for ns in ["almadar:runtime:sm", "almadar:cli", "almadar:lolo", "orbital_lolo"] {
            assert_eq!(back.namespace_level(ns), config.namespace_level(ns), "{ns}");
            assert_eq!(back.enabled(Level::INFO, ns), config.enabled(Level::INFO, ns), "{ns}");
        }
        assert_eq!(get("ALMADAR_LOG_FORMAT"), Some("json"));
    }

    #[test]
    fn control_an_empty_config_writes_empty_lists_that_read_as_none() {
        let env = LogConfig::default().env(LogFormat::Console);
        let get = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str());
        assert_eq!((get("LOG_LEVEL"), get("ALMADAR_DEBUG"), get("ALMADAR_LOG_LEVELS")), (Some("warn"), Some(""), Some("")));
        let back = LogConfig::parse(get("LOG_LEVEL"), get("ALMADAR_DEBUG"), get("ALMADAR_LOG_LEVELS"));
        assert!(!back.enabled(Level::INFO, "almadar:cli"));
        assert!(back.enabled(Level::WARN, "almadar:cli"));
    }

    fn record(path: &[&'static str], start_ms: u64, elapsed_ms: u64) -> SpanRecord {
        SpanRecord {
            name: path[path.len() - 1],
            target: "almadar:cli",
            path: path.to_vec(),
            start: Duration::from_millis(start_ms),
            elapsed: Duration::from_millis(elapsed_ms),
            thread: 1,
            fields: serde_json::Map::new(),
        }
    }

    #[test]
    fn the_timing_tree_merges_repeated_paths_and_orders_children_by_first_start() {
        let tree = TimingTree::of(&[
            record(&["orb", "lower"], 5, 10),
            record(&["orb", "parse"], 1, 3),
            record(&["orb", "lower"], 20, 30),
            record(&["orb"], 0, 100),
        ]);
        let root = &tree.roots[0];
        assert_eq!((root.name, root.calls, root.total), ("orb", 1, Duration::from_millis(100)));
        let names: Vec<_> = root.children.iter().map(|c| (c.name, c.calls)).collect();
        assert_eq!(names, vec![("parse", 1), ("lower", 2)]);
        assert_eq!(root.children[1].total, Duration::from_millis(40));
        assert_eq!(root.self_time(), Duration::from_millis(57));
    }

    #[test]
    fn self_time_never_goes_negative_when_children_ran_in_parallel() {
        let tree = TimingTree::of(&[
            record(&["orb", "lower"], 1, 80),
            record(&["orb", "lower"], 1, 80),
            record(&["orb"], 0, 100),
        ]);
        assert_eq!(tree.roots[0].self_time(), Duration::ZERO);
    }

    #[test]
    fn the_rendered_tree_draws_branches_under_the_root() {
        let tree = TimingTree::of(&[
            record(&["orb", "parse"], 1, 3),
            record(&["orb", "lower"], 5, 10),
            record(&["orb"], 0, 20),
        ]);
        let text = tree.to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].starts_with("orb "), "{text}");
        assert!(lines[2].starts_with("├─ parse"), "{text}");
        assert!(lines[3].starts_with("└─ lower"), "{text}");
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn durations_render_at_the_scale_they_are_read() {
        assert_eq!(format_duration(Duration::from_micros(420)), "0.42ms");
        assert_eq!(format_duration(Duration::from_micros(41_300)), "41.3ms");
        assert_eq!(format_duration(Duration::from_millis(1_420)), "1.42s");
    }

    #[test]
    fn recorded_spans_carry_their_path_fields_and_outcome_into_the_chrome_trace() {
        let timings = Timings::new();
        let subscriber = tracing_subscriber::registry().with(timings.layer());
        tracing::subscriber::with_default(subscriber, || {
            let root = tracing::info_span!(target: "almadar:cli", "orb", command = "validate");
            let _root = root.enter();
            let phase = tracing::debug_span!(target: "almadar:cli", "lower", outcome = tracing::field::Empty);
            let result: Result<(), ()> = Err(());
            record_outcome(&phase, &result);
            phase.in_scope(|| ());
        });
        let records = timings.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, vec!["orb", "lower"]);
        assert_eq!(records[0].fields.get(OUTCOME), Some(&serde_json::json!("failed")));
        let trace = timings.chrome_trace();
        let events = trace["traceEvents"].as_array().expect("events");
        assert_eq!(events[1]["name"], "orb");
        assert_eq!(events[1]["ph"], "X");
        assert_eq!(events[1]["args"]["command"], "validate");
    }

    #[test]
    fn json_lines_follow_the_structured_log_entry_shape() {
        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'w> MakeWriter<'w> for Buffer {
            type Writer = Buffer;
            fn make_writer(&'w self) -> Buffer {
                self.clone()
            }
        }
        let buffer = Buffer::default();
        let config = LogConfig::parse(Some("info"), None, None);
        let layer = event_layer(LogFormat::Json, config, ConsoleOptions { ansi: false, uptime: false, spans: true }, buffer.clone());
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
            tracing::info_span!(target: "almadar:cli", "lower", file = "app.lolo").in_scope(|| {
                tracing::warn!(target: "almadar:cli", count = 2, "two orbitals");
            });
            tracing::debug!(target: "almadar:cli", "below the floor");
        });
        let text = String::from_utf8(buffer.0.lock().clone()).expect("utf8");
        let lines: Vec<serde_json::Value> =
            text.lines().map(|l| serde_json::from_str(l).expect("json line")).collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0]["severity"], "WARNING");
        assert_eq!(lines[0]["namespace"], "almadar:cli");
        assert_eq!(lines[0]["message"], "two orbitals");
        assert_eq!(lines[0]["data"]["count"], 2);
        assert_eq!(lines[1]["message"], "[PROFILE] lower");
        assert_eq!(lines[1]["data"]["file"], "app.lolo");
        assert!(lines[1]["data"]["elapsedMs"].is_number());
    }
}
}
