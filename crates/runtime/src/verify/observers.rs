//! Observers — pure verdicts over the captured frame stream (+ schema for structural
//! invariants). Each is the Rust analogue of a `packages/almadar-verify/src/observer/*`
//! assertion.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::verify::frame::{Frame, FrameStatus};
use orbital_core_base::schema::types::TraitDefinition;
use serde::Serialize;

/// One pass/fail verdict with a human detail.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

impl Verdict {
    fn new(name: &str, offenders: Vec<String>) -> Self {
        let passed = offenders.is_empty();
        let detail = if passed {
            String::new()
        } else {
            format!("{} offender(s): {}", offenders.len(), offenders.join("; "))
        };
        Verdict {
            name: name.to_string(),
            passed,
            detail,
        }
    }

    /// Pass when `offenders` is empty, else fail with an "N offender(s): …" detail.
    /// Public constructor for verdicts produced outside this module (e.g.
    /// `crate::shell`).
    pub fn from_offenders(name: &str, offenders: Vec<String>) -> Self {
        Self::new(name, offenders)
    }

    /// A passing verdict that still carries a human-readable note (e.g. a
    /// "gate skipped" reason).
    pub fn pass_with_detail(name: &str, detail: &str) -> Self {
        Verdict {
            name: name.to_string(),
            passed: true,
            detail: detail.to_string(),
        }
    }

    /// A structural verdict: pass when `offenders` is empty. On either outcome
    /// the detail records that the real compile is CI/SDK-deferred (so a green
    /// structural pass isn't mistaken for a full compile).
    pub fn structural(name: &str, offenders: Vec<String>, deferral: &str) -> Self {
        let passed = offenders.is_empty();
        let detail = if passed {
            deferral.to_string()
        } else {
            format!(
                "{} offender(s): {} ({})",
                offenders.len(),
                offenders.join("; "),
                deferral
            )
        };
        Verdict {
            name: name.to_string(),
            passed,
            detail,
        }
    }
}

/// All frame-based verdicts. (Render-ui binding resolution is NOT checked here — it is
/// client-only at the kernel layer; the deferred bindings are recorded in the report and
/// enforced by Layer 2.)
pub fn run_all(frames: &[Frame]) -> Vec<Verdict> {
    vec![
        state_transition(frames),
        effects_fired(frames),
        proposition_held(frames),
    ]
}

/// Every `@ensures` of a transition the walk fired held on the run (the author's oracle, not the derived one). An
/// inconclusive judgement is not a pass and not a failure: it is carried on the frame.
fn proposition_held(frames: &[Frame]) -> Verdict {
    let offenders = frames
        .iter()
        .flat_map(|f| f.propositions.iter())
        .filter_map(|c| match &c.verdict {
            crate::verify::plan_wire::PropositionVerdict::Broken { proposition } => Some(format!(
                "{}:{}--{}-->{} @ensures {}",
                c.trait_name,
                c.from,
                c.event,
                c.to,
                orbital_core_base::schema::format_sexpr(proposition)
            )),
            _ => None,
        })
        .collect();
    Verdict::new("proposition_held", offenders)
}

/// Every CONCLUSIVE step reached its declared target. `Inconclusive` frames (preamble
/// could not reach `from`) are a coverage gap surfaced in the summary, not a failure.
fn state_transition(frames: &[Frame]) -> Verdict {
    let offenders = frames
        .iter()
        .filter(|f| f.status == FrameStatus::Fail)
        .map(|f| {
            format!(
                "{}:{}--{}-->{} (got {})",
                f.cause.trait_name, f.cause.from, f.cause.event, f.cause.to, f.state_after
            )
        })
        .collect();
    Verdict::new("state_transition", offenders)
}

/// No declared effect failed during execution. An `http` failure the program routed to its declared
/// `emit.failure` event is handled, not a failure; an inconclusive frame is not evidence.
fn effects_fired(frames: &[Frame]) -> Verdict {
    let mut offenders = Vec::new();
    for f in frames
        .iter()
        .filter(|f| f.status != FrameStatus::Inconclusive)
    {
        for e in &f.effects {
            if e.outcome == "failed" && e.routed.is_none() {
                offenders.push(format!(
                    "{}:{} {}({}) failed: {}",
                    f.cause.trait_name,
                    f.cause.event,
                    e.kind,
                    e.target,
                    e.error.as_deref().unwrap_or("?")
                ));
            }
        }
    }
    Verdict::new("effects_fired", offenders)
}

