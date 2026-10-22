//! Cross-orbital event listener wiring for the runtime kernel.

use orbital_core_base::bus_event::BusEventSource;
use orbital_core_base::value::ValueMap;
use orbital_core_base::schema::types::ListenSource;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_eval::evaluator::context::BindingContextBuilder;
use orbital_core_eval::evaluator::Evaluator;
use orbital_core_base::schema::types::ListenDefinition;
use orbital_core_base::events::bus::SharedEventBus;
use orbital_core_base::events::types::SubscriptionId;
use orbital_core_base::value::Value;
use parking_lot::RwLock;
use std::collections::HashMap;
use tracing::debug;

/// Registered listener information.
#[derive(Debug, Clone)]
pub struct ListenerInfo {
    /// Source event pattern.
    pub event_pattern: String,

    /// Target trait name.
    pub trait_name: String,

    /// The orbital that owns the listening trait: a name-only `trait` source hears only this orbital.
    pub orbital: String,

    /// Internal event to trigger.
    pub triggers: String,

    /// Source-trait scope: which emitter this listener reacts to. `None`/`Any`
    /// matches any source; `Trait`/`Orbital` matches only that emitting trait.
    pub source: Option<ListenSource>,

    /// Optional payload remap applied to the source event's payload.
    pub payload_mapping: Option<orbital_core_base::schema::sexpr::SObject>,

    /// Optional guard gating whether the listener fires.
    pub guard: Option<SExpression>,

    /// Subscription ID for unsubscribing.
    pub subscription_id: SubscriptionId,
}

impl ListenerInfo {
    /// Whether this listener's declared source matches an event stamped `source`. Id-first: a source that
    /// carries the ids for its scope compares ids only (rename-proof, and an id on one side only is a
    /// mismatch); otherwise names, where a name-only `trait` source is confined to the listener's own orbital
    /// and an `orbital` source names the emitter's orbital. JS twin: `buildSourceMatcher`.
    pub fn matches_source(&self, source: &BusEventSource) -> bool {
        matches_listen_source(self.source.as_ref(), &self.orbital, source)
    }
}

/// Callback invoked when a listener triggers an internal event.
pub type TriggerCallback = Arc<dyn Fn(&str, &str, Value) + Send + Sync>;

use std::sync::Arc;

/// Manager for cross-orbital event listeners.
pub struct ListenerManager {
    event_bus: SharedEventBus,
    listeners: RwLock<HashMap<String, Vec<ListenerInfo>>>,
    /// Trait names in first-registration order: fan-out follows declaration order on every path.
    order: RwLock<Vec<String>>,
    trigger_callback: Option<TriggerCallback>,
}

impl std::fmt::Debug for ListenerManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerManager")
            .field("event_bus", &self.event_bus)
            .field("listeners", &self.listeners)
            .field("listener_count", &self.listener_count())
            .finish()
    }
}

impl ListenerManager {
    /// Create a new listener manager attached to the given event bus.
    pub fn new(event_bus: SharedEventBus) -> Self {
        Self {
            event_bus,
            listeners: RwLock::new(HashMap::new()),
            order: RwLock::new(Vec::new()),
            trigger_callback: None,
        }
    }

    /// Set the callback used to trigger internal events.
    pub fn with_trigger_callback(mut self, callback: TriggerCallback) -> Self {
        self.trigger_callback = Some(callback);
        self
    }

