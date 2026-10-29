//! Spec propositions as proof obligations over a resolved program.
//!
//! A row invariant of entity `X` is owed at every write of `X` (`persist create|update|upsert`, single or batched):
//! the new row satisfies it given the transition's guard, the branch conditions that reach the write and, for an
//! update, every row invariant of the old row. A state invariant of trait `T` in state `S` is owed on every
//! transition and tick that lands in `S` (assuming it before when it starts in `S`), on mount when `S` is initial,
//! and on every other trait's write that can change `T`'s `@entity` while `T` sits in `S`.
//!
//! Effects run in order and later effects read earlier `set`s (`exec_effect_expr`), so each effect list is executed
//! symbolically, splitting on `if` / `when`. An obligation the model over-approximates is marked `inexact`: its
//! counterexample may not be reachable, so it can never be reported as a violation.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::formula::{Sort, VarInfo, Vars};
use orbital_core_base::effect_types::strip_trailing_emit_options;
use orbital_core_base::persist_batch::{
    batch_written_entities, classify_ops, op_tuple, BatchOps,
};
use orbital_core_base::policy::identity_entity_def;
use orbital_core_base::schema::sexpr::{SExpression, SequentialBindings};
use orbital_core_base::schema::types::{
    EntityDefinition, EntityRef, ExpectDeclaration, FieldDefinition, FieldType, OrbitalSchema,
    PayloadField, Persistence, TraitDefinition, TraitReference,
};
use std::collections::{BTreeMap, BTreeSet};

/// The old row an update merges into.
pub const ROW: &str = "@__row";
/// The row a trait is bound to on mount.
pub const INIT: &str = "@__init";
/// Another trait's `@entity`, for a write that is not the proposition's own trait.
pub const OTHER: &str = "@__other";
/// A value the program reads from a row it fetched.
pub const FETCHED: &str = "@__fetched";
/// The entity of a write whose target is not statically known.
pub const UNKNOWN_TARGET: &str = "@?";
/// The head the prover gives a value it cannot model; the evaluator has no such operator, so a counterexample that
/// depends on one is never confirmed.
pub const UNRESOLVED: &str = "proof/unresolved";

const MAX_PATHS: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Proposition {
    Row {
        entity: String,
        expr: SExpression,
    },
    State {
        trait_name: String,
        state: String,
        expr: SExpression,
    },
    /// `@ensures` of the transition `from --event--> to`: holds over the `@entity` after its effects, with
    /// `@fromEntity` the `@entity` before them.
    Transition {
        trait_name: String,
        from: String,
        event: String,
        to: String,
        expr: SExpression,
    },
}

/// The root an `@ensures` reads the entity before the transition through.
pub const FROM_ENTITY: &str = "@fromEntity";

pub struct Obligation {
    pub proposition: Proposition,
    /// Where it is owed, e.g. `Checkout: open --PAY--> paid`.
    pub site: String,
    pub assumptions: Vec<SExpression>,
    pub goal: SExpression,
    pub vars: Vars,
    /// Why a counterexample may be unreachable, when the model over-approximates.
    pub inexact: Option<String>,
    /// Set when the obligation could not be built (too many effect paths).
    pub unbuildable: Option<String>,
    /// Values of the `set`s this path ran: a `set` whose value fails to evaluate stops its transition, so each
    /// evaluated to some value wherever the post-state is reached.
    pub evaluated: Vec<SExpression>,
}

