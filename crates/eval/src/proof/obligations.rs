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
    batch_written_entities, classify_ops, is_op_tuple, BatchOps,
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

// ---------------------------------------------------------------------------------------------------------------
// The program as the prover reads it
// ---------------------------------------------------------------------------------------------------------------

/// The propositions the rest of the program already owes, for induction.
struct Known<'a> {
    rows: &'a dyn Fn(&str) -> Vec<SExpression>,
    states: &'a dyn Fn(&str, &str) -> Vec<SExpression>,
}

struct Program<'a> {
    schema: &'a OrbitalSchema,
    traits: Vec<&'a TraitDefinition>,
    entities: Vec<&'a EntityDefinition>,
}

/// One transition or tick: what fires it, from where to where, and what it does.
struct Step<'a> {
    site: String,
    event: Option<&'a str>,
    from: Vec<&'a str>,
    /// `None` for a tick: it leaves the state as it is.
    to: Option<&'a str>,
    guard: Option<&'a SExpression>,
    effects: &'a [SExpression],
}

fn steps(td: &TraitDefinition) -> Vec<Step<'_>> {
    let mut out = Vec::new();
    for t in &td.state_machine.transitions {
        out.push(Step {
            site: format!("{}: {} --{}--> {}", td.name, t.from, t.event, t.to),
            event: Some(&t.event),
            from: t.from.states(),
            to: Some(&t.to),
            guard: t.guard.as_ref(),
            effects: t.effects.as_deref().unwrap_or(&[]),
        });
    }
    for tick in td.ticks.iter().flatten() {
        let from = match &tick.applies_to {
            Some(states) if !states.is_empty() => states.iter().map(String::as_str).collect(),
            _ => td
                .state_machine
                .states
                .iter()
                .map(|s| s.name.as_str())
                .collect(),
        };
        out.push(Step {
            site: format!("{}: tick {}", td.name, tick.name),
            event: None,
            from,
            to: None,
            guard: tick.guard.as_ref(),
            effects: &tick.effects,
        });
    }
    out
}

impl<'a> Program<'a> {
    fn new(schema: &'a OrbitalSchema) -> Self {
        let mut traits = Vec::new();
        let mut entities = Vec::new();
        for orbital in schema.orbitals.iter().filter(|o| !o.is_spec()) {
            for t in &orbital.traits {
                if let TraitReference::Inline(td) = t {
                    traits.push(td);
                }
            }
            for e in
                std::iter::once(&orbital.entity).chain(orbital.auxiliary_entities.iter().flatten())
            {
                if let EntityRef::Inline(def) = e {
                    entities.push(def);
                }
            }
        }
        Program {
            schema,
            traits,
            entities,
        }
    }

