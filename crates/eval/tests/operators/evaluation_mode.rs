//! The installed operator table declares how the evaluator runs each operator it dispatches (EXEC-EVAL-3b), and the
//! declaration is the implementation's: a strict operator's operands are evaluated before it runs, a form evaluates
//! its own.

use orbital_core_base::language::registry::{operators, OperatorEvaluation};
use orbital_core_eval::evaluator::operators::{get_operator, DISPATCHED_NAMES};
use orbital_core_eval::language_loader::preflight;

#[test]
fn the_table_declares_the_evaluation_mode_the_evaluator_implements() {
    preflight().expect("the installed language data must load");
    let mut drift = Vec::new();
    for name in DISPATCHED_NAMES {
        let Some(kind) = get_operator(name) else { continue };
        let implemented = kind.evaluation();
        let declared = operators().get(*name).and_then(|op| op.evaluation);
        if declared != Some(implemented) {
            drift.push(format!("{name}\t{implemented:?}\t{declared:?}"));
        }
    }
    drift.sort();
    assert!(drift.is_empty(), "{} operators (name, implemented, declared):\n{}", drift.len(), drift.join("\n"));
}

#[test]
fn control_an_operator_the_evaluator_does_not_dispatch_declares_no_mode() {
    preflight().expect("the installed language data must load");
    let stray: Vec<&String> = operators()
        .iter()
        .filter(|(name, op)| op.evaluation.is_some() && get_operator(name).is_none())
        .map(|(name, _)| name)
        .collect();
    assert!(stray.is_empty(), "{stray:?}");
}
