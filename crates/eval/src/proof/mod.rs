//! The one symbolic reasoner over guard expressions: witness search for the verify planner, and the decision
//! procedure that proves spec invariants (`@invariant`, `@holds`) for `orb validate`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

pub mod confirm;
pub mod decide;
pub mod formula;
pub mod obligations;
pub mod rat;
pub mod witness;

use decide::{decide, fill_free, Outcome};
use formula::{Formula, Translator};
use obligations::{obligations, Proposition};
use orbital_core_base::schema::types::OrbitalSchema;

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Proved,
    /// A counterexample the evaluator confirms, on an obligation the model does not over-approximate.
    Violated {
        counterexample: serde_json::Value,
        /// Why the proposition fails: the proposition, and how many conditions reaching the site held on the model.
        reason: String,
    },
    Unproved {
        reason: String,
        counterexample: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub proposition: Proposition,
    pub site: String,
    pub verdict: Verdict,
}

/// How a spec proposition fared: a counterexample the evaluator confirmed, or a proof the prover could not make.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProveVerdict {
    Violated,
    Unproved,
}

/// The proposition a finding is about.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ProvedProposition {
    Row {
        entity: String,
        #[cfg_attr(feature = "ts-export", ts(type = "JsonValue"))]
        expr: orbital_core_base::schema::SExpression,
    },
    State {
        #[serde(rename = "trait")]
        trait_name: String,
        state: String,
        #[cfg_attr(feature = "ts-export", ts(type = "JsonValue"))]
        expr: orbital_core_base::schema::SExpression,
    },
    Transition {
        #[serde(rename = "trait")]
        trait_name: String,
        from: String,
        event: String,
        to: String,
        #[cfg_attr(feature = "ts-export", ts(type = "JsonValue"))]
        expr: orbital_core_base::schema::SExpression,
    },
}

/// One spec proposition the program cannot be proved to keep. `counterexample` is the confirmed one of a `violated`
/// finding, or the possible one an `unproved` finding found.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProveFinding {
    pub verdict: ProveVerdict,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(type = "JsonValue"))]
    pub counterexample: Option<serde_json::Value>,
    pub site: String,
    pub proposition: ProvedProposition,
}

/// What `program/prove` answers: every proposition not proved, in obligation order.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProveReport {
    pub findings: Vec<ProveFinding>,
}

impl Finding {
    /// The finding as the report prints it; a proved proposition is no finding.
    pub fn unproved(self) -> Option<ProveFinding> {
        let proposition = match self.proposition {
            Proposition::Row { entity, expr } => ProvedProposition::Row { entity, expr },
            Proposition::State { trait_name, state, expr } => ProvedProposition::State { trait_name, state, expr },
            Proposition::Transition { trait_name, from, event, to, expr } => {
                ProvedProposition::Transition { trait_name, from, event, to, expr }
            }
        };
        let (verdict, reason, counterexample) = match self.verdict {
            Verdict::Proved => return None,
            Verdict::Violated { counterexample, reason } => (ProveVerdict::Violated, reason, Some(counterexample)),
            Verdict::Unproved { reason, counterexample } => (ProveVerdict::Unproved, reason, counterexample),
        };
        Some(ProveFinding { verdict, reason, counterexample, site: self.site, proposition })
    }
}

/// The report of proving every spec proposition of the program.
pub fn prove_report(schema: &OrbitalSchema) -> ProveReport {
    ProveReport { findings: prove(schema).into_iter().filter_map(Finding::unproved).collect() }
}

/// Every obligation the program's spec propositions owe, with its verdict.
pub fn prove(schema: &OrbitalSchema) -> Vec<Finding> {
    obligations(schema)
        .into_iter()
        .map(|o| {
            let verdict = match &o.unbuildable {
                Some(reason) => Verdict::Unproved {
                    reason: reason.clone(),
                    counterexample: None,
                },
                None => judge(&o),
            };
            Finding {
                proposition: o.proposition,
                site: o.site,
                verdict,
            }
        })
        .collect()
}

