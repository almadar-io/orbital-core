//! `persist batch`: one effect that writes many rows.
//!
//! IR: `["persist", "batch", <ops>, { "emit": … }?]`. `<ops>` evaluates to an
//! array whose elements are `["create", E, data]`, `["update", E, id, data]` or
//! `["delete", E, id]`. This module is the one place that reads the STATIC shape
//! of that argument: which op tuples are written out literally, and so which
//! entities the batch is known to touch. Placement, validation and codegen all
//! ask here.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::runs_on::Residence;
use crate::schema::sexpr::SExpression;

/// The three op actions a batch element may carry.
pub const BATCH_ACTIONS: [&str; 3] = ["create", "update", "delete"];

/// Element count (head included) an op tuple of `action` must have, or `None`
/// for an unknown action.
pub fn batch_op_arity(action: &str) -> Option<usize> {
    match action {
        "create" | "delete" => Some(3),
        "update" => Some(4),
        _ => None,
    }
}

/// How the `<ops>` argument is written.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchOps<'a> {
    /// A literal list, each element authored in place (a well-formed element is an op tuple).
    Literal(&'a [SExpression]),
    /// `(array/map <coll> (fn r <tuple>))` whose lambda body is one op tuple.
    Mapped(&'a [SExpression]),
    /// Any other expression: only its array-ness is checkable.
    Expression,
}

fn as_items(expr: &SExpression) -> Option<&[SExpression]> {
    match expr {
        SExpression::Array(items) => Some(items.as_slice()),
        _ => None,
    }
}

/// An array whose head is a batch action is an op tuple, not an expression.
pub fn is_op_tuple(expr: &SExpression) -> bool {
    as_items(expr)
        .and_then(|items| items.first())
        .and_then(|h| h.as_str())
        .is_some_and(|h| BATCH_ACTIONS.contains(&h))
}

/// The lambda body of `["fn", <params>, <body>]` / `["lambda", …]`.
pub fn lambda_body(expr: &SExpression) -> Option<&SExpression> {
    let items = as_items(expr)?;
    match items.first()?.as_str()? {
        "fn" | "lambda" if items.len() >= 3 => items.last(),
        _ => None,
    }
}

/// Read the static shape of a batch's `<ops>` argument.
pub fn classify_ops(ops: &SExpression) -> BatchOps<'_> {
    let Some(items) = as_items(ops) else {
        return BatchOps::Expression;
    };
    match items.first() {
        None => BatchOps::Literal(items),
        Some(SExpression::String(head)) if head == "list" => BatchOps::Literal(&items[1..]),
        Some(SExpression::String(_)) => match mapped_body(ops) {
            Some(body) if is_op_tuple(body) => BatchOps::Mapped(as_items(body).unwrap_or_default()),
            _ => BatchOps::Expression,
        },
        Some(_) => BatchOps::Literal(items),
    }
}

/// The lambda body of `(array/map <coll> <lambda>)`: what each element becomes.
/// `Some` for any such call, whether or not the body is a recognised op tuple.
pub fn mapped_body(ops: &SExpression) -> Option<&SExpression> {
    let items = as_items(ops)?;
    match items.first()?.as_str()? {
        "array/map" if items.len() == 3 => lambda_body(&items[2]),
        _ => None,
    }
}

/// The entity a statically-readable op tuple names (a literal, never a binding).
pub fn op_entity(tuple: &[SExpression]) -> Option<&str> {
    tuple
        .get(1)
        .and_then(|e| e.as_str())
        .filter(|e| !e.starts_with('@'))
}

/// Every distinct entity the batch's statically-readable ops name, in first-seen order.
pub fn batch_entities(ops: &SExpression) -> Vec<&str> {
    let tuples: Vec<&[SExpression]> = match classify_ops(ops) {
        BatchOps::Literal(elements) => elements
            .iter()
            .filter(|e| is_op_tuple(e))
            .filter_map(as_items)
            .collect(),
        BatchOps::Mapped(t) => vec![t],
        BatchOps::Expression => Vec::new(),
    };
    let mut out: Vec<&str> = Vec::new();
    for entity in tuples.into_iter().filter_map(op_entity) {
        if !out.contains(&entity) {
            out.push(entity);
        }
    }
    out
}

