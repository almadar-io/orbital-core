//! Guard S-expressions as formulas over typed variables, following the evaluator's semantics exactly: truthiness
//! (`Value::is_truthy`), `=` as `loose_eq` (a null equals only null), ordering and arithmetic coercing null to 0
//! (`Value::compare`, `to_number`). Anything outside that fragment becomes an opaque atom, named so an unproved
//! obligation can say what it could not decide.
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

/// A numeric operand: its linear value, and the variable it is when it is a bare (possibly null) variable.
struct NumTerm {
    lin: Lin,
    bare: Option<String>,
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

    /// The expression is truthy (`Value::is_truthy`) when evaluated.
    pub fn truthy(&mut self, expr: &SExpression) -> Option<Formula> {
        match expr {
            SExpression::Null => Some(Formula::Const(false)),
            SExpression::Bool(b) => Some(Formula::Const(*b)),
            SExpression::Number(n) => Some(Formula::Const(*n != 0.0 && !n.is_nan())),
            SExpression::String(s) if !s.starts_with('@') => Some(Formula::Const(!s.is_empty())),
            SExpression::String(name) => {
                let Some(info) = self.info(name).cloned() else {
                    return Some(self.opaque(expr));
                };
                let present = self.null_of(name).negate()?;
                let value = match info.sort {
                    Sort::Bool => Formula::atom(Atom::Bool(name.clone())),
                    Sort::Num => Formula::atom(Atom::Lin(Lin::var(name), Rel::Eq)).negate()?,
                    Sort::Str(_) => Formula::Lit(
                        Atom::StrEq(STerm::Var(name.clone()), STerm::Lit(String::new())),
                        false,
                    ),
                };
                Some(Formula::and(vec![present, value]))
            }
            SExpression::Object(map) => Some(Formula::Const(!map.is_empty())),
            SExpression::Array(items) => {
                let Some((SExpression::String(op), args)) = items.split_first() else {
                    return Some(Formula::Const(!items.is_empty()));
                };
                match op.as_str() {
                    "object/has" => match args {
                        [SExpression::Object(record), SExpression::String(key)]
                            if !key.starts_with('@') =>
                        {
                            Some(Formula::Const(record.contains_key(key)))
                        }
                        _ => Some(self.opaque(expr)),
                    },
                    "and" | "&&" => {
                        let parts = args
                            .iter()
                            .map(|a| self.truthy(a))
                            .collect::<Option<Vec<_>>>()?;
                        Some(Formula::and(parts))
                    }
                    "or" | "||" => {
                        let parts = args
                            .iter()
                            .map(|a| self.truthy(a))
                            .collect::<Option<Vec<_>>>()?;
                        Some(Formula::or(parts))
                    }
                    "not" | "!" => match args.first() {
                        Some(a) => self.truthy(a)?.negate(),
                        None => Some(self.opaque(expr)),
                    },
                    "if" if args.len() >= 2 => {
                        let c = self.truthy(&args[0])?;
                        let then = self.truthy(&args[1])?;
                        let otherwise = match args.get(2) {
                            Some(e) => self.truthy(e)?,
                            None => Formula::Const(false),
                        };
                        Some(Formula::or(vec![
                            Formula::and(vec![c.clone(), then]),
                            Formula::and(vec![c.negate()?, otherwise]),
                        ]))
                    }
                    "let" => match substitute_let(args) {
                        Some(body) => self.truthy(&body),
                        None => Some(self.opaque(expr)),
                    },
                    "=" | "==" | "!=" | "<>" | "<" | "<=" | ">" | ">="
                        if args.len() >= 2
                            && (self.split_first_if(&args[0]).is_some()
                                || self.split_first_if(&args[1]).is_some()) =>
                    {
                        // `(if c a b)` inside an operand is `a` when `c` holds, else `b` (null when absent); the same for
                        // `(math/max a b)` and `(math/min a b)` over operands that are surely numbers.
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
                        let cond = self.truthy(&c)?;
                        let yes = self.truthy(&with(then))?;
                        let no = self.truthy(&with(otherwise))?;
                        Some(Formula::or(vec![
                            Formula::and(vec![cond.clone(), yes]),
                            Formula::and(vec![cond.negate()?, no]),
                        ]))
                    }
                    "=" | "==" | "!=" | "<>" if args.len() >= 2 => {
                        let eq = self.equal(&args[0], &args[1], expr)?;
                        if op == "=" || op == "==" {
                            Some(eq)
                        } else {
                            eq.negate()
                        }
                    }
                    "<" | "<=" | ">" | ">=" if args.len() >= 2 => {
                        self.order(op, &args[0], &args[1], expr)
                    }
                    _ => Some(self.opaque(expr)),
                }
            }
        }
    }

