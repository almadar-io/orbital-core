//! Satisfiability of a [`Formula`]: a case split over its `or`s, each branch checked by three theories —
//! linear real arithmetic (Fourier–Motzkin over exact rationals), string equality with closed domains
//! (union-find), and booleans / nullness / opaque atoms (plain consistency). Complete for the fragment: `Unsat`
//! is a proof, `Sat` carries a model, `Unknown` names the resource bound that stopped it.

use super::formula::{formula_vars, Atom, Formula, Lin, Rel, STerm, Sort, VarInfo, Vars};
use super::rat::Rat;
use std::collections::{BTreeMap, BTreeSet};

const MAX_BRANCHES: usize = 200_000;
const MAX_CONSTRAINTS: usize = 20_000;

#[derive(Clone, Debug, PartialEq)]
pub enum ModelValue {
    Null,
    Num(Rat),
    Str(String),
    Bool(bool),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub values: BTreeMap<String, ModelValue>,
    pub opaque: BTreeMap<String, bool>,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Unsat,
    Sat(Model),
    Unknown(String),
}

#[derive(Clone, Default)]
struct Branch {
    lins: Vec<(Lin, Rel)>,
    strs: Vec<(STerm, STerm, bool)>,
    bools: BTreeMap<String, bool>,
    nulls: BTreeMap<String, bool>,
    opaque: BTreeMap<String, bool>,
}

impl Branch {
    /// `false` when the literal contradicts the branch outright.
    fn add(&mut self, atom: &Atom, positive: bool) -> bool {
        let set = |map: &mut BTreeMap<String, bool>, key: &str| match map
            .insert(key.to_string(), positive)
        {
            Some(prev) => prev == positive,
            None => true,
        };
        match atom {
            Atom::Lin(lin, rel) => {
                if lin.terms.is_empty() {
                    return holds(lin.c, *rel);
                }
                self.lins.push((lin.clone(), *rel));
                true
            }
            Atom::StrEq(a, b) => {
                self.strs.push((a.clone(), b.clone(), positive));
                true
            }
            Atom::Bool(v) => set(&mut self.bools, v),
            Atom::Null(v) => set(&mut self.nulls, v),
            Atom::Opaque(k) => set(&mut self.opaque, k),
        }
    }
}

fn holds(c: Rat, rel: Rel) -> bool {
    match rel {
        Rel::Lt => c.is_negative(),
        Rel::Le => !c.is_positive(),
        Rel::Eq => c.is_zero(),
    }
}

/// Axioms tying each variable to its declaration: a nullable number reads as 0 when null (`to_number`), and a
/// variable that cannot be null is not.
fn axioms(f: &Formula, vars: &Vars) -> Formula {
    let mut used = BTreeSet::new();
    formula_vars(f, &mut used);
    let mut parts = Vec::new();
    for v in used {
        match vars.get(&v) {
            Some(VarInfo {
                nullable: false, ..
            }) => parts.push(Formula::Lit(Atom::Null(v.clone()), false)),
            Some(VarInfo {
                sort: Sort::Num,
                nullable: true,
            }) => parts.push(Formula::or(vec![
                Formula::Lit(Atom::Null(v.clone()), false),
                Formula::atom(Atom::Lin(Lin::var(&v), Rel::Eq)),
            ])),
            _ => {}
        }
    }
    Formula::and(parts)
}

pub fn decide(f: &Formula, vars: &Vars) -> Outcome {
    let whole = Formula::and(vec![f.clone(), axioms(f, vars)]);
    let mut budget = MAX_BRANCHES;
    let mut unknown: Option<String> = None;
    let found = search(
        vec![&whole],
        Branch::default(),
        vars,
        &mut budget,
        &mut unknown,
    );
    match (found, unknown) {
        (Some(mut model), _) => {
            // Any extension of a satisfying branch satisfies the formula: a variable the branch left free takes a
            // typed, present value, so the evaluator replays what the declarations allow.
            let mut used = BTreeSet::new();
            formula_vars(&whole, &mut used);
            for v in used {
                if model.values.contains_key(&v) {
                    continue;
                }
                let value = match vars.get(&v).map(|i| &i.sort) {
                    Some(Sort::Num) => ModelValue::Num(Rat::ZERO),
                    Some(Sort::Str(domain)) => ModelValue::Str(
                        domain
                            .as_ref()
                            .and_then(|d| d.first().cloned())
                            .unwrap_or_else(|| "s".to_string()),
                    ),
                    Some(Sort::Bool) => ModelValue::Bool(false),
                    None => continue,
                };
                model.values.insert(v, value);
            }
            Outcome::Sat(model)
        }
        (None, Some(reason)) => Outcome::Unknown(reason),
        (None, None) if budget == 0 => {
            Outcome::Unknown(format!("more than {MAX_BRANCHES} case splits"))
        }
        (None, None) => Outcome::Unsat,
    }
}