    /// Register a listener from a `ListenDefinition`.
    pub fn register(
        &self,
        trait_name: &str,
        orbital: &str,
        listen: &ListenDefinition,
    ) -> Result<SubscriptionId, String> {
        let (event_pattern, source) = match &listen.source {
            Some(source) => (listen.event.clone(), Some(source.clone())),
            None => orbital_core_base::schema::types::split_event_address(&listen.event),
        };
        let triggers = listen
            .triggers
            .clone()
            .unwrap_or_else(|| event_pattern.clone());
        let payload_mapping = listen.payload_mapping.clone();
        let guard = listen.guard.clone();
        let trait_name_owned = trait_name.to_string();

        let trigger_callback = self.trigger_callback.clone();
        let triggers_clone = triggers.clone();
        let trait_name_clone = trait_name_owned.clone();

        let subscription_id = self.event_bus.on(&event_pattern, move |event| {
            let evaluator = Evaluator::new();
            let ctx = BindingContextBuilder::new()
                .payload(event.payload.clone())
                .build();

            if let Some(ref guard_expr) = guard {
                match evaluator.evaluate_guard(guard_expr, &ctx) {
                    Ok(true) => {}
                    Ok(false) => {
                        debug!(
                            event = %event.name,
                            trait_name = %trait_name_clone,
                            "Listener guard blocked event"
                        );
                        return;
                    }
                    Err(_) => return,
                }
            }

            let mapped_payload = if let Some(ref mapping) = payload_mapping {
                let mut mapped = ValueMap::new();
                for (key, expr) in mapping {
                    if let Ok(value) = evaluator.evaluate(expr, &ctx) {
                        mapped.insert(key.clone(), value);
                    }
                }
                Value::Object(mapped)
            } else {
                event.payload.clone()
            };

            if let Some(ref callback) = trigger_callback {
                callback(&trait_name_clone, &triggers_clone, mapped_payload);
            }
        });

        let info = ListenerInfo {
            event_pattern: event_pattern.clone(),
            trait_name: trait_name_owned.clone(),
            orbital: orbital.to_string(),
            triggers,
            source,
            payload_mapping: listen.payload_mapping.clone(),
            guard: listen.guard.clone(),
            subscription_id,
        };

        let mut order = self.order.write();
        if !order.contains(&trait_name_owned) {
            order.push(trait_name_owned.clone());
        }
        self.listeners
            .write()
            .entry(trait_name_owned)
            .or_default()
            .push(info);

        Ok(subscription_id)
    }

    /// Register all listeners for a trait.
    pub fn register_all(
        &self,
        trait_name: &str,
        orbital: &str,
        listens: &[ListenDefinition],
    ) -> Vec<Result<SubscriptionId, String>> {
        listens
            .iter()
            .map(|listen| self.register(trait_name, orbital, listen))
            .collect()
    }

    /// Unregister all listeners for a trait.
    pub fn unregister_trait(&self, trait_name: &str) -> usize {
        self.order.write().retain(|name| name != trait_name);
        let mut listeners = self.listeners.write();
        if let Some(infos) = listeners.remove(trait_name) {
            let count = infos.len();
            for info in infos {
                self.event_bus.off(info.subscription_id);
            }
            count
        } else {
            0
        }
    }

    /// Get listener info for a trait.
    pub fn get_listeners(&self, trait_name: &str) -> Vec<ListenerInfo> {
        self.listeners
            .read()
            .get(trait_name)
            .cloned()
            .unwrap_or_default()
    }

    /// Get all registered listener info.
    pub fn all_listeners(&self) -> HashMap<String, Vec<ListenerInfo>> {
        self.listeners.read().clone()
    }

    /// Every listener that reacts to `event` emitted by `source_trait` — i.e. the
    /// listener's `event_pattern` matches `event` AND its source-scope matches
    /// `source_trait`. This is the synchronous fan-out query the kernel's cascade
    /// dispatch uses (the bus-subscription path needs a `trigger_callback`, which
    /// the wrapping client/server don't set).
    pub fn matching(&self, event: &str, source: &BusEventSource) -> Vec<ListenerInfo> {
        let listeners = self.listeners.read();
        self.order
            .read()
            .iter()
            .filter_map(|name| listeners.get(name))
            .flatten()
            .filter(|l| l.event_pattern == event && l.matches_source(source))
            .cloned()
            .collect()
    }

