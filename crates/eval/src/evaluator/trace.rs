//! Evaluation trace: every node's entry, value and exit by structural path, lambda re-entries and the
//! arguments an operator never ran. Off by default: `eval_expr` pays one relaxed atomic load.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::context::BindingContext;
use super::operators::{get_operator, EvalResult};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::value::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One step of a path from the traced expression's root to a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSeg {
    Index(usize),
    Key(String),
}

pub type NodePath = Vec<PathSeg>;

#[derive(Debug, Clone)]
pub enum TraceStep {
    Enter {
        path: NodePath,
    },
    Iter {
        path: NodePath,
    },
    Exit {
        path: NodePath,
        outcome: Result<Value, String>,
    },
    Skip {
        path: NodePath,
    },
}

struct Collector {
    paths: HashMap<*const SExpression, NodePath>,
    entered: Vec<NodePath>,
    steps: Vec<TraceStep>,
}

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

orbital_core_base::kernel_local! {
    static COLLECTOR: RefCell<Option<Collector>> = const { RefCell::new(None) };
}

#[inline]
pub(super) fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed) != 0
}

fn index_tree(
    node: &SExpression,
    path: &NodePath,
    paths: &mut HashMap<*const SExpression, NodePath>,
) {
    paths.insert(node as *const SExpression, path.clone());
    match node {
        SExpression::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                let mut p = path.clone();
                p.push(PathSeg::Index(i));
                index_tree(child, &p, paths);
            }
        }
        SExpression::Object(map) => {
            for (k, child) in map {
                let mut p = path.clone();
                p.push(PathSeg::Key(k.clone()));
                index_tree(child, &p, paths);
            }
        }
        _ => {}
    }
}

/// A lambda body cloned out of `original` is the same node under another address: index the clone under
/// the original's path, so its evaluations trace as iterations of the original body.
pub(super) fn adopt(clone: &SExpression, original: &SExpression) {
    if !is_active() {
        return;
    }
    COLLECTOR.with(|slot| {
        if let Some(collector) = slot.borrow_mut().as_mut() {
            if let Some(path) = collector
                .paths
                .get(&(original as *const SExpression))
                .cloned()
            {
                index_tree(clone, &path, &mut collector.paths);
            }
        }
    });
}

fn with_collector<T>(f: impl FnOnce(&mut Collector) -> T) -> Option<T> {
    COLLECTOR.with(|slot| slot.borrow_mut().as_mut().map(f))
}

fn ran_under(entered: &[NodePath], path: &NodePath) -> bool {
    entered.iter().any(|e| e.starts_with(path))
}

pub(super) fn step(
    expr: &SExpression,
    ctx: &BindingContext,
    run: fn(&SExpression, &BindingContext) -> EvalResult,
) -> EvalResult {
    let Some(path) =
        with_collector(|c| c.paths.get(&(expr as *const SExpression)).cloned()).flatten()
    else {
        return run(expr, ctx);
    };
    with_collector(|c| {
        if c.entered.contains(&path) {
            c.steps.push(TraceStep::Iter { path: path.clone() });
        } else {
            c.entered.push(path.clone());
        }
        c.steps.push(TraceStep::Enter { path: path.clone() });
    });
    let result = run(expr, ctx);
    with_collector(|c| {
        c.steps.push(TraceStep::Exit {
            path: path.clone(),
            outcome: match &result {
                Ok(value) => Ok(value.clone()),
                Err(err) => Err(err.to_string()),
            },
        });
        if result.is_ok() {
            if let SExpression::Array(items) = expr {
                let is_call = matches!(items.first(), Some(SExpression::String(op)) if get_operator(op).is_some());
                if is_call {
                    for i in 1..items.len() {
                        let mut child = path.clone();
                        child.push(PathSeg::Index(i));
                        if !ran_under(&c.entered, &child) {
                            c.steps.push(TraceStep::Skip { path: child });
                        }
                    }
                }
            }
        }
    });
    result
}

struct Active;

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
        COLLECTOR.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Evaluate `expr`, recording its trace. Never panics on an evaluation failure: the result and the steps up to it come back.
pub fn evaluate_traced(expr: &SExpression, ctx: &BindingContext) -> (EvalResult, Vec<TraceStep>) {
    let mut paths = HashMap::new();
    index_tree(expr, &Vec::new(), &mut paths);
    COLLECTOR.with(|slot| {
        *slot.borrow_mut() = Some(Collector {
            paths,
            entered: Vec::new(),
            steps: Vec::new(),
        })
    });
    ACTIVE.fetch_add(1, Ordering::Relaxed);
    let guard = Active;
    let result = super::operators::eval_expr(expr, ctx);
    let steps = with_collector(|c| std::mem::take(&mut c.steps)).unwrap_or_default();
    drop(guard);
    (result, steps)
}
