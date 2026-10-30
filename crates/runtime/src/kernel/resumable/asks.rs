//! What a dispatch asks the host and how the host answers: requests, routes, call sites and answers.

use super::*;

/// Names one request of one dispatch; an answer carries the id of the request it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub(super) u64);

impl RequestId {
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

/// The I/O the kernel needs the host to do. The kernel applies row policy, filters and paging itself
/// around these, so a host answers plain storage reads and writes. A credential value is a [`Secret`], which never
/// prints (an `HttpRequest` redacts its own credentials).
#[derive(Clone, PartialEq, Debug)]
pub enum IoRequest {
    ReadRow {
        entity_type: String,
        id: String,
    },
    ListRows {
        entity_type: String,
    },
    /// A query the host runs where the data lives: only the page (or its ids) crosses. Sent only to a host
    /// that declared capabilities covering `query`.
    QueryRows {
        entity_type: String,
        query: RowQuery,
        project: RowProjection,
    },
    /// Conditional writes the host runs in one step: `batch.on_failure` is `atomic` (a `persist batch`) or
    /// `stop` (a run of plain persists). Sent only to a host that declared write capabilities covering it.
    WriteRows {
        batch: RowWriteBatch,
    },
    /// Rows the kernel decided, committed together only if every one still equals the version read
    /// (EXEC-STORE-5). Sent only to a host that declared `commitRows`.
    CommitRows {
        rows: Vec<RowCommit>,
    },
    CreateRow {
        entity_type: String,
        data: Value,
    },
    WriteRow {
        entity_type: String,
        id: String,
        data: Value,
    },
    DeleteRow {
        entity_type: String,
        id: String,
    },
    CallService {
        service: String,
        method: String,
        args: Value,
        key: Option<String>,
        caller: Option<ServiceCaller>,
        /// Where the call is made from; `None` for a call outside a transition's effects.
        site: Option<CallSite>,
    },
    CancelCall {
        key: String,
    },
    /// One substrate or program op. A keyed op with a declared route sets `key` and `site`: the host may then answer
    /// [`IoAnswer::ServiceDeferred`] and deliver its outcome later as a dispatch of its own (R-a).
    Substrate {
        op: String,
        args: Vec<Value>,
        key: Option<String>,
        site: Option<CallSite>,
    },
    /// The page facts a `browser/page-facts` asks for, in read order.
    PageFacts {
        fields: Vec<PageFactField>,
    },
    /// A `browser/prompt`: the on-device model answers with JSON text satisfying the JSON Schema text `schema`.
    /// `key` lets `cancel-call` abort the ask in flight.
    Prompt {
        text: String,
        schema: String,
        key: Option<String>,
    },
    /// A `browser/embed`: the on-device embedder answers one vector per text, in order.
    Embed {
        texts: Vec<String>,
        key: Option<String>,
    },
    /// One raw ask of a `browser/tab-*` effect: the host performs a tab or page operation and answers its JSON
    /// reply. `key` lets `cancel-call` abort the ask in flight.
    Tab {
        ask: TabAsk,
        key: Option<String>,
    },
    /// A credential by name. `principal` is the dispatch's `@user` id for a user-scoped credential
    /// (set by the kernel, never by the program); `None` is the app-wide credential.
    Secret {
        name: String,
        principal: Option<String>,
    },
    /// Keep a derived secret in the host's store.
    SecretSet {
        name: String,
        value: Secret<String>,
        ttl_ms: u64,
        principal: Option<String>,
    },
    /// Replace the credential `name` the host holds with `value`, durably (a rotated refresh token).
    SecretRotate {
        name: String,
        value: Secret<String>,
        principal: Option<String>,
    },
    /// `bytes` of CSPRNG output, journaled so a replay sees the same bytes.
    Random {
        bytes: u32,
    },
    /// The dispatch's wall clock in epoch milliseconds, journaled so a replay sees the same instant.
    Clock,
    /// A cryptographic operator that draws entropy of its own (`rsa/sign` blinding, the `ecdh/p256-ephemeral` scalar).
    /// The kernel answers it itself and never sends it to a host: the entropy is drawn fresh and is never journaled.
    /// `args` are the operator's operands (a credential name, public values), never a secret value; the answer is the
    /// operator's output, which a replay returns without running the operator again.
    Sealed {
        op: String,
        args: Vec<Value>,
    },
    /// One `os/<op>` effect. A streaming op sets `site`: the host dispatches each event to its route, then
    /// answers [`IoAnswer::Os`] with the final reply. `key` lets `cancel-call` abort the ask in flight.
    Os {
        op: OsOp,
        args: Vec<Value>,
        key: Option<String>,
        site: Option<CallSite>,
        /// The signed-in viewer's id, set by the kernel from the dispatch's `@user`, for a `scope: "user"` credential segment.
        principal: Option<String>,
    },
    /// Whether the host holds a value for the credential `name` (for `principal` when set).
    CredentialStatus {
        name: String,
        principal: Option<String>,
    },
    /// Drop the credential `name` the host holds (for `principal` when set).
    CredentialForget {
        name: String,
        principal: Option<String>,
    },
    /// Exchange an OAuth code at the grant's token endpoint and keep the result under its credential name. The code and
    /// verifier are [`Secret`]s, so the journaled ask never shows them.
    CredentialConnect {
        grant: Box<crate::credential_admin::CredentialGrant>,
    },
    /// The credential store's state: whether it can encrypt (a master key is present) and the service and name of every
    /// stored row, never a value. Answered `None` by a host that holds no store.
    CredentialStoreList,
    /// The last four characters of each of `names` the host's own environment holds, never a value.
    CredentialEnvTails {
        names: Vec<String>,
    },
    /// The stored value of credential `name`, from the store only (no environment fallthrough).
    CredentialStoreRead {
        name: String,
    },
    /// Encrypt `value` into the store as credential `name` of `service`, replacing any row of that name.
    CredentialStoreWrite {
        service: String,
        name: String,
        value: Secret<String>,
    },
    /// Delete the stored row of credential `name`; whether one existed.
    CredentialStoreRemove {
        name: String,
    },
    /// Re-encrypt every stored row under the current master key.
    CredentialStoreRotate,
    /// The host's integrations mode flag; answered `None` by a host that keeps none.
    CredentialModeRead,
    /// Switch the host's integrations mode flag to `mock` or `real`; the host may refuse.
    CredentialModeWrite {
        mode: String,
    },
    /// An `emit` whose declared listener runs in another deployment unit (EXEC-QUEUE-9). `site` is the emitting
    /// trait's call site; `key` lets `cancel-call` withdraw the ask.
    Deliver {
        envelope: Envelope,
        key: Option<String>,
        site: CallSite,
    },
}

/// The events a `call-service` call site routes its outcome and live messages to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallRoutes {
    pub success: Option<String>,
    pub failure: Option<String>,
    pub cancelled: Option<String>,
    pub on_message: Option<String>,
}