/// Every entity a batch's `<ops>` can create, update or upsert, following the list builders a program writes a batch
/// with (`array/concat`, `array/prepend`, `array/append`, `array/filter`, `array/map` over an op template,
/// `array/flatten` of lists, `array/take` (nothing for a literal 0), `if` branches). `None` when some element comes from something it cannot follow (a binding, another call, an op whose
/// entity is not a literal): the batch may then write anything.
pub fn batch_written_entities(ops: &SExpression) -> Option<std::collections::BTreeSet<&str>> {
    fn element<'a>(e: &'a SExpression, out: &mut std::collections::BTreeSet<&'a str>) -> Option<()> {
        if is_op_tuple(e) {
            let tuple = as_items(e)?;
            let entity = op_entity(tuple)?;
            if tuple.first()?.as_str()? != "delete" {
                out.insert(entity);
            }
            return Some(());
        }
        let items = as_items(e)?;
        match items.first()?.as_str()? {
            "if" if items.len() == 4 => {
                element(&items[2], out)?;
                element(&items[3], out)
            }
            _ => None,
        }
    }
    fn list<'a>(e: &'a SExpression, out: &mut std::collections::BTreeSet<&'a str>) -> Option<()> {
        let items = as_items(e)?;
        match items.first() {
            None => Some(()),
            Some(SExpression::String(head)) => match head.as_str() {
                "list" => items[1..].iter().try_for_each(|x| element(x, out)),
                "array/concat" => items[1..].iter().try_for_each(|x| list(x, out)),
                "array/prepend" | "array/append" if items.len() == 3 => {
                    list(&items[1], out)?;
                    element(&items[2], out)
                }
                "array/filter" if items.len() == 3 => list(&items[1], out),
                "array/map" if items.len() == 3 => element(lambda_body(&items[2])?, out),
                "array/flatten" if items.len() == 2 => {
                    let lists = as_items(&items[1])?;
                    let lists = match lists.first().and_then(SExpression::as_str) {
                        Some("list") => &lists[1..],
                        Some(_) => return None,
                        None => lists,
                    };
                    lists.iter().try_for_each(|x| list(x, out))
                }
                "array/take" if items.len() == 3 => match &items[2] {
                    SExpression::Number(n) if *n == 0.0 => Some(()),
                    _ => list(&items[1], out),
                },
                "if" if items.len() == 4 => {
                    list(&items[2], out)?;
                    list(&items[3], out)
                }
                _ => None,
            },
            Some(_) => items.iter().try_for_each(|x| element(x, out)),
        }
    }
    let mut out = std::collections::BTreeSet::new();
    list(ops, &mut out)?;
    Some(out)
}

/// One statically-readable write of a batch: what a single `persist` of that op
/// would do. `data` is `None` for `delete`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BatchWrite<'a> {
    pub action: &'a str,
    pub entity: &'a str,
    pub data: Option<&'a SExpression>,
}

/// Every statically-readable write of the batch's `<ops>` argument, so analyses
/// that look at what `persist` writes read a batch the way they read a single op.
pub fn batch_writes(ops: &SExpression) -> Vec<BatchWrite<'_>> {
    let tuples: Vec<&[SExpression]> = match classify_ops(ops) {
        BatchOps::Literal(elements) => elements
            .iter()
            .filter(|e| is_op_tuple(e))
            .filter_map(as_items)
            .collect(),
        BatchOps::Mapped(t) => vec![t],
        BatchOps::Expression => Vec::new(),
    };
    tuples
        .into_iter()
        .filter_map(|t| {
            let action = t.first()?.as_str()?;
            let entity = op_entity(t)?;
            let data = match action {
                "create" => t.get(2),
                "update" => t.get(3),
                _ => None,
            };
            Some(BatchWrite {
                action,
                entity,
                data,
            })
        })
        .collect()
}

/// `args` of a `persist` call (head stripped): the `<ops>` argument when the
/// action is `batch`.
pub fn batch_ops_arg(args: &[SExpression]) -> Option<&SExpression> {
    match args.first().and_then(|a| a.as_str()) {
        Some("batch") => args.get(1),
        _ => None,
    }
}

