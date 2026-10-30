//! State machines, transitions and ticks.

use super::*;

// =============================================================================
// State Machine
// =============================================================================

/// State machine definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateMachine {
    /// States in the machine
    pub states: Vec<StateDefinition>,

    /// Events (for documentation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<EventDefinition>>,

    /// Transitions
    pub transitions: Vec<TransitionDefinition>,
}

impl StateMachine {
    /// Every proposition the machine states: each state's `@holds` and each transition's `@ensures`.
    pub fn propositions_mut(&mut self) -> impl Iterator<Item = &mut SExpression> {
        let StateMachine { states, transitions, .. } = self;
        states
            .iter_mut()
            .filter_map(|s| s.holds.as_mut())
            .chain(transitions.iter_mut().flat_map(|t| t.ensures.iter_mut().flatten()))
    }
}

/// State definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateDefinition {
    /// State name
    pub name: String,

    /// Whether this is the initial state
    #[serde(rename = "isInitial", skip_serializing_if = "Option::is_none")]
    pub is_initial: Option<bool>,

    /// Whether this is a terminal state (no outgoing transitions expected)
    #[serde(rename = "isTerminal", skip_serializing_if = "Option::is_none")]
    pub is_terminal: Option<bool>,

    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Duration in milliseconds after which a synthesized/explicit TIMEOUT
    /// transition should fire, from `state X for Ns { ... }`. `None` for a
    /// state with no time bound.
    #[serde(rename = "timeBoundMs", skip_serializing_if = "Option::is_none")]
    pub time_bound_ms: Option<u64>,

    /// `@holds <expr>`: the trait's `@entity` satisfies it whenever the trait is in this state, proved by
    /// `orb validate`. No runtime effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holds: Option<SExpression>,
}

/// Event definition (for documentation)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventDefinition {
    /// Event key
    pub key: String,

    /// V4 dual-carry id sibling of `key` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Display name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Semantic description (authored `@description` on the emit/listen), the
    /// per-event analogue of a field/config `@description`. Feeds the matcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// User-vocabulary synonyms (comma-separated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,

    /// Authoring tier (`essential`/`customization`/`advanced`/`internal`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,

    /// Payload schema — declarative type info for the event's payload.
    /// JSON key: `payloadSchema`. Distinct from the runtime payload value
    /// (`@payload.X` bindings, `EventPayload`) which is unrelated.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "payloadSchema"
    )]
    pub payload_schema: Option<Vec<PayloadField>>,

    /// Same marker as `EmitDefinition.payload_entity` — this transition's
    /// bare-event payload was declared `type X = Event <EntityName>` and is
    /// a flat mirror of that entity's fields. See that field's doc for the
    /// full contract (G4, `Almadar_Compiler_Gaps.md` §82).
    #[serde(
        rename = "payloadEntity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub payload_entity: Option<String>,

    /// A declared input (`listens { EVENT -> input { … } }`): a
    /// client outside the program (an API, an agent, an MCP client) may send
    /// this event to the trait. Absent = not an input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<bool>,

    /// Declared in the trait's own `listens { EVENT … }` block (the local form,
    /// external or not): an input the author offers to the program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listened: Option<bool>,

    /// A declared inbound delivery (`listens { EVENT -> inbound "name" answers ANSWER { … } }`): the host
    /// carries the HTTP request at `/api/hooks/<name>` and the kernel delivers it to this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbound: Option<InboundDeclaration>,
}

/// What an inbound event declares: the hook name it receives and the event whose payload answers the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundDeclaration {
    /// The hook name, the `:name` of `/api/hooks/:name`.
    pub name: String,
    /// The event the handler emits to answer; its payload is `{ status, body }`.
    pub answers: String,
    /// The sender check the kernel makes before the handler runs; its result is the payload's `verified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifies: Option<InboundVerification>,
}

/// `verifies "<header>" "<CREDENTIAL>"`: the request header that must equal the named credential. The kernel
/// compares them in constant time; the credential's value never reaches program data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundVerification {
    /// The header name, lower-case.
    pub header: String,
    /// The credential name the host resolves.
    pub credential: String,
}

