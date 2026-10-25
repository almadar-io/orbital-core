//! The effect surface of a trait, as a type.
//!
//! A trait's type has always been `Entity → StateMachine`: the entity binding is
//! inferred and checked (`EntityFieldContract`), but its EFFECTS live as values
//! buried in transitions and never surface in the signature. So `uses X.traits.T`
//! is all-or-nothing — a consumer inherits every `(render-ui …)` and every
//! `ticks {}` the atom declares, and no gate can see that it happened.
//!
//! The effect row closes that. It is INFERRED (never authored), stamped onto
//! `TraitDefinition` beside `entityContract`, and answers one question a consumer
//! cannot otherwise ask without reading the whole atom: *what does composing this
//! trait actually bring along?*
//!
//! This module is the ONE place that answers "what is effect `K`'s resource, and
//! where in its argument list does it live?" for both compiler layers. It lives in
//! `orbital-core` because `orbital-lolo` (L1) and `orbital-compiler` (L2) are
//! siblings — L1 dev-depends on L2, so it cannot reach the baked operator registry,
//! and CLAUDE.md's rule is that a check both layers need lives in the crate they
//! share. Effect NAME membership stays owned by [`is_known_effect_name`]; this
//! module only adds the resource position, so the two can never disagree about
//! what counts as an effect.
//!
//! The two layers see different amounts of the program, and the split is deliberate:
//!
//! - **L1** knows one file. It infers the row from the trait's own effects and
//!   ticks, and a resource that is a binding (`@config.surface`) stays
//!   [`EffectUse::resolved`]`= false` — L1 cannot know a call site's config.
//! - **L2** knows the whole program post-inline. After `rewrite_config_bindings`
//!   substitutes call-site config, it re-resolves those entries and only then are
//!   resource comparisons meaningful.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::schema::sexpr::SExpression;

use crate::effect_types::is_known_effect_name;

/// Where in a trait an effect is declared.
///
/// The distinction is load-bearing, not bookkeeping: a `render-ui` in a
/// transition fires when that transition fires, but a `render-ui` inside a
/// `ticks` block repaints EVERY frame. Two traits can name the same slot in
/// transitions and never coexist (a modal and the catalog behind it); two traits
/// painting one slot from tick bodies paint it together, every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EffectSite {
    /// Declared in a state-machine transition (the common case).
    #[default]
    Transition,
    /// Declared inside a `ticks` block — runs on every tick of the clock.
    Tick,
}

impl EffectSite {
    fn is_transition(&self) -> bool {
        matches!(self, EffectSite::Transition)
    }
}

fn is_true(b: &bool) -> bool {
    *b
}

fn default_true() -> bool {
    true
}

/// One effect a trait produces, with the resource it acts on.
///
/// `resource` is the effect's single addressable target — the slot for
/// `render-ui`, the entity for `persist`/`fetch`, the event for `emit`, the
/// binding path for `set`. Effects with no addressable target (`notify`, `log`,
/// `navigate-back`) carry `None`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EffectUse {
    /// The `kebab-case` effect name, exactly as written at the head of the
    /// S-expression and as [`is_known_effect_name`] accepts it. Namespaced
    /// operators keep their full name (`agent/ask`, `os/read`).
    pub kind: String,

    /// The effect's addressable target, when it has one, recorded verbatim as
    /// authored — `main`, `Product`, `SCORED`, `@entity.score`. When the author
    /// deferred it to a call site (`@config.surface`) the reference is kept as
    /// written and `resolved` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,

    /// False while `resource` is still a binding only a call site can make
    /// concrete. Resource comparisons are meaningless until this is true, so a
    /// consumer that compares unresolved resources is asking the wrong layer.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub resolved: bool,

    /// Whether the effect sits in a transition or a `ticks` block.
    #[serde(default, skip_serializing_if = "EffectSite::is_transition")]
    pub site: EffectSite,
}

impl EffectUse {
    /// A resolved literal resource, or `None` when the effect has no resource or
    /// its resource is still an unresolved binding. The only correct way to read
    /// a resource for comparison.
    pub fn resolved_resource(&self) -> Option<&str> {
        if self.resolved {
            self.resource.as_deref()
        } else {
            None
        }
    }
}

