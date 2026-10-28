//! G6 Game (Wave 1b): grid pathfinding operators (`path/*`).
//!
//! Grid cells are `{x,y}` integer objects. The open-set is a linear-scanned
//! array (NOT a heap) with a deterministic tie-break (lower f, lower h, earliest
//! insertion) so Rust and JS produce identical paths.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::vector::{read_fields, whole, POINT};
use super::*;
use orbital_core_base::value::ValueMap;
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Cell {
    x: i64,
    y: i64,
}

fn cell_value(c: Cell) -> Value {
    let mut m = ValueMap::default();
    m.insert("x".to_string(), Value::Number(c.x as f64));
    m.insert("y".to_string(), Value::Number(c.y as f64));
    Value::object(m)
}

fn read_cell(op: &str, param: &str, v: &Value) -> Result<Cell, EvalError> {
    let [x, y] = read_fields(op, param, v, POINT)?;
    Ok(Cell {
        x: whole(op, &format!("{param}.x"), x)?,
        y: whole(op, &format!("{param}.y"), y)?,
    })
}

fn eval_cell(
    op: &str,
    param: &str,
    arg: &SExpression,
    ctx: &BindingContext,
) -> Result<Cell, OrbitalError> {
    Ok(read_cell(op, param, &eval_expr(arg, ctx)?)?)
}

fn eval_extent(
    op: &str,
    param: &str,
    arg: &SExpression,
    ctx: &BindingContext,
) -> Result<i64, OrbitalError> {
    Ok(whole(op, param, eval_expr(arg, ctx)?.number()?)?)
}

fn key(c: Cell) -> String {
    format!("{},{}", c.x, c.y)
}

fn read_blocked(op: &str, v: &Value) -> Result<HashSet<String>, EvalError> {
    let items = v.as_array().ok_or_else(|| v.kind_error("array"))?;
    items
        .iter()
        .map(|item| read_cell(op, "blocked", item).map(key))
        .collect()
}

fn neighbor_offsets(diagonal: bool) -> Vec<(i64, i64)> {
    let mut offs = vec![(0, -1), (1, 0), (0, 1), (-1, 0)];
    if diagonal {
        offs.extend_from_slice(&[(1, -1), (1, 1), (-1, 1), (-1, -1)]);
    }
    offs
}

fn arg_bool(args: &[SExpression], i: usize, ctx: &BindingContext) -> Result<bool, OrbitalError> {
    match args.get(i) {
        Some(a) => Ok(eval_expr(a, ctx)?.boolean()?),
        None => Ok(false),
    }
}

pub struct PathAstarOp;
impl Operator for PathAstarOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "path/astar";
        require_arity!(args, OP, >= 5);
        let start = eval_cell(OP, "start", &args[0], ctx)?;
        let goal = eval_cell(OP, "goal", &args[1], ctx)?;
        let blocked = read_blocked(OP, &eval_expr(&args[2], ctx)?)?;
        let w = eval_extent(OP, "w", &args[3], ctx)?;
        let h = eval_extent(OP, "h", &args[4], ctx)?;
        let diagonal = arg_bool(args, 5, ctx)?;
        let offs = neighbor_offsets(diagonal);

        let heuristic = |c: Cell| -> i64 {
            let dx = (c.x - goal.x).abs();
            let dy = (c.y - goal.y).abs();
            if diagonal {
                dx.max(dy)
            } else {
                dx + dy
            }
        };

        // Open node: cell, g, h, f, insertion order.
        struct Node {
            cell: Cell,
            g: i64,
            h: i64,
            f: i64,
            order: u64,
        }

        let mut counter: u64 = 0;
        let mut open: Vec<Node> = Vec::new();
        let mut came_from: std::collections::HashMap<String, Cell> =
            std::collections::HashMap::new();
        let mut g_score: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        let mut closed: HashSet<String> = HashSet::new();

        let h0 = heuristic(start);
        open.push(Node {
            cell: start,
            g: 0,
            h: h0,
            f: h0,
            order: counter,
        });
        counter += 1;
        g_score.insert(key(start), 0);

        while !open.is_empty() {
            // Pick lowest f, then lowest h, then earliest insertion.
            let mut best = 0usize;
            for i in 1..open.len() {
                let n = &open[i];
                let b = &open[best];
                if n.f < b.f
                    || (n.f == b.f && n.h < b.h)
                    || (n.f == b.f && n.h == b.h && n.order < b.order)
                {
                    best = i;
                }
            }
            let current = open.swap_remove(best);

            if current.cell == goal {
                // Reconstruct.
                let mut path = vec![goal];
                let mut k = key(goal);
                while let Some(&prev) = came_from.get(&k) {
                    path.push(prev);
                    k = key(prev);
                }
                path.reverse();
                return Ok(Value::Array(path.into_iter().map(cell_value).collect()));
            }

            closed.insert(key(current.cell));

            for &(dx, dy) in &offs {
                let nb = Cell {
                    x: current.cell.x + dx,
                    y: current.cell.y + dy,
                };
                if nb.x < 0 || nb.x >= w || nb.y < 0 || nb.y >= h {
                    continue;
                }
                let nk = key(nb);
                if blocked.contains(&nk) || closed.contains(&nk) {
                    continue;
                }
                let tentative = current.g + 1;
                let known = g_score.get(&nk).copied();
                if known.is_none() || tentative < known.unwrap() {
                    came_from.insert(nk.clone(), current.cell);
                    g_score.insert(nk.clone(), tentative);
                    let hh = heuristic(nb);
                    open.push(Node {
                        cell: nb,
                        g: tentative,
                        h: hh,
                        f: tentative + hh,
                        order: counter,
                    });
                    counter += 1;
                }
            }
        }

        Ok(Value::array(vec![]))
    }
}

