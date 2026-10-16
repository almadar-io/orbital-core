//! The path memory of one dispatch (Runtime Spec Clause 5.5); twin of `almadar-runtime` `DispatchMemory`.

use crate::effects::response::DispatchLog;
use orbital_core_eval::evaluator::context::BindingContext;
use orbital_core_base::value::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// What a guard or effect sees of the dispatch: the current delivery and this trait's log so far.
#[derive(Debug, Clone, Default)]
pub struct DispatchView {
    pub event: Arc<Value>,
    pub prev_events: Arc<Vec<Value>>,
    pub prev_states: Arc<Vec<String>>,
}

impl DispatchView {
    pub fn apply(&self, ctx: &mut BindingContext) {
        ctx.event = Arc::clone(&self.event);
        ctx.prev_events = Arc::clone(&self.prev_events);
        ctx.prev_states = Arc::clone(&self.prev_states);
    }
}

#[derive(Debug, Default)]
pub struct DispatchMemory {
    deliveries: HashMap<String, Arc<Vec<Value>>>,
    left: HashMap<String, Arc<Vec<String>>>,
}

impl DispatchMemory {
    pub fn view(&self, trait_name: &str, delivery: &Value) -> DispatchView {
        DispatchView {
            event: Arc::new(delivery.clone()),
            prev_events: self.deliveries.get(trait_name).cloned().unwrap_or_default(),
            prev_states: self.left.get(trait_name).cloned().unwrap_or_default(),
        }
    }

    /// Continue a dispatch that began elsewhere: the target trait's log so far.
    pub fn seed(&mut self, trait_name: &str, log: &DispatchLog) {
        self.deliveries.insert(
            trait_name.to_string(),
            Arc::new(log.prev_events.iter().map(|d| d.value()).collect()),
        );
        self.left
            .insert(trait_name.to_string(), Arc::new(log.prev_states.clone()));
    }

    pub fn record_delivery(&mut self, trait_name: &str, delivery: &Value) {
        Arc::make_mut(self.deliveries.entry(trait_name.to_string()).or_default())
            .push(delivery.clone());
    }

    pub fn record_transition(&mut self, trait_name: &str, from: &str, to: &str) {
        if from != to {
            Arc::make_mut(self.left.entry(trait_name.to_string()).or_default())
                .push(from.to_string());
        }
    }
}
