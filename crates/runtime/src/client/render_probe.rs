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
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::client::bridge::{KernelBridge, ServerBridge};
use crate::client::client_core::ClientCore;
use crate::client::recurse::trait_ref_name;
use crate::program_machine::{
    EntityBinding, EntityBindingKind, RenderedNode, RenderedSlot, TraitRef,
};
use crate::render_report::{RenderReport, RenderVerdict};
use orbital_core_base::drawable::{is_draw_host, is_drawable};
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_base::value::ValueMap;
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
        GameCanvasOutcome::Skip(format!(
            "`{pattern}`: this renderer has no game-canvas probe"
        ))
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
    let bridge =
        Arc::new(KernelBridge::new(schema.clone()).map_err(|e| format!("kernel bridge: {e}"))?);
    match crate::persona::default_probe_persona(schema) {
        Some(user) => {
            let id = user
                .get_path("id")
                .and_then(|v| v.as_str().map(str::to_string));
            bridge.set_current_user(user);
            bridge.seed_sample_as(schema, crate::persona::DEFAULT_IDENTITY_ROSTER_COUNT, id);
        }
        None => bridge.seed_sample(schema, 1),
    }
    Ok(bridge)
}

/// Probe `schema` through the in-process kernel, restricted to `traits` when given.
pub fn probe(
    item: &str,
    schema: OrbitalSchema,
    traits: Option<&[String]>,
    target: &dyn RenderTarget,
) -> Result<RenderReport, String> {
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
    let mut found = Findings::default();
    let mut game_canvas_failures: Vec<String> = Vec::new();
    let mut slots_rendered = 0usize;
    let facts = ProgramFacts::new(&schema);
    let trait_union = facts.trait_union(orbital_count, &build_app)?;
    let cx = FactCx {
        trait_content: &trait_union,
        target,
        trait_scope: scope.as_ref(),
        overlay_only: &facts.overlay_only,
    };

    // Pass 2 — walk EVERY populated slot of each orbital (not just `main`), checking refs
    // against the union and entity bindings against this orbital's freshly-fetched cache.
    // The `system` slot holds invisible listeners (InputListener/CollisionDetector) — not
    // a render surface — so it is excluded from coverage.
    for idx in 0..orbital_count {
        let app = build_app(idx)?;

        let entities_cache = &app.entities;
        for (slot_name, contents) in &app.ui_slots {
            if orbital_core_base::slots::slot_role(slot_name)
                == orbital_core_base::slots::SlotRole::System
            {
                continue;
            }
            for slot in contents {
                slots_rendered += 1;
                // Own: this slot's top-level content was produced by a selected trait's
                // render-ui. An out-of-scope root still gets walked so a `--trait`
                // embedded ONLY inside another trait's tree is still found (the sticky
                // embed rule in `node_facts`).
                let root_in_scope = scope
                    .as_ref()
                    .is_none_or(|s| slot.source_trait.as_deref().is_some_and(|t| s.contains(t)));
                let nodes = facts.slot_nodes(
                    &cx,
                    slot,
                    entities_cache,
                    root_in_scope,
                    &mut game_canvas_failures,
                );
                judge_nodes(&nodes, &mut unreal_patterns, &mut found);
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
    for path in orbital_core_base::slots::modal_open_paths(&schema) {
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
                matches!(t, orbital_core_base::schema::types::TraitReference::Inline(td) if td.name == path.source_trait)
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
                    orbital_core_base::schema::types::TraitReference::Inline(td)
                        if td.name == trait_name =>
                    {
                        td.state_machine
                            .events
                            .as_ref()
                            .and_then(|events| events.iter().find(|e| e.key == event))
                            .and_then(|e| e.payload_schema.clone())
                    }
                    _ => None,
                })
        };
        let rows_for = |entity: &str| app.entities.get(entity).map(Vec::as_slice);
        let mut payload = crate::projected_payload(
            payload_of(&path.source_trait, &path.source_event)
                .as_deref()
                .unwrap_or_default(),
            rows_for,
        );
        if let Some(mapping) = &path.payload_mapping {
            payload.extend(crate::projected_payload::projected_through_mapping(
                payload_of(&path.modal_trait, &path.modal_event)
                    .as_deref()
                    .unwrap_or_default(),
                mapping,
                rows_for,
            ));
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
            // `path.modal_trait` was already filtered against `scope` above, so this
            // slot is owned by a selected trait — always in scope.
            let nodes =
                facts.slot_nodes(&cx, slot, &entities_cache, true, &mut game_canvas_failures);
            judge_nodes(&nodes, &mut overlay_offenders, &mut found);
        }
    }

    let verdicts = vec![
        offenders("renderer_coverage", unreal_patterns),
        offenders("trait_bindings_resolved", found.unresolved),
        offenders("entity_data_bound", found.unbound),
        offenders("game_canvas_renders", game_canvas_failures),
        offenders("overlay_slots_render", overlay_offenders),
    ];

    Ok(RenderReport {
        item: item.to_string(),
        verdicts,
        slots_rendered,
        access_scoped_empty: found.access_scoped_empty,
        traits: resolved_traits,
        stubbed: dedup_sorted(found.stubbed),
    })
}