pub struct PathReachableOp;
impl Operator for PathReachableOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "path/reachable";
        require_arity!(args, OP, >= 5);
        let start = eval_cell(OP, "start", &args[0], ctx)?;
        let steps = eval_extent(OP, "steps", &args[1], ctx)?;
        let blocked = read_blocked(OP, &eval_expr(&args[2], ctx)?)?;
        let w = eval_extent(OP, "w", &args[3], ctx)?;
        let h = eval_extent(OP, "h", &args[4], ctx)?;
        let diagonal = arg_bool(args, 5, ctx)?;
        let offs = neighbor_offsets(diagonal);

        let mut visited: HashSet<String> = HashSet::new();
        let mut result: Vec<Cell> = Vec::new();
        // BFS frontier with distance.
        let mut frontier: Vec<(Cell, i64)> = vec![(start, 0)];
        visited.insert(key(start));
        result.push(start);

        let mut idx = 0;
        while idx < frontier.len() {
            let (cell, dist) = frontier[idx];
            idx += 1;
            if dist >= steps {
                continue;
            }
            for &(dx, dy) in &offs {
                let nb = Cell {
                    x: cell.x + dx,
                    y: cell.y + dy,
                };
                if nb.x < 0 || nb.x >= w || nb.y < 0 || nb.y >= h {
                    continue;
                }
                let nk = key(nb);
                if blocked.contains(&nk) || visited.contains(&nk) {
                    continue;
                }
                visited.insert(nk);
                result.push(nb);
                frontier.push((nb, dist + 1));
            }
        }

        // Sort row-major: y asc, then x asc.
        result.sort_by(|a, b| a.y.cmp(&b.y).then(a.x.cmp(&b.x)));
        Ok(Value::Array(result.into_iter().map(cell_value).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(form: &str) -> EvalResult {
        let expr: SExpression = serde_json::from_str(form).expect("s-expression");
        eval_expr(&expr, &BindingContext::default())
    }

    fn error_text(form: &str) -> String {
        format!("{:?}", eval(form).expect_err("expected a typed error"))
    }

    #[test]
    fn declared_arity_and_strict_cells() {
        let open = eval(r#"["path/reachable", {"x":1,"y":1}, 1, [], 3, 3, false]"#).unwrap();
        assert_eq!(open.as_array().unwrap().len(), 5);
        assert_eq!(
            eval(r#"["path/astar", {"x":0,"y":0}, {"x":2,"y":0}, [], 3, 1]"#)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(matches!(
            eval(r#"["path/reachable", {"x":1,"y":1}, 1, [], 3]"#),
            Err(OrbitalError::Eval(EvalError::ArityMismatch { .. }))
        ));
        assert!(
            error_text(r#"["path/astar", null, {"x":2,"y":0}, [], 3, 1]"#)
                .contains("argument `start`")
        );
        assert!(
            error_text(r#"["path/astar", {"x":0,"y":0}, {"x":2,"y":0}, null, 3, 1]"#)
                .contains("array")
        );
        assert!(
            error_text(r#"["path/astar", {"x":0,"y":0}, {"x":2,"y":0}, [{"x":1}], 3, 1]"#)
                .contains("missing field")
        );
        assert!(
            error_text(r#"["path/reachable", {"x":1,"y":1}, 1, [], 3, 3, null]"#)
                .contains("boolean")
        );
    }
}
