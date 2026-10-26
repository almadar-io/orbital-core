//! Building an executor: its environment, delegate and handlers.

use super::*;

impl EffectExecutor {
    /// Create a new effect executor for the given environment
    pub fn new(environment: RuntimeEnvironment) -> Self {
        Self {
            environment,
            event_bus: None,
            persistence: None,
            fetch_handler: None,
            stream_fetch_handler: None,
            service_handler: None,
            call_canceller: None,
            substrate_handler: None,
            render_handler: None,
            navigation_handler: None,
            game_handler: None,
            delegate: None,
            entity_policies: None,
        }
    }

    /// Create a server-side executor
    pub fn server() -> Self {
        Self::new(RuntimeEnvironment::Server)
    }

    /// Create a client-side executor
    pub fn client() -> Self {
        Self::new(RuntimeEnvironment::Client)
    }

    /// Set the event bus
    pub fn with_event_bus(mut self, bus: SharedEventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }

    /// Set the persistence handler
    pub fn with_persistence(mut self, handler: Arc<dyn PersistenceHandler>) -> Self {
        self.persistence = Some(handler);
        self
    }

    /// Set the fetch handler
    pub fn with_fetch_handler(mut self, handler: Arc<dyn FetchHandler>) -> Self {
        self.fetch_handler = Some(handler);
        self
    }

    /// Set the streaming fetch handler
    pub fn with_stream_fetch_handler(mut self, handler: Arc<dyn StreamFetchHandler>) -> Self {
        self.stream_fetch_handler = Some(handler);
        self
    }

    /// Set the service handler
    pub fn with_substrate_handler(mut self, handler: Arc<dyn SubstrateHandler>) -> Self {
        self.substrate_handler = Some(handler);
        self
    }

    pub fn with_service_handler(mut self, handler: Arc<dyn ServiceHandler>) -> Self {
        self.service_handler = Some(handler);
        self
    }

    /// Set where `cancel-call` goes when no service handler is set.
    pub fn with_call_canceller(mut self, canceller: Arc<dyn ServiceHandler>) -> Self {
        self.call_canceller = Some(canceller);
        self
    }

    /// Set the render handler
    pub fn with_render_handler(mut self, handler: Arc<dyn RenderHandler>) -> Self {
        self.render_handler = Some(handler);
        self
    }

    /// Set the navigation handler
    pub fn with_navigation_handler(mut self, handler: Arc<dyn NavigationHandler>) -> Self {
        self.navigation_handler = Some(handler);
        self
    }

    /// Set the game entity handler
    pub fn with_game_handler(mut self, handler: Arc<dyn GameEntityHandler>) -> Self {
        self.game_handler = Some(handler);
        self
    }

    /// Set the delegate for cross-environment effects
    pub fn with_delegate(mut self, delegate: Arc<dyn EffectDelegate>) -> Self {
        self.delegate = Some(delegate);
        self
    }

    /// Set the schema-derived entity access policy table, shared with the
    /// kernel's `FetchHandler` so both the read and write sides check the
    /// same declared directives.
    pub fn with_entity_policies(mut self, table: Arc<RwLock<EntityAccessTable>>) -> Self {
        self.entity_policies = Some(table);
        self
    }

    /// Get the current environment
    pub fn environment(&self) -> RuntimeEnvironment {
        self.environment
    }

    /// The configured cross-environment delegate, if any. `RuntimeKernel::
    /// dispatch_with_server_leg` downcasts this (via `EffectDelegate::as_any`)
    /// to the concrete `ServerLegCollector` every client-role kernel installs.
    pub fn delegate(&self) -> Option<&Arc<dyn EffectDelegate>> {
        self.delegate.as_ref()
    }
}