/// A check's offenders, judged by `orb verify` through `std-render-report`.
fn offenders(name: &str, offenders: Vec<String>) -> RenderVerdict {
    RenderVerdict {
        name: name.to_string(),
        passed: false,
        detail: String::new(),
        offenders,
    }
}

/// What the walks of one program share: the entities with a declared `@read`, each trait's `linkedEntity`, and the
/// traits that render only once an overlay opens.
struct ProgramFacts {
    // Entities whose author declared a real `@read` predicate (a `@read none` waiver lowers to
    // no policy and is deliberately absent). An empty list on one of these is what a
    // fail-closed viewer is SUPPOSED to see.
    gated: HashSet<String>,
    // An `entity` prop that resolved to an inline array carries no type name, so the
    // rendering trait's bound entity is what attributes it.
    linked_entity: HashMap<String, String>,
    overlay_only: HashSet<String>,
}

impl ProgramFacts {
    fn new(schema: &OrbitalSchema) -> Self {
        use orbital_core_base::schema::types::{EntityRef, TraitReference};
        let gated = schema
            .orbitals
            .iter()
            .flat_map(|o| std::iter::once(&o.entity).chain(o.auxiliary_entities.iter().flatten()))
            .filter_map(|er| match er {
                EntityRef::Inline(e) => e.read_policy.as_ref().map(|_| e.name.clone()),
                _ => None,
            })
            .collect();
        let linked_entity = schema
            .orbitals
            .iter()
            .flat_map(|o| o.traits.iter())
            .filter_map(|t| match t {
                TraitReference::Inline(td) => td
                    .linked_entity
                    .as_ref()
                    .map(|e| (td.name.clone(), e.clone())),
                _ => None,
            })
            .collect();
        Self {
            gated,
            linked_entity,
            overlay_only: orbital_core_base::slots::overlay_only_traits(schema),
        }
    }

    /// Init every orbital and take the UNION of their trait content: a `@trait.X` defined in orbital A
    /// and embedded by orbital B's render-ui (a valid cross-orbital composition) still resolves. First write wins.
    fn trait_union(
        &self,
        orbital_count: usize,
        build_app: &dyn Fn(usize) -> Result<ClientCore, String>,
    ) -> Result<ValueMap, String> {
        let mut union = ValueMap::default();
        for idx in 0..orbital_count {
            let app = build_app(idx)?;
            for (name, content) in &app.trait_content {
                union.entry(name.clone()).or_insert_with(|| content.clone());
            }
        }
        Ok(union)
    }

    fn slot_nodes(
        &self,
        cx: &FactCx<'_>,
        slot: &crate::client::helpers::SlotContent,
        entities_cache: &HashMap<String, Vec<Value>>,
        in_scope: bool,
        game_canvas_failures: &mut Vec<String>,
    ) -> Vec<RenderedNode> {
        let access = AccessScope {
            gated: &self.gated,
            linked: &self.linked_entity,
            owner: owner_entity(&slot.props, &self.linked_entity),
        };
        let mut nodes = Vec::new();
        node_facts(
            &slot.props,
            cx,
            entities_cache,
            &mut HashSet::new(),
            access,
            in_scope,
            &mut nodes,
            game_canvas_failures,
        );
        nodes
    }
}

