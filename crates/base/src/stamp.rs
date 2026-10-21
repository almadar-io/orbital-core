//! Phase-2 identity stamping: mint ids for authored nodes and resolve in-file
//! name references to id edges, on the already-lowered `OrbitalSchema`.
//!
//! Two invariants drive the design:
//!
//! - **Zero churn.** Given a `prior` schema, every node's id is looked up by
//!   `(kind, bakedName, parentId)` in the prior ledger and REUSED verbatim.
//!   A genuinely new node gets an id DERIVED from the program's name and its
//!   `(kind, bakedName, parentId)` (`identity::derive_node_id`), never entropy —
//!   so re-lowering the same `.lolo` with no prior at all also produces
//!   byte-identical ids (the live-behaviors model reads sources with nothing stored).
//! - **Dual-carry.** Names still carry every edge; the id sibling is populated
//!   only when the target resolves in-file. Cross-file / alias references
//!   (`Alias.traits.X`, `Alias.entity`) leave the id `None`.
//!
//! Hoisted render-embed synthetics (parser `is_inline_synthetic` traits) get a
//! DERIVED id rooted in the embedding parent (`derive_id`) instead of an
//! entropy mint, so the embed edge is recoverable from the id and re-derivation
//! never churns.

use crate::identity::{
    derive_id, derive_node_id, IdKind, IdentityLedger, LedgerEntry, LedgerOwner,
};
use crate::schema::sexpr::SExpression;
use crate::schema::types::{
    ConfigField, EntityRef, FieldDefinition, ListenSource, Orbital, OrbitalSchema, PageRef,
    TraitDefinition, TraitReference,
};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The `.orb` identity schema version stamped once ids are minted.
const SCHEMA_VERSION: u32 = 1;

/// Mint ids across `schema` and populate the dual-carry id fields + ledger.
/// `prior` is the previously-emitted schema for this source (if any); its
/// ledger anchors id reuse. `synthetic_traits` is the set of trait names the
/// parser hoisted from inline `@(...)` / JSX refs (their id derives from the
/// embedding parent, never entropy).
pub fn stamp_schema_identity(
    schema: &mut OrbitalSchema,
    prior: Option<&OrbitalSchema>,
    synthetic_traits: &HashSet<String>,
) {
    let mut minter = Minter::new(prior.and_then(|p| p.ledger.as_ref()), &schema.name);

    for orbital in &mut schema.orbitals {
        stamp_orbital(orbital, synthetic_traits, &mut minter);
    }

    schema.ledger = Some(IdentityLedger {
        schema_version: SCHEMA_VERSION,
        entries: minter.entries,
    });
    schema.schema_version = Some(SCHEMA_VERSION);
}

/// Auto-stamp on load. If `schema` carries no identity ledger — a raw,
/// hand-authored, or legacy name-only `.orb` — mint ids + side-maps in place so
/// every downstream reader (inline, resolve, codegen) sees id-covered
/// references. `.lolo`-emitted and factory-stamped schemas already carry a
/// ledger and are left untouched (zero churn). This is the intrinsic-stamping
/// chokepoint that makes name-only inputs impossible past the parse boundary,
/// so the compiler's name-fallback rewriters are dead code.
pub fn ensure_stamped(schema: &mut OrbitalSchema) {
    if schema.ledger.is_none() {
        stamp_schema_identity(schema, None, &HashSet::new());
    }
}

/// Id allocator: reuses prior ids by `(kind, bakedName, parentId)`, mints fresh
/// otherwise, and accumulates the emitted ledger. Several nodes can share one
/// key (a trait `A` in two orbitals), so each prior id is handed out once.
struct Minter<'a> {
    /// `(kind, bakedName, parentId) -> ids`, built from the prior ledger, sorted.
    prior_by_key: HashMap<(IdKind, String, Option<String>), Vec<String>>,
    prior_ledger: Option<&'a IdentityLedger>,
    /// The program's name — the scope every derived id is rooted in.
    scope: String,
    /// How many declaration nodes have been minted per `(kind, bakedName, parentId)`.
    derived: HashMap<(IdKind, String, Option<String>), usize>,
    claimed: HashSet<String>,
    entries: BTreeMap<String, LedgerEntry>,
}