fn search(
    mut pending: Vec<&Formula>,
    mut branch: Branch,
    vars: &Vars,
    budget: &mut usize,
    unknown: &mut Option<String>,
) -> Option<Model> {
    while let Some(f) = pending.pop() {
        match f {
            Formula::Const(true) => {}
            Formula::Const(false) => return None,
            Formula::Lit(atom, positive) => {
                if !branch.add(atom, *positive) {
                    return None;
                }
            }
            Formula::And(parts) => pending.extend(parts.iter()),
            Formula::Or(parts) => {
                for part in parts {
                    if *budget == 0 {
                        return None;
                    }
                    *budget -= 1;
                    let mut next = pending.clone();
                    next.push(part);
                    if let Some(model) = search(next, branch.clone(), vars, budget, unknown) {
                        return Some(model);
                    }
                }
                return None;
            }
        }
    }
    match theory(&branch, vars) {
        Ok(model) => model,
        Err(reason) => {
            unknown.get_or_insert(reason);
            None
        }
    }
}

/// The model of a fully split branch, `Ok(None)` when its theories conflict, `Err` when a bound stopped the check.
fn theory(branch: &Branch, vars: &Vars) -> Result<Option<Model>, String> {
    let is_null = |v: &str| branch.nulls.get(v) == Some(&true);
    let mentions_null = |t: &STerm| matches!(t, STerm::Var(v) if is_null(v));
    let strs: Vec<_> = branch
        .strs
        .iter()
        .filter(|(a, b, _)| !mentions_null(a) && !mentions_null(b))
        .cloned()
        .collect();
    let Some(strings) = solve_strings(&strs, vars) else {
        return Ok(None);
    };
    let Some(numbers) = solve_linear(&branch.lins)? else {
        return Ok(None);
    };
    let mut used = BTreeSet::new();
    for (lin, _) in &branch.lins {
        used.extend(lin.terms.keys().cloned());
    }
    for (a, b, _) in &branch.strs {
        for t in [a, b] {
            if let STerm::Var(v) = t {
                used.insert(v.clone());
            }
        }
    }
    used.extend(branch.bools.keys().cloned());
    used.extend(branch.nulls.keys().cloned());
    let mut model = Model {
        opaque: branch.opaque.clone(),
        ..Model::default()
    };
    for v in used {
        let value = if is_null(&v) {
            ModelValue::Null
        } else {
            match vars.get(&v).map(|i| &i.sort) {
                Some(Sort::Num) => ModelValue::Num(numbers.get(&v).copied().unwrap_or(Rat::ZERO)),
                Some(Sort::Str(domain)) => match strings.get(&v) {
                    Some(s) => ModelValue::Str(s.clone()),
                    None => ModelValue::Str(
                        domain
                            .as_ref()
                            .and_then(|d| d.first().cloned())
                            .unwrap_or_else(|| "s".to_string()),
                    ),
                },
                Some(Sort::Bool) => {
                    ModelValue::Bool(branch.bools.get(&v).copied().unwrap_or(false))
                }
                None => continue,
            }
        };
        model.values.insert(v, value);
    }
    Ok(Some(model))
}

// ---------------------------------------------------------------------------------------------------------------
// Linear real arithmetic
// ---------------------------------------------------------------------------------------------------------------

const OVERFLOW: &str = "rational overflow";

/// One elimination step: the variable, and the constraints that bounded it (over variables eliminated later).
struct Stage {
    var: String,
    bounds: Vec<(Lin, bool)>,
}

fn substitute(lin: &Lin, var: &str, by: &Lin) -> Option<Lin> {
    let Some(a) = lin.terms.get(var).copied() else {
        return Some(lin.clone());
    };
    let mut rest = lin.clone();
    rest.terms.remove(var);
    rest.add(&by.scale(a)?)
}