/// Structural: every WALKED trait has a non-empty state machine (≥1 state, ≥1
/// transition) — `traits` is the same trait-scoped set the walk just drove, so a
/// `--trait`-scoped run reports only on the named trait(s), never on siblings that
/// were never walked.
pub fn ref_trait_invariant(traits: &[&TraitDefinition]) -> Verdict {
    let mut offenders = Vec::new();
    for td in traits {
        let sm = &td.state_machine;
        if sm.states.is_empty() {
            offenders.push(format!("{} has no states", td.name));
        }
        if sm.transitions.is_empty() {
            offenders.push(format!("{} has no transitions", td.name));
        }
    }
    Verdict::new("ref_trait_invariant", offenders)
}

/// One firing of an event on the session's bus, as a verifier saw it.
#[derive(Debug, Clone)]
pub struct EmitFiring {
    pub event: String,
    pub payload: orbital_core_base::value::Value,
    /// The emitting trait, when the bus entry names it; an unattributed firing counts for every declarer of `event`.
    pub trait_name: Option<String>,
    /// The firing followed a real click on the page, not a dispatch whose payload the planner synthesized.
    pub dom: bool,
}

struct DeclaredEmitField {
    trait_name: String,
    event: String,
    path: Vec<String>,
    required: bool,
}

fn declared_fields(
    fields: &[orbital_core_base::schema::types::PayloadField],
    prefix: &[String],
    trait_name: &str,
    event: &str,
    out: &mut Vec<DeclaredEmitField>,
) {
    for field in fields.iter().filter(|f| !f.name.is_empty()) {
        let mut path = prefix.to_vec();
        path.push(field.name.clone());
        // A plain nested object contract is walked to its leaves; an entity-typed field is a row and one leaf.
        match &field.properties {
            Some(nested)
                if !nested.is_empty() && field.entity.is_none() && field.field_type == "object" =>
            {
                declared_fields(nested, &path, trait_name, event, out)
            }
            _ => out.push(DeclaredEmitField {
                trait_name: trait_name.to_string(),
                event: event.to_string(),
                path,
                required: field.required == Some(true),
            }),
        }
    }
}

fn value_at<'a>(
    payload: &'a orbital_core_base::value::Value,
    path: &[String],
) -> Option<&'a orbital_core_base::value::Value> {
    path.iter()
        .try_fold(payload, |cursor, segment| match cursor {
            orbital_core_base::value::Value::Object(map) => map.get(segment.as_str()),
            _ => None,
        })
}

/// The twin of `assertEmitPayloadAlwaysEmpty` in `@almadar-io/verify`: a declared emit payload field that was empty on
/// every firing this session. A required field fails on its first empty firing (`''`, absent or null); an optional one
/// needs two firings, all `''` or absent (null is an optional field's declared absence). A required field empty on a
/// click-driven firing fails even when a synthesized dispatch once carried a value.
pub fn emit_payload_always_empty(
    schema: &orbital_core_base::schema::types::OrbitalSchema,
    firings: &[EmitFiring],
) -> Verdict {
    use orbital_core_base::schema::types::TraitReference;
    use orbital_core_base::value::Value;
    let mut declared = Vec::new();
    for def in schema
        .orbitals
        .iter()
        .flat_map(|o| o.traits.iter())
        .filter_map(|t| match t {
            TraitReference::Inline(def) => Some(def),
            _ => None,
        })
    {
        for emit in def.emits.iter().flatten() {
            declared_fields(
                emit.payload_schema.as_deref().unwrap_or_default(),
                &[],
                &def.name,
                &emit.event,
                &mut declared,
            );
        }
    }
    let mut offenders: Vec<String> = Vec::new();
    for field in &declared {
        let owned: Vec<&EmitFiring> = firings
            .iter()
            .filter(|f| {
                f.event == field.event
                    && f.trait_name
                        .as_deref()
                        .is_none_or(|t| t == field.trait_name)
            })
            .collect();
        let fired: Vec<&Value> = owned.iter().map(|f| &f.payload).collect();
        let empty = |payload: &&Value| match value_at(payload, &field.path) {
            None => true,
            Some(Value::String(s)) => s.is_empty(),
            Some(Value::Null) => field.required,
            Some(_) => false,
        };
        let always = fired.len() >= if field.required { 1 } else { 2 } && fired.iter().all(empty);
        let clicked: Vec<&Value> = owned.iter().filter(|f| f.dom).map(|f| &f.payload).collect();
        let on_click = field.required && clicked.iter().any(empty);
        if always || on_click {
            let line = if always {
                format!(
                    "{}'s {} fired {} time(s); {}field '{}' was empty on every firing",
                    field.trait_name,
                    field.event,
                    fired.len(),
                    if field.required { "required " } else { "" },
                    field.path.join("."),
                )
            } else {
                format!(
                    "{}'s {}: required field '{}' was empty on {}/{} click-driven firing(s); only a synthesized dispatch carried a value",
                    field.trait_name,
                    field.event,
                    field.path.join("."),
                    clicked.iter().filter(|p| empty(p)).count(),
                    clicked.len(),
                )
            };
            if !offenders.contains(&line) {
                offenders.push(line);
            }
        }
    }
    Verdict::new("emit-payload-always-empty", offenders)
}

