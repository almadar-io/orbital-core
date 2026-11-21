//! What a dispatch asks the host and how the host answers: requests, routes, call sites and answers.

use super::*;
use orbital_core_base::os_grants::OsGrants;

/// The guest session kernel a guest ask runs in. It is live state in the asking process, so it rides beside the ask and
/// is never serialized: a wire host receives the ask without it and can only refuse it. Two asks naming the same
/// session are equal whatever kernel they carry.
#[derive(Clone, Default)]
pub struct GuestKernel(pub Option<crate::kernel::GuestSession>);

impl std::fmt::Debug for GuestKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "GuestKernel(live)" } else { "GuestKernel(detached)" })
    }
}

impl PartialEq for GuestKernel {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// The work of a `program/open { path }` (EXEC-CONC-7): read the program, then build, register and mount the guest,
/// run once by the host off the asking lane. Like [`GuestKernel`] it is live state that rides beside the ask and is
/// never serialized; two asks are equal whatever work they carry.
pub type GuestOpenFn = Box<dyn FnOnce() -> Result<Value, crate::effects::executor::SubstrateFailure> + Send>;

#[derive(Clone, Default)]
pub struct GuestOpenWork(pub Arc<orbital_core_base::sync::Mutex<Option<GuestOpenFn>>>);

impl GuestOpenWork {
    pub fn of(work: GuestOpenFn) -> Self {
        Self(Arc::new(orbital_core_base::sync::Mutex::new(Some(work))))
    }

    /// Run the work; a second run (a replayed ask) is refused, never repeated.
    pub fn run(&self) -> Result<Value, crate::effects::executor::SubstrateFailure> {
        match self.0.lock().take() {
            Some(work) => work(),
            None => Err(crate::agent_ports::guest_failure(
                orbital_core_base::os::OsErrorKind::Invalid,
                "program/open: this open already ran",
            )),
        }
    }
}

impl std::fmt::Debug for GuestOpenWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GuestOpenWork")
    }
}