impl<'a> Minter<'a> {
    fn new(prior_ledger: Option<&'a IdentityLedger>, scope: &str) -> Self {
        let mut prior_by_key: HashMap<(IdKind, String, Option<String>), Vec<String>> =
            HashMap::new();
        if let Some(ledger) = prior_ledger {
            for entry in ledger.entries.values() {
                prior_by_key
                    .entry((entry.kind, entry.baked_name.clone(), entry.parent.clone()))
                    .or_default()
                    .push(entry.id.clone());
            }
        }
        for ids in prior_by_key.values_mut() {
            ids.sort();
        }
        Minter {
            prior_by_key,
            prior_ledger,
            scope: scope.to_string(),
            derived: HashMap::new(),
            claimed: HashSet::new(),
            entries: BTreeMap::new(),
        }
    }

    /// Reuse an unclaimed prior id for `(kind, baked, parent)` — `preferred`
    /// when the prior holds it, else the first — copying its ledger row
    /// verbatim (preserving `curName`/`renames`). Returns `None` on a
    /// genuinely new node.
    fn reuse(
        &mut self,
        kind: IdKind,
        baked: &str,
        parent: Option<&str>,
        preferred: Option<&str>,
    ) -> Option<String> {
        let key = (kind, baked.to_string(), parent.map(str::to_string));
        let open: Vec<&String> = self
            .prior_by_key
            .get(&key)?
            .iter()
            .filter(|id| !self.claimed.contains(*id))
            .collect();
        let id = preferred
            .and_then(|p| open.iter().find(|id| id.as_str() == p))
            .or_else(|| open.first())
            .map(|id| (*id).clone())?;
        self.claimed.insert(id.clone());
        if let Some(entry) = self.prior_ledger.and_then(|l| l.entries.get(&id)) {
            self.entries.insert(id.clone(), entry.clone());
        }
        Some(id)
    }

    fn record(&mut self, id: String, kind: IdKind, baked: &str, parent: Option<&str>) {
        self.claimed.insert(id.clone());
        self.entries.insert(
            id.clone(),
            LedgerEntry {
                id,
                kind,
                baked_name: baked.to_string(),
                cur_name: baked.to_string(),
                renames: Vec::new(),
                owner: LedgerOwner::Workspace,
                parent: parent.map(str::to_string),
            },
        );
    }

    /// Reuse a prior id for a declaration node, else derive it from the program
    /// scope and the node's `(kind, bakedName, parentId)` and its ordinal among
    /// same-keyed nodes — never entropy.
    fn mint(&mut self, kind: IdKind, baked: &str, parent: Option<&str>) -> String {
        let seen = self
            .derived
            .entry((kind, baked.to_string(), parent.map(str::to_string)))
            .or_insert(0);
        let mut ordinal = *seen;
        *seen += 1;
        let derived = derive_node_id(kind, &self.scope, baked, parent, ordinal);
        if let Some(id) = self.reuse(kind, baked, parent, Some(&derived)) {
            return id;
        }
        let mut id = derived;
        while self.claimed.contains(&id) {
            ordinal += 1;
            id = derive_node_id(kind, &self.scope, baked, parent, ordinal);
        }
        self.record(id.clone(), kind, baked, parent);
        id
    }

    /// Derive (or reuse) a parent-rooted id for a hoisted synthetic node.
    fn derive(&mut self, kind: IdKind, baked: &str, parent_id: &str) -> String {
        if let Some(id) = self.reuse(kind, baked, None, None) {
            return id;
        }
        let id = derive_id(parent_id, baked);
        self.record(id.clone(), kind, baked, Some(parent_id));
        id
    }
}

