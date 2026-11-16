//! Tool: how the canonical lowering treats every guard and effect of a lowered corpus (P-039 step 2).
//! `ORB_LOWER_CORPUS=<file of {"path","orb"} lines from orb lower> cargo test -p orbital-core-eval --test operators
//! lowering_of_a_corpus -- --ignored --nocapture`

use orbital_core_base::json_value::from_json_value;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_eval::language_loader::preflight;
use orbital_core_eval::evaluator::context::BindingContextBuilder;
use orbital_core_eval::evaluator::operators::eval_expr;
use orbital_core_eval::lower::lower_sexpr;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;

const HOLDERS: [&str; 3] = ["transitions", "ticks", "listens"];

/// Every guard and effect of the transitions, ticks and listeners under `node`.
fn expressions(node: &Value, held: bool, out: &mut Vec<Value>) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value) {
                    ("guard", expr) if held && !expr.is_null() => out.push(expr.clone()),
                    ("effects", Value::Array(effects)) if held => out.extend(effects.iter().cloned()),
                    (key, value) => expressions(value, held || HOLDERS.contains(&key), out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| expressions(item, held, out)),
        _ => {}
    }
}

fn head(expr: &SExpression) -> String {
    match expr {
        SExpression::Array(items) => match items.first() {
            Some(SExpression::String(op)) => op.to_string(),
            _ => "<list>".to_string(),
        },
        SExpression::Object(_) => "<record>".to_string(),
        SExpression::String(text) if text.starts_with('@') => "<binding>".to_string(),
        _ => "<literal>".to_string(),
    }
}

/// The first node where `back` differs from `original`, as the pair of heads.
fn first_difference(original: &SExpression, back: &SExpression) -> Option<(String, String)> {
    if original == back {
        return None;
    }
    if let (SExpression::Array(a), SExpression::Array(b)) = (original, back) {
        if a.len() == b.len() && a.first() == b.first() {
            return a.iter().zip(b.iter()).find_map(|(x, y)| first_difference(x, y));
        }
    }
    if let (SExpression::Object(a), SExpression::Object(b)) = (original, back) {
        if a.len() == b.len() && a.keys().eq(b.keys()) {
            return a.values().zip(b.values()).find_map(|(x, y)| first_difference(x, y));
        }
    }
    Some((head(original), head(back)))
}

#[test]
#[ignore = "tool: ORB_LOWER_CORPUS=<orb lower output lines> reports how the lowering treats each guard and effect"]
fn lowering_of_a_corpus() {
    preflight().expect("the installed language data must load");
    let path = std::env::var("ORB_LOWER_CORPUS").expect("ORB_LOWER_CORPUS");
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("corpus file"));
    let mut seen = BTreeSet::new();
    let (mut total, mut exact, mut no_form, mut same_outcome) = (0usize, 0usize, 0usize, 0usize);
    let mut outcome_drift: Vec<String> = Vec::new();
    let mut refused_samples: BTreeMap<String, String> = BTreeMap::new();
    let ctx = BindingContextBuilder::new().build();
    let outcome = |expr: &SExpression| match eval_expr(expr, &ctx) {
        Ok(value) => format!("ok {}", serde_json::to_string(&value).unwrap_or_default()),
        Err(error) => format!("err {error}"),
    };
    let mut refused: BTreeMap<String, usize> = BTreeMap::new();
    let mut changed: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut samples: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    for line in file.lines() {
        let entry: Value = orbital_core_base::json_value::from_json_str(&line.expect("line")).expect("json line");
        let mut found = Vec::new();
        expressions(&entry["orb"], false, &mut found);
        for raw in found {
            let text = raw.to_string();
            if !seen.insert(text) {
                continue;
            }
            total += 1;
            let expr: SExpression = from_json_value(&raw).expect("an expression");
            match lower_sexpr(&expr) {
                Err(error) => {
                    let why = format!("{}: {error}", head(&expr));
                    refused_samples.entry(why.clone()).or_insert_with(|| serde_json::to_string(&expr).unwrap_or_default());
                    *refused.entry(why).or_default() += 1;
                }
                Ok(lowered) => match lowered.to_sexpression() {
                    None => no_form += 1,
                    Some(back) => {
                        let (was, now) = (outcome(&expr), outcome(&back));
                        if was == now {
                            same_outcome += 1;
                        } else {
                            outcome_drift.push(format!("{}\n    was  {was}\n    back {now}", serde_json::to_string(&expr).unwrap_or_default()));
                        }
                        match first_difference(&expr, &back) {
                        None => exact += 1,
                        Some(pair) => {
                            samples.entry(pair.clone()).or_insert_with(|| {
                                (serde_json::to_string(&expr).unwrap_or_default(), serde_json::to_string(&back).unwrap_or_default())
                            });
                            *changed.entry(pair).or_default() += 1;
                        }
                    }}
                },
            }
        }
    }
    println!("OUTCOME same={same_outcome} drift={}", outcome_drift.len());
    for drift in &outcome_drift {
        println!("DRIFT {drift}");
    }
    println!("LOWER distinct={total} exact={exact} no_sexpr_form={no_form} refused={} changed={}", refused.values().sum::<usize>(), changed.values().sum::<usize>());
    let mut refused: Vec<_> = refused.into_iter().collect();
    refused.sort_by(|a, b| b.1.cmp(&a.1));
    for (why, count) in refused {
        println!("REFUSED {count}\t{why}\n    was  {}", refused_samples[&why]);
    }
    let mut changed: Vec<_> = changed.into_iter().collect();
    changed.sort_by(|a, b| b.1.cmp(&a.1));
    for ((from, to), count) in changed {
        let (original, back) = &samples[&(from.clone(), to.clone())];
        println!("CHANGED {count}\t{from} -> {to}\n    was  {original}\n    back {back}");
    }
}

