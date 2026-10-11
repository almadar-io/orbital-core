//! Layer 2 — the client render contract, renderer-neutral.
//!
//! Drives the shared client circuit ([`ClientCore`]) against a bridge with no
//! window and asserts what every renderer must uphold, asking the renderer under
//! test ([`RenderTarget`]) only which patterns it draws and how a game canvas
//! paints:
//!
//! - **renderer_coverage**: every pattern that reaches a slot (and every
//!   `@trait`-resolved child) has a real renderer. A pattern the renderer
//!   declares a stub for is reported in [`RenderReport::stubbed`], not failed.
//! - **trait_bindings_resolved**: every `@trait.X` reference resolves to indexed
//!   trait content, against the union of every orbital's initialised content.
//! - **entity_data_bound**: every `table-view`/`data-list`/`data-grid` names an
//!   entity with rows after a seeded INIT.
//! - **game_canvas_renders**: every game-canvas pattern paints a non-empty frame
//!   (the renderer's probe; skipped when it has none or no GPU).
//! - **overlay_slots_render**: every modal/drawer open transition fills its slot
//!   with real renderers.

use crate::client::bridge::{KernelBridge, ServerBridge};
use crate::client::client_core::ClientCore;
use crate::client::recurse::trait_ref_name;
use crate::drawable::{is_draw_host, is_drawable};
use crate::render_report::{RenderReport, RenderVerdict};
use crate::{OrbitalSchema, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Outcome of a game-canvas paint probe.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GameCanvasOutcome {
    Pass,
    Fail(String),
    /// The environment cannot paint (no GPU, no probe) — inconclusive, not a failure.
    Skip(String),
}

/// What the probe asks of the renderer under test.
pub trait RenderTarget {
    /// Whether this renderer draws `pattern` for real.
    fn supports(&self, pattern: &str) -> bool;
    /// Whether this renderer declares a stub for `pattern` (reported, not failed).
    fn is_stub(&self, _pattern: &str) -> bool {
        false
    }
    /// Paint one game-canvas `pattern` headlessly.
    fn probe_game_canvas(&self, pattern: &str, _props: &Value) -> GameCanvasOutcome {
        GameCanvasOutcome::Skip(format!("`{pattern}`: this renderer has no game-canvas probe"))
    }
}

fn dedup_sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// A fresh in-process kernel for `schema`, seeded the way every headless probe
/// is: the default probe persona bound (so `@read` sees a viewer) and the
/// identity roster seeded as it, else one sample row per entity.
pub fn seeded_kernel(schema: &OrbitalSchema) -> Result<Arc<dyn ServerBridge>, String> {
    let bridge = Arc::new(KernelBridge::new(schema.clone()).map_err(|e| format!("kernel bridge: {e}"))?);
    match crate::runtime::default_probe_persona(schema) {
        Some(user) => {
            let id = user.get_path("id").and_then(|v| v.as_str().map(str::to_string));
            bridge.set_current_user(user);
            bridge.seed_sample_as(schema, crate::runtime::DEFAULT_IDENTITY_ROSTER_COUNT, id);
        }
        None => bridge.seed_sample(schema, 1),
    }
    Ok(bridge)
}

/// Probe `schema` through the in-process kernel, restricted to `traits` when given.
pub fn probe(item: &str, schema: OrbitalSchema, traits: Option<&[String]>, target: &dyn RenderTarget) -> Result<RenderReport, String> {
    probe_with(item, schema, traits, target, |s, _idx| seeded_kernel(s))
}

/// Probe `schema`, driving each orbital through a fresh bridge from
/// `make_bridge(schema, orbital_idx)` (a shared kernel or server would carry
/// state across orbitals). `traits` (`--trait`) restricts the checks to the
/// slots and trees those traits own or embed; every orbital still INITs.
pub fn probe_with<F>(
    item: &str,
    schema: OrbitalSchema,
    traits: Option<&[String]>,
    target: &dyn RenderTarget,
    make_bridge: F,
) -> Result<RenderReport, String>
where
    F: Fn(&OrbitalSchema, usize) -> Result<Arc<dyn ServerBridge>, String>,
{
    let resolved_traits = traits
        .map(|names| crate::planner::resolve_trait_names(&schema, names))
        .transpose()
        .map_err(|e| e.to_string())?;
    let scope: Option<HashSet<&str>> = resolved_traits
        .as_ref()
        .map(|names| names.iter().map(String::as_str).collect());
    let build_app = |idx: usize| -> Result<ClientCore, String> {
        let bridge = make_bridge(&schema, idx)?;
        let mut app = ClientCore::new(schema.clone(), Some(bridge));
        app.set_orbital(idx);
        app.init_all_traits_blocking();
        app.settle(8);
        Ok(app)
    };
    let orbital_count = schema.orbitals.len();

    let mut unreal_patterns: Vec<String> = Vec::new();
    let mut stubbed: Vec<String> = Vec::new();
    let mut unresolved_traits: Vec<String> = Vec::new();
    let mut unbound_entities: Vec<String> = Vec::new();
    let mut game_canvas_failures: Vec<String> = Vec::new();
    let mut slots_rendered = 0usize;

    // Entities whose author declared a real `@read` predicate (a `@read none`
    // waiver lowers to no policy and is deliberately absent). An empty list on
    // one of these is what a fail-closed viewer is SUPPOSED to see.
    let gated: HashSet<String> = schema
        .orbitals
        .iter()
        .flat_map(|o| std::iter::once(&o.entity).chain(o.auxiliary_entities.iter().flatten()))
        .filter_map(|er| match er {
            crate::schema::EntityRef::Inline(e) => {
                e.read_policy.as_ref().map(|_| e.name.clone())
            }
            _ => None,
        })
        .collect();
    // `linkedEntity` per trait — an `entity` prop that resolved to an inline
    // array carries no type name, so the rendering trait's bound entity is what
    // attributes it.
    let linked_entity: HashMap<String, String> = schema
        .orbitals
        .iter()
        .flat_map(|o| o.traits.iter())
        .filter_map(|t| match t {
            crate::TraitReference::Inline(td) => td
                .linked_entity
                .as_ref()
                .map(|e| (td.name.clone(), e.clone())),
            _ => None,
        })
        .collect();
    let mut access_scoped_empty: Vec<String> = Vec::new();

    // Pass 1 — init every orbital and accumulate a UNION of trait_content. A `@trait.X`
    // defined in orbital A but referenced in orbital B's render-ui (a valid cross-orbital
    // composition) must still resolve. First write wins.
    let mut trait_union: HashMap<String, Value> = HashMap::new();
    for idx in 0..orbital_count {
        let app = build_app(idx)?;
        for (name, content) in &app.trait_content {
            trait_union
                .entry(name.clone())
                .or_insert_with(|| content.clone());
        }
    }
    // Traits that render nothing until an overlay-open transition fires — an unresolved
    // `<trait.X/>` embed of one of these is not a Pass 2 miss, it's Pass 3's job below.
    let overlay_only = crate::overlay_only_traits(&schema);

    // Pass 2 — walk EVERY populated slot of each orbital (not just `main`), checking refs
    // against the union and entity bindings against this orbital's freshly-fetched cache.
    // The `system` slot holds invisible listeners (InputListener/CollisionDetector) — not
    // a render surface — so it is excluded from coverage.
    for idx in 0..orbital_count {
        let app = build_app(idx)?;

        let entities_cache = &app.entities;
        for (slot_name, contents) in &app.ui_slots {
            if crate::slot_role(slot_name) == crate::SlotRole::System {
                continue;
            }
            for slot in contents {
                slots_rendered += 1;
                let mut visited = HashSet::new();
                let access = AccessScope {
                    gated: &gated,
                    linked: &linked_entity,
                    owner: owner_entity(&slot.props, &linked_entity),
                };
                // Own: this slot's top-level content was produced by a selected trait's
                // render-ui. An out-of-scope root still gets walked (below) so a `--trait`
                // embedded ONLY inside another trait's tree is still found — see the
                // sticky embed rule in `walk`.
                let root_in_scope = scope
                    .as_ref()
                    .is_none_or(|s| slot.source_trait.as_deref().is_some_and(|t| s.contains(t)));
                walk(
                    &slot.props,
                    &trait_union,
                    target,
                    entities_cache,
                    &mut visited,
                    &mut unreal_patterns,
                    &mut stubbed,
                    &mut unresolved_traits,
                    &mut unbound_entities,
                    &mut game_canvas_failures,
                    access,
                    &mut access_scoped_empty,
                    scope.as_ref(),
                    root_in_scope,
                    &overlay_only,
                );
            }
        }
    }

    // Pass 3 — drive every "open a non-`main` slot" transition (modal/drawer/overlay/…)
    // and assert the target slot populates with a real renderer. This closes RS-G17: the
    // main-only walk never exercised overlay slots, so a dropped modal passed silently.
    // A transition whose open event does NOT execute under a generic payload (guard holds,
    // or its `from` state isn't the post-INIT state) is inconclusive, not a failure — we
    // only flag a slot that opens but renders an unreal/stub pattern.
    let mut overlay_offenders: Vec<String> = Vec::new();
    for path in crate::modal_open_paths(&schema) {
        // `--trait`: only drive open paths whose render-ui TARGET is a selected trait.
        if let Some(s) = &scope {
            if !s.contains(path.modal_trait.as_str()) {
                continue;
            }
        }
        // Drive the SOURCE event from the SOURCE trait (the button's owner) so the open
        // goes through the same cross-trait listener routing the GUI uses — not the modal
        // trait in isolation (which masked the routing gap).
        let Some(idx) = schema.orbitals.iter().position(|o| {
            o.traits.iter().any(|t| {
                matches!(t, crate::TraitReference::Inline(td) if td.name == path.source_trait)
            })
        }) else {
            continue;
        };
        let mut app = build_app(idx)?;
        // A `T.f` field — of the source event, or of the modal's event that the listen's
        // declared mapping passes a source field into — carries `f` of a row of `T` the app
        // has loaded, so an overlay that filters by it (a tag's notes) opens on real data.
        let payload_of = |trait_name: &str, event: &str| {
            schema
                .orbitals
                .iter()
                .flat_map(|o| o.traits.iter())
                .find_map(|t| match t {
                    crate::TraitReference::Inline(td) if td.name == trait_name => td
                        .state_machine
                        .events
                        .as_ref()
                        .and_then(|events| events.iter().find(|e| e.key == event))
                        .and_then(|e| e.payload_schema.clone()),
                    _ => None,
                })
        };
        let rows_for = |entity: &str| app.entities.get(entity).map(Vec::as_slice);
        let mut payload = crate::runtime::projected_payload(
            payload_of(&path.source_trait, &path.source_event)
                .as_deref()
                .unwrap_or_default(),
            rows_for,
        );
        if let Some(mapping) = &path.payload_mapping {
            payload.extend(
                crate::runtime::projected_payload::projected_through_mapping(
                    payload_of(&path.modal_trait, &path.modal_event)
                        .as_deref()
                        .unwrap_or_default(),
                    mapping,
                    rows_for,
                ),
            );
        }
        app.drive_blocking(
            &path.source_event,
            Some(&path.source_trait),
            Value::Object(payload.into_iter().collect()),
        );
        app.settle(8);

        let opened = app
            .ui_slots
            .get(&path.slot)
            .map(|c| !c.is_empty())
            .unwrap_or(false);
        if !opened {
            tracing::debug!(
                source_trait = %path.source_trait, source_event = %path.source_event,
                modal_trait = %path.modal_trait, slot = %path.slot,
                "overlay-open: slot did not populate (inconclusive — guard/from-state)",
            );
            continue;
        }
        slots_rendered += 1;
        let entities_cache = app.entities.clone();
        let contents = app.ui_slots.get(&path.slot).cloned().unwrap_or_default();
        for slot in &contents {
            let mut visited = HashSet::new();
            let access = AccessScope {
                gated: &gated,
                linked: &linked_entity,
                owner: owner_entity(&slot.props, &linked_entity),
            };
            walk(
                &slot.props,
                &trait_union,
                target,
                &entities_cache,
                &mut visited,
                &mut overlay_offenders,
                &mut stubbed,
                &mut unresolved_traits,
                &mut unbound_entities,
                &mut game_canvas_failures,
                access,
                &mut access_scoped_empty,
                scope.as_ref(),
                // `path.modal_trait` was already filtered against `scope` above, so this
                // slot is owned by a selected trait — always in scope.
                true,
                &overlay_only,
            );
        }
    }

    let verdicts = vec![
        RenderVerdict::new("renderer_coverage", unreal_patterns),
        RenderVerdict::new("trait_bindings_resolved", unresolved_traits),
        RenderVerdict::new("entity_data_bound", unbound_entities),
        RenderVerdict::new("game_canvas_renders", game_canvas_failures),
        RenderVerdict::new("overlay_slots_render", overlay_offenders),
    ];

    Ok(RenderReport {
        item: item.to_string(),
        verdicts,
        slots_rendered,
        access_scoped_empty,
        traits: resolved_traits,
        stubbed: dedup_sorted(stubbed),
    })
}

/// Patterns whose `props["entity"]` must map to a populated row set after a seeded INIT.
/// Every renderer that reads `ctx.entity_data(props["entity"])` belongs here, so a
/// `@payload.data` binding that fails to resolve to a concrete type (RS-G16) is caught.
const ENTITY_BOUND_PATTERNS: &[&str] = &["data-grid", "table-view", "data-list"];

/// Game-canvas patterns that have a real wgpu mapper (others fall to renderer_coverage).
const GAME_CANVAS_PATTERNS: &[&str] = &["game-canvas-2d", "mini-map"];

/// The entity a slot's content is bound to, via the `sourceTrait` the renderer
/// stamped on it — the only attribution available once an `entity` prop has
/// resolved to an inline row array.
fn owner_entity<'a>(props: &Value, linked_entity: &'a HashMap<String, String>) -> Option<&'a str> {
    let source = props.get_path("sourceTrait")?.as_str()?;
    linked_entity.get(source).map(String::as_str)
}