fn eval_lin(lin: &Lin, values: &BTreeMap<String, Rat>) -> Option<Rat> {
    lin.terms.iter().try_fold(lin.c, |acc, (v, a)| {
        acc.checked_plus(a.checked_times(values.get(v).copied().unwrap_or(Rat::ZERO))?)
    })
}

/// `Ok(Some(values))` satisfying every constraint, `Ok(None)` when they conflict.
fn solve_linear(constraints: &[(Lin, Rel)]) -> Result<Option<BTreeMap<String, Rat>>, String> {
    let mut eqs: Vec<Lin> = Vec::new();
    let mut ineqs: Vec<(Lin, bool)> = Vec::new();
    for (lin, rel) in constraints {
        match rel {
            Rel::Eq => eqs.push(lin.clone()),
            Rel::Lt => ineqs.push((lin.clone(), true)),
            Rel::Le => ineqs.push((lin.clone(), false)),
        }
    }
    // Gaussian elimination of the equalities, recording `var = expr` for back-substitution.
    let mut solved: Vec<(String, Lin)> = Vec::new();
    while let Some(eq) = eqs.pop() {
        let Some((var, a)) = eq.terms.iter().next().map(|(v, a)| (v.clone(), *a)) else {
            if !eq.c.is_zero() {
                return Ok(None);
            }
            continue;
        };
        let mut rest = eq.clone();
        rest.terms.remove(&var);
        let by = rest
            .scale(
                Rat::ONE
                    .checked_negate()
                    .and_then(|m| m.checked_over(a))
                    .ok_or(OVERFLOW)?,
            )
            .ok_or(OVERFLOW)?;
        for e in eqs.iter_mut() {
            *e = substitute(e, &var, &by).ok_or(OVERFLOW)?;
        }
        for (i, _) in ineqs.iter_mut() {
            *i = substitute(i, &var, &by).ok_or(OVERFLOW)?;
        }
        solved.push((var, by));
    }
    // Fourier–Motzkin on the inequalities.
    let mut stages: Vec<Stage> = Vec::new();
    loop {
        let mut set: BTreeSet<(Lin, bool)> = BTreeSet::new();
        for (lin, strict) in ineqs.drain(..) {
            if lin.terms.is_empty() {
                if !holds(lin.c, if strict { Rel::Lt } else { Rel::Le }) {
                    return Ok(None);
                }
            } else {
                set.insert((lin, strict));
            }
        }
        ineqs = set.into_iter().collect();
        let mut candidates: BTreeMap<&String, (usize, usize)> = BTreeMap::new();
        for (lin, _) in &ineqs {
            for (v, a) in &lin.terms {
                let e = candidates.entry(v).or_default();
                if a.is_positive() {
                    e.0 += 1
                } else {
                    e.1 += 1
                }
            }
        }
        let Some(var) = candidates
            .iter()
            .min_by_key(|(_, (p, n))| p * n)
            .map(|(v, _)| (*v).clone())
        else {
            break;
        };
        let (with, without): (Vec<_>, Vec<_>) = ineqs
            .into_iter()
            .partition(|(l, _)| l.terms.contains_key(&var));
        let (upper, lower): (Vec<_>, Vec<_>) = with
            .iter()
            .cloned()
            .partition(|(l, _)| l.terms[&var].is_positive());
        let mut next = without;
        for (u, us) in &upper {
            for (l, ls) in &lower {
                let au = u.terms[&var];
                let al = l.terms[&var].checked_negate().ok_or(OVERFLOW)?;
                let combined = u
                    .scale(al)
                    .and_then(|x| l.scale(au).and_then(|y| x.add(&y)))
                    .ok_or(OVERFLOW)?;
                next.push((combined, *us || *ls));
            }
        }
        if next.len() > MAX_CONSTRAINTS {
            return Err(format!("more than {MAX_CONSTRAINTS} linear constraints"));
        }
        stages.push(Stage { var, bounds: with });
        ineqs = next;
    }
    // Back-substitution: each eliminated variable takes a value inside the bounds its stage recorded.
    let mut values: BTreeMap<String, Rat> = BTreeMap::new();
    for stage in stages.iter().rev() {
        let mut lo: Option<(Rat, bool)> = None;
        let mut hi: Option<(Rat, bool)> = None;
        for (lin, strict) in &stage.bounds {
            let a = lin.terms[&stage.var];
            let mut rest = lin.clone();
            rest.terms.remove(&stage.var);
            let k = eval_lin(&rest, &values).ok_or(OVERFLOW)?;
            let bound = k
                .checked_negate()
                .and_then(|m| m.checked_over(a))
                .ok_or(OVERFLOW)?;
            if a.is_positive() {
                let tighter = match hi {
                    None => true,
                    Some((h, hs)) => bound < h || (bound == h && *strict && !hs),
                };
                if tighter {
                    hi = Some((bound, *strict));
                }
            } else {
                let tighter = match lo {
                    None => true,
                    Some((l, ls)) => bound > l || (bound == l && *strict && !ls),
                };
                if tighter {
                    lo = Some((bound, *strict));
                }
            }
        }
        let value = match (lo, hi) {
            (None, None) => Rat::ZERO,
            (Some((l, false)), _) => l,
            (None, Some((h, false))) => h,
            (Some((l, true)), None) => l.checked_plus(Rat::ONE).ok_or(OVERFLOW)?,
            (None, Some((h, true))) => h.checked_minus(Rat::ONE).ok_or(OVERFLOW)?,
            (Some((l, true)), Some((h, false))) => {
                if l.checked_plus(Rat::ONE).is_some_and(|x| x <= h) {
                    l.checked_plus(Rat::ONE).ok_or(OVERFLOW)?
                } else {
                    h
                }
            }
            (Some((l, true)), Some((h, true))) => l
                .checked_plus(h)
                .and_then(|s| s.checked_over(Rat::int(2)))
                .ok_or(OVERFLOW)?,
        };
        values.insert(stage.var.clone(), value);
    }
    for (var, by) in solved.iter().rev() {
        let v = eval_lin(by, &values).ok_or(OVERFLOW)?;
        values.insert(var.clone(), v);
    }
    for (lin, rel) in constraints {
        let v = eval_lin(lin, &values).ok_or(OVERFLOW)?;
        if !holds(v, *rel) {
            return Err("linear model check failed".to_string());
        }
    }
    Ok(Some(values))
}

