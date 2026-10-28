//! Guard S-expressions as formulas over typed variables, following the evaluator's strict semantics exactly. A
//! condition is a boolean, so an expression has two outcomes that can each fail to occur: it evaluates to `true`
//! ([`Translator::truthy`]), it evaluates to `false` ([`Translator::falsy`]), or it raises (neither formula holds).
//! `=` is structural (different kinds are unequal, a null equals only null) and never raises; ordering and arithmetic
//! raise on a null operand, so such an atom holds or fails only where every variable it reads is present. `and` / `or`
//! are boolean connectives that short-circuit, and `default` is the coalesce. Anything outside that fragment becomes
//! an opaque atom, named so an unproved obligation can say what it could not decide.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::rat::Rat;
use orbital_core_base::schema::sexpr::{SExpression, SequentialBindings};
use std::collections::{BTreeMap, BTreeSet};

/// What a variable holds: its declared kind, and whether it can be absent (null).
#[derive(Clone, Debug, PartialEq)]
pub enum Sort {
    Num,
    /// A string; `Some` lists the closed set of values a literal-union type allows.
    Str(Option<Vec<String>>),
    Bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VarInfo {
    pub sort: Sort,
    pub nullable: bool,
}

/// Variables by binding text (`@entity.total`).
pub type Vars = BTreeMap<String, VarInfo>;

/// `terms · vars + c`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Lin {
    pub terms: BTreeMap<String, Rat>,
    pub c: Rat,
}

impl Lin {
    pub fn constant(c: Rat) -> Lin {
        Lin {
            terms: BTreeMap::new(),
            c,
        }
    }

    pub fn var(name: &str) -> Lin {
        Lin {
            terms: BTreeMap::from([(name.to_string(), Rat::ONE)]),
            c: Rat::ZERO,
        }
    }

    pub fn scale(&self, k: Rat) -> Option<Lin> {
        let mut terms = BTreeMap::new();
        for (v, a) in &self.terms {
            let a = a.checked_times(k)?;
            if !a.is_zero() {
                terms.insert(v.clone(), a);
            }
        }
        Some(Lin {
            terms,
            c: self.c.checked_times(k)?,
        })
    }

    pub fn add(&self, o: &Lin) -> Option<Lin> {
        let mut terms = self.terms.clone();
        for (v, a) in &o.terms {
            let sum = terms
                .get(v)
                .copied()
                .unwrap_or(Rat::ZERO)
                .checked_plus(*a)?;
            if sum.is_zero() {
                terms.remove(v);
            } else {
                terms.insert(v.clone(), sum);
            }
        }
        Some(Lin {
            terms,
            c: self.c.checked_plus(o.c)?,
        })
    }

