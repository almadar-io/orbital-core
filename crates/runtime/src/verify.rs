//! Layer 1 — the kernel circuit walk: plan every trait's transitions, drive the
//! kernel headlessly, observe. Pure over a resolved program, so the compiled
//! `orb verify` and an in-browser client run the same walk.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

pub mod driver;
pub mod flows;
pub mod frame;
pub mod observers;
pub mod payload_solve;
pub mod plan_wire;
pub mod precondition;
pub mod report;

pub use report::Report;

use orbital_core_base::value::ValueMap;
use crate::planner;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;

/// Verify a resolved schema: plan every trait's transitions, drive the kernel, observe.
///
/// An app that declares an `[identity]` roster has no anonymous viewer. Left
/// unbound, every `@read` directive denies every row, so the walk verifies an
/// empty app and Layer 2 fails `entity_data_bound` on a list that legitimately
/// has data — measured on `std-helpdesk`: 0/1 unbound vs 1/1 (5/5 render) with a
/// persona bound. So with `ALMADAR_PERSONA` unset the default single run binds
/// the FIRST declared persona: deterministic (roster order), never a heuristic
/// ranking of roles. An explicit `ALMADAR_PERSONA` still wins — `Harness::new`
/// resolved it already, and this only fills a viewer nobody set.
pub fn verify_schema(item: &str, schema: OrbitalSchema) -> Result<Report, String> {
    verify_schema_scoped(item, schema, None)
}

/// Same walk as [`verify_schema`], restricted to `traits` when given (`--trait`): only
/// the named trait(s)' `(from, event)` pairs are driven, and coverage/verdicts count
/// only those traits. `traits` accepts either spelling a trait can carry post-resolve
/// (see [`planner::trait_matches_name`]); a name matching nothing is an error listing
/// every available trait name.
pub fn verify_schema_scoped(
    item: &str,
    schema: OrbitalSchema,
    traits: Option<&[String]>,
) -> Result<Report, String> {
    let harness = driver::Harness::new(schema.clone())?;
    if matches!(
        crate::persona::persona_from_env_spec(),
        Ok(crate::persona::EnvPersona::None)
    ) {
        let roster = crate::persona::identity_roster(&schema);
        if let Some(first) = roster.first() {
            harness.set_persona(first.clone());
        }
    }
    verify_with_harness(item, &schema, &harness, traits)
}

/// Verify a resolved schema as a specific persona — same walk as [`verify_schema`],
/// with `@user` bound to `persona` throughout (task #129, persona-parameterised
/// verify run). One fresh `Harness` per call; the persona is bound once before
/// any step is driven, so every fetch/mutation access-policy check sees it.
pub fn verify_schema_as(item: &str, schema: OrbitalSchema, persona: Value) -> Result<Report, String> {
    verify_schema_as_scoped(item, schema, persona, None)
}

/// [`verify_schema_as`], restricted to `traits` — see [`verify_schema_scoped`].
pub fn verify_schema_as_scoped(
    item: &str,
    schema: OrbitalSchema,
    persona: Value,
    traits: Option<&[String]>,
) -> Result<Report, String> {
    let harness = driver::Harness::new(schema.clone())?;
    harness.set_persona(persona);
    verify_with_harness(item, &schema, &harness, traits)
}

