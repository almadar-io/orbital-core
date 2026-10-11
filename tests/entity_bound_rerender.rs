// A client kernel repaints every mounted trait whose last `render-ui` reads an entity that a dispatch or tick just changed. Twin of the web's `RenderBindingMarker` path (`deferEntityBindings` in `almadar-runtime`, `resolveRenderBindingMarkers` in `almadar-ui`).

use orbital_core::effects::ClientEffect;
use orbital_core::runtime::{EntitySource, InMemoryEntityCache, KernelConfig, RuntimeKernel};
use orbital_core::{EventResponse, OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn render(slot: &str, content: Json) -> Json {
    json!(["render-ui", slot, content])
}

fn text(content: &str) -> Json {
    json!({ "type": "typography", "content": content })
}

fn trait_of(name: &str, entity: &str, arms: Json, ticks: Json) -> Json {
    json!({
        "name": name, "linkedEntity": entity, "local": true, "ticks": ticks,
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": arms }
    })
}

fn arm(event: &str, effects: Json) -> Json {
    json!({ "from": "idle", "to": "idle", "event": event, "effects": effects })
}

fn schema(persistence: &str, shared: bool) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "rerender",
        "orbitals": [
            {
                "name": "Main",
                "entity": { "name": "Board", "shared": shared, "persistence": persistence,
                    "fields": [{ "name": "id", "type": "string" }, { "name": "score", "type": "number", "default": 0 }, { "name": "painted", "type": "number", "default": 0 }] },
                "traits": [
                    trait_of("Driver", "Board", json!([
                        arm("INIT", json!([render("driver", text("driver"))])),
                        arm("BUMP", json!([["set", "@entity.score", ["+", "@entity.score", 1]]])),
                        arm("BUMP_TWICE", json!([
                            ["set", "@entity.score", ["+", "@entity.score", 1]],
                            ["set", "@entity.score", ["+", "@entity.score", 10]]
                        ])),
                        arm("BUMP_AND_HAND_OFF", json!([
                            ["set", "@entity.score", ["+", "@entity.score", 1]],
                            ["emit", "HAND_OFF", {}]
                        ]))
                    ]), json!([
                        { "name": "step", "interval": 100, "effects": [["set", "@entity.score", ["+", "@entity.score", 1]]] }
                    ])),
                    {
                        "name": "Second", "linkedEntity": "Board", "local": true,
                        "listens": [{ "event": "HAND_OFF", "triggers": "ADD", "source": { "kind": "trait", "trait": "Driver" } }],
                        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                            arm("ADD", json!([["set", "@entity.score", ["+", "@entity.score", 100]]]))
                        ] }
                    },
                    trait_of("Hud", "Board", json!([
                        arm("INIT", json!([render("hud", text("@entity.score")), ["emit", "HUD_PAINTED", {}]])),
                        arm("TAGGED", json!([render("hud", text("score @entity.score tag @payload.tag"))])),
                        arm("CLEAR", json!([["render-ui", "hud", Json::Null]]))
                    ]), json!([])),
                    trait_of("Banner", "Board", json!([
                        arm("INIT", json!([render("banner", text("static"))]))
                    ]), json!([])),
                    {
                        "name": "Painted", "linkedEntity": "Board", "local": true,
                        "listens": [{ "event": "HUD_PAINTED", "triggers": "COUNT", "source": { "kind": "trait", "trait": "Hud" } }],
                        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                            arm("COUNT", json!([["set", "@entity.painted", ["+", "@entity.painted", 1]]]))
                        ] }
                    }
                ]
            },
            {
                "name": "Side",
                "entity": { "name": "Gauge", "persistence": "runtime",
                    "fields": [{ "name": "id", "type": "string" }, { "name": "level", "type": "number", "default": 0 }] },
                "traits": [
                    trait_of("Gauge", "Gauge", json!([
                        arm("INIT", json!([render("gauge", text("level @entity.level"))]))
                    ]), json!([]))
                ]
            }
        ]
    }))
    .unwrap()
}

