// The tick clock runs the ticks of the MOUNTED traits only (twin of `useClientTicks` over the page's `traitBindings`); a kernel with no mount declared covers the whole schema.

use orbital_core_runtime::embed::EmbeddedKernel;
use orbital_core_base::schema::types::OrbitalSchema;
use serde_json::json;

fn kernel() -> EmbeddedKernel {
    let tick = |trait_name: &str| {
        json!({
            "name": trait_name, "linkedEntity": "E", "dispatch": "client",
            "stateMachine": { "states": [{ "name": "on", "isInitial": true }], "transitions": [] },
            "ticks": [{ "name": "beat", "interval": 0, "effects": [["set", "@entity.n", 1]] }]
        })
    };
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number" }] },
            "traits": [tick("A"), tick("B")]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "c").unwrap()
}

fn fired(k: &EmbeddedKernel, now_ms: u64) -> Vec<String> {
    k.frame(now_ms)
        .unwrap()
        .map(|f| f.fired.into_values().flatten().map(|t| t.trait_name).collect())
        .unwrap_or_default()
}

#[test]
fn a_remount_scopes_the_clock_to_the_mounted_traits() {
    let k = kernel();
    k.remount(vec!["A".to_string()]);
    assert_eq!(fired(&k, 0), ["A"]);
}

#[test]
fn mount_adds_to_the_mounted_traits_and_unmount_removes() {
    let k = kernel();
    k.remount(vec!["A".to_string()]);
    k.mount(vec!["B".to_string()]);
    assert_eq!(fired(&k, 0), ["A", "B"]);
    k.unmount(vec!["A".to_string()]);
    assert_eq!(fired(&k, 16), ["B"]);
}

#[test]
fn control_a_kernel_with_no_mount_declared_ticks_every_trait() {
    assert_eq!(fired(&kernel(), 0), ["A", "B"]);
}

#[test]
fn edge_an_empty_mount_ticks_nothing() {
    let k = kernel();
    k.remount(Vec::new());
    assert_eq!(fired(&k, 0), Vec::<String>::new());
}

fn paged() -> EmbeddedKernel {
    let with = |name: &str, event: &str| {
        json!({
            "name": name, "linkedEntity": "E", "dispatch": "client",
            "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                "transitions": [{ "from": "on", "to": "on", "event": event, "effects": [] }] }
        })
    };
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [with("A", "INIT"), with("B", "INIT"), with("C", "GO")],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "A" }, { "ref": "B" }, { "ref": "C" }] }]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "c").unwrap()
}

