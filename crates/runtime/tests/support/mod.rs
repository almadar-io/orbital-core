//! Builders shared by the runtime integration tests: one way to parse a test schema and one way to stand up a kernel over it.
#![allow(dead_code)]

use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::config::KernelConfig;
use orbital_core_runtime::effects::executor::EffectDelegate;
use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
use orbital_core_runtime::kernel::RuntimeKernel;
use orbital_core_runtime::ServerLegCollector;
use serde_json::{json, Value as Json};
use std::sync::Arc;

pub fn schema(value: Json) -> OrbitalSchema {
    serde_json::from_value(value).expect("test schema parses")
}

pub struct TestKernel {
    environment: RuntimeEnvironment,
    source: Arc<dyn EntitySource>,
    config: KernelConfig,
}

impl TestKernel {
    pub fn server() -> Self {
        Self {
            environment: RuntimeEnvironment::Server,
            source: Arc::new(InMemoryEntityCache::new()),
            config: KernelConfig::default(),
        }
    }

    /// The client role, with its server legs collected rather than posted.
    pub fn client() -> Self {
        Self {
            environment: RuntimeEnvironment::Client,
            source: Arc::new(InMemoryEntityCache::new()),
            config: KernelConfig {
                environment: RuntimeEnvironment::Client,
                delegate: Some(Arc::new(ServerLegCollector::new()) as Arc<dyn EffectDelegate>),
                ..KernelConfig::default()
            },
        }
    }

    pub fn source(mut self, source: Arc<dyn EntitySource>) -> Self {
        self.source = source;
        self
    }

    pub fn config(mut self, config: KernelConfig) -> Self {
        self.config = config;
        self
    }

    pub fn build(self, schema_json: Json) -> RuntimeKernel {
        self.register(schema(schema_json))
    }

    pub fn register(self, schema: OrbitalSchema) -> RuntimeKernel {
        let kernel = RuntimeKernel::new(self.environment, self.source, self.config);
        kernel.register_schema(schema).expect("test schema registers");
        kernel
    }
}

/// Signing a session in under test: a program fragment holding a `[runtime]` `[identity]` entity and a trait whose
/// `SIGN_IN` runs `jws/verify` over an unsigned emulator token (EXEC-IDENTITY-5) into `identity/sign-in`. The token
/// names any subject, and each claim named by `fields` becomes the identity row's field of that name.
pub mod identity {
    use super::*;
    use orbital_core_base::value::Value;

    pub const SIGNER: &str = "TestSigner";
    const EMULATOR_ISSUER: &str = "firebase-auth-emulator@example.com";

    pub fn program(fields: &[&str]) -> OrbitalSchema {
        let mut declared = vec![json!({ "name": "id", "type": "string" })];
        declared.extend(
            fields
                .iter()
                .map(|name| json!({ "name": name, "type": if *name == "profile" { "object" } else { "string" } })),
        );
        schema(json!({
            "name": "TestIdentity",
            "typeContractVersion": 1,
            "orbitals": [{
                "name": "TestIdentityOrbital",
                "entity": { "name": "TestPrincipal", "identity": true, "persistence": "runtime", "fields": declared },
                "traits": [{
                    "name": SIGNER, "linkedEntity": "TestPrincipal",
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "SIGN_IN", "effects": [
                            ["let", [["v", ["jws/verify", "@payload.token", [],
                                { "issuer": EMULATOR_ISSUER, "audience": "test", "allowUnsigned": true }]]],
                                ["identity/sign-in", "@v"]]] },
                            { "from": "idle", "to": "idle", "event": "SIGN_OUT", "effects": [["identity/sign-out"]] }] }
                }]
            }]
        }))
    }

    pub fn token(subject: &str, claims: Json) -> String {
        let header = orbital_core_base::codec::base64url_encode(br#"{"alg":"none","typ":"JWT"}"#);
        let mut payload = json!({
            "iss": EMULATOR_ISSUER, "aud": "test", "sub": subject, "exp": 4_000_000_000u64,
        });
        if let (Some(base), Json::Object(extra)) = (payload.as_object_mut(), claims) {
            base.extend(extra);
        }
        format!("{header}.{}.", orbital_core_base::codec::base64url_encode(payload.to_string().as_bytes()))
    }

    /// Register the sign-in program on `kernel`.
    pub fn enable(kernel: &RuntimeKernel, fields: &[&str]) {
        if kernel.trait_dispatch_mode(SIGNER).is_some() {
            return;
        }
        kernel.register_schema(program(fields)).expect("the sign-in program registers");
    }

    /// Sign the session this thread is in as `subject`, its claims carrying the identity row's fields.
    pub fn sign_in(kernel: &RuntimeKernel, subject: &str, claims: Json) {
        let payload: Value = serde_json::from_value(json!({ "token": token(subject, claims) })).expect("payload");
        kernel.dispatch(SIGNER, None, "SIGN_IN", payload).expect("the sign-in dispatches");
    }

    /// [`sign_in`] in the session `client`.
    pub fn sign_in_as(kernel: &RuntimeKernel, client: &str, subject: &str, claims: Json) {
        let _session = kernel.enter_session(client);
        sign_in(kernel, subject, claims);
    }

    /// Sign the client session `client` of an embedded server in as `subject`; the server must have run [`enable`] on its kernel. Answers the sign-in's response.
    pub fn sign_in_server(
        server: &orbital_core_runtime::embed::EmbeddedServer,
        client: &str,
        subject: &str,
        claims: Json,
    ) -> orbital_core_runtime::effects::response::EventResponse {
        signer_request(server, client, "SIGN_IN", Some(json!({ "token": token(subject, claims) })))
    }

    /// Sign the client session `client` of an embedded server out; answers the response.
    pub fn sign_out_server(
        server: &orbital_core_runtime::embed::EmbeddedServer,
        client: &str,
    ) -> orbital_core_runtime::effects::response::EventResponse {
        signer_request(server, client, "SIGN_OUT", None)
    }

    fn signer_request(
        server: &orbital_core_runtime::embed::EmbeddedServer,
        client: &str,
        event: &str,
        payload: Option<Json>,
    ) -> orbital_core_runtime::effects::response::EventResponse {
        use orbital_core_runtime::effects::response::EventRequest;
        use orbital_core_runtime::embed::ServerProgress;
        let request = EventRequest {
            event: event.to_string(),
            target_trait: Some(SIGNER.to_string()),
            client_id: Some(client.to_string()),
            payload: payload.map(|payload| serde_json::from_value(payload).expect("payload")),
            ..Default::default()
        };
        match server.start("TestIdentityOrbital", request).expect("the identity request starts") {
            ServerProgress::Done(response) => response,
            _ => panic!("the identity request never asks the host"),
        }
    }
}