/// The render facts of one played request: the client circuit after a seeded INIT of the trait's orbital, then
/// `event` driven from `trait_name` unless it is INIT, reporting each non-`system` slot that trait filled. The
/// request's `from` state is not restored: a slot its event does not reach after INIT is simply not reported.
pub fn played_render_facts<F>(
    schema: &OrbitalSchema,
    trait_name: &str,
    event: &str,
    payload: Value,
    target: &dyn RenderTarget,
    make_bridge: F,
) -> Result<Vec<RenderedSlot>, String>
where
    F: Fn(&OrbitalSchema, usize) -> Result<Arc<dyn ServerBridge>, String>,
{
    use orbital_core_base::schema::types::TraitReference;
    let idx = schema
        .orbitals
        .iter()
        .position(|o| {
            o.traits
                .iter()
                .any(|t| matches!(t, TraitReference::Inline(td) if td.name == trait_name))
        })
        .ok_or_else(|| format!("the program has no trait \"{trait_name}\""))?;
    let build_app = |idx: usize| -> Result<ClientCore, String> {
        let bridge = make_bridge(schema, idx)?;
        let mut app = ClientCore::new(schema.clone(), Some(bridge));
        app.set_orbital(idx);
        app.init_all_traits_blocking();
        app.settle(8);
        Ok(app)
    };
    let facts = ProgramFacts::new(schema);
    let trait_union = facts.trait_union(schema.orbitals.len(), &build_app)?;
    let mut app = build_app(idx)?;
    if event != "INIT" {
        app.drive_blocking(event, Some(trait_name), payload);
        app.settle(8);
    }
    let cx = FactCx {
        trait_content: &trait_union,
        target,
        trait_scope: None,
        overlay_only: &facts.overlay_only,
    };
    let mut canvas = Vec::new();
    let mut slots: Vec<RenderedSlot> = app
        .ui_slots
        .iter()
        .filter(|(name, _)| {
            orbital_core_base::slots::slot_role(name) != orbital_core_base::slots::SlotRole::System
        })
        .flat_map(|(name, contents)| contents.iter().map(move |slot| (name, slot)))
        .filter(|(_, slot)| slot.source_trait.as_deref() == Some(trait_name))
        .map(|(name, slot)| RenderedSlot {
            slot: name.clone(),
            source_trait: trait_name.to_string(),
            owner_entity: facts
                .linked_entity
                .get(trait_name)
                .cloned()
                .unwrap_or_default(),
            nodes: facts.slot_nodes(&cx, slot, &app.entities, true, &mut canvas),
        })
        .collect();
    slots.sort_by(|a, b| a.slot.cmp(&b.slot));
    Ok(slots)
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

/// What every node of one walk is read against.
struct FactCx<'a> {
    trait_content: &'a ValueMap,
    target: &'a dyn RenderTarget,
    // `--trait` scope: the resolved trait names the walk is restricted to (`None` = no filter).
    trait_scope: Option<&'a HashSet<&'a str>>,
    // Traits that render nothing until an overlay-open transition fires (`overlay_only_traits`).
    overlay_only: &'a HashSet<String>,
}

