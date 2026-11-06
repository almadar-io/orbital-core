//! A click on a guest's painted control (G-CROSS-400): the opener paints a guest session's UI inside a `guest-view`
//! node `{ guest, deliver, paints, children }`. A host scopes each guest paint to the trait that drew it, as it scopes
//! its own, so a click names its painter: the opener's trait is dispatched `<Painter>.<EVENT>`. When a `guest-view` it
//! painted shows a paint of `<Painter>`, that is delivered to the trait as the view's `deliver` event with
//! `{ guest, address: "<Painter>.<EVENT>", event, payload }`, and the guest's own kernel runs it as any click.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::{RuntimeKernel, RUNTIME_SINGLETON};
use orbital_core_base::value::{Value, ValueMap};

const GUEST_VIEW: &str = "guest-view";

/// The first `guest-view` in `value` whose `paints` hold a paint of `painter`: its `(guest, deliver)`.
fn guest_view_painting(value: &Value, painter: &str) -> Option<(String, String)> {
    match value {
        Value::Object(node) => {
            if matches!(node.get("type"), Some(Value::String(t)) if t.as_str() == GUEST_VIEW) {
                let text = |key: &str| match node.get(key) {
                    Some(Value::String(s)) if !s.as_str().is_empty() => Some(s.as_str().to_string()),
                    _ => None,
                };
                let paints_painter = matches!(node.get("paints"), Some(Value::Array(paints)) if paints.iter().any(|paint| {
                    matches!(paint.get("trait"), Some(Value::String(t)) if t.as_str() == painter)
                }));
                return match (text("guest"), text("deliver"), paints_painter) {
                    (Some(guest), Some(deliver), true) => Some((guest, deliver)),
                    _ => None,
                };
            }
            node.values().find_map(|child| guest_view_painting(child, painter))
        }
        Value::Array(items) => items.iter().find_map(|item| guest_view_painting(item, painter)),
        _ => None,
    }
}

impl RuntimeKernel {
    /// The `deliver` event and payload a guest click `<Painter>.<EVENT>` becomes on `trait_name`, when a `guest-view`
    /// the trait shows holds a paint of `Painter`. An event naming no painter is the trait's own.
    pub(super) fn guest_control_delivery(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event: &str,
        payload: &Value,
    ) -> Option<(String, Value)> {
        let (painter, control) = event.split_once('.')?;
        if !self.renders.read().shows_guest_view(trait_name) {
            return None;
        }
        let held = self
            .renders
            .read()
            .shown(trait_name, entity_id.unwrap_or(RUNTIME_SINGLETON));
        let (guest, deliver) = held
            .iter()
            .find_map(|(_, content)| guest_view_painting(content, painter))?;
        let mut delivered = ValueMap::default();
        delivered.insert("guest".into(), Value::string(guest));
        delivered.insert("address".into(), Value::string(event.to_string()));
        delivered.insert("event".into(), Value::string(control.to_string()));
        delivered.insert("payload".into(), payload.clone());
        Some((deliver, Value::object(delivered)))
    }
}
