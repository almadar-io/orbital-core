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

use decide::{decide, Outcome};
use formula::{Formula, Translator};
use obligations::{obligations, Proposition};
use orbital_core_base::schema::types::OrbitalSchema;

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Proved,
    /// A counterexample the evaluator confirms, on an obligation the model does not over-approximate.
    Violated {
        counterexample: serde_json::Value,
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
    let mut t = Translator::new(&vars);
    let mut parts = Vec::new();
    for len in &lens {
        let nonneg = orbital_core_base::schema::sexpr::SExpression::Array(vec![
            orbital_core_base::schema::sexpr::SExpression::String(">=".to_string()),
            orbital_core_base::schema::sexpr::SExpression::String(formula::array_len_var(len)),
            orbital_core_base::schema::sexpr::SExpression::Number(0.0),
        ]);
        match t.truthy(&nonneg) {
            Some(f) => parts.push(f),
            None => return overflow(),
        }
    }
    match t.truthy(&orbital_core_base::schema::sexpr::SExpression::Array(vec![
        orbital_core_base::schema::sexpr::SExpression::String(">".to_string()),
        orbital_core_base::schema::sexpr::SExpression::String("@now".to_string()),
        orbital_core_base::schema::sexpr::SExpression::Number(0.0),
    ])) {
        Some(f) => parts.push(f),
        None => return overflow(),
    }
    for a in &o.assumptions {
        match t.truthy(a) {
            Some(f) => parts.push(f),
            None => return overflow(),
        }
    }
    let Some(goal) = t.truthy(&o.goal).and_then(Formula::negate) else {
        return overflow();
    };
    parts.push(goal);
    match decide(&Formula::and(parts), &vars) {
        Outcome::Unsat => Verdict::Proved,
        Outcome::Unknown(reason) => Verdict::Unproved {
            reason,
            counterexample: None,
        },
        Outcome::Sat(model) => {
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
                Verdict::Violated {
                    counterexample: cex,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