impl PartialEq for GuestOpenWork {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

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

/// `program/dispatch { reply: { events, match, timeoutMs } }` into a guest (EXEC-CONC-7): the dispatch answers once the guest
/// emits one of `events` whose payload's `field` equals `value`, the same field of the dispatched payload, or fails after
/// `timeout_ms`. An outcome the guest reaches in a later completion (a sign-in after a key fetch) is waited for this way.
#[cfg_attr(all(test, feature = "ts-export"), derive(ts_rs::TS))]
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestReply {
    pub events: Vec<String>,
    pub field: String,
    pub value: Value,
    pub timeout_ms: u64,
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
    /// A `program/dispatch` into the guest session `guest` that the host runs (EXEC-CONC-7): `event` with `payload` (a copy)
    /// to each of `targets` in the session `kernel`, its asks answered by a host holding only `grants`. Keyed, the host runs
    /// it on a thread of its own and answers [`IoAnswer::ServiceDeferred`], the outcome arriving as the call site's keyed
    /// completion event (EXEC-CONC-3); unkeyed, it answers the outcome in place. With `leg`, the request options of a
    /// split guest's viewer kernel, the guest's host answers `payload` as a served events request instead (EXEC-CONC-7).
    Guest {
        guest: String,
        kernel: GuestKernel,
        targets: Vec<String>,
        event: String,
        payload: Value,
        grants: OsGrants,
        leg: Option<Value>,
        key: Option<String>,
        site: Option<CallSite>,
        /// `options.reply`: the dispatch answers once the guest emits one of these events for the same request.
        reply: Option<GuestReply>,
    },
    /// A `program/open { path }`: the host runs `work` (read, build, register, mount) and answers the open's answer;
    /// keyed with a declared route it runs off the lane and settles through the call site's routes.
    GuestOpen {
        work: GuestOpenWork,
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
        /// The orbital whose trait made the ask: the one whose `osGrants` (plus the app's) the answerer checks.
        orbital: Option<String>,
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

/// The key a keyed host ask carries (EXEC-QUEUE-10): the asking session, the trait and row of its call site, then the
/// program's key. A host keeps one pending completion per key, so two callers' same program key never collide. Each
/// leading segment is length-prefixed, so no segment can be read as another.
pub fn host_call_key(session: Option<&str>, trait_name: &str, row: &str, key: &str) -> String {
    let mut scoped = String::new();
    for part in [session.unwrap_or_default(), trait_name, row] {
        scoped.push_str(&part.len().to_string());
        scoped.push(':');
        scoped.push_str(part);
    }
    scoped.push_str(key);
    scoped
}

/// The program's key a [`host_call_key`] carries. A key the kernel did not scope (one a host made itself) is
/// returned as it stands.
pub fn program_call_key(host_key: &str) -> &str {
    let mut rest = host_key;
    for _ in 0..3 {
        let Some((len, tail)) = rest.split_once(':') else { return host_key };
        let Ok(len) = len.parse::<usize>() else { return host_key };
        let Some(after) = tail.get(len..) else { return host_key };
        rest = after;
    }
    rest
}

/// The session of the step that made a [`host_call_key`] ask: `Some(None)` for the program session, `Some(Some(session))`
/// for a client session, `None` for a key the kernel did not scope (one a host made itself).
pub fn call_key_session(host_key: &str) -> Option<Option<&str>> {
    let (len, tail) = host_key.split_once(':')?;
    let len = len.parse::<usize>().ok()?;
    let session = tail.get(..len)?;
    if program_call_key(host_key) == host_key {
        return None;
    }
    Some((!session.is_empty()).then_some(session))
}

/// The event and payload `outcome` fires on `site`'s routes.
pub fn substrate_completion_event(site: &CallSite, key: &str, outcome: Result<Value, Value>) -> Option<(String, Value)> {
    let (event, payload) = match outcome {
        Ok(payload) => (site.emit.success.clone()?, payload),
        Err(payload) => (site.emit.failure.clone()?, payload),
    };
    let payload = match payload {
        Value::Object(mut fields) => {
            fields.insert("key".into(), Value::string(program_call_key(key).to_string()));
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
    let key = Value::string(program_call_key(key).to_string());
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

/// How a deferred keyed `call-service` settled on its host.
#[derive(Debug, Clone, PartialEq)]
pub enum CallOutcome {
    Answered(Value),
    Failed(String),
    Cancelled,
}

/// The event a deferred keyed `call-service` outcome fires on its call site (EXEC-QUEUE-10), its payload carrying the
/// program's key: the answer (a record gains `key`; any other value is passed as it stands), `{ error, key }`, or
/// `{ key }`. `None` when the site declares no route for the outcome.
pub fn call_completion_event(site: &CallSite, key: Option<&str>, outcome: CallOutcome) -> Option<(String, Value)> {
    let key = key.map(|k| Value::string(program_call_key(k).to_string()));
    let with_key = |payload: Value| match (payload, &key) {
        (Value::Object(mut fields), Some(key)) => {
            fields.insert("key".into(), key.clone());
            Value::Object(fields)
        }
        (other, _) => other,
    };
    match outcome {
        CallOutcome::Answered(value) => Some((site.emit.success.clone()?, with_key(value))),
        CallOutcome::Failed(message) => Some((
            site.emit.failure.clone()?,
            with_key(Value::object(ValueMap::from_iter([("error".into(), Value::String(message.into()))]))),
        )),
        CallOutcome::Cancelled => Some((site.emit.cancelled.clone()?, with_key(Value::object(ValueMap::new())))).filter(|_| key.is_some()),
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
        if let (IoRequest::Guest { key, site, .. }, IoAnswer::ServiceDeferred) = (request, self) {
            return key.is_some() && site.is_some();
        }
        if let (IoRequest::Guest { .. }, IoAnswer::Substrate(_)) = (request, self) {
            return true;
        }
        if let (IoRequest::GuestOpen { key, site, .. }, IoAnswer::ServiceDeferred) = (request, self) {
            return key.is_some() && site.is_some();
        }
        if let (IoRequest::GuestOpen { .. }, IoAnswer::Substrate(_)) = (request, self) {
            return true;
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

#[cfg(test)]
mod call_completion_tests {
    use super::*;

    fn site(success: Option<&str>, failure: Option<&str>, cancelled: Option<&str>) -> CallSite {
        CallSite {
            orbital: None,
            trait_name: "Keyed".into(),
            follow_on: true,
            entity_id: "r1".into(),
            emit: CallRoutes {
                success: success.map(Into::into),
                failure: failure.map(Into::into),
                cancelled: cancelled.map(Into::into),
                on_message: None,
            },
        }
    }

    fn key_of(payload: &Value) -> Option<&str> {
        payload.get("key").and_then(Value::as_str)
    }

    #[test]
    fn each_outcome_fires_its_route_with_the_programs_key_not_the_hosts() {
        let host_key = host_call_key(Some("tab-1"), "Keyed", "r1", "job");
        let at = site(Some("DONE"), Some("FAILED"), Some("STOPPED"));
        let answer = Value::object(ValueMap::from_iter([("answer".into(), Value::from(42.0))]));
        let (event, payload) = call_completion_event(&at, Some(&host_key), CallOutcome::Answered(answer)).expect("success route");
        assert_eq!((event.as_str(), key_of(&payload)), ("DONE", Some("job")));
        assert_eq!(payload.get("answer").and_then(|v| v.number().ok()), Some(42.0));
        let (event, payload) = call_completion_event(&at, Some(&host_key), CallOutcome::Failed("model down".into())).expect("failure route");
        assert_eq!((event.as_str(), key_of(&payload)), ("FAILED", Some("job")));
        assert_eq!(payload.get("error").and_then(Value::as_str), Some("model down"));
        let (event, payload) = call_completion_event(&at, Some(&host_key), CallOutcome::Cancelled).expect("cancelled route");
        assert_eq!((event.as_str(), key_of(&payload)), ("STOPPED", Some("job")));
    }

    #[test]
    fn edge_an_answer_that_is_not_a_record_is_passed_as_it_stands_and_a_key_the_kernel_did_not_scope_is_kept() {
        let at = site(Some("DONE"), None, None);
        let (_, payload) = call_completion_event(&at, Some("plain"), CallOutcome::Answered(Value::from(7.0))).expect("success route");
        assert_eq!(payload.number().ok(), Some(7.0));
        let (_, payload) = call_completion_event(&at, Some("plain"), CallOutcome::Answered(Value::object(ValueMap::new()))).expect("success route");
        assert_eq!(key_of(&payload), Some("plain"));
        let (_, payload) = call_completion_event(&at, None, CallOutcome::Answered(Value::object(ValueMap::new()))).expect("success route");
        assert_eq!(key_of(&payload), None, "edge: a keyless call adds no key");
        assert!(call_completion_event(&site(None, None, Some("STOPPED")), None, CallOutcome::Cancelled).is_none(), "edge: only a keyed call is cancelled");
    }

    #[test]
    fn control_an_outcome_whose_route_is_not_declared_fires_nothing() {
        let at = site(Some("DONE"), None, None);
        assert!(call_completion_event(&at, Some("k"), CallOutcome::Failed("x".into())).is_none());
        assert!(call_completion_event(&at, Some("k"), CallOutcome::Cancelled).is_none());
    }
}