// ---------------------------------------------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------------------------------------------

fn solve_strings(lits: &[(STerm, STerm, bool)], vars: &Vars) -> Option<BTreeMap<String, String>> {
    let mut index: BTreeMap<STerm, usize> = BTreeMap::new();
    let mut nodes: Vec<STerm> = Vec::new();
    let mut id = |t: &STerm, nodes: &mut Vec<STerm>| -> usize {
        *index.entry(t.clone()).or_insert_with(|| {
            nodes.push(t.clone());
            nodes.len() - 1
        })
    };
    let pairs: Vec<(usize, usize, bool)> = lits
        .iter()
        .map(|(a, b, p)| (id(a, &mut nodes), id(b, &mut nodes), *p))
        .collect();
    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while parent[r] != r {
            r = parent[r];
        }
        let mut c = x;
        while parent[c] != r {
            let n = parent[c];
            parent[c] = r;
            c = n;
        }
        r
    }
    for &(a, b, p) in &pairs {
        if p {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            parent[ra] = rb;
        }
    }
    let mut classes: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..nodes.len() {
        let r = find(&mut parent, i);
        classes.entry(r).or_default().push(i);
    }
    let mut fixed: BTreeMap<usize, String> = BTreeMap::new();
    let mut domains: BTreeMap<usize, Option<BTreeSet<String>>> = BTreeMap::new();
    let mut every_literal: BTreeSet<String> = BTreeSet::new();
    for (root, members) in &classes {
        let mut domain: Option<BTreeSet<String>> = None;
        for m in members {
            match &nodes[*m] {
                STerm::Lit(s) => {
                    every_literal.insert(s.clone());
                    if fixed
                        .insert(*root, s.clone())
                        .is_some_and(|prev| prev != *s)
                    {
                        return None;
                    }
                }
                STerm::Var(v) => {
                    if let Some(VarInfo {
                        sort: Sort::Str(Some(d)),
                        ..
                    }) = vars.get(v)
                    {
                        every_literal.extend(d.iter().cloned());
                        let d: BTreeSet<String> = d.iter().cloned().collect();
                        domain = Some(match domain {
                            None => d,
                            Some(prev) => prev.intersection(&d).cloned().collect(),
                        });
                    }
                }
            }
        }
        if let (Some(value), Some(d)) = (fixed.get(root), &domain) {
            if !d.contains(value) {
                return None;
            }
        }
        domains.insert(*root, domain);
    }
    let mut apart: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for &(a, b, p) in &pairs {
        if !p {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra == rb {
                return None;
            }
            apart.entry(ra).or_default().insert(rb);
            apart.entry(rb).or_default().insert(ra);
        }
    }
    let mut fresh = 0usize;
    let mut fresh_value = || loop {
        let candidate = format!("s{fresh}");
        fresh += 1;
        if !every_literal.contains(&candidate) {
            return candidate;
        }
    };
    let mut assigned = fixed.clone();
    let open: Vec<usize> = classes
        .keys()
        .copied()
        .filter(|r| !fixed.contains_key(r))
        .collect();
    for r in &open {
        if domains.get(r).is_some_and(|d| d.is_none()) {
            assigned.insert(*r, fresh_value());
        }
    }
    let finite: Vec<usize> = open
        .iter()
        .copied()
        .filter(|r| !assigned.contains_key(r))
        .collect();
    fn assign(
        i: usize,
        finite: &[usize],
        domains: &BTreeMap<usize, Option<BTreeSet<String>>>,
        apart: &BTreeMap<usize, BTreeSet<usize>>,
        assigned: &mut BTreeMap<usize, String>,
    ) -> bool {
        let Some(&r) = finite.get(i) else { return true };
        let options = domains.get(&r).cloned().flatten().unwrap_or_default();
        for option in options {
            let clash = apart
                .get(&r)
                .is_some_and(|others| others.iter().any(|o| assigned.get(o) == Some(&option)));
            if clash {
                continue;
            }
            assigned.insert(r, option);
            if assign(i + 1, finite, domains, apart, assigned) {
                return true;
            }
            assigned.remove(&r);
        }
        false
    }
    if !assign(0, &finite, &domains, &apart, &mut assigned) {
        return None;
    }
    let mut out = BTreeMap::new();
    for (root, members) in &classes {
        for m in members {
            if let STerm::Var(v) = &nodes[*m] {
                out.insert(v.clone(), assigned[root].clone());
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::super::formula::{Translator, VarInfo};
    use super::*;
    use orbital_core_base::schema::sexpr::SExpression;

    fn vars(list: &[(&str, Sort, bool)]) -> Vars {
        list.iter()
            .map(|(n, s, nullable)| {
                (
                    n.to_string(),
                    VarInfo {
                        sort: s.clone(),
                        nullable: *nullable,
                    },
                )
            })
            .collect()
    }

    fn parse(text: &str) -> SExpression {
        serde_json::from_str(text).unwrap()
    }

    fn sat(expr: &str, v: &Vars) -> Outcome {
        let mut t = Translator::new(v);
        let f = t.truthy(&parse(expr)).unwrap();
        decide(&f, v)
    }

    /// `assumptions ⟹ goal` is valid: its negation is unsatisfiable.
    fn valid(assume: &str, goal: &str, v: &Vars) -> Outcome {
        let mut t = Translator::new(v);
        let a = t.truthy(&parse(assume)).unwrap();
        let g = t.truthy(&parse(goal)).unwrap();
        decide(&Formula::and(vec![a, g.negate().unwrap()]), v)
    }

    fn num(names: &[&str]) -> Vars {
        vars(
            &names
                .iter()
                .map(|n| (*n, Sort::Num, false))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn empty_and_or_follow_the_evaluator() {
        let v = num(&[]);
        assert!(matches!(sat(r#"["and"]"#, &v), Outcome::Sat(_)));
        assert_eq!(sat(r#"["or"]"#, &v), Outcome::Unsat);
    }

    #[test]
    fn transitivity_is_proved_and_its_control_is_refuted() {
        let v = num(&["@a", "@b", "@c"]);
        assert_eq!(
            valid(
                r#"["and", ["<=", "@a", "@b"], ["<=", "@b", "@c"]]"#,
                r#"["<=", "@a", "@c"]"#,
                &v
            ),
            Outcome::Unsat
        );
        assert!(matches!(
            valid(r#"["<=", "@a", "@b"]"#, r#"["<=", "@a", "@c"]"#, &v),
            Outcome::Sat(_)
        ));
    }

    #[test]
    fn strict_bounds_distinguish_lt_from_le() {
        let v = num(&["@x"]);
        assert_eq!(
            sat(r#"["and", ["<", "@x", 1], [">", "@x", 1]]"#, &v),
            Outcome::Unsat
        );
        assert_eq!(
            sat(r#"["and", ["<", "@x", 1], [">=", "@x", 1]]"#, &v),
            Outcome::Unsat
        );
        assert!(matches!(
            sat(r#"["and", ["<=", "@x", 1], [">=", "@x", 1]]"#, &v),
            Outcome::Sat(_)
        ));
        let Outcome::Sat(m) = sat(r#"["and", ["<", "@x", 2], [">", "@x", 1]]"#, &v) else {
            panic!()
        };
        let ModelValue::Num(x) = m.values["@x"] else {
            panic!()
        };
        assert!(x > Rat::int(1) && x < Rat::int(2));
    }

    #[test]
    fn three_variable_linear_update_is_proved() {
        let v = num(&["@paid", "@total", "@amount"]);
        assert_eq!(
            valid(
                r#"["and", ["<=", "@paid", "@total"], ["<=", ["+", "@paid", "@amount"], "@total"]]"#,
                r#"["<=", ["+", "@paid", "@amount"], "@total"]"#,
                &v
            ),
            Outcome::Unsat
        );
        let Outcome::Sat(m) = valid(
            r#"["<=", "@paid", "@total"]"#,
            r#"["<=", ["+", "@paid", "@amount"], "@total"]"#,
            &v,
        ) else {
            panic!()
        };
        let get = |k: &str| match m.values[k] {
            ModelValue::Num(r) => r,
            _ => panic!(),
        };
        assert!(get("@paid").checked_plus(get("@amount")).unwrap() > get("@total"));
    }

    #[test]
    fn disequality_splits_and_equality_cycles_close() {
        let v = num(&["@x", "@y"]);
        assert_eq!(
            sat(r#"["and", ["=", "@x", "@y"], ["!=", "@x", "@y"]]"#, &v),
            Outcome::Unsat
        );
        assert!(matches!(sat(r#"["!=", "@x", 3]"#, &v), Outcome::Sat(_)));
        assert_eq!(
            valid(
                r#"["and", ["=", "@x", ["+", "@y", 1]], ["=", "@y", 2]]"#,
                r#"["=", "@x", 3]"#,
                &v
            ),
            Outcome::Unsat
        );
    }

    #[test]
    fn enum_domains_are_exhaustive() {
        let status = Sort::Str(Some(vec!["open".into(), "paid".into(), "refunded".into()]));
        let v = vars(&[("@s", status, false)]);
        assert_eq!(
            sat(
                r#"["and", ["!=", "@s", "open"], ["!=", "@s", "paid"], ["!=", "@s", "refunded"]]"#,
                &v
            ),
            Outcome::Unsat
        );
        let Outcome::Sat(m) = sat(r#"["and", ["!=", "@s", "open"], ["!=", "@s", "paid"]]"#, &v)
        else {
            panic!()
        };
        assert_eq!(m.values["@s"], ModelValue::Str("refunded".into()));
        assert_eq!(sat(r#"["=", "@s", "shipped"]"#, &v), Outcome::Unsat);
    }

    #[test]
    fn string_chains_and_open_strings() {
        let v = vars(&[
            ("@a", Sort::Str(None), false),
            ("@b", Sort::Str(None), false),
            ("@c", Sort::Str(None), false),
        ]);
        assert_eq!(
            sat(
                r#"["and", ["=", "@a", "@b"], ["=", "@b", "@c"], ["!=", "@a", "@c"]]"#,
                &v
            ),
            Outcome::Unsat
        );
        assert_eq!(
            sat(r#"["and", ["=", "@a", "x"], ["=", "@a", "y"]]"#, &v),
            Outcome::Unsat
        );
        let Outcome::Sat(m) = sat(r#"["and", ["!=", "@a", "@b"], ["!=", "@a", ""]]"#, &v) else {
            panic!()
        };
        assert_ne!(m.values["@a"], m.values["@b"]);
        assert_ne!(m.values["@a"], ModelValue::Str(String::new()));
    }

    #[test]
    fn null_follows_loose_eq_and_numeric_coercion() {
        let v = vars(&[("@x", Sort::Num, true)]);
        // A null number orders as 0 but equals only null.
        assert_eq!(
            valid(r#"["=", "@x", null]"#, r#"[">=", "@x", 0]"#, &v),
            Outcome::Unsat
        );
        assert_eq!(
            sat(r#"["and", ["=", "@x", null], ["=", "@x", 0]]"#, &v),
            Outcome::Unsat
        );
        assert!(matches!(
            sat(
                r#"["and", ["<", "@x", 1], [">", "@x", -1], ["!=", "@x", 0]]"#,
                &v
            ),
            Outcome::Sat(_)
        ));
        // Control: a non-nullable number is never null.
        let strict = vars(&[("@x", Sort::Num, false)]);
        assert_eq!(sat(r#"["=", "@x", null]"#, &strict), Outcome::Unsat);
        // A nullable enum keeps its domain only when present.
        let status = Sort::Str(Some(vec!["open".into()]));
        let s = vars(&[("@s", status, true)]);
        assert!(matches!(
            sat(r#"["!=", "@s", "open"]"#, &s),
            Outcome::Sat(_)
        ));
    }

    #[test]
    fn booleans_and_truthiness() {
        let v = vars(&[
            ("@f", Sort::Bool, true),
            ("@n", Sort::Num, false),
            ("@s", Sort::Str(None), false),
        ]);
        assert_eq!(
            sat(r#"["and", "@f", ["=", "@f", false]]"#, &v),
            Outcome::Unsat
        );
        assert_eq!(sat(r#"["and", "@n", ["=", "@n", 0]]"#, &v), Outcome::Unsat);
        assert_eq!(sat(r#"["and", "@s", ["=", "@s", ""]]"#, &v), Outcome::Unsat);
        assert_eq!(
            sat(r#"["and", ["not", "@f"], ["=", "@f", true]]"#, &v),
            Outcome::Unsat
        );
        assert!(matches!(sat(r#"["not", "@f"]"#, &v), Outcome::Sat(_)));
    }

    #[test]
    fn if_and_let_are_unfolded() {
        let v = num(&["@x"]);
        assert_eq!(
            valid(
                r#"[">", "@x", 5]"#,
                r#"["if", [">", "@x", 0], true, false]"#,
                &v
            ),
            Outcome::Unsat
        );
        assert_eq!(
            valid(
                r#"["=", "@x", 4]"#,
                r#"["let", [["y", ["+", "@x", 1]]], ["=", "@y", 5]]"#,
                &v
            ),
            Outcome::Unsat
        );
    }

    #[test]
    fn an_if_operand_is_split_per_branch() {
        let v = num(&["@x"]);
        assert_eq!(
            valid(
                r#"true"#,
                r#"[">=", ["if", [">", "@x", 0], "@x", 0], 0]"#,
                &v
            ),
            Outcome::Unsat
        );
        assert_eq!(
            valid(
                r#"true"#,
                r#"["<=", 0, ["if", [">", "@x", 0], "@x", 0]]"#,
                &v
            ),
            Outcome::Unsat
        );
        // Control: without the clamp the operand can be negative; a missing else is null, which equals no number.
        assert!(matches!(
            valid(
                r#"true"#,
                r#"[">=", ["if", [">", "@x", -5], "@x", 0], 0]"#,
                &v
            ),
            Outcome::Sat(_)
        ));
        assert!(matches!(
            valid(r#"true"#, r#"["=", ["if", [">", "@x", 0], 1], 1]"#, &v),
            Outcome::Sat(_)
        ));
    }

    #[test]
    fn opaque_atoms_are_free_and_named() {
        let v = vars(&[("@s", Sort::Str(None), false)]);
        let mut t = Translator::new(&v);
        let f = t.truthy(&parse(r#"[">", ["str/len", "@s"], 3]"#)).unwrap();
        assert_eq!(t.opaque.len(), 1);
        assert!(matches!(decide(&f, &v), Outcome::Sat(_)));
        // The same opaque expression is one atom: it cannot be both true and false.
        let both =
            parse(r#"["and", [">", ["str/len", "@s"], 3], ["not", [">", ["str/len", "@s"], 3]]]"#);
        let f = Translator::new(&v).truthy(&both).unwrap();
        assert_eq!(decide(&f, &v), Outcome::Unsat);
    }
}
