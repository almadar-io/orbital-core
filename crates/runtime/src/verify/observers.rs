//! Observers — pure verdicts over the captured frame stream (+ schema for structural
//! invariants). Each is the Rust analogue of a `packages/almadar-verify/src/observer/*`
//! assertion.

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
    vec![state_transition(frames), effects_fired(frames), proposition_held(frames)]
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
/// `emit.failure` event is handled, not a failure.
fn effects_fired(frames: &[Frame]) -> Verdict {
    let mut offenders = Vec::new();
    for f in frames {
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

#[cfg(test)]
mod proposition_tests {
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
        let v = report.verdicts.iter().find(|v| v.name == "proposition_held").expect("the verdict is always reported");
        let judged = report.frames.iter().map(|f| f.propositions.len()).sum();
        (v.passed, v.detail.lines().map(str::to_string).collect(), judged)
    }

    #[test]
    fn a_correct_transition_holds_its_ensures_on_the_walk() {
        let (passed, detail, judged) = held(wallet(serde_json::json!(["-", "@entity.balance", "@payload.amount"])));
        assert!(judged > 0, "the walk judged the SPEND step");
        assert!(passed, "{detail:?}");
    }

    #[test]
    fn a_wrong_value_in_the_right_state_fails_proposition_held() {
        let (passed, detail, _) = held(wallet(serde_json::json!(["-", ["-", "@entity.balance", "@payload.amount"], 1])));
        assert!(!passed, "the off-by-one write reaches the right state but breaks the contract");
        assert!(detail.iter().any(|l| l.contains("SPEND") && l.contains("@ensures")), "{detail:?}");
    }
}
