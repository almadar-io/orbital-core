//! The embedded kernel's wire: what every host (C ABI, JNI, WASM) hands across its boundary as
//! JSON. Field names are the hosts' contract; `tests/fixtures/embed/golden` pins them.

use crate::effects::{ServiceCaller, SubstrateFailure};
use crate::runtime::resumable::{
    CallRoutes, CallSite, IoAnswer, IoReply, IoRequest, PageFacts, PendingRequest, RequestId,
};
use crate::ir::PageFactField;
use crate::oauth::ask::{OAuthAsk, OAuthReply};
use crate::runtime::http_request::{BeaconOutcome, StreamFraming};
use crate::runtime::http_service::{HttpRequest, HttpResponse, TransportError};
use crate::runtime::os::{OsError, OsOp};
use base64::Engine;
use crate::runtime::row_query::{
    RowIdPage, RowPage, RowProjection, RowQuery, RowWriteBatch, RowWriteOutcome,
};
use crate::runtime::{AwaitingTrait, DispatchMode, ResolvedTheme, TraitSnapshot};
use crate::{EventRequest, EventResponse, Value};
use std::sync::Arc;
use serde::{Deserialize, Serialize};

/// A server leg and the orbital that owns the trait it is addressed to (the route it posts to).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct RoutedLeg {
    pub orbital: String,
    pub leg: EventRequest,
}

/// The `(trait, event)` that began a dispatch; a leg failure is reported against its event.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct SeedWire {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
}

/// One dispatch: the local response, the legs to post, the mode, the pre-dispatch snapshot
/// (`RuntimeOptimistic` only), the `(trait, event)` pairs already realized locally and the
/// traits awaiting the legs.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchWire {
    pub orbital: String,
    pub seed: SeedWire,
    pub response: EventResponse,
    pub server_legs: Vec<RoutedLeg>,
    pub mode: DispatchMode,
    pub snapshot: Option<TraitSnapshot>,
    pub already_delivered: Vec<(String, String)>,
    pub awaiting: Vec<AwaitingTrait>,
    /// The plain leg a stateful host is posted when the dispatch delegated no effect.
    pub stateful_leg: Option<StatefulLegWire>,
    /// The traits whose pre-dispatch frame is the base a posted answer is folded against (`foldBase`).
    pub fold_seeded: Vec<String>,
}

/// The leg of a server-backed dispatch that delegated nothing, and the traits awaiting its answer.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatefulLegWire {
    pub orbital: String,
    pub leg: EventRequest,
    pub awaiting: Vec<AwaitingTrait>,
}

/// One `mount` leg: the seeds of one owning orbital that need the host, posted together.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MountLegWire {
    pub orbital: String,
    pub seeds: Vec<SeedWire>,
    pub leg: EventRequest,
    /// The union of the seeds' already-realized `(trait, event)` pairs.
    pub already_delivered: Vec<(String, String)>,
    pub awaiting: Vec<AwaitingTrait>,
    /// The seeds whose pre-dispatch frame is the fold base (`foldBase`).
    pub fold_seeded: Vec<String>,
}

/// A mount batch: each seed's local dispatch and the legs to post, per host topology.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MountWire {
    pub seeds: Vec<DispatchWire>,
    /// For a host that carries the circuit state (stateless): a leg per orbital whose seeds delegated effects.
    pub legs: Vec<MountLegWire>,
    /// For a host that holds it (stateful): the legs of every server-backed seed, delegated effect or not.
    pub stateful_legs: Vec<MountLegWire>,
    /// The seed that failed to dispatch; the batch stopped there and carries no legs.
    pub failed: Option<MountFailure>,
}

/// The seed a mount batch stopped at, and why.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct MountFailure {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
    pub message: String,
}

/// A folded server response: each applied `(trait, newState)` and the merged response.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedWire {
    pub applied: Vec<(String, String)>,
    pub response: EventResponse,
    /// The legs the fold's own arms owe the host (a refetch after a browser-stored write), to post and fold in turn.
    pub server_legs: Vec<RoutedLeg>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct FiredTick {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub tick: String,
}

