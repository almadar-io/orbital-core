//! The one native host of a server-role kernel: every ask a dispatch makes, answered in process. Rows come from the
//! entity source, `http` and `os/` under the program's declared egress and grants, credentials, OAuth, random and clock
//! from the secret source and the operating system. Each piece is the existing answerer (`BlockingHost`, `HttpHost`,
//! `answer_os`); this owns them so a host can share one answerer across threads and rebuild it when a program registers.

use crate::effects::executor::{ServiceCaller, ServiceHandler, SubstrateHandler};
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
    substrate: Option<Arc<dyn SubstrateHandler>>,
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
            substrate: None,
        }
    }

    /// Answer the substrate operators (`program/*`, `behavior/*`) through `handler`; without one they fail.
    pub fn with_substrate(mut self, handler: Option<Arc<dyn SubstrateHandler>>) -> Self {
        self.substrate = handler;
        self
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
            substrate: self.substrate.as_deref(),
        }
        .answer_with_http(&http, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::executor::SubstrateFailure;
    use crate::entity_source::InMemoryEntityCache;
    use crate::http_service::{HttpRequest, HttpResponse, TransportError};

    struct Echo;
    struct Offline;

    impl Transport for Offline {
        fn send(&self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
            unreachable!("a substrate ask sends no request")
        }
    }

    impl SubstrateHandler for Echo {
        fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
            Ok(Value::String(format!("{op}/{}", args.len())))
        }
    }

    fn host() -> NativeHost {
        NativeHost::new(
            Arc::new(InMemoryEntityCache::new()),
            Arc::new(Offline),
            Arc::new(crate::credentials::Credentials::from_env(Default::default())),
            Vec::new(),
            OsGrants::default(),
            Arc::new(OsHandles::default()),
            Arc::new(|_: Option<&CallSite>, _: Value| {}),
        )
    }

    fn ask(host: &NativeHost) -> IoAnswer {
        host.answer(&IoRequest::Substrate { op: "program/read".to_string(), args: vec![Value::Null] })
    }

    #[test]
    fn a_substrate_ask_reaches_the_handler_the_host_was_given() {
        match ask(&host().with_substrate(Some(Arc::new(Echo)))) {
            IoAnswer::Substrate(Ok(Value::String(answer))) => assert_eq!(answer, "program/read/1"),
            other => panic!("expected the handler's answer, got {other:?}"),
        }
    }

    #[test]
    fn control_a_host_without_a_handler_names_the_op_it_cannot_answer() {
        match ask(&host()) {
            IoAnswer::Substrate(Err(failure)) => assert_eq!(failure.message, "no substrate host configured for program/read"),
            other => panic!("expected the refusal, got {other:?}"),
        }
    }
}
