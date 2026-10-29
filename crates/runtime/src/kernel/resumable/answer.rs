//! Answering asks and driving a dispatch on blocking and concurrent hosts.

use super::*;

/// `bytes` of operating-system CSPRNG output; wasm hosts answer `Random` themselves.
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub(super) fn os_random(bytes: u32) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; bytes as usize];
    getrandom::getrandom(&mut buf).map_err(|e| format!("random: {e}"))?;
    Ok(buf)
}

#[cfg(any(target_arch = "wasm32", not(feature = "std")))]
pub(super) fn os_random(_bytes: u32) -> Result<Vec<u8>, String> {
    Err("random: the host must answer Random".to_string())
}

/// `cancel-call` of a deferred keyed `os/` call: its pending completion is dropped, so its outcome never fires a route.
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub(crate) fn os_completion_cancelled(host: &PortHost<'_>, key: &str) -> bool {
    host.os.is_some_and(|os| os.handles.cancel_completion(key))
}

#[cfg(any(target_arch = "wasm32", not(feature = "std")))]
pub(crate) fn os_completion_cancelled(_host: &PortHost<'_>, _key: &str) -> bool {
    false
}

/// An [`IoRequest::Os`] answered by the native answerer; wasm hosts answer `Os` themselves.
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub(super) fn answer_os_host(host: Option<&PortHost<'_>>, request: &IoRequest) -> IoAnswer {
    crate::os_host::answer_os(host.and_then(|h| h.os), request)
}

#[cfg(any(target_arch = "wasm32", not(feature = "std")))]
pub(super) fn answer_os_host(_host: Option<&PortHost<'_>>, _request: &IoRequest) -> IoAnswer {
    IoAnswer::Os(Err(OsError::new(OsErrorKind::Unsupported, "os: the host must answer Os")))
}

