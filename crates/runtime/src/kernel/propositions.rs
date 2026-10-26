//! The `@ensures` judge: a fired transition's propositions evaluated on one run, through the evaluator the guards use.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

/// One fired transition as a proposition judge reads it: whether it completed, and the trait's `@entity` at its start
/// (`@fromEntity`) and after its effects (`@entity`), with the payload it was delivered with.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropositionObservation {
    pub executed: bool,
    /// An effect failed: the transition stopped part way (EXEC-STEP-7), so its contract is not owed.
    pub failed: bool,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub entity_before: orbital_core_base::value::Value,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub entity_after: orbital_core_base::value::Value,
    #[serde(default)]
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub payload: orbital_core_base::value::Value,
    /// Set by a driver whose `entityAfter` is not the full layered read (it could not run some effect that writes
    /// the row): the judgement is inconclusive, with this reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unobserved: Option<String>,
}

impl PropositionObservation {
    /// A dispatched hop as observed: the rows the kernel recorded before and after its effects.
    pub fn of_hop(hop: &crate::process_result::ProcessEventResult) -> Self {
        let entry = hop.entry.as_ref();
        Self {
            executed: hop.executed,
            failed: hop.failure.is_some()
                || hop.effect_results.iter().any(|r| {
                    matches!(
                        r.outcome,
                        orbital_core_base::effect_types::EffectOutcome::Failed
                    )
                }),
            entity_before: entry
                .map(|e| e.entity_before.to_value())
                .unwrap_or(orbital_core_base::value::Value::Null),
            entity_after: entry
                .and_then(|e| e.entity_after.as_ref().map(|after| after.to_value()))
                .unwrap_or(orbital_core_base::value::Value::Null),
            payload: orbital_core_base::value::Value::clone(&hop.payload),
            unobserved: None,
        }
    }
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum PropositionVerdict {
    Held,
    Broken {
        #[cfg_attr(test, ts(type = "JsonValue"))]
        proposition: orbital_core_base::schema::SExpression,
    },
    /// Never a pass: the transition did not complete, or a proposition could not be evaluated.
    Inconclusive {
        reason: String,
    },
}

/// Judge a transition's `@ensures` on one run, through the evaluator the guards use: `@entity` is the row after the
/// effects and `@fromEntity` the row before them.
pub fn judge_propositions(
    ensures: &[orbital_core_base::schema::SExpression],
    observation: &PropositionObservation,
) -> PropositionVerdict {
    use orbital_core_eval::evaluator::{context::BindingContextBuilder, Evaluator};
    if !observation.executed {
        return PropositionVerdict::Inconclusive {
            reason: "the transition did not fire".to_string(),
        };
    }
    if observation.failed {
        return PropositionVerdict::Inconclusive {
            reason: "an effect failed, so the transition did not complete".to_string(),
        };
    }
    if let Some(reason) = &observation.unobserved {
        return PropositionVerdict::Inconclusive {
            reason: format!("the row after the transition was not observed: {reason}"),
        };
    }
    let ctx = BindingContextBuilder::new()
        .entity(observation.entity_after.clone())
        .payload(observation.payload.clone())
        .local("fromEntity", observation.entity_before.clone())
        .build();
    let evaluator = Evaluator::new();
    for p in ensures {
        let shown = orbital_core_base::schema::format_sexpr(p);
        match evaluator.evaluate(p, &ctx) {
            Ok(orbital_core_base::value::Value::Bool(true)) => {}
            Ok(orbital_core_base::value::Value::Bool(false)) => {
                return PropositionVerdict::Broken {
                    proposition: p.clone(),
                }
            }
            // A proposition is a comparison: any other value (an unknown head reads as a list) proves nothing.
            Ok(other) => {
                return PropositionVerdict::Inconclusive {
                    reason: format!(
                        "`{shown}` evaluated to {}, not a boolean",
                        serde_json::to_string(&other).unwrap_or_default()
                    ),
                }
            }
            Err(e) => {
                return PropositionVerdict::Inconclusive {
                    reason: format!("`{shown}` could not be evaluated: {e}"),
                }
            }
        }
    }
    PropositionVerdict::Held
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop_obs(
        executed: bool,
        failed: bool,
        before: f64,
        after: f64,
        amount: f64,
    ) -> PropositionObservation {
        serde_json::from_value(serde_json::json!({
            "executed": executed, "failed": failed,
            "entityBefore": { "balance": before }, "entityAfter": { "balance": after }, "payload": { "amount": amount }
        }))
        .unwrap()
    }

    fn spend() -> Vec<orbital_core_base::schema::SExpression> {
        serde_json::from_value(serde_json::json!([[
            "=",
            "@entity.balance",
            ["-", "@fromEntity.balance", "@payload.amount"]
        ]]))
        .unwrap()
    }

    #[test]
    fn an_ensures_that_held_on_the_run_is_held() {
        assert_eq!(
            judge_propositions(&spend(), &prop_obs(true, false, 10.0, 7.0, 3.0)),
            PropositionVerdict::Held
        );
    }

    #[test]
    fn a_wrong_value_in_the_right_state_breaks_the_ensures() {
        let PropositionVerdict::Broken { proposition } =
            judge_propositions(&spend(), &prop_obs(true, false, 10.0, 6.0, 3.0))
        else {
            panic!("an off-by-one write must break it")
        };
        assert_eq!(proposition, spend()[0]);
    }

    #[test]
    fn a_step_that_did_not_complete_is_inconclusive_never_held() {
        assert!(
            matches!(
                judge_propositions(&spend(), &prop_obs(false, false, 10.0, 10.0, 3.0)),
                PropositionVerdict::Inconclusive { .. }
            ),
            "its guard did not hold"
        );
        assert!(
            matches!(
                judge_propositions(&spend(), &prop_obs(true, true, 10.0, 7.0, 3.0)),
                PropositionVerdict::Inconclusive { .. }
            ),
            "an effect failed (EXEC-STEP-7)"
        );
    }

    #[test]
    fn a_proposition_the_evaluator_cannot_run_is_inconclusive() {
        let broken: Vec<orbital_core_base::schema::SExpression> =
            serde_json::from_value(serde_json::json!([["no/such-op", "@entity.balance"]])).unwrap();
        assert!(matches!(
            judge_propositions(&broken, &prop_obs(true, false, 1.0, 1.0, 0.0)),
            PropositionVerdict::Inconclusive { .. }
        ));
    }

    #[test]
    fn a_driver_that_could_not_observe_the_row_gets_inconclusive() {
        let mut o = prop_obs(true, false, 10.0, 7.0, 3.0);
        o.unobserved = Some("the walk does not run persists".to_string());
        let PropositionVerdict::Inconclusive { reason } = judge_propositions(&spend(), &o) else {
            panic!("never held on a partial row")
        };
        assert!(reason.contains("persists"), "{reason}");
    }
}