fn client_kernel(persistence: &str, shared: bool) -> (RuntimeKernel, Arc<InMemoryEntityCache>) {
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        cache.clone() as Arc<dyn EntitySource>,
        KernelConfig::for_environment(RuntimeEnvironment::Client),
    );
    k.register_schema(schema(persistence, shared)).unwrap();
    (k, cache)
}

fn mounted(persistence: &str, shared: bool) -> (RuntimeKernel, Arc<InMemoryEntityCache>) {
    let (k, cache) = client_kernel(persistence, shared);
    for t in ["Driver", "Hud", "Banner"] {
        k.dispatch_with_server_leg("Main", t, None, "INIT", Value::Null, None)
            .unwrap();
    }
    k.dispatch_with_server_leg("Side", "Gauge", None, "INIT", Value::Null, None)
        .unwrap();
    (k, cache)
}

/// `(trait, slot, merged pattern)` of every render the response carries.
fn paints(r: &EventResponse) -> Vec<(String, String, Json)> {
    r.client_effects_by_trait
        .iter()
        .flatten()
        .filter_map(|e| match &e.effect {
            ClientEffect::RenderUi { slot, .. } => Some((
                e.trait_name.clone(),
                slot.clone(),
                serde_json::to_value(e.effect.render_ui_value()?).unwrap(),
            )),
            _ => None,
        })
        .collect()
}

fn painted_by(r: &EventResponse, trait_name: &str) -> Vec<Json> {
    paints(r)
        .into_iter()
        .filter(|(t, _, _)| t == trait_name)
        .map(|(_, _, c)| c)
        .collect()
}

fn bump(k: &RuntimeKernel, event: &str) -> EventResponse {
    k.dispatch_with_server_leg("Main", "Driver", None, event, Value::Null, None)
        .unwrap()
        .response
}

#[test]
fn a_sibling_bound_to_the_shared_entity_repaints_when_another_trait_writes_it() {
    let (k, _) = mounted("runtime", true);
    let r = bump(&k, "BUMP");
    let hud = painted_by(&r, "Hud");
    assert_eq!(
        hud.len(),
        1,
        "the bound sibling repaints once: {:?}",
        paints(&r)
    );
    assert_eq!(hud[0]["content"].as_f64(), Some(1.0));
}

#[test]
fn control_a_sibling_that_reads_no_entity_leaf_does_not_repaint() {
    let (k, _) = mounted("runtime", true);
    let r = bump(&k, "BUMP");
    assert!(painted_by(&r, "Banner").is_empty(), "{:?}", paints(&r));
    assert!(
        painted_by(&r, "Driver").is_empty(),
        "the writer's static render is unchanged: {:?}",
        paints(&r)
    );
}

#[test]
fn control_a_trait_bound_to_another_entity_does_not_repaint() {
    let (k, _) = mounted("runtime", true);
    let r = bump(&k, "BUMP");
    assert!(painted_by(&r, "Gauge").is_empty(), "{:?}", paints(&r));
}

#[test]
fn control_a_write_that_changes_nothing_repaints_nothing() {
    let (k, _) = mounted("runtime", true);
    let r = k
        .dispatch_with_server_leg("Main", "Driver", None, "INIT", Value::Null, None)
        .unwrap()
        .response;
    assert!(painted_by(&r, "Hud").is_empty(), "{:?}", paints(&r));
}

#[test]
fn a_tick_that_writes_the_shared_entity_repaints_the_bound_renderer() {
    let (k, _) = mounted("runtime", true);
    let tick = k
        .dispatch_tick("Main", "Driver", None, "step", None)
        .unwrap();
    assert!(tick.outcome.executed);
    let mut folded = EventResponse::default();
    folded.fold_rerenders(&tick.rerenders);
    let hud = painted_by(&folded, "Hud");
    assert_eq!(hud.len(), 1, "{:?}", paints(&folded));
    assert_eq!(hud[0]["content"].as_f64(), Some(1.0));
    assert!(painted_by(&folded, "Banner").is_empty());
}