/// Per-orbital resolution index built during declaration minting.
#[derive(Default)]
struct OrbitalIds {
    /// Entity name -> id (primary + auxiliary, in this orbital's scope).
    entities: HashMap<String, String>,
    /// Trait name -> id (inline traits declared in this orbital).
    traits: HashMap<String, String>,
    /// trait id -> (event name -> event id).
    trait_events: HashMap<String, HashMap<String, String>>,
    /// Event name -> some event id (any owner; best-effort cross-trait resolve).
    events_any: HashMap<String, String>,
}

fn stamp_orbital(orbital: &mut Orbital, synthetic_traits: &HashSet<String>, minter: &mut Minter) {
    orbital.id = Some(minter.mint(IdKind::Orbital, &orbital.name, None));

    let mut ids = OrbitalIds::default();

    // Entities (primary + auxiliary).
    if let EntityRef::Inline(entity) = &mut orbital.entity {
        let id = minter.mint(IdKind::Entity, &entity.name, None);
        ids.entities.insert(entity.name.clone(), id.clone());
        entity.id = Some(id);
    }
    if let Some(aux) = orbital.auxiliary_entities.as_mut() {
        for a in aux.iter_mut() {
            if let EntityRef::Inline(entity) = a {
                let id = minter.mint(IdKind::Entity, &entity.name, None);
                ids.entities.insert(entity.name.clone(), id.clone());
                entity.id = Some(id);
            }
        }
    }

    // Traits — mint declaration ids in dependency order so a hoisted synthetic
    // derives from its (already-minted) embedding parent. Discover the embed
    // edges by scanning each inline trait's render effects for `@trait.<child>`
    // strings that name a synthetic sibling.
    let parent_of = embed_parents(orbital, synthetic_traits);
    mint_trait_ids(orbital, synthetic_traits, &parent_of, &mut ids, minter);

    // Per-trait event ids (parent = the owning trait id).
    for tr in &mut orbital.traits {
        if let TraitReference::Inline(td) = tr {
            let Some(trait_id) = td.id.clone() else {
                continue;
            };
            let mut event_ids: HashMap<String, String> = HashMap::new();
            for ev in td.state_machine.events.iter().flatten() {
                let id = minter.mint(IdKind::Event, &ev.key, Some(&trait_id));
                ids.events_any.entry(ev.key.clone()).or_insert(id.clone());
                event_ids.insert(ev.key.clone(), id);
            }
            ids.trait_events.insert(trait_id, event_ids);
        }
    }

    // Per-trait entity-ref + trait-embed id side-maps (V4 leverage-ids). Scan each
    // inline trait's serialized body for entity names (`ids.entities`) and bare
    // `@trait.<sibling>` embed tokens (`ids.traits`) and record name→id, so readers
    // resolve these bare-`SExpression::String` positions by stable id (else name).
    // This is the coverage that lets the entity/embed name-rewriters be deleted: on a
    // rename the token keeps its old name AND the map key stays old → old-name → map →
    // stable id → the (renamed) declaration. A superset is safe — readers consult the
    // map only at genuine ref positions, so extra entries are inert; a MISSING entry is
    // the only hazard, so the scan errs broad.
    for tr in &mut orbital.traits {
        let TraitReference::Inline(td) = tr else {
            continue;
        };
        let Ok(body) = serde_json::to_value(&*td) else {
            continue;
        };
        let mut ent_names: HashSet<String> = HashSet::new();
        collect_entity_ref_names(&body, &ids.entities, &mut ent_names);
        let ent_names: std::collections::BTreeSet<String> = ent_names.into_iter().collect();
        let entity_map: HashMap<String, String> = ent_names
            .into_iter()
            .filter_map(|n| ids.entities.get(&n).map(|id| (n, id.clone())))
            .collect();
        td.entity_ref_ids = (!entity_map.is_empty()).then_some(entity_map);

        let mut embed_names: HashSet<String> = HashSet::new();
        collect_embed_ref_names(&body, &ids.traits, &mut embed_names);
        let embed_names: std::collections::BTreeSet<String> = embed_names.into_iter().collect();
        let embed_map: HashMap<String, String> = embed_names
            .into_iter()
            .filter_map(|n| ids.traits.get(&n).map(|id| (n, id.clone())))
            .collect();
        td.trait_embed_ids = (!embed_map.is_empty()).then_some(embed_map);
    }

    // V4-W5 config-ref id side-map — covers BOTH inline and REFERENCE traits
    // (the entity_ref/trait_embed loop above skips Reference variants, but a ref
    // trait's `config` is exactly where an entity/trait/event NAME is passed).
    // A config field typed `entity`/`trait`/`event` holds a reference: resolve
    // its value name to the node's id so readers rewrite the value to the node's
    // CURRENT name by id. The TYPE is the contract — no value string-scanning.
    for tr in &mut orbital.traits {
        let (config, definer_knobs) = match tr {
            TraitReference::Inline(td) => {
                let definer_knobs = crate::schema::types::definer_knobs_for_trait(td);
                (td.config.as_mut(), definer_knobs)
            }
            // A Reference trait's own body carries no emits/stateMachine to
            // derive definer knobs from — its `config` overrides only reach
            // an already-resolved base trait at inline time, so there is
            // nothing to skip here (probe P2).
            TraitReference::Reference { config, .. } => (config.as_mut(), HashSet::new()),
        };
        stamp_config_ref_ids(config, &ids, &definer_knobs);
    }

    // Pages.
    for page in orbital.pages.iter_mut().flatten() {
        if let PageRef::Inline(p) = page {
            p.id = Some(minter.mint(IdKind::Page, &p.name, None));
        }
    }

    // Reference edges (dual-carry id siblings).
    resolve_orbital_edges(orbital, &ids);
}