/// Every effect a trait produces, sorted and deduplicated.
///
/// Inferred by the lolo lowerer from the trait's own transitions and ticks, then
/// re-resolved post-inline once call-site config is known. Absent on traits that
/// produce no effects at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectRow {
    pub effects: Vec<EffectUse>,
}

impl EffectRow {
    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    /// Sort and deduplicate in place. Callers build unordered and finish here so
    /// the emitted `.orb` is byte-stable across runs.
    pub fn normalize(&mut self) {
        self.effects.sort();
        self.effects.dedup();
    }

    /// True when the trait declares a `ticks` block.
    pub fn has_ticks(&self) -> bool {
        self.effects.iter().any(|e| e.kind == "ticks")
    }

    /// The slots this trait repaints on every tick — the input to the
    /// tick-contention check. Only resolved literals are yielded; an unresolved
    /// slot has no comparable identity yet.
    pub fn tick_rendered_slots(&self) -> impl Iterator<Item = &str> {
        self.effects.iter().filter_map(|e| {
            (e.kind == "render-ui" && e.site == EffectSite::Tick)
                .then(|| e.resolved_resource())
                .flatten()
        })
    }
}

/// Accumulates a trait's effect row.
///
/// Both inference sites go through this — the lolo lowerer walking the AST, and
/// the compiler re-deriving post-inline once call-site config has been
/// substituted — so the "declare the clock, sort, drop-if-empty" rules exist
/// once. Two sites computing the same row two ways is exactly how a gate ends up
/// green for the wrong reason on one path and not the other.
#[derive(Debug, Default)]
pub struct EffectRowBuilder {
    effects: Vec<EffectUse>,
}

impl EffectRowBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Collect every effect call in `effects`, tagged with the site it sits at.
    pub fn add_effects<'a, I>(&mut self, site: EffectSite, effects: I)
    where
        I: IntoIterator<Item = &'a SExpression>,
    {
        for effect in effects {
            effect.collect_effect_uses(site, &mut self.effects);
        }
    }

    /// Record that the trait declares a `ticks` block. Call this whenever a tick
    /// exists, even one whose body produces no effects: a guard-only tick still
    /// owns a clock, and the row states the declaration, not a summary of the body.
    pub fn declare_ticks(&mut self) {
        self.effects.push(EffectUse {
            kind: TICKS_KIND.to_string(),
            resource: None,
            resolved: true,
            site: EffectSite::Tick,
        });
    }

    /// Finish into a normalized row, or `None` when the trait produces no effects
    /// at all — so effect-free traits stay byte-identical in the emitted `.orb`.
    pub fn finish(self) -> Option<EffectRow> {
        if self.effects.is_empty() {
            return None;
        }
        let mut row = EffectRow {
            effects: self.effects,
        };
        row.normalize();
        Some(row)
    }
}

/// The `ticks` marker kind. A trait that declares a `ticks` block carries one of
/// these even when the tick body produces no effects (a guard-only tick still
/// owns a clock).
pub const TICKS_KIND: &str = "ticks";

/// Which argument of effect `kind` names its resource, counting AFTER the head.
///
/// `None` means the effect has no single addressable resource. Keep in sync with
/// `parse_effect`'s explicit arms in `effects::types` — the argument positions
/// here are the same ones it destructures.
fn resource_arg_index(kind: &str) -> Option<usize> {
    match kind {
        // (render-ui <slot> <content>)
        "render-ui" => Some(0),
        // (emit <EVENT> <payload>)
        "emit" => Some(0),
        // (set <@binding.path> <value>)
        "set" => Some(0),
        // (persist <operation> <Entity> ...) — operation first, entity second
        "persist" => Some(1),
        // (fetch <Entity> <options>)
        "fetch" => Some(0),
        // (navigate <path> <params>)
        "navigate" => Some(0),
        // (spawn <entityType> <props>)
        "spawn" => Some(0),
        // (despawn <entityId>)
        "despawn" => Some(0),
        // (call-service <service> <method> <args>) — joined below, both parts matter
        "call-service" => Some(0),
        // (http <METHOD> <origin> <path> ...) — the origin is the resource
        "http" => Some(1),
        _ => None,
    }
}