#[test]
fn a_second_tick_repaints_with_the_next_value_not_the_first() {
    let (k, _) = mounted("runtime", true);
    k.dispatch_tick("Main", "Driver", None, "step", None)
        .unwrap();
    let tick = k
        .dispatch_tick("Main", "Driver", None, "step", None)
        .unwrap();
    let mut folded = EventResponse::default();
    folded.fold_rerenders(&tick.rerenders);
    assert_eq!(painted_by(&folded, "Hud")[0]["content"].as_f64(), Some(2.0));
}

#[test]
fn the_repaint_re_runs_no_transition_effect_or_emit() {
    let (k, _) = mounted("runtime", true);
    let dispatch = k
        .dispatch_with_server_leg("Main", "Driver", None, "BUMP", Value::Null, None)
        .unwrap();
    let ran: Vec<&str> = dispatch.outcomes.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        ran,
        vec!["Driver"],
        "only the dispatched trait ran a transition"
    );
    assert!(
        dispatch.response.emitted_events.is_empty(),
        "Hud's INIT emit was not replayed"
    );
    let board = k.snapshot_trait("Driver", None).entity;
    let board = board.as_object().unwrap();
    assert_eq!(board.get("score"), Some(&Value::Number(1.0)));
    assert_eq!(
        board.get("painted"),
        Some(&Value::Number(1.0)),
        "Hud's INIT ran once, at mount, and not again for the repaint"
    );
}

#[test]
fn several_writes_in_one_dispatch_repaint_the_trait_once_with_the_final_value() {
    let (k, _) = mounted("runtime", true);
    let r = bump(&k, "BUMP_TWICE");
    let hud = painted_by(&r, "Hud");
    assert_eq!(hud.len(), 1, "{:?}", paints(&r));
    assert_eq!(hud[0]["content"].as_f64(), Some(11.0));
}

#[test]
fn writes_by_two_traits_in_one_cascade_repaint_once_with_the_final_value() {
    let (k, _) = mounted("runtime", true);
    let r = bump(&k, "BUMP_AND_HAND_OFF");
    let hud = painted_by(&r, "Hud");
    assert_eq!(hud.len(), 1, "{:?}", paints(&r));
    assert_eq!(hud[0]["content"].as_f64(), Some(101.0));
}

#[test]
fn a_second_pass_with_nothing_new_repaints_nothing() {
    let (k, _) = mounted("runtime", true);
    bump(&k, "BUMP");
    assert!(k.rerender_stale().is_empty());
}

#[test]
fn the_repaint_equals_what_a_fresh_render_of_the_same_state_produces() {
    let (k, _) = mounted("runtime", true);
    let repainted = painted_by(&bump(&k, "BUMP"), "Hud").remove(0);
    let fresh = k
        .dispatch_with_server_leg("Main", "Hud", None, "INIT", Value::Null, None)
        .unwrap()
        .response;
    assert_eq!(painted_by(&fresh, "Hud")[0], repainted);
}

#[test]
fn a_leaf_that_also_reads_the_payload_stays_frozen_like_the_webs_eager_leaf() {
    let (k, _) = mounted("runtime", true);
    let tagged = json!({ "tag": "t1" });
    k.dispatch_with_server_leg(
        "Main",
        "Hud",
        None,
        "TAGGED",
        serde_json::from_value(tagged).unwrap(),
        None,
    )
    .unwrap();
    let r = bump(&k, "BUMP");
    assert!(
        painted_by(&r, "Hud").is_empty(),
        "no entity-only leaf to re-evaluate: {:?}",
        paints(&r)
    );
}

#[test]
fn a_cleared_slot_is_not_repainted() {
    let (k, _) = mounted("runtime", true);
    k.dispatch_with_server_leg("Main", "Hud", None, "CLEAR", Value::Null, None)
        .unwrap();
    let r = bump(&k, "BUMP");
    assert!(painted_by(&r, "Hud").is_empty(), "{:?}", paints(&r));
}