/// Where a batch's statically-readable ops live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchPlacement {
    /// Every named entity is browser-stored.
    Client,
    /// No named entity is browser-stored.
    Server,
    /// Browser-stored and server-stored entities in one batch: no single executor holds both.
    Mixed,
    /// No op names an entity statically.
    Undetermined,
}

/// Placement of a batch's `<ops>` argument under `residence`.
pub fn batch_placement(ops: &SExpression, residence: &Residence) -> BatchPlacement {
    let entities = batch_entities(ops);
    let client = entities.iter().filter(|e| residence.is_client(e)).count();
    match (entities.len(), client) {
        (0, _) => BatchPlacement::Undetermined,
        (n, c) if c == n => BatchPlacement::Client,
        (_, 0) => BatchPlacement::Server,
        _ => BatchPlacement::Mixed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ops(v: serde_json::Value) -> SExpression {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_flattened_batch_of_branches_names_what_each_branch_can_write() {
        // The std-name-claim shape.
        let built = ops(json!(["array/flatten", [
            [["create", "NameLock", { "id": "n" }]],
            ["if", "@entity.held", [["delete", "NameLock", "@entity.held"]], ["array/take", [["delete", "NameLock", ""]], 0.0]],
            ["if", "@entity.heldRow", [["update", "NameHolder", "h", { "name": "n" }]], [["create", "NameHolder", { "id": "h" }]]]
        ]]));
        assert_eq!(batch_written_entities(&built).unwrap().into_iter().collect::<Vec<_>>(), vec!["NameHolder", "NameLock"]);
        let listed = ops(json!(["array/flatten", ["list", [["create", "A", {}]], [["update", "B", "b", {}]]]]));
        assert_eq!(batch_written_entities(&listed).unwrap().into_iter().collect::<Vec<_>>(), vec!["A", "B"]);
    }

    #[test]
    fn a_take_of_none_writes_nothing_and_any_other_take_is_a_subset_of_its_list() {
        assert!(batch_written_entities(&ops(json!(["array/take", [["create", "A", {}]], 0.0]))).unwrap().is_empty());
        assert_eq!(batch_written_entities(&ops(json!(["array/take", [["create", "A", {}]], 1.0]))).unwrap().into_iter().collect::<Vec<_>>(), vec!["A"]);
        assert_eq!(batch_written_entities(&ops(json!(["array/take", [["create", "A", {}]], "@entity.n"]))).unwrap().into_iter().collect::<Vec<_>>(), vec!["A"]);
    }

    #[test]
    fn control_a_flatten_or_take_over_something_it_cannot_follow_may_write_anything() {
        assert!(batch_written_entities(&ops(json!(["array/flatten", "@entity.lists"]))).is_none());
        assert!(batch_written_entities(&ops(json!(["array/flatten", [[["create", "A", {}]], "@payload.more"]]))).is_none());
        assert!(batch_written_entities(&ops(json!(["array/take", "@payload.ops", 0.0]))).unwrap().is_empty(), "taking none of anything is nothing");
        assert!(batch_written_entities(&ops(json!(["array/take", "@payload.ops", 2.0]))).is_none());
        assert!(batch_written_entities(&ops(json!(["if", "@c", [["create", "A", {}]], "@payload.ops"]))).is_none());
    }

    #[test]
    fn a_built_batch_names_the_entities_its_op_templates_write() {
        // The std-graph shape: a literal create, concatenated with a mapped update, behind a filter and a prepend.
        let built = ops(json!(["array/concat",
            [["create", "GraphEdge", { "id": "e" }]],
            ["array/map", ["array/filter", "@entity.pending", ["fn", "u", true]], ["fn", "u", ["update", "GraphNode", "@u.id", { "depth": 1 }]]],
            ["array/prepend", ["array/map", "@entity.edges", ["fn", "e", ["delete", "GraphEdge", "@e.id"]]], ["delete", "GraphNode", "n"]]
        ]));
        let written: Vec<&str> = batch_written_entities(&built).unwrap().into_iter().collect();
        assert_eq!(written, vec!["GraphEdge", "GraphNode"]);
        let deletes_only = ops(json!(["array/map", "@entity.edges", ["fn", "e", ["delete", "GraphEdge", "@e.id"]]]));
        assert!(batch_written_entities(&deletes_only).unwrap().is_empty(), "a delete writes no row");
        let branch = ops(json!(["array/map", "@entity.items", ["fn", "i", ["if", "@i.new", ["create", "A", {}], ["update", "B", "@i.id", {}]]]]));
        assert_eq!(batch_written_entities(&branch).unwrap().into_iter().collect::<Vec<_>>(), vec!["A", "B"]);
    }

    #[test]
    fn control_a_batch_it_cannot_follow_is_none() {
        assert!(batch_written_entities(&ops(json!("@payload.ops"))).is_none(), "a binding can hold any op");
        assert!(batch_written_entities(&ops(json!(["array/concat", [["create", "A", {}]], "@payload.ops"]))).is_none());
        assert!(batch_written_entities(&ops(json!(["array/map", "@entity.items", ["fn", "i", "@i.op"]]))).is_none());
        assert!(batch_written_entities(&ops(json!([["create", "@config.target", {}]]))).is_none(), "a bound entity name");
    }

    fn sx(v: serde_json::Value) -> SExpression {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn literal_list_reads_each_tuple() {
        let ops = sx(json!([["create", "A", {}], ["delete", "B", "id"]]));
        match classify_ops(&ops) {
            BatchOps::Literal(e) => assert_eq!(e.len(), 2),
            other => panic!("{other:?}"),
        }
        assert_eq!(batch_entities(&ops), vec!["A", "B"]);
    }

    #[test]
    fn list_marker_and_empty_literal() {
        assert!(matches!(classify_ops(&sx(json!([]))), BatchOps::Literal(t) if t.is_empty()));
        let ops = sx(json!(["list", ["update", "A", "i", {}]]));
        assert_eq!(batch_entities(&ops), vec!["A"]);
    }

    #[test]
    fn array_map_with_literal_tuple_body_is_mapped() {
        for lam in [
            json!(["fn", "r", ["update", "Row", ["object/get", "@r", "id"], {}]]),
            json!([
                "fn",
                ["r"],
                ["update", "Row", ["object/get", "@r", "id"], {}]
            ]),
        ] {
            let ops = sx(json!(["array/map", "@payload.data", lam]));
            assert!(matches!(classify_ops(&ops), BatchOps::Mapped(_)));
            assert_eq!(batch_entities(&ops), vec!["Row"]);
        }
    }

    #[test]
    fn other_expressions_are_opaque() {
        for ops in [
            json!("@payload.ops"),
            json!(["array/map", "@x", ["fn", "r", ["object/get", "@r", "op"]]]),
            json!(["array/filter", "@x", ["fn", "r", true]]),
        ] {
            let ops = sx(ops);
            assert_eq!(classify_ops(&ops), BatchOps::Expression);
            assert!(batch_entities(&ops).is_empty());
        }
    }

    #[test]
    fn binding_entities_are_not_static() {
        let ops = sx(json!([["create", "@config.entity", {}]]));
        assert!(batch_entities(&ops).is_empty());
    }

    #[test]
    fn batch_writes_read_each_op_like_a_single_persist() {
        let ops = sx(
            json!([["create", "A", {"x": 1}], ["update", "B", "id", {"y": 2}], ["delete", "C", "id"]]),
        );
        let writes = batch_writes(&ops);
        let seen: Vec<_> = writes
            .iter()
            .map(|w| (w.action, w.entity, w.data.is_some()))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("create", "A", true),
                ("update", "B", true),
                ("delete", "C", false)
            ]
        );
        let mapped = sx(json!(["array/map", "@x", ["fn", "r", ["update", "Row", "@r", {"s": 1}]]]));
        assert_eq!(batch_writes(&mapped).len(), 1);
        assert!(batch_writes(&sx(json!("@payload.ops"))).is_empty());
    }

    #[test]
    fn arity_table() {
        assert_eq!(batch_op_arity("create"), Some(3));
        assert_eq!(batch_op_arity("update"), Some(4));
        assert_eq!(batch_op_arity("delete"), Some(3));
        assert_eq!(batch_op_arity("upsert"), None);
    }
}
