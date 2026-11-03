//! A click on a guest's painted control (G-CROSS-400): the opener paints a guest session's UI inside a `guest-view`
//! node `{ guest, deliver, source, children }`. A control event the opener's trait does not handle in its current state,
//! and that a control inside one of its painted `guest-view`s carries, is delivered to the trait as the view's `deliver`
//! event with `{ guest, address: "<source>.<EVENT>", event, payload }`. The trait's own transitions always win.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::{RuntimeKernel, RUNTIME_SINGLETON};
use orbital_core_base::value::{Value, ValueMap};

const GUEST_VIEW: &str = "guest-view";

/// Whether `node` (a rendered pattern) names `event` in one of its event-bearing props: the registry kinds core reads
/// bus events from (`PATTERN_EVENT_NAME_PROPS`), and a `callback` prop given an event key.
fn carries_event(node: &ValueMap, event: &str) -> bool {
    use orbital_core_base::language::pattern_props::PATTERN_EVENT_NAME_PROPS;
    let Some(Value::String(pattern)) = node.get("type") else {
        return false;
    };
    let named = |value: Option<&Value>| matches!(value, Some(Value::String(name)) if name.as_str() == event);
    let event_props = PATTERN_EVENT_NAME_PROPS.get(pattern.as_str());
    let in_event_prop = event_props
        .into_iter()
        .flatten()
        .any(|(prop, (kind, field))| match (*kind, node.get(*prop)) {
            ("event" | "event-ref" | "event-listen", value) => named(value),
            ("event-list", Some(Value::Array(items))) => items.iter().any(|item| match item {
                Value::Object(entry) => named(entry.get(*field)),
                _ => false,
            }),
            ("event-map", Some(Value::Object(entries))) => entries.values().any(|v| named(Some(v))),
            _ => false,
        });
    in_event_prop
        || orbital_core_base::language::pattern_registry::get_props_schema(pattern.as_str())
            .is_some_and(|props| {
                props.iter().any(|(prop, schema)| {
                    schema.kind.as_deref() == Some("callback") && named(node.get(prop.as_str()))
                })
            })
}

/// Whether the rendered subtree `value` holds a control naming `event`.
fn holds_control(value: &Value, event: &str) -> bool {
    match value {
        Value::Object(node) => {
            carries_event(node, event) || node.values().any(|child| holds_control(child, event))
        }
        Value::Array(items) => items.iter().any(|item| holds_control(item, event)),
        _ => false,
    }
}

/// The first `guest-view` in `value` whose children hold a control carrying `event`: its `(guest, deliver, source)`.
fn guest_view_for(value: &Value, event: &str) -> Option<(String, String, String)> {
    match value {
        Value::Object(node) => {
            if matches!(node.get("type"), Some(Value::String(t)) if t.as_str() == GUEST_VIEW) {
                let text = |key: &str| match node.get(key) {
                    Some(Value::String(s)) if !s.as_str().is_empty() => {
                        Some(s.as_str().to_string())
                    }
                    _ => None,
                };
                if let (Some(guest), Some(deliver), Some(source)) =
                    (text("guest"), text("deliver"), text("source"))
                {
                    if node
                        .get("children")
                        .is_some_and(|children| holds_control(children, event))
                    {
                        return Some((guest, deliver, source));
                    }
                }
                return None;
            }
            node.values().find_map(|child| guest_view_for(child, event))
        }
        Value::Array(items) => items.iter().find_map(|item| guest_view_for(item, event)),
        _ => None,
    }
}

impl RuntimeKernel {
    /// The `deliver` event and payload a guest control's `event` becomes on `trait_name`, when the trait does not
    /// handle `event` in its current state and a `guest-view` it painted holds a control carrying it.
    pub(super) fn guest_control_delivery(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event: &str,
        payload: &Value,
    ) -> Option<(String, Value)> {
        let definition = self
            .registry
            .trait_definitions
            .read()
            .get(trait_name)
            .cloned()?;
        let state =
            self.current_state_name(trait_name, Some(entity_id.unwrap_or(RUNTIME_SINGLETON)));
        if definition
            .state_machine
            .transitions
            .iter()
            .any(|t| t.event == event && t.from.matches(&state))
        {
            return None;
        }
        let held = self
            .renders
            .read()
            .shown(trait_name, entity_id.unwrap_or(RUNTIME_SINGLETON));
        let (guest, deliver, source) = held
            .iter()
            .find_map(|(_, content)| guest_view_for(content, event))?;
        let mut delivered = ValueMap::default();
        delivered.insert("guest".into(), Value::string(guest));
        delivered.insert("address".into(), Value::string(format!("{source}.{event}")));
        delivered.insert("event".into(), Value::string(event.to_string()));
        delivered.insert("payload".into(), payload.clone());
        Some((deliver, Value::object(delivered)))
    }
}
