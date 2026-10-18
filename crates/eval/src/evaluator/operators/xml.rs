//! XML operators.
//!
//! `(xml/parse s Shape)` — pure, deterministic: XML text → `Value`; `Shape` names the type the value has (checked by
//! `orb validate`, never evaluated). The root element's content is the result (its own name is dropped), mapped by
//! one rule set the `@almadar/evaluator` twin (`std/xml.ts`) shares, vector for vector
//! (orbital-core `tests/fixtures/operators/xml-vectors.json`):
//!
//! - namespaces are ignored: names and attribute names are local names, `xmlns` declarations are dropped;
//! - an attribute reads as the field `@name`;
//! - a child element is the field `name`; the same name more than once is a list, once is the value itself;
//! - an element with no attributes and no child elements is its text; so is an element with no attributes
//!   whose own text is not blank (inline markup flattens to the text it holds);
//! - any other element is an object of its attributes and children, plus `#text` when it has text of its own;
//! - text is every descendant text node in document order, whitespace runs collapsed to one space and trimmed.
//!
//! `(xml/parse s Shape paths)` — the compiler rewrites a call whose `Shape` declares list fields into this three-argument
//! form (`bake_list_shapes`): `paths` is `(list (list "a" "b") ...)`, every field path (through nested shapes and list
//! elements) whose declared type is a list. With it, such a path always reads as a list (one child becomes a
//! one-element list, an absent one stays absent); without it the two-argument rules above are unchanged.
//!
//! The five predefined entities, character references and CDATA read as text; the declaration, comments,
//! processing instructions and a doctype are ignored. Malformed XML (no root, a second root, text outside the
//! root, mismatched or unclosed tags, an unknown entity, a duplicate attribute, an invalid name) is an evaluation
//! error. An absent (`null`) input is `null`.

use super::macros::*;
use super::*;
use orbital_core_base::schema::types::{Orbital, OrbitalSchema, PayloadField};
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

#[derive(Default)]
struct Node {
    attrs: Vec<(String, String)>,
    children: Vec<(String, Node)>,
    own: String,
    full: String,
}

fn is_xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn collapse(text: &str) -> String {
    text.split(is_xml_space)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn bad(message: impl std::fmt::Display) -> OrbitalError {
    OrbitalError::Eval(EvalError::InvalidArgument(format!("xml/parse: {message}")))
}

fn local_name(raw: &[u8]) -> Result<String, OrbitalError> {
    let name = std::str::from_utf8(raw).map_err(bad)?;
    let local = name.rsplit(':').next().unwrap_or(name);
    if is_name(local) {
        Ok(local.to_string())
    } else {
        Err(bad(format!("invalid name `{name}`")))
    }
}

fn open(start: &BytesStart) -> Result<(String, Node), OrbitalError> {
    let name = local_name(start.local_name().as_ref())?;
    let mut node = Node::default();
    for attr in start.attributes() {
        let attr = attr.map_err(bad)?;
        if attr.key.as_namespace_binding().is_some() {
            continue;
        }
        let key = local_name(attr.key.local_name().as_ref())?;
        let value = attr.unescape_value().map_err(bad)?.into_owned();
        match node.attrs.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => node.attrs.push((key, value)),
        }
    }
    Ok((name, node))
}