fn round_trip(source: serde_json::Value) -> (SExpression, SExpression) {
    preflight().expect("the installed language data must load");
    let expr: SExpression = from_json_value(&source).expect("an expression");
    let back = lower_sexpr(&expr).expect("lowers").to_sexpression().expect("a pure expression");
    (expr, back)
}

#[test]
fn an_equality_lowers_and_comes_back_as_written() {
    let (expr, back) = round_trip(serde_json::json!(["=", "@payload.a", 1]));
    assert_eq!(expr, back);
}

#[test]
fn control_a_variadic_and_comes_back_as_nested_pairs_with_the_same_answer() {
    let (expr, back) = round_trip(serde_json::json!(["and", true, ["=", 1, 1], false]));
    let nested: SExpression =
        from_json_value(&serde_json::json!(["and", ["and", true, ["=", 1, 1]], false])).expect("an expression");
    assert_eq!(back, nested);
    let ctx = BindingContextBuilder::new().build();
    assert_eq!(eval_expr(&expr, &ctx).expect("evaluates"), eval_expr(&back, &ctx).expect("evaluates"));
}

#[test]
#[ignore = "tool: ORB_LOWER_CORPUS=<orb lower output lines> reports how much of the corpus has a step form and agrees"]
fn step_form_of_a_corpus() {
    use crate::step_runner::{evaluated, run, step_of};
    preflight().expect("the installed language data must load");
    let path = std::env::var("ORB_LOWER_CORPUS").expect("ORB_LOWER_CORPUS");
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("corpus file"));
    let ctx = BindingContextBuilder::new().build();
    let mut seen = BTreeSet::new();
    let (mut total, mut lowered, mut agreed, mut other_fault) = (0usize, 0usize, 0usize, 0usize);
    let mut refused: BTreeMap<String, usize> = BTreeMap::new();
    let mut not_run: BTreeMap<String, usize> = BTreeMap::new();
    let mut drift = Vec::new();
    for line in file.lines() {
        let entry: Value = orbital_core_base::json_value::from_json_str(&line.expect("line")).expect("json line");
        let mut found = Vec::new();
        expressions(&entry["orb"], false, &mut found);
        for raw in found {
            if !seen.insert(raw.to_string()) {
                continue;
            }
            total += 1;
            let expr: SExpression = from_json_value(&raw).expect("an expression");
            match step_of(&expr) {
                Err(refusal) => *refused.entry(refusal).or_default() += 1,
                Ok(module) => {
                    lowered += 1;
                    match run(&module, &ctx) {
                        Err(what) => *not_run.entry(what).or_default() += 1,
                        Ok(ran) if ran == evaluated(&expr, &ctx) => agreed += 1,
                        // Both fail, each on a different fault of the same call: which fault is reported (EXEC-EVAL-3b).
                        Ok(Err(_)) if evaluated(&expr, &ctx).is_err() => other_fault += 1,
                        Ok(ran) => drift.push(format!("{raw}\n    step {ran:?}\n    eval {:?}", evaluated(&expr, &ctx))),
                    }
                }
            }
        }
    }
    println!(
        "STEP distinct={total} lowered={lowered} agreed={agreed} other_fault={other_fault} drift={} not_run={} refused={}",
        drift.len(),
        not_run.values().sum::<usize>(),
        refused.values().sum::<usize>()
    );
    for (title, table) in [("NOT_RUN", not_run), ("REFUSED", refused)] {
        let mut rows: Vec<_> = table.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        for (why, count) in rows {
            println!("{title} {count}\t{why}");
        }
    }
    for case in drift {
        println!("DRIFT {case}");
    }
}

