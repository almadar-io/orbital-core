//! What running an effect returns: persist operations, results and outcomes.

use super::*;

/// Persistence operation type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersistOperation {
    Create,
    Upsert,
    Read,
    Update,
    Delete,
}

impl PersistOperation {
    /// Parse from string
    #[allow(clippy::should_implement_trait)] // returns Option, not the Result FromStr requires
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "create" => Some(PersistOperation::Create),
            "upsert" => Some(PersistOperation::Upsert),
            "read" => Some(PersistOperation::Read),
            "update" => Some(PersistOperation::Update),
            "delete" => Some(PersistOperation::Delete),
            _ => None,
        }
    }
}

/// Log level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Result of executing an effect
#[derive(Debug, Clone)]
pub struct EffectResult {
    /// The effect that was processed
    pub effect: Effect,

    /// How the effect was handled
    pub outcome: EffectOutcome,

    /// Return value (if any)
    pub return_value: Option<Value>,

    /// Error message (if failed)
    pub error: Option<String>,

    /// Entity type (for fetch effects, used to track which entity type was fetched)
    ///
    /// This is populated when executing Fetch effects, allowing the caller
    /// to easily identify which entity type the returned data belongs to.
    pub entity_type: Option<String>,

    /// A fetch's row count before offset/limit (the success payload's `totalCount`).
    pub total_count: Option<usize>,

    /// This `emit` is a `persist` effect's `emit.success` envelope: the one emit a live-push host also
    /// fans out to every other connected client.
    pub persist_success: bool,

    /// This `emit` is the reply or failure an effect's declared `emit` route fires on completion (`http`, `os`,
    /// `persist`, `fetch`, `random-bytes`, `credential-*`, `oauth-*`, a keyed `call-service`, ...): it carries that
    /// completion's identity, so the dispatch loop never collapses it into an earlier identical delivery
    /// (EXEC-QUEUE-4).
    pub completion: bool,

    /// Why a failed persist resolved no target row (`ServerEffectResult.code`).
    pub code: Option<crate::bus_event::ServerEffectCode>,

    /// For an `emit`: its cross-unit deliveries that were refused (EXEC-QUEUE-9); each one is a `DELIVERY_REJECTED`.
    pub refused: Vec<crate::envelope::DeliveryRefused>,

    /// For a failed `persist batch`: the operation at fault and how many ran before it.
    pub batch_failure: Option<BatchFailure>,
}

/// Where a `persist batch` stopped: the position of the failing operation in the batch and the number of
/// operations that completed before it (a store that runs the batch atomically has rolled those back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchFailure {
    pub index: usize,
    pub completed_count: usize,
}

impl EffectResult {
    /// Create a successful execution result
    pub fn executed(effect: Effect) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Executed,
            return_value: None,
            error: None,
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Create a successful execution with return value
    pub fn executed_with_value(effect: Effect, value: Value) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Executed,
            return_value: Some(value),
            error: None,
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Create a successful fetch execution with entity type tracking
    ///
    /// This is used for Fetch effects to track which entity type was fetched.
    pub fn executed_fetch(
        effect: Effect,
        entities: Value,
        entity_type: &str,
        total: Option<usize>,
    ) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Executed,
            return_value: Some(entities),
            error: None,
            entity_type: Some(entity_type.to_string()),
            total_count: total,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// A fetch delivered by reference (`deliver: ids`): the success payload's `data` is the reference's
    /// render-prop leaf, `totalCount` its count.
    pub fn executed_row_set(
        effect: Effect,
        reference: &crate::row_set::RowSetRef,
        total: Option<usize>,
    ) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Executed,
            return_value: Some(crate::row_set::row_set_marker(reference)),
            error: None,
            entity_type: Some(reference.entity_type.clone()),
            total_count: total,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Create a no-op result (effect skipped for this environment)
    pub fn noop(effect: Effect, reason: &str) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::NoOp(reason.to_string()),
            return_value: None,
            error: None,
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Create a delegated result (effect sent to another environment)
    pub fn delegated(effect: Effect, target: RuntimeEnvironment) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Delegated(target),
            return_value: None,
            error: None,
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// This result, marked as a `persist` effect's success envelope.
    /// Mark this `emit` as an effect's completion delivery.
    pub fn as_completion(mut self) -> Self {
        self.completion = true;
        self
    }

    /// This result, its effect carrying the completion events the declared effect routes to: an executor that
    /// rebuilds the effect it ran (a `fetch`'s resolved query) drops them, and [`Self::routed_failure`] reads them.
    pub fn with_declared_routes(mut self, emit: &FetchEmit) -> Self {
        if let Some(slot) = self.effect.emit_slot() {
            *slot = Some(emit.clone());
        }
        self
    }

    pub fn as_persist_success(mut self) -> Self {
        self.persist_success = true;
        self
    }

    /// The declared `emit.failure` event this effect's failure was routed to; `None` for a success or an effect
    /// that declares no failure route. The kernel fires that event, so the failure is a handled outcome.
    pub fn routed_failure(&self) -> Option<String> {
        match self.outcome {
            EffectOutcome::Failed => self.effect.emit_routes()?.failure.clone(),
            _ => None,
        }
    }

    /// This failed `persist batch`, with the operation that failed.
    pub fn with_batch_failure(mut self, index: usize, completed_count: usize) -> Self {
        self.batch_failure = Some(BatchFailure { index, completed_count });
        self
    }

    /// This failed result, with the coded reason its persist resolved no target row.
    pub fn with_code(mut self, code: crate::bus_event::ServerEffectCode) -> Self {
        self.code = Some(code);
        self
    }

    /// Create a failed result
    pub fn failed(effect: Effect, error: &str) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Failed,
            return_value: None,
            error: Some(error.to_string()),
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Check if the effect was executed
    pub fn is_executed(&self) -> bool {
        matches!(self.outcome, EffectOutcome::Executed)
    }

    /// Check if the effect was a no-op
    pub fn is_noop(&self) -> bool {
        matches!(self.outcome, EffectOutcome::NoOp(_))
    }

    /// A `runsOn: client` host-port effect a client dispatch hands to its client host (`ClientEffect::HostAsk`).
    pub fn host_asked(effect: Effect) -> Self {
        Self {
            outcome: EffectOutcome::HostAsked,
            ..Self::executed(effect)
        }
    }

    /// Check if the effect was handed to the client host as an ask
    pub fn is_host_asked(&self) -> bool {
        matches!(self.outcome, EffectOutcome::HostAsked)
    }

    /// Check if the effect was delegated
    pub fn is_delegated(&self) -> bool {
        matches!(self.outcome, EffectOutcome::Delegated(_))
    }

    /// Check if the effect failed
    pub fn is_failed(&self) -> bool {
        matches!(self.outcome, EffectOutcome::Failed)
    }
}

/// How an effect was handled
#[derive(Debug, Clone)]
pub enum EffectOutcome {
    /// Effect was executed in this environment
    Executed,

    /// Effect was skipped (not applicable to this environment)
    NoOp(String),

    /// Effect was delegated to another environment
    Delegated(RuntimeEnvironment),

    /// A `runsOn: client` host-port effect the client dispatch hands to the client host as a `host-ask` client effect
    HostAsked,

    /// Effect execution failed
    Failed,
}
