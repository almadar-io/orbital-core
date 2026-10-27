//! What each mounted trait last rendered, kept so an entity change repaints it.
//!
//! The web carries a `render-ui` leaf that reads `@entity` (and no `@payload`) as a
//! `RenderBindingMarker` and re-evaluates it against the live entity on every paint
//! (`deferEntityBindings` in `almadar-runtime`, `resolveRenderBindingMarkers` in
//! `almadar-ui`). A kernel hands its host fully resolved content, so it keeps the same
//! split itself: the unresolved source of each rendered slot, the bindings it was
//! flushed with, and the `@entity` it was resolved against. [`RenderStore::stale`]
//! re-resolves only the marker leaves against the entity as it is now.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::row_query::row_sets_in;
use orbital_core_base::row_set::RowSetRef;
use orbital_core_base::shared_entity::SharedEntity;
use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::context::BindingContext;
use orbital_core_eval::evaluator::operators::get_operator;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Deepest `@config.X` forward chain followed to uncover an `@entity` leaf (the web's bound).
const CONFIG_HOPS: u8 = 8;

/// One slot of a trait's last render.
#[derive(Debug, Clone)]
struct RenderedSlot {
    slot: String,
    source: Value,
    flushed: Arc<BindingContext>,
    entity: SharedEntity,
    resolved: Value,
    /// The row-set references the resolved content carries (EXEC-RENDER-3).
    row_sets: Vec<RowSetRef>,
    /// A kernel write touched an entity type one of `row_sets` points at since the slot last painted.
    rows_dirty: bool,
}

/// A trait instance's `(trait name, entity id)`.
type Instance = (String, String);

/// A slot whose marker leaves resolve differently against the entity as it is now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Repaint {
    pub slot: String,
    pub content: Value,
    /// Repainted because the rows a reference in it points at changed (its content may be unchanged).
    pub rows_changed: bool,
}

/// The mounted traits' last renders, in `(trait, entity id)` order.
#[derive(Debug, Default)]
pub(crate) struct RenderStore {
    instances: BTreeMap<Instance, Vec<RenderedSlot>>,
}

impl RenderStore {
    /// `renders` are one transition's `render-ui` effects in run order, as
    /// `(slot, unresolved source, resolved content)`; `flushed` is the context they resolved
    /// against. A null source clears its slot, as it does on the host.
    pub(crate) fn record(
        &mut self,
        trait_name: &str,
        entity_id: &str,
        renders: Vec<(String, Value, Value)>,
        flushed: &BindingContext,
    ) {
        if renders.is_empty() {
            return;
        }
        let flushed = Arc::new(flushed.clone());
        let slots = self
            .instances
            .entry((trait_name.to_string(), entity_id.to_string()))
            .or_default();
        for (slot, source, resolved) in renders {
            slots.retain(|s| s.slot != slot);
            if source.is_null() {
                continue;
            }
            slots.push(RenderedSlot {
                slot,
                source,
                flushed: flushed.clone(),
                entity: flushed.entity.clone(),
                row_sets: row_sets_in(&resolved),
                resolved,
                rows_dirty: false,
            });
        }
        if slots.is_empty() {
            self.instances
                .remove(&(trait_name.to_string(), entity_id.to_string()));
        }
    }

    /// What `trait_name` last painted on each slot, as the content the host holds, in paint order.
    pub(crate) fn held(&self, trait_name: &str, entity_id: &str) -> Vec<(String, Value)> {
        self.instances
            .get(&(trait_name.to_string(), entity_id.to_string()))
            .into_iter()
            .flatten()
            .map(|s| (s.slot.clone(), s.resolved.clone()))
            .collect()
    }

    /// A newer paint of `slot` by `trait_name` (the host's) replaces whatever the kernel last rendered
    /// there: that older source must not repaint over it.
    pub(crate) fn forget_slot(&mut self, trait_name: &str, slot: &str) {
        for ((t, _), slots) in &mut self.instances {
            if t == trait_name {
                slots.retain(|s| s.slot != slot);
            }
        }
        self.instances.retain(|_, slots| !slots.is_empty());
    }