/// Every effect node in `expr`, outermost first.
fn effects_in<'a>(expr: &'a orbital_core_base::ir::expr::OirExpr, out: &mut Vec<&'a orbital_core_base::ir::effects::OirEffect>) {
    if let orbital_core_base::ir::expr::OirExpr::Effect { effect } = expr {
        out.push(effect);
    }
    for child in expr.child_exprs() {
        effects_in(child, out);
    }
}

/// What the kernel hands an effect for one operand written as `written` (`kernel/effect_bindings.rs`,
/// `step_effects.rs::set_value`): `set` evaluates a list or record and reads a string as a binding, a template or
/// itself; `emit` resolves its payload dropping unset bindings; every other effect resolves the value as a template.
fn kernel_operand(kind: &str, written: &SExpression, ctx: &orbital_core_eval::evaluator::context::BindingContext) -> Result<orbital_core_base::value::Value, String> {
    use orbital_core_base::value::Value as V;
    use orbital_core_eval::evaluator::context::BindingContext;
    let value = V::from(written.clone());
    let text = |error: orbital_core_base::error::EvalError| orbital_core_base::error::OrbitalError::Eval(error).to_string();
    match kind {
        "set" => match &value {
            V::Array(_) | V::Object(_) => eval_expr(written, ctx).map_err(|error| error.to_string()),
            V::String(s) if BindingContext::binding_token_end(s, 0) == Some(s.len()) => Ok(ctx.resolve_binding(s).unwrap_or(V::Null)),
            V::String(s) if s.starts_with('@') => Ok(ctx.resolve_value(&value).into_owned()),
            _ => Ok(value),
        },
        "emit" => ctx.resolve_data_checked(&value).map(std::borrow::Cow::into_owned).map_err(text),
        _ => ctx.resolve_data_checked(&value).map(std::borrow::Cow::into_owned).map_err(text),
    }
}