/// Transition definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransitionDefinition {
    /// Source state(s)
    pub from: StringOrArray,

    /// Target state
    pub to: String,

    /// Triggering event
    pub event: String,

    /// V4 dual-carry id sibling of `event` — optional until the Phase-7 flip.
    #[serde(rename = "eventId", default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,

    /// Guard condition (S-expression)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<SExpression>,

    /// Effects to execute (S-expressions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<SExpression>>,

    /// `@ensures <expr>`: the outcome contract owed at this transition (`@fromEntity` is the row before it), proved
    /// by `orb validate` and judged by `orb verify`. No runtime effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensures: Option<Vec<SExpression>>,

    /// Predicted renders this transition's server effects lead to (awaiting-server skeletons). Computed by
    /// orbital-compiler from the resolved schema; never authored.
    #[serde(rename = "awaitRender", default, skip_serializing_if = "Option::is_none")]
    pub await_render: Option<Vec<crate::ir::types::AwaitRender>>,
}

/// String or array of strings (for `from` field)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StringOrArray {
    Single(String),
    Multiple(Vec<String>),
}

impl std::fmt::Display for StringOrArray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StringOrArray::Single(s) => f.write_str(s),
            StringOrArray::Multiple(arr) => write!(f, "[{}]", arr.join(", ")),
        }
    }
}

impl StringOrArray {
    /// Check if a state matches
    pub fn matches(&self, state: &str) -> bool {
        match self {
            StringOrArray::Single(s) => s == state,
            StringOrArray::Multiple(arr) => arr.iter().any(|s| s == state),
        }
    }

    /// Get all states as a slice
    pub fn states(&self) -> Vec<&str> {
        match self {
            StringOrArray::Single(s) => vec![s.as_str()],
            StringOrArray::Multiple(arr) => arr.iter().map(|s| s.as_str()).collect(),
        }
    }
}

// =============================================================================
// Tick Definition
// =============================================================================

/// Scheduled tick definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickDefinition {
    /// Tick name
    pub name: String,

    /// Interval (milliseconds, "5s", "1m", or cron expression)
    pub interval: StringOrNumber,

    /// Guard condition
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<SExpression>,

    /// Effects to execute
    pub effects: Vec<SExpression>,

    /// The trait states the tick runs in; absent or empty = every state.
    #[serde(rename = "appliesTo", skip_serializing_if = "Option::is_none")]
    pub applies_to: Option<Vec<String>>,

    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// `[background]`: runs with no client mounted; otherwise paused while no client has the trait mounted.
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
}

impl TickDefinition {
    /// This tick's interval as milliseconds when it is a duration: a number, or a string `<digits>[ms|s|m|h]` (a bare
    /// digit string is milliseconds). `None` for a per-frame tick, a cron expression and any other string. The one
    /// duration parser, the Rust twin of `@almadar/runtime`'s `parseDurationString` (`time/duration.ts`).
    pub fn duration_ms(&self) -> Option<u64> {
        match &self.interval {
            StringOrNumber::Number(n) => Some(n.max(0.0) as u64),
            StringOrNumber::String(s) => {
                let s = s.trim();
                let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
                let (digits, unit) = s.split_at(split);
                let value: u64 = digits.parse().ok()?;
                match unit {
                    "ms" | "" => Some(value),
                    "s" => Some(value * 1000),
                    "m" => Some(value * 60 * 1000),
                    "h" => Some(value * 60 * 60 * 1000),
                    _ => None,
                }
            }
        }
    }

    /// Resolve this tick's interval to milliseconds. `None` = per-frame
    /// (`"frame"`/`"every-frame"`), fired on every client frame. A string that is
    /// neither a duration nor a frame keyword (a cron expression is scheduled by
    /// its caller before this is asked) falls back to 1000ms. Shared by
    /// the runtime kernel's tick path and the client tick scheduler.
    pub fn interval_ms(&self) -> Option<u64> {
        if let StringOrNumber::String(s) = &self.interval {
            let s = s.trim();
            if s.eq_ignore_ascii_case("frame") || s.eq_ignore_ascii_case("every-frame") {
                return None;
            }
        }
        Some(self.duration_ms().unwrap_or(1000))
    }
}

/// String or number (for intervals)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StringOrNumber {
    String(String),
    Number(f64),
}