    pub fn sub(&self, o: &Lin) -> Option<Lin> {
        self.add(&o.scale(Rat::ONE.checked_negate()?)?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rel {
    /// `lin < 0`
    Lt,
    /// `lin <= 0`
    Le,
    /// `lin = 0`
    Eq,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum STerm {
    Var(String),
    Lit(String),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Atom {
    Lin(Lin, Rel),
    StrEq(STerm, STerm),
    /// A boolean variable holds `true`.
    Bool(String),
    /// A variable is null.
    Null(String),
    /// An expression outside the fragment, by its canonical text.
    Opaque(String),
}

/// Negation normal form: negation lives only on literals, and a linear literal is always positive.
#[derive(Clone, Debug, PartialEq)]
pub enum Formula {
    Const(bool),
    Lit(Atom, bool),
    And(Vec<Formula>),
    Or(Vec<Formula>),
}

impl Formula {
    pub fn and(parts: Vec<Formula>) -> Formula {
        let mut out = Vec::new();
        for p in parts {
            match p {
                Formula::Const(true) => {}
                Formula::Const(false) => return Formula::Const(false),
                Formula::And(inner) => out.extend(inner),
                other => out.push(other),
            }
        }
        match out.len() {
            0 => Formula::Const(true),
            1 => out.remove(0),
            _ => Formula::And(out),
        }
    }

    pub fn or(parts: Vec<Formula>) -> Formula {
        let mut out = Vec::new();
        for p in parts {
            match p {
                Formula::Const(false) => {}
                Formula::Const(true) => return Formula::Const(true),
                Formula::Or(inner) => out.extend(inner),
                other => out.push(other),
            }
        }
        match out.len() {
            0 => Formula::Const(false),
            1 => out.remove(0),
            _ => Formula::Or(out),
        }
    }

    pub fn atom(a: Atom) -> Formula {
        Formula::Lit(a, true)
    }

    /// `None` only when negating a linear literal overflows.
    pub fn negate(self) -> Option<Formula> {
        Some(match self {
            Formula::Const(b) => Formula::Const(!b),
            Formula::Lit(Atom::Lin(lin, rel), true) => {
                let neg = lin.scale(Rat::ONE.checked_negate()?)?;
                match rel {
                    Rel::Lt => Formula::atom(Atom::Lin(neg, Rel::Le)),
                    Rel::Le => Formula::atom(Atom::Lin(neg, Rel::Lt)),
                    Rel::Eq => Formula::or(vec![
                        Formula::atom(Atom::Lin(lin, Rel::Lt)),
                        Formula::atom(Atom::Lin(neg, Rel::Lt)),
                    ]),
                }
            }
            Formula::Lit(a, p) => Formula::Lit(a, !p),
            Formula::And(parts) => Formula::or(
                parts
                    .into_iter()
                    .map(Formula::negate)
                    .collect::<Option<_>>()?,
            ),
            Formula::Or(parts) => Formula::and(
                parts
                    .into_iter()
                    .map(Formula::negate)
                    .collect::<Option<_>>()?,
            ),
        })
    }

    pub fn implies(self, goal: Formula) -> Option<Formula> {
        Some(Formula::or(vec![self.negate()?, goal]))
    }
}

/// A numeric operand: its linear value, the variable it is when it is a bare (possibly null) variable, every variable
/// evaluating it reads, and whether it contains a null literal (arithmetic and ordering raise on one).
struct NumTerm {
    lin: Lin,
    bare: Option<String>,
    reads: BTreeSet<String>,
    null_literal: bool,
}

impl NumTerm {
    fn constant(c: Rat) -> NumTerm {
        NumTerm {
            lin: Lin::constant(c),
            bare: None,
            reads: BTreeSet::new(),
            null_literal: false,
        }
    }

    fn variable(name: &str) -> NumTerm {
        NumTerm {
            lin: Lin::var(name),
            bare: Some(name.to_string()),
            reads: BTreeSet::from([name.to_string()]),
            null_literal: false,
        }
    }
}

/// The two outcomes of a condition: `(evaluates to true, evaluates to false)`. Both fail when it raises.
type Outcomes = (Formula, Formula);

fn raises() -> Outcomes {
    (Formula::Const(false), Formula::Const(false))
}

/// Translates expressions against a variable table, recording the opaque atoms it had to introduce.
pub struct Translator<'a> {
    vars: &'a Vars,
    pub opaque: BTreeMap<String, SExpression>,
}

fn canonical(expr: &SExpression) -> String {
    serde_json::to_string(expr).unwrap_or_default()
}

fn is_binding(expr: &SExpression) -> Option<&str> {
    match expr {
        SExpression::String(s) if s.starts_with('@') => Some(s),
        _ => None,
    }
}

impl<'a> Translator<'a> {
    pub fn new(vars: &'a Vars) -> Self {
        Translator {
            vars,
            opaque: BTreeMap::new(),
        }
    }

    fn opaque(&mut self, expr: &SExpression) -> Formula {
        let key = canonical(expr);
        self.opaque.insert(key.clone(), expr.clone());
        Formula::atom(Atom::Opaque(key))
    }

    fn info(&self, name: &str) -> Option<&VarInfo> {
        self.vars.get(name)
    }

    fn null_of(&self, name: &str) -> Formula {
        match self.info(name) {
            Some(VarInfo { nullable: true, .. }) => Formula::atom(Atom::Null(name.to_string())),
            _ => Formula::Const(false),
        }
    }

    /// The expression evaluates to `true`.
    pub fn truthy(&mut self, expr: &SExpression) -> Option<Formula> {
        self.outcomes(expr).map(|(yes, _)| yes)
    }

    /// The expression evaluates to `false`: it does not raise, which is what separates it from `not truthy`.
    pub fn falsy(&mut self, expr: &SExpression) -> Option<Formula> {
        self.outcomes(expr).map(|(_, no)| no)
    }

    fn opaque_outcomes(&mut self, expr: &SExpression) -> Option<Outcomes> {
        let yes = self.opaque(expr);
        let no = yes.clone().negate()?;
        Some((yes, no))
    }

    fn outcomes(&mut self, expr: &SExpression) -> Option<Outcomes> {
        match expr {
            SExpression::Bool(b) => Some((Formula::Const(*b), Formula::Const(!*b))),
            SExpression::Null | SExpression::Number(_) | SExpression::Object(_) => Some(raises()),
            SExpression::String(s) if !s.starts_with('@') => Some(raises()),
            SExpression::String(name) => {
                let Some(info) = self.info(name).cloned() else {
                    return self.opaque_outcomes(expr);
                };
                if info.sort != Sort::Bool {
                    return Some(raises());
                }
                let present = self.null_of(name).negate()?;
                let value = Formula::atom(Atom::Bool(name.clone().into_inner()));
                Some((
                    Formula::and(vec![present.clone(), value.clone()]),
                    Formula::and(vec![present, value.negate()?]),
                ))
            }
            SExpression::Array(items) => {
                let Some((SExpression::String(op), args)) = items.split_first() else {
                    return Some(raises());
                };
                match op.as_str() {
                    "object/has" => match args {
                        [SExpression::Object(record), SExpression::String(key)]
                            if !key.starts_with('@') =>
                        {
                            let has = record.contains_key(key.as_str());
                            Some((Formula::Const(has), Formula::Const(!has)))
                        }
                        _ => self.opaque_outcomes(expr),
                    },
                    "and" => {
                        let mut reached = Vec::new();
                        let mut fails = Vec::new();
                        for arg in args {
                            let (yes, no) = self.outcomes(arg)?;
                            let mut case = reached.clone();
                            case.push(no);
                            fails.push(Formula::and(case));
                            reached.push(yes);
                        }
                        Some((Formula::and(reached), Formula::or(fails)))
                    }
                    "or" => {
                        let mut missed = Vec::new();
                        let mut holds = Vec::new();
                        for arg in args {
                            let (yes, no) = self.outcomes(arg)?;
                            let mut case = missed.clone();
                            case.push(yes);
                            holds.push(Formula::and(case));
                            missed.push(no);
                        }
                        Some((Formula::or(holds), Formula::and(missed)))
                    }
                    "not" => match args.first() {
                        Some(a) => {
                            let (yes, no) = self.outcomes(a)?;
                            Some((no, yes))
                        }
                        None => self.opaque_outcomes(expr),
                    },
                    "if" if args.len() >= 2 => {
                        let cond = self.outcomes(&args[0])?;
                        let then = self.outcomes(&args[1])?;
                        let otherwise = match args.get(2) {
                            Some(e) => self.outcomes(e)?,
                            None => raises(),
                        };
                        Self::branch(cond, then, otherwise)
                    }
                    "let" => match substitute_let(args) {
                        Some(body) => self.outcomes(&body),
                        None => self.opaque_outcomes(expr),
                    },
                    "=" | "==" | "!=" | "<" | "<=" | ">" | ">="
                        if args.len() >= 2
                            && (self.split_first_if(&args[0]).is_some()
                                || self.split_first_if(&args[1]).is_some()) =>
                    {
                        // `(if c a b)` inside an operand is `a` when `c` holds, else `b` (null when absent); the same for
                        // `(default a b)` and for `(math/max a b)` / `(math/min a b)` / `(math/abs a)`.
                        let (left, (c, then, otherwise)) = match self.split_first_if(&args[0]) {
                            Some(parts) => (true, parts),
                            None => (false, self.split_first_if(&args[1])?),
                        };
                        let with = |branch: SExpression| {
                            let (a, b) = if left {
                                (branch, args[1].clone())
                            } else {
                                (args[0].clone(), branch)
                            };
                            SExpression::Array(vec![SExpression::String(op.clone()), a, b])
                        };
                        let cond = self.outcomes(&c)?;
                        let yes = self.outcomes(&with(then))?;
                        let no = self.outcomes(&with(otherwise))?;
                        Self::branch(cond, yes, no)
                    }
                    "=" | "==" | "!=" if args.len() >= 2 => {
                        let (defined, eq) = self.equal(&args[0], &args[1], expr)?;
                        let same = Formula::and(vec![defined.clone(), eq.clone()]);
                        let apart = Formula::and(vec![defined, eq.negate()?]);
                        if op == "!=" {
                            Some((apart, same))
                        } else {
                            Some((same, apart))
                        }
                    }
                    "<" | "<=" | ">" | ">=" if args.len() >= 2 => {
                        self.order(op, &args[0], &args[1], expr)
                    }
                    _ => self.opaque_outcomes(expr),
                }
            }
        }
    }

    /// `(if cond then otherwise)` as outcomes: the chosen branch's, given the condition's.
    fn branch(cond: Outcomes, then: Outcomes, otherwise: Outcomes) -> Option<Outcomes> {
        let (cond_yes, cond_no) = cond;
        Some((
            Formula::or(vec![
                Formula::and(vec![cond_yes.clone(), then.0]),
                Formula::and(vec![cond_no.clone(), otherwise.0]),
            ]),
            Formula::or(vec![
                Formula::and(vec![cond_yes, then.1]),
                Formula::and(vec![cond_no, otherwise.1]),
            ]),
        ))
    }

    /// The first `if` (or `default`, `math/abs`, `math/max`, `math/min`) an operand evaluates through, as the
    /// condition and the operand rewritten for each outcome. The condition itself raises where an operand would.
    fn split_first_if(
        &self,
        expr: &SExpression,
    ) -> Option<(SExpression, SExpression, SExpression)> {
        if let Some(parts) = as_if(expr) {
            return Some(parts);
        }
        let SExpression::Array(items) = expr else {
            return None;
        };
        let (SExpression::String(op), args) = items.split_first()? else {
            return None;
        };
        match op.as_str() {
            "default" if args.len() == 2 => {
                let present = SExpression::Array(vec![
                    SExpression::string("!="),
                    args[0].clone(),
                    SExpression::Null,
                ]);
                Some((present, args[0].clone(), args[1].clone()))
            }
            "+" | "-" | "*" | "/" => {
                for (i, arg) in args.iter().enumerate() {
                    if let Some((c, then, otherwise)) = self.split_first_if(arg) {
                        let with = |branch: SExpression| {
                            let mut next = items.clone();
                            next[i + 1] = branch;
                            SExpression::Array(next)
                        };
                        return Some((c, with(then), with(otherwise)));
                    }
                }
                None
            }
            "math/abs" if args.len() == 1 => {
                self.num_term(&args[0])?;
                let cond = SExpression::Array(vec![
                    SExpression::string(">="),
                    args[0].clone(),
                    SExpression::Number(0.0),
                ]);
                let negated = SExpression::Array(vec![
                    SExpression::string("-"),
                    SExpression::Number(0.0),
                    args[0].clone(),
                ]);
                Some((cond, args[0].clone(), negated))
            }
            "math/max" | "math/min" if args.len() == 2 => {
                self.num_term(&args[0])?;
                self.num_term(&args[1])?;
                let cmp = if op.ends_with("max") { ">=" } else { "<=" };
                let cond = SExpression::Array(vec![
                    SExpression::string(cmp.to_string()),
                    args[0].clone(),
                    args[1].clone(),
                ]);
                Some((cond, args[0].clone(), args[1].clone()))
            }
            _ => None,
        }
    }

    fn num_term(&self, expr: &SExpression) -> Option<NumTerm> {
        match expr {
            SExpression::Number(n) => Some(NumTerm::constant(Rat::from_f64(*n)?)),
            SExpression::Null => Some(NumTerm {
                null_literal: true,
                ..NumTerm::constant(Rat::ZERO)
            }),
            SExpression::String(name) if name.starts_with('@') => match self.info(name) {
                Some(VarInfo {
                    sort: Sort::Num, ..
                }) => Some(NumTerm::variable(name)),
                _ => None,
            },
            SExpression::Array(items) => {
                let (SExpression::String(op), args) = items.split_first()? else {
                    return None;
                };
                if op == "array/len" && args.len() == 1 {
                    let name = array_len_var(expr);
                    if self.info(&name).is_some() {
                        return Some(NumTerm::variable(&name));
                    }
                    return None;
                }
                if matches!(op.as_str(), "time/now")
                    && args.is_empty()
                    && self.info("@now").is_some()
                {
                    return Some(NumTerm::variable("@now"));
                }
                if !matches!(op.as_str(), "+" | "-" | "*" | "/") {
                    return None;
                }
                let parts = args
                    .iter()
                    .map(|a| self.num_term(a))
                    .collect::<Option<Vec<_>>>()?;
                let lin = match op.as_str() {
                    "+" => parts
                        .iter()
                        .try_fold(Lin::constant(Rat::ZERO), |acc, t| acc.add(&t.lin))?,
                    "-" => match parts.as_slice() {
                        [] => Lin::constant(Rat::ZERO),
                        [only] => only.lin.scale(Rat::ONE.checked_negate()?)?,
                        [first, rest @ ..] => rest
                            .iter()
                            .try_fold(first.lin.clone(), |acc, t| acc.sub(&t.lin))?,
                    },
                    "*" => {
                        let mut acc = Lin::constant(Rat::ONE);
                        for t in &parts {
                            acc = match (acc.terms.is_empty(), t.lin.terms.is_empty()) {
                                (true, _) => t.lin.scale(acc.c)?,
                                (false, true) => acc.scale(t.lin.c)?,
                                (false, false) => return None,
                            };
                        }
                        acc
                    }
                    _ => match parts.as_slice() {
                        [num, den] => {
                            if !den.lin.terms.is_empty() || den.lin.c.is_zero() {
                                return None;
                            }
                            num.lin.scale(Rat::ONE.checked_over(den.lin.c)?)?
                        }
                        _ => return None,
                    },
                };
                Some(NumTerm {
                    lin,
                    bare: None,
                    reads: parts.iter().flat_map(|t| t.reads.iter().cloned()).collect(),
                    null_literal: parts.iter().any(|t| t.null_literal),
                })
            }
            _ => None,
        }
    }

    /// Every variable the operand reads is present (and it holds no null literal): arithmetic and ordering raise
    /// otherwise. Equality passes `bare_may_be_null`, since a bare operand is compared as it is, null included.
    fn present(&self, term: &NumTerm, bare_may_be_null: bool) -> Option<Formula> {
        if term.null_literal {
            return Some(Formula::Const(false));
        }
        let mut parts = Vec::new();
        for v in &term.reads {
            if bare_may_be_null && term.bare.as_deref() == Some(v.as_str()) {
                continue;
            }
            parts.push(self.null_of(v).negate()?);
        }
        Some(Formula::and(parts))
    }

    /// `(defined, equal)`: equality never raises on its own, so `defined` only carries the presence an arithmetic
    /// operand needs, and `equal` is the structural equality of the two values.
    fn equal(
        &mut self,
        a: &SExpression,
        b: &SExpression,
        whole: &SExpression,
    ) -> Option<(Formula, Formula)> {
        let always = Formula::Const(true);
        if matches!(a, SExpression::Null) || matches!(b, SExpression::Null) {
            let other = if matches!(a, SExpression::Null) { b } else { a };
            return Some(match other {
                SExpression::Null => (always, Formula::Const(true)),
                SExpression::String(name) if name.starts_with('@') => match self.info(name) {
                    Some(_) => (always, self.null_of(name)),
                    None => (always, self.opaque(whole)),
                },
                SExpression::Bool(_) | SExpression::Number(_) | SExpression::String(_) => {
                    (always, Formula::Const(false))
                }
                _ => match self.num_term(other) {
                    Some(t) => (self.present(&t, false)?, Formula::Const(false)),
                    None => (always, self.opaque(whole)),
                },
            });
        }
        let null_pair =
            |t: &Self, x: Option<&str>, y: Option<&str>| -> Option<(Formula, Formula)> {
                let nx = x.map_or(Formula::Const(false), |v| t.null_of(v));
                let ny = y.map_or(Formula::Const(false), |v| t.null_of(v));
                Some((nx, ny))
            };
        let joined = |nx: Formula, ny: Formula, value: Formula| -> Option<Formula> {
            Some(Formula::or(vec![
                Formula::and(vec![nx.clone(), ny.clone()]),
                Formula::and(vec![nx.negate()?, ny.negate()?, value]),
            ]))
        };
        if let (Some(x), Some(y)) = (self.num_term(a), self.num_term(b)) {
            let (nx, ny) = null_pair(self, x.bare.as_deref(), y.bare.as_deref())?;
            let defined = Formula::and(vec![self.present(&x, true)?, self.present(&y, true)?]);
            let value = Formula::atom(Atom::Lin(x.lin.sub(&y.lin)?, Rel::Eq));
            return Some((defined, joined(nx, ny, value)?));
        }
        if let (Some(x), Some(y)) = (self.str_term(a), self.str_term(b)) {
            let var = |t: &STerm| match t {
                STerm::Var(v) => Some(v.clone()),
                STerm::Lit(_) => None,
            };
            let (nx, ny) = null_pair(self, var(&x).as_deref(), var(&y).as_deref())?;
            let value = match (&x, &y) {
                (STerm::Lit(p), STerm::Lit(q)) => Formula::Const(p == q),
                _ => Formula::atom(Atom::StrEq(x, y)),
            };
            return Some((always, joined(nx, ny, value)?));
        }
        if let (Some((x, xs)), Some((y, ys))) = (self.bool_term(a), self.bool_term(b)) {
            let (nx, ny) = null_pair(self, x.as_deref(), y.as_deref())?;
            let value = match (&x, &y) {
                (None, None) => Formula::Const(xs == ys),
                (Some(v), None) | (None, Some(v)) => {
                    let lit = if x.is_none() { xs } else { ys };
                    Formula::Lit(Atom::Bool(v.clone()), lit)
                }
                (Some(p), Some(q)) => {
                    let (bp, bq) = (
                        Formula::atom(Atom::Bool(p.clone())),
                        Formula::atom(Atom::Bool(q.clone())),
                    );
                    Formula::or(vec![
                        Formula::and(vec![bp.clone(), bq.clone()]),
                        Formula::and(vec![bp.negate()?, bq.negate()?]),
                    ])
                }
            };
            return Some((always, joined(nx, ny, value)?));
        }
        Some((always, self.opaque(whole)))
    }

    fn str_term(&self, expr: &SExpression) -> Option<STerm> {
        match expr {
            SExpression::String(name) if name.starts_with('@') => match self.info(name) {
                Some(VarInfo {
                    sort: Sort::Str(_), ..
                }) => Some(STerm::Var(name.clone().into_inner())),
                _ => None,
            },
            SExpression::String(s) => Some(STerm::Lit(s.clone().into_inner())),
            _ => None,
        }
    }

    /// A boolean operand: `(Some(var), _)` for a boolean variable, `(None, value)` for a literal.
    fn bool_term(&self, expr: &SExpression) -> Option<(Option<String>, bool)> {
        match expr {
            SExpression::Bool(b) => Some((None, *b)),
            SExpression::String(name) if name.starts_with('@') => match self.info(name) {
                Some(VarInfo {
                    sort: Sort::Bool, ..
                }) => Some((Some(name.clone().into_inner()), true)),
                _ => None,
            },
            _ => None,
        }
    }

    fn order(
        &mut self,
        op: &str,
        a: &SExpression,
        b: &SExpression,
        whole: &SExpression,
    ) -> Option<Outcomes> {
        let (Some(x), Some(y)) = (self.num_term(a), self.num_term(b)) else {
            return self.opaque_outcomes(whole);
        };
        let (lin, rel) = match op {
            "<" => (x.lin.sub(&y.lin)?, Rel::Lt),
            "<=" => (x.lin.sub(&y.lin)?, Rel::Le),
            ">" => (y.lin.sub(&x.lin)?, Rel::Lt),
            _ => (y.lin.sub(&x.lin)?, Rel::Le),
        };
        let defined = Formula::and(vec![self.present(&x, false)?, self.present(&y, false)?]);
        let holds = Formula::atom(Atom::Lin(lin, rel));
        Some((
            Formula::and(vec![defined.clone(), holds.clone()]),
            Formula::and(vec![defined, holds.negate()?]),
        ))
    }
}

/// `(if c a [b])` as `(c, a, b)`, the absent else being null.
fn as_if(expr: &SExpression) -> Option<(SExpression, SExpression, SExpression)> {
    match expr {
        SExpression::Array(items)
            if items.len() >= 3
                && matches!(items.first(), Some(SExpression::String(h)) if h == "if") =>
        {
            Some((
                items[1].clone(),
                items[2].clone(),
                items.get(3).cloned().unwrap_or(SExpression::Null),
            ))
        }
        _ => None,
    }
}

/// `(let ((a e1) (b e2)) body)` with each binding substituted into the later ones and the body, or `None` when a
/// nested binder reuses one of the names (substitution would capture it).
pub fn substitute_let(args: &[SExpression]) -> Option<SExpression> {
    let scope = SequentialBindings::from_args(args)?;
    let names: BTreeSet<&str> = scope.bindings.iter().map(|(n, _)| n.as_str()).collect();
    if args.iter().any(|a| rebinds(a, &names)) {
        return None;
    }
    let mut values: Vec<(String, SExpression)> = Vec::new();
    for (name, init) in &scope.bindings {
        let mut v = (*init).clone();
        for (prev, pv) in &values {
            v = super::witness::substitute_var(&v, prev, pv);
        }
        values.push((name.to_string(), v));
    }
    let mut body = scope.body.clone();
    for (name, value) in values.iter().rev() {
        body = super::witness::substitute_var(&body, name, value);
    }
    Some(body)
}

fn rebinds(expr: &SExpression, names: &BTreeSet<&str>) -> bool {
    let SExpression::Array(items) = expr else {
        return false;
    };
    let binder = match items.first() {
        Some(SExpression::String(op)) if op == "let" => items
            .get(1)
            .and_then(|b| b.as_array())
            .is_some_and(|pairs| {
                pairs.iter().any(|p| {
                    p.as_array()
                        .and_then(|p| p.first())
                        .and_then(|n| n.as_str())
                        .is_some_and(|n| names.contains(n))
                })
            }),
        Some(SExpression::String(op)) if op == "fn" => items
            .get(1)
            .and_then(|n| n.as_str())
            .is_some_and(|n| names.contains(n)),
        _ => false,
    };
    binder || items.iter().any(|i| rebinds(i, names))
}

/// Every variable a formula mentions.
pub fn formula_vars(f: &Formula, out: &mut BTreeSet<String>) {
    match f {
        Formula::Const(_) => {}
        Formula::Lit(atom, _) => match atom {
            Atom::Lin(lin, _) => out.extend(lin.terms.keys().cloned()),
            Atom::StrEq(a, b) => {
                for t in [a, b] {
                    if let STerm::Var(v) = t {
                        out.insert(v.clone());
                    }
                }
            }
            Atom::Bool(v) | Atom::Null(v) => {
                out.insert(v.clone());
            }
            Atom::Opaque(_) => {}
        },
        Formula::And(parts) | Formula::Or(parts) => parts.iter().for_each(|p| formula_vars(p, out)),
    }
}

/// Mention the variable a binding names, for callers that need every binding of an expression.
pub fn binding_texts(expr: &SExpression, out: &mut BTreeSet<String>) {
    match expr {
        SExpression::String(_) => {
            if let Some(b) = is_binding(expr) {
                out.insert(b.to_string());
            }
        }
        SExpression::Array(items) => items.iter().for_each(|i| binding_texts(i, out)),
        SExpression::Object(map) => map.values().for_each(|v| binding_texts(v, out)),
        _ => {}
    }
}

/// The variable standing for the length of an array the prover cannot see into: a non-negative number.
pub fn array_len_var(expr: &SExpression) -> String {
    format!("@len:{}", orbital_core_base::schema::format_sexpr(expr))
}

/// Every `(array/len …)` an expression reads.
pub fn array_lens(expr: &SExpression, out: &mut Vec<SExpression>) {
    if let SExpression::Array(items) = expr {
        if matches!(items.first(), Some(SExpression::String(h)) if h == "array/len")
            && items.len() == 2
            && !out.contains(expr)
        {
            out.push(expr.clone());
        }
        for item in items {
            array_lens(item, out);
        }
    }
}