fn parse_tree(text: &str) -> Result<Node, OrbitalError> {
    let mut reader = Reader::from_str(text);
    let mut stack: Vec<(String, Node)> = Vec::new();
    let mut root: Option<Node> = None;

    fn close(stack: &mut [(String, Node)], root: &mut Option<Node>, name: String, node: Node) {
        match stack.last_mut() {
            Some((_, parent)) => {
                parent.full.push_str(&node.full);
                parent.children.push((name, node));
            }
            None => *root = Some(node),
        }
    }

    fn add_text(
        stack: &mut [(String, Node)],
        root: &Option<Node>,
        piece: &str,
    ) -> Result<(), OrbitalError> {
        match stack.last_mut() {
            Some((_, node)) => {
                node.own.push_str(piece);
                node.full.push_str(piece);
                Ok(())
            }
            None if piece.chars().all(is_xml_space) && !piece.is_empty() => Ok(()),
            None if root.is_some() => Err(bad("text after the root element")),
            None => Err(bad("text before the root element")),
        }
    }

    loop {
        match reader.read_event().map_err(bad)? {
            Event::Start(start) => {
                if root.is_some() && stack.is_empty() {
                    return Err(bad("more than one root element"));
                }
                stack.push(open(&start)?);
            }
            Event::Empty(start) => {
                if root.is_some() && stack.is_empty() {
                    return Err(bad("more than one root element"));
                }
                let (name, node) = open(&start)?;
                close(&mut stack, &mut root, name, node);
            }
            Event::End(_) => {
                let (name, node) = stack.pop().ok_or_else(|| bad("unexpected closing tag"))?;
                close(&mut stack, &mut root, name, node);
            }
            Event::Text(piece) => add_text(&mut stack, &root, &piece.decode().map_err(bad)?)?,
            Event::CData(piece) => {
                if stack.is_empty() {
                    return Err(bad("CDATA outside the root element"));
                }
                add_text(&mut stack, &root, &piece.decode().map_err(bad)?)?;
            }
            Event::GeneralRef(reference) => {
                if stack.is_empty() {
                    return Err(bad("entity outside the root element"));
                }
                let decoded = match reference.resolve_char_ref().map_err(bad)? {
                    Some(c) => c.to_string(),
                    None => {
                        let name = reference.decode().map_err(bad)?;
                        resolve_xml_entity(&name)
                            .ok_or_else(|| bad(format!("unknown entity `&{name};`")))?
                            .to_string()
                    }
                };
                add_text(&mut stack, &root, &decoded)?;
            }
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err(bad("unclosed element"));
    }
    root.ok_or_else(|| bad("no root element"))
}

fn to_value(node: Node) -> Value {
    let text = collapse(&node.full);
    let own_has_text = node.own.chars().any(|c| !is_xml_space(c));
    if node.attrs.is_empty() && (node.children.is_empty() || own_has_text) {
        return Value::String(text);
    }
    let mut object = ValueMap::new();
    for (key, value) in node.attrs {
        object.insert(format!("@{key}"), Value::String(value));
    }
    let leaf_or_mixed = node.children.is_empty() || own_has_text;
    for (name, child) in node.children {
        let value = to_value(child);
        match object.get_mut(&name) {
            Some(Value::Array(items)) => items.push(value),
            Some(existing) => {
                let first = std::mem::replace(existing, Value::Null);
                *existing = Value::Array(vec![first, value]);
            }
            None => {
                object.insert(name, value);
            }
        }
    }
    if leaf_or_mixed && !text.is_empty() {
        object.insert("#text".to_string(), Value::String(text));
    }
    Value::Object(object)
}

pub fn parse_xml(text: &str) -> Result<Value, OrbitalError> {
    parse_tree(text).map(to_value)
}

/// Make every list-typed path of `paths` (relative to `value`) a list: a lone child is wrapped, an absent one stays
/// absent, and the rest of a path descends into each element of a list.
fn apply_list_paths(value: &mut Value, paths: &[Vec<String>]) {
    match value {
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| apply_list_paths(item, paths)),
        Value::Object(map) => {
            let mut heads: Vec<&str> = paths
                .iter()
                .filter_map(|p| p.first())
                .map(String::as_str)
                .collect();
            heads.dedup();
            for head in heads {
                let Some(child) = map.get_mut(head) else {
                    continue;
                };
                if paths.iter().any(|p| p.len() == 1 && p[0] == head)
                    && !matches!(child, Value::Array(_))
                {
                    *child = Value::Array(vec![std::mem::replace(child, Value::Null)]);
                }
                let rest: Vec<Vec<String>> = paths
                    .iter()
                    .filter(|p| p.len() > 1 && p[0] == head)
                    .map(|p| p[1..].to_vec())
                    .collect();
                if !rest.is_empty() {
                    apply_list_paths(child, &rest);
                }
            }
        }
        _ => {}
    }
}

fn list_paths_of(value: Value) -> Result<Vec<Vec<String>>, OrbitalError> {
    let malformed = || bad("the list-path argument must be a list of lists of field names");
    let Value::Array(paths) = value else {
        return Err(malformed());
    };
    paths
        .into_iter()
        .map(|path| match path {
            Value::Array(segments) => segments
                .into_iter()
                .map(|s| match s {
                    Value::String(name) => Ok(name),
                    _ => Err(malformed()),
                })
                .collect(),
            _ => Err(malformed()),
        })
        .collect()
}

