//! Resources that probes create: which operation made them, how to find
//! their id in the create response, and how to delete them. The executor
//! records each one in the journal; cleanup deletes them in reverse order
//! with `--allow-destructive` and lists them as `left behind` otherwise.

use serde_json::Value;

use super::graph::Graph;
use super::operation_key;
use super::plan::segment;
use crate::openapi::model::{ApiModel, Operation};

pub use rustman_report::exploration::Resource;

/// How to identify what one create operation made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Creator {
    /// `METHOD /path` of the create operation.
    pub create: String,
    /// Where the id is in its response body.
    pub pointer: String,
    /// `METHOD /path` of the delete operation, when documented.
    pub delete: Option<String>,
}

impl Creator {
    /// The resource a `2xx` response `body` to the create step at `path`
    /// (a template) and `url` (redacted) describes, when it holds an id.
    pub fn resource(&self, probe: &str, path: &str, url: &str, body: &[u8]) -> Option<Resource> {
        let body: Value = serde_json::from_slice(body).ok()?;
        let id = match body.pointer(&self.pointer)? {
            Value::String(s) if !s.is_empty() => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        };
        let id = segment(&id);
        let url = url.split(['?', '#']).next().unwrap_or(url);
        Some(Resource {
            probe: probe.to_owned(),
            operation: self.create.clone(),
            path: format!("{}/{id}", path.trim_end_matches('/')),
            url: format!("{}/{id}", url.trim_end_matches('/')),
            delete: self.delete.clone(),
        })
    }
}

/// Every POST whose created item lives at `<its path>/{param}`, with the
/// pointer the graph found for that parameter.
pub fn creators(model: &ApiModel, graph: &Graph) -> Vec<Creator> {
    model
        .operations
        .iter()
        .filter(|op| op.method == "POST")
        .filter_map(|create| creator(model, graph, create))
        .collect()
}

fn creator(model: &ApiModel, graph: &Graph, create: &Operation) -> Option<Creator> {
    let key = operation_key(create);
    let items = model.operations.iter().filter_map(|op| {
        let param = item_parameter(&create.path, &op.path)?;
        Some((op, param))
    });
    let mut pointer = None;
    let mut delete = None;
    for (op, param) in items {
        let consumer = operation_key(op);
        if let Some(edge) = graph
            .producers(&consumer, param)
            .find(|e| e.producer == key)
        {
            pointer.get_or_insert_with(|| edge.pointer.clone());
        }
        if op.method == "DELETE" {
            delete = Some(consumer);
        }
    }
    Some(Creator {
        create: key,
        pointer: pointer?,
        delete,
    })
}

/// `param` when `item` is `parent/{param}`.
fn item_parameter<'a>(parent: &str, item: &'a str) -> Option<&'a str> {
    let rest = item.strip_prefix(parent.trim_end_matches('/'))?;
    rest.strip_prefix("/{")?
        .strip_suffix('}')
        .filter(|p| !p.contains(['/', '{', '}']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_parameters_are_one_segment_below() {
        assert_eq!(item_parameter("/users", "/users/{id}"), Some("id"));
        assert_eq!(item_parameter("/users", "/users/{id}/roles"), None);
        assert_eq!(item_parameter("/users", "/users"), None);
        assert_eq!(item_parameter("/users", "/usersx/{id}"), None);
    }

    #[test]
    fn a_resource_is_found_by_its_pointer() {
        let creator = Creator {
            create: "POST /users".into(),
            pointer: "/id".into(),
            delete: Some("DELETE /users/{id}".into()),
        };
        let found = creator
            .resource("p0001", "/users", "http://h/users?x=1", br#"{"id": 42}"#)
            .unwrap();
        assert_eq!(found.path, "/users/42");
        assert_eq!(found.url, "http://h/users/42");
        let text = creator.resource("p", "/users", "u", br#"{"id": "a b"}"#);
        assert_eq!(text.unwrap().path, "/users/a%20b");
        assert!(creator.resource("p", "/users", "u", b"{}").is_none());
        assert!(creator.resource("p", "/users", "u", b"oops").is_none());
    }
}