/// Every proposition the program owes, once: each strict spec proposition, and each one written on a real entity,
/// state or transition. A structurally equal pair is one proposition.
pub fn propositions(schema: &OrbitalSchema) -> Vec<Proposition> {
    let mut out: Vec<Proposition> = Vec::new();
    let mut add = |p: Proposition| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    for orbital in &schema.orbitals {
        for e in orbital.expects.iter().flatten().filter(|e| e.is_strict()) {
            match e {
                ExpectDeclaration::Entity {
                    name,
                    invariants: Some(list),
                    ..
                } => {
                    for expr in list {
                        add(Proposition::Row {
                            entity: name.clone(),
                            expr: expr.clone(),
                        });
                    }
                }
                ExpectDeclaration::State {
                    trait_name,
                    state,
                    holds: Some(expr),
                    ..
                } => {
                    add(Proposition::State {
                        trait_name: trait_name.clone(),
                        state: state.clone(),
                        expr: expr.clone(),
                    });
                }
                ExpectDeclaration::Transition {
                    trait_name,
                    from,
                    event,
                    to,
                    ensures: Some(list),
                    ..
                } => {
                    for expr in list {
                        add(Proposition::Transition {
                            trait_name: trait_name.clone(),
                            from: from.clone(),
                            event: event.clone(),
                            to: to.clone(),
                            expr: expr.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
        if orbital.is_spec() {
            continue;
        }
        for e in std::iter::once(&orbital.entity).chain(orbital.auxiliary_entities.iter().flatten())
        {
            if let EntityRef::Inline(def) = e {
                for expr in def.invariants.iter().flatten() {
                    add(Proposition::Row {
                        entity: def.name.clone(),
                        expr: expr.clone(),
                    });
                }
            }
        }
        for t in &orbital.traits {
            let TraitReference::Inline(td) = t else {
                continue;
            };
            for st in &td.state_machine.states {
                if let Some(expr) = &st.holds {
                    add(Proposition::State {
                        trait_name: td.name.clone(),
                        state: st.name.clone(),
                        expr: expr.clone(),
                    });
                }
            }
            for tr in &td.state_machine.transitions {
                for expr in tr.ensures.iter().flatten() {
                    for from in tr.from.states() {
                        add(Proposition::Transition {
                            trait_name: td.name.clone(),
                            from: from.to_string(),
                            event: tr.event.clone(),
                            to: tr.to.clone(),
                            expr: expr.clone(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// Every obligation the program owes its propositions.
pub fn obligations(schema: &OrbitalSchema) -> Vec<Obligation> {
    let props = propositions(schema);
    let program = Program::new(schema);
    let mut out = Vec::new();
    for p in &props {
        match p {
            Proposition::Row { entity, expr } => {
                let invariants: Vec<SExpression> = props
                    .iter()
                    .filter_map(|q| match q {
                        Proposition::Row { entity: e, expr } if e == entity => Some(expr.clone()),
                        _ => None,
                    })
                    .collect();
                program.row_obligations(p, entity, expr, &invariants, &mut out);
            }
            Proposition::State {
                trait_name,
                state,
                expr,
            } => {
                let row_invariants = |entity: &str| -> Vec<SExpression> {
                    props
                        .iter()
                        .filter_map(|q| match q {
                            Proposition::Row { entity: e, expr } if e == entity => {
                                Some(expr.clone())
                            }
                            _ => None,
                        })
                        .collect()
                };
                let state_holds = |trait_: &str, st: &str| -> Vec<SExpression> {
                    props
                        .iter()
                        .filter_map(|q| match q {
                            Proposition::State {
                                trait_name: t,
                                state: s,
                                expr,
                            } if t == trait_ && s == st => Some(expr.clone()),
                            _ => None,
                        })
                        .collect()
                };
                program.state_obligations(
                    p,
                    trait_name,
                    state,
                    expr,
                    &Known {
                        rows: &row_invariants,
                        states: &state_holds,
                    },
                    &mut out,
                );
            }
            Proposition::Transition {
                trait_name,
                from,
                event,
                to,
                expr,
            } => {
                let source_holds: Vec<SExpression> = props
                    .iter()
                    .filter_map(|q| match q {
                        Proposition::State {
                            trait_name: t,
                            state: s,
                            expr,
                        } if t == trait_name && s == from => Some(expr.clone()),
                        _ => None,
                    })
                    .collect();
                program.transition_obligations(
                    p,
                    (trait_name, from, event, to),
                    expr,
                    &source_holds,
                    &mut out,
                );
            }
        }
    }
    out
}

use super::witness;

mod program;
mod exec;
mod helpers;
use program::*;
use exec::*;
use helpers::*;
pub use helpers::{field_sort, subst_fields, unresolved};