/// The call site of a `call-service`: the trait whose transition runs it (and that trait's orbital),
/// the routes it declares, and whether the step is a follow-on of the dispatch (a listener's arm, or
/// the calling trait's own next step) rather than the request's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub orbital: Option<String>,
    pub trait_name: String,
    pub follow_on: bool,
    /// The frame key the call was made on ([`crate::process_result::RUNTIME_SINGLETON`] for an id-less dispatch): a
    /// deferred outcome is dispatched to it. A host names it to a viewer through [`crate::process_result::public_entity_id`].
    pub entity_id: String,
    pub emit: CallRoutes,
}

/// The event and payload `outcome` fires on `site`'s routes.
pub fn substrate_completion_event(site: &CallSite, key: &str, outcome: Result<Value, Value>) -> Option<(String, Value)> {
    let (event, payload) = match outcome {
        Ok(payload) => (site.emit.success.clone()?, payload),
        Err(payload) => (site.emit.failure.clone()?, payload),
    };
    let payload = match payload {
        Value::Object(mut fields) => {
            fields.insert("key".into(), Value::string(key.to_string()));
            Value::Object(fields)
        }
        other => other,
    };
    Some((event, payload))
}

pub fn completion_event(
    site: &CallSite,
    key: &str,
    outcome: Result<Value, OsError>,
) -> Option<(String, Value)> {
    let key = Value::string(key.to_string());
    match outcome {
        Ok(result) => {
            let mut fields = match result {
                Value::Object(fields) => fields,
                other => ValueMap::from_iter([("value".into(), other)]).into(),
            };
            fields.insert("key".into(), key);
            Some((site.emit.success.clone()?, Value::Object(fields)))
        }
        Err(error) => Some((
            site.emit.failure.clone()?,
            Value::object(ValueMap::from_iter([
                ("error".into(), Value::String(error.message.into())),
                (
                    "kind".into(),
                    Value::String(error.kind.as_str().to_string().into()),
                ),
                ("key".into(), key),
            ])),
        )),
    }
}

