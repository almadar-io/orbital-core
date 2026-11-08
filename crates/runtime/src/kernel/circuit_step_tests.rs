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
        "typeContractVersion": 1, "orbitals": [{
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
    data.insert("key".into(), Value::string(k.to_string()));
    Value::object(data)
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

fn failing_os_kernel() -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    let routes = serde_json::json!({ "emit": { "success": "OPENED", "failure": "OPEN_FAILED" } });
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "OsApp",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Files",
            "entity": { "name": "Doc", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Opener",
                "linkedEntity": "Doc",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "TOP", "effects": [["os/read-file", "/a", routes]] },
                        { "from": "idle", "to": "idle", "event": "NESTED", "effects": [["if", true, ["os/read-file", "/a", routes]]] },
                        { "from": "idle", "to": "idle", "event": "BARE_TOP", "effects": [["os/read-file", "/a"]] },
                        { "from": "idle", "to": "idle", "event": "BARE_NESTED", "effects": [["if", true, ["os/read-file", "/a"]]] },
                        { "from": "idle", "to": "idle", "event": "OPEN_FAILED" },
                        { "from": "idle", "to": "idle", "event": "OPENED" }
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    kernel.register_schema(schema).unwrap();
    kernel
}

fn played_first(event: &str) -> crate::program_machine::PlayedEffect {
    let result = failing_os_kernel()
        .process_event("Opener", None, event, Value::Null)
        .unwrap();
    assert_eq!(result.effect_runs[0].status, EffectRunStatus::Failed, "{result:?}");
    crate::program_machine::played_effect(&result.effect_runs[0])
}

#[test]
fn control_a_top_level_failing_os_op_with_a_failure_route_reports_routed() {
    assert!(played_first("TOP").routed);
}

#[test]
fn a_failing_os_op_inside_an_if_reports_its_nested_failure_route_as_routed() {
    assert!(played_first("NESTED").routed);
}

#[test]
fn a_failing_os_op_without_a_failure_route_is_not_routed_top_level_or_nested() {
    assert!(!played_first("BARE_TOP").routed);
    assert!(!played_first("BARE_NESTED").routed);
}

fn nested_kernel() -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "NestedApp",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Nest",
            "entity": { "name": "Doc", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "mode", "type": "string", "default": "normal" }] },
            "traits": [{
                "name": "Runner",
                "linkedEntity": "Doc",
                "emits": [{ "event": "FOO", "payload": [{ "name": "n", "type": "number" }] }],
                "stateMachine": {
                    "states": [{ "name": "ready", "isInitial": true }],
                    "transitions": [
                        { "from": "ready", "to": "ready", "event": "RAN",
                          "effects": [["let", [["x", 1]], ["do", ["set", "@entity.mode", "ran"], ["emit", "FOO", { "n": "@x" }]]]] },
                        { "from": "ready", "to": "ready", "event": "IDLE",
                          "effects": [["let", [["x", 1]], ["when", false, ["set", "@entity.mode", "never"]]]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    kernel.register_schema(schema).unwrap();
    kernel
}

fn played_nested(event: &str) -> crate::program_machine::PlayedEffect {
    let result = nested_kernel().process_event("Runner", None, event, Value::Null).unwrap();
    assert_eq!(result.effect_runs.len(), 1, "{result:?}");
    crate::program_machine::played_effect(&result.effect_runs[0])
}

#[test]
fn a_let_that_ran_an_emit_carries_the_nested_emit_and_is_not_skipped() {
    use crate::program_machine::PlayedEffectStatus;
    let played = played_nested("RAN");
    assert_eq!(played.effect_type, "let");
    assert_eq!(played.status, PlayedEffectStatus::Executed, "{played:?}");
    let body = played.nested.as_ref().expect("a compound carries its nested run");
    assert_eq!(body.len(), 1, "the `do` is the let's one body effect");
    assert_eq!(body[0].effect_type, "do");
    assert_eq!(body[0].status, PlayedEffectStatus::Executed);
    let inner = body[0].nested.as_ref().expect("the do carries its body");
    assert_eq!(inner.iter().map(|e| e.effect_type.as_str()).collect::<Vec<_>>(), ["set", "emit"]);
    assert_eq!(inner[1].args[0].as_str(), Some("FOO"));
    assert_eq!(inner[0].status, PlayedEffectStatus::Executed, "{inner:?}");
    assert!(inner.iter().all(|e| e.nested.is_none()), "{inner:?}");
}

#[test]
fn control_a_let_whose_body_ran_nothing_is_skipped_with_an_empty_nested_trace() {
    use crate::program_machine::PlayedEffectStatus;
    let played = played_nested("IDLE");
    assert_eq!(played.status, PlayedEffectStatus::Skipped, "{played:?}");
    let body = played.nested.as_ref().expect("a compound carries its nested run");
    assert_eq!(body.len(), 1);
    assert_eq!(body[0].effect_type, "when");
    assert_eq!(body[0].status, PlayedEffectStatus::Skipped);
    assert_eq!(body[0].nested.as_deref().map(<[_]>::len), Some(0), "the branch not taken ran no body effect");
    let wire = serde_json::to_value(&played).unwrap();
    assert!(wire["nested"][0]["nested"].is_array());
}