/// Map each hoisted-synthetic trait name to its unique embedding parent trait
/// name, discovered from `@trait.<synthetic>` strings inside parent render
/// effects. A synthetic referenced by 0 or >1 parents is left unmapped (it then
/// mints normally rather than deriving from an ambiguous root).
fn embed_parents(orbital: &Orbital, synthetic_traits: &HashSet<String>) -> HashMap<String, String> {
    let mut candidates: HashMap<String, Vec<String>> = HashMap::new();
    for tr in &orbital.traits {
        let TraitReference::Inline(td) = tr else {
            continue;
        };
        let mut refs: HashSet<String> = HashSet::new();
        for transition in &td.state_machine.transitions {
            for effect in transition.effects.iter().flatten() {
                collect_trait_refs(effect, &mut refs);
            }
        }
        for tick in td.ticks.iter().flatten() {
            for effect in &tick.effects {
                collect_trait_refs(effect, &mut refs);
            }
        }
        for child in refs {
            if synthetic_traits.contains(&child) && child != td.name {
                candidates.entry(child).or_default().push(td.name.clone());
            }
        }
    }
    candidates
        .into_iter()
        .filter_map(|(child, parents)| (parents.len() == 1).then(|| (child, parents[0].clone())))
        .collect()
}

/// Collect bare `@trait.<Name>` sibling references (no dotted alias path) from
/// an S-expression tree.
fn collect_trait_refs(expr: &SExpression, out: &mut HashSet<String>) {
    match expr {
        SExpression::String(s) => {
            if let Some(name) = s.strip_prefix("@trait.") {
                if !name.is_empty() && !name.contains('.') {
                    out.insert(name.to_string());
                }
            }
        }
        SExpression::Array(items) => items.iter().for_each(|i| collect_trait_refs(i, out)),
        SExpression::Object(map) => map.values().for_each(|v| collect_trait_refs(v, out)),
        _ => {}
    }
}