/// How a host settled a `call-service` other than by a result or an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallVerdict {
    /// The call was aborted by `cancel-call`: the cancelled route fires with `{ key }`.
    Cancelled,
    /// The host started the call and runs its outcome later as its own dispatch: nothing fires now.
    Deferred,
}

/// A host's answer to one [`IoRequest`]; the variant must be the one the request calls for. A credential value
/// (`Secret`, `CredentialStoreRead`) and the entropy of `Random` are [`Secret`]s, which never print.
#[derive(Clone, PartialEq, Debug)]
pub enum IoAnswer {
    Row(Result<Value, String>),
    Rows(Result<Vec<Arc<Value>>, String>),
    Page(Result<RowPage, String>),
    IdPage(Result<RowIdPage, String>),
    /// The host cannot run the query it was sent; nothing was read. The kernel lists the table itself.
    QueryUnsupported(String),
    Wrote(Result<Vec<RowWriteOutcome>, String>),
    /// The host cannot run the writes it was sent; nothing was written. The kernel takes the one-row path.
    WriteUnsupported(String),
    /// `true` when every row was committed; `false` on a conflict, when nothing was.
    Committed(Result<bool, String>),
    Deleted(Result<(), String>),
    Service(Result<Value, String>),
    /// The call was aborted by `cancel-call` while it ran.
    ServiceCancelled,
    /// The call was started and its outcome will arrive as a dispatch of its own.
    ServiceDeferred,
    Cancelled(Result<(), String>),
    Substrate(Result<Value, SubstrateFailure>),
    PageFacts(Result<PageFacts, String>),
    Prompt(Result<String, ModelFailure>),
    Embed(Result<Vec<Vec<f64>>, ModelFailure>),
    Tab(Result<Value, TabFailure>),
    Secret(Result<Option<Secret<String>>, String>),
    SecretSet(Result<(), String>),
    SecretRotate(Result<(), String>),
    Random(Result<Secret<Vec<u8>>, String>),
    Clock(Result<i64, String>),
    /// The output of a [`IoRequest::Sealed`] operator.
    Sealed(Result<Value, String>),
    Os(Result<Secret<Value>, OsError>),
    CredentialStatus(Result<crate::credential_admin::CredentialStanding, String>),
    CredentialForget(Result<(), String>),
    CredentialConnect(Result<crate::credential_admin::CredentialStanding, crate::credential_admin::ConnectFailure>),
    CredentialStoreList(Result<Option<StoreListing>, String>),
    CredentialEnvTails(Result<crate::credential_admin::EnvTails, String>),
    CredentialStoreRead(Result<Option<Secret<String>>, String>),
    CredentialStoreWrite(Result<(), String>),
    CredentialStoreRemove(Result<bool, String>),
    CredentialStoreRotate(Result<RotateCounts, String>),
    CredentialModeRead(Result<Option<String>, String>),
    CredentialModeWrite(Result<String, String>),
    Deliver(Result<Accepted, DeliverRefusal>),
}

/// Why the on-device model or embedder gave no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFailureKind {
    /// The host wires no model or embedder.
    Unconfigured,
    /// The host has one that cannot answer now (still downloading, not supported on this device).
    Unavailable,
    /// The model or embedder answered with an error, or with an answer outside the contract.
    Failed,
}

impl ModelFailureKind {
    /// The word a failure payload carries as `kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "unconfigured",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

/// A `browser/prompt` or `browser/embed` the host could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFailure {
    pub kind: ModelFailureKind,
    pub message: String,
}

