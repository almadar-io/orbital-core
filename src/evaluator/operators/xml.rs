//! XML operators.
//!
//! `(xml/parse s Shape)` — pure, deterministic: XML text → `Value`; `Shape` names the type the value has (checked by
//! `orb validate`, never evaluated). The root element's content is the result (its own name is dropped), mapped by
//! one rule set the `@almadar/evaluator` twin (`std/xml.ts`) shares, vector for vector
//! (`@almadar-io/parity` `fixtures/operators/xml-vectors.json`):
//!
//! - namespaces are ignored: names and attribute names are local names, `xmlns` declarations are dropped;
//! - an attribute reads as the field `@name`;
//! - a child element is the field `name`; the same name more than once is a list, once is the value itself;
//! - an element with no attributes and no child elements is its text; so is an element with no attributes
//!   whose own text is not blank (inline markup flattens to the text it holds);
//! - any other element is an object of its attributes and children, plus `#text` when it has text of its own;
//! - text is every descendant text node in document order, whitespace runs collapsed to one space and trimmed.
//!
//! The five predefined entities, character references and CDATA read as text; the declaration, comments,
//! processing instructions and a doctype are ignored. Malformed XML (no root, a second root, text outside the
//! root, mismatched or unclosed tags, an unknown entity, a duplicate attribute, an invalid name) is an evaluation
//! error. An absent (`null`) input is `null`.

use super::macros::*;
use super::*;
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

pub(crate) fn parse_xml(text: &str) -> Result<Value, OrbitalError> {
    parse_tree(text).map(to_value)
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
        match eval_expr(&args[0], ctx)? {
            Value::Null => Ok(Value::Null),
            Value::String(text) => parse_xml(&text),
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

    #[test]
    fn a_name_check_accepts_prefixes_dashes_and_digits_after_the_start() {
        assert!(is_name("primary_category") && is_name("a-b.c9") && !is_name("9a") && !is_name(""));
    }
}