    /// Get the total count of registered listeners.
    pub fn listener_count(&self) -> usize {
        self.listeners.read().values().map(|v| v.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use {orbital_core_base::schema::types::ListenDefinition, orbital_core_base::schema::sexpr::SExpression};
    use {orbital_core_base::events::bus::shared_bus, orbital_core_base::events::types::Event, orbital_core_base::value::Value};

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_register_listener() {
        let bus = shared_bus();
        let manager = ListenerManager::new(bus.clone());

        let listen = ListenDefinition {
            event: "USER_REGISTERED".to_string(),
            triggers: Some("CREATE_PROFILE".to_string()),
            scope: None,
            description: None,
            synonyms: None,
            tier: None,
            payload_mapping: None,
            source: None,
            guard: None,
            event_id: None,
            triggers_id: None,
        };

        let result = manager.register("ProfileManagement", "Orb", &listen);
        assert!(result.is_ok());

        let listeners = manager.get_listeners("ProfileManagement");
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].event_pattern, "USER_REGISTERED");
        assert_eq!(listeners[0].triggers, "CREATE_PROFILE");
    }

    /// Fan-out order is declaration order (Runtime Spec Clause 5.5: the log is canonical across runtimes).
    #[test]
    fn matching_returns_listeners_in_registration_order() {
        let names = [
            "Once", "Twin", "All", "Prov", "Reenter", "Zeta", "Alpha", "Mid",
        ];
        let manager = ListenerManager::new(shared_bus());
        let listen = ListenDefinition {
            event: "X".to_string(),
            triggers: Some("GOT".to_string()),
            scope: None,
            description: None,
            synonyms: None,
            tier: None,
            payload_mapping: None,
            source: None,
            guard: None,
            event_id: None,
            triggers_id: None,
        };
        for name in names {
            manager.register(name, "Orb", &listen).unwrap();
        }
        for _ in 0..20 {
            let order: Vec<String> = manager
                .matching("X", &BusEventSource::from_trait("Src"))
                .into_iter()
                .map(|l| l.trait_name)
                .collect();
            assert_eq!(order, names);
        }
        manager.unregister_trait("Twin");
        manager.register("Twin", "Orb", &listen).unwrap();
        let order: Vec<String> = manager
            .matching("X", &BusEventSource::from_trait("Src"))
            .into_iter()
            .map(|l| l.trait_name)
            .collect();
        assert_eq!(
            order.last().map(String::as_str),
            Some("Twin"),
            "a re-registered trait goes to the end"
        );
    }

