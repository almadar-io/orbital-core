//! The one native host of a server-role kernel: every ask a dispatch makes, answered in process. Rows come from the
//! entity source, `http` and `os/` under the program's declared egress and grants, credentials, OAuth, random and clock
//! from the secret source and the operating system. Each piece is the existing answerer (`BlockingHost`, `HttpHost`,
//! `answer_os`); this owns them so a host can share one answerer across threads and rebuild it when a program registers.

use crate::effects::executor::{ServiceCaller, ServiceHandler};
use crate::entity_source::EntitySource;
use crate::http_service::{SecretSource, Transport};
use crate::os_host::{OsGrants, OsHandles, OsHost};
use crate::resumable::{BlockingHost, CallSite, HttpHost, IoAnswer, IoRequest};
use orbital_core_base::value::Value;
use std::sync::Arc;

/// Receives each event of a stream, with the call site whose `onMessage` route it belongs to.
pub type StreamEvents = dyn Fn(Option<&CallSite>, Value) + Send + Sync;

/// The refusal every `call-service` gets from a native host: it has no provider for any service (hosting and the registry
/// are `@almadar-io/hosting`'s, answered by the JS host; see G-CROSS-170). The wording is the JS host's `NOT_CONFIGURED`
/// integration error, so a program sees one refusal on every host.
struct NoProviders;

impl ServiceHandler for NoProviders {
    fn call(&self, service: &str, method: &str, _: &Value, _: Option<&str>, _: Option<&ServiceCaller>) -> Result<Value, String> {
        Err(format!("Integration not configured: {service}.{method}"))
    }

    fn cancel(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

pub struct NativeHost {
    source: Arc<dyn EntitySource>,
    transport: Arc<dyn Transport>,
    secrets: Arc<dyn SecretSource>,
    egress: Vec<String>,
    grants: OsGrants,
    handles: Arc<OsHandles>,
    on_event: Arc<StreamEvents>,
}

impl NativeHost {
    pub fn new(
        source: Arc<dyn EntitySource>,
        transport: Arc<dyn Transport>,
        secrets: Arc<dyn SecretSource>,
        egress: Vec<String>,
        grants: OsGrants,
        handles: Arc<OsHandles>,
        on_event: Arc<StreamEvents>,
    ) -> Self {
        Self {
            source,
            transport,
            secrets,
            egress,
            grants,
            handles,
            on_event,
        }
    }

    pub fn answer(&self, request: &IoRequest) -> IoAnswer {
        let on_event = |site: Option<&CallSite>, event: Value| (self.on_event)(site, event);
        let os = OsHost {
            grants: &self.grants,
            handles: &self.handles,
            on_event: &on_event,
        };
        let http = HttpHost {
            transport: self.transport.as_ref(),
            secrets: self.secrets.as_ref(),
            egress: &self.egress,
            on_event: &on_event,
            os: Some(&os),
        };
        BlockingHost {
            source: self.source.as_ref(),
            service: Some(&NoProviders),
            substrate: None,
        }
        .answer_with_http(&http, request)
    }
}
