//! The path memory of one dispatch (Runtime Spec Clause 5.5); twin of `almadar-runtime` `DispatchMemory`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::effects::response::DispatchLog;
use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::context::BindingContext;
use orbital_core_eval::util::SharedLog;
use std::collections::HashMap;
use std::sync::Arc;

/// What a guard or effect sees of the dispatch: the current delivery and this trait's log so far. The log is a prefix
/// of the dispatch's one shared log, so every recorded step's view costs O(1) (G-CROSS-288).
#[derive(Debug, Clone, Default)]
pub struct DispatchView {
    pub event: Arc<Value>,
    pub prev_events: SharedLog<Arc<Value>>,
    pub prev_states: SharedLog<String>,
}

impl DispatchView {
    pub fn apply(&self, ctx: &mut BindingContext) {
        ctx.event = Arc::clone(&self.event);
        ctx.prev_events = self.prev_events.clone();
        ctx.prev_states = self.prev_states.clone();
    }
}

#[derive(Debug, Default)]
pub struct DispatchMemory {
    deliveries: HashMap<String, SharedLog<Arc<Value>>>,
    left: HashMap<String, SharedLog<String>>,
}

impl DispatchMemory {
    pub fn view(&self, trait_name: &str, delivery: &Arc<Value>) -> DispatchView {
        DispatchView {
            event: Arc::clone(delivery),
            prev_events: self.deliveries.get(trait_name).cloned().unwrap_or_default(),
            prev_states: self.left.get(trait_name).cloned().unwrap_or_default(),
        }
    }

    /// Continue a dispatch that began elsewhere: the target trait's log so far.
    pub fn seed(&mut self, trait_name: &str, log: &DispatchLog) {
        self.deliveries.insert(
            trait_name.to_string(),
            log.prev_events.iter().map(|d| Arc::new(d.value())).collect(),
        );
        self.left.insert(
            trait_name.to_string(),
            log.prev_states.iter().cloned().collect(),
        );
    }

    pub fn record_delivery(&mut self, trait_name: &str, delivery: &Arc<Value>, view: DispatchView) {
        drop(view);
        self.deliveries
            .entry(trait_name.to_string())
            .or_default()
            .push(Arc::clone(delivery));
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

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::schema::sexpr::SExpression;

    #[test]
    fn recording_a_delivery_never_deep_copies_the_log_a_view_shares() {
        let body = Arc::new(SExpression::String("x".to_string()));
        let held = Arc::new(Value::Lambda {
            params: vec!["x".to_string()],
            body: Arc::clone(&body),
        });
        let null = Arc::new(Value::Null);
        let mut memory = DispatchMemory::default();
        let first = memory.view("T", &held);
        memory.record_delivery("T", &held, first);
        assert_eq!(Arc::strong_count(&body), 2, "the view and the record share the one delivery");
        let view = memory.view("T", &null);
        assert_eq!(view.prev_events.len(), 1);
        memory.record_delivery("T", &null, view);
        assert_eq!(Arc::strong_count(&body), 2, "the view and the record share the one delivery");
        assert_eq!(memory.view("T", &null).prev_events.len(), 2);
    }

    /// A step's recorded view (`DeliveryEntry::view`) outlives the record: it keeps its own prefix and the append
    /// still copies nothing.
    #[test]
    fn a_view_kept_past_the_record_keeps_its_prefix_without_a_copy() {
        let body = Arc::new(SExpression::String("x".to_string()));
        let held = Arc::new(Value::Lambda {
            params: vec!["x".to_string()],
            body: Arc::clone(&body),
        });
        let null = Arc::new(Value::Null);
        let mut memory = DispatchMemory::default();
        let first = memory.view("T", &held);
        memory.record_delivery("T", &held, first);
        let view = memory.view("T", &null);
        let kept = view.clone();
        memory.record_delivery("T", &null, view);
        assert_eq!(Arc::strong_count(&body), 2, "the view and the record share the one delivery");
        assert_eq!(kept.prev_events.len(), 1);
        assert_eq!(memory.view("T", &null).prev_events.len(), 2);
    }
}
