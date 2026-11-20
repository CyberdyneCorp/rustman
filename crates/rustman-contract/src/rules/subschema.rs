//! The schema-valued keywords `items` and `additionalProperties` appearing,
//! disappearing or switching between a boolean and a schema. Absent,
//! `true` and a schema that says nothing (`{}`, or only documentation)
//! accept anything; `false` accepts nothing; any other schema is in
//! between. A move towards `false` is stricter and a move towards "anything"
//! is looser, classified like any other constraint (see
//! [`super::constraints`]). When both sides are schemas, the walker compares
//! them member by member instead.
//!
//! An `allOf` member with `additionalProperties: false` closes the whole
//! object (as in [`super::properties`]), so it counts as the object's.

use serde_json::Value;

use super::constraints::{Change, rule};
use super::object::parts;
use super::{Pair, Rule, shown};
use crate::difference::Sink;
use crate::spec::{Node, inline};

pub struct SubschemaRule;

const KEYWORDS: [&str; 2] = ["items", "additionalProperties"];

/// What a schema-valued keyword allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Absent, `true` or an empty schema.
    Any,
    /// `false`.
    Never,
    /// A schema that constrains the values.
    Schema,
}

/// One side of the keyword: its shape, how it is shown and its pointer.
struct Side {
    shape: Shape,
    shown: String,
    pointer: String,
}

impl Rule for SubschemaRule {
    fn check(&self, pair: &Pair<'_, '_>, sink: &mut Sink<'_>) {
        for keyword in KEYWORDS {
            let old = side(pair.old, pair.old_doc, keyword);
            let new = side(pair.new, pair.new_doc, keyword);
            let Some(change) = change(old.shape, new.shape) else {
                continue;
            };
            let message = format!(
                "{}: {keyword} {} -> {}",
                pair.place(None),
                old.shown,
                new.shown
            );
            sink.push(
                rule(pair.direction, change),
                message,
                &old.pointer,
                &new.pointer,
            );
        }
    }
}

/// How the keyword changed, or `None` when it did not (or when both sides
/// are schemas, which the walker compares).
fn change(old: Shape, new: Shape) -> Option<Change> {
    match (old, new) {
        _ if old == new => None,
        (Shape::Any, _) | (_, Shape::Never) => Some(Change::Stricter),
        (_, Shape::Any) | (Shape::Never, _) => Some(Change::Looser),
        (Shape::Schema, Shape::Schema) => None,
    }
}

fn side(node: &Node<'_>, doc: &Value, keyword: &str) -> Side {
    if keyword == "additionalProperties"
        && let Some(closed) = closed_member(node, doc)
    {
        return closed;
    }
    match node.child(keyword) {
        None => Side {
            shape: Shape::Any,
            shown: "none".to_owned(),
            pointer: node.pointer.clone(),
        },
        Some(child) => Side {
            shape: shape(child.value, doc),
            shown: shown(Some(child.value)),
            pointer: child.pointer,
        },
    }
}

/// `additionalProperties: false` on the object itself or one of its
/// `allOf` members.
fn closed_member(node: &Node<'_>, doc: &Value) -> Option<Side> {
    parts(node, doc)
        .iter()
        .filter_map(|part| part.child("additionalProperties"))
        .find(|child| child.value == &Value::Bool(false))
        .map(|child| Side {
            shape: Shape::Never,
            shown: "false".to_owned(),
            pointer: child.pointer,
        })
}

fn shape(value: &Value, doc: &Value) -> Shape {
    match value {
        Value::Bool(true) => Shape::Any,
        Value::Bool(false) => Shape::Never,
        Value::Object(_) if inline(doc, value, true) == Value::Object(Default::default()) => {
            Shape::Any
        }
        _ => Shape::Schema,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shapes_order_strictness() {
        use Shape::{Any, Never, Schema};
        assert_eq!(change(Any, Never), Some(Change::Stricter));
        assert_eq!(change(Any, Schema), Some(Change::Stricter));
        assert_eq!(change(Schema, Never), Some(Change::Stricter));
        assert_eq!(change(Never, Any), Some(Change::Looser));
        assert_eq!(change(Never, Schema), Some(Change::Looser));
        assert_eq!(change(Schema, Any), Some(Change::Looser));
        assert_eq!(change(Schema, Schema), None);
        assert_eq!(change(Any, Any), None);
    }

    #[test]
    fn empty_and_documentation_only_schemas_accept_anything() {
        let doc = json!({"e": {"description": "anything"}});
        assert_eq!(shape(&json!({}), &doc), Shape::Any);
        assert_eq!(shape(&json!({"$ref": "#/e"}), &doc), Shape::Any);
        assert_eq!(shape(&json!(true), &doc), Shape::Any);
        assert_eq!(shape(&json!(false), &doc), Shape::Never);
        assert_eq!(shape(&json!({"type": "integer"}), &doc), Shape::Schema);
    }
}
