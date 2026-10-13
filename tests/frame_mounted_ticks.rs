// The tick clock runs the ticks of the MOUNTED traits only (twin of `useClientTicks` over the page's `traitBindings`); a kernel with no mount declared covers the whole schema.

use orbital_core::embed::EmbeddedKernel;
use orbital_core::OrbitalSchema;
use serde_json::json;

fn kernel() -> EmbeddedKernel {
    let tick = |trait_name: &str| {
        json!({
            "name": trait_name, "linkedEntity": "E",
            "stateMachine": { "states": [{ "name": "on", "isInitial": true }], "transitions": [] },
            "ticks": [{ "name": "beat", "interval": 0, "effects": [["set", "@entity.n", 1]] }]
        })
    };
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
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
        .map(|f| f.fired.into_iter().map(|t| t.trait_name).collect())
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