/// V4-W5: stamp the `refId` id sibling on each reference-typed config field of a
/// trait's `config` map. A field typed `entity`/`trait`/`event` holds a
/// reference NAME as its `default`; resolve that name to the node's id in this
/// orbital's scope (`ids.entities` / `ids.traits` / `ids.events_any`) so readers
/// resolve the value to the node's CURRENT name by id under a declaration-only
/// rename. `@trait.<Name>` embed values are stripped to their bare name; `@`
/// bindings/forwards for entity/event values are left unstamped (they resolve at
/// inline time against the composing scope, not to an in-file declaration).
///
/// `definer_knobs` (A-UNIFY) are event-typed knobs wired in a DEFINER
/// position — their default CREATES an event name rather than referencing an
/// existing one, so it must never resolve against `ids.events_any`: a
/// definer default that happened to collide with a SIBLING event's literal
/// name would false-bind, and a later rename of that sibling would then
/// rewrite this knob's DEFINITION out from under it via identity-normalize.
fn stamp_config_ref_ids(
    config: Option<&mut HashMap<String, ConfigField>>,
    ids: &OrbitalIds,
    definer_knobs: &HashSet<String>,
) {
    let Some(config) = config else { return };
    for (name, cf) in config.iter_mut() {
        let raw = match &cf.default {
            Some(SExpression::String(s)) => s.as_str(),
            _ => continue,
        };
        let resolved = match cf.field_type.as_str() {
            "entity" if !raw.starts_with('@') => ids.entities.get(raw),
            "event" if !raw.starts_with('@') && !definer_knobs.contains(name) => {
                ids.events_any.get(raw)
            }
            "trait" => raw.strip_prefix("@trait.").and_then(|n| ids.traits.get(n)),
            _ => None,
        };
        if let Some(id) = resolved {
            cf.ref_id = Some(id.clone());
        }
    }
}

/// Collect every entity name (present in `entities`) referenced anywhere in a
/// serialized trait body: as an `@Name`/`@Name.field` binding token, a bare
/// positional/prop slot value (`["fetch","Name",…]`, `{entity:"Name"}`), or a
/// `[Name]` payload list type. Mirrors the position set of the JS
/// `rewriteEntityInSExpr` rewriter, but broadened to a name-set (a superset is
/// safe — see the mint comment).
fn collect_entity_ref_names(
    v: &serde_json::Value,
    entities: &HashMap<String, String>,
    out: &mut HashSet<String>,
) {
    match v {
        serde_json::Value::String(s) => {
            if let Some(rest) = s.strip_prefix('@') {
                let name = rest.split('.').next().unwrap_or("");
                if entities.contains_key(name) {
                    out.insert(name.to_string());
                }
            }
            if entities.contains_key(s) {
                out.insert(s.clone());
            }
            if let Some(inner) = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
                if entities.contains_key(inner) {
                    out.insert(inner.to_string());
                }
            }
        }
        serde_json::Value::Array(a) => a
            .iter()
            .for_each(|x| collect_entity_ref_names(x, entities, out)),
        serde_json::Value::Object(o) => o
            .values()
            .for_each(|x| collect_entity_ref_names(x, entities, out)),
        _ => {}
    }
}

/// Collect every bare `@trait.<Name>` embed token (no dotted alias path) whose
/// `<Name>` is a sibling trait (`traits`), from a serialized trait body. Same
/// token shape as `collect_trait_refs`, applied over the whole body (embeds also
/// live in `config` values, not only effects).
fn collect_embed_ref_names(
    v: &serde_json::Value,
    traits: &HashMap<String, String>,
    out: &mut HashSet<String>,
) {
    match v {
        serde_json::Value::String(s) => {
            if let Some(name) = s.strip_prefix("@trait.") {
                if !name.is_empty() && !name.contains('.') && traits.contains_key(name) {
                    out.insert(name.to_string());
                }
            }
        }
        serde_json::Value::Array(a) => a
            .iter()
            .for_each(|x| collect_embed_ref_names(x, traits, out)),
        serde_json::Value::Object(o) => o
            .values()
            .for_each(|x| collect_embed_ref_names(x, traits, out)),
        _ => {}
    }
}

