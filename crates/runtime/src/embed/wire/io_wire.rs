//! The I/O asks and answers as they cross the host boundary.

use super::*;

/// The I/O a dispatch needs the host to do (`IoRequest` on the wire).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IoRequestWire {
    #[serde(rename_all = "camelCase")]
    ReadRow { entity_type: String, id: String },
    #[serde(rename_all = "camelCase")]
    ListRows { entity_type: String },
    #[serde(rename_all = "camelCase")]
    QueryRows {
        entity_type: String,
        query: RowQuery,
        project: RowProjection,
    },
    #[serde(rename_all = "camelCase")]
    WriteRows { batch: RowWriteBatch },
    #[serde(rename_all = "camelCase")]
    CommitRows { rows: Vec<RowCommit> },
    #[serde(rename_all = "camelCase")]
    CreateRow { entity_type: String, data: Value },
    #[serde(rename_all = "camelCase")]
    WriteRow {
        entity_type: String,
        id: String,
        data: Value,
    },
    #[serde(rename_all = "camelCase")]
    DeleteRow { entity_type: String, id: String },
    #[serde(rename_all = "camelCase")]
    CallService {
        service: String,
        method: String,
        args: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller: Option<CallerWire>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<CallSiteWire>,
    },
    #[serde(rename_all = "camelCase")]
    CancelCall { key: String },
    #[serde(rename_all = "camelCase")]
    Substrate {
        op: String,
        args: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<CallSiteWire>,
    },
    #[serde(rename_all = "camelCase")]
    PageFacts { fields: Vec<PageFactFieldWire> },
    #[serde(rename_all = "camelCase")]
    Prompt {
        text: String,
        schema: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Embed {
        texts: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Tab {
        ask: TabAsk,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Secret {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    SecretSet {
        name: String,
        #[cfg_attr(test, ts(type = "string"))]
        value: SecretText,
        ttl_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    SecretRotate {
        name: String,
        #[cfg_attr(test, ts(type = "string"))]
        value: SecretText,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Deliver {
        envelope: Envelope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        site: CallSiteWire,
    },
    #[serde(rename_all = "camelCase")]
    Random { bytes: u32 },
    Clock,
    /// Answered by the kernel itself; a host never receives it.
    #[serde(rename_all = "camelCase")]
    Sealed { op: String, args: Vec<Value> },
    #[serde(rename_all = "camelCase")]
    Os {
        op: OsOp,
        args: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<CallSiteWire>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    CredentialStatus {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    CredentialForget {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    CredentialConnect { grant: CredentialGrantWire },
    CredentialStoreList,
    #[serde(rename_all = "camelCase")]
    CredentialEnvTails { names: Vec<String> },
    #[serde(rename_all = "camelCase")]
    CredentialStoreRead { name: String },
    #[serde(rename_all = "camelCase")]
    CredentialStoreWrite {
        service: String,
        name: String,
        #[cfg_attr(test, ts(type = "string"))]
        value: SecretText,
    },
    #[serde(rename_all = "camelCase")]
    CredentialStoreRemove { name: String },
    CredentialStoreRotate,
    CredentialModeRead,
    #[serde(rename_all = "camelCase")]
    CredentialModeWrite { mode: String },
}

/// A `credential-connect` ask on the wire: the code and verifier travel as plain text to the host that exchanges them.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialGrantWire {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    pub token_endpoint: String,
    pub grant: String,
    #[cfg_attr(test, ts(type = "string"))]
    pub code: SecretText,
    pub redirect_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "string"))]
    pub verifier: Option<SecretText>,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub client_auth: ClientAuth,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion: Option<AssertionSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_token: Option<JsonTokenSpec>,
}

impl From<&CredentialGrant> for CredentialGrantWire {
    fn from(grant: &CredentialGrant) -> Self {
        Self {
            name: grant.name.clone(),
            principal: grant.principal.clone(),
            token_endpoint: grant.token_endpoint.clone(),
            grant: grant.grant.clone(),
            code: SecretText::new(grant.code.clone()),
            redirect_uri: grant.redirect_uri.clone(),
            verifier: grant.verifier.clone().map(SecretText::new),
            client_id: grant.client_id.clone(),
            client_secret: grant.client_secret.clone(),
            client_auth: grant.client_auth,
            scopes: grant.scopes.clone(),
            refresh_credential: grant.refresh_credential.clone(),
            assertion: grant.assertion.clone(),
            json_token: grant.json_token.clone(),
        }
    }
}

impl From<CredentialGrantWire> for CredentialGrant {
    fn from(wire: CredentialGrantWire) -> Self {
        Self {
            name: wire.name,
            principal: wire.principal,
            token_endpoint: wire.token_endpoint,
            grant: wire.grant,
            code: wire.code.into_inner(),
            redirect_uri: wire.redirect_uri,
            verifier: wire.verifier.map(SecretText::into_inner),
            client_id: wire.client_id,
            client_secret: wire.client_secret,
            client_auth: wire.client_auth,
            scopes: wire.scopes,
            refresh_credential: wire.refresh_credential,
            assertion: wire.assertion,
            json_token: wire.json_token,
        }
    }
}

/// Why the on-device model or embedder gave no answer.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFailureWire {
    pub kind: ModelFailureKindWire,
    pub message: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelFailureKindWire {
    Unconfigured,
    Unavailable,
    Failed,
}

impl From<&ModelFailure> for ModelFailureWire {
    fn from(failure: &ModelFailure) -> Self {
        Self {
            kind: match failure.kind {
                ModelFailureKind::Unconfigured => ModelFailureKindWire::Unconfigured,
                ModelFailureKind::Unavailable => ModelFailureKindWire::Unavailable,
                ModelFailureKind::Failed => ModelFailureKindWire::Failed,
            },
            message: failure.message.clone(),
        }
    }
}

impl From<ModelFailureWire> for ModelFailure {
    fn from(wire: ModelFailureWire) -> Self {
        let kind = match wire.kind {
            ModelFailureKindWire::Unconfigured => ModelFailureKind::Unconfigured,
            ModelFailureKindWire::Unavailable => ModelFailureKind::Unavailable,
            ModelFailureKindWire::Failed => ModelFailureKind::Failed,
        };
        ModelFailure::new(kind, wire.message)
    }
}

/// A page fact `browser/page-facts` reads.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageFactFieldWire {
    Hostname,
    Referrer,
    ScreenWidth,
    Href,
}

impl From<PageFactField> for PageFactFieldWire {
    fn from(field: PageFactField) -> Self {
        match field {
            PageFactField::Hostname => Self::Hostname,
            PageFactField::Referrer => Self::Referrer,
            PageFactField::ScreenWidth => Self::ScreenWidth,
            PageFactField::Href => Self::Href,
        }
    }
}

impl From<PageFactFieldWire> for PageFactField {
    fn from(field: PageFactFieldWire) -> Self {
        match field {
            PageFactFieldWire::Hostname => Self::Hostname,
            PageFactFieldWire::Referrer => Self::Referrer,
            PageFactFieldWire::ScreenWidth => Self::ScreenWidth,
            PageFactFieldWire::Href => Self::Href,
        }
    }
}

/// The page facts a host read: `available` is false when there is no page.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageFactsWire {
    pub available: bool,
    pub hostname: String,
    pub referrer: String,
    pub screen_width: Option<f64>,
    pub href: String,
}

/// A host's answer to one request; `kind` must be the one the request calls for.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IoAnswerWire {
    Row {
        #[cfg_attr(test, ts(type = "Settled<EntityRow | null, string>"))]
        result: Settled<Value, String>,
    },
    Rows {
        #[cfg_attr(test, ts(type = "Settled<Array<EntityRow>, string>"))]
        result: Settled<Vec<Value>, String>,
    },
    Page {
        result: Settled<RowPage, String>,
    },
    IdPage {
        result: Settled<RowIdPage, String>,
    },
    /// The host cannot run the query it was sent; nothing was read.
    QueryUnsupported {
        reason: String,
    },
    Wrote {
        result: Settled<Vec<RowWriteOutcome>, String>,
    },
    /// The host cannot run the writes it was sent; nothing was written.
    WriteUnsupported {
        reason: String,
    },
    /// `true` when every row was committed; `false` on a conflict, when nothing was.
    Committed {
        result: Settled<bool, String>,
    },
    Deleted {
        result: Settled<(), String>,
    },
    Service {
        result: Settled<Value, String>,
    },
    /// The call was aborted by `cancel-call` while it ran: the call site's cancelled route fires.
    ServiceCancelled,
    /// The call was started and its outcome arrives later as a dispatch of its own.
    ServiceDeferred,
    Cancelled {
        result: Settled<(), String>,
    },
    Substrate {
        result: Settled<Value, SubstrateFailureWire>,
    },
    PageFacts {
        result: Settled<PageFactsWire, String>,
    },
    Prompt {
        result: Settled<String, ModelFailureWire>,
    },
    Embed {
        result: Settled<Vec<Vec<f64>>, ModelFailureWire>,
    },
    Tab {
        result: Settled<Value, TabFailure>,
    },
    Secret {
        #[cfg_attr(test, ts(type = "Settled<string | null, string>"))]
        result: Settled<Option<SecretText>, String>,
    },
    SecretSet {
        result: Settled<(), String>,
    },
    SecretRotate {
        result: Settled<(), String>,
    },
    /// The random bytes, standard base64.
    Random {
        #[cfg_attr(test, ts(type = "Settled<string, string>"))]
        result: Settled<SecretText, String>,
    },
    /// The host clock, epoch milliseconds.
    Clock {
        result: Settled<i64, String>,
    },
    Sealed {
        result: Settled<Value, String>,
    },
    Os {
        result: Settled<Value, OsError>,
    },
    CredentialStatus {
        result: Settled<CredentialStanding, String>,
    },
    CredentialForget {
        result: Settled<(), String>,
    },
    CredentialConnect {
        result: Settled<CredentialStanding, ConnectFailure>,
    },
    CredentialStoreList {
        result: Settled<Option<StoreListing>, String>,
    },
    CredentialEnvTails {
        result: Settled<EnvTails, String>,
    },
    CredentialStoreRead {
        #[cfg_attr(test, ts(type = "Settled<string | null, string>"))]
        result: Settled<Option<SecretText>, String>,
    },
    CredentialStoreWrite {
        result: Settled<(), String>,
    },
    CredentialStoreRemove {
        result: Settled<bool, String>,
    },
    CredentialStoreRotate {
        result: Settled<RotateCounts, String>,
    },
    CredentialModeRead {
        result: Settled<Option<String>, String>,
    },
    CredentialModeWrite {
        result: Settled<String, String>,
    },
    Deliver {
        result: Settled<Accepted, DeliverRefusal>,
    },
}

/// A request a dispatch is waiting on, named by `id`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRequestWire {
    pub id: u64,
    pub request: IoRequestWire,
}