    pub(crate) fn forget_traits<'a>(&mut self, traits: impl IntoIterator<Item = &'a str>) {
        let gone: Vec<&str> = traits.into_iter().collect();
        self.instances
            .retain(|(t, _), _| !gone.contains(&t.as_str()));
    }

    pub(crate) fn clear(&mut self) {
        self.instances.clear();
    }

    /// A kernel write to each of `entity_types`: the slots holding a row-set reference to one of them repaint
    /// on the next [`Self::stale`], though the reference itself is unchanged, so a renderer re-resolves it.
    pub(crate) fn mark_rows_changed(&mut self, entity_types: &BTreeSet<String>) {
        for slot in self.instances.values_mut().flatten() {
            if slot
                .row_sets
                .iter()
                .any(|r| entity_types.contains(&r.entity_type))
            {
                slot.rows_dirty = true;
            }
        }
    }

    /// Every instance with a slot whose marker leaves resolve differently now, as
    /// `(trait, entity id, repaints)`. `entity_now` reads an instance's current `@entity`;
    /// `reads_live` says whether the client owns that instance's entity bindings (the web's
    /// `clientResolvesRenderBindings`). The store is updated to what it returns, so a second
    /// call with nothing new returns nothing.
    pub(crate) fn stale(
        &mut self,
        reads_live: impl Fn(&str) -> bool,
        entity_now: impl Fn(&str, &str) -> Value,
    ) -> Vec<(String, String, Vec<Repaint>)> {
        let mut out = Vec::new();
        for ((trait_name, entity_id), slots) in &mut self.instances {
            if !reads_live(trait_name) {
                continue;
            }
            let now = Arc::new(entity_now(trait_name, entity_id));
            let mut repaints = Vec::new();
            for slot in slots
                .iter_mut()
                .filter(|s| s.rows_dirty || s.entity.as_value() != &*now)
            {
                let rows_changed = std::mem::take(&mut slot.rows_dirty);
                let resolved = if slot.entity.as_value() != &*now {
                    let mut live = (*slot.flushed).clone();
                    live.entity = SharedEntity::from_arc(Arc::clone(&now));
                    resolve_deferred(&slot.source, &slot.flushed, &live, 0)
                } else {
                    slot.resolved.clone()
                };
                slot.entity = SharedEntity::from_arc(Arc::clone(&now));
                if rows_changed || resolved != slot.resolved {
                    slot.resolved = resolved.clone();
                    slot.row_sets = row_sets_in(&resolved);
                    repaints.push(Repaint {
                        slot: slot.slot.clone(),
                        content: resolved,
                        rows_changed,
                    });
                }
            }
            if !repaints.is_empty() {
                out.push((trait_name.clone(), entity_id.clone(), repaints));
            }
        }
        out
    }
}

/// Whether `s` holds the token `name` (`@entity`, `@payload`) as a whole path root.
fn holds_token(s: &str, name: &str) -> bool {
    s.match_indices(name).any(|(at, _)| {
        s[at + name.len()..]
            .chars()
            .next()
            .is_none_or(|c| matches!(c, '.' | '[' | ']'))
    })
}

fn holds_binding(value: &Value, names: &[&str]) -> bool {
    match value {
        Value::String(s) => names.iter().any(|n| holds_token(s, n)),
        Value::Array(items) => items.iter().any(|v| holds_binding(v, names)),
        Value::Object(map) => map.values().any(|v| holds_binding(v, names)),
        _ => false,
    }
}

/// A leaf the host re-evaluates against the live entity: it reads `@entity` and no payload.
fn is_marker(value: &Value) -> bool {
    holds_binding(value, &["@entity"]) && !holds_binding(value, &["@payload", "@callsitePayload"])
}