/// Whether the kernel's value differs from the evaluated one only where a binding has no value: the evaluator
/// answers null, the kernel's resolver keeps the binding's text or leaves the key out.
fn same_but_for_unset(evaluated: &orbital_core_base::value::Value, kernel: &orbital_core_base::value::Value) -> bool {
    use orbital_core_base::value::Value as V;
    use orbital_core_eval::evaluator::context::BindingContext;
    match (evaluated, kernel) {
        (V::Null, V::String(s)) => BindingContext::binding_token_end(s, 0) == Some(s.len()),
        (V::Array(a), V::Array(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_but_for_unset(x, y)),
        (V::Object(a), V::Object(b)) => {
            b.keys().all(|key| a.contains_key(key))
                && a.iter().all(|(key, x)| match b.get(key) {
                    Some(y) => same_but_for_unset(x, y),
                    None => x.is_null(),
                })
        }
        (a, b) => a == b,
    }
}

#[test]
#[ignore = "tool: ORB_LOWER_CORPUS=<orb lower output lines> compares each effect operand's step value with the kernel's resolution"]
fn effect_operands_of_a_corpus() {
    use crate::step_runner::{run, step_of};
    use orbital_core_base::ir::expr::OirBindingRoot;
    preflight().expect("the installed language data must load");
    let path = std::env::var("ORB_LOWER_CORPUS").expect("ORB_LOWER_CORPUS");
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("corpus file"));
    // The roots a step always has, each holding no field: a missing key is null, as in a running kernel.
    let empty = || orbital_core_base::value::Value::object(orbital_core_base::value::ValueMap::default());
    let ctx = BindingContextBuilder::new().entity(empty()).payload(empty()).config(empty()).user(empty()).build();
    let mut seen = BTreeSet::new();
    // kind -> [agree, both fail, reads a local, no step form, drift, differs only on unset bindings]
    let mut table: BTreeMap<String, [usize; 6]> = BTreeMap::new();
    let mut samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut no_operands: BTreeMap<String, usize> = BTreeMap::new();
    for line in file.lines() {
        let entry: Value = orbital_core_base::json_value::from_json_str(&line.expect("line")).expect("json line");
        let mut found = Vec::new();
        expressions(&entry["orb"], false, &mut found);
        for raw in found {
            if !seen.insert(raw.to_string()) {
                continue;
            }
            let expr: SExpression = from_json_value(&raw).expect("an expression");
            let Ok(lowered) = lower_sexpr(&expr) else { continue };
            let mut effects = Vec::new();
            effects_in(&lowered, &mut effects);
            for effect in effects {
                let kind = effect.kind().to_string();
                let children = effect.child_exprs();
                if children.is_empty() {
                    *no_operands.entry(kind.clone()).or_default() += 1;
                }
                for child in children {
                    let row = table.entry(kind.clone()).or_default();
                    if child.collect_bindings().iter().any(|b| matches!(b.root, OirBindingRoot::Local(_))) {
                        row[2] += 1;
                        continue;
                    }
                    let Some(written) = child.to_sexpression() else {
                        row[3] += 1;
                        continue;
                    };
                    let stepped = match step_of(&written).and_then(|module| run(&module, &ctx)) {
                        Ok(ran) => ran,
                        Err(_) => {
                            row[3] += 1;
                            continue;
                        }
                    };
                    let kernel = kernel_operand(&kind, &written, &ctx);
                    match (&stepped, &kernel) {
                        (Ok(a), Ok(b)) if a == b => row[0] += 1,
                        (Err(_), Err(_)) => row[1] += 1,
                        (Ok(a), Ok(b)) if same_but_for_unset(a, b) => {
                            row[5] += 1;
                            let list = samples.entry(format!("unset:{kind}")).or_default();
                            if list.len() < 3 {
                                list.push(format!("{}\n    step   {a:?}\n    kernel {b:?}", serde_json::to_string(&written).unwrap_or_default()));
                            }
                        }
                        _ => {
                            row[4] += 1;
                            let list = samples.entry(kind.clone()).or_default();
                            if list.len() < 6 {
                                list.push(format!("{}\n    step   {stepped:?}\n    kernel {kernel:?}", serde_json::to_string(&written).unwrap_or_default()));
                            }
                        }
                    }
                }
            }
        }
    }
    let mut total = [0usize; 6];
    for row in table.values() {
        for (sum, n) in total.iter_mut().zip(row) {
            *sum += n;
        }
    }
    println!("OPERANDS kind\tagree\tboth_fail\treads_local\tno_step_form\tdrift\tunset_only");
    println!("OPERANDS TOTAL\t{}\t{}\t{}\t{}\t{}\t{}", total[0], total[1], total[2], total[3], total[4], total[5]);
    for (kind, [agree, both, local, none, drift, unset]) in &table {
        println!("OPERANDS {kind}\t{agree}\t{both}\t{local}\t{none}\t{drift}\t{unset}");
    }
    for (kind, count) in &no_operands {
        println!("NO_OPERANDS {kind}\t{count}");
    }
    for (kind, list) in &samples {
        for sample in list {
            println!("DRIFT {kind} {sample}");
        }
    }
}

