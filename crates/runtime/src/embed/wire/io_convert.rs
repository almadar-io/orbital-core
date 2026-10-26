//! Conversions between the I/O wire and the kernel's own ask and answer types.

use super::*;

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
            entity_id: (site.entity_id != crate::process_result::RUNTIME_SINGLETON).then(|| site.entity_id.clone()),
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
            entity_id: site.entity_id.unwrap_or_else(|| crate::process_result::RUNTIME_SINGLETON.to_string()),
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
            IoRequestWire::CommitRows { rows } => Self::CommitRows { rows },
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
            IoRequestWire::Substrate { op, args, key, site } => Self::Substrate {
                op,
                args,
                key,
                site: site.map(CallSite::from),
            },
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
                continuation: None,
            },
            IoRequestWire::HttpBeacon { request, key, site } => Self::HttpBeacon {
                request: request.into(),
                key,
                site: site.map(CallSite::from),
            },
            IoRequestWire::PageFacts { fields } => Self::PageFacts {
                fields: fields.into_iter().map(PageFactField::from).collect(),
            },
            IoRequestWire::Prompt { text, schema, key } => Self::Prompt { text, schema, key },
            IoRequestWire::Embed { texts, key } => Self::Embed { texts, key },
            IoRequestWire::Tab { ask, key } => Self::Tab { ask, key },
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
            IoRequestWire::Clock => Self::Clock,
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
            IoRequestWire::CredentialStoreList => Self::CredentialStoreList,
            IoRequestWire::CredentialStoreRead { name } => Self::CredentialStoreRead { name },
            IoRequestWire::CredentialStoreWrite { service, name, value } => {
                Self::CredentialStoreWrite { service, name, value: value.into_inner() }
            }
            IoRequestWire::CredentialStoreRemove { name } => Self::CredentialStoreRemove { name },
            IoRequestWire::CredentialStoreRotate => Self::CredentialStoreRotate,
            IoRequestWire::CredentialModeRead => Self::CredentialModeRead,
            IoRequestWire::CredentialModeWrite { mode } => Self::CredentialModeWrite { mode },
            IoRequestWire::OAuth { ask, principal } => Self::OAuth { ask, principal },
            IoRequestWire::Deliver { envelope, key, site } => Self::Deliver {
                envelope,
                key,
                site: CallSite::from(site),
            },
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
            IoRequest::CommitRows { rows } => Self::CommitRows { rows: rows.clone() },
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
            IoRequest::Substrate { op, args, key, site } => Self::Substrate {
                op: op.clone(),
                args: args.clone(),
                key: key.clone(),
                site: site.as_ref().map(CallSiteWire::from),
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
                ..
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
            IoRequest::Prompt { text, schema, key } => Self::Prompt {
                text: text.clone(),
                schema: schema.clone(),
                key: key.clone(),
            },
            IoRequest::Embed { texts, key } => Self::Embed {
                texts: texts.clone(),
                key: key.clone(),
            },
            IoRequest::Tab { ask, key } => Self::Tab {
                ask: ask.clone(),
                key: key.clone(),
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
            IoRequest::Clock => Self::Clock,
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
            IoRequest::CredentialStoreList => Self::CredentialStoreList,
            IoRequest::CredentialStoreRead { name } => Self::CredentialStoreRead { name: name.clone() },
            IoRequest::CredentialStoreWrite { service, name, value } => Self::CredentialStoreWrite {
                service: service.clone(),
                name: name.clone(),
                value: SecretText::new(value.clone()),
            },
            IoRequest::CredentialStoreRemove { name } => Self::CredentialStoreRemove { name: name.clone() },
            IoRequest::CredentialStoreRotate => Self::CredentialStoreRotate,
            IoRequest::CredentialModeRead => Self::CredentialModeRead,
            IoRequest::CredentialModeWrite { mode } => Self::CredentialModeWrite { mode: mode.clone() },
            IoRequest::OAuth { ask, principal } => Self::OAuth { ask: ask.clone(), principal: principal.clone() },
            IoRequest::Deliver { envelope, key, site } => Self::Deliver {
                envelope: envelope.clone(),
                key: key.clone(),
                site: CallSiteWire::from(site),
            },
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
            IoAnswer::Committed(r) => Self::Committed {
                result: r.clone().into(),
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
            IoAnswer::Prompt(r) => Self::Prompt {
                result: r.as_ref().map(Clone::clone).map_err(ModelFailureWire::from).into(),
            },
            IoAnswer::Embed(r) => Self::Embed {
                result: r.as_ref().map(Clone::clone).map_err(ModelFailureWire::from).into(),
            },
            IoAnswer::Tab(r) => Self::Tab {
                result: r.clone().into(),
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
                    .map(|bytes| orbital_core_base::codec::base64_encode(bytes))
                    .map_err(Clone::clone)
                    .into(),
            },
            IoAnswer::Clock(r) => Self::Clock { result: r.clone().into() },
            IoAnswer::Os(r) => Self::Os { result: r.clone().into() },
            IoAnswer::CredentialStatus(r) => Self::CredentialStatus {
                result: r.clone().into(),
            },
            IoAnswer::CredentialForget(r) => Self::CredentialForget {
                result: r.clone().into(),
            },
            IoAnswer::CredentialStoreList(r) => Self::CredentialStoreList { result: r.clone().into() },
            IoAnswer::CredentialStoreRead(r) => Self::CredentialStoreRead {
                result: r.clone().map(|value| value.map(SecretText::new)).into(),
            },
            IoAnswer::CredentialStoreWrite(r) => Self::CredentialStoreWrite { result: r.clone().into() },
            IoAnswer::CredentialStoreRemove(r) => Self::CredentialStoreRemove { result: r.clone().into() },
            IoAnswer::CredentialStoreRotate(r) => Self::CredentialStoreRotate { result: r.clone().into() },
            IoAnswer::CredentialModeRead(r) => Self::CredentialModeRead { result: r.clone().into() },
            IoAnswer::CredentialModeWrite(r) => Self::CredentialModeWrite { result: r.clone().into() },
            IoAnswer::OAuth(r) => Self::OAuth { result: r.clone().into() },
            IoAnswer::Deliver(r) => Self::Deliver { result: r.clone().into() },
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
            IoAnswerWire::Committed { result } => Self::Committed(result.into()),
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
            IoAnswerWire::Prompt { result } => {
                let result: Result<String, ModelFailureWire> = result.into();
                Self::Prompt(result.map_err(ModelFailure::from))
            }
            IoAnswerWire::Embed { result } => {
                let result: Result<Vec<Vec<f64>>, ModelFailureWire> = result.into();
                Self::Embed(result.map_err(ModelFailure::from))
            }
            IoAnswerWire::Tab { result } => Self::Tab(result.into()),
            IoAnswerWire::Secret { result } => {
                let result: Result<Option<SecretText>, String> = result.into();
                Self::Secret(result.map(|value| value.map(SecretText::into_inner)))
            }
            IoAnswerWire::SecretSet { result } => Self::SecretSet(result.into()),
            IoAnswerWire::SecretRotate { result } => Self::SecretRotate(result.into()),
            IoAnswerWire::Random { result } => {
                let result: Result<String, String> = result.into();
                Self::Random(result.and_then(|text| {
                    orbital_core_base::codec::base64_decode(&text).map_err(|e| format!("random: not base64: {e}"))
                }))
            }
            IoAnswerWire::Clock { result } => Self::Clock(result.into()),
            IoAnswerWire::Os { result } => Self::Os(result.into()),
            IoAnswerWire::CredentialStatus { result } => Self::CredentialStatus(result.into()),
            IoAnswerWire::CredentialForget { result } => Self::CredentialForget(result.into()),
            IoAnswerWire::CredentialStoreList { result } => Self::CredentialStoreList(result.into()),
            IoAnswerWire::CredentialStoreRead { result } => {
                let result: Result<Option<SecretText>, String> = result.into();
                Self::CredentialStoreRead(result.map(|value| value.map(SecretText::into_inner)))
            }
            IoAnswerWire::CredentialStoreWrite { result } => Self::CredentialStoreWrite(result.into()),
            IoAnswerWire::CredentialStoreRemove { result } => Self::CredentialStoreRemove(result.into()),
            IoAnswerWire::CredentialStoreRotate { result } => Self::CredentialStoreRotate(result.into()),
            IoAnswerWire::CredentialModeRead { result } => Self::CredentialModeRead(result.into()),
            IoAnswerWire::CredentialModeWrite { result } => Self::CredentialModeWrite(result.into()),
            IoAnswerWire::OAuth { result } => Self::OAuth(result.into()),
            IoAnswerWire::Deliver { result } => Self::Deliver(result.into()),
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