    fn entity(&self, name: &str) -> Option<&'a EntityDefinition> {
        self.entities.iter().copied().find(|e| e.name == name)
    }

    fn payload_fields<'t>(
        &self,
        td: &'t TraitDefinition,
        event: Option<&str>,
    ) -> &'t [PayloadField] {
        let Some(event) = event else { return &[] };
        td.state_machine
            .events
            .iter()
            .flatten()
            .find(|e| e.key == event)
            .and_then(|e| e.payload_schema.as_deref())
            .unwrap_or(&[])
    }

    /// Sorts of everything an obligation over `td` handling `event` can read.
    fn base_vars(&self, td: &TraitDefinition, event: Option<&str>) -> Vars {
        let mut vars = Vars::new();
        if let Some(def) = td.linked_entity.as_deref().and_then(|e| self.entity(e)) {
            add_entity_vars(&mut vars, "@entity", def, true);
        }
        for f in self.payload_fields(td, event) {
            let name = format!("@payload.{}", f.name);
            let nullable = f.required != Some(true) || f.nullable == Some(true);
            match payload_sort(f) {
                Some(sort) => {
                    vars.insert(name, VarInfo { sort, nullable });
                }
                None => {
                    let record = f
                        .entity
                        .as_deref()
                        .or(Some(f.field_type.as_str()))
                        .and_then(|e| self.entity(e));
                    if let Some(def) = record {
                        add_entity_vars(&mut vars, &name, def, true);
                    }
                }
            }
        }
        if let Some(user) = identity_entity_def(self.schema) {
            add_entity_vars(&mut vars, "@user", user, false);
        }
        vars
    }

    fn row_obligations(
        &self,
        prop: &Proposition,
        entity: &str,
        inv: &SExpression,
        invariants: &[SExpression],
        out: &mut Vec<Obligation>,
    ) {
        let Some(def) = self.entity(entity) else {
            return;
        };
        let intrinsic: BTreeSet<&str> = def
            .fields
            .iter()
            .filter(|f| f.intrinsic == Some(true))
            .filter_map(|f| f.name.as_deref())
            .collect();
        for td in &self.traits {
            let set_fields = fields_set_anywhere(td);
            for step in steps(td) {
                if !step.effects.iter().any(has_persist) {
                    continue;
                }
                let paths = match run(step.effects, td, step.event, self) {
                    Ok(paths) => paths,
                    Err(reason) => {
                        out.push(unbuildable(prop, &step.site, reason));
                        continue;
                    }
                };
                for (pi, path) in paths.iter().enumerate() {
                    for w in path.writes.iter().filter(|w| w.entity == UNKNOWN_TARGET) {
                        let site = format!("{}, {}{}", step.site, w.at, path_label(&paths, pi));
                        out.push(unbuildable(
                            prop,
                            &site,
                            "a write whose target entity is not statically known".to_string(),
                        ));
                    }
                    for w in path.writes.iter().filter(|w| w.entity == entity) {
                        let site = format!("{}, {}{}", step.site, w.at, path_label(&paths, pi));
                        let linked = td.linked_entity.as_deref() == Some(entity)
                            && matches!(&w.id, Some(SExpression::String(s)) if s == "@entity.id");
                        let to_row = |e: &SExpression| -> SExpression {
                            if linked {
                                subst_fields(e, "@entity", &|f| {
                                    (!set_fields.contains(f)).then(|| bind(ROW, f))
                                })
                            } else {
                                e.clone()
                            }
                        };
                        let mut vars = self.base_vars(td, step.event);
                        add_entity_vars(&mut vars, ROW, def, false);
                        let mut notes = path.notes.clone();
                        let mut base: Vec<SExpression> = step
                            .guard
                            .into_iter()
                            .cloned()
                            .chain(path.conds.iter().cloned())
                            .map(|e| to_row(&e))
                            .collect();
                        if let Data::Whole(b) | Data::Merge(b, _) = &w.data {
                            self.add_record_vars(&mut vars, b, def);
                        }
                        let new_field = |f: &str, old: Option<SExpression>| -> SExpression {
                            if intrinsic.contains(f) {
                                return SExpression::Null;
                            }
                            match &w.data {
                                Data::Fields(m) => match m.get(f) {
                                    Some(v) => to_row(v),
                                    None => old.unwrap_or_else(|| default_of(def, f)),
                                },
                                Data::Whole(SExpression::String(b)) if b.starts_with('@') => {
                                    to_row(&SExpression::String(format!("{b}.{f}")))
                                }
                                Data::Whole(other) => {
                                    unresolved(&format!("{f} of {}", canonical(other)))
                                }
                                Data::Merge(b, over) => match (over.get(f), b) {
                                    (Some(v), _) => to_row(v),
                                    (None, SExpression::String(b)) if b.starts_with('@') => {
                                        to_row(&SExpression::String(format!("{b}.{f}")))
                                    }
                                    (None, other) => {
                                        unresolved(&format!("{f} of {}", canonical(other)))
                                    }
                                },
                            }
                        };
                        let fields: Vec<&str> = def
                            .fields
                            .iter()
                            .filter_map(|f| f.name.as_deref())
                            .collect();
                        if matches!(w.op, WriteOp::Create | WriteOp::Upsert) {
                            let created: BTreeMap<&str, SExpression> =
                                fields.iter().map(|f| (*f, new_field(f, None))).collect();
                            let whole = match &w.data {
                                Data::Fields(written) => {
                                    let mut record: orbital_core_base::schema::SObject = written
                                        .iter()
                                        .filter(|(name, _)| !intrinsic.contains(name.as_str()))
                                        .map(|(name, value)| (name.clone(), to_row(value)))
                                        .collect();
                                    for field in &def.fields {
                                        if let (Some(name), Some(default)) =
                                            (&field.name, &field.default)
                                        {
                                            if !intrinsic.contains(name.as_str()) {
                                                record
                                                    .entry(name.clone())
                                                    .or_insert_with(|| default.clone());
                                            }
                                        }
                                    }
                                    SExpression::Object(record)
                                }
                                _ => unresolved("created whole record"),
                            };
                            out.push(configured(
                                td,
                                Obligation {
                                    proposition: prop.clone(),
                                    site: format!("{site} (create)"),
                                    assumptions: base.clone(),
                                    goal: subst_record(
                                        inv,
                                        "@entity",
                                        &|f| {
                                            Some(
                                                created
                                                    .get(f)
                                                    .cloned()
                                                    .unwrap_or(SExpression::Null),
                                            )
                                        },
                                        &whole,
                                    ),
                                    vars: vars.clone(),
                                    inexact: join(&notes),
                                    unbuildable: None,
                                },
                            ));
                        }
                        if matches!(w.op, WriteOp::Update | WriteOp::Upsert) {
                            if matches!(w.data, Data::Whole(_) | Data::Merge(..)) {
                                notes.push(
                                    "a whole-record update merges only the keys its value carries"
                                        .to_string(),
                                );
                            }
                            let old = |f: &str| -> SExpression {
                                if intrinsic.contains(f) {
                                    SExpression::Null
                                } else {
                                    bind(ROW, f)
                                }
                            };
                            for i in invariants {
                                base.push(subst_fields(i, "@entity", &|f| Some(old(f))));
                            }
                            let updated: BTreeMap<&str, SExpression> = fields
                                .iter()
                                .map(|f| (*f, new_field(f, Some(old(f)))))
                                .collect();
                            out.push(configured(
                                td,
                                Obligation {
                                    proposition: prop.clone(),
                                    site: format!("{site} (update)"),
                                    assumptions: base,
                                    goal: subst_fields(inv, "@entity", &|f| {
                                        Some(updated.get(f).cloned().unwrap_or_else(|| old(f)))
                                    }),
                                    vars,
                                    inexact: join(&notes),
                                    unbuildable: None,
                                },
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Sorts for the fields of a whole record a write takes from a binding (`@payload.order`).
    fn add_record_vars(&self, vars: &mut Vars, record: &SExpression, def: &EntityDefinition) {
        if let SExpression::String(b) = record {
            if b.starts_with("@payload.") && !b[9..].contains('.') {
                add_entity_vars(vars, b, def, true);
            }
        }
    }

    fn state_obligations(
        &self,
        prop: &Proposition,
        trait_name: &str,
        state: &str,
        holds: &SExpression,
        known: &Known,
        out: &mut Vec<Obligation>,
    ) {
        let (row_invariants, state_holds) = (known.rows, known.states);
        for td in self.traits.iter().filter(|t| t.name == trait_name) {
            let linked = td.linked_entity.as_deref().and_then(|e| self.entity(e));
            let set_fields = fields_set_anywhere(td);
            for step in steps(td) {
                if step.to.is_some_and(|to| to != state)
                    || (step.to.is_none() && !step.from.contains(&state))
                {
                    continue;
                }
                let paths = match run(step.effects, td, step.event, self) {
                    Ok(paths) => paths,
                    Err(reason) => {
                        out.push(unbuildable(prop, &step.site, reason));
                        continue;
                    }
                };
                // A tick, or a transition out of the state itself, starts where the proposition already holds. A
                // transition out of another state starts where that state's own proposition holds (each is proved on
                // entry, so assuming it is induction), or anywhere when it declares none.
                let mut starts: Vec<(bool, Vec<SExpression>)> = Vec::new();
                if step.to.is_none() || step.from.contains(&state) {
                    starts.push((true, vec![holds.clone()]));
                }
                if step.to.is_some() {
                    let mut anywhere = false;
                    for f in step.from.iter().filter(|f| **f != state) {
                        let known = state_holds(trait_name, f);
                        if known.is_empty() {
                            anywhere = true;
                        } else if !starts.iter().any(|(_, a)| *a == known) {
                            starts.push((false, known));
                        }
                    }
                    if anywhere {
                        starts.push((false, Vec::new()));
                    }
                }
                for (pi, path) in paths.iter().enumerate() {
                    let mut notes = path.notes.clone();
                    let after = frame_after(path, td, linked, &set_fields, &mut notes);
                    let mut vars = self.base_vars(td, step.event);
                    if let Some(def) = linked {
                        add_entity_vars(&mut vars, FETCHED, def, true);
                    }
                    let goal = subst_fields(holds, "@entity", &|f| after.get(f).cloned());
                    for (inside, held) in &starts {
                        let inside = *inside;
                        let mut assumptions: Vec<SExpression> = step
                            .guard
                            .into_iter()
                            .cloned()
                            .chain(path.conds.iter().cloned())
                            .collect();
                        assumptions.extend(held.iter().cloned());
                        let label = if starts.len() > 1 {
                            if inside {
                                " (from the state)"
                            } else {
                                " (from another state)"
                            }
                        } else {
                            ""
                        };
                        out.push(configured(
                            td,
                            Obligation {
                                proposition: prop.clone(),
                                site: format!("{}{}{}", step.site, path_label(&paths, pi), label),
                                assumptions,
                                goal: goal.clone(),
                                vars: vars.clone(),
                                inexact: join(&notes),
                                unbuildable: None,
                            },
                        ));
                    }
                }
            }
            if td
                .state_machine
                .states
                .iter()
                .any(|s| s.name == state && s.is_initial == Some(true))
            {
                self.initial_obligations(prop, td, linked, holds, row_invariants, out);
            }
            self.foreign_obligations(prop, td, linked, &set_fields, holds, out);
        }
    }

    /// `@ensures` of `from --event--> to`, owed on every effect path of each such transition, assuming its guard,
    /// the branch conditions and the `@holds` of `from`. The goal reads `@entity` after the effects and `@fromEntity`
    /// as the `@entity` before them.
    fn transition_obligations(
        &self,
        prop: &Proposition,
        (trait_name, from, event, to): (&str, &str, &str, &str),
        ensures: &SExpression,
        source_holds: &[SExpression],
        out: &mut Vec<Obligation>,
    ) {
        for td in self.traits.iter().filter(|t| t.name == trait_name) {
            let linked = td.linked_entity.as_deref().and_then(|e| self.entity(e));
            let set_fields = fields_set_anywhere(td);
            for step in steps(td) {
                if step.event != Some(event) || step.to != Some(to) || !step.from.contains(&from) {
                    continue;
                }
                let paths = match run(step.effects, td, step.event, self) {
                    Ok(paths) => paths,
                    Err(reason) => {
                        out.push(unbuildable(prop, &step.site, reason));
                        continue;
                    }
                };
                for (pi, path) in paths.iter().enumerate() {
                    let mut notes = path.notes.clone();
                    let after = frame_after(path, td, linked, &set_fields, &mut notes);
                    let mut vars = self.base_vars(td, step.event);
                    if let Some(def) = linked {
                        add_entity_vars(&mut vars, FETCHED, def, true);
                    }
                    let post = subst_fields(ensures, "@entity", &|f| after.get(f).cloned());
                    let goal = subst_fields(&post, FROM_ENTITY, &|f| Some(bind("@entity", f)));
                    let mut assumptions: Vec<SExpression> = step
                        .guard
                        .into_iter()
                        .cloned()
                        .chain(path.conds.iter().cloned())
                        .collect();
                    assumptions.extend(source_holds.iter().cloned());
                    out.push(configured(
                        td,
                        Obligation {
                            proposition: prop.clone(),
                            site: format!("{}{}", step.site, path_label(&paths, pi)),
                            assumptions,
                            goal,
                            vars,
                            inexact: join(&notes),
                            unbuildable: None,
                        },
                    ));
                }
            }
        }
    }

    fn initial_obligations(
        &self,
        prop: &Proposition,
        td: &TraitDefinition,
        linked: Option<&EntityDefinition>,
        holds: &SExpression,
        row_invariants: &dyn Fn(&str) -> Vec<SExpression>,
        out: &mut Vec<Obligation>,
    ) {
        let mut vars = self.base_vars(td, None);
        let defaults = |f: &str| Some(linked.map_or(SExpression::Null, |def| default_of(def, f)));
        out.push(configured(
            td,
            Obligation {
                proposition: prop.clone(),
                site: format!("{}: mounted with its entity's defaults", td.name),
                assumptions: Vec::new(),
                goal: subst_fields(holds, "@entity", &defaults),
                vars: vars.clone(),
                inexact: None,
                unbuildable: None,
            },
        ));
        let Some(def) = linked.filter(|d| d.persistence != Some(Persistence::Runtime)) else {
            return;
        };
        add_entity_vars(&mut vars, INIT, def, true);
        let row = |f: &str| Some(bind(INIT, f));
        out.push(configured(
            td,
            Obligation {
                proposition: prop.clone(),
                site: format!("{}: mounted on a stored {} row", td.name, def.name),
                assumptions: row_invariants(&def.name)
                    .iter()
                    .map(|i| subst_fields(i, "@entity", &row))
                    .collect(),
                goal: subst_fields(holds, "@entity", &row),
                vars,
                inexact: None,
                unbuildable: None,
            },
        ));
    }

    /// Another trait's write of the row `td` may be showing, or of a shared frame, can change `td`'s `@entity`
    /// while it sits in the state.
    fn foreign_obligations(
        &self,
        prop: &Proposition,
        td: &TraitDefinition,
        linked: Option<&EntityDefinition>,
        set_fields: &BTreeSet<String>,
        holds: &SExpression,
        out: &mut Vec<Obligation>,
    ) {
        let Some(def) = linked else { return };
        let holds = &config_read(td, holds);
        for other in self.traits.iter().filter(|u| u.name != td.name) {
            let shares_frame =
                def.shared && other.linked_entity.as_deref() == Some(def.name.as_str());
            for step in steps(other) {
                let Ok(paths) = run(step.effects, other, step.event, self) else {
                    out.push(unbuildable(
                        prop,
                        &step.site,
                        format!("more than {MAX_PATHS} effect paths"),
                    ));
                    continue;
                };
                for (pi, path) in paths.iter().enumerate() {
                    let rename = |e: &SExpression| {
                        config_read(
                            other,
                            &subst_fields(e, "@entity", &|f| Some(bind(OTHER, f))),
                        )
                    };
                    let mut changed: BTreeMap<String, SExpression> = BTreeMap::new();
                    if path.writes.iter().any(|w| w.entity == UNKNOWN_TARGET) {
                        for f in def.fields.iter().filter_map(|f| f.name.as_deref()) {
                            if !set_fields.contains(f) {
                                changed.insert(
                                    f.to_string(),
                                    unresolved(&format!("{f} after an unknown write")),
                                );
                            }
                        }
                    }
                    for w in path.writes.iter().filter(|w| {
                        w.entity == def.name && matches!(w.op, WriteOp::Update | WriteOp::Upsert)
                    }) {
                        match &w.data {
                            Data::Fields(m) => {
                                for (f, v) in m {
                                    if !set_fields.contains(f) {
                                        changed.insert(f.clone(), rename(v));
                                    }
                                }
                            }
                            Data::Whole(b) | Data::Merge(b, _) => {
                                for f in def.fields.iter().filter_map(|f| f.name.as_deref()) {
                                    if !set_fields.contains(f) {
                                        changed.insert(
                                            f.to_string(),
                                            unresolved(&format!("{f} of {}", canonical(b))),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    if shares_frame {
                        for (f, v) in &path.frame {
                            changed.insert(f.clone(), rename(v));
                        }
                    }
                    if changed.is_empty() {
                        continue;
                    }
                    let mut vars = self.base_vars(other, step.event);
                    // The two traits' `@entity` are different frames: `td`'s keeps its name, `other`'s is renamed.
                    vars.retain(|k, _| !k.starts_with("@entity."));
                    add_entity_vars(&mut vars, "@entity", def, true);
                    if let Some(o) = other.linked_entity.as_deref().and_then(|e| self.entity(e)) {
                        add_entity_vars(&mut vars, OTHER, o, true);
                    }
                    let mut assumptions: Vec<SExpression> = step
                        .guard
                        .into_iter()
                        .chain(path.conds.iter())
                        .map(rename)
                        .collect();
                    assumptions.push(holds.clone());
                    out.push(Obligation {
                        proposition: prop.clone(),
                        site: format!("{}{} (while {} is in the state)", step.site, path_label(&paths, pi), td.name),
                        assumptions,
                        goal: subst_fields(holds, "@entity", &|f| changed.get(f).cloned()),
                        vars,
                        inexact: Some(format!("{} may write while {} is in the state; which row each shows is not modeled", other.name, td.name)),
                        unbuildable: None,
                    });
                }
            }
        }
    }
}

/// The trait's `@entity` after a path's effects, as a field → value map over the pre-state (fields absent are unchanged).
fn frame_after(
    path: &Path,
    td: &TraitDefinition,
    linked: Option<&EntityDefinition>,
    set_fields: &BTreeSet<String>,
    notes: &mut Vec<String>,
) -> BTreeMap<String, SExpression> {
    let mut after = path.frame.clone();
    let Some(def) = linked else { return after };
    for w in path.writes.iter().filter(|w| w.entity == UNKNOWN_TARGET) {
        notes.push(format!(
            "{} may write its own row through a target that is not statically known ({})",
            td.name, w.at
        ));
        for f in def.fields.iter().filter_map(|f| f.name.as_deref()) {
            after
                .entry(f.to_string())
                .or_insert_with(|| unresolved(&format!("{f} after an unknown write")));
        }
    }
    for w in &path.writes {
        let own_row = w.entity == def.name
            && matches!(w.op, WriteOp::Update | WriteOp::Upsert)
            && matches!(&w.id, Some(SExpression::String(s)) if s == "@entity.id");
        if !own_row {
            continue;
        }
        match &w.data {
            Data::Fields(m) => {
                for (f, v) in m {
                    if after.contains_key(f) {
                        continue;
                    }
                    if set_fields.contains(f) {
                        notes.push(format!("{} sets `{f}` elsewhere, so its frame may shadow the row this transition writes", td.name));
                        after.insert(f.clone(), unresolved(&format!("frame or row {f}")));
                    } else {
                        after.insert(f.clone(), v.clone());
                    }
                }
            }
            Data::Whole(b) | Data::Merge(b, _) => {
                notes.push("a whole-record update of the trait's own row".to_string());
                for f in def.fields.iter().filter_map(|f| f.name.as_deref()) {
                    after
                        .entry(f.to_string())
                        .or_insert_with(|| unresolved(&format!("{f} of {}", canonical(b))));
                }
            }
        }
    }
    after
}

// ---------------------------------------------------------------------------------------------------------------
// Symbolic execution of an effect list
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum WriteOp {
    Create,
    Update,
    Upsert,
}

#[derive(Clone, Debug)]
enum Data {
    Fields(BTreeMap<String, SExpression>),
    Whole(SExpression),
    /// `(object/merge <record> { literal })`: the literal's keys, every other key read from the record.
    Merge(SExpression, BTreeMap<String, SExpression>),
}

#[derive(Clone, Debug)]
struct Write {
    op: WriteOp,
    entity: String,
    id: Option<SExpression>,
    data: Data,
    at: String,
}

#[derive(Clone, Debug, Default)]
struct Path {
    /// `@entity` fields written by `set`, as values over the pre-state.
    frame: BTreeMap<String, SExpression>,
    /// Branch conditions taken, over the pre-state.
    conds: Vec<SExpression>,
    writes: Vec<Write>,
    notes: Vec<String>,
}

impl Path {
    /// `expr` as read at this point: `@entity.<f>` already set reads the set value.
    fn read(&self, expr: &SExpression) -> SExpression {
        subst_fields(expr, "@entity", &|f| self.frame.get(f).cloned())
    }
}

fn run(
    effects: &[SExpression],
    td: &TraitDefinition,
    event: Option<&str>,
    program: &Program<'_>,
) -> Result<Vec<Path>, String> {
    let mut paths = vec![Path::default()];
    for (i, e) in effects.iter().enumerate() {
        let mut next = Vec::new();
        for p in paths {
            next.extend(exec(e, p, &format!("effect[{i}]"), td, event, program)?);
        }
        if next.len() > MAX_PATHS {
            return Err(format!("more than {MAX_PATHS} effect paths"));
        }
        paths = next;
    }
    Ok(paths)
}

fn exec(
    effect: &SExpression,
    mut path: Path,
    at: &str,
    td: &TraitDefinition,
    event: Option<&str>,
    program: &Program<'_>,
) -> Result<Vec<Path>, String> {
    let SExpression::Array(items) = effect else {
        return Ok(vec![path]);
    };
    let Some(SExpression::String(head)) = items.first() else {
        return Ok(vec![path]);
    };
    let seq = |body: &[SExpression], start: Path| -> Result<Vec<Path>, String> {
        let mut paths = vec![start];
        for (j, e) in body.iter().enumerate() {
            let mut next = Vec::new();
            for p in paths {
                next.extend(exec(e, p, &format!("{at}.{j}"), td, event, program)?);
            }
            if next.len() > MAX_PATHS {
                return Err(format!("more than {MAX_PATHS} effect paths"));
            }
            paths = next;
        }
        Ok(paths)
    };
    match head.as_str() {
        "do" => seq(&items[1..], path),
        "if" | "when" => {
            let Some(cond) = items.get(1) else {
                return Ok(vec![path]);
            };
            let cond = path.read(cond);
            let mut yes = path.clone();
            yes.conds.push(cond.clone());
            let mut no = path;
            no.conds.push(SExpression::Array(vec![
                SExpression::String("not".to_string()),
                cond,
            ]));
            let (then, otherwise): (&[SExpression], &[SExpression]) = if head == "if" {
                (
                    items.get(2).map(std::slice::from_ref).unwrap_or(&[]),
                    items.get(3).map(std::slice::from_ref).unwrap_or(&[]),
                )
            } else {
                (&items[2..], &[])
            };
            let mut out = seq(then, yes)?;
            out.extend(seq(otherwise, no)?);
            Ok(out)
        }
        "let" => {
            let Some(scope) = SequentialBindings::from_args(&items[1..]) else {
                return Ok(vec![path]);
            };
            let mut values: Vec<(String, SExpression)> = Vec::new();
            for (name, init) in &scope.bindings {
                let mut v = path.read(init);
                for (prev, pv) in &values {
                    v = super::witness::substitute_var(&v, prev, pv);
                }
                values.push((name.to_string(), v));
            }
            let body: Vec<SExpression> = items[2..]
                .iter()
                .map(|b| {
                    values.iter().rev().fold(b.clone(), |acc, (n, v)| {
                        super::witness::substitute_var(&acc, n, v)
                    })
                })
                .collect();
            seq(&body, path)
        }
        "set" => {
            match (items.get(1), items.get(2)) {
                (Some(SExpression::String(target)), Some(value)) => {
                    match target.strip_prefix("@entity.") {
                        Some(field) if !field.contains('.') => {
                            // A binding that reads nothing stores null, as every other read of it (G-ORBITAL-CORE-019).
                            path.frame.insert(field.to_string(), path.read(value));
                        }
                        _ => path
                            .notes
                            .push(format!("`set {target}` is not a top-level `@entity` field")),
                    }
                }
                _ => path
                    .notes
                    .push("a `set` without a literal target".to_string()),
            }
            Ok(vec![path])
        }
        "persist" => {
            let linked = td.linked_entity.as_deref().and_then(|e| program.entity(e));
            let args = strip_trailing_emit_options(&items[1..]);
            let unknown = |path: &mut Path, at: String| {
                path.writes.push(Write {
                    op: WriteOp::Upsert,
                    entity: UNKNOWN_TARGET.to_string(),
                    id: None,
                    data: Data::Fields(BTreeMap::new()),
                    at,
                });
            };
            match args.first().and_then(|a| a.as_str()) {
                Some("batch") => match args.get(1).and_then(batch_tuples) {
                    // Built at run time: what it writes is unknown, but which entities is known when every op is a
                    // template naming its entity; the others' invariants owe it nothing.
                    None => match args.get(1).and_then(batch_written_entities) {
                        Some(entities) => {
                            for entity in entities {
                                path.writes.push(Write {
                                    op: WriteOp::Upsert,
                                    entity: entity.to_string(),
                                    id: None,
                                    data: Data::Whole(unresolved(&format!(
                                        "a {entity} row a built batch writes"
                                    ))),
                                    at: format!("{at} (batch)"),
                                });
                            }
                        }
                        None => unknown(&mut path, format!("{at} (batch)")),
                    },
                    Some(tuples) => {
                        for t in tuples {
                            let (op, id, data) = match t.first().and_then(|a| a.as_str()) {
                                Some("create") => (WriteOp::Create, None, t.get(2)),
                                Some("upsert") => (WriteOp::Upsert, None, t.get(2)),
                                Some("update") => {
                                    (WriteOp::Update, t.get(2).map(|i| path.read(i)), t.get(3))
                                }
                                _ => continue,
                            };
                            let Some(entity) = t.get(1).and_then(|e| target_entity(td, e)) else {
                                unknown(&mut path, format!("{at} (batch)"));
                                continue;
                            };
                            let data = data
                                .map(|d| data_of(&mut path, d, linked))
                                .unwrap_or(Data::Fields(BTreeMap::new()));
                            path.writes.push(Write {
                                op,
                                entity,
                                id,
                                data,
                                at: format!("{at} (batch)"),
                            });
                        }
                    }
                },
                Some(action @ ("create" | "update" | "upsert")) => {
                    let Some(entity) = args.get(1).and_then(|e| target_entity(td, e)) else {
                        unknown(&mut path, at.to_string());
                        return Ok(vec![path]);
                    };
                    let (id, data) = match (action, args.len()) {
                        ("update", n) if n >= 4 => (Some(path.read(&args[2])), args.get(3)),
                        _ => (None, args.get(2)),
                    };
                    let data = data
                        .map(|d| data_of(&mut path, d, linked))
                        .unwrap_or(Data::Fields(BTreeMap::new()));
                    let id = id.or_else(|| match &data {
                        Data::Fields(m) => m.get("id").cloned(),
                        Data::Whole(SExpression::String(b)) if b.starts_with('@') => {
                            Some(SExpression::String(format!("{b}.id")))
                        }
                        Data::Merge(b, over) => over.get("id").cloned().or_else(|| match b {
                            SExpression::String(b) if b.starts_with('@') => {
                                Some(SExpression::String(format!("{b}.id")))
                            }
                            _ => None,
                        }),
                        Data::Whole(_) => None,
                    });
                    let op = match action {
                        "create" => WriteOp::Create,
                        "update" => WriteOp::Update,
                        _ => WriteOp::Upsert,
                    };
                    path.writes.push(Write {
                        op,
                        entity,
                        id,
                        data,
                        at: at.to_string(),
                    });
                }
                _ => {}
            }
            Ok(vec![path])
        }
        "fetch" => {
            if items.get(1).and_then(|e| e.as_str()) == td.linked_entity.as_deref() {
                if let Some(def) = td.linked_entity.as_deref().and_then(|e| program.entity(e)) {
                    path.notes.push(format!(
                        "a `fetch` of {} may rebind the trait's `@entity`",
                        def.name
                    ));
                    for f in def.fields.iter().filter_map(|f| f.name.as_deref()) {
                        path.frame.insert(f.to_string(), bind(FETCHED, f));
                    }
                }
            }
            Ok(vec![path])
        }
        _ => Ok(vec![path]),
    }
}

/// What a write stores. A literal record is its fields; `@entity` written whole is the linked entity's fields as the
/// frame holds them now; any other value is a whole record read field by field.
fn data_of(path: &mut Path, data: &SExpression, linked: Option<&EntityDefinition>) -> Data {
    match (data, linked) {
        (SExpression::Object(map), _) => {
            Data::Fields(map.iter().map(|(k, v)| (k.clone(), path.read(v))).collect())
        }
        (SExpression::String(b), Some(def)) if b == "@entity" => {
            path.notes
                .push("`@entity` written whole carries only the keys its frame holds".to_string());
            Data::Fields(
                def.fields
                    .iter()
                    .filter_map(|f| f.name.as_deref())
                    .map(|f| (f.to_string(), path.read(&bind("@entity", f))))
                    .collect(),
            )
        }
        (SExpression::Array(items), _)
            if items.len() == 3
                && matches!(&items[0], SExpression::String(h) if h == "object/merge")
                && matches!(&items[2], SExpression::Object(_)) =>
        {
            let SExpression::Object(over) = &items[2] else {
                unreachable!()
            };
            Data::Merge(
                path.read(&items[1]),
                over.iter()
                    .map(|(k, v)| (k.clone(), path.read(v)))
                    .collect(),
            )
        }
        (other, _) => Data::Whole(path.read(other)),
    }
}

/// Whether an effect (or one nested in its control flow) is a `persist`.
fn has_persist(effect: &SExpression) -> bool {
    let SExpression::Array(items) = effect else {
        return false;
    };
    matches!(items.first(), Some(SExpression::String(h)) if h == "persist")
        || items.iter().any(has_persist)
}

/// The entity a write names: a literal, or `@config.<knob>` read through the trait's resolved config (call-site
/// overrides are folded into it). `None` when it is not statically known.
fn target_entity(td: &TraitDefinition, expr: &SExpression) -> Option<String> {
    let SExpression::String(s) = expr else {
        return None;
    };
    if !s.starts_with('@') {
        return Some(s.clone());
    }
    let knob = s.strip_prefix("@config.")?;
    match td.config.as_ref()?.get(knob)?.default.as_ref()? {
        SExpression::String(v) if !v.starts_with('@') => Some(v.clone()),
        _ => None,
    }
}

/// The op tuples of a batch, or `None` when its ops are an expression (or a literal element is not an op tuple).
fn batch_tuples(ops: &SExpression) -> Option<Vec<&[SExpression]>> {
    match classify_ops(ops) {
        BatchOps::Literal(elements) => elements
            .iter()
            .map(|e| match e {
                SExpression::Array(items) if is_op_tuple(e) => Some(items.as_slice()),
                _ => None,
            })
            .collect(),
        BatchOps::Mapped(tuple) => Some(vec![tuple]),
        BatchOps::Expression => None,
    }
}

/// Fields a trait writes with `set` anywhere: its frame may hold them, shadowing the row.
fn fields_set_anywhere(td: &TraitDefinition) -> BTreeSet<String> {
    fn walk(e: &SExpression, out: &mut BTreeSet<String>) {
        if let SExpression::Array(items) = e {
            if let (Some(SExpression::String(h)), Some(SExpression::String(t))) =
                (items.first(), items.get(1))
            {
                if h == "set" {
                    if let Some(f) = t.strip_prefix("@entity.") {
                        out.insert(f.split('.').next().unwrap_or(f).to_string());
                    }
                }
            }
            items.iter().for_each(|i| walk(i, out));
        }
    }
    let mut out = BTreeSet::new();
    for t in &td.state_machine.transitions {
        t.effects.iter().flatten().for_each(|e| walk(e, &mut out));
    }
    for tick in td.ticks.iter().flatten() {
        tick.effects.iter().for_each(|e| walk(e, &mut out));
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------------------------

/// `expr` with each `@config.<knob>` read as the trait's resolved config holds it for the run (call-site overrides
/// are folded in): a scalar default is that constant, no default reads null, and a default bound at run time
/// (`@user.…`, a forward) or a structured one is unresolved. A knob the trait does not declare is left as written.
fn config_read(td: &TraitDefinition, expr: &SExpression) -> SExpression {
    let Some(config) = td.config.as_ref() else {
        return expr.clone();
    };
    subst_fields(expr, "@config", &|knob| {
        let field = config.get(knob)?;
        Some(match &field.default {
            None | Some(SExpression::Null) => SExpression::Null,
            Some(v @ (SExpression::Number(_) | SExpression::Bool(_))) => v.clone(),
            Some(SExpression::String(s)) if !s.starts_with('@') => SExpression::String(s.clone()),
            Some(other) => unresolved(&format!("@config.{knob} = {}", canonical(other))),
        })
    })
}

fn configured(td: &TraitDefinition, mut o: Obligation) -> Obligation {
    o.assumptions = o.assumptions.iter().map(|a| config_read(td, a)).collect();
    o.goal = config_read(td, &o.goal);
    o
}

fn bind(root: &str, field: &str) -> SExpression {
    SExpression::String(format!("{root}.{field}"))
}

pub fn unresolved(what: &str) -> SExpression {
    SExpression::Array(vec![
        SExpression::String(UNRESOLVED.to_string()),
        SExpression::String(what.to_string()),
    ])
}

fn canonical(e: &SExpression) -> String {
    serde_json::to_string(e).unwrap_or_default()
}

fn join(notes: &[String]) -> Option<String> {
    (!notes.is_empty()).then(|| notes.join("; "))
}

fn path_label(paths: &[Path], i: usize) -> String {
    if paths.len() > 1 {
        format!(" [branch {}]", i + 1)
    } else {
        String::new()
    }
}

fn unbuildable(prop: &Proposition, site: &str, reason: String) -> Obligation {
    Obligation {
        proposition: prop.clone(),
        site: site.to_string(),
        assumptions: Vec::new(),
        goal: SExpression::Bool(true),
        vars: Vars::new(),
        inexact: None,
        unbuildable: Some(reason),
    }
}

/// `expr` with each read `<root>.<field>[.rest]` replaced by `value(field)` when it gives one: a binding is
/// re-rooted (keeping `.rest`), any other value replaces a bare field read, and a nested read of a non-binding value
/// is unresolved. Exact whole-record replacement uses the same traversal separately.
pub fn subst_fields(
    expr: &SExpression,
    root: &str,
    value: &dyn Fn(&str) -> Option<SExpression>,
) -> SExpression {
    subst_record(expr, root, value, &SExpression::String(root.to_string()))
}

fn subst_record(
    expr: &SExpression,
    root: &str,
    value: &dyn Fn(&str) -> Option<SExpression>,
    whole: &SExpression,
) -> SExpression {
    match expr {
        SExpression::String(s) => {
            let Some(rest) = s.strip_prefix(root) else {
                return expr.clone();
            };
            if rest.is_empty() {
                return whole.clone();
            }
            let Some(path) = rest.strip_prefix('.') else {
                return expr.clone();
            };
            let (field, tail) = match path.split_once('.') {
                Some((f, t)) => (f, Some(t)),
                None => (path, None),
            };
            match (value(field), tail) {
                (None, _) => expr.clone(),
                (Some(v), None) => v,
                (Some(SExpression::String(b)), Some(t)) if b.starts_with('@') => {
                    SExpression::String(format!("{b}.{t}"))
                }
                (Some(_), Some(_)) => unresolved(s),
            }
        }
        SExpression::Array(items) => SExpression::Array(
            items
                .iter()
                .map(|i| subst_record(i, root, value, whole))
                .collect(),
        ),
        SExpression::Object(map) => SExpression::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), subst_record(v, root, value, whole)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn default_of(def: &EntityDefinition, field: &str) -> SExpression {
    def.fields
        .iter()
        .find(|f| f.name.as_deref() == Some(field))
        .and_then(|f| f.default.clone())
        .unwrap_or(SExpression::Null)
}

pub fn field_sort(f: &FieldDefinition) -> Option<Sort> {
    match f.field_type {
        FieldType::Number | FieldType::Int | FieldType::Money | FieldType::Instant => Some(Sort::Num),
        FieldType::Boolean => Some(Sort::Bool),
        FieldType::Enum => Some(Sort::Str(f.values.clone())),
        FieldType::String
        | FieldType::Date
        | FieldType::Datetime
        | FieldType::Timestamp
        | FieldType::Email
        | FieldType::Url
        | FieldType::Phone
        | FieldType::Uuid
        | FieldType::Image => Some(Sort::Str(f.values.clone())),
        _ => None,
    }
}

fn payload_sort(f: &PayloadField) -> Option<Sort> {
    match f.field_type.as_str() {
        "number" | "money" | "instant" => Some(Sort::Num),
        "boolean" => Some(Sort::Bool),
        "string" | "enum" | "date" | "datetime" | "timestamp" | "email" | "url" | "phone"
        | "uuid" | "image" => Some(Sort::Str(f.values.clone())),
        _ => None,
    }
}

/// `<root>.<field>` for every typed field of `def`; `id` is never null in a stored row.
fn add_entity_vars(vars: &mut Vars, root: &str, def: &EntityDefinition, all_nullable: bool) {
    for f in &def.fields {
        let (Some(name), Some(sort)) = (f.name.as_deref(), field_sort(f)) else {
            continue;
        };
        let nullable =
            all_nullable || f.nullable == Some(true) || (f.required != Some(true) && name != "id");
        vars.insert(format!("{root}.{name}"), VarInfo { sort, nullable });
    }
    if !all_nullable {
        vars.entry(format!("{root}.id")).or_insert(VarInfo {
            sort: Sort::Str(None),
            nullable: false,
        });
    }
}
