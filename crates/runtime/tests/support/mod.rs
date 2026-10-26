//! Builders shared by the runtime integration tests: one way to parse a test schema and one way to stand up a kernel over it.
#![allow(dead_code)]

use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::config::KernelConfig;
use orbital_core_runtime::effects::executor::EffectDelegate;
use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
use orbital_core_runtime::kernel::RuntimeKernel;
use orbital_core_runtime::ServerLegCollector;
use serde_json::Value as Json;
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
        let kernel = RuntimeKernel::new(self.environment, self.source, self.config);
        kernel.register_schema(schema(schema_json)).expect("test schema registers");
        kernel
    }
}