/// The listener cascade of one tick `emit`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct FrameDispatch {
    #[serde(rename = "trait")]
    pub trait_name: String,
    #[serde(flatten)]
    pub dispatch: DispatchWire,
}

/// One host frame of the tick clock.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameWire {
    pub fired: Vec<FiredTick>,
    pub response: EventResponse,
    pub dispatches: Vec<FrameDispatch>,
    /// The `runTick` legs of fired ticks that write browser-stored data, for the host that owns the store.
    pub browser_legs: Vec<RoutedLeg>,
    pub dropped_server_effects: usize,
    pub errors: Vec<String>,
}

/// A trait name more than one orbital declares; never listed under `states`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct AmbiguousTrait {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub orbitals: Vec<String>,
}

/// Every trait state plus the resolved theme and what it derives.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotWire {
    #[cfg_attr(test, ts(type = "JsonObject"))]
    pub states: serde_json::Map<String, serde_json::Value>,
    pub ambiguous_traits: Vec<AmbiguousTrait>,
    pub theme: ResolvedTheme,
    pub canvas_theme: crate::runtime::canvas_theme::CanvasTheme,
    pub motion: crate::runtime::motion::ResolvedMotion,
}

/// Who a `call-service` runs for.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerWire {
    pub principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// The events a `call-service` call site routes its outcome and live messages to.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRoutesWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_message: Option<String>,
}

/// Where a `call-service` is made from: the trait (and orbital) whose transition runs it, its routes,
/// and whether the step is a follow-on of the dispatch.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSiteWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbital: Option<String>,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub follow_on: bool,
    pub emit: CallRoutesWire,
}

/// A substrate operator's failure.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubstrateFailureWire {
    pub message: String,
    pub errors: Vec<Value>,
}

/// A host's result for one request: the value, or the message it failed with.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Settled<T, E> {
    Ok(T),
    Error(E),
}

impl<T, E> From<Result<T, E>> for Settled<T, E> {
    fn from(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Settled::Ok(value),
            Err(error) => Settled::Error(error),
        }
    }
}

impl<T, E> From<Settled<T, E>> for Result<T, E> {
    fn from(settled: Settled<T, E>) -> Self {
        match settled {
            Settled::Ok(value) => Ok(value),
            Settled::Error(error) => Err(error),
        }
    }
}

/// A secret value on the wire: a plain JSON string whose `Debug` never shows it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretText(String);

