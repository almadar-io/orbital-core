//! The hosts that answer asks in place: host-answered, blocking and HTTP.

use super::*;

/// Stands in for a store, service or substrate host that answers through [`Progress::Await`]: the
/// kernel is built with it where an async host has no synchronous handler. A call that reaches it
/// outside a resumable run fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostAnswered;

pub(super) const HOST_ANSWERED: &str = "this host answers through Progress::Await";

impl EntitySource for HostAnswered {
    fn read(&self, _: &str, _: &str) -> Result<Arc<Value>, String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn write(&self, _: &str, _: &str, _: &Value) -> Result<Value, String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn create(&self, _: &str, _: &Value) -> Result<Value, String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn delete(&self, _: &str, _: &str) -> Result<(), String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn clear(&self) {}
    fn count(&self) -> usize {
        0
    }
    fn list(&self, _: &str) -> Result<Vec<Arc<Value>>, String> {
        Err(HOST_ANSWERED.to_string())
    }
}

impl ServiceHandler for HostAnswered {
    fn call(
        &self,
        _: &str,
        _: &str,
        _: &Value,
        _: Option<&str>,
        _: Option<&ServiceCaller>,
    ) -> Result<Value, String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn cancel(&self, _: &str) -> Result<(), String> {
        Err(HOST_ANSWERED.to_string())
    }
}

impl SubstrateHandler for HostAnswered {
    fn run(&self, _: &str, _: &[Value]) -> Result<Value, SubstrateFailure> {
        Err(SubstrateFailure {
            message: HOST_ANSWERED.to_string(),
            errors: Vec::new(),
        })
    }
}

/// The synchronous handlers of a blocking host, as one answerer of [`IoRequest`]s.
pub struct BlockingHost<'a> {
    pub source: &'a dyn EntitySource,
    pub service: Option<&'a dyn ServiceHandler>,
    pub substrate: Option<&'a dyn SubstrateHandler>,
}

impl BlockingHost<'_> {
    pub fn answer(&self, request: &IoRequest) -> IoAnswer {
        match request {
            IoRequest::ReadRow { entity_type, id } => IoAnswer::Row(
                self.source
                    .find(entity_type, id)
                    .map(|row| row.map_or(Value::Null, |r| (*r).clone())),
            ),
            IoRequest::ListRows { entity_type } => IoAnswer::Rows(self.source.list(entity_type)),
            IoRequest::QueryRows {
                entity_type,
                query,
                project: RowProjection::Rows,
            } => match self.source.query_page(entity_type, query) {
                QueryAnswer::Rows(page) => IoAnswer::Page(Ok(page)),
                QueryAnswer::Declined(reason) => IoAnswer::QueryUnsupported(reason),
                QueryAnswer::Failed(message) => IoAnswer::Page(Err(message)),
            },
            IoRequest::QueryRows {
                entity_type,
                query,
                project: RowProjection::Ids,
            } => match self.source.query_ids(entity_type, query) {
                QueryAnswer::Rows(page) => IoAnswer::IdPage(Ok(page)),
                QueryAnswer::Declined(reason) => IoAnswer::QueryUnsupported(reason),
                QueryAnswer::Failed(message) => IoAnswer::IdPage(Err(message)),
            },
            IoRequest::CommitRows { rows } => IoAnswer::Committed(commit_on(self.source, rows)),
            IoRequest::WriteRows { batch } => match self.source.write_batch(batch) {
                Some(outcomes) => IoAnswer::Wrote(outcomes),
                None => {
                    IoAnswer::WriteUnsupported("the source has no conditional writes".to_string())
                }
            },
            IoRequest::CreateRow { entity_type, data } => {
                IoAnswer::Row(self.source.create(entity_type, data))
            }
            IoRequest::WriteRow {
                entity_type,
                id,
                data,
            } => IoAnswer::Row(self.source.write(entity_type, id, data)),
            IoRequest::DeleteRow { entity_type, id } => {
                IoAnswer::Deleted(self.source.delete(entity_type, id))
            }
            IoRequest::CallService {
                service,
                method,
                args,
                key,
                caller,
                ..
            } => IoAnswer::Service(match self.service {
                Some(handler) => {
                    handler.call(service, method, args, key.as_deref(), caller.as_ref())
                }
                None => Err("no service handler configured".to_string()),
            }),
            IoRequest::CancelCall { key } => IoAnswer::Cancelled(match self.service {
                Some(handler) => handler.cancel(key),
                None => Err("no service handler configured".to_string()),
            }),
            IoRequest::Substrate { op, args, .. } => IoAnswer::Substrate(match self.substrate {
                Some(handler) => handler.run(op, args),
                None => Err(SubstrateFailure {
                    message: format!("no substrate host configured for {op}"),
                    errors: Vec::new(),
                }),
            }),
            IoRequest::PageFacts { .. }
            | IoRequest::Prompt { .. }
            | IoRequest::Embed { .. }
            | IoRequest::Tab { .. }
            | IoRequest::Secret { .. }
            | IoRequest::SecretSet { .. }
            | IoRequest::SecretRotate { .. }
            | IoRequest::Random { .. }
            | IoRequest::Clock
            | IoRequest::Sealed { .. }
            | IoRequest::Os { .. }
            | IoRequest::CredentialStatus { .. }
            | IoRequest::CredentialForget { .. }
            | IoRequest::CredentialStoreList
            | IoRequest::CredentialStoreRead { .. }
            | IoRequest::CredentialStoreWrite { .. }
            | IoRequest::CredentialStoreRemove { .. }
            | IoRequest::CredentialStoreRotate
            | IoRequest::CredentialModeRead
            | IoRequest::CredentialModeWrite { .. }
            | IoRequest::Deliver { .. } => answer_ports(None, request),
        }
    }

    /// [`Self::answer`], with the host's own asks answered by `ports`.
    pub fn answer_with_ports(&self, ports: &PortHost<'_>, request: &IoRequest) -> IoAnswer {
        match request {
            IoRequest::PageFacts { .. }
            | IoRequest::Prompt { .. }
            | IoRequest::Embed { .. }
            | IoRequest::Tab { .. }
            | IoRequest::Secret { .. }
            | IoRequest::SecretSet { .. }
            | IoRequest::SecretRotate { .. }
            | IoRequest::Random { .. }
            | IoRequest::Clock
            | IoRequest::Sealed { .. }
            | IoRequest::Os { .. }
            | IoRequest::CredentialStatus { .. }
            | IoRequest::CredentialForget { .. }
            | IoRequest::CredentialStoreList
            | IoRequest::CredentialStoreRead { .. }
            | IoRequest::CredentialStoreWrite { .. }
            | IoRequest::CredentialStoreRemove { .. }
            | IoRequest::CredentialStoreRotate
            | IoRequest::CredentialModeRead
            | IoRequest::CredentialModeWrite { .. }
            | IoRequest::Deliver { .. } => answer_ports(Some(ports), request),
            IoRequest::CancelCall { key } if os_completion_cancelled(ports, key) => IoAnswer::Cancelled(Ok(())),
            other => self.answer(other),
        }
    }
}

/// What a blocking host holds to answer the kernel's secret, random, clock, credential and `os/` asks.
pub struct PortHost<'a> {
    pub secrets: &'a dyn SecretSource,
    /// Receives each event of a stream, with the call site whose `onMessage` route it belongs to.
    pub on_event: &'a dyn Fn(Option<&CallSite>, Value),
    /// The native `os/` answerer's grants and handles; `None` denies every `os/` ask.
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    pub os: Option<&'a crate::os_host::OsHost<'a>>,
}
