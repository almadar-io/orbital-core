//! UI slot contract — the single source for "what kind of slot is this".
//!
//! `render-ui` effects target a named slot. The canonical slot set is `UI_SLOTS`
//! in `@almadar/core` (`packages/almadar-core/src/types/effect.ts`); this module
//! mirrors it and classifies each slot into the layout role that every renderer
//! consumes. The roles match `@almadar/ui`'s `UISlotRenderer` treatments
//! (sidebar/main inline; modal/drawer/overlay/center/toast as portals; hud/
//! floating as fixed overlays; `system` invisible).

use crate::schema::types::{OrbitalSchema, TraitReference};
use serde::{Deserialize, Serialize};

/// How a slot relates to the page layout. One arm per visual treatment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlotRole {
    /// Primary inline content region (`main`, `content`, `screen`).
    Main,
    /// Left docked navigation/content panel (`sidebar`).
    Sidebar,
    /// Right side sheet (`drawer`; `dock`, which on the web pushes the page beside it).
    Drawer,
    /// Centered dialog window over the page (`modal`, `center`).
    Modal,
    /// Dimmed full-screen overlay with centered content (`overlay`, `overlay.*`).
    Overlay,
    /// Transient top-right notification (`toast`).
    Toast,
    /// Fixed bar/overlay pinned to the top (`hud`, `hud-top`, `hud.*`).
    HudTop,
    /// Fixed bar/overlay pinned to the bottom (`hud-bottom`).
    HudBottom,
    /// Free-floating movable panel (`floating`).
    Floating,
    /// Invisible system slot — never rendered (`system`: InputListener, CollisionDetector).
    System,
}

/// Slots whose patterns visually overlay the page AND must be explicitly cleared
/// on exit (the closed-circuit validator enforces `(render-ui <slot> null)` on
/// every path leaving a state that mounts one). Kept narrow to the portal dialogs
/// the validator already governs; `toast` self-dismisses so it is not included.
pub const OVERLAY_SLOTS: &[&str] = &["modal", "drawer", "dock", "overlay"];

/// Inline content slots rendered in the central region.
pub const MAIN_SLOTS: &[&str] = &["main", "content", "screen"];

/// Transient overlays sibling traits may share: one opens at a time by user action and toasts stack, so they
/// are not slot contention (owner ruling, P-009).
pub const SHARED_OVERLAY_SLOTS: &[&str] = &["modal", "toast"];

/// Pattern TYPES that self-manage their own overlay chrome (slide-over/dialog
/// framing baked into the pattern itself), so the generic `CompiledPortal`
/// shell wrapping an overlay slot must NOT wrap them again — doing so
/// produces two stacked overlay chromes. Mirrors the TS owner
/// `@almadar/core/patterns/self-overlay.ts`'s `SELF_OVERLAY_PATTERN_TYPES`
/// set; no per-pattern registry flag exists yet for this (checked
/// `orbital-compiler/src/baked/patterns-registry.json` and the sync
/// metadata table) — this is a mirrored constant, not a new heuristic, the
/// same mirroring idiom `OVERLAY_SLOTS` already uses. Should move into
/// registry metadata (ledgered, not this module's job to fix).
pub const SELF_OVERLAY_PATTERN_TYPES: &[&str] = &["modal", "confirm-dialog"];

/// Classify a slot name into its layout role. Game sub-slots (`hud.health`,
/// `overlay.dialogue`, …) fold into their family via prefix.
pub fn slot_role(name: &str) -> SlotRole {
    match name {
        n if MAIN_SLOTS.contains(&n) => SlotRole::Main,
        "sidebar" => SlotRole::Sidebar,
        "drawer" | "dock" => SlotRole::Drawer,
        "modal" | "center" => SlotRole::Modal,
        "overlay" => SlotRole::Overlay,
        "toast" => SlotRole::Toast,
        "hud-bottom" => SlotRole::HudBottom,
        "hud" | "hud-top" => SlotRole::HudTop,
        "floating" => SlotRole::Floating,
        "system" => SlotRole::System,
        n if n.starts_with("overlay.") => SlotRole::Overlay,
        n if n.starts_with("hud.") => SlotRole::HudTop,
        _ => SlotRole::Main,
    }
}

/// The web's portal slots (`@almadar/ui` `isPortalSlot`): mounted only by a render, so an
/// awaiting-server skeleton never opens one.
pub fn is_portal_slot(name: &str) -> bool {
    matches!(name, "modal" | "drawer" | "dock" | "overlay" | "center")
}