fn seeded(entered: &orbital_core_runtime::embed::wire::Entered) -> Vec<&str> {
    entered.seeds.iter().map(|s| s.r#trait.as_str()).collect()
}

fn released(left: &[orbital_core_runtime::embed::wire::Left]) -> Vec<&str> {
    left.iter().map(|l| l.trait_name.as_str()).collect()
}

#[test]
fn entering_answers_the_seeds_of_the_traits_not_entered_yet() {
    let k = paged();
    let first = k.enter(Some("/"), "page", vec!["A".into(), "B".into()]).unwrap();
    assert_eq!(seeded(&first), ["A", "B"]);
    assert!(first.left.is_empty());
    let again = k.enter(Some("/"), "page", vec!["A".into(), "B".into()]).unwrap();
    assert!(again.seeds.is_empty(), "a trait entered once is not seeded again");
}

#[test]
fn entering_a_larger_set_seeds_only_the_new_traits_and_a_trait_with_no_lifecycle_arm_has_no_seed() {
    let k = paged();
    k.enter(Some("/"), "page", vec!["A".into()]).unwrap();
    let more = k.enter(Some("/"), "page", vec!["A".into(), "B".into(), "C".into()]).unwrap();
    assert_eq!(seeded(&more), ["B"]);
}

#[test]
fn entering_a_smaller_set_answers_the_unmount_legs_of_the_traits_that_left() {
    let k = paged();
    k.enter(Some("/"), "page", vec!["A".into(), "B".into()]).unwrap();
    let next = k.enter(Some("/"), "page", vec!["B".into()]).unwrap();
    assert_eq!(released(&next.left), ["A"]);
    assert_eq!(next.left[0].orbital, "Main");
    assert_eq!(next.left[0].leg.event, "$UNMOUNT");
    assert_eq!(next.left[0].leg.target_trait.as_deref(), Some("A"));
    assert_eq!(next.left[0].leg.client_id.as_deref(), Some("c"));
}

#[test]
fn a_remount_releases_the_entered_traits_it_does_not_hold_and_enters_the_rest_again() {
    let k = paged();
    k.enter(Some("/"), "page", vec!["A".into(), "B".into()]).unwrap();
    let left = k.remount(vec!["B".into()]);
    assert_eq!(released(&left), ["A"]);
    let again = k.enter(Some("/"), "page", vec!["B".into()]).unwrap();
    assert_eq!(seeded(&again), ["B"], "a new mount seeds its traits afresh");
}

#[test]
fn unmount_and_leave_release_only_what_was_entered() {
    let k = paged();
    k.enter(Some("/"), "page", vec!["A".into(), "B".into()]).unwrap();
    assert!(k.unmount(vec!["C".into()]).is_empty(), "control: a trait never entered has no leg");
    assert_eq!(released(&k.unmount(vec!["A".into()])), ["A"]);
    assert_eq!(released(&k.leave()), ["B"]);
    assert!(k.leave().is_empty());
}

#[test]
fn entering_an_unknown_trait_is_refused() {
    assert!(paged().enter(Some("/"), "page", vec!["Nope".into()]).is_err());
}

#[test]
fn another_mount_key_is_a_new_mount_that_enters_every_trait_afresh_and_releases_the_old_pages_traits() {
    let k = paged();
    k.enter(Some("/"), "home", vec!["A".into(), "B".into()]).unwrap();
    let next = k.enter(Some("/"), "other", vec!["B".into()]).unwrap();
    assert_eq!(seeded(&next), ["B"], "B enters again under the new key");
    assert_eq!(released(&next.left), ["A"]);
    let same = k.enter(Some("/"), "other", vec!["B".into()]).unwrap();
    assert!(same.seeds.is_empty() && same.left.is_empty(), "control: the same key is the same mount");
}

#[test]
fn a_page_with_no_route_enters_its_traits_and_seeds_none() {
    let k = paged();
    let entered = k.enter(None, "page", vec!["A".into()]).unwrap();
    assert!(entered.seeds.is_empty());
    assert!(k.enter(None, "page", vec!["A".into()]).unwrap().left.is_empty());
}

#[test]
fn a_same_key_enter_right_after_leave_resumes_the_mount_without_seeding_it_again() {
    let k = paged();
    let first = k.enter(Some("/"), "page", vec!["A".into(), "B".into()]).unwrap();
    assert_eq!(seeded(&first), ["A", "B"]);
    k.leave();
    let resumed = k.enter(Some("/"), "page", vec!["A".into(), "B".into()]).unwrap();
    assert!(resumed.seeds.is_empty(), "one mount runs each lifecycle event once, whatever strict mode does to its effects");
    assert!(resumed.left.is_empty());
    assert_eq!(released(&k.leave()), ["A", "B"], "the resumed traits are entered again, so they leave again");
}

#[test]
fn a_resumed_mount_seeds_only_a_trait_it_had_not_entered() {
    let k = paged();
    k.enter(Some("/"), "page", vec!["A".into()]).unwrap();
    k.leave();
    let resumed = k.enter(Some("/"), "page", vec!["A".into(), "B".into()]).unwrap();
    assert_eq!(seeded(&resumed), ["B"]);
}

#[test]
fn control_another_key_after_leave_is_a_new_mount_and_so_is_the_old_key_after_another_page() {
    let k = paged();
    k.enter(Some("/"), "home", vec!["A".into(), "B".into()]).unwrap();
    k.leave();
    let other = k.enter(Some("/"), "away", vec!["A".into(), "B".into()]).unwrap();
    assert_eq!(seeded(&other), ["A", "B"]);
    k.leave();
    let back = k.enter(Some("/"), "home", vec!["A".into(), "B".into()]).unwrap();
    assert_eq!(seeded(&back), ["A", "B"], "revisiting a page seeds it again");
}