/// `source` resolved with every marker leaf read against `live` and everything else against
/// `flushed`; the same leaf split `deferEntityBindings` makes. With `live == flushed` the
/// result is `flushed.resolve_value(source)`.
pub(crate) fn resolve_deferred(
    source: &Value,
    flushed: &BindingContext,
    live: &BindingContext,
    hops: u8,
) -> Value {
    match source {
        Value::String(s) => {
            if is_marker(source) {
                return live.resolve_value(source).into_owned();
            }
            if let Some(key) = s.strip_prefix("@config.") {
                if !s.contains(' ') && hops < CONFIG_HOPS {
                    let forwarded = flushed.config.as_object().and_then(|o| o.get(key));
                    if let Some(next) = forwarded.filter(|next| *next != source) {
                        return resolve_deferred(next, flushed, live, hops + 1);
                    }
                }
            }
            flushed.resolve_value(source).into_owned()
        }
        Value::Array(items) => {
            let operator = items.first().and_then(|head| match head {
                Value::String(op) => Some(op.as_str()),
                _ => None,
            });
            match operator {
                Some("fn") => flushed.resolve_value(source).into_owned(),
                Some(op) if get_operator(op).is_some() => {
                    let ctx = if is_marker(source) { live } else { flushed };
                    ctx.resolve_value(source).into_owned()
                }
                _ => Value::Array(
                    items
                        .iter()
                        .map(|item| {
                            if flushed.is_unset_config_binding(item) {
                                Value::Null
                            } else {
                                resolve_deferred(item, flushed, live, hops)
                            }
                        })
                        .collect(),
                ),
            }
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, v)| !flushed.is_unset_config_binding(v))
                .map(|(k, v)| (k.clone(), resolve_deferred(v, flushed, live, hops)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(entity: serde_json::Value, payload: serde_json::Value) -> BindingContext {
        let mut c = BindingContext::new();
        c.set_entity(serde_json::from_value(entity).unwrap());
        c.set_payload(serde_json::from_value(payload).unwrap());
        c
    }

    fn val(v: serde_json::Value) -> Value {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_token_is_a_whole_path_root() {
        assert!(holds_token("@entity", "@entity"));
        assert!(holds_token("a @entity.x b", "@entity"));
        assert!(holds_token("@entity[0]", "@entity"));
        assert!(!holds_token("@entityRow.x", "@entity"));
        assert!(!holds_token("entity.x", "@entity"));
    }

    #[test]
    fn with_the_same_entity_the_split_walk_equals_plain_resolution() {
        let c = ctx(json!({ "n": 3 }), json!({ "tag": "t" }));
        let source = val(json!({
            "a": "@entity.n",
            "b": ["x", "@payload.tag", { "c": "n=@entity.n" }],
            "d": ["+", "@entity.n", 1],
            "e": 5
        }));
        assert_eq!(
            resolve_deferred(&source, &c, &c, 0),
            c.resolve_value(&source).into_owned()
        );
    }

    #[test]
    fn an_unset_config_knob_is_an_absent_prop_in_a_repaint_as_in_the_first_paint() {
        let mut c = ctx(json!({ "n": 1 }), json!({}));
        c.set_config(val(json!({ "title": "T" })));
        let source = val(
            json!({ "title": "@config.title", "asset": "@config.asset", "rows": ["@config.asset", "@entity.n"] }),
        );
        let plain = c.resolve_value(&source).into_owned();
        assert_eq!(plain, val(json!({ "title": "T", "rows": [null, 1] })));
        assert_eq!(resolve_deferred(&source, &c, &c, 0), plain);
    }

    #[test]
    fn control_a_set_config_knob_still_resolves_in_a_repaint() {
        let mut c = ctx(json!({ "n": 1 }), json!({}));
        c.set_config(val(json!({ "asset": { "src": "a.png" } })));
        let source = val(json!({ "asset": "@config.asset" }));
        assert_eq!(
            resolve_deferred(&source, &c, &c, 0),
            val(json!({ "asset": { "src": "a.png" } }))
        );
    }

    #[test]
    fn an_entity_only_leaf_follows_the_live_entity() {
        let flushed = ctx(json!({ "n": 1 }), json!({}));
        let live = ctx(json!({ "n": 2 }), json!({}));
        let source = val(
            json!({ "pure": "@entity.n", "embedded": "n=@entity.n", "call": ["+", "@entity.n", 10] }),
        );
        assert_eq!(
            resolve_deferred(&source, &flushed, &live, 0),
            val(json!({ "pure": 2, "embedded": "n=2", "call": 12 }))
        );
    }

    #[test]
    fn control_a_leaf_that_also_reads_the_payload_stays_at_the_flushed_entity() {
        let flushed = ctx(json!({ "n": 1 }), json!({ "tag": "t" }));
        let live = ctx(json!({ "n": 2 }), json!({}));
        let source = val(
            json!({ "mixed": "n=@entity.n @payload.tag", "call": ["str/concat", "@entity.n", "@payload.tag"] }),
        );
        let resolved = resolve_deferred(&source, &flushed, &live, 0);
        assert_eq!(
            resolved.as_object().unwrap().get("mixed"),
            Some(&val(json!("n=1 t")))
        );
    }

    #[test]
    fn a_config_forward_that_uncovers_an_entity_leaf_follows_the_live_entity() {
        let mut flushed = ctx(json!({ "n": 1 }), json!({}));
        flushed.set_config(val(json!({ "body": "@entity.n" })));
        let mut live = flushed.clone();
        live.set_entity(val(json!({ "n": 9 })));
        assert_eq!(
            resolve_deferred(&val(json!("@config.body")), &flushed, &live, 0),
            val(json!(9))
        );
    }

    #[test]
    fn control_a_self_forwarding_config_knob_terminates() {
        let mut flushed = ctx(json!({}), json!({}));
        flushed.set_config(val(json!({ "loop": "@config.loop" })));
        let out = resolve_deferred(&val(json!("@config.loop")), &flushed, &flushed.clone(), 0);
        assert_eq!(out, val(json!("@config.loop")));
    }

    #[test]
    fn a_render_lambda_body_is_left_for_the_renderer() {
        let flushed = ctx(json!({ "n": 1 }), json!({}));
        let live = ctx(json!({ "n": 2 }), json!({}));
        let source =
            val(json!(["fn", "row", { "type": "typography", "content": "@entity.n @row.name" }]));
        assert_eq!(
            resolve_deferred(&source, &flushed, &live, 0),
            flushed.resolve_value(&source).into_owned()
        );
    }

    #[test]
    fn stale_repaints_only_changed_marker_slots_and_then_settles() {
        let flushed = ctx(json!({ "n": 1 }), json!({}));
        let mut store = RenderStore::default();
        store.record(
            "Hud",
            "e",
            vec![
                (
                    "hud".into(),
                    val(json!({ "content": "@entity.n" })),
                    val(json!({ "content": 1 })),
                ),
                (
                    "fixed".into(),
                    val(json!({ "content": "static" })),
                    val(json!({ "content": "static" })),
                ),
            ],
            &flushed,
        );
        let now = val(json!({ "n": 2 }));
        let first = store.stale(|_| true, |_, _| now.clone());
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].2,
            vec![Repaint {
                slot: "hud".into(),
                content: val(json!({ "content": 2 })),
                rows_changed: false
            }]
        );
        assert!(store.stale(|_| true, |_, _| now.clone()).is_empty());
    }

    #[test]
    fn control_a_trait_the_client_does_not_own_is_skipped() {
        let flushed = ctx(json!({ "n": 1 }), json!({}));
        let mut store = RenderStore::default();
        store.record(
            "Hud",
            "e",
            vec![("hud".into(), val(json!("@entity.n")), val(json!(1)))],
            &flushed,
        );
        assert!(store
            .stale(|_| false, |_, _| val(json!({ "n": 2 })))
            .is_empty());
    }

    #[test]
    fn a_null_source_clears_the_slot() {
        let flushed = ctx(json!({ "n": 1 }), json!({}));
        let mut store = RenderStore::default();
        store.record(
            "Hud",
            "e",
            vec![("hud".into(), val(json!("@entity.n")), val(json!(1)))],
            &flushed,
        );
        store.record(
            "Hud",
            "e",
            vec![("hud".into(), Value::Null, Value::Null)],
            &flushed,
        );
        assert!(store
            .stale(|_| true, |_, _| val(json!({ "n": 2 })))
            .is_empty());
    }

    #[test]
    fn forgetting_a_trait_drops_only_that_trait() {
        let flushed = ctx(json!({ "n": 1 }), json!({}));
        let mut store = RenderStore::default();
        for t in ["A", "B"] {
            store.record(
                t,
                "e",
                vec![("s".into(), val(json!("@entity.n")), val(json!(1)))],
                &flushed,
            );
        }
        store.forget_traits(["A"]);
        let out = store.stale(|_| true, |_, _| val(json!({ "n": 2 })));
        assert_eq!(
            out.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>(),
            vec!["B"]
        );
    }
}