/// Answer a `Secret` / `SecretSet` / `Random` / `Os` / `CredentialStatus` / `CredentialForget` ask with `host`.
pub fn answer_ports(host: Option<&PortHost<'_>>, request: &IoRequest) -> IoAnswer {
    match (request, host) {
        (IoRequest::PageFacts { .. }, _) => IoAnswer::PageFacts(Ok(PageFacts {
            available: false,
            hostname: String::new(),
            referrer: String::new(),
            screen_width: None,
        })),
        (IoRequest::Prompt { .. }, _) => IoAnswer::Prompt(Err(ModelFailure::new(ModelFailureKind::Unconfigured, NO_PROMPT_HOST))),
        (IoRequest::Embed { .. }, _) => IoAnswer::Embed(Err(ModelFailure::new(ModelFailureKind::Unconfigured, NO_EMBED_HOST))),
        (IoRequest::Tab { .. }, _) => IoAnswer::Tab(Err(TabFailure::new(TabFailureKind::Unavailable, NO_TAB_HOST))),
        (IoRequest::Secret { name, principal }, Some(host)) => {
            IoAnswer::Secret(host.secrets.secret_of(name, principal.as_deref()))
        }
        (IoRequest::Secret { .. }, None) => IoAnswer::Secret(Err(NO_PORT_HOST.to_string())),
        (
            IoRequest::SecretSet {
                name,
                value,
                ttl_ms,
                principal,
            },
            Some(host),
        ) => IoAnswer::SecretSet(host.secrets.store_secret_of(name, value, *ttl_ms, principal.as_deref())),
        (IoRequest::SecretSet { .. }, None) => IoAnswer::SecretSet(Err(NO_PORT_HOST.to_string())),
        (IoRequest::SecretRotate { name, value, principal }, Some(host)) => {
            IoAnswer::SecretRotate(host.secrets.rotate_secret_of(name, value, principal.as_deref()))
        }
        (IoRequest::SecretRotate { .. }, None) => IoAnswer::SecretRotate(Err(NO_PORT_HOST.to_string())),
        (IoRequest::Random { bytes }, _) => IoAnswer::Random(os_random(*bytes)),
        (IoRequest::Clock, _) => IoAnswer::Clock(Ok(orbital_core_base::time_util::now_millis())),
        (IoRequest::Os { .. }, _) => answer_os_host(host, request),
        (IoRequest::CredentialStatus { name, principal }, Some(host)) => {
            IoAnswer::CredentialStatus(host.secrets.secret_of(name, principal.as_deref()).and_then(|value| {
                let connected = crate::secret_source::credential_status_connected(
                    value.as_deref(),
                    principal.as_deref(),
                );
                let label = if connected { host.secrets.label_of(name, principal.as_deref())? } else { None };
                Ok(crate::credential_admin::CredentialStanding { connected, label })
            }))
        }
        (IoRequest::CredentialForget { name, principal }, Some(host)) => {
            IoAnswer::CredentialForget(host.secrets.forget_of(name, principal.as_deref()))
        }
        (IoRequest::Deliver { envelope, .. }, _) => IoAnswer::Deliver(Err(DeliverRefusal {
            kind: DeliverRefusalKind::Down,
            id: envelope.id.clone(),
        })),
        (IoRequest::CredentialStatus { .. }, None) => IoAnswer::CredentialStatus(Err(NO_PORT_HOST.to_string())),
        (IoRequest::CredentialForget { .. }, None) => IoAnswer::CredentialForget(Err(NO_PORT_HOST.to_string())),
        (IoRequest::CredentialStoreList, Some(host)) => IoAnswer::CredentialStoreList(host.secrets.credential_store_list()),
        (IoRequest::CredentialStoreRead { name }, Some(host)) => {
            IoAnswer::CredentialStoreRead(host.secrets.credential_store_read(name))
        }
        (IoRequest::CredentialStoreWrite { service, name, value }, Some(host)) => {
            IoAnswer::CredentialStoreWrite(host.secrets.credential_store_write(service, name, value))
        }
        (IoRequest::CredentialStoreRemove { name }, Some(host)) => {
            IoAnswer::CredentialStoreRemove(host.secrets.credential_store_remove(name))
        }
        (IoRequest::CredentialStoreRotate, Some(host)) => IoAnswer::CredentialStoreRotate(host.secrets.credential_store_rotate()),
        (IoRequest::CredentialModeRead, Some(host)) => IoAnswer::CredentialModeRead(host.secrets.credential_mode()),
        (IoRequest::CredentialModeWrite { mode }, Some(host)) => {
            IoAnswer::CredentialModeWrite(host.secrets.set_credential_mode(mode))
        }
        (IoRequest::CredentialStoreList, None) => IoAnswer::CredentialStoreList(Err(NO_PORT_HOST.to_string())),
        (IoRequest::CredentialStoreRead { .. }, None) => IoAnswer::CredentialStoreRead(Err(NO_PORT_HOST.to_string())),
        (IoRequest::CredentialStoreWrite { .. }, None) => IoAnswer::CredentialStoreWrite(Err(NO_PORT_HOST.to_string())),
        (IoRequest::CredentialStoreRemove { .. }, None) => IoAnswer::CredentialStoreRemove(Err(NO_PORT_HOST.to_string())),
        (IoRequest::CredentialStoreRotate, None) => IoAnswer::CredentialStoreRotate(Err(NO_PORT_HOST.to_string())),
        (IoRequest::CredentialModeRead, None) => IoAnswer::CredentialModeRead(Err(NO_PORT_HOST.to_string())),
        (IoRequest::CredentialModeWrite { .. }, None) => IoAnswer::CredentialModeWrite(Err(NO_PORT_HOST.to_string())),
        _ => IoAnswer::Service(Err("not an HTTP request".to_string())),
    }
}

/// Answer each [`Progress::Await`] on the spot with `answer` until the dispatch is done: the adapter
/// that lets a blocking host run the resumable code path.
pub fn drive_blocking(
    kernel: &crate::kernel::RuntimeKernel,
    progress: Progress<DispatchOutcomes>,
    answer: &dyn Fn(&IoRequest) -> IoAnswer,
) -> Result<DispatchOutcomes, DispatchError> {
    drive_with(kernel, progress, |requests| {
        requests
            .iter()
            .map(|p| IoReply { id: p.id, answer: answer(&p.request) })
            .collect()
    })
}

#[cfg(feature = "std")]
/// [`drive_blocking`] for a host whose answerer is `Sync`: the keyed `call-service` requests of one step run on
/// scoped threads at the same time (EXEC-QUEUE-8) and every other request is answered in order on this thread.
pub fn drive_concurrent(
    kernel: &crate::kernel::RuntimeKernel,
    progress: Progress<DispatchOutcomes>,
    answer: &(dyn Fn(&IoRequest) -> IoAnswer + Sync),
) -> Result<DispatchOutcomes, DispatchError> {
    drive_with(kernel, progress, |requests| answer_batch(requests, answer))
}