/// Mint trait declaration ids, deriving hoisted synthetics from their parent
/// once the parent id is known (iterative fixpoint over the embed edges).
fn mint_trait_ids(
    orbital: &mut Orbital,
    synthetic_traits: &HashSet<String>,
    parent_of: &HashMap<String, String>,
    ids: &mut OrbitalIds,
    minter: &mut Minter,
) {
    let mut pending: Vec<usize> = orbital
        .traits
        .iter()
        .enumerate()
        .filter_map(|(i, tr)| matches!(tr, TraitReference::Inline(_)).then_some(i))
        .collect();

    loop {
        let mut progressed = false;
        let mut still_pending = Vec::new();
        for idx in pending {
            let name = match &orbital.traits[idx] {
                TraitReference::Inline(td) => td.name.clone(),
                _ => continue,
            };
            let derive_parent = synthetic_traits
                .contains(&name)
                .then(|| parent_of.get(&name))
                .flatten();
            let id = match derive_parent {
                Some(parent_name) => match ids.traits.get(parent_name) {
                    Some(parent_id) => minter.derive(IdKind::Trait, &name, &parent_id.clone()),
                    None => {
                        // Parent not minted yet; defer this round.
                        still_pending.push(idx);
                        continue;
                    }
                },
                None => minter.mint(IdKind::Trait, &name, None),
            };
            if let TraitReference::Inline(td) = &mut orbital.traits[idx] {
                td.id = Some(id.clone());
            }
            ids.traits.insert(name, id);
            progressed = true;
        }
        if still_pending.is_empty() {
            break;
        }
        if !progressed {
            // Cycle / missing parent — mint the remainder normally so no node
            // is left id-less.
            for idx in &still_pending {
                if let TraitReference::Inline(td) = &orbital.traits[*idx] {
                    let name = td.name.clone();
                    let id = minter.mint(IdKind::Trait, &name, None);
                    if let TraitReference::Inline(td) = &mut orbital.traits[*idx] {
                        td.id = Some(id.clone());
                    }
                    ids.traits.insert(name, id);
                }
            }
            break;
        }
        pending = still_pending;
    }

    // Reference-form declarations (`trait X = Alias.traits.Y` — the entire
    // composed organism surface) are local nodes too: mint their DECLARATION
    // ids (`id`, distinct from the target's `ref_id`) so page trait refs and
    // `@trait.X` embeds resolve by id across `name:` overrides — this was the
    // one declaration class without an id, which stranded every page ref on a
    // legal rename (FACTORY-PAGE-TRAIT-NAME-COUPLING, battery 2026-07-20).
    // The local name is the explicit `name:` else the reference's last
    // `.`-segment; a name already minted is skipped (first declaration wins).
    for tr in orbital.traits.iter_mut() {
        let TraitReference::Reference {
            reference,
            id,
            name,
            ..
        } = tr
        else {
            continue;
        };
        let local_name = name.clone().or_else(|| {
            reference
                .as_ref()
                .map(|r| r.rsplit('.').next().unwrap_or(r).to_string())
        });
        let Some(local_name) = local_name else {
            continue;
        };
        if local_name.is_empty() || ids.traits.contains_key(&local_name) {
            continue;
        }
        let minted = minter.mint(IdKind::Trait, &local_name, None);
        *id = Some(minted.clone());
        ids.traits.insert(local_name, minted);
    }
}