impl ModelFailure {
    pub fn new(kind: ModelFailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

/// What the page offered: `available` is false when there is no page; the other facts are as read.
#[derive(Debug, Clone, PartialEq)]
pub struct PageFacts {
    pub available: bool,
    pub hostname: String,
    pub referrer: String,
    pub screen_width: Option<f64>,
    pub href: String,
}

impl IoAnswer {
    pub(super) fn answers(&self, request: &IoRequest) -> bool {
        if let (IoRequest::Os { op, key, site, .. }, IoAnswer::ServiceDeferred) = (request, self) {
            return key.is_some() && (site.is_some() || !op.defers_completion());
        }
        if let (IoRequest::Substrate { key, site, .. }, IoAnswer::ServiceDeferred) = (request, self) {
            return key.is_some() && site.is_some();
        }
        matches!(
            (request, self),
            (
                IoRequest::ReadRow { .. }
                    | IoRequest::CreateRow { .. }
                    | IoRequest::WriteRow { .. },
                IoAnswer::Row(_)
            ) | (IoRequest::ListRows { .. }, IoAnswer::Rows(_))
                | (
                    IoRequest::QueryRows {
                        project: RowProjection::Rows,
                        ..
                    },
                    IoAnswer::Page(_) | IoAnswer::QueryUnsupported(_)
                )
                | (
                    IoRequest::QueryRows {
                        project: RowProjection::Ids,
                        ..
                    },
                    IoAnswer::IdPage(_) | IoAnswer::QueryUnsupported(_)
                )
                | (
                    IoRequest::WriteRows { .. },
                    IoAnswer::Wrote(_) | IoAnswer::WriteUnsupported(_)
                )
                | (IoRequest::CommitRows { .. }, IoAnswer::Committed(_))
                | (IoRequest::DeleteRow { .. }, IoAnswer::Deleted(_))
                | (
                    IoRequest::CallService { .. },
                    IoAnswer::Service(_) | IoAnswer::ServiceCancelled | IoAnswer::ServiceDeferred
                )
                | (IoRequest::CancelCall { .. }, IoAnswer::Cancelled(_))
                | (IoRequest::Substrate { .. }, IoAnswer::Substrate(_))
                | (IoRequest::PageFacts { .. }, IoAnswer::PageFacts(_))
                | (IoRequest::Prompt { .. }, IoAnswer::Prompt(_) | IoAnswer::ServiceCancelled)
                | (IoRequest::Embed { .. }, IoAnswer::Embed(_) | IoAnswer::ServiceCancelled)
                | (IoRequest::Tab { .. }, IoAnswer::Tab(_) | IoAnswer::ServiceCancelled)
                | (IoRequest::Secret { .. }, IoAnswer::Secret(_))
                | (IoRequest::SecretSet { .. }, IoAnswer::SecretSet(_))
                | (IoRequest::SecretRotate { .. }, IoAnswer::SecretRotate(_))
                | (IoRequest::Random { .. }, IoAnswer::Random(_))
                | (IoRequest::Clock, IoAnswer::Clock(_))
                | (IoRequest::Sealed { .. }, IoAnswer::Sealed(_))
                | (IoRequest::Os { .. }, IoAnswer::Os(_) | IoAnswer::ServiceCancelled)
                | (IoRequest::CredentialStatus { .. }, IoAnswer::CredentialStatus(_))
                | (IoRequest::CredentialForget { .. }, IoAnswer::CredentialForget(_))
                | (IoRequest::CredentialConnect { .. }, IoAnswer::CredentialConnect(_))
                | (IoRequest::CredentialStoreList, IoAnswer::CredentialStoreList(_))
                | (IoRequest::CredentialEnvTails { .. }, IoAnswer::CredentialEnvTails(_))
                | (IoRequest::CredentialStoreRead { .. }, IoAnswer::CredentialStoreRead(_))
                | (IoRequest::CredentialStoreWrite { .. }, IoAnswer::CredentialStoreWrite(_))
                | (IoRequest::CredentialStoreRemove { .. }, IoAnswer::CredentialStoreRemove(_))
                | (IoRequest::CredentialStoreRotate, IoAnswer::CredentialStoreRotate(_))
                | (IoRequest::CredentialModeRead, IoAnswer::CredentialModeRead(_))
                | (IoRequest::CredentialModeWrite { .. }, IoAnswer::CredentialModeWrite(_))
                | (IoRequest::Deliver { .. }, IoAnswer::Deliver(_))
        )
    }
}