impl SecretText {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

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
    Substrate { op: String, args: Vec<Value> },
    #[serde(rename_all = "camelCase")]
    Http {
        request: HttpRequestWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<CallSiteWire>,
    },
    #[serde(rename_all = "camelCase")]
    HttpStream {
        request: HttpRequestWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        framing: StreamFramingWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<CallSiteWire>,
    },
    #[serde(rename_all = "camelCase")]
    HttpBeacon {
        request: HttpRequestWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<CallSiteWire>,
    },
    #[serde(rename_all = "camelCase")]
    PageFacts { fields: Vec<PageFactFieldWire> },
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
    #[serde(rename = "oauth", rename_all = "camelCase")]
    OAuth {
        ask: OAuthAsk,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Random { bytes: u32 },
    #[serde(rename_all = "camelCase")]
    Os {
        op: OsOp,
        args: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<CallSiteWire>,
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
}

/// A page fact `browser/page-facts` reads.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageFactFieldWire {
    Hostname,
    Referrer,
    ScreenWidth,
}

impl From<PageFactField> for PageFactFieldWire {
    fn from(field: PageFactField) -> Self {
        match field {
            PageFactField::Hostname => Self::Hostname,
            PageFactField::Referrer => Self::Referrer,
            PageFactField::ScreenWidth => Self::ScreenWidth,
        }
    }
}

impl From<PageFactFieldWire> for PageFactField {
    fn from(field: PageFactFieldWire) -> Self {
        match field {
            PageFactFieldWire::Hostname => Self::Hostname,
            PageFactFieldWire::Referrer => Self::Referrer,
            PageFactFieldWire::ScreenWidth => Self::ScreenWidth,
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
}

/// How a host took a beacon: its queue accepted it, or one keepalive fetch answered `status`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "via", rename_all = "camelCase")]
pub enum BeaconOutcomeWire {
    Queue,
    Fetch { status: u16 },
    /// The host has no beacon port.
    Unavailable { message: String },
}

impl From<BeaconOutcome> for BeaconOutcomeWire {
    fn from(outcome: BeaconOutcome) -> Self {
        match outcome {
            BeaconOutcome::Queued => Self::Queue,
            BeaconOutcome::Fetched { status } => Self::Fetch { status },
            BeaconOutcome::Unavailable { message } => Self::Unavailable { message },
        }
    }
}

impl From<BeaconOutcomeWire> for BeaconOutcome {
    fn from(outcome: BeaconOutcomeWire) -> Self {
        match outcome {
            BeaconOutcomeWire::Queue => Self::Queued,
            BeaconOutcomeWire::Fetch { status } => Self::Fetched { status },
            BeaconOutcomeWire::Unavailable { message } => Self::Unavailable { message },
        }
    }
}

/// How a streamed `http` reply is framed.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamFramingWire {
    Sse,
    JsonLines,
}

impl From<StreamFraming> for StreamFramingWire {
    fn from(framing: StreamFraming) -> Self {
        match framing {
            StreamFraming::Sse => Self::Sse,
            StreamFraming::JsonLines => Self::JsonLines,
        }
    }
}

impl From<StreamFramingWire> for StreamFraming {
    fn from(framing: StreamFramingWire) -> Self {
        match framing {
            StreamFramingWire::Sse => Self::Sse,
            StreamFramingWire::JsonLines => Self::JsonLines,
        }
    }
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
    Http {
        result: Settled<HttpResponseWire, HttpFailureWire>,
    },
    HttpBeacon {
        result: Settled<BeaconOutcomeWire, HttpFailureWire>,
    },
    PageFacts {
        result: Settled<PageFactsWire, String>,
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
        result: Settled<String, String>,
    },
    Os {
        result: Settled<Value, OsError>,
    },
    CredentialStatus {
        result: Settled<bool, String>,
    },
    CredentialForget {
        result: Settled<(), String>,
    },
    #[serde(rename = "oauth")]
    OAuth {
        result: Settled<OAuthReply, String>,
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

impl From<CallerWire> for ServiceCaller {
    fn from(caller: CallerWire) -> Self {
        Self {
            principal: caller.principal,
            role: caller.role,
        }
    }
}

impl From<&CallSite> for CallSiteWire {
    fn from(site: &CallSite) -> Self {
        Self {
            orbital: site.orbital.clone(),
            trait_name: site.trait_name.clone(),
            follow_on: site.follow_on,
            emit: CallRoutesWire {
                success: site.emit.success.clone(),
                failure: site.emit.failure.clone(),
                cancelled: site.emit.cancelled.clone(),
                on_message: site.emit.on_message.clone(),
            },
        }
    }
}

impl From<CallSiteWire> for CallSite {
    fn from(site: CallSiteWire) -> Self {
        Self {
            orbital: site.orbital,
            trait_name: site.trait_name,
            follow_on: site.follow_on,
            emit: CallRoutes {
                success: site.emit.success,
                failure: site.emit.failure,
                cancelled: site.emit.cancelled,
                on_message: site.emit.on_message,
            },
        }
    }
}

impl From<IoRequestWire> for IoRequest {
    fn from(request: IoRequestWire) -> Self {
        match request {
            IoRequestWire::ReadRow { entity_type, id } => Self::ReadRow { entity_type, id },
            IoRequestWire::ListRows { entity_type } => Self::ListRows { entity_type },
            IoRequestWire::QueryRows {
                entity_type,
                query,
                project,
            } => Self::QueryRows {
                entity_type,
                query,
                project,
            },
            IoRequestWire::WriteRows { batch } => Self::WriteRows { batch },
            IoRequestWire::CreateRow { entity_type, data } => Self::CreateRow { entity_type, data },
            IoRequestWire::WriteRow {
                entity_type,
                id,
                data,
            } => Self::WriteRow {
                entity_type,
                id,
                data,
            },
            IoRequestWire::DeleteRow { entity_type, id } => Self::DeleteRow { entity_type, id },
            IoRequestWire::CallService {
                service,
                method,
                args,
                key,
                caller,
                site,
            } => Self::CallService {
                service,
                method,
                args,
                key,
                caller: caller.map(ServiceCaller::from),
                site: site.map(CallSite::from),
            },
            IoRequestWire::CancelCall { key } => Self::CancelCall { key },
            IoRequestWire::Substrate { op, args } => Self::Substrate { op, args },
            IoRequestWire::Http {
                request,
                key,
                delay_ms,
                site,
            } => Self::Http {
                request: request.into(),
                key,
                delay_ms,
                site: site.map(CallSite::from),
            },
            IoRequestWire::HttpStream {
                request,
                key,
                framing,
                site,
            } => Self::HttpStream {
                request: request.into(),
                key,
                framing: framing.into(),
                site: site.map(CallSite::from),
            },
            IoRequestWire::HttpBeacon { request, key, site } => Self::HttpBeacon {
                request: request.into(),
                key,
                site: site.map(CallSite::from),
            },
            IoRequestWire::PageFacts { fields } => Self::PageFacts {
                fields: fields.into_iter().map(PageFactField::from).collect(),
            },
            IoRequestWire::Secret { name, principal } => Self::Secret { name, principal },
            IoRequestWire::SecretSet {
                name,
                value,
                ttl_ms,
                principal,
            } => Self::SecretSet {
                name,
                value: value.into_inner(),
                ttl_ms,
                principal,
            },
            IoRequestWire::SecretRotate { name, value, principal } => Self::SecretRotate {
                name,
                value: value.into_inner(),
                principal,
            },
            IoRequestWire::Random { bytes } => Self::Random { bytes },
            IoRequestWire::Os { op, args, key, site } => Self::Os {
                op,
                args,
                key,
                site: site.map(CallSite::from),
            },
            IoRequestWire::CredentialStatus { name, principal } => {
                Self::CredentialStatus { name, principal }
            }
            IoRequestWire::CredentialForget { name, principal } => {
                Self::CredentialForget { name, principal }
            }
            IoRequestWire::OAuth { ask, principal } => Self::OAuth { ask, principal },
        }
    }
}

impl From<&IoRequest> for IoRequestWire {
    fn from(request: &IoRequest) -> Self {
        match request {
            IoRequest::ReadRow { entity_type, id } => Self::ReadRow {
                entity_type: entity_type.clone(),
                id: id.clone(),
            },
            IoRequest::ListRows { entity_type } => Self::ListRows {
                entity_type: entity_type.clone(),
            },
            IoRequest::QueryRows {
                entity_type,
                query,
                project,
            } => Self::QueryRows {
                entity_type: entity_type.clone(),
                query: query.clone(),
                project: *project,
            },
            IoRequest::WriteRows { batch } => Self::WriteRows {
                batch: batch.clone(),
            },
            IoRequest::CreateRow { entity_type, data } => Self::CreateRow {
                entity_type: entity_type.clone(),
                data: data.clone(),
            },
            IoRequest::WriteRow {
                entity_type,
                id,
                data,
            } => Self::WriteRow {
                entity_type: entity_type.clone(),
                id: id.clone(),
                data: data.clone(),
            },
            IoRequest::DeleteRow { entity_type, id } => Self::DeleteRow {
                entity_type: entity_type.clone(),
                id: id.clone(),
            },
            IoRequest::CallService {
                service,
                method,
                args,
                key,
                caller,
                site,
            } => Self::CallService {
                service: service.clone(),
                method: method.clone(),
                args: args.clone(),
                key: key.clone(),
                caller: caller.as_ref().map(CallerWire::from),
                site: site.as_ref().map(CallSiteWire::from),
            },
            IoRequest::CancelCall { key } => Self::CancelCall { key: key.clone() },
            IoRequest::Substrate { op, args } => Self::Substrate {
                op: op.clone(),
                args: args.clone(),
            },
            IoRequest::Http {
                request,
                key,
                delay_ms,
                site,
            } => Self::Http {
                request: HttpRequestWire::from(request),
                key: key.clone(),
                delay_ms: *delay_ms,
                site: site.as_ref().map(CallSiteWire::from),
            },
            IoRequest::HttpStream {
                request,
                key,
                framing,
                site,
            } => Self::HttpStream {
                request: HttpRequestWire::from(request),
                key: key.clone(),
                framing: StreamFramingWire::from(*framing),
                site: site.as_ref().map(CallSiteWire::from),
            },
            IoRequest::HttpBeacon { request, key, site } => Self::HttpBeacon {
                request: HttpRequestWire::from(request),
                key: key.clone(),
                site: site.as_ref().map(CallSiteWire::from),
            },
            IoRequest::PageFacts { fields } => Self::PageFacts {
                fields: fields.iter().copied().map(PageFactFieldWire::from).collect(),
            },
            IoRequest::Secret { name, principal } => Self::Secret {
                name: name.clone(),
                principal: principal.clone(),
            },
            IoRequest::SecretSet {
                name,
                value,
                ttl_ms,
                principal,
            } => Self::SecretSet {
                name: name.clone(),
                value: SecretText::new(value.clone()),
                ttl_ms: *ttl_ms,
                principal: principal.clone(),
            },
            IoRequest::SecretRotate { name, value, principal } => Self::SecretRotate {
                name: name.clone(),
                value: SecretText::new(value.clone()),
                principal: principal.clone(),
            },
            IoRequest::Random { bytes } => Self::Random { bytes: *bytes },
            IoRequest::Os { op, args, key, site } => Self::Os {
                op: *op,
                args: args.clone(),
                key: key.clone(),
                site: site.as_ref().map(CallSiteWire::from),
            },
            IoRequest::CredentialStatus { name, principal } => Self::CredentialStatus {
                name: name.clone(),
                principal: principal.clone(),
            },
            IoRequest::CredentialForget { name, principal } => Self::CredentialForget {
                name: name.clone(),
                principal: principal.clone(),
            },
            IoRequest::OAuth { ask, principal } => Self::OAuth { ask: ask.clone(), principal: principal.clone() },
        }
    }
}

impl From<&PendingRequest> for PendingRequestWire {
    fn from(pending: &PendingRequest) -> Self {
        Self {
            id: pending.id.raw(),
            request: IoRequestWire::from(&pending.request),
        }
    }
}

impl From<&IoAnswer> for IoAnswerWire {
    fn from(answer: &IoAnswer) -> Self {
        match answer {
            IoAnswer::Row(r) => Self::Row {
                result: r.clone().into(),
            },
            IoAnswer::Rows(r) => Self::Rows {
                result: r
                    .as_ref()
                    .map(|rows| rows.iter().map(|row| (**row).clone()).collect())
                    .map_err(Clone::clone)
                    .into(),
            },
            IoAnswer::Page(r) => Self::Page {
                result: r.clone().into(),
            },
            IoAnswer::IdPage(r) => Self::IdPage {
                result: r.clone().into(),
            },
            IoAnswer::QueryUnsupported(reason) => Self::QueryUnsupported {
                reason: reason.clone(),
            },
            IoAnswer::Wrote(r) => Self::Wrote {
                result: r.clone().into(),
            },
            IoAnswer::WriteUnsupported(reason) => Self::WriteUnsupported {
                reason: reason.clone(),
            },
            IoAnswer::Deleted(r) => Self::Deleted {
                result: r.clone().into(),
            },
            IoAnswer::Service(r) => Self::Service {
                result: r.clone().into(),
            },
            IoAnswer::ServiceCancelled => Self::ServiceCancelled,
            IoAnswer::ServiceDeferred => Self::ServiceDeferred,
            IoAnswer::Cancelled(r) => Self::Cancelled {
                result: r.clone().into(),
            },
            IoAnswer::Substrate(r) => Self::Substrate {
                result: r
                    .clone()
                    .map_err(|f| SubstrateFailureWire {
                        message: f.message,
                        errors: f.errors,
                    })
                    .into(),
            },
            IoAnswer::Http(r) => Self::Http {
                result: r
                    .as_ref()
                    .map(HttpResponseWire::from)
                    .map_err(HttpFailureWire::from)
                    .into(),
            },
            IoAnswer::HttpBeacon(r) => Self::HttpBeacon {
                result: r
                    .as_ref()
                    .map(|outcome| BeaconOutcomeWire::from(outcome.clone()))
                    .map_err(HttpFailureWire::from)
                    .into(),
            },
            IoAnswer::PageFacts(r) => Self::PageFacts {
                result: r
                    .as_ref()
                    .map(|facts| PageFactsWire {
                        available: facts.available,
                        hostname: facts.hostname.clone(),
                        referrer: facts.referrer.clone(),
                        screen_width: facts.screen_width,
                    })
                    .map_err(Clone::clone)
                    .into(),
            },
            IoAnswer::Secret(r) => Self::Secret {
                result: r.clone().map(|value| value.map(SecretText::new)).into(),
            },
            IoAnswer::SecretSet(r) => Self::SecretSet {
                result: r.clone().into(),
            },
            IoAnswer::SecretRotate(r) => Self::SecretRotate {
                result: r.clone().into(),
            },
            IoAnswer::Random(r) => Self::Random {
                result: r
                    .as_ref()
                    .map(|bytes| crate::codec::base64_encode(bytes))
                    .map_err(Clone::clone)
                    .into(),
            },
            IoAnswer::Os(r) => Self::Os { result: r.clone().into() },
            IoAnswer::CredentialStatus(r) => Self::CredentialStatus {
                result: r.clone().into(),
            },
            IoAnswer::CredentialForget(r) => Self::CredentialForget {
                result: r.clone().into(),
            },
            IoAnswer::OAuth(r) => Self::OAuth { result: r.clone().into() },
        }
    }
}

impl From<IoAnswerWire> for IoAnswer {
    fn from(answer: IoAnswerWire) -> Self {
        match answer {
            IoAnswerWire::Row { result } => Self::Row(result.into()),
            IoAnswerWire::Rows { result } => {
                let result: Result<Vec<Value>, String> = result.into();
                Self::Rows(result.map(|rows| rows.into_iter().map(Arc::new).collect()))
            }
            IoAnswerWire::Page { result } => Self::Page(result.into()),
            IoAnswerWire::IdPage { result } => Self::IdPage(result.into()),
            IoAnswerWire::QueryUnsupported { reason } => Self::QueryUnsupported(reason),
            IoAnswerWire::Wrote { result } => Self::Wrote(result.into()),
            IoAnswerWire::WriteUnsupported { reason } => Self::WriteUnsupported(reason),
            IoAnswerWire::Deleted { result } => Self::Deleted(result.into()),
            IoAnswerWire::Service { result } => Self::Service(result.into()),
            IoAnswerWire::ServiceCancelled => Self::ServiceCancelled,
            IoAnswerWire::ServiceDeferred => Self::ServiceDeferred,
            IoAnswerWire::Cancelled { result } => Self::Cancelled(result.into()),
            IoAnswerWire::Substrate { result } => {
                let result: Result<Value, SubstrateFailureWire> = result.into();
                Self::Substrate(result.map_err(|f| SubstrateFailure {
                    message: f.message,
                    errors: f.errors,
                }))
            }
            IoAnswerWire::Http { result } => {
                let result: Result<HttpResponseWire, HttpFailureWire> = result.into();
                Self::Http(result.map(HttpResponse::from).map_err(TransportError::from))
            }
            IoAnswerWire::HttpBeacon { result } => {
                let result: Result<BeaconOutcomeWire, HttpFailureWire> = result.into();
                Self::HttpBeacon(result.map(BeaconOutcome::from).map_err(TransportError::from))
            }
            IoAnswerWire::PageFacts { result } => {
                let result: Result<PageFactsWire, String> = result.into();
                Self::PageFacts(result.map(|facts| PageFacts {
                    available: facts.available,
                    hostname: facts.hostname,
                    referrer: facts.referrer,
                    screen_width: facts.screen_width,
                }))
            }
            IoAnswerWire::Secret { result } => {
                let result: Result<Option<SecretText>, String> = result.into();
                Self::Secret(result.map(|value| value.map(SecretText::into_inner)))
            }
            IoAnswerWire::SecretSet { result } => Self::SecretSet(result.into()),
            IoAnswerWire::SecretRotate { result } => Self::SecretRotate(result.into()),
            IoAnswerWire::Random { result } => {
                let result: Result<String, String> = result.into();
                Self::Random(result.and_then(|text| {
                    crate::codec::base64_decode(&text).map_err(|e| format!("random: not base64: {e}"))
                }))
            }
            IoAnswerWire::Os { result } => Self::Os(result.into()),
            IoAnswerWire::CredentialStatus { result } => Self::CredentialStatus(result.into()),
            IoAnswerWire::CredentialForget { result } => Self::CredentialForget(result.into()),
            IoAnswerWire::OAuth { result } => Self::OAuth(result.into()),
        }
    }
}

impl From<IoReplyWire> for IoReply {
    fn from(reply: IoReplyWire) -> Self {
        Self {
            id: RequestId::from_raw(reply.id),
            answer: reply.answer.into(),
        }
    }
}

#[cfg(test)]
#[path = "ts_export.rs"]
mod ts_export;

/// A request or reply body on the wire: text as text, any other bytes as base64, so a binary body survives JSON.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HttpBody {
    #[default]
    Empty,
    Text {
        text: String,
    },
    Base64 {
        data: String,
    },
}

impl std::fmt::Debug for HttpBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Text { text } => write!(f, "Text({} bytes)", text.len()),
            Self::Base64 { data } => write!(f, "Base64({} chars)", data.len()),
        }
    }
}

