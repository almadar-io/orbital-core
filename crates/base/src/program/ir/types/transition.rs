//! Transitions and what they carry: guards, ticks, listeners and emits.

use super::*;

/// Transition definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirTransition {
    /// Source state(s) - can be single, multiple, or "*" for any
    pub from: OirTransitionFrom,
    /// Target state
    pub to: String,
    /// Triggering event
    pub event: String,
    /// Guard condition (S-expression)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<crate::ir::expr::OirExpr>,
    /// Effects to execute (S-expressions)
    #[serde(default)]
    pub effects: Vec<crate::ir::expr::OirExpr>,
    /// Predicted renders this transition's server effects lead to (awaiting-server
    /// skeletons). Computed by orbital-compiler from the resolved schema; never authored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub await_render: Vec<AwaitRender>,
}

/// One `await_render` prediction keyed by the dispatch that leads to it: while the server leg of
/// `request_trait` taking `event` from one of `from` (any state when `None`) is in flight, `trait`'s
/// empty `slot` shows `skeleton`. The table a native shell embeds (the web reads the same
/// predictions off each transition).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwaitRenderRow {
    pub request_trait: String,
    pub event: String,
    pub from: Option<Vec<String>>,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub slot: String,
    pub skeleton: SkeletonNode,
}

/// The same rows read off a resolved schema (`orb resolve` output, whose transitions carry the
/// compiler's `awaitRender`), for hosts that run on the schema rather than OIR (the desktop
/// clients). A transition without predictions contributes nothing; an unreadable prediction is
/// skipped with a warning, never guessed.
pub fn await_render_rows_from_resolved(resolved: &serde_json::Value) -> Vec<AwaitRenderRow> {
    let mut rows = Vec::new();
    let orbitals = resolved
        .get("orbitals")
        .and_then(serde_json::Value::as_array);
    for trait_value in orbitals
        .into_iter()
        .flatten()
        .filter_map(|o| o.get("traits")?.as_array())
        .flatten()
    {
        let Some(name) = trait_value.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let transitions = trait_value
            .get("stateMachine")
            .and_then(|sm| sm.get("transitions"))
            .and_then(serde_json::Value::as_array);
        for transition in transitions.into_iter().flatten() {
            let Some(renders) = transition.get("awaitRender") else {
                continue;
            };
            let renders: Vec<AwaitRender> = match serde_json::from_value(renders.clone()) {
                Ok(renders) => renders,
                Err(error) => {
                    tracing::warn!(trait_name = %name, %error, "unreadable awaitRender skipped");
                    continue;
                }
            };
            let from = match transition.get("from") {
                Some(serde_json::Value::String(state)) if state != "*" => Some(vec![state.clone()]),
                Some(serde_json::Value::Array(states)) => Some(
                    states
                        .iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect(),
                ),
                _ => None,
            };
            let event = transition
                .get("event")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            for r in renders {
                rows.push(AwaitRenderRow {
                    request_trait: name.to_string(),
                    event: event.to_string(),
                    from: from.clone(),
                    trait_name: r.trait_name,
                    slot: r.slot,
                    skeleton: r.skeleton,
                });
            }
        }
    }
    rows
}

/// The skeleton shapes every shell can draw (`@almadar/core` `SKELETON_VARIANTS`).
pub const SKELETON_VARIANTS: [&str; 9] = [
    "header", "table", "list", "grid", "detail", "stats", "form", "card", "text",
];

/// A sized skeleton shape (`@almadar/core` `SkeletonShape`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkeletonShape {
    pub variant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<u32>,
}

/// A predicted skeleton: one shape, or a vertical stack (`@almadar/core` `SkeletonNode`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkeletonNode {
    Shape { shape: SkeletonShape },
    Stack { stack: Vec<SkeletonNode> },
}

/// One predicted render: `trait` will render into `slot` something shaped like `skeleton`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AwaitRender {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub slot: String,
    pub skeleton: SkeletonNode,
}

impl OirTransition {
    /// Create a simple transition
    pub fn new(from: impl Into<String>, to: impl Into<String>, event: impl Into<String>) -> Self {
        Self {
            from: OirTransitionFrom::Single(from.into()),
            to: to.into(),
            event: event.into(),
            guard: None,
            effects: vec![],
            await_render: vec![],
        }
    }

    /// Create a transition from any state
    pub fn from_any(to: impl Into<String>, event: impl Into<String>) -> Self {
        Self {
            from: OirTransitionFrom::Any,
            to: to.into(),
            event: event.into(),
            guard: None,
            effects: vec![],
            await_render: vec![],
        }
    }

    /// Add a guard
    pub fn with_guard(mut self, guard: crate::ir::expr::OirExpr) -> Self {
        self.guard = Some(guard);
        self
    }

    /// Add effects
    pub fn with_effects(mut self, effects: Vec<crate::ir::expr::OirExpr>) -> Self {
        self.effects = effects;
        self
    }
}

/// Transition source
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OirTransitionFrom {
    Single(String),
    Multiple(Vec<String>),
    #[serde(rename = "*")]
    Any,
}

impl OirTransitionFrom {
    /// Check if this matches a given state
    pub fn matches(&self, state: &str) -> bool {
        match self {
            Self::Single(s) => s == state,
            Self::Multiple(states) => states.iter().any(|s| s == state),
            Self::Any => true,
        }
    }
}