fn verify_with_harness(
    item: &str,
    schema: &OrbitalSchema,
    harness: &driver::Harness,
    traits: Option<&[String]>,
) -> Result<Report, String> {
    let resolved_traits = traits
        .map(|names| planner::resolve_trait_names(schema, names))
        .transpose()
        .map_err(|e| e.to_string())?;
    let selected = planner::inline_traits_scoped(schema, resolved_traits.as_deref());
    let mut frames: Vec<frame::Frame> = Vec::new();
    // Failure-route steps are never driven: they are credited from the transitions a request step's
    // own dispatch fired, resolved once every step has run.
    let mut credit_steps: Vec<plan_wire::CreditStep> = Vec::new();
    let credit_step = |step: &planner::Step, fired: Vec<frame::FiredHop>| plan_wire::CreditStep {
        trait_name: step.trait_name.clone(),
        from: step.from.clone(),
        event: step.event.clone(),
        arm: step.arm,
        reply: step.reply,
        fired,
    };
    let ctx = harness.entity_ctx(schema);
    // The roster's default persona (or none), read once so a per-step role
    // rewrite always restores exactly what the caller bound — never a
    // previous step's rewritten role.
    let default_persona = harness.current_persona();
    for td in selected.iter().copied() {
        let seed_id = td
            .linked_entity
            .as_deref()
            .map(|e| harness.effective_seed_id(e));
        for step in planner::plan_trait(td, &ctx, seed_id.as_deref()) {
            if step.reply.is_some() {
                credit_steps.push(credit_step(&step, Vec::new()));
                frames.push(harness.not_driven(
                    &step,
                    frames.len(),
                    step.payload.clone(),
                    frame::FrameStatus::Gap,
                    Some(frame::GapReason::NotReachableWithoutNetwork),
                    "",
                ));
                continue;
            }
            // A role-only persist policy (`@delete (= @user.role "approver")`)
            // denies the default persona unless it happens to carry that role.
            // Deriving the satisfying role per step and rebinding to it before
            // driving is what lets the single default walk exercise the step
            // for real, instead of bucketing it as an expected access denial
            // that only `--all-personas` would find a passing viewer for.
            let role = planner::persist_targets(td, &step.from, &step.event)
                .into_iter()
                .find_map(|(entity, kind)| {
                    crate::entity_access::role_satisfying_policy(schema, &entity, kind)
                });
            if let Some((field, value)) = &role {
                harness.set_persona(persona_with_field(&default_persona, field, value));
            }
            let frame = harness.drive(td, &step, frames.len(), &ctx);
            if role.is_some() {
                harness.set_persona(default_persona.clone());
            }
            credit_steps.push(credit_step(&step, frame.fired.clone()));
            frames.push(frame);
        }
    }
    for credit in plan_wire::credit_failure_routes(&credit_steps) {
        let frame = &mut frames[credit.index];
        frame.state_before = credit.state_before;
        frame.state_after = credit.state_after;
        frame.executed = true;
        frame.status = frame::FrameStatus::Pass;
        frame.gap = None;
    }
    let mut verdicts = observers::run_all(&frames);
    verdicts.push(observers::ref_trait_invariant(&selected));
    let mut report = Report::new(item.to_string(), frames, verdicts);
    report.traits = resolved_traits;
    Ok(report)
}