impl From<&[u8]> for HttpBody {
    fn from(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::Empty;
        }
        match std::str::from_utf8(bytes) {
            Ok(text) => Self::Text {
                text: text.to_string(),
            },
            Err(_) => Self::Base64 {
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            },
        }
    }
}

impl From<HttpBody> for Vec<u8> {
    fn from(body: HttpBody) -> Self {
        match body {
            HttpBody::Empty => Vec::new(),
            HttpBody::Text { text } => text.into_bytes(),
            HttpBody::Base64 { data } => base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap_or_default(),
        }
    }
}

/// `HttpRequest` on the wire.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequestWire {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: HttpBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl std::fmt::Debug for HttpRequestWire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&HttpRequest::from(self.clone()), f)
    }
}

impl From<&HttpRequest> for HttpRequestWire {
    fn from(r: &HttpRequest) -> Self {
        Self {
            method: r.method.clone(),
            url: r.url.clone(),
            headers: r.headers.clone(),
            body: HttpBody::from(r.body.as_slice()),
            timeout_ms: r.timeout.map(|d| d.as_millis() as u64),
        }
    }
}

impl From<HttpRequestWire> for HttpRequest {
    fn from(w: HttpRequestWire) -> Self {
        Self {
            method: w.method,
            url: w.url,
            headers: w.headers,
            body: w.body.into(),
            timeout: w.timeout_ms.map(std::time::Duration::from_millis),
        }
    }
}