/// The host's answer to the request `id`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IoReplyWire {
    pub id: u64,
    pub answer: IoAnswerWire,
}

/// Where a server dispatch stands: finished with its response, or waiting on `requests` (answer them
/// in any order, concurrently, and `resume` the `flight`).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
#[expect(clippy::large_enum_variant, reason = "public unboxed layout is the API; pinned by tests/public_layout_contract.rs")]
pub enum ServerProgressWire {
    Done {
        response: EventResponse,
    },
    Await {
        flight: u64,
        requests: Vec<PendingRequestWire>,
    },
    Queued {
        ticket: u64,
    },
}

/// A queued request the kernel started once the lane freed, under the ticket `start` answered with.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum NextWire {
    Started {
        ticket: u64,
        progress: ServerProgressWire,
    },
    Failed {
        ticket: u64,
        error: String,
    },
}

/// Where a row-set resolve through a host-answered store stands: the resolved page, or `requests` to answer
/// before `resumeResolve`-ing the `flight`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ResolveProgressWire {
    Done {
        #[cfg_attr(test, ts(type = "Array<EntityRow>"))]
        rows: Vec<Value>,
        total: usize,
    },
    Await {
        flight: u64,
        requests: Vec<PendingRequestWire>,
    },
}

impl From<&ServiceCaller> for CallerWire {
    fn from(caller: &ServiceCaller) -> Self {
        Self {
            principal: caller.principal.clone(),
            role: caller.role.clone(),
        }
    }
}