/// Populate the dual-carry id siblings on every in-file reference surface.
fn resolve_orbital_edges(orbital: &mut Orbital, ids: &OrbitalIds) {
    // Extends form on the primary entity (`{ extends: "Alias.entity" }`) — only
    // resolves for a bare in-file entity name (dotted alias paths stay None).
    if let EntityRef::Extends(e) = &mut orbital.entity {
        e.extends_id = ids.entities.get(&e.reference).cloned();
    }

    // Entity relation targets (recurse into nested array/object fields).
    if let EntityRef::Inline(entity) = &mut orbital.entity {
        for f in &mut entity.fields {
            resolve_field_relations(f, ids);
        }
    }

    for tr in orbital.traits.iter_mut() {
        resolve_trait_ref_edges(tr, ids);
    }

    for page in orbital.pages.iter_mut().flatten() {
        match page {
            PageRef::Inline(p) => {
                // `PageDefinition` carries no `primaryEntityId` sibling yet
                // (Phase-1 note: page-level primary entity is un-id-carried on
                // both paths); dual-carry keeps the name edge until core adds it.
                if let Some(traits) = p.traits.as_mut() {
                    for tr in traits {
                        resolve_trait_ref_edges(tr, ids);
                    }
                }
            }
            PageRef::Object(o) => {
                o.linked_entity_id = o
                    .linked_entity
                    .as_ref()
                    .and_then(|n| ids.entities.get(n).cloned());
                if let Some(traits) = o.traits.as_mut() {
                    let mut ref_ids = Vec::with_capacity(traits.len());
                    for tr in traits.iter_mut() {
                        resolve_trait_ref_edges(tr, ids);
                        ref_ids.push(trait_ref_id_of(tr, ids).unwrap_or_default());
                    }
                    if ref_ids.iter().any(|s| !s.is_empty()) {
                        o.trait_ref_ids = Some(ref_ids);
                    }
                }
            }
            PageRef::Reference(_) => {}
        }
    }
}

/// Resolve a single relation field's `entity_id`, recursing into array items
/// and object properties.
fn resolve_field_relations(field: &mut FieldDefinition, ids: &OrbitalIds) {
    if let Some(rel) = field.relation.as_mut() {
        rel.entity_id = ids.entities.get(&rel.entity).cloned();
    }
    if let Some(items) = field.items.as_mut() {
        resolve_field_relations(items, ids);
    }
    if let Some(props) = field.properties.as_mut() {
        for p in props.values_mut() {
            resolve_field_relations(p, ids);
        }
    }
}

/// The trait-id an in-file reference resolves to (for a page's `trait_ref_ids`
/// mirror). `None` for cross-file/alias refs.
fn trait_ref_id_of(tr: &TraitReference, ids: &OrbitalIds) -> Option<String> {
    match tr {
        TraitReference::Inline(td) => td.id.clone(),
        TraitReference::Reference { reference, .. } => reference
            .as_ref()
            .filter(|r| !r.contains('.'))
            .and_then(|r| ids.traits.get(r).cloned()),
    }
}

fn resolve_trait_ref_edges(tr: &mut TraitReference, ids: &OrbitalIds) {
    match tr {
        TraitReference::Inline(td) => resolve_inline_trait_edges(td, ids),
        TraitReference::Reference {
            reference,
            ref_id,
            linked_entity,
            linked_entity_id,
            ..
        } => {
            // Bare in-file trait name resolves (page `-> TraitName`); dotted
            // `Alias.traits.X` import paths stay None.
            *ref_id = reference
                .as_ref()
                .filter(|r| !r.contains('.'))
                .and_then(|r| ids.traits.get(r).cloned());
            *linked_entity_id = linked_entity
                .as_ref()
                .and_then(|n| ids.entities.get(n).cloned());
        }
    }
}