/// `HttpResponse` on the wire. A non-2xx status is a reply, not a failure.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponseWire {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: HttpBody,
}

impl std::fmt::Debug for HttpResponseWire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&HttpResponse::from(self.clone()), f)
    }
}

impl From<&HttpResponse> for HttpResponseWire {
    fn from(r: &HttpResponse) -> Self {
        Self {
            status: r.status,
            headers: r.headers.clone(),
            body: HttpBody::from(r.body.as_slice()),
        }
    }
}

impl From<HttpResponseWire> for HttpResponse {
    fn from(w: HttpResponseWire) -> Self {
        Self {
            status: w.status,
            headers: w.headers,
            body: w.body.into(),
        }
    }
}

/// `TransportError` on the wire: the request produced no reply.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HttpFailureWire {
    TimedOut,
    Failed { message: String },
    Refused { message: String },
}

impl From<&TransportError> for HttpFailureWire {
    fn from(e: &TransportError) -> Self {
        match e {
            TransportError::TimedOut => Self::TimedOut,
            TransportError::Failed(message) => Self::Failed {
                message: message.clone(),
            },
            TransportError::Refused(message) => Self::Refused {
                message: message.clone(),
            },
        }
    }
}

impl From<HttpFailureWire> for TransportError {
    fn from(w: HttpFailureWire) -> Self {
        match w {
            HttpFailureWire::TimedOut => Self::TimedOut,
            HttpFailureWire::Failed { message } => Self::Failed(message),
            HttpFailureWire::Refused { message } => Self::Refused(message),
        }
    }
}