/// The dotted-path descriptor of a declared shape: every field path whose type is a list, nested shapes and list
/// elements walked through. An attribute (`@name`) is always a string, so a path never runs through one.
pub fn list_paths(fields: &[PayloadField]) -> Vec<Vec<String>> {
    fn walk(fields: &[PayloadField], prefix: &[String], out: &mut Vec<Vec<String>>) {
        for field in fields {
            if field.name.starts_with('@') {
                continue;
            }
            let mut path = prefix.to_vec();
            path.push(field.name.clone());
            if field.field_type.starts_with('[') {
                out.push(path.clone());
            }
            if let Some(children) = &field.properties {
                walk(children, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(fields, &[], &mut out);
    out
}

fn bake_value(
    node: &mut serde_json::Value,
    types: &std::collections::BTreeMap<String, Vec<PayloadField>>,
) -> bool {
    use serde_json::Value as J;
    let mut changed = false;
    match node {
        J::Array(items) => {
            if let [J::String(op), _, J::String(shape)] = items.as_slice() {
                if op == "xml/parse" {
                    if let Some(fields) = types.get(shape) {
                        let paths = list_paths(fields);
                        if !paths.is_empty() {
                            let mut descriptor = vec![J::String("list".into())];
                            descriptor.extend(paths.into_iter().map(|path| {
                                let mut entry = vec![J::String("list".into())];
                                entry.extend(path.into_iter().map(J::String));
                                J::Array(entry)
                            }));
                            items.push(J::Array(descriptor));
                            changed = true;
                        }
                    }
                }
            }
            for item in items.iter_mut() {
                changed |= bake_value(item, types);
            }
        }
        J::Object(map) => {
            for child in map.values_mut() {
                changed |= bake_value(child, types);
            }
        }
        _ => {}
    }
    changed
}

/// Rewrite every `(xml/parse s Shape)` whose `Shape` is one of its orbital's declared struct aliases and declares
/// list fields into `(xml/parse s Shape paths)`. Idempotent: a call that already carries `paths` is left alone, and
/// so is a bare name no orbital declares (the validator reports it).
pub fn bake_list_shapes(schema: &mut OrbitalSchema) -> Result<(), serde_json::Error> {
    for orbital in &mut schema.orbitals {
        let Some(types) = orbital.types.clone().filter(|t| !t.is_empty()) else {
            continue;
        };
        let mut tree = serde_json::to_value(&*orbital)?;
        if let Some(object) = tree.as_object_mut() {
            object.remove("types");
        }
        if bake_value(&mut tree, &types) {
            let mut baked: Orbital = serde_json::from_value(tree)?;
            baked.types = Some(types);
            *orbital = baked;
        }
    }
    Ok(())
}

pub struct XmlValidOp;
impl Operator for XmlValidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.len() != 1 {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(
                "xml/valid: expected exactly one argument".into(),
            )));
        }
        match eval_expr(&args[0], ctx)? {
            Value::String(text) => Ok(Value::Bool(parse_tree(&text).is_ok())),
            other => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "string".into(),
                actual: other.type_name().into(),
            })),
        }
    }
}