/// Every lambda in `expr` whose body reads `@item` or `@index` without naming it as a parameter (its own or an
/// enclosing lambda's), as the operator that takes it and the name read.
fn implicit_reads(expr: &orbital_core_base::ir::expr::OirExpr, head: &str, declared: &[String], out: &mut Vec<(String, String)>) {
    use orbital_core_base::ir::expr::{OirBindingRoot, OirExpr};
    match expr {
        OirExpr::Lambda { params, body } => {
            let mut inner = declared.to_vec();
            inner.extend(params.iter().cloned());
            implicit_reads(body, head, &inner, out);
        }
        OirExpr::Binding { binding } => {
            if let OirBindingRoot::Local(name) = &binding.root {
                if (name == "item" || name == "index") && !declared.contains(name) {
                    out.push((head.to_string(), name.clone()));
                }
            }
        }
        OirExpr::Call { function, args } => {
            let bound: Vec<String> = match (function.as_str(), args.first()) {
                ("let", Some(OirExpr::Array { items })) => items
                    .iter()
                    .filter_map(|pair| match pair {
                        OirExpr::Array { items } => match items.first() {
                            Some(OirExpr::String { value }) => Some(value.clone()),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let mut inner = declared.to_vec();
            inner.extend(bound);
            for arg in args {
                implicit_reads(arg, function, &inner, out);
            }
        }
        other => {
            for child in other.child_exprs() {
                implicit_reads(child, head, declared, out);
            }
        }
    }
}

#[test]
#[ignore = "tool: ORB_LOWER_CORPUS=<orb lower output lines> lists lambdas that read @item or @index without declaring it"]
fn implicit_callback_names_of_a_corpus() {
    preflight().expect("the installed language data must load");
    let path = std::env::var("ORB_LOWER_CORPUS").expect("ORB_LOWER_CORPUS");
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("corpus file"));
    let mut by_operator: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut programs: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();
    for line in file.lines() {
        let entry: Value = orbital_core_base::json_value::from_json_str(&line.expect("line")).expect("json line");
        let mut found = Vec::new();
        expressions(&entry["orb"], false, &mut found);
        for raw in found {
            let expr: SExpression = from_json_value(&raw).expect("an expression");
            let Ok(lowered) = lower_sexpr(&expr) else { continue };
            let mut reads = Vec::new();
            implicit_reads(&lowered, "", &[], &mut reads);
            if reads.is_empty() {
                continue;
            }
            *programs.entry(entry["path"].as_str().unwrap_or("?").to_string()).or_default() += reads.len();
            if samples.len() < 5 {
                samples.push(raw.to_string());
            }
            for key in reads {
                *by_operator.entry(key).or_default() += 1;
            }
        }
    }
    println!("IMPLICIT reads={} programs={}", by_operator.values().sum::<usize>(), programs.len());
    for ((operator, name), count) in &by_operator {
        println!("IMPLICIT_BY {operator}\t@{name}\t{count}");
    }
    for (program, count) in &programs {
        println!("IMPLICIT_IN {count}\t{program}");
    }
    for sample in &samples {
        println!("IMPLICIT_SAMPLE {sample}");
    }
}

/// Every operator call written inside a render pattern: the kernel evaluates it when it resolves the pattern, so its
/// callbacks are bound by the operator, not by the renderer. A bare `fn` is a render-time lambda and is not entered.
fn calls_in_pattern<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    match node {
        Value::Array(items) => match items.first().and_then(Value::as_str) {
            Some("fn") => {}
            Some(head) if orbital_core_eval::evaluator::operators::get_operator(head).is_some() => out.push(node),
            _ => items.iter().for_each(|item| calls_in_pattern(item, out)),
        },
        Value::Object(map) => map.values().for_each(|value| calls_in_pattern(value, out)),
        _ => {}
    }
}

#[test]
#[ignore = "tool: ORB_LOWER_CORPUS=<orb lower output lines> lists render-pattern operator calls whose lambdas read @item or @index undeclared"]
fn implicit_callback_names_in_render_patterns_of_a_corpus() {
    preflight().expect("the installed language data must load");
    let path = std::env::var("ORB_LOWER_CORPUS").expect("ORB_LOWER_CORPUS");
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("corpus file"));
    let mut programs: BTreeMap<String, usize> = BTreeMap::new();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();
    for line in file.lines() {
        let entry: Value = orbital_core_base::json_value::from_json_str(&line.expect("line")).expect("json line");
        let mut found = Vec::new();
        expressions(&entry["orb"], false, &mut found);
        let mut renders = Vec::new();
        fn render_effects<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
            if let Value::Array(items) = node {
                if items.first().and_then(Value::as_str) == Some("render-ui") {
                    out.extend(items.iter().skip(2));
                } else {
                    items.iter().for_each(|item| render_effects(item, out));
                }
            }
        }
        found.iter().for_each(|raw| render_effects(raw, &mut renders));
        let mut calls = Vec::new();
        renders.iter().for_each(|content| calls_in_pattern(content, &mut calls));
        for call in calls {
            let Ok(expr) = from_json_value::<SExpression>(call) else { continue };
            let Ok(lowered) = lower_sexpr(&expr) else { continue };
            let mut reads = Vec::new();
            implicit_reads(&lowered, "", &[], &mut reads);
            if reads.is_empty() {
                continue;
            }
            *programs.entry(entry["path"].as_str().unwrap_or("?").to_string()).or_default() += reads.len();
            for (_, name) in reads {
                *names.entry(name).or_default() += 1;
            }
            if samples.len() < 4 {
                samples.push(call.to_string());
            }
        }
    }
    println!("RENDER_IMPLICIT reads={} programs={}", names.values().sum::<usize>(), programs.len());
    for (name, count) in &names {
        println!("RENDER_IMPLICIT_NAME @{name}\t{count}");
    }
    for (program, count) in &programs {
        println!("RENDER_IMPLICIT_IN {count}\t{program}");
    }
    for sample in &samples {
        println!("RENDER_IMPLICIT_SAMPLE {sample}");
    }
}