fn resolve_inline_trait_edges(td: &mut TraitDefinition, ids: &OrbitalIds) {
    td.linked_entity_id = td
        .linked_entity
        .as_ref()
        .and_then(|n| ids.entities.get(n).cloned());

    let own_events = td.id.as_ref().and_then(|id| ids.trait_events.get(id));

    if let Some(emits) = td.emits.as_mut() {
        for e in emits {
            e.event_id = own_events.and_then(|m| m.get(&e.event).cloned());
        }
    }
    if let Some(listens) = td.listens.as_mut() {
        for l in listens {
            // The listened-for event belongs to a source trait (cross-orbital);
            // resolve best-effort against any same-named event in this file.
            l.event_id = ids.events_any.get(&l.event).cloned();
            // The internal event a listen triggers is this trait's own event.
            if let Some(trigger) = &l.triggers {
                l.triggers_id = own_events
                    .and_then(|m| m.get(trigger).cloned())
                    .or_else(|| ids.events_any.get(trigger).cloned());
            }
            if let Some(source) = l.source.as_mut() {
                resolve_listen_source(source, ids);
            }
        }
    }

    // V4 event-dispatch layer: stamp each declared event's own id and each
    // transition's triggering-event id from this trait's minted event ids. The
    // transition→event edge is the id-keyed dispatch target the runtimes match
    // on (id-primary, name fallback) once names are stripped.
    if let Some(oe) = own_events {
        for ev in td.state_machine.events.iter_mut().flatten() {
            ev.id = oe.get(&ev.key).cloned();
        }
        for t in td.state_machine.transitions.iter_mut() {
            t.event_id = oe.get(&t.event).cloned();
        }
    }
}

fn resolve_listen_source(source: &mut ListenSource, ids: &OrbitalIds) {
    match source {
        ListenSource::Trait {
            trait_name,
            trait_id,
        } => {
            *trait_id = ids.traits.get(trait_name).cloned();
        }
        ListenSource::Orbital {
            orbital: _,
            orbital_id: _,
            trait_name,
            trait_id,
        } => {
            // Cross-orbital source trait: resolve in-file if the named trait is
            // local; orbital_id resolution is a cross-file concern (left None).
            *trait_id = ids.traits.get(trait_name).cloned();
        }
        ListenSource::Any => {}
    }
}

#[cfg(test)]
mod tests {
    use super::stamp_schema_identity;
    use crate::schema::types::{OrbitalSchema, TraitReference};
    use std::collections::HashSet;

    fn orbital(name: &str, entity: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "entity": { "name": entity, "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "A", "linkedEntity": entity, "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "events": [{ "key": "INIT", "name": "INIT" }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
            } }],
            "pages": []
        })
    }

    fn program(orbitals: Vec<serde_json::Value>) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({ "name": "P", "orbitals": orbitals }))
            .expect("schema")
    }

    fn stamped(orbitals: Vec<serde_json::Value>, prior: Option<&OrbitalSchema>) -> OrbitalSchema {
        let mut schema = program(orbitals);
        stamp_schema_identity(&mut schema, prior, &HashSet::new());
        schema
    }

    fn trait_ids(schema: &OrbitalSchema) -> Vec<String> {
        schema
            .orbitals
            .iter()
            .flat_map(|o| o.traits.iter())
            .filter_map(|tr| match tr {
                TraitReference::Inline(td) => td.id.clone(),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn restamp_keeps_same_named_traits_in_two_orbitals_distinct_and_stable() {
        let two = || vec![orbital("O", "E"), orbital("O2", "E2")];
        let first = stamped(two(), None);
        let ids = trait_ids(&first);
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        let again = stamped(two(), Some(&first));
        assert_eq!(trait_ids(&again), ids);
        assert_eq!(
            again.ledger.as_ref().map(|l| l.entries.len()),
            first.ledger.as_ref().map(|l| l.entries.len())
        );
    }

    #[test]
    fn restamp_adding_a_same_named_trait_mints_a_fresh_id() {
        let first = stamped(vec![orbital("O", "E")], None);
        let again = stamped(vec![orbital("O", "E"), orbital("O2", "E2")], Some(&first));
        let ids = trait_ids(&again);
        assert_eq!(ids[0], trait_ids(&first)[0]);
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn control_single_trait_restamps_to_its_prior_id() {
        let first = stamped(vec![orbital("O", "E")], None);
        let again = stamped(vec![orbital("O", "E")], Some(&first));
        assert_eq!(trait_ids(&again), trait_ids(&first));
    }
}