pub struct XmlParseOp;
impl Operator for XmlParseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "xml/parse", >= 2);
        let paths = match args.get(2) {
            Some(expr) => list_paths_of(eval_expr(expr, ctx)?)?,
            None => Vec::new(),
        };
        match eval_expr(&args[0], ctx)? {
            Value::Null => Ok(Value::Null),
            Value::String(text) => {
                let mut value = parse_xml(&text)?;
                apply_list_paths(&mut value, &paths);
                Ok(value)
            }
            other => Err(bad(format!(
                "expected a string argument, got {}",
                other.type_name()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(value: &Value) -> &str {
        match value {
            Value::String(s) => s,
            other => panic!("expected a string, got {other:?}"),
        }
    }

    #[test]
    fn a_whitespace_only_container_has_no_text_but_a_single_text_node_does() {
        let Value::Object(map) = parse_xml("<a>\n <b>x</b>\n</a>").unwrap() else {
            panic!("object")
        };
        assert!(map.get("#text").is_none());
        assert_eq!(string(map.get("b").unwrap()), "x");
    }

    #[test]
    fn an_xml_space_set_excludes_the_no_break_space() {
        assert_eq!(
            string(&parse_xml("<a>\u{a0}x\u{a0}</a>").unwrap()),
            "\u{a0}x\u{a0}"
        );
    }

    fn field(name: &str, ty: &str, properties: Option<Vec<PayloadField>>) -> PayloadField {
        PayloadField {
            properties,
            ..serde_json::from_value(serde_json::json!({ "name": name, "type": ty })).unwrap()
        }
    }

    fn feed() -> Vec<PayloadField> {
        vec![
            field("title", "string", None),
            field("@href", "[string]", None),
            field(
                "entry",
                "[object]",
                Some(vec![
                    field("id", "string", None),
                    field(
                        "author",
                        "[object]",
                        Some(vec![field("name", "string", None)]),
                    ),
                    field(
                        "owner",
                        "object",
                        Some(vec![field("tags", "[string]", None)]),
                    ),
                ]),
            ),
        ]
    }

    #[test]
    fn list_paths_walk_nested_shapes_and_list_elements_but_never_an_attribute() {
        assert_eq!(
            list_paths(&feed()),
            vec![
                vec!["entry".to_string()],
                vec!["entry".to_string(), "author".to_string()],
                vec!["entry".to_string(), "owner".to_string(), "tags".to_string()],
            ]
        );
        assert!(
            list_paths(&[field("a", "string", None)]).is_empty(),
            "control: no list field, no paths"
        );
    }

    #[test]
    fn list_paths_wrap_a_lone_child_at_every_depth_and_leave_the_rest() {
        let paths = list_paths(&feed());
        let mut value = parse_xml(
            "<f><title>t</title><entry><id>1</id><author><name>A</name></author><owner><tags>x</tags></owner></entry></f>",
        )
        .unwrap();
        apply_list_paths(&mut value, &paths);
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "title": "t",
                "entry": [{ "id": "1", "author": [{ "name": "A" }], "owner": { "tags": ["x"] } }]
            })
        );
        let mut absent = parse_xml("<f><title>t</title></f>").unwrap();
        apply_list_paths(&mut absent, &paths);
        assert_eq!(
            serde_json::to_value(&absent).unwrap(),
            serde_json::json!({ "title": "t" })
        );
        let mut bare = parse_xml("<f><entry><id>1</id></entry></f>").unwrap();
        apply_list_paths(&mut bare, &[]);
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({ "entry": { "id": "1" } })
        );
    }

    fn schema_with(call: serde_json::Value, types: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "app",
            "orbitals": [{
                "name": "O",
                "entity": { "name": "E", "persistence": "runtime", "fields": [] },
                "traits": [{
                    "name": "T",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "GO" }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["set", "@entity.x", call]] }]
                    }
                }],
                "types": types
            }]
        }))
        .unwrap()
    }

    fn baked_call(schema: &OrbitalSchema) -> serde_json::Value {
        let tree = serde_json::to_value(schema).unwrap();
        tree["orbitals"][0]["traits"][0]["stateMachine"]["transitions"][0]["effects"][0][2].clone()
    }

    #[test]
    fn bake_appends_the_list_paths_to_a_call_naming_a_declared_shape() {
        let types = serde_json::json!({ "Feed": [{ "name": "entry", "type": "[object]", "properties": [{ "name": "id", "type": "string" }] }] });
        let mut schema = schema_with(
            serde_json::json!(["xml/parse", "@payload.body", "Feed"]),
            types,
        );
        bake_list_shapes(&mut schema).unwrap();
        let expected = serde_json::json!([
            "xml/parse",
            "@payload.body",
            "Feed",
            ["list", ["list", "entry"]]
        ]);
        assert_eq!(baked_call(&schema), expected);
        bake_list_shapes(&mut schema).unwrap();
        assert_eq!(baked_call(&schema), expected, "idempotent");
    }

    #[test]
    fn bake_leaves_a_shape_without_lists_and_an_undeclared_name_bare() {
        let flat = serde_json::json!({ "Row": [{ "name": "id", "type": "string" }] });
        let mut schema = schema_with(
            serde_json::json!(["xml/parse", "@payload.body", "Row"]),
            flat.clone(),
        );
        bake_list_shapes(&mut schema).unwrap();
        assert_eq!(
            baked_call(&schema),
            serde_json::json!(["xml/parse", "@payload.body", "Row"])
        );
        let mut undeclared = schema_with(
            serde_json::json!(["xml/parse", "@payload.body", "Nope"]),
            flat,
        );
        bake_list_shapes(&mut undeclared).unwrap();
        assert_eq!(
            baked_call(&undeclared),
            serde_json::json!(["xml/parse", "@payload.body", "Nope"])
        );
    }

    #[test]
    fn a_name_check_accepts_prefixes_dashes_and_digits_after_the_start() {
        assert!(is_name("primary_category") && is_name("a-b.c9") && !is_name("9a") && !is_name(""));
    }
}
