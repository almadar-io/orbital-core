//! Structured, namespaced logging — the Rust mirror of `@almadar/logger`.
//!
//! Same contract as the TypeScript stack so the Rust shell's logs read the same:
//!
//! - **Namespaces** are `:`-delimited and hierarchical, e.g. `almadar:runtime:fetch`,
//!   `almadar:server:data`, `almadar:bridge`, `almadar:ui:data-grid`. In Rust they are
//!   the tracing event `target`. Use the [`NS`] constants or pass `target:` directly.
//! - **Levels**: `error`/`warn`/`info`/`debug` (`tracing` macros).
//! - **Structured fields**: `debug!(target: NS::RUNTIME_FETCH, entity = %e, count = n, "fetch:list")`.
//! - **Enable/disable** via the `ALMADAR_DEBUG` env var — an ALLOWLIST of namespace patterns,
//!   comma-separated, each either exact (`almadar:runtime:fetch`), a prefix wildcard
//!   (`almadar:runtime:*`), or `*` (everything). `WARN`/`ERROR` ALWAYS fire regardless of
//!   the allowlist (only `DEBUG`/`INFO` are namespace-gated), matching `@almadar/logger`.
//!   `LOG_LEVEL` (default `warn`) sets the global floor for warn/error.
//!
//! Example: `ALMADAR_DEBUG='almadar:runtime:*,almadar:bridge' orb verify <app> --screenshot …`

use std::sync::Once;
use tracing::{Level, Metadata};
use tracing_subscriber::layer::{Context, Filter, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

/// Canonical namespace constants (mirror the TS `createLogger('…')` namespaces).
pub mod ns {
    pub const RUNTIME_KERNEL: &str = "almadar:runtime:kernel";
    pub const RUNTIME_FETCH: &str = "almadar:runtime:fetch";
    pub const RUNTIME_EFFECTS: &str = "almadar:runtime:effects";
    pub const RUNTIME_PERSIST: &str = "almadar:runtime:persist";
    pub const RUNTIME_SM: &str = "almadar:runtime:sm";
    pub const SERVER_DATA: &str = "almadar:server:data";
    pub const BRIDGE: &str = "almadar:bridge";
    pub const SEED: &str = "almadar:runtime:seed";
    pub const UI_DATA_GRID: &str = "almadar:ui:data-grid";
}

/// One parsed `ALMADAR_DEBUG` allowlist entry.
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
}

/// The `@almadar/logger` allowlist filter: `WARN`/`ERROR` always pass the floor; `INFO`/
/// `DEBUG`/`TRACE` pass only when their `target` namespace is in `ALMADAR_DEBUG`.
struct AlmadarFilter {
    floor: Level,
    allow: Vec<Pattern>,
}

impl AlmadarFilter {
    fn from_env() -> Self {
        let floor = std::env::var("LOG_LEVEL")
            .ok()
            .and_then(|s| match s.to_ascii_lowercase().as_str() {
                "error" => Some(Level::ERROR),
                "warn" => Some(Level::WARN),
                "info" => Some(Level::INFO),
                "debug" => Some(Level::DEBUG),
                "trace" => Some(Level::TRACE),
                _ => None,
            })
            .unwrap_or(Level::WARN);
        let allow = std::env::var("ALMADAR_DEBUG")
            .unwrap_or_default()
            .split(',')
            .filter_map(Pattern::parse)
            .collect();
        AlmadarFilter { floor, allow }
    }

    fn namespace_allowed(&self, target: &str) -> bool {
        self.allow.iter().any(|p| p.matches(target))
    }

    fn allows(&self, meta: &Metadata<'_>) -> bool {
        let level = *meta.level();
        // tracing Ord: ERROR < WARN < INFO < DEBUG < TRACE (more-severe is "less").
        if level <= Level::WARN {
            // warn/error: gated only by the global floor, never the namespace allowlist.
            level <= self.floor
        } else {
            // info/debug/trace: the namespace allowlist IS the enable signal — it bypasses
            // the warn floor (otherwise enabling `ALMADAR_DEBUG` would still show nothing).
            self.namespace_allowed(meta.target())
        }
    }
}

impl<S> Filter<S> for AlmadarFilter {
    fn enabled(&self, meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        self.allows(meta)
    }
}

static INIT: Once = Once::new();

/// Install the namespaced tracing subscriber (idempotent; safe to call from every binary
/// entry point). Reads `ALMADAR_DEBUG` + `LOG_LEVEL`. No-op if a subscriber is already set.
pub fn init() {
    INIT.call_once(|| {
        // stdout is the data channel (`--json`); logs never share it.
        let fmt = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(true)
            .with_level(true)
            .compact();
        let _ = tracing_subscriber::registry()
            .with(fmt.with_filter(AlmadarFilter::from_env()))
            .try_init();
    });
}