/// The facts of a rendered tree, in pre-order: each node's pattern and renderer coverage, its entity binding and
/// its `@trait.X` references, with each embedded trait's content walked once where it is embedded. A node is
/// reported only while `in_scope`, which is STICKY: once true (the owning trait is selected, or an ancestor embed
/// matched), every descendant stays true. Game canvases are painted by the target as they are met.
#[allow(clippy::too_many_arguments)]
fn node_facts(
    content: &Value,
    cx: &FactCx<'_>,
    entities_cache: &HashMap<String, Vec<Value>>,
    visited_traits: &mut HashSet<String>,
    access: AccessScope<'_>,
    in_scope: bool,
    nodes: &mut Vec<RenderedNode>,
    game_canvas_failures: &mut Vec<String>,
) {
    let Some(obj) = content.as_object() else {
        return;
    };
    let pattern = obj.get("type").and_then(|v| v.as_str());
    let at = in_scope.then(|| {
        nodes.push(node_fact(
            pattern,
            obj,
            content,
            cx.target,
            entities_cache,
            access,
            game_canvas_failures,
        ));
        nodes.len() - 1
    });
    // A draw-host paints its authored `children` itself (routed to its `drawables` prop), so a neutral
    // drawable child is not independently rendered and is not walked; a non-drawable child is
    // (mirrors the validator's ORB_RUI_CHILD_NOT_DRAWABLE).
    let host_paints = pattern.is_some_and(is_draw_host);
    let Some(children) = obj.get("children").and_then(|v| v.as_array()) else {
        return;
    };
    for child in children {
        if host_paints
            && child
                .as_object()
                .and_then(|o| o.get("type"))
                .and_then(|v| v.as_str())
                .is_some_and(is_drawable)
        {
            continue;
        }
        if let Some(s) = child.as_str() {
            let Some(name) = trait_ref_name(s) else {
                continue;
            };
            let resolved = cx.trait_content.get(&name);
            if let Some(i) = at {
                nodes[i].trait_refs.push(TraitRef {
                    name: name.clone(),
                    resolved: resolved.is_some(),
                    overlay_only: cx.overlay_only.contains(&name),
                });
            }
            if let Some(resolved) = resolved {
                if visited_traits.insert(name.clone()) {
                    let child_in_scope =
                        in_scope || cx.trait_scope.is_none_or(|s| s.contains(name.as_str()));
                    node_facts(
                        resolved,
                        cx,
                        entities_cache,
                        visited_traits,
                        access.descend(resolved),
                        child_in_scope,
                        nodes,
                        game_canvas_failures,
                    );
                }
            }
        } else {
            node_facts(
                child,
                cx,
                entities_cache,
                visited_traits,
                access,
                in_scope,
                nodes,
                game_canvas_failures,
            );
        }
    }
}

/// One node's facts. A `type` that is itself a binding (`@config.viewPattern`) is resolved at run time from config
/// the headless seed does not synthesize: a dynamic node, not a missing renderer, and not entity-bound.
fn node_fact(
    pattern: Option<&str>,
    obj: &ValueMap,
    content: &Value,
    target: &dyn RenderTarget,
    entities_cache: &HashMap<String, Vec<Value>>,
    access: AccessScope<'_>,
    game_canvas_failures: &mut Vec<String>,
) -> RenderedNode {
    let Some(pattern) = pattern else {
        return RenderedNode {
            pattern: String::new(),
            dynamic: false,
            supported: true,
            stub: false,
            binding: EntityBinding {
                kind: EntityBindingKind::None,
                entity: String::new(),
                rows: 0,
                read_gated: false,
            },
            trait_refs: Vec::new(),
        };
    };
    let dynamic = pattern.starts_with('@');
    let binding = |kind, entity: &str, rows: usize, read_gated| EntityBinding {
        kind,
        entity: entity.to_string(),
        rows: u32::try_from(rows).unwrap_or(u32::MAX),
        read_gated,
    };
    let binding = if dynamic || !ENTITY_BOUND_PATTERNS.contains(&pattern) {
        binding(EntityBindingKind::None, "", 0, false)
    } else {
        match obj.get("entity") {
            None | Some(Value::Null) => binding(EntityBindingKind::Absent, "", 0, false),
            Some(Value::Array(rows)) => {
                tracing::debug!(
                    target: orbital_core_base::logging::ns::UI_DATA_GRID,
                    pattern, entity = "<resolved-array>", rows = rows.len(), "data-bound:check",
                );
                binding(
                    EntityBindingKind::Inline,
                    access.owner.unwrap_or_default(),
                    rows.len(),
                    access.explains(access.owner),
                )
            }
            Some(Value::String(b)) if b.starts_with("@payload") => {
                binding(EntityBindingKind::PayloadUnresolved, b, 0, false)
            }
            Some(Value::String(b)) if b.starts_with('@') => {
                binding(EntityBindingKind::ContextUnresolved, b, 0, false)
            }
            Some(Value::String(entity_type)) => {
                let rows = entities_cache.get(entity_type).map_or(0, Vec::len);
                tracing::debug!(
                    target: orbital_core_base::logging::ns::UI_DATA_GRID,
                    pattern, entity = %entity_type, rows, "data-bound:check",
                );
                binding(
                    EntityBindingKind::Named,
                    entity_type,
                    rows,
                    access.explains(Some(entity_type)),
                )
            }
            Some(_) => binding(EntityBindingKind::Malformed, "", 0, false),
        }
    };
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
    RenderedNode {
        pattern: pattern.to_string(),
        dynamic,
        supported: !dynamic && (is_draw_host(pattern) || target.supports(pattern)),
        stub: !dynamic && target.is_stub(pattern),
        binding,
        trait_refs: Vec::new(),
    }
}