/// `base` with `field` overridden to `value` — `base` cloned if it is already
/// an object, or a fresh one when no persona was bound at all (an app with no
/// `[identity]` roster still declares a role-scoped policy, so the walker
/// needs a synthesized viewer to carry it).
fn persona_with_field(base: &Value, field: &str, value: &str) -> Value {
    let mut obj = match base {
        Value::Object(m) => m.clone(),
        _ => ValueMap::default(),
    };
    obj.insert(field.to_string(), Value::String(value.to_string()));
    Value::Object(obj)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::frame::FrameStatus;
    use serde_json::json;

    fn http_schema() -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "egress": ["https://api.example.com"],
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Caller", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true }, { "name": "waiting" },
                            { "name": "done" }, { "name": "failed" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "waiting", "event": "CALL", "effects": [
                                ["http", "GET", "https://api.example.com", "/items", {},
                                 { "emit": { "success": "CALLED", "failure": "CALL_FAILED" } }]
                            ] },
                            { "from": "waiting", "to": "done", "event": "CALLED",
                              "guard": ["<", "@payload.status", 300] },
                            { "from": "waiting", "to": "failed", "event": "CALLED",
                              "guard": [">=", "@payload.status", 300] },
                            { "from": "waiting", "to": "failed", "event": "CALL_FAILED" }
                        ]
                    } }]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn an_http_effect_is_counted_fired_with_its_request_and_its_reply_arms_are_gaps() {
        let report = verify_schema("http", http_schema()).unwrap();
        let call = report
            .frames
            .iter()
            .find(|f| f.cause.event == "CALL")
            .expect("the CALL step is walked");
        let http = call.effects.iter().find(|e| e.kind == "http").expect("the http effect is summarised");
        assert_eq!(http.target, "GET https://api.example.com/items");
        assert_eq!(http.outcome, "failed", "no response is fabricated: verify has no network");
        assert_eq!(http.routed.as_deref(), Some("CALL_FAILED"));
        assert_eq!(call.state_after, "waiting");
        assert_eq!(call.status, FrameStatus::Pass);
        assert!(
            call.effects.iter().all(|e| !(e.kind == "http" && e.outcome == "executed")),
            "an executed http effect would be a fabricated reply"
        );
        let verdict = |name: &str| report.verdicts.iter().find(|v| v.name == name).unwrap();
        assert!(verdict("effects_fired").passed, "{}", verdict("effects_fired").detail);
        assert!(verdict("state_transition").passed, "{}", verdict("state_transition").detail);

        let status_of = |event: &str| -> Vec<(FrameStatus, Option<frame::GapReason>, String)> {
            report
                .frames
                .iter()
                .filter(|f| f.cause.event == event)
                .map(|f| (f.status, f.gap, f.state_after.clone()))
                .collect()
        };
        let gap = Some(frame::GapReason::NotReachableWithoutNetwork);
        assert_eq!(
            status_of("CALLED"),
            vec![(FrameStatus::Gap, gap, String::new()), (FrameStatus::Gap, gap, String::new())],
            "both reply arms (2xx and non-2xx) are gaps, never walked from a synthetic state"
        );
        assert_eq!(
            status_of("CALL_FAILED"),
            vec![(FrameStatus::Pass, None, "failed".to_string())],
            "the failure arm the request's own dispatch fired is credited"
        );
        assert_eq!(call.fired.len(), 1);
        assert_eq!(call.fired[0].event, "CALL_FAILED");
        assert_eq!(report.summary.gap_frames, 2);
        assert!(report.ok());
    }

    #[test]
    fn a_failure_arm_the_dispatch_did_not_fire_is_a_gap() {
        let text = serde_json::to_string(&http_schema()).unwrap().replace(
            r#""event":"CALL_FAILED"}"#,
            r#""event":"CALL_FAILED","guard":["=","@payload.kind","never"]}"#,
        );
        let report = verify_schema("http", serde_json::from_str(&text).unwrap()).unwrap();
        let failed: Vec<_> = report.frames.iter().filter(|f| f.cause.event == "CALL_FAILED").collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].status, FrameStatus::Gap);
        assert_eq!(failed[0].gap, Some(frame::GapReason::NotReachableWithoutNetwork));
    }

    #[test]
    fn an_http_effect_with_no_failure_route_stays_a_failure() {
        let mut schema = http_schema();
        let text = serde_json::to_string(&schema).unwrap().replace(
            r#","failure":"CALL_FAILED""#,
            "",
        );
        schema = serde_json::from_str(&text).unwrap();
        let report = verify_schema("http", schema).unwrap();
        let effects_fired = report.verdicts.iter().find(|v| v.name == "effects_fired").unwrap();
        assert!(!effects_fired.passed);
    }

    fn reply_arms(arms: serde_json::Value) -> OrbitalSchema {
        let mut transitions = vec![json!({ "from": "idle", "to": "idle", "event": "INIT" })];
        transitions.extend(arms.as_array().unwrap().clone());
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Caller", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true }, { "name": "waiting" },
                            { "name": "done" }, { "name": "failed" }, { "name": "other" }
                        ],
                        "transitions": transitions
                    } }]
            }]
        }))
        .unwrap()
    }

    fn arm_statuses(arms: serde_json::Value) -> Vec<(String, FrameStatus)> {
        let report = verify_schema("arms", reply_arms(arms)).unwrap();
        report
            .frames
            .iter()
            .filter(|f| f.cause.event == "REPLY")
            .map(|f| (format!("{}->{}", f.cause.to, f.state_after), f.status))
            .collect()
    }

    #[test]
    fn disjoint_arms_each_land_on_their_exact_target() {
        let statuses = arm_statuses(json!([
            { "from": "idle", "to": "done", "event": "REPLY", "guard": ["<", "@payload.status", 300] },
            { "from": "idle", "to": "failed", "event": "REPLY", "guard": [">=", "@payload.status", 300] },
            { "from": "idle", "to": "other", "event": "REPLY", "guard": ["=", "@payload.status", 404] }
        ]));
        assert_eq!(
            statuses,
            vec![
                ("done->done".to_string(), FrameStatus::Pass),
                ("failed->failed".to_string(), FrameStatus::Pass),
                ("other->failed".to_string(), FrameStatus::Inconclusive),
            ],
            "the third arm (404) is shadowed by the second (>= 300): a coverage gap, not a pass"
        );
    }

    #[test]
    fn an_arm_an_earlier_arm_takes_first_is_an_inconclusive_gap() {
        let statuses = arm_statuses(json!([
            { "from": "idle", "to": "done", "event": "REPLY", "guard": ["<", "@payload.status", 500] },
            { "from": "idle", "to": "failed", "event": "REPLY", "guard": ["<", "@payload.status", 300] }
        ]));
        assert_eq!(
            statuses,
            vec![
                ("done->done".to_string(), FrameStatus::Pass),
                ("failed->done".to_string(), FrameStatus::Inconclusive),
            ]
        );
    }

    #[test]
    fn an_arm_whose_guard_no_payload_is_planned_for_is_an_inconclusive_gap() {
        let statuses = arm_statuses(json!([
            { "from": "idle", "to": "done", "event": "REPLY", "guard": ["str/includes", "@payload.text", "x"] }
        ]));
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].1, FrameStatus::Inconclusive);
    }

    fn persistor(effect: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "Notes",
                "entity": { "name": "Note", "persistence": "runtime", "fields": [
                    { "name": "id", "type": "string" }, { "name": "content", "type": "string" }] },
                "traits": [{ "name": "NotePersistor", "linkedEntity": "Note",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [
                            { "key": "DO_CREATE", "name": "DO_CREATE", "payloadSchema": [
                                { "name": "data", "type": "object", "properties": [
                                    { "name": "content", "type": "string" }] }] },
                            { "key": "DO_UPDATE", "name": "DO_UPDATE", "payloadSchema": [
                                { "name": "data", "type": "object", "properties": [
                                    { "name": "content", "type": "string" }] }] }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "DO_CREATE", "effects": [effect] },
                            { "from": "idle", "to": "idle", "event": "DO_UPDATE",
                              "effects": [["persist", "update", "Note", "@payload.data"]] }
                        ]
                    } }]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn a_create_payload_never_carries_the_seed_rows_id_while_an_update_keeps_it() {
        let report = verify_schema("notes", persistor(json!(["persist", "create", "Note", "@payload.data"]))).unwrap();
        let frame = |event: &str| report.frames.iter().find(|f| f.cause.event == event).unwrap();
        let create = frame("DO_CREATE");
        assert_eq!(create.payload["data"]["id"], "Note-verify-new-1");
        assert!(create.effects.iter().all(|e| e.outcome != "failed"), "{:?}", create.effects);
        assert_eq!(create.status, FrameStatus::Pass);
        let update = frame("DO_UPDATE");
        assert_eq!(update.payload["data"]["id"], "Note-verify-seed-1");
        assert_eq!(update.status, FrameStatus::Pass, "{:?}", update.effects);
    }

    #[test]
    fn a_self_identity_create_is_not_seeded_but_its_update_is() {
        let mut schema = persistor(json!(["persist", "create", "Account", "@payload.data"]));
        let text = serde_json::to_string(&schema)
            .unwrap()
            .replace("\"name\":\"Note\"", "\"name\":\"Account\",\"identity\":true,\"read_policy\":[\"=\",\"@entity.id\",\"@user.id\"]")
            .replace("\"linkedEntity\":\"Note\"", "\"linkedEntity\":\"Account\"")
            .replace("[\"persist\",\"update\",\"Note\"", "[\"persist\",\"update\",\"Account\"");
        schema = serde_json::from_str(&text).unwrap();
        let viewer: Value = serde_json::from_value(json!({ "id": "viewer-1" })).unwrap();
        let report = verify_schema_as("accounts", schema, viewer).unwrap();
        let frame = |event: &str| report.frames.iter().find(|f| f.cause.event == event).unwrap();
        let create = frame("DO_CREATE");
        assert_eq!(create.payload["data"]["id"], "viewer-1");
        assert!(create.effects.iter().all(|e| e.outcome != "failed"), "{:?}", create.effects);
        assert_eq!(create.status, FrameStatus::Pass);
        assert_eq!(create.entity_counts.get("Account"), Some(&1), "the create made the only row");
        let update = frame("DO_UPDATE");
        assert_eq!(update.payload["data"]["id"], "viewer-1");
        assert!(update.effects.iter().all(|e| e.outcome != "failed"), "{:?}", update.effects);
        assert_eq!(update.status, FrameStatus::Pass);
    }

    #[test]
    fn a_reply_route_is_scoped_to_the_trait_it_is_delivered_to() {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "egress": ["https://api.example.com"],
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    { "name": "Caller", "linkedEntity": "Job",
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "waiting" }, { "name": "done" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "waiting", "event": "CALL", "effects": [
                                ["http", "GET", "https://api.example.com", "/items", {}, { "emit": { "success": "DONE" } }]] },
                            { "from": "waiting", "to": "done", "event": "DONE" }
                        ] } },
                    { "name": "Plain", "linkedEntity": "Job",
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "done", "event": "DONE" }
                        ] } }
                ]
            }]
        }))
        .unwrap();
        let report = verify_schema("scoped", schema).unwrap();
        let done = |trait_name: &str| {
            let frame = report
                .frames
                .iter()
                .find(|f| f.cause.event == "DONE" && f.cause.trait_name == trait_name)
                .unwrap();
            (frame.status, frame.gap, frame.state_after.clone())
        };
        assert_eq!(
            done("Caller"),
            (FrameStatus::Gap, Some(frame::GapReason::NotReachableWithoutNetwork), String::new())
        );
        assert_eq!(done("Plain"), (FrameStatus::Pass, None, "done".to_string()));
    }
}
