//! Repeated expression subtrees written once per document (G-CROSS-637). Inside [`to_vec_with_defs`], a list or record
//! of an S-expression that another holder shares is written the first time as `{ "$def": n, "value": … }` and afterwards
//! as `{ "$def": n }`; reading resolves each id against the document's own interning scope. Outside the scope a program
//! is written exactly as before. This is storage only: a document read back equals the plain encoding.

use std::cell::RefCell;
use std::collections::HashMap;

/// Key of a repeated-subtree leaf.
pub const DEF_MARKER: &str = "$def";

struct WriteScope {
    ids: HashMap<usize, u64>,
    next: u64,
}

thread_local! {
    static WRITING: RefCell<Option<WriteScope>> = const { RefCell::new(None) };
}

struct Close;

impl Drop for Close {
    fn drop(&mut self) {
        WRITING.with(|w| w.borrow_mut().take());
    }
}

/// `value` as JSON with every shared expression subtree after its first occurrence written as a `$def` reference.
pub fn to_vec_with_defs<T: serde::Serialize + ?Sized>(value: &T) -> serde_json::Result<Vec<u8>> {
    WRITING.with(|w| *w.borrow_mut() = Some(WriteScope { ids: HashMap::new(), next: 0 }));
    let _close = Close;
    serde_json::to_vec(value)
}

pub(crate) enum DefWrite {
    Plain,
    First(u64),
    Reference(u64),
}

/// How the node at `at` is written: plain outside a scope or when nothing else holds it, else first or by reference.
pub(crate) fn decide(at: usize, shared: bool) -> DefWrite {
    if !shared {
        return DefWrite::Plain;
    }
    WRITING.with(|w| match w.borrow_mut().as_mut() {
        None => DefWrite::Plain,
        Some(scope) => match scope.ids.get(&at) {
            Some(id) => DefWrite::Reference(*id),
            None => {
                let id = scope.next;
                scope.next += 1;
                scope.ids.insert(at, id);
                DefWrite::First(id)
            }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::sexpr::SExpression;

    fn repeated() -> SExpression {
        let presets: SExpression = serde_json::from_value(serde_json::json!({ "naqsh": { "fill": "#0d0f1e" }, "glass": { "fill": "#ffffff" } })).unwrap();
        let read = |name: &str| SExpression::Array(vec![SExpression::from("object/get"), presets.clone(), SExpression::from(name)].into());
        SExpression::Array(vec![read("a"), read("b"), read("c")].into())
    }

    #[test]
    fn a_repeated_subtree_is_written_once_and_read_back_equal() {
        let tree = repeated();
        let plain = serde_json::to_vec(&tree).unwrap();
        let with_defs = to_vec_with_defs(&tree).unwrap();
        assert!(with_defs.len() < plain.len(), "{} >= {}", with_defs.len(), plain.len());
        assert_eq!(String::from_utf8_lossy(&with_defs).matches("#0d0f1e").count(), 1);
        let back: SExpression = serde_json::from_slice(&with_defs).unwrap();
        assert_eq!(serde_json::to_vec(&back).unwrap(), plain);
    }

    #[test]
    fn control_outside_the_scope_a_program_is_written_as_before() {
        let tree = repeated();
        let plain = serde_json::to_string(&tree).unwrap();
        assert!(!plain.contains(DEF_MARKER));
        assert_eq!(plain.matches("#0d0f1e").count(), 3);
    }

    #[test]
    fn control_a_tree_with_no_shared_subtree_has_no_reference() {
        let tree: SExpression = serde_json::from_value(serde_json::json!(["do", ["set", "@entity.a", 1], ["set", "@entity.b", 2]])).unwrap();
        let written = String::from_utf8(to_vec_with_defs(&tree).unwrap()).unwrap();
        assert!(!written.contains(DEF_MARKER), "{written}");
    }

    #[test]
    fn edge_a_reference_to_an_id_never_written_is_refused() {
        let bad = serde_json::from_str::<SExpression>(r#"["object/get", {"$def": 7}, "a"]"#);
        assert!(bad.is_err());
    }

    #[test]
    fn edge_a_program_written_with_defs_reads_back_as_its_plain_form() {
        let program: crate::schema::OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "app", "orbitals": [{ "name": "A", "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] }, "traits": [{ "name": "T", "linkedEntity": "E", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                { "from": "idle", "to": "idle", "event": "X", "effects": [["set", "@entity.v", { "big": [1, 2, 3, 4, 5, 6, 7, 8] }]] },
                { "from": "idle", "to": "idle", "event": "Y", "effects": [["set", "@entity.v", { "big": [1, 2, 3, 4, 5, 6, 7, 8] }]] }
            ] } }] }]
        }))
        .unwrap();
        let plain = serde_json::to_vec(&program).unwrap();
        let with_defs = to_vec_with_defs(&program).unwrap();
        assert!(String::from_utf8_lossy(&with_defs).contains(DEF_MARKER));
        let back: crate::schema::OrbitalSchema = serde_json::from_slice(&with_defs).unwrap();
        assert_eq!(serde_json::to_vec(&back).unwrap(), plain);
    }
}
