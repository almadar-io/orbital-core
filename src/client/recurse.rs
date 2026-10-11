//! Shared, backend-agnostic recursion logic for pattern rendering.
//!
//! Owns the drift-prone "what to render next" decisions — extracting a pattern's
//! child specs and (in a later phase) resolving `@trait.X` references — so the
//! desktop (egui) and tui (ratatui) renderers share one implementation. The
//! "how to render a single pattern" step stays backend-specific (different `ui`
//! types) and is supplied via `RenderContext::child_render`.

use crate::ir::{OirBinding, OirBindingRoot};
use crate::Value;
use std::collections::HashMap;

/// Return the ordered child pattern specs to render for a layout/container
/// pattern: the entries of its `children` array (empty if none).
///
/// Each entry is a pattern object (`{ "type": "...", ... }`) or a `@trait.X[.slot]`
/// string leaf. A `@trait.X` leaf is resolved to that trait's current render
/// output from `trait_content` (matching `@almadar/ui`'s `<TraitFrame>`); an
/// unresolved leaf (trait not rendered yet) is dropped for this frame.
/// Returns `(content, source_trait)` per child: a `@trait.X` leaf resolves to that
/// trait's content tagged with `Some("X")` (so the renderer can switch the event
/// source for that subtree); an inline child is `(child, None)` (inherits the parent
/// source).
pub fn child_patterns(
    props: &Value,
    trait_content: &HashMap<String, Value>,
) -> Vec<(Value, Option<String>)> {
    let Some(arr) = props
        .as_object()
        .and_then(|o| o.get("children"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for child in arr {
        // A `@config.X`-bound child slot whose config default is `null` means "no
        // content" — skip it instead of handing the backend a Null spec (which
        // would surface as an unsupported-pattern fallback).
        if child.is_null() {
            continue;
        }
        if let Some(name) = child.as_str().and_then(trait_ref_name) {
            if let Some(content) = trait_content.get(&name) {
                out.push((content.clone(), Some(name)));
            }
            continue;
        }
        out.push((child.clone(), None));
    }
    out
}

/// Extract the trait name `X` from a `@trait.X[.slot]` binding leaf, via the
/// canonical `crate::ir::OirBinding` parser — orbital-core owns the
/// binding grammar, so the client never re-parses `@trait` strings by hand.
pub fn trait_ref_name(s: &str) -> Option<String> {
    match OirBinding::parse(s)?.root {
        OirBindingRoot::Trait(name) => Some(name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use std::collections::HashMap;

    fn obj(pairs: Vec<(&str, Value)>) -> Value {
        Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    #[test]
    fn extracts_children_array() {
        let props = obj(vec![
            ("type", Value::String("stack".into())),
            (
                "children",
                Value::Array(vec![
                    obj(vec![("type", Value::String("button".into()))]),
                    obj(vec![("type", Value::String("button".into()))]),
                ]),
            ),
        ]);
        let kids = child_patterns(&props, &HashMap::new());
        assert_eq!(kids.len(), 2);
        assert_eq!(
            kids[0].0.as_object().and_then(|o| o.get("type")),
            Some(&Value::String("button".into()))
        );
        assert_eq!(kids[0].1, None, "inline child inherits parent source");
    }

    #[test]
    fn no_children_is_empty() {
        let empty = HashMap::new();
        assert!(
            child_patterns(&obj(vec![("type", Value::String("button".into()))]), &empty).is_empty()
        );
        assert!(child_patterns(&Value::Null, &empty).is_empty());
    }

    // Wave-1 gate: a `@trait.X` leaf in `children` resolves to that trait's
    // current render content from the index; an unresolved one is dropped.
    #[test]
    fn resolves_trait_ref() {
        let mut index = HashMap::new();
        index.insert(
            "Search".to_string(),
            obj(vec![("type", Value::String("table-view".into()))]),
        );
        let props = obj(vec![(
            "children",
            Value::Array(vec![
                Value::String("@trait.Search".into()),  // resolves
                Value::String("@trait.Missing".into()), // dropped (not in index)
                obj(vec![("type", Value::String("button".into()))]),
            ]),
        )]);
        let kids = child_patterns(&props, &index);
        assert_eq!(
            kids.len(),
            2,
            "missing trait ref should be dropped: {kids:?}"
        );
        assert_eq!(
            kids[0]
                .0
                .as_object()
                .and_then(|o| o.get("type"))
                .and_then(|v| v.as_str()),
            Some("table-view")
        );
        assert_eq!(kids[0].1.as_deref(), Some("Search"), "@trait child carries its source");
        assert_eq!(trait_ref_name("@trait.Search.main").as_deref(), Some("Search"));
        assert_eq!(trait_ref_name("not-a-trait"), None);
    }

    // Wave-8 (`C-TRAITREF-WASM-UNRESOLVED`): a peer that renders only to an overlay slot
    // (e.g. a popover) is now indexed into `trait_content` too; once present, its `@trait.X`
    // leaf resolves on the read side exactly like a `main` peer — the write-side slot role is
    // irrelevant here (the index is a flat trait→content map).
    #[test]
    fn resolves_overlay_sourced_peer() {
        let mut index = HashMap::new();
        index.insert(
            "Popover1".to_string(),
            obj(vec![("type", Value::String("popover".into()))]),
        );
        let props = obj(vec![(
            "children",
            Value::Array(vec![Value::String("@trait.Popover1".into())]),
        )]);
        let kids = child_patterns(&props, &index);
        assert_eq!(kids.len(), 1, "overlay peer present in index resolves");
        assert_eq!(kids[0].1.as_deref(), Some("Popover1"));
        assert_eq!(
            kids[0].0.as_object().and_then(|o| o.get("type")).and_then(|v| v.as_str()),
            Some("popover")
        );
    }

    // Wave-0 gate: the recursion hook on RenderContext invokes the backend
    // callback once per child — proves layout renderers can recurse.
    #[test]
    fn render_children_invokes_callback_per_child() {
        use crate::client::helpers::RenderContext;
        use std::cell::Cell;

        let count = Cell::new(0usize);
        let cb = |_ui: *mut (), _child: &Value, _ctx: &mut RenderContext| {
            count.set(count.get() + 1);
        };

        let entities = HashMap::new();
        let tc = HashMap::new();
        let editing = None;
        let mut form = HashMap::new();
        let mut pending = Vec::new();
        let mut ctx = RenderContext {
            trait_name: "T",
            orbital_name: "O",
            current_state: "listing",
            entities: &entities,
            form_data: &mut form,
            editing_entity: &editing,
            pending_events: &mut pending,
            child_render: Some(&cb),
            trait_content: &tc,
            source_trait: None,
            canvas_theme: None,
        };

        let props = obj(vec![
            ("type", Value::String("stack".into())),
            (
                "children",
                Value::Array(vec![
                    obj(vec![("type", Value::String("button".into()))]),
                    obj(vec![("type", Value::String("button".into()))]),
                    obj(vec![("type", Value::String("button".into()))]),
                ]),
            ),
        ]);

        ctx.render_children(std::ptr::null_mut(), &props);
        assert_eq!(count.get(), 3);
    }

    // RS-G20: a `@trait.X` child switches the event source to X for its subtree, so a
    // button nested under it emits from X (which the modal's `listens` matches), while a
    // sibling inline child keeps the parent source.
    #[test]
    fn render_children_threads_source_trait_for_trait_child() {
        use crate::client::helpers::RenderContext;
        use std::cell::RefCell;

        let seen: RefCell<Vec<Option<String>>> = RefCell::new(Vec::new());
        let cb = |_ui: *mut (), _child: &Value, ctx: &mut RenderContext| {
            seen.borrow_mut().push(ctx.source_trait.clone());
        };

        let mut tc = HashMap::new();
        tc.insert("Catalog".to_string(), obj(vec![("type", Value::String("stack".into()))]));

        let entities = HashMap::new();
        let editing = None;
        let mut form = HashMap::new();
        let mut pending = Vec::new();
        let mut ctx = RenderContext {
            trait_name: "Layout",
            orbital_name: "O",
            current_state: "idle",
            entities: &entities,
            form_data: &mut form,
            editing_entity: &editing,
            pending_events: &mut pending,
            child_render: Some(&cb),
            trait_content: &tc,
            source_trait: Some("Layout".to_string()),
            canvas_theme: None,
        };

        let props = obj(vec![(
            "children",
            Value::Array(vec![
                Value::String("@trait.Catalog".into()),            // → source switches to Catalog
                obj(vec![("type", Value::String("button".into()))]), // inline → inherits Layout
            ]),
        )]);

        ctx.render_children(std::ptr::null_mut(), &props);
        let seen = seen.borrow();
        assert_eq!(seen[0].as_deref(), Some("Catalog"), "@trait child renders with X as source");
        assert_eq!(seen[1].as_deref(), Some("Layout"), "inline sibling keeps parent source");
        // Source restored after the loop.
        assert_eq!(ctx.source_trait.as_deref(), Some("Layout"));
    }
}
