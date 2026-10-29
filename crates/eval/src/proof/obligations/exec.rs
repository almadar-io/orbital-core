//! Symbolic execution of an effect list.

use super::*;

// ---------------------------------------------------------------------------------------------------------------
// Symbolic execution of an effect list
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum WriteOp {
    Create,
    Update,
    Upsert,
}

#[derive(Clone, Debug)]
pub(super) enum Data {
    Fields(BTreeMap<String, SExpression>),
    Whole(SExpression),
    /// `(object/merge <record> { literal })`: the literal's keys, every other key read from the record.
    Merge(SExpression, BTreeMap<String, SExpression>),
}

#[derive(Clone, Debug)]
pub(super) struct Write {
    pub(super) op: WriteOp,
    pub(super) entity: String,
    pub(super) id: Option<SExpression>,
    pub(super) data: Data,
    pub(super) at: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Path {
    /// `@entity` fields written by `set`, as values over the pre-state.
    pub(super) frame: BTreeMap<String, SExpression>,
    /// Branch conditions taken, over the pre-state.
    pub(super) conds: Vec<SExpression>,
    pub(super) writes: Vec<Write>,
    pub(super) evaluated: Vec<SExpression>,
    pub(super) notes: Vec<String>,
}

impl Path {
    /// `expr` as read at this point: `@entity.<f>` already set reads the set value.
    pub(super) fn read(&self, expr: &SExpression) -> SExpression {
        subst_fields(expr, "@entity", &|f| self.frame.get(f).cloned())
    }
}

pub(super) fn run(
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

pub(super) fn exec(
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
                SExpression::string("not"),
                cond,
            ].into()));
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
            for (name, init, _) in &scope.bindings {
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
                            let value = path.read(value);
                            path.evaluated.push(value.clone());
                            path.frame.insert(field.to_string(), value);
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
                            Some(SExpression::string(format!("{b}.id")))
                        }
                        Data::Merge(b, over) => over.get("id").cloned().or_else(|| match b {
                            SExpression::String(b) if b.starts_with('@') => {
                                Some(SExpression::string(format!("{b}.id")))
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
pub(super) fn data_of(path: &mut Path, data: &SExpression, linked: Option<&EntityDefinition>) -> Data {
    match (data, linked) {
        (SExpression::Object(map), _) => {
            Data::Fields(map.iter().map(|(k, v)| (k.to_string(), path.read(v))).collect())
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
                    .map(|(k, v)| (k.to_string(), path.read(v)))
                    .collect(),
            )
        }
        (other, _) => Data::Whole(path.read(other)),
    }
}

/// Whether an effect (or one nested in its control flow) is a `persist`.
pub(super) fn has_persist(effect: &SExpression) -> bool {
    let SExpression::Array(items) = effect else {
        return false;
    };
    matches!(items.first(), Some(SExpression::String(h)) if h == "persist")
        || items.iter().any(has_persist)
}

/// The entity a write names: a literal, or `@config.<knob>` read through the trait's resolved config (call-site
/// overrides are folded into it). `None` when it is not statically known.
pub(super) fn target_entity(td: &TraitDefinition, expr: &SExpression) -> Option<String> {
    let SExpression::String(s) = expr else {
        return None;
    };
    if !s.starts_with('@') {
        return Some(s.clone().into_inner());
    }
    let knob = s.strip_prefix("@config.")?;
    match td.config.as_ref()?.get(knob)?.default.as_ref()? {
        SExpression::String(v) if !v.starts_with('@') => Some(v.clone().into_inner()),
        _ => None,
    }
}

/// The op tuples of a batch, or `None` when its ops are an expression (or a literal element is not an op tuple).
pub(super) fn batch_tuples(ops: &SExpression) -> Option<Vec<&[SExpression]>> {
    match classify_ops(ops) {
        BatchOps::Literal(elements) => elements.iter().map(op_tuple).collect(),
        BatchOps::Mapped(tuple) => Some(vec![tuple]),
        BatchOps::Expression => None,
    }
}

/// Fields a trait writes with `set` anywhere: its frame may hold them, shadowing the row.
pub(super) fn fields_set_anywhere(td: &TraitDefinition) -> BTreeSet<String> {
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