/// Who may explain an empty entity binding: the entities carrying a declared
/// `@read`, plus the entity the currently-walked content's trait is bound to
/// (an `entity` prop resolved to an inline array carries no type name of its own).
#[derive(Clone, Copy)]
struct AccessScope<'a> {
    gated: &'a HashSet<String>,
    linked: &'a HashMap<String, String>,
    owner: Option<&'a str>,
}

impl<'a> AccessScope<'a> {
    /// True when this entity's own `@read` directive can account for an empty list.
    fn explains(&self, entity: Option<&str>) -> bool {
        entity.is_some_and(|e| self.gated.contains(e))
    }

    /// Re-attribute on entering another trait's resolved content — the child
    /// binds its own entity, so the embedder's is not the one to credit.
    fn descend(&self, props: &Value) -> AccessScope<'a> {
        AccessScope {
            owner: owner_entity(props, self.linked).or(self.owner),
            ..*self
        }
    }
}

/// Walk a rendered pattern tree: flag patterns with no real renderer, `@trait.X` child
/// references that don't resolve to indexed content, entity bindings with no rows, and
/// game-canvas patterns that fail the headless pixel probe.
#[allow(clippy::too_many_arguments)]
fn walk(
    content: &Value,
    trait_content: &HashMap<String, Value>,
    target: &dyn RenderTarget,
    entities_cache: &HashMap<String, Vec<Value>>,
    visited_traits: &mut HashSet<String>,
    unreal: &mut Vec<String>,
    stubbed: &mut Vec<String>,
    unresolved_traits: &mut Vec<String>,
    unbound_entities: &mut Vec<String>,
    game_canvas_failures: &mut Vec<String>,
    access: AccessScope<'_>,
    access_scoped_empty: &mut Vec<String>,
    // `--trait` scope: the resolved trait names the walk is restricted to (`None` = no
    // filter, today's full walk). `in_scope` is STICKY — once true (this node's owning
    // trait is selected, or an ancestor embed matched), every descendant stays true: the
    // named trait's "own or embed" closure is checked whole, not re-tested per node.
    trait_scope: Option<&HashSet<&str>>,
    in_scope: bool,
    // Traits that legitimately render nothing until an overlay-open transition fires
    // (`crate::overlay_only_traits`) — an unresolved embed of one of these is
    // Pass 3's job, not a Pass 2 miss.
    overlay_only: &HashSet<String>,
) {
    let Some(obj) = content.as_object() else {
        return;
    };
    if in_scope {
        if let Some(pattern) = obj.get("type").and_then(|v| v.as_str()) {
            // A `type` that is itself a `@`-binding (`@config.layoutPattern`,
            // `@config.viewPattern`, `@config.detailPattern`, …) is resolved to a concrete
            // pattern at runtime from config the headless seed doesn't synthesize. It is a
            // dynamic node, not a missing renderer — exempt it from coverage/entity/game
            // classification (its concrete children are still walked below). This mirrors the
            // draw-host exemption: the node delegates to runtime resolution.
            let dynamic_type = pattern.starts_with('@');
            // A draw-host renders by projecting + painting its authored drawable `children`
            // (exempt below) via its painter — it has no independent per-pattern renderer of
            // its own. The unified `canvas` host's native drawable painter lands in P7; until
            // then coverage is satisfied by the host delegating to its (covered) children,
            // mirroring the child exemption. Only bites a draw-host lacking a renderer
            // (`canvas`); `canvas-2d`/`game-canvas-3d` keep their real renderers.
            if !dynamic_type && !is_draw_host(pattern) && !target.supports(pattern) {
                if target.is_stub(pattern) {
                    stubbed.push(pattern.to_string());
                } else {
                    unreal.push(pattern.to_string());
                }
            }
            if !dynamic_type && ENTITY_BOUND_PATTERNS.contains(&pattern) {
                match obj.get("entity") {
                    // `entity` resolved to its inline collection (a `@payload.data` / `@entity.X`
                    // binding) — the rows travel with the content; assert it's non-empty.
                    Some(Value::Array(rows)) => {
                        let populated = !rows.is_empty();
                        tracing::debug!(
                            target: crate::logging::ns::UI_DATA_GRID,
                            pattern, entity = "<resolved-array>", populated, rows = rows.len(), "data-bound:check",
                        );
                        if !populated {
                            if access.explains(access.owner) {
                                access_scoped_empty.push(format!(
                                    "{pattern}[{}: @read]",
                                    access.owner.unwrap_or("?")
                                ));
                            } else {
                                unbound_entities.push(format!("{pattern}[entity=<empty-array>]"));
                            }
                        }
                    }
                    // A concrete type-name string → check the fetch cache.
                    Some(Value::String(entity_type)) if !entity_type.starts_with('@') => {
                        let populated = entities_cache
                            .get(entity_type)
                            .map(|r| !r.is_empty())
                            .unwrap_or(false);
                        tracing::debug!(
                            target: crate::logging::ns::UI_DATA_GRID,
                            pattern, entity = %entity_type, populated, "data-bound:check",
                        );
                        if !populated {
                            if access.explains(Some(entity_type)) {
                                access_scoped_empty
                                    .push(format!("{pattern}[{entity_type}: @read]"));
                            } else {
                                unbound_entities.push(format!("{pattern}[entity={entity_type}]"));
                            }
                        }
                    }
                    // An unresolved `@payload.*` means the load event didn't carry the rows —
                    // the table renders empty (the RS-G16 symptom). Flag it. Other contextual
                    // bindings (`@entity.<subCollection>`, `@config.X`) legitimately depend on
                    // data the headless seed doesn't fully synthesize (it seeds scalar fields,
                    // not nested collections), so a literal `@entity.*`/`@config.*` is skipped.
                    Some(Value::String(b)) if b.starts_with("@payload") => {
                        unbound_entities.push(format!("{pattern}[unresolved={b}]"));
                    }
                    Some(Value::String(_)) => {}
                    _ => {}
                }
            }
            if GAME_CANVAS_PATTERNS.contains(&pattern) {
                match target.probe_game_canvas(pattern, content) {
                    GameCanvasOutcome::Pass => {}
                    GameCanvasOutcome::Fail(msg) => {
                        tracing::warn!(pattern, %msg, "game-canvas pixel probe: FAIL");
                        game_canvas_failures.push(msg);
                    }
                    GameCanvasOutcome::Skip(reason) => {
                        tracing::info!(pattern, %reason, "game-canvas pixel probe: SKIP (env gap)");
                    }
                }
            }
        }
    } // in_scope
      // A draw-host paints its authored `children` itself (they are routed to the host's
      // `drawables` prop and drawn by its painter — the L2 twin of UISlotRenderer's runtime
      // routing), so a neutral drawable child is NOT independently slot-rendered and needs no
      // standalone renderer. Exempt such children from renderer-coverage; a NON-drawable child
      // still falls through to the normal walk and is flagged (mirrors the validator's
      // ORB_RUI_CHILD_NOT_DRAWABLE — defense-in-depth, not a name/heuristic check).
    let host_paints = obj
        .get("type")
        .and_then(|v| v.as_str())
        .map(is_draw_host)
        .unwrap_or(false);
    if let Some(children) = obj.get("children").and_then(|v| v.as_array()) {
        for child in children {
            if host_paints {
                if let Some(ct) = child
                    .as_object()
                    .and_then(|o| o.get("type"))
                    .and_then(|v| v.as_str())
                {
                    if is_drawable(ct) {
                        continue;
                    }
                }
            }
            if let Some(s) = child.as_str() {
                if let Some(name) = trait_ref_name(s) {
                    match trait_content.get(&name) {
                        Some(resolved) => {
                            if visited_traits.insert(name.clone()) {
                                // Sticky: already in scope, or this embed IS the named
                                // trait — either way its own closure is checked from here.
                                let child_in_scope = in_scope
                                    || trait_scope.is_none_or(|s| s.contains(name.as_str()));
                                walk(
                                    resolved,
                                    trait_content,
                                    target,
                                    entities_cache,
                                    visited_traits,
                                    unreal,
                                    stubbed,
                                    unresolved_traits,
                                    unbound_entities,
                                    game_canvas_failures,
                                    access.descend(resolved),
                                    access_scoped_empty,
                                    trait_scope,
                                    child_in_scope,
                                    overlay_only,
                                );
                            }
                        }
                        None => {
                            if in_scope && !overlay_only.contains(&name) {
                                unresolved_traits.push(name);
                            }
                        }
                    }
                }
            } else {
                walk(
                    child,
                    trait_content,
                    target,
                    entities_cache,
                    visited_traits,
                    unreal,
                    stubbed,
                    unresolved_traits,
                    unbound_entities,
                    game_canvas_failures,
                    access,
                    access_scoped_empty,
                    trait_scope,
                    in_scope,
                    overlay_only,
                );
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    struct Target {
        supported: &'static [&'static str],
        stubs: &'static [&'static str],
    }

    impl RenderTarget for Target {
        fn supports(&self, pattern: &str) -> bool {
            self.supported.contains(&pattern)
        }
        fn is_stub(&self, pattern: &str) -> bool {
            self.stubs.contains(&pattern)
        }
    }

    fn program(main: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "t",
            "orbitals": [{
                "name": "Notes",
                "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "NotesView", "linkedEntity": "Note",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                            "effects": [["render-ui", "main", main]] }]
                    }
                }],
                "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "NotesView" }] }]
            }]
        }))
        .unwrap()
    }

    fn verdict<'a>(report: &'a RenderReport, name: &str) -> &'a RenderVerdict {
        report.verdicts.iter().find(|v| v.name == name).unwrap()
    }

    #[test]
    fn a_supported_pattern_passes_coverage() {
        let report = probe("t", program(serde_json::json!({ "type": "typography", "content": "x" })), None, &Target { supported: &["typography"], stubs: &[] }).unwrap();
        assert!(report.ok(), "{:?}", report.verdicts);
        assert!(report.slots_rendered >= 1);
    }

    #[test]
    fn control_a_pattern_the_renderer_lacks_fails_coverage() {
        let report = probe("t", program(serde_json::json!({ "type": "health-bar" })), None, &Target { supported: &["typography"], stubs: &[] }).unwrap();
        let coverage = verdict(&report, "renderer_coverage");
        assert!(!coverage.passed && coverage.detail.contains("health-bar"), "{}", coverage.detail);
    }

    #[test]
    fn a_stubbed_pattern_is_reported_not_failed() {
        let report = probe("t", program(serde_json::json!({ "type": "health-bar" })), None, &Target { supported: &[], stubs: &["health-bar"] }).unwrap();
        assert!(verdict(&report, "renderer_coverage").passed);
        assert_eq!(report.stubbed, vec!["health-bar".to_string()]);
        assert!(report.headline().ends_with("1 stubbed"), "{}", report.headline());
    }

    #[test]
    fn edge_an_unknown_trait_scope_is_refused() {
        let err = probe("t", program(serde_json::json!({ "type": "typography" })), Some(&["Nope".to_string()]), &Target { supported: &["typography"], stubs: &[] }).unwrap_err();
        assert!(err.contains("unknown --trait \"Nope\""), "{err}");
    }
}
