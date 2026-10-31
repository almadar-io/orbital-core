//! The program as the prover reads it: steps, frames and what each step knows.

use super::*;

// ---------------------------------------------------------------------------------------------------------------
// The program as the prover reads it
// ---------------------------------------------------------------------------------------------------------------

/// The propositions the rest of the program already owes, for induction.
pub(super) struct Known<'a> {
    pub(super) rows: &'a dyn Fn(&str) -> Vec<SExpression>,
    pub(super) states: &'a dyn Fn(&str, &str) -> Vec<SExpression>,
}

pub(super) struct Program<'a> {
    pub(super) schema: &'a OrbitalSchema,
    pub(super) traits: Vec<&'a TraitDefinition>,
    pub(super) entities: Vec<&'a EntityDefinition>,
}

/// One transition or tick: what fires it, from where to where, and what it does.
pub(super) struct Step<'a> {
    pub(super) site: String,
    pub(super) event: Option<&'a str>,
    pub(super) from: Vec<&'a str>,
    /// `None` for a tick: it leaves the state as it is.
    pub(super) to: Option<&'a str>,
    pub(super) guard: Option<&'a SExpression>,
    pub(super) effects: &'a [SExpression],
    /// The arm's own `@ensures`; a tick has none.
    pub(super) ensures: &'a [SExpression],
}

pub(super) fn steps(td: &TraitDefinition) -> Vec<Step<'_>> {
    let mut out = Vec::new();
    for t in &td.state_machine.transitions {
        out.push(Step {
            site: format!("{}: {} --{}--> {}", td.name, t.from, t.event, t.to),
            event: Some(&t.event),
            from: t.from.states(),
            to: Some(&t.to),
            guard: t.guard.as_ref(),
            effects: t.effects.as_deref().unwrap_or(&[]),
            ensures: t.ensures.as_deref().unwrap_or(&[]),
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
            ensures: &[],
        });
    }
    out
}

impl<'a> Program<'a> {
    pub(super) fn new(schema: &'a OrbitalSchema) -> Self {
        let mut traits = Vec::new();
        let mut entities = Vec::new();
        for orbital in schema.orbitals.iter().filter(|o| !o.is_spec()) {
            for t in &orbital.traits {
                if let TraitReference::Inline(td) = t {
                    traits.push(&**td);
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

    pub(super) fn entity(&self, name: &str) -> Option<&'a EntityDefinition> {
        self.entities.iter().copied().find(|e| e.name == name)
    }

    pub(super) fn payload_fields<'t>(
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
    pub(super) fn base_vars(&self, td: &TraitDefinition, event: Option<&str>) -> Vars {
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

    pub(super) fn row_obligations(
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
                if !WriteReach::of(step.effects, td).may_write(entity) {
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
                    let dynamic: Vec<Write> = path
                        .writes
                        .iter()
                        .filter(|w| w.entity == UNKNOWN_TARGET)
                        .map(|w| Write {
                            op: WriteOp::Create,
                            entity: entity.to_string(),
                            id: None,
                            data: Data::Whole(SExpression::string(WRITTEN)),
                            at: format!(
                                "{} (a {entity} row through a target chosen at run time)",
                                w.at
                            ),
                        })
                        .collect();
                    for w in path
                        .writes
                        .iter()
                        .filter(|w| w.entity == entity)
                        .chain(&dynamic)
                    {
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
                                    to_row(&SExpression::string(format!("{b}.{f}")))
                                }
                                Data::Whole(other) => {
                                    unresolved(&format!("{f} of {}", canonical(other)))
                                }
                                Data::Merge(b, over) => match (over.get(f), b) {
                                    (Some(v), _) => to_row(v),
                                    (None, SExpression::String(b)) if b.starts_with('@') => {
                                        to_row(&SExpression::string(format!("{b}.{f}")))
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
                                        .map(|(name, value)| (name.as_str().into(), to_row(value)))
                                        .collect();
                                    for field in &def.fields {
                                        if let (Some(name), Some(default)) =
                                            (&field.name, &field.default)
                                        {
                                            if !intrinsic.contains(name.as_str()) {
                                                record
                                                    .entry(name.as_str().into())
                                                    .or_insert_with(|| default.clone());
                                            }
                                        }
                                    }
                                    SExpression::object(record)
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
                                    evaluated: Vec::new(),
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
                                    evaluated: Vec::new(),
                                },
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Sorts for the fields of a whole record a write takes from a binding (`@payload.order`).
    pub(super) fn add_record_vars(
        &self,
        vars: &mut Vars,
        record: &SExpression,
        def: &EntityDefinition,
    ) {
        if let SExpression::String(b) = record {
            if b == WRITTEN || (b.starts_with("@payload.") && !b[9..].contains('.')) {
                add_entity_vars(vars, b, def, true);
            }
        }
    }

    pub(super) fn state_obligations(
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
                                evaluated: Vec::new(),
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
    pub(super) fn transition_obligations(
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
            let arms: Vec<Step<'_>> = steps(td)
                .into_iter()
                .filter(|step| {
                    step.event == Some(event) && step.to == Some(to) && step.from.contains(&from)
                })
                .collect();
            // An arm's own `@ensures` binds that arm; a spec transition, which no arm carries, binds every arm.
            let own = arms.iter().any(|step| step.ensures.contains(ensures));
            for step in arms {
                if own && !step.ensures.contains(ensures) {
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
                            evaluated: path.evaluated.iter().map(|v| config_read(td, v)).collect(),
                        },
                    ));
                }
            }
        }
    }

    pub(super) fn initial_obligations(
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
                evaluated: Vec::new(),
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
                evaluated: Vec::new(),
            },
        ));
    }

    /// Another trait's write of the row `td` may be showing, or of a shared frame, can change `td`'s `@entity`
    /// while it sits in the state.
    pub(super) fn foreign_obligations(
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
                let reach = WriteReach::of(step.effects, other);
                if !reach.may_write(&def.name) && !(shares_frame && reach.may_change_frame()) {
                    continue;
                }
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
                        evaluated: Vec::new(),
                    });
                }
            }
        }
    }
}

/// The trait's `@entity` after a path's effects, as a field → value map over the pre-state (fields absent are unchanged).
pub(super) fn frame_after(
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