    #[test]
    fn test_listener_triggers_callback() {
        let bus = shared_bus();
        let triggered = Arc::new(AtomicUsize::new(0));
        let triggered_clone = triggered.clone();

        let manager = ListenerManager::new(bus.clone()).with_trigger_callback(Arc::new(
            move |trait_name, event, _payload| {
                assert_eq!(trait_name, "ProfileManagement");
                assert_eq!(event, "CREATE_PROFILE");
                triggered_clone.fetch_add(1, Ordering::SeqCst);
            },
        ));

        let listen = ListenDefinition {
            event: "USER_REGISTERED".to_string(),
            triggers: Some("CREATE_PROFILE".to_string()),
            scope: None,
            description: None,
            synonyms: None,
            tier: None,
            payload_mapping: None,
            source: None,
            guard: None,
            event_id: None,
            triggers_id: None,
        };

        manager
            .register("ProfileManagement", "Orb", &listen)
            .unwrap();

        bus.emit(Event::new("USER_REGISTERED", Value::Null));

        assert_eq!(triggered.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_listener_with_guard() {
        let bus = shared_bus();
        let triggered = Arc::new(AtomicUsize::new(0));
        let triggered_clone = triggered.clone();

        let manager =
            ListenerManager::new(bus.clone()).with_trigger_callback(Arc::new(move |_, _, _| {
                triggered_clone.fetch_add(1, Ordering::SeqCst);
            }));

        let listen = ListenDefinition {
            event: "USER_UPDATED".to_string(),
            triggers: Some("SYNC_PROFILE".to_string()),
            scope: None,
            source: None,
            description: None,
            synonyms: None,
            tier: None,
            payload_mapping: None,
            guard: Some(serde_json::from_str(r#"["=", "@payload.status", "active"]"#).unwrap()),
            event_id: None,
            triggers_id: None,
        };

        manager
            .register("ProfileManagement", "Orb", &listen)
            .unwrap();

        let mut payload1 = ValueMap::new();
        payload1.insert("status".to_string(), Value::String("pending".to_string()));
        bus.emit(Event::new("USER_UPDATED", Value::Object(payload1)));

        assert_eq!(triggered.load(Ordering::SeqCst), 0);

        let mut payload2 = ValueMap::new();
        payload2.insert("status".to_string(), Value::String("active".to_string()));
        bus.emit(Event::new("USER_UPDATED", Value::Object(payload2)));

        assert_eq!(triggered.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_listener_with_payload_mapping() {
        let bus = shared_bus();
        let received_payload = Arc::new(RwLock::new(Value::Null));
        let received_clone = received_payload.clone();

        let manager = ListenerManager::new(bus.clone()).with_trigger_callback(Arc::new(
            move |_, _, payload| {
                *received_clone.write() = payload;
            },
        ));

        let mut mapping = orbital_core_base::schema::sexpr::SObject::new();
        mapping.insert(
            "userId".to_string(),
            serde_json::from_str::<SExpression>(r#""@payload.id""#).unwrap(),
        );

        let listen = ListenDefinition {
            event: "USER_CREATED".to_string(),
            triggers: Some("INIT_PROFILE".to_string()),
            scope: None,
            description: None,
            synonyms: None,
            tier: None,
            payload_mapping: Some(mapping),
            source: None,
            guard: None,
            event_id: None,
            triggers_id: None,
        };

        manager
            .register("ProfileManagement", "Orb", &listen)
            .unwrap();

        let mut payload = ValueMap::new();
        payload.insert("id".to_string(), Value::String("user-123".to_string()));
        payload.insert("name".to_string(), Value::String("John".to_string()));
        bus.emit(Event::new("USER_CREATED", Value::Object(payload)));

        let received = received_payload.read();
        if let Value::Object(obj) = &*received {
            assert!(obj.contains_key("userId"));
        }
    }

    #[test]
    fn test_unregister_trait() {
        let bus = shared_bus();
        let manager = ListenerManager::new(bus.clone());

        let listen1 = ListenDefinition {
            event: "EVENT_1".to_string(),
            triggers: Some("TRIGGER_1".to_string()),
            scope: None,
            description: None,
            synonyms: None,
            tier: None,
            payload_mapping: None,
            source: None,
            guard: None,
            event_id: None,
            triggers_id: None,
        };

        let listen2 = ListenDefinition {
            event: "EVENT_2".to_string(),
            triggers: Some("TRIGGER_2".to_string()),
            scope: None,
            description: None,
            synonyms: None,
            tier: None,
            payload_mapping: None,
            source: None,
            guard: None,
            event_id: None,
            triggers_id: None,
        };

        manager.register("TestTrait", "Orb", &listen1).unwrap();
        manager.register("TestTrait", "Orb", &listen2).unwrap();

        assert_eq!(manager.listener_count(), 2);

        let removed = manager.unregister_trait("TestTrait");
        assert_eq!(removed, 2);
        assert_eq!(manager.listener_count(), 0);
    }

    #[test]
    fn test_register_all() {
        let bus = shared_bus();
        let manager = ListenerManager::new(bus.clone());

        let listens = vec![
            ListenDefinition {
                event: "EVENT_1".to_string(),
                triggers: Some("TRIGGER_1".to_string()),
                scope: None,
                source: None,
                description: None,
                synonyms: None,
                tier: None,
                payload_mapping: None,
                guard: None,
                event_id: None,
                triggers_id: None,
            },
            ListenDefinition {
                event: "EVENT_2".to_string(),
                triggers: Some("TRIGGER_2".to_string()),
                scope: None,
                source: None,
                description: None,
                synonyms: None,
                tier: None,
                payload_mapping: None,
                guard: None,
                event_id: None,
                triggers_id: None,
            },
        ];

        let results = manager.register_all("TestTrait", "Orb", &listens);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));
        assert_eq!(manager.listener_count(), 2);
    }
}

#[allow(unused_imports)]
pub use orbital_core_base::listen_match::{matches_listen_source};
