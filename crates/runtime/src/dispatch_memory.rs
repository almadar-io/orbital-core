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

    /// Takes the step's view so its share of the log is released before the append; a live view would make
    /// `make_mut` deep-copy the whole log on every step.
    pub fn record_delivery(&mut self, trait_name: &str, delivery: &Value, view: DispatchView) {
        drop(view);
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

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::schema::sexpr::SExpression;

    #[test]
    fn recording_a_delivery_never_deep_copies_the_log_a_view_shares() {
        let body = Arc::new(SExpression::String("x".to_string()));
        let held = Value::Lambda {
            params: vec!["x".to_string()],
            body: Arc::clone(&body),
        };
        let mut memory = DispatchMemory::default();
        let first = memory.view("T", &held);
        memory.record_delivery("T", &held, first);
        assert_eq!(Arc::strong_count(&body), 3);
        let view = memory.view("T", &Value::Null);
        assert_eq!(view.prev_events.len(), 1);
        memory.record_delivery("T", &Value::Null, view);
        assert_eq!(Arc::strong_count(&body), 3);
        assert_eq!(memory.view("T", &Value::Null).prev_events.len(), 2);
    }

    #[test]
    fn control_a_view_kept_past_the_record_forces_a_copy_of_the_log() {
        let body = Arc::new(SExpression::String("x".to_string()));
        let held = Value::Lambda {
            params: vec!["x".to_string()],
            body: Arc::clone(&body),
        };
        let mut memory = DispatchMemory::default();
        let first = memory.view("T", &held);
        memory.record_delivery("T", &held, first);
        let view = memory.view("T", &Value::Null);
        let kept = view.clone();
        memory.record_delivery("T", &Value::Null, view);
        assert_eq!(Arc::strong_count(&body), 4);
        assert_eq!(kept.prev_events.len(), 1);
    }
}