/// The one binding prefix a call site can still change.
///
/// `rewrite_config_bindings` substitutes `@config.<knob>` in a child body from
/// the merged call-site config and nothing else, so this prefix is exactly the
/// set of resources whose identity is not yet decided. Every other `@`-path is a
/// stable identity in its own right — `(set @entity.score …)` addresses
/// `@entity.score` and always will — and marking those unresolved would make the
/// row claim a call site could move them.
const CALL_SITE_BINDING_PREFIX: &str = "@config.";

/// Read effect `kind`'s resource out of its argument list.
///
/// Returns `(resource, resolved)`, the resource recorded VERBATIM as authored.
/// Verbatim matters: post-inline the compiler re-runs this same walker over the
/// substituted effect tree, where `@config.surface` has already become the
/// literal slot, so resolution needs no second code path — the row simply
/// re-derives correctly. Anything that is not a plain string (a computed slot, a
/// nested call) has no readable identity and yields `None`.
fn effect_resource(kind: &str, args: &[SExpression]) -> (Option<String>, bool) {
    let Some(index) = resource_arg_index(kind) else {
        return (None, true);
    };
    let Some(raw) = args.get(index).and_then(|a| a.as_str()) else {
        return (None, true);
    };

    // `call-service` is addressed by service AND method; either alone is ambiguous.
    let raw = if kind == "call-service" {
        match args.get(1).and_then(|a| a.as_str()) {
            Some(method) => format!("{raw}.{method}"),
            None => raw.to_string(),
        }
    } else {
        raw.to_string()
    };

    let resolved = !raw.starts_with(CALL_SITE_BINDING_PREFIX);
    (Some(raw), resolved)
}

