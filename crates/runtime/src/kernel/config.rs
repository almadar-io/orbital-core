//! Runtime kernel configuration.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::effects::executor::{EffectDelegate, ServiceHandler, SubstrateHandler};
use orbital_core_base::effect_types::RuntimeEnvironment;
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

    /// Runs `call-service`; `None` leaves every call a no-op.
    pub service_handler: Option<Arc<dyn ServiceHandler>>,

    /// Runs substrate operators (`program/*`, `behavior/*`, …); `None` fails them.
    pub substrate_handler: Option<Arc<dyn SubstrateHandler>>,

    /// The dispatch stamp source (ms-epoch): every dispatch's one `@now` reads it.
    /// `None` (the default) is the wall clock; a fixture pins it to replay a dispatch.
    pub clock: Option<DispatchClock>,

    /// The deployment unit this kernel runs (a key of the program's `deployment`). `None` is no placement:
    /// every `emit` stays on the local worklist.
    pub unit: Option<String>,

    /// The rows of `[runtime]` entities live in the kernel's own memory and are never a host ask (the server role
    /// that answers its I/O through `Progress::Await`). `false` (the default) leaves them to the entity source.
    pub runtime_rows_in_kernel: bool,
}

/// A host-supplied millisecond clock for dispatch stamps.
pub type DispatchClock = Arc<dyn Fn() -> f64 + Send + Sync>;

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
            .field("service_handler", &self.service_handler.is_some())
            .field("substrate_handler", &self.substrate_handler.is_some())
            .field("clock", &self.clock.is_some())
            .field("unit", &self.unit)
            .field("runtime_rows_in_kernel", &self.runtime_rows_in_kernel)
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
            service_handler: None,
            substrate_handler: None,
            clock: None,
            unit: None,
            runtime_rows_in_kernel: false,
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
