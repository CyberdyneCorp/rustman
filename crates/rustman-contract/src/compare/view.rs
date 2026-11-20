//! An operation as the diff sees it: its matching key, its raw object and
//! path item (for pointers), and its merged parameters with the pointer
//! each one is declared at.

use serde_json::Value;

use rustman_explore::openapi::model::Operation;

use crate::spec::{Node, template_key};

pub struct OpView<'a> {
    pub op: &'a Operation,
    /// `GET /users/{}`: method and path template without parameter names.
    pub key: String,
    /// `GET /users/{id}`.
    pub display: String,
    pub node: Node<'a>,
    /// The path item holding the operation.
    pub item: Node<'a>,
}

impl<'a> OpView<'a> {
    pub fn all(doc: &'a Value, operations: &'a [Operation]) -> Vec<OpView<'a>> {
        operations
            .iter()
            .filter_map(|op| OpView::of(doc, op))
            .collect()
    }

    fn of(doc: &'a Value, op: &'a Operation) -> Option<OpView<'a>> {
        let item_pointer = op.pointer.rsplit_once('/').map_or("", |(item, _)| item);
        Some(OpView {
            op,
            key: format!("{} {}", op.method, template_key(&op.path)),
            display: format!("{} {}", op.method, op.path),
            node: Node::at(doc, &op.pointer)?,
            item: Node::at(doc, item_pointer)?,
        })
    }

    /// The parameters of the path item and the operation, the operation's
    /// winning on the same name and location.
    pub fn parameters(&self, doc: &'a Value) -> Vec<Param<'a>> {
        let mut merged = declared(doc, &self.item);
        for own in declared(doc, &self.node) {
            merged.retain(|p| !(p.name == own.name && p.location == own.location));
            merged.push(own);
        }
        merged
    }
}

/// One parameter, resolved.
pub struct Param<'a> {
    pub name: String,
    /// `path`, `query`, `header` or `cookie`.
    pub location: String,
    pub required: bool,
    pub node: Node<'a>,
}

impl Param<'_> {
    /// Whether `other` names the same parameter (header names ignore case).
    pub fn same_name(&self, other: &Param<'_>) -> bool {
        if self.location == "header" || other.location == "header" {
            self.name.eq_ignore_ascii_case(&other.name)
        } else {
            self.name == other.name
        }
    }

    /// `query parameter 'limit'`.
    pub fn label(&self) -> String {
        format!("{} parameter '{}'", self.location, self.name)
    }
}

/// The parameters listed under `holder`'s `parameters`.
fn declared<'a>(doc: &'a Value, holder: &Node<'a>) -> Vec<Param<'a>> {
    let Some(list) = holder.child("parameters") else {
        return Vec::new();
    };
    let count = list.value.as_array().map_or(0, Vec::len);
    (0..count)
        .filter_map(|i| list.item(i))
        .map(|node| node.resolved(doc))
        .filter_map(|node| {
            let name = node.get("name")?.as_str()?.to_owned();
            let location = node.get("in")?.as_str()?.to_owned();
            let required = node.get("required").and_then(Value::as_bool) == Some(true);
            Some(Param {
                name,
                location,
                required,
                node,
            })
        })
        .collect()
}