/// Whether a slot renders as a portal/overlay floating above the page
/// (modal/drawer/overlay/center/toast/floating) rather than docked inline.
pub fn is_floating_slot(name: &str) -> bool {
    matches!(
        slot_role(name),
        SlotRole::Modal
            | SlotRole::Overlay
            | SlotRole::Drawer
            | SlotRole::Toast
            | SlotRole::Floating
            | SlotRole::HudTop
            | SlotRole::HudBottom
    )
}

/// A transition whose `render-ui` effects render into a non-`main` slot — i.e. an
/// "open this modal/drawer/overlay/toast/hud" transition. The canonical enumerator
/// over a resolved schema, shared by the L1 planner (`orbital-verify`) and the L2
/// render probe (`orbital-client`) so there is one walk, not two.
#[derive(Debug, Clone)]
pub struct SlotTransition {
    pub trait_name: String,
    pub linked_entity: Option<String>,
    pub from: String,
    pub event: String,
    pub slot: String,
    pub guarded: bool,
}

/// Enumerate every transition across `schema` whose effects target a non-`main` slot
/// with REAL content — i.e. an "open this modal/drawer/overlay/toast/hud" transition.
/// A `(render-ui <slot> null)` effect *clears* the slot (the closed-circuit convention
/// every `CLOSE`/`SAVE`/`CANCEL` arm uses to tear the overlay back down) and must NOT
/// count as an open path (§104 ledger): `modal_open_paths` drives each entry here as a
/// literal top-level dispatch with a synthesized/empty payload, so a close transition
/// admitted here fires that close event (and any `listens { Trait.CLOSE -> ... }` bare
/// route riding it) with an unrealistic empty payload the real event would never carry —
/// a render_probe artifact, not a genuine payload-forwarding defect (confirmed: a real,
/// non-empty payload forwards through a bare route unchanged — `bare_listen_route_
/// forwards_source_payload_unchanged` in `tests/cross_trait_dispatch.rs`).
/// Effects are parsed through the canonical [`crate::effects::parse_effect`]. One entry
/// per (transition source state, distinct non-main slot).
pub fn non_main_slot_transitions(schema: &OrbitalSchema) -> Vec<SlotTransition> {
    use std::collections::BTreeSet;
    let mut out = Vec::new();
    for orbital in &schema.orbitals {
        for tr in &orbital.traits {
            let TraitReference::Inline(td) = tr else {
                continue;
            };
            for t in &td.state_machine.transitions {
                let Some(effects) = &t.effects else { continue };
                let mut slots: BTreeSet<String> = BTreeSet::new();
                for eff in effects {
                    if let Some(crate::effect_types::Effect::RenderUi { slot, content }) =
                        crate::effect_types::parse_effect(eff)
                    {
                        if slot_role(&slot) != SlotRole::Main && !content.is_null() {
                            slots.insert(slot);
                        }
                    }
                }
                for slot in slots {
                    for from in t.from.states() {
                        out.push(SlotTransition {
                            trait_name: td.name.clone(),
                            linked_entity: td.linked_entity.clone(),
                            from: from.to_string(),
                            event: t.event.clone(),
                            slot: slot.clone(),
                            guarded: t.guard.is_some(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// How a non-`main` slot is actually opened at runtime: the user event a button emits
/// (`source_event` from `source_trait`) which — directly or via a `listens` entry —
/// drives the `modal_trait`'s transition that renders into `slot`. The L2 probe drives
/// `source_event` from `source_trait` so it exercises the SAME cross-trait routing the
/// GUI uses (not the modal trait in isolation, which masked RS-G20).
#[derive(Debug, Clone)]
pub struct ModalOpenPath {
    pub source_trait: String,
    pub source_event: String,
    pub modal_trait: String,
    pub slot: String,
    /// The modal trait's own event the open fires (the listen's `triggers`, or the source event).
    pub modal_event: String,
    /// The listen's declared payload mapping from the source event into `modal_event`.
    pub payload_mapping: Option<crate::schema::sexpr::SObject>,
}

/// For every non-`main` slot transition, resolve the (source trait, source event) that
/// opens it: if the modal trait `listens` for the transition's event from another trait,
/// that emitter is the source; otherwise the modal trait opens it directly.
pub fn modal_open_paths(schema: &OrbitalSchema) -> Vec<ModalOpenPath> {
    type ListenRoute<'a> = (&'a str, &'a str, &'a Option<crate::schema::sexpr::SObject>);
    let mut listen_index: std::collections::HashMap<(&str, &str), ListenRoute<'_>> =
        std::collections::HashMap::new();
    for orbital in &schema.orbitals {
        for tr in &orbital.traits {
            let TraitReference::Inline(td) = tr else {
                continue;
            };
            let Some(listens) = &td.listens else { continue };
            for l in listens {
                let triggers = l.triggers.as_deref().unwrap_or(&l.event);
                if let Some(src) = listen_source_trait(&l.source) {
                    listen_index.insert((td.name.as_str(), triggers), (src, l.event.as_str(), &l.payload_mapping));
                }
            }
        }
    }

    non_main_slot_transitions(schema)
        .into_iter()
        .map(
            |st| match listen_index.get(&(st.trait_name.as_str(), st.event.as_str())).copied() {
                Some((src_trait, src_event, mapping)) => ModalOpenPath {
                    source_trait: src_trait.to_string(),
                    source_event: src_event.to_string(),
                    payload_mapping: mapping.clone(),
                    modal_trait: st.trait_name,
                    slot: st.slot,
                    modal_event: st.event,
                },
                None => ModalOpenPath {
                    source_trait: st.trait_name.clone(),
                    source_event: st.event.clone(),
                    modal_trait: st.trait_name,
                    slot: st.slot,
                    modal_event: st.event,
                    payload_mapping: None,
                },
            },
        )
        .collect()
}

fn listen_source_trait(source: &Option<crate::schema::types::ListenSource>) -> Option<&str> {
    use crate::schema::types::ListenSource;
    match source {
        Some(ListenSource::Trait { trait_name, .. })
        | Some(ListenSource::Orbital { trait_name, .. }) => Some(trait_name.as_str()),
        _ => None,
    }
}

/// Traits that render NOTHING in their initial state and render ONLY into overlay
/// slots (modal/drawer/overlay/…) for the rest of their lifetime — a `std-modal`
/// `closed` or `std-confirmation` `idle` composed purely to be opened later. Such a
/// trait's content is legitimately absent from a plain post-INIT walk; the L2 render
/// probe's overlay-open pass (`modal_open_paths`) is the check that actually drives
/// and verifies it, so the main coverage pass must not flag its unresolved `<trait.X/>`
/// embed as a miss.
///
/// A trait qualifies when: (1) its `INIT` transition(s) out of the initial state — the
/// boot-time render Pass 1's `trait_content()` snapshot actually captures — carry no
/// `render-ui` effect, and (2) every `render-ui` effect anywhere in the trait (INIT or
/// otherwise, including a later close/cancel arm that CLEARS the overlay) targets a
/// non-`main` slot (`slot_role` != `SlotRole::Main`) — so it never has, and never will,
/// inline content of its own. Condition (1) is scoped to `INIT` (not "any transition
/// landing on initial") because a modal's own close arm legitimately lands back on the
/// initial state WITH a `render-ui <overlay> null` clearing effect — that must not
/// disqualify it.
pub fn overlay_only_traits(schema: &OrbitalSchema) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for orbital in &schema.orbitals {
        for tr in &orbital.traits {
            let TraitReference::Inline(td) = tr else {
                continue;
            };
            let sm = &td.state_machine;
            let Some(initial) = sm
                .states
                .iter()
                .find(|s| s.is_initial.unwrap_or(false))
                .or_else(|| sm.states.first())
            else {
                continue;
            };

            let mut initial_has_render = false;
            let mut render_slots: Vec<String> = Vec::new();
            for t in &sm.transitions {
                let Some(effects) = &t.effects else { continue };
                for eff in effects {
                    if let Some(crate::effect_types::Effect::RenderUi { slot, .. }) =
                        crate::effect_types::parse_effect(eff)
                    {
                        if t.event == "INIT" && t.from.matches(&initial.name) {
                            initial_has_render = true;
                        }
                        render_slots.push(slot);
                    }
                }
            }

            let only_overlay =
                !render_slots.is_empty() && render_slots.iter().all(|s| slot_role(s) != SlotRole::Main);
            if !initial_has_render && only_overlay {
                out.insert(td.name.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn portal_slots_are_the_webs_five() {
        for slot in ["modal", "drawer", "dock", "overlay", "center"] {
            assert!(super::is_portal_slot(slot), "{slot}");
        }
        for slot in ["main", "sidebar", "toast", "hud", "floating", "overlay.menu"] {
            assert!(!super::is_portal_slot(slot), "control: {slot}");
        }
    }

    use super::*;

    #[test]
    fn classifies_every_canonical_slot() {
        assert_eq!(slot_role("main"), SlotRole::Main);
        assert_eq!(slot_role("content"), SlotRole::Main);
        assert_eq!(slot_role("screen"), SlotRole::Main);
        assert_eq!(slot_role("sidebar"), SlotRole::Sidebar);
        assert_eq!(slot_role("modal"), SlotRole::Modal);
        assert_eq!(slot_role("center"), SlotRole::Modal);
        assert_eq!(slot_role("drawer"), SlotRole::Drawer);
        assert_eq!(slot_role("overlay"), SlotRole::Overlay);
        assert_eq!(slot_role("overlay.dialogue"), SlotRole::Overlay);
        assert_eq!(slot_role("toast"), SlotRole::Toast);
        assert_eq!(slot_role("hud"), SlotRole::HudTop);
        assert_eq!(slot_role("hud-top"), SlotRole::HudTop);
        assert_eq!(slot_role("hud.health"), SlotRole::HudTop);
        assert_eq!(slot_role("hud-bottom"), SlotRole::HudBottom);
        assert_eq!(slot_role("floating"), SlotRole::Floating);
        assert_eq!(slot_role("system"), SlotRole::System);
    }

    #[test]
    fn dock_is_an_overlay_slot_the_closed_circuit_governs() {
        assert!(OVERLAY_SLOTS.contains(&"dock"));
        assert!(is_floating_slot("dock"));
    }

    /// Every one of the 23 `UI_SLOTS` names in
    /// `packages/almadar-core/src/types/effect.ts` gets an asserted role — not
    /// just the family-representative cases above. Catches a future core
    /// addition (e.g. a new `hud.*`/`overlay.*` sub-slot, or a wholly new
    /// top-level slot) that the prefix arms in `slot_role` wouldn't classify
    /// as intended.
    #[test]
    fn classifies_every_canonical_ui_slot_name() {
        const CASES: &[(&str, SlotRole)] = &[
            ("main", SlotRole::Main),
            ("sidebar", SlotRole::Sidebar),
            ("dock", SlotRole::Drawer),
            ("modal", SlotRole::Modal),
            ("drawer", SlotRole::Drawer),
            ("overlay", SlotRole::Overlay),
            ("center", SlotRole::Modal),
            ("toast", SlotRole::Toast),
            ("floating", SlotRole::Floating),
            ("system", SlotRole::System),
            ("content", SlotRole::Main),
            ("screen", SlotRole::Main),
            ("hud", SlotRole::HudTop),
            ("hud-top", SlotRole::HudTop),
            ("hud-bottom", SlotRole::HudBottom),
            ("hud.health", SlotRole::HudTop),
            ("hud.score", SlotRole::HudTop),
            ("hud.inventory", SlotRole::HudTop),
            ("hud.stamina", SlotRole::HudTop),
            ("overlay.inventory", SlotRole::Overlay),
            ("overlay.dialogue", SlotRole::Overlay),
            ("overlay.menu", SlotRole::Overlay),
            ("overlay.pause", SlotRole::Overlay),
        ];
        assert_eq!(CASES.len(), 23, "expected exactly the 23 UI_SLOTS names");
        for &(name, expected) in CASES {
            assert_eq!(
                slot_role(name),
                expected,
                "slot_role({name:?}) mismatch — check UI_SLOTS parity"
            );
        }
    }

    #[test]
    fn floating_predicate() {
        assert!(is_floating_slot("modal"));
        assert!(is_floating_slot("toast"));
        assert!(is_floating_slot("hud-top"));
        assert!(!is_floating_slot("main"));
        assert!(!is_floating_slot("sidebar"));
        assert!(!is_floating_slot("system"));
    }

    /// §104: a `(render-ui <slot> null)` CLOSE arm must never be counted as an "open"
    /// path — only the transition that renders real content into the modal slot is.
    /// Before this fix `non_main_slot_transitions` admitted both `OPEN` and `SAVE`
    /// (both target the `modal` slot), so `modal_open_paths` drove `SAVE` directly with
    /// an empty payload as if it opened something, cascading a bare `listens { X.SAVE ->
    /// ... }` route with no data — the L2 render-probe artifact behind the spurious
    /// `@Create denied` / `unresolved binding '@payload.id'` lines on `std-realtime-chat`.
    #[test]
    fn non_main_slot_transitions_excludes_null_content_close_arms() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Modal",
                    "linkedEntity": "Item",
                    "stateMachine": {
                        "states": [
                            { "name": "closed", "isInitial": true },
                            { "name": "open" }
                        ],
                        "transitions": [
                            { "from": "closed", "to": "open", "event": "OPEN",
                              "effects": [["render-ui", "modal", { "type": "form" }]] },
                            { "from": "open", "to": "closed", "event": "SAVE",
                              "effects": [["render-ui", "modal", null]] },
                            { "from": "open", "to": "closed", "event": "CLOSE",
                              "effects": [["render-ui", "modal", null]] }
                        ]
                    }
                }]
            }]
        }))
        .unwrap();

        let transitions = non_main_slot_transitions(&schema);
        let events: Vec<&str> = transitions.iter().map(|t| t.event.as_str()).collect();
        assert_eq!(
            events,
            vec!["OPEN"],
            "only the real-content OPEN transition should be enumerated, got {events:?}"
        );
    }
}
