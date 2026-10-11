//! The path memory of one dispatch (Runtime Spec Clause 5.5); twin of `almadar-runtime` `DispatchMemory`.

use crate::effects::DispatchLog;
use crate::evaluator::{BindingContext, Value};
use std::collections::HashMap;

/// What a guard or effect sees of the dispatch: the current delivery and this trait's log so far.
#[derive(Debug, Clone, Default)]
pub struct DispatchView {
    pub event: Value,
    pub prev_events: Vec<Value>,
    pub prev_states: Vec<String>,
}

impl DispatchView {
    pub fn apply(&self, ctx: &mut BindingContext) {
        ctx.event = self.event.clone();
        ctx.prev_events = self.prev_events.clone();
        ctx.prev_states = self.prev_states.clone();
    }
}

#[derive(Debug, Default)]
pub struct DispatchMemory {
    deliveries: HashMap<String, Vec<Value>>,
    left: HashMap<String, Vec<String>>,
}

impl DispatchMemory {
    pub fn view(&self, trait_name: &str, delivery: &Value) -> DispatchView {
        DispatchView {
            event: delivery.clone(),
            prev_events: self.deliveries.get(trait_name).cloned().unwrap_or_default(),
            prev_states: self.left.get(trait_name).cloned().unwrap_or_default(),
        }
    }

    /// Continue a dispatch that began elsewhere: the target trait's log so far.
    pub fn seed(&mut self, trait_name: &str, log: &DispatchLog) {
        self.deliveries.insert(
            trait_name.to_string(),
            log.prev_events.iter().map(|d| d.value()).collect(),
        );
        self.left
            .insert(trait_name.to_string(), log.prev_states.clone());
    }

    pub fn record_delivery(&mut self, trait_name: &str, delivery: &Value) {
        self.deliveries
            .entry(trait_name.to_string())
            .or_default()
            .push(delivery.clone());
    }

    pub fn record_transition(&mut self, trait_name: &str, from: &str, to: &str) {
        if from != to {
            self.left
                .entry(trait_name.to_string())
                .or_default()
                .push(from.to_string());
        }
    }
}
