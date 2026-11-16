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
        let entry: Value = serde_json::from_str(&line.expect("line")).expect("json line");
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
        let entry: Value = serde_json::from_str(&line.expect("line")).expect("json line");
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