#[test]
fn an_unmounted_trait_is_not_repainted() {
    let (k, _) = mounted("runtime", true);
    k.unmount_traits(["Hud"]);
    let r = bump(&k, "BUMP");
    assert!(painted_by(&r, "Hud").is_empty(), "{:?}", paints(&r));
}

#[test]
fn control_a_remount_forgets_every_prior_render() {
    let (k, _) = mounted("runtime", true);
    k.remount_traits(["Driver"]);
    let r = bump(&k, "BUMP");
    assert!(painted_by(&r, "Hud").is_empty(), "{:?}", paints(&r));
}

#[test]
fn rows_written_into_the_store_repaint_a_client_resident_entity() {
    let (k, cache) = mounted("runtime", false);
    let row: Value = serde_json::from_value(json!({ "id": "singleton", "level": 7 })).unwrap();
    cache.write("Gauge", "singleton", &row).unwrap();
    let rerenders = k.rerender_stale();
    let gauge: Vec<_> = rerenders
        .iter()
        .filter(|r| r.trait_name == "Gauge")
        .collect();
    assert_eq!(gauge.len(), 1);
    let mut folded = EventResponse::default();
    folded.fold_rerenders(&rerenders);
    assert_eq!(painted_by(&folded, "Gauge")[0]["content"], json!("level 7"));
}

#[test]
fn control_a_persistent_entity_is_server_resolved_and_never_repainted_by_the_client() {
    let (k, cache) = mounted("persistent", false);
    let row: Value = serde_json::from_value(json!({ "id": "singleton", "score": 5 })).unwrap();
    cache.write("Board", "singleton", &row).unwrap();
    assert!(k.rerender_stale().iter().all(|r| r.trait_name != "Hud"));
}

#[test]
fn control_a_server_role_kernel_never_repaints() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema("runtime", true)).unwrap();
    k.dispatch("Hud", None, "INIT", Value::Null).unwrap();
    k.dispatch("Driver", None, "BUMP", Value::Null).unwrap();
    assert!(k.rerender_stale().is_empty());
}

fn repaints(rerenders: &[orbital_core::runtime::Rerender]) -> EventResponse {
    let mut folded = EventResponse::default();
    folded.fold_rerenders(rerenders);
    folded
}

#[test]
fn a_rollback_repaints_the_bound_sibling_with_the_restored_value() {
    let (k, _) = mounted("runtime", true);
    let snapshot = k.snapshot_trait("Driver", None);
    let r = bump(&k, "BUMP");
    assert_eq!(painted_by(&r, "Hud")[0]["content"].as_f64(), Some(1.0));

    let restored = {
        k.restore_trait("Driver", None, snapshot);
        repaints(&k.rerender_stale())
    };
    let hud = painted_by(&restored, "Hud");
    assert_eq!(hud.len(), 1, "{:?}", paints(&restored));
    assert_eq!(hud[0]["content"].as_f64(), Some(0.0), "the restored value, not the optimistic one");
    assert!(painted_by(&restored, "Banner").is_empty(), "control: a static sibling stays put");
    assert!(
        k.rerender_stale().is_empty(),
        "control: the repaint is delivered once"
    );
}

#[test]
fn control_a_rollback_that_changes_nothing_repaints_nothing() {
    let (k, _) = mounted("runtime", true);
    let snapshot = k.snapshot_trait("Driver", None);
    k.restore_trait("Driver", None, snapshot);
    assert!(k.rerender_stale().is_empty());
}

#[test]
fn control_a_committed_dispatch_repaints_once_and_leaves_nothing_pending() {
    let (k, _) = mounted("runtime", true);
    let r = bump(&k, "BUMP");
    assert_eq!(painted_by(&r, "Hud").len(), 1);
    assert!(k.rerender_stale().is_empty());
}