#[cfg(feature = "std")]
/// Answer one step's pending requests: each keyed `call-service` on its own scoped thread, the rest in
/// order on the calling thread. The replies keep the order of `requests`; the kernel matches them by id.
pub fn answer_batch(
    requests: &[PendingRequest],
    answer: &(dyn Fn(&IoRequest) -> IoAnswer + Sync),
) -> Vec<IoReply> {
    let dispatch_id = crate::dispatch_trace::answer_dispatch_id();
    std::thread::scope(|scope| {
        let overlapped: Vec<_> = requests
            .iter()
            .map(|p| {
                matches!(p.request, IoRequest::CallService { key: Some(_), .. })
                    .then(|| scope.spawn(move || {
                        let _context = crate::dispatch_trace::AnswerContext::enter(dispatch_id);
                        answer(&p.request)
                    }))
            })
            .collect();
        requests
            .iter()
            .zip(overlapped)
            .map(|(p, handle)| IoReply {
                id: p.id,
                answer: match handle {
                    Some(handle) => handle
                        .join()
                        .unwrap_or_else(|_| IoAnswer::Service(Err("the call-service answerer panicked".to_string()))),
                    None => {
                        let started = std::time::Instant::now();
                        let answered = answer(&p.request);
                        tracing::debug!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, request = ?std::mem::discriminant(&p.request), ms = started.elapsed().as_millis() as u64, deferred = matches!(answered, IoAnswer::ServiceDeferred), "host ask answered");
                        answered
                    }
                },
            })
            .collect()
    })
}

pub(super) fn drive_with(
    kernel: &crate::kernel::RuntimeKernel,
    mut progress: Progress<DispatchOutcomes>,
    mut serve: impl FnMut(&[PendingRequest]) -> Vec<IoReply>,
) -> Result<DispatchOutcomes, DispatchError> {
    loop {
        match progress {
            Progress::Done(outcomes) => return Ok(outcomes),
            Progress::Await { continuation, requests } => {
                let context = crate::dispatch_trace::AnswerContext::enter(Some(continuation.dispatch_id()));
                let replies = serve(&requests);
                drop(context);
                progress = kernel.resume(continuation, replies)?;
            }
        }
    }
}

/// Check `replies` against `pending` and turn them into the exchanges the replayed step is served from.
pub(crate) fn accept_replies(
    pending: &[PendingRequest],
    replies: Vec<IoReply>,
    check: &dyn Fn(&IoRequest, IoAnswer) -> IoAnswer,
) -> Result<Vec<Exchange>, ResumeError> {
    let mut answers: Vec<Option<IoAnswer>> = vec![None; pending.len()];
    for reply in replies {
        let Some(at) = pending.iter().position(|p| p.id == reply.id) else {
            return Err(ResumeError::Unknown(reply.id));
        };
        if answers[at].is_some() {
            return Err(ResumeError::Duplicate(reply.id));
        }
        if !reply.answer.answers(&pending[at].request) {
            return Err(ResumeError::WrongKind(reply.id));
        }
        answers[at] = Some(check(&pending[at].request, reply.answer));
    }
    pending
        .iter()
        .zip(answers)
        .map(|(p, answer)| match answer {
            Some(answer) => Ok(Exchange::new(p.request.clone(), answer)),
            None => Err(ResumeError::Unanswered(p.id)),
        })
        .collect()
}


/// A compare-and-commit over `source` (EXEC-STORE-5): every row must still equal the version the kernel read, then the
/// puts and deletes apply as one atomic batch.
pub(super) fn commit_on(source: &dyn EntitySource, rows: &[RowCommit]) -> Result<bool, String> {
    for row in rows {
        let current = source.find(&row.entity_type, &row.id)?.map(|r| (*r).clone());
        if current != row.expected {
            return Ok(false);
        }
    }
    let ops: Vec<RowWriteOp> = rows
        .iter()
        .filter_map(|row| match &row.write {
            RowCommitWrite::Keep => None,
            RowCommitWrite::Put { row: data } => Some(RowWriteOp::Upsert { entity_type: row.entity_type.clone(), data: data.clone() }),
            RowCommitWrite::Delete => Some(RowWriteOp::Delete { entity_type: row.entity_type.clone(), id: row.id.clone(), guard: None }),
        })
        .collect();
    if ops.is_empty() {
        return Ok(true);
    }
    match source.write_batch(&RowWriteBatch { ops, on_failure: RowWriteFailureMode::Atomic }) {
        Some(Ok(outcomes)) if outcomes.iter().all(RowWriteOutcome::applied) => Ok(true),
        Some(Ok(outcomes)) => Err(format!("the commit did not apply: {outcomes:?}")),
        Some(Err(error)) => Err(error),
        None => Err("this source cannot commit rows atomically".to_string()),
    }
}
