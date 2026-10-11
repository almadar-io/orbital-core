//! Runtime kernel configuration.

use crate::effects::{EffectDelegate, RuntimeEnvironment};
use std::sync::Arc;

/// Configuration for the runtime kernel.
#[derive(Clone)]
pub struct KernelConfig {
    /// Runtime environment.
    pub environment: RuntimeEnvironment,

    /// Maximum entities per collection (server-only hint).
    pub max_entities_per_collection: usize,

    /// Enable event history tracking.
    pub enable_event_history: bool,

    /// Maximum event history size.
    pub max_event_history: usize,

    /// Cross-environment effect delegate, threaded into the kernel's
    /// `EffectExecutor` at construction (`RuntimeKernel::new`, BEFORE the
    /// `Arc` wrap — `with_delegate` is a consuming builder). Every client-role
    /// kernel is configured with the ONE `runtime::server_leg::
    /// ServerLegCollector`; `None` (the default) for a Server-env kernel,
    /// which never delegates.
    pub delegate: Option<Arc<dyn EffectDelegate>>,
}

impl std::fmt::Debug for KernelConfig {
    // Manual impl: `Arc<dyn EffectDelegate>` isn't `Debug` (no supertrait
    // bound on the trait object), so `#[derive(Debug)]` no longer applies
    // once `delegate` was added.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelConfig")
            .field("environment", &self.environment)
            .field(
                "max_entities_per_collection",
                &self.max_entities_per_collection,
            )
            .field("enable_event_history", &self.enable_event_history)
            .field("max_event_history", &self.max_event_history)
            .field("delegate", &self.delegate.is_some())
            .finish()
    }
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            environment: RuntimeEnvironment::Server,
            max_entities_per_collection: 10000,
            enable_event_history: true,
            max_event_history: 1000,
            delegate: None,
        }
    }
}

impl KernelConfig {
    /// Create a config for the given environment.
    pub fn for_environment(environment: RuntimeEnvironment) -> Self {
        Self {
            environment,
            ..Default::default()
        }
    }
}