fn judge(o: &obligations::Obligation) -> Verdict {
    let overflow = || Verdict::Unproved {
        reason: "arithmetic overflow while building the formula".to_string(),
        counterexample: None,
    };
    // Time is a positive epoch number the whole obligation shares (`@now` and `(time/now)`).
    let mut vars = o.vars.clone();
    vars.entry("@now".to_string()).or_insert(formula::VarInfo {
        sort: formula::Sort::Num,
        nullable: false,
    });
    let mut lens = Vec::new();
    for e in o.assumptions.iter().chain(std::iter::once(&o.goal)) {
        formula::array_lens(e, &mut lens);
    }
    for len in &lens {
        vars.insert(
            formula::array_len_var(len),
            formula::VarInfo {
                sort: formula::Sort::Num,
                nullable: false,
            },
        );
    }
    let mut t = Translator::new(&vars).with_evaluated(&o.evaluated);
    let mut parts = Vec::new();
    for len in &lens {
        let nonneg = orbital_core_base::schema::sexpr::SExpression::Array(vec![
            orbital_core_base::schema::sexpr::SExpression::string(">="),
            orbital_core_base::schema::sexpr::SExpression::string(formula::array_len_var(len)),
            orbital_core_base::schema::sexpr::SExpression::Number(0.0),
        ].into());
        match t.truthy(&nonneg) {
            Some(f) => parts.push(f),
            None => return overflow(),
        }
    }
    match t.truthy(&orbital_core_base::schema::sexpr::SExpression::Array(vec![
        orbital_core_base::schema::sexpr::SExpression::string(">"),
        orbital_core_base::schema::sexpr::SExpression::string("@now"),
        orbital_core_base::schema::sexpr::SExpression::Number(0.0),
    ].into())) {
        Some(f) => parts.push(f),
        None => return overflow(),
    }
    for a in &o.assumptions {
        match t.truthy(a) {
            Some(f) => parts.push(f),
            None => return overflow(),
        }
    }
    let Some(goal) = t.falsy(&o.goal) else {
        return overflow();
    };
    parts.push(goal);
    match decide(&Formula::and(parts), &vars) {
        Outcome::Unsat => Verdict::Proved,
        Outcome::Unknown(reason) => Verdict::Unproved {
            reason,
            counterexample: None,
        },
        Outcome::Sat(mut model) => {
            let mut read = std::collections::BTreeSet::new();
            for e in o.assumptions.iter().chain(std::iter::once(&o.goal)) {
                formula::binding_texts(e, &mut read);
            }
            fill_free(&mut model, read, &vars);
            let cex = confirm::counterexample(&model);
            if !confirm::confirms(&model, &o.assumptions, &o.goal) {
                let undecided: Vec<String> = t
                    .opaque
                    .values()
                    .map(orbital_core_base::schema::format_sexpr)
                    .collect();
                let reason = if undecided.is_empty() {
                    "the solver's model is not confirmed by the evaluator".to_string()
                } else {
                    format!("outside the decidable fragment: {}", undecided.join(", "))
                };
                Verdict::Unproved {
                    reason,
                    counterexample: None,
                }
            } else if let Some(reason) = &o.inexact {
                Verdict::Unproved {
                    reason: reason.clone(),
                    counterexample: Some(cex),
                }
            } else {
                let goal = orbital_core_base::schema::format_sexpr(match &o.proposition {
                    Proposition::Row { expr, .. } | Proposition::State { expr, .. } | Proposition::Transition { expr, .. } => expr,
                });
                let held = o.assumptions.len();
                let reason = if held == 0 {
                    format!("`{goal}` is false on the counterexample the evaluator confirmed")
                } else {
                    format!("`{goal}` is false on the counterexample the evaluator confirmed, while the {held} condition(s) reaching the site hold")
                };
                Verdict::Violated {
                    counterexample: cex,
                    reason,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