/// The offenders and notes the render verdicts are built from.
#[derive(Debug, Default)]
struct Findings {
    stubbed: Vec<String>,
    unresolved: Vec<String>,
    unbound: Vec<String>,
    access_scoped_empty: Vec<String>,
}

/// Judge walked facts: a non-dynamic node the renderer lacks goes to `unreal` (or `stubbed`), an unresolved
/// `@trait` reference that is not overlay-only is a miss, and an entity-bound pattern with no rows is unbound unless
/// its entity's `@read` explains the empty list.
fn judge_nodes(nodes: &[RenderedNode], unreal: &mut Vec<String>, found: &mut Findings) {
    for node in nodes {
        if !node.dynamic && !node.supported {
            if node.stub {
                found.stubbed.push(node.pattern.clone());
            } else {
                unreal.push(node.pattern.clone());
            }
        }
        let (pattern, b) = (&node.pattern, &node.binding);
        match b.kind {
            EntityBindingKind::Inline | EntityBindingKind::Named if b.rows == 0 => {
                if b.read_gated {
                    found
                        .access_scoped_empty
                        .push(format!("{pattern}[{}: @read]", b.entity));
                } else if b.kind == EntityBindingKind::Inline {
                    found
                        .unbound
                        .push(format!("{pattern}[entity=<empty-array>]"));
                } else {
                    found
                        .unbound
                        .push(format!("{pattern}[entity={}]", b.entity));
                }
            }
            EntityBindingKind::PayloadUnresolved => found
                .unbound
                .push(format!("{pattern}[unresolved={}]", b.entity)),
            EntityBindingKind::None
            | EntityBindingKind::Absent
            | EntityBindingKind::Malformed
            | EntityBindingKind::Inline
            | EntityBindingKind::Named
            | EntityBindingKind::ContextUnresolved => {}
        }
        found.unresolved.extend(
            node.trait_refs
                .iter()
                .filter(|r| !r.resolved && !r.overlay_only)
                .map(|r| r.name.clone()),
        );
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
        let report = probe(
            "t",
            program(serde_json::json!({ "type": "typography", "content": "x" })),
            None,
            &Target {
                supported: &["typography"],
                stubs: &[],
            },
        )
        .unwrap();
        assert!(
            report.verdicts.iter().all(|v| v.offenders.is_empty()),
            "{:?}",
            report.verdicts
        );
        assert!(report.slots_rendered >= 1);
    }

    #[test]
    fn control_a_pattern_the_renderer_lacks_fails_coverage() {
        let report = probe(
            "t",
            program(serde_json::json!({ "type": "health-bar" })),
            None,
            &Target {
                supported: &["typography"],
                stubs: &[],
            },
        )
        .unwrap();
        let coverage = verdict(&report, "renderer_coverage");
        assert!(
            coverage.offenders.iter().any(|o| o.contains("health-bar")),
            "{:?}",
            coverage.offenders
        );
    }

    #[test]
    fn a_stubbed_pattern_is_reported_not_failed() {
        let report = probe(
            "t",
            program(serde_json::json!({ "type": "health-bar" })),
            None,
            &Target {
                supported: &[],
                stubs: &["health-bar"],
            },
        )
        .unwrap();
        assert!(verdict(&report, "renderer_coverage").offenders.is_empty());
        assert_eq!(report.stubbed, vec!["health-bar".to_string()]);
    }

    fn facts_of(
        content: serde_json::Value,
        entities: &[(&str, usize)],
        gated: &[&str],
    ) -> Vec<RenderedNode> {
        let content: Value = serde_json::from_value(content).unwrap();
        let mut trait_content = ValueMap::default();
        trait_content.insert(
            "Shown".to_string(),
            serde_json::from_value(serde_json::json!({ "type": "typography" })).unwrap(),
        );
        let overlay_only: HashSet<String> = ["Drawer".to_string()].into_iter().collect();
        let gated: HashSet<String> = gated.iter().map(|g| g.to_string()).collect();
        let linked: HashMap<String, String> = [("NotesView".to_string(), "Note".to_string())]
            .into_iter()
            .collect();
        let cache: HashMap<String, Vec<Value>> = entities
            .iter()
            .map(|(e, n)| (e.to_string(), vec![Value::Null; *n]))
            .collect();
        let target = Target {
            supported: &[
                "stack",
                "typography",
                "data-list",
                "table-view",
                "data-grid",
            ],
            stubs: &["health-bar"],
        };
        let cx = FactCx {
            trait_content: &trait_content,
            target: &target,
            trait_scope: None,
            overlay_only: &overlay_only,
        };
        let access = AccessScope {
            gated: &gated,
            linked: &linked,
            owner: Some("Note"),
        };
        let mut nodes = Vec::new();
        let mut canvas = Vec::new();
        node_facts(
            &content,
            &cx,
            &cache,
            &mut HashSet::new(),
            access,
            true,
            &mut nodes,
            &mut canvas,
        );
        nodes
    }

    #[test]
    fn every_entity_binding_is_a_named_kind() {
        let nodes = facts_of(
            serde_json::json!({ "type": "stack", "children": [
                { "type": "data-list", "entity": "Note" },
                { "type": "data-list", "entity": "Tag" },
                { "type": "table-view", "entity": [] },
                { "type": "data-grid", "entity": "@payload.rows" },
                { "type": "data-list", "entity": "@entity.items" },
                { "type": "data-grid" },
                { "type": "data-list", "entity": 5 },
                { "type": "typography" }
            ] }),
            &[("Note", 2)],
            &["Note"],
        );
        let kinds: Vec<(EntityBindingKind, &str, u32, bool)> = nodes
            .iter()
            .map(|n| {
                (
                    n.binding.kind,
                    n.binding.entity.as_str(),
                    n.binding.rows,
                    n.binding.read_gated,
                )
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                (EntityBindingKind::None, "", 0, false),
                (EntityBindingKind::Named, "Note", 2, true),
                (EntityBindingKind::Named, "Tag", 0, false),
                (EntityBindingKind::Inline, "Note", 0, true),
                (
                    EntityBindingKind::PayloadUnresolved,
                    "@payload.rows",
                    0,
                    false
                ),
                (
                    EntityBindingKind::ContextUnresolved,
                    "@entity.items",
                    0,
                    false
                ),
                (EntityBindingKind::Absent, "", 0, false),
                (EntityBindingKind::Malformed, "", 0, false),
                (EntityBindingKind::None, "", 0, false),
            ]
        );
        let (mut unreal, mut found) = (Vec::new(), Findings::default());
        judge_nodes(&nodes, &mut unreal, &mut found);
        assert!(unreal.is_empty(), "{unreal:?}");
        assert_eq!(
            found.unbound,
            vec![
                "data-list[entity=Tag]",
                "data-grid[unresolved=@payload.rows]"
            ]
        );
        assert_eq!(found.access_scoped_empty, vec!["table-view[Note: @read]"]);
    }

    #[test]
    fn nodes_carry_coverage_and_trait_reference_facts() {
        let nodes = facts_of(
            serde_json::json!({ "type": "stack", "children": [
                { "type": "health-bar" },
                { "type": "radar" },
                { "type": "@config.viewPattern" },
                { "children": ["@trait.Shown", "@trait.Missing", "@trait.Drawer"] }
            ] }),
            &[],
            &[],
        );
        let facts: Vec<(&str, bool, bool, bool)> = nodes
            .iter()
            .map(|n| (n.pattern.as_str(), n.dynamic, n.supported, n.stub))
            .collect();
        assert_eq!(
            facts,
            vec![
                ("stack", false, true, false),
                ("health-bar", false, false, true),
                ("radar", false, false, false),
                ("@config.viewPattern", true, false, false),
                ("", false, true, false),
                ("typography", false, true, false),
            ],
            "a typeless node needs no renderer; @trait content is visited in place"
        );
        assert_eq!(
            nodes[4].trait_refs,
            vec![
                TraitRef {
                    name: "Shown".into(),
                    resolved: true,
                    overlay_only: false
                },
                TraitRef {
                    name: "Missing".into(),
                    resolved: false,
                    overlay_only: false
                },
                TraitRef {
                    name: "Drawer".into(),
                    resolved: false,
                    overlay_only: true
                },
            ]
        );
        let (mut unreal, mut found) = (Vec::new(), Findings::default());
        judge_nodes(&nodes, &mut unreal, &mut found);
        assert_eq!(unreal, vec!["radar"]);
        assert_eq!(found.stubbed, vec!["health-bar"]);
        assert_eq!(
            found.unresolved,
            vec!["Missing"],
            "an overlay-only embed is Pass 3's, not a miss"
        );
    }

    #[test]
    fn a_played_step_renders_the_slots_its_trait_filled() {
        let mut schema = serde_json::to_value(program(serde_json::json!({ "type": "stack", "children": [{ "type": "data-list", "entity": "Note" }, { "type": "radar" }] }))).unwrap();
        let effects =
            &mut schema["orbitals"][0]["traits"][0]["stateMachine"]["transitions"][0]["effects"];
        effects
            .as_array_mut()
            .unwrap()
            .insert(0, serde_json::json!(["fetch", "Note"]));
        let schema: OrbitalSchema = serde_json::from_value(schema).unwrap();
        let target = Target {
            supported: &["stack", "data-list"],
            stubs: &[],
        };
        let slots = played_render_facts(
            &schema,
            "NotesView",
            "INIT",
            Value::Null,
            &target,
            |s, _| seeded_kernel(s),
        )
        .unwrap();
        assert_eq!(slots.len(), 1, "{slots:?}");
        assert_eq!(
            (
                slots[0].slot.as_str(),
                slots[0].source_trait.as_str(),
                slots[0].owner_entity.as_str()
            ),
            ("main", "NotesView", "Note")
        );
        let list = slots[0]
            .nodes
            .iter()
            .find(|n| n.pattern == "data-list")
            .expect("the list node");
        assert_eq!(list.binding.kind, EntityBindingKind::Named);
        assert!(
            list.binding.rows >= 1,
            "the seeded INIT loaded rows: {list:?}"
        );
        assert!(slots[0]
            .nodes
            .iter()
            .any(|n| n.pattern == "radar" && !n.supported));
        let other = played_render_facts(
            &schema,
            "Elsewhere",
            "INIT",
            Value::Null,
            &target,
            |s, _| seeded_kernel(s),
        );
        assert!(
            other.unwrap_err().contains("no trait \"Elsewhere\""),
            "control: an unknown trait is refused"
        );
    }

    #[test]
    fn edge_an_unknown_trait_scope_is_refused() {
        let err = probe(
            "t",
            program(serde_json::json!({ "type": "typography" })),
            Some(&["Nope".to_string()]),
            &Target {
                supported: &["typography"],
                stubs: &[],
            },
        )
        .unwrap_err();
        assert!(err.contains("unknown --trait \"Nope\""), "{err}");
    }
}