impl std::fmt::Display for OirTransitionFrom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(s) => write!(f, "{}", s),
            Self::Multiple(states) => write!(f, "[{}]", states.join(", ")),
            Self::Any => write!(f, "*"),
        }
    }
}

/// Named guard definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirGuard {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub expression: crate::ir::expr::OirExpr,
}

impl OirGuard {
    /// Create a new guard
    pub fn new(name: impl Into<String>, expression: crate::ir::expr::OirExpr) -> Self {
        Self {
            name: name.into(),
            description: None,
            expression,
        }
    }
}

/// Tick definition (scheduled behavior)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirTick {
    pub name: String,
    /// Interval specification
    pub interval: OirInterval,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<crate::ir::expr::OirExpr>,
    #[serde(default)]
    pub effects: Vec<crate::ir::expr::OirExpr>,
    #[serde(default)]
    pub priority: i32,
    /// `[background]`: runs with no client mounted.
    #[serde(
        rename = "runsInBackground",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub background: bool,
    /// `[visible]`: a browser tick that pauses while its trait's rendered output is off screen.
    #[serde(
        rename = "whileVisible",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub while_visible: bool,
    /// The trait states the tick runs in; empty = every state.
    #[serde(rename = "appliesTo", default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to: Vec<String>,
}

impl OirTick {
    /// Create a new tick
    pub fn new(name: impl Into<String>, interval: OirInterval) -> Self {
        Self {
            name: name.into(),
            interval,
            guard: None,
            effects: vec![],
            priority: 0,
            background: false,
            while_visible: false,
            applies_to: vec![],
        }
    }
}

/// Interval specification
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OirInterval {
    Milliseconds {
        value: u64,
    },
    Seconds {
        value: u64,
    },
    Minutes {
        value: u64,
    },
    /// Every frame (games)
    Frame,
    /// A validated 5-field cron expression (e.g. `"0 9 * * *"`), stored as
    /// the raw source string — see `crate::cron` for parsing/matching.
    /// `resolve_tick` only produces this variant after confirming the
    /// string is a syntactically valid cron expression.
    Cron { expression: String },
}

impl OirInterval {
    /// Convert to milliseconds. `None` for `Frame` (no fixed interval) and
    /// `Cron` (needs calendar matching, not a fixed delay — see `crate::cron`).
    pub fn to_millis(&self) -> Option<u64> {
        match self {
            Self::Milliseconds { value } => Some(*value),
            Self::Seconds { value } => Some(*value * 1000),
            Self::Minutes { value } => Some(*value * 60 * 1000),
            Self::Frame => None,
            Self::Cron { .. } => None,
        }
    }
}

/// Listener definition (cross-trait events)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirListener {
    /// BARE event name (no source prefix). The emitter fires this exact key
    /// on the bus; source filtering happens via `source_trait` / `source_orbital`.
    pub event_pattern: String,
    /// Trait name of the expected emitter. When present, the generated hook
    /// filters incoming events to those whose `source.trait` matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_trait: Option<String>,
    /// V4 dual-carry id sibling of `source_trait` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_trait_id: Option<String>,
    /// Orbital name of the expected emitter. When present (alongside
    /// `source_trait`), the generated hook additionally filters by
    /// `source.orbital` for fully-qualified cross-orbital listens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_orbital: Option<String>,
    /// V4 dual-carry id sibling of `source_orbital` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_orbital_id: Option<String>,
    /// `true` when the author wrote `* EVENT -> TRIGGER` (accept any source).
    /// Mutually exclusive with the source fields above.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_any: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<crate::ir::expr::OirExpr>,
    /// `with { field: expr }` payload projection. Evaluated payload-only at
    /// delivery (mirrors `runtime/listener.rs`); the mapped payload REPLACES
    /// the source payload. `None` delivers the source payload unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_mapping: Option<crate::ir::expr::OirFields>,
    /// Event to trigger internally
    pub triggers: String,
}

impl OirListener {
    /// Create a new listener with no source scoping (accepts any source).
    pub fn new(event_pattern: impl Into<String>, triggers: impl Into<String>) -> Self {
        Self {
            event_pattern: event_pattern.into(),
            source_trait: None,
            source_trait_id: None,
            source_orbital: None,
            source_orbital_id: None,
            source_any: false,
            guard: None,
            payload_mapping: None,
            triggers: triggers.into(),
        }
    }
}

/// Event emission definition - declares events this trait can emit to other traits
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirEmit {
    /// Event name
    pub event: String,
    /// V4 dual-carry id sibling of `event` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// Description of when/why this event is emitted
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Scope: internal (same trait) or external (cross-trait)
    #[serde(default)]
    pub scope: OirEventScope,
    /// Payload schema for this event
    #[serde(default)]
    pub payload: Vec<OirPayloadField>,
}

impl OirEmit {
    /// Create a new emit definition
    pub fn new(event: impl Into<String>) -> Self {
        Self {
            event: event.into(),
            event_id: None,
            description: None,
            scope: OirEventScope::External,
            payload: vec![],
        }
    }
}

/// Event scope
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OirEventScope {
    /// Internal to this trait
    Internal,
    /// Can be received by other traits
    #[default]
    External,
}

/// Payload field definition for emitted events
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirPayloadField {
    pub name: String,
    pub field_type: OirType,
    #[serde(default)]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
