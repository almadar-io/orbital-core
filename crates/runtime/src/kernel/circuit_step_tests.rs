//! One dispatch reports the circuit it ran: which `from --event-->` arm fired, every
//! guard tried, and one status per authored effect of the fired arm (what
//! `orb execute --steps` and the cosmic zoom's Step read).
use super::*;
use crate::entity_source::InMemoryEntityCache;

fn kernel() -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "EditorApp",
        "orbitals": [{
            "name": "Editor",
            "entity": { "name": "Doc", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "mode", "type": "string", "default": "normal" }] },
            "traits": [{
                "name": "Modes",
                "linkedEntity": "Doc",
                "stateMachine": {
                    "states": [{ "name": "NORMAL", "isInitial": true }, { "name": "INSERT" }, { "name": "VISUAL" }],
                    "transitions": [
                        { "from": "NORMAL", "to": "INSERT", "event": "KEY", "guard": ["=", "@payload.key", "i"], "effects": [["set", "@entity.mode", "insert"]] },
                        { "from": "NORMAL", "to": "VISUAL", "event": "KEY", "guard": ["=", "@payload.key", "v"],
                          "effects": [["set", "@entity.mode", "visual"], ["if", true, ["set", "@entity.mode", "visual"], ["set", "@entity.mode", "never"]]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    kernel.register_schema(schema).unwrap();
    kernel
}

fn key(k: &str) -> Value {
    let mut data = ValueMap::default();
    data.insert("key".to_string(), Value::String(k.to_string()));
    Value::Object(data)
}

#[test]
fn a_fired_dispatch_reports_its_arm_its_guards_and_one_status_per_authored_effect() {
    let result = kernel()
        .process_event("Modes", None, "KEY", key("v"))
        .unwrap();
    assert!(result.executed);
    assert_eq!(result.fired_arm, Some(1));
    assert_eq!(
        result
            .guards
            .iter()
            .map(|g| (g.arm, g.guard.passed))
            .collect::<Vec<_>>(),
        vec![(0, false), (1, true)]
    );
    assert_eq!(
        result.effect_runs.len(),
        2,
        "one run per authored effect, the compound `if` included"
    );
    assert!(result
        .effect_runs
        .iter()
        .all(|r| r.status == EffectRunStatus::Executed));
}

#[test]
fn control_a_blocked_dispatch_has_no_arm_and_no_effect_runs_but_keeps_its_guards() {
    let result = kernel()
        .process_event("Modes", None, "KEY", key("x"))
        .unwrap();
    assert!(!result.executed);
    assert_eq!(result.fired_arm, None);
    assert_eq!(result.guards.len(), 2);
    assert!(result.effect_runs.is_empty());
}