    /// The first `if` (or surely-numeric `math/max`/`math/min`) an operand evaluates through, as the condition and the
    /// operand rewritten for each outcome.
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
            "or" | "||" if args.len() == 2 => {
                Some((args[0].clone(), args[0].clone(), args[1].clone()))
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
            "math/abs" | "abs" if args.len() == 1 => {
                let surely = !matches!(args[0], SExpression::Null)
                    && self.num_term(&args[0]).is_some_and(|t| {
                        t.bare
                            .as_deref()
                            .is_none_or(|v| self.info(v).is_some_and(|i| !i.nullable))
                    });
                if !surely {
                    return None;
                }
                let cond = SExpression::Array(vec![
                    SExpression::String(">=".to_string()),
                    args[0].clone(),
                    SExpression::Number(0.0),
                ]);
                let negated = SExpression::Array(vec![
                    SExpression::String("-".to_string()),
                    SExpression::Number(0.0),
                    args[0].clone(),
                ]);
                Some((cond, args[0].clone(), negated))
            }
            "math/max" | "max" | "math/min" | "min" if args.len() == 2 => {
                let numeric = |a: &SExpression| {
                    !matches!(a, SExpression::Null)
                        && self.num_term(a).is_some_and(|t| {
                            t.bare
                                .as_deref()
                                .is_none_or(|v| self.info(v).is_some_and(|i| !i.nullable))
                        })
                };
                if !(numeric(&args[0]) && numeric(&args[1])) {
                    return None;
                }
                let cmp = if op.ends_with("max") { ">=" } else { "<=" };
                let cond = SExpression::Array(vec![
                    SExpression::String(cmp.to_string()),
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
            SExpression::Number(n) => Some(NumTerm {
                lin: Lin::constant(Rat::from_f64(*n)?),
                bare: None,
            }),
            SExpression::Null => Some(NumTerm {
                lin: Lin::constant(Rat::ZERO),
                bare: None,
            }),
            SExpression::String(name) if name.starts_with('@') => match self.info(name) {
                Some(VarInfo {
                    sort: Sort::Num, ..
                }) => Some(NumTerm {
                    lin: Lin::var(name),
                    bare: Some(name.clone()),
                }),
                _ => None,
            },
            SExpression::Array(items) => {
                let (SExpression::String(op), args) = items.split_first()? else {
                    return None;
                };
                if op == "array/len" && args.len() == 1 {
                    let name = array_len_var(expr);
                    if self.info(&name).is_some() {
                        return Some(NumTerm {
                            lin: Lin::var(&name),
                            bare: Some(name),
                        });
                    }
                    return None;
                }
                if matches!(op.as_str(), "time/now" | "now")
                    && args.is_empty()
                    && self.info("@now").is_some()
                {
                    return Some(NumTerm {
                        lin: Lin::var("@now"),
                        bare: Some("@now".to_string()),
                    });
                }
                let lin = match op.as_str() {
                    "+" => args.iter().try_fold(Lin::constant(Rat::ZERO), |acc, a| {
                        acc.add(&self.num_term(a)?.lin)
                    })?,
                    "-" => match args {
                        [] => Lin::constant(Rat::ZERO),
                        [only] => self.num_term(only)?.lin.scale(Rat::ONE.checked_negate()?)?,
                        [first, rest @ ..] => {
                            rest.iter().try_fold(self.num_term(first)?.lin, |acc, a| {
                                acc.sub(&self.num_term(a)?.lin)
                            })?
                        }
                    },
                    "*" => {
                        let mut acc = Lin::constant(Rat::ONE);
                        for a in args {
                            let t = self.num_term(a)?.lin;
                            acc = match (acc.terms.is_empty(), t.terms.is_empty()) {
                                (true, _) => t.scale(acc.c)?,
                                (false, true) => acc.scale(t.c)?,
                                (false, false) => return None,
                            };
                        }
                        acc
                    }
                    "/" if args.len() >= 2 => {
                        let num = self.num_term(&args[0])?;
                        let den = self.num_term(&args[1])?;
                        // A null numerator is a type error in `/`, so only a non-null one is linear.
                        let numerator_nullable = num
                            .bare
                            .as_deref()
                            .is_some_and(|v| self.info(v).is_some_and(|i| i.nullable));
                        if !den.lin.terms.is_empty() || den.lin.c.is_zero() || numerator_nullable {
                            return None;
                        }
                        num.lin.scale(Rat::ONE.checked_over(den.lin.c)?)?
                    }
                    _ => return None,
                };
                Some(NumTerm { lin, bare: None })
            }
            _ => None,
        }
    }

    fn equal(&mut self, a: &SExpression, b: &SExpression, whole: &SExpression) -> Option<Formula> {
        if matches!(a, SExpression::Null) || matches!(b, SExpression::Null) {
            let other = if matches!(a, SExpression::Null) { b } else { a };
            return Some(match other {
                SExpression::Null => Formula::Const(true),
                SExpression::String(name) if name.starts_with('@') => match self.info(name) {
                    Some(_) => self.null_of(name),
                    None => self.opaque(whole),
                },
                SExpression::Bool(_) | SExpression::Number(_) | SExpression::String(_) => {
                    Formula::Const(false)
                }
                _ => match self.num_term(other) {
                    Some(_) => Formula::Const(false),
                    None => self.opaque(whole),
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
            let value = Formula::atom(Atom::Lin(x.lin.sub(&y.lin)?, Rel::Eq));
            return joined(nx, ny, value);
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
            return joined(nx, ny, value);
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
            return joined(nx, ny, value);
        }
        Some(self.opaque(whole))
    }

    fn str_term(&self, expr: &SExpression) -> Option<STerm> {
        match expr {
            SExpression::String(name) if name.starts_with('@') => match self.info(name) {
                Some(VarInfo {
                    sort: Sort::Str(_), ..
                }) => Some(STerm::Var(name.clone())),
                _ => None,
            },
            SExpression::String(s) => Some(STerm::Lit(s.clone())),
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
                }) => Some((Some(name.clone()), true)),
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
    ) -> Option<Formula> {
        let (Some(x), Some(y)) = (self.num_term(a), self.num_term(b)) else {
            return Some(self.opaque(whole));
        };
        let (lin, rel) = match op {
            "<" => (x.lin.sub(&y.lin)?, Rel::Lt),
            "<=" => (x.lin.sub(&y.lin)?, Rel::Le),
            ">" => (y.lin.sub(&x.lin)?, Rel::Lt),
            _ => (y.lin.sub(&x.lin)?, Rel::Le),
        };
        Some(Formula::atom(Atom::Lin(lin, rel)))
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
    let names: BTreeSet<&str> = scope.bindings.iter().map(|(n, _)| *n).collect();
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