impl SExpression {
    /// Walk this S-expression collecting every effect call into `out`, tagged
    /// with the `site` it was found at.
    ///
    /// Recursion is unconditional — effects nest inside `do` / `when` / `if` /
    /// `let` compounds and inside render content — so the result is a SUPERSET of
    /// what a given run will execute. That is the honest direction for a row that
    /// answers "what does composing this bring along": missing an effect would
    /// make a gate green for the wrong reason, whereas listing one that a guard
    /// happens to skip costs nothing.
    ///
    /// The single canonical walker behind the inferred effect row — used by the
    /// lolo lowerer and by the post-inline compiler re-resolution.
    pub fn collect_effect_uses(&self, site: EffectSite, out: &mut Vec<EffectUse>) {
        // Iterative, not recursive, and deliberately so: authored render trees
        // nest deeply enough that a per-node stack frame overflows the main
        // thread on real corpus files. `emit orb` already runs close to that
        // limit, so a walker that recurses would fail on exactly the largest,
        // most-composed behaviors — the ones this row matters most for.
        let mut pending: Vec<&SExpression> = vec![self];
        while let Some(node) = pending.pop() {
            match node {
                SExpression::Array(arr) => {
                    if let Some(SExpression::String(op)) = arr.first() {
                        if let Some(ops) = (op == "persist")
                            .then(|| crate::persist_batch::batch_ops_arg(&arr[1..]))
                            .flatten()
                        {
                            let entities = crate::persist_batch::batch_entities(ops);
                            if entities.is_empty() {
                                out.push(EffectUse {
                                    kind: op.clone(),
                                    resource: None,
                                    resolved: true,
                                    site,
                                });
                            }
                            for entity in entities {
                                out.push(EffectUse {
                                    kind: op.clone(),
                                    resource: Some(entity.to_string()),
                                    resolved: true,
                                    site,
                                });
                            }
                        } else if is_known_effect_name(op) {
                            let (resource, resolved) = effect_resource(op, &arr[1..]);
                            out.push(EffectUse {
                                kind: op.clone(),
                                resource,
                                resolved,
                                site,
                            });
                        }
                    }
                    pending.extend(arr.iter());
                }
                SExpression::Object(obj) => pending.extend(obj.values()),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sexpr(json: &str) -> SExpression {
        serde_json::from_str(json).expect("fixture must parse")
    }

    fn uses(json: &str, site: EffectSite) -> Vec<EffectUse> {
        let mut out = Vec::new();
        sexpr(json).collect_effect_uses(site, &mut out);
        out
    }

    #[test]
    fn a_literal_slot_is_a_resolved_resource() {
        let found = uses(
            r#"["render-ui", "main", {"type": "box"}]"#,
            EffectSite::Transition,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, "render-ui");
        assert_eq!(found[0].resolved_resource(), Some("main"));
    }

    #[test]
    fn a_config_bound_slot_is_unresolved_and_keeps_its_reference_verbatim() {
        let found = uses(
            r#"["render-ui", "@config.surface", {}]"#,
            EffectSite::Transition,
        );
        assert_eq!(found.len(), 1);
        assert!(!found[0].resolved, "a call site still has to supply this");
        assert_eq!(found[0].resource.as_deref(), Some("@config.surface"));
        assert_eq!(
            found[0].resolved_resource(),
            None,
            "an unresolved resource must never compare equal to a literal"
        );
    }

    #[test]
    fn a_non_config_binding_is_a_resolved_identity() {
        // `@entity.score` is what this set addresses and always will be; only
        // `@config.*` is a call site's to change.
        let found = uses(r#"["set", "@entity.score", 1]"#, EffectSite::Transition);
        assert_eq!(found[0].resolved_resource(), Some("@entity.score"));
    }

    #[test]
    fn persist_addresses_its_entity_not_its_operation() {
        let found = uses(
            r#"["persist", "create", "Product", {}]"#,
            EffectSite::Transition,
        );
        assert_eq!(found[0].resolved_resource(), Some("Product"));
    }

    #[test]
    fn a_persist_batch_addresses_each_entity_its_ops_name() {
        let found = uses(
            r#"["persist", "batch", [["create", "Product", {}], ["delete", "Order", "o1"], ["create", "Product", {}]]]"#,
            EffectSite::Transition,
        );
        let resources: Vec<_> = found
            .iter()
            .map(|u| (u.kind.as_str(), u.resolved_resource()))
            .collect();
        assert_eq!(
            resources,
            vec![("persist", Some("Product")), ("persist", Some("Order"))]
        );
    }

    #[test]
    fn a_persist_batch_over_an_opaque_expression_has_no_resource() {
        let found = uses(
            r#"["persist", "batch", "@payload.ops"]"#,
            EffectSite::Transition,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].resource, None);
    }

    #[test]
    fn call_service_is_addressed_by_service_and_method() {
        let found = uses(
            r#"["call-service", "billing", "charge", {}]"#,
            EffectSite::Transition,
        );
        assert_eq!(found[0].resolved_resource(), Some("billing.charge"));
    }

    #[test]
    fn cancel_call_is_recorded_as_an_effect_without_a_resource() {
        let found = uses(
            r#"["cancel-call", "@entity.batchId"]"#,
            EffectSite::Transition,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, "cancel-call");
        assert_eq!(found[0].resource, None);
    }

    #[test]
    fn effects_nested_in_compounds_are_collected() {
        let found = uses(
            r#"["do", ["set", "@entity.x", 1], ["when", ["==", "@entity.a", true], ["emit", "DONE", {}]]]"#,
            EffectSite::Transition,
        );
        let kinds: Vec<&str> = found.iter().map(|e| e.kind.as_str()).collect();
        assert!(kinds.contains(&"set"), "got {kinds:?}");
        assert!(kinds.contains(&"emit"), "got {kinds:?}");
    }

    #[test]
    fn tick_renders_are_distinguishable_from_transition_renders() {
        let mut row = EffectRow {
            effects: [
                uses(r#"["render-ui", "main", {}]"#, EffectSite::Tick),
                uses(r#"["render-ui", "modal", {}]"#, EffectSite::Transition),
            ]
            .concat(),
        };
        row.normalize();
        let ticked: Vec<&str> = row.tick_rendered_slots().collect();
        assert_eq!(
            ticked,
            vec!["main"],
            "only the tick-site render repaints every frame"
        );
    }

    #[test]
    fn an_unresolved_tick_slot_is_not_yielded_for_comparison() {
        let row = EffectRow {
            effects: uses(r#"["render-ui", "@config.surface", {}]"#, EffectSite::Tick),
        };
        assert_eq!(row.tick_rendered_slots().count(), 0);
    }

    #[test]
    fn non_effect_calls_are_not_collected() {
        let found = uses(r#"["==", "@entity.active", true]"#, EffectSite::Transition);
        assert!(found.is_empty(), "guards are not effects, got {found:?}");
    }
}