/// The runtime's own verdict on one dispatch, as a host recorded it: the event and whether the runtime answered it
/// successfully, with its error.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchRecord {
    pub event: String,
    pub success: bool,
    pub error: Option<String>,
}

/// The twin of the TS walk's fail-closed dispatch check: a step whose own dispatch the runtime answered with
/// `success: false` (a thrown effect, a refused payload) failed, whatever state it left behind. `None` when the host
/// recorded no verdict for the event.
pub fn judge_dispatch(
    trait_name: &str,
    event: &str,
    records: &[DispatchRecord],
) -> Option<Verdict> {
    let own: Vec<&DispatchRecord> = records.iter().filter(|r| r.event == event).collect();
    if own.is_empty() {
        return None;
    }
    let offenders = own
        .iter()
        .filter(|r| !r.success)
        .map(|r| {
            format!(
                "{trait_name} {event}: {}",
                r.error
                    .as_deref()
                    .unwrap_or("the runtime reported success: false")
            )
        })
        .collect();
    Some(Verdict::new("dispatch", offenders))
}

#[cfg(test)]
mod proposition_tests {
    use super::{emit_payload_always_empty, judge_dispatch, DispatchRecord, EmitFiring};
    use orbital_core_base::schema::types::OrbitalSchema;

    fn wallet(spend: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "wallet",
            "orbitals": [{
                "name": "Wallet",
                "entity": { "name": "Account", "persistence": "runtime",
                    "fields": [{ "name": "id", "type": "string" }, { "name": "balance", "type": "number", "default": 0 }] },
                "traits": [{
                    "name": "Wallet", "linkedEntity": "Account",
                    "stateMachine": {
                        "states": [{ "name": "active", "isInitial": true }],
                        "events": [{ "key": "INIT", "name": "Init" },
                            { "key": "SPEND", "name": "Spend", "payloadSchema": [{ "name": "amount", "type": "number", "required": true }] }],
                        "transitions": [
                            { "from": "active", "to": "active", "event": "INIT", "effects": [["set", "@entity.balance", 100]] },
                            { "from": "active", "to": "active", "event": "SPEND",
                              "guard": ["and", [">=", "@payload.amount", 0], ["<=", "@payload.amount", "@entity.balance"]],
                              "effects": [["set", "@entity.balance", spend]],
                              "ensures": [["=", "@entity.balance", ["-", "@fromEntity.balance", "@payload.amount"]]] }
                        ]
                    }
                }],
                "pages": []
            }]
        }))
        .unwrap()
    }

    fn held(schema: OrbitalSchema) -> (bool, Vec<String>, usize) {
        let report = crate::verify::verify_schema("wallet", schema).unwrap();
        let v = report
            .verdicts
            .iter()
            .find(|v| v.name == "proposition_held")
            .expect("the verdict is always reported");
        let judged = report.frames.iter().map(|f| f.propositions.len()).sum();
        (
            v.passed,
            v.detail.lines().map(str::to_string).collect(),
            judged,
        )
    }

    #[test]
    fn a_correct_transition_holds_its_ensures_on_the_walk() {
        let (passed, detail, judged) = held(wallet(serde_json::json!([
            "-",
            "@entity.balance",
            "@payload.amount"
        ])));
        assert!(judged > 0, "the walk judged the SPEND step");
        assert!(passed, "{detail:?}");
    }

    #[test]
    fn a_wrong_value_in_the_right_state_fails_proposition_held() {
        let (passed, detail, _) = held(wallet(serde_json::json!([
            "-",
            ["-", "@entity.balance", "@payload.amount"],
            1
        ])));
        assert!(
            !passed,
            "the off-by-one write reaches the right state but breaks the contract"
        );
        assert!(
            detail
                .iter()
                .any(|l| l.contains("SPEND") && l.contains("@ensures")),
            "{detail:?}"
        );
    }

    fn chat_schema() -> orbital_core_base::schema::types::OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "chat",
            "orbitals": [{
                "name": "ChatOrbital",
                "entity": { "name": "Msg", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Composer",
                    "stateMachine": { "states": [{ "name": "ready", "isInitial": true }], "events": [], "transitions": [] },
                    "emits": [{
                        "event": "SAVE",
                        "payloadSchema": [
                            { "name": "data", "type": "object", "properties": [{ "name": "content", "type": "string", "required": true }] },
                            { "name": "note", "type": "string" }
                        ]
                    }]
                }],
                "pages": []
            }]
        }))
        .expect("schema")
    }

    fn firing(payload: serde_json::Value, trait_name: Option<&str>) -> EmitFiring {
        EmitFiring {
            event: "SAVE".to_string(),
            payload: serde_json::from_value(payload).expect("value"),
            trait_name: trait_name.map(str::to_string),
            dom: false,
        }
    }

    #[test]
    fn a_required_field_empty_on_a_click_fails_though_a_dispatch_carried_it() {
        let mut click = firing(serde_json::json!({ "data": { "content": "" } }), None);
        click.dom = true;
        let firings = [
            firing(serde_json::json!({ "data": { "content": "verify" } }), None),
            click.clone(),
        ];
        let verdict = emit_payload_always_empty(&chat_schema(), &firings);
        assert!(
            !verdict.passed && verdict.detail.contains("click-driven"),
            "{}",
            verdict.detail
        );
        let mut carried = click;
        carried.payload = serde_json::from_value(
            serde_json::json!({ "data": { "content": "typed" }, "note": "n" }),
        )
        .expect("value");
        let firings = [
            firing(
                serde_json::json!({ "data": { "content": "verify" }, "note": "n" }),
                None,
            ),
            carried,
        ];
        assert!(
            emit_payload_always_empty(&chat_schema(), &firings).passed,
            "control: a click that carries the field"
        );
    }

    #[test]
    fn a_required_nested_field_empty_on_its_one_firing_fails() {
        let verdict = emit_payload_always_empty(
            &chat_schema(),
            &[firing(
                serde_json::json!({ "data": { "content": "" }, "note": "x" }),
                None,
            )],
        );
        assert!(!verdict.passed);
        assert!(
            verdict.detail.contains("data.content"),
            "{}",
            verdict.detail
        );
        assert!(
            !verdict.detail.contains("'note'"),
            "control: a carried optional field is not flagged"
        );
    }

    #[test]
    fn one_carried_value_clears_the_field() {
        let firings = [
            firing(
                serde_json::json!({ "data": { "content": "" }, "note": "a" }),
                None,
            ),
            firing(
                serde_json::json!({ "data": { "content": "hi" }, "note": "b" }),
                None,
            ),
        ];
        assert!(emit_payload_always_empty(&chat_schema(), &firings).passed);
    }

    #[test]
    fn an_optional_field_needs_two_empty_firings_and_null_is_its_absence() {
        let one = [firing(
            serde_json::json!({ "data": { "content": "hi" } }),
            None,
        )];
        assert!(
            emit_payload_always_empty(&chat_schema(), &one).passed,
            "one firing proves nothing for an optional field"
        );
        let two_null = [
            firing(
                serde_json::json!({ "data": { "content": "hi" }, "note": null }),
                None,
            ),
            firing(
                serde_json::json!({ "data": { "content": "ho" }, "note": null }),
                None,
            ),
        ];
        assert!(
            emit_payload_always_empty(&chat_schema(), &two_null).passed,
            "null is an optional field's declared absence"
        );
        let two_absent = [
            firing(serde_json::json!({ "data": { "content": "hi" } }), None),
            firing(serde_json::json!({ "data": { "content": "ho" } }), None),
        ];
        let verdict = emit_payload_always_empty(&chat_schema(), &two_absent);
        assert!(
            !verdict.passed && verdict.detail.contains("'note'"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_firing_attributed_to_another_trait_and_an_unfired_event_are_silent() {
        let other = [firing(
            serde_json::json!({ "data": { "content": "" } }),
            Some("Elsewhere"),
        )];
        assert!(emit_payload_always_empty(&chat_schema(), &other).passed);
        assert!(emit_payload_always_empty(&chat_schema(), &[]).passed);
        let own = [firing(
            serde_json::json!({ "data": { "content": "" } }),
            Some("Composer"),
        )];
        assert!(!emit_payload_always_empty(&chat_schema(), &own).passed);
    }

    #[test]
    fn a_dispatch_the_runtime_refused_fails_its_step() {
        let record = |event: &str, success: bool, error: Option<&str>| DispatchRecord {
            event: event.into(),
            success,
            error: error.map(str::to_string),
        };
        let refused = judge_dispatch(
            "Card",
            "INIT",
            &[record("INIT", false, Some("Card.READY: Type mismatch"))],
        )
        .expect("a verdict");
        assert!(
            !refused.passed && refused.detail.contains("Type mismatch"),
            "{}",
            refused.detail
        );
        assert!(
            judge_dispatch("Card", "INIT", &[record("INIT", true, None)]).is_some_and(|v| v.passed)
        );
        assert!(
            judge_dispatch("Card", "INIT", &[record("OPEN", false, None)]).is_none(),
            "control: another event's verdict"
        );
    }
}
