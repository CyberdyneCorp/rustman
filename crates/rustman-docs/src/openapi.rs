//! OpenAPI enrichment (api-docs-generation "OpenAPI enrichment"): the
//! operations of the import snapshots under `.rustman/openapi/`, matched to
//! requests by the `openapi:operationId=` / `openapi:path=` tags the import
//! wrote, as `rustman_explore::sync::local` does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rustman_core::Workspace;
use rustman_core::collection::Collection;
use rustman_core::model::HttpRequest;
use rustman_explore::store::{SOURCE_FORMAT, Target};
use rustman_explore::sync::local::operation_key;
use serde_json::Value;

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// What an operation adds to its request's page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Operation {
    pub summary: Option<String>,
    pub description: Option<String>,
    pub deprecated: bool,
    /// Parameter name to its description.
    pub parameters: BTreeMap<String, String>,
    pub request_schema: Option<Value>,
    /// Status code and schema, in document order.
    pub response_schemas: Vec<(String, Value)>,
}

/// The operations of every imported collection, by collection folder.
#[derive(Debug, Default)]
pub struct OpenApiIndex {
    collections: BTreeMap<PathBuf, BTreeMap<String, Operation>>,
}

impl OpenApiIndex {
    /// Read the import snapshot of every collection imported from OpenAPI.
    /// A missing or unreadable snapshot only means no enrichment.
    pub fn load(workspace: &Workspace) -> Self {
        let mut index = Self::default();
        for collection in workspace.collections() {
            if let Some(document) = snapshot(&workspace.root, collection) {
                index
                    .collections
                    .insert(collection.path.clone(), operations(&document));
            }
        }
        index
    }

    /// The operation `request` was generated from, looked up in the
    /// nearest imported collection among `ancestors` (root first).
    pub fn find(&self, ancestors: &[&Collection], request: &HttpRequest) -> Option<&Operation> {
        let key = operation_key(request)?;
        ancestors
            .iter()
            .rev()
            .find_map(|c| self.collections.get(&c.path))
            .and_then(|ops| ops.get(&key))
    }
}

fn snapshot(root: &Path, collection: &Collection) -> Option<Value> {
    let source = collection.meta.source.as_ref()?;
    if source.format != SOURCE_FORMAT {
        return None;
    }
    let path = Target::new(root, root.join(&collection.path)).snapshot_path();
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Every operation of `document`, under its `operationId` and under
/// `METHOD /path`.
fn operations(document: &Value) -> BTreeMap<String, Operation> {
    let mut out = BTreeMap::new();
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return out;
    };
    for (path, item) in paths {
        let item = resolve(document, item);
        for method in METHODS {
            let Some(op) = item.get(method) else { continue };
            let operation = operation(document, item, op);
            if let Some(id) = op.get("operationId").and_then(Value::as_str) {
                out.insert(id.to_owned(), operation.clone());
            }
            out.insert(format!("{} {path}", method.to_ascii_uppercase()), operation);
        }
    }
    out
}

fn operation(document: &Value, item: &Value, op: &Value) -> Operation {
    let text = |key: &str| op.get(key).and_then(Value::as_str).map(str::to_owned);
    let mut parameters = BTreeMap::new();
    let lists = [item.get("parameters"), op.get("parameters")];
    for param in lists
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
    {
        let param = resolve(document, param);
        let name = param.get("name").and_then(Value::as_str);
        let description = param.get("description").and_then(Value::as_str);
        if let (Some(name), Some(description)) = (name, description) {
            parameters.insert(name.to_owned(), description.to_owned());
        }
    }
    let request_schema = op
        .get("requestBody")
        .map(|body| resolve(document, body))
        .and_then(|body| content_schema(document, body));
    Operation {
        summary: text("summary"),
        description: text("description"),
        deprecated: op.get("deprecated").and_then(Value::as_bool) == Some(true),
        parameters,
        request_schema,
        response_schemas: response_schemas(document, op),
    }
}

fn response_schemas(document: &Value, op: &Value) -> Vec<(String, Value)> {
    let Some(responses) = op.get("responses").and_then(Value::as_object) else {
        return Vec::new();
    };
    responses
        .iter()
        .filter_map(|(code, response)| {
            let schema = content_schema(document, resolve(document, response))?;
            Some((code.clone(), schema))
        })
        .collect()
}

/// The schema of the first media type of `holder.content` (JSON first),
/// with a top-level `$ref` resolved.
fn content_schema(document: &Value, holder: &Value) -> Option<Value> {
    let content = holder.get("content")?.as_object()?;
    let media = content
        .get("application/json")
        .or_else(|| content.values().next())?;
    media.get("schema").map(|s| resolve(document, s).clone())
}

/// `value`, or the local `#/...` component its `$ref` points to.
fn resolve<'a>(document: &'a Value, value: &'a Value) -> &'a Value {
    value
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix('#'))
        .and_then(|pointer| document.pointer(pointer))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document() -> Value {
        json!({
            "openapi": "3.1.0",
            "paths": {
                "/users": {
                    "parameters": [{"$ref": "#/components/parameters/Limit"}],
                    "get": {
                        "operationId": "listUsers",
                        "summary": "List users",
                        "description": "List users",
                        "deprecated": true,
                        "parameters": [{"name": "role", "in": "query", "description": "Filter by role"}],
                        "responses": {
                            "200": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Users"}}}},
                            "204": {"description": "none"}
                        }
                    },
                    "post": {
                        "requestBody": {"content": {"text/plain": {"schema": {"type": "string"}}}},
                        "responses": {}
                    }
                }
            },
            "components": {
                "parameters": {"Limit": {"name": "limit", "in": "query", "description": "Page size"}},
                "schemas": {"Users": {"type": "array"}}
            }
        })
    }

    #[test]
    fn operations_are_keyed_by_id_and_by_path() {
        let ops = operations(&document());
        let list = &ops["listUsers"];
        assert_eq!(list, &ops["GET /users"]);
        assert!(list.deprecated);
        assert_eq!(list.summary.as_deref(), Some("List users"));
        assert_eq!(list.parameters["limit"], "Page size");
        assert_eq!(list.parameters["role"], "Filter by role");
        assert_eq!(
            list.response_schemas,
            [("200".to_owned(), json!({"type": "array"}))]
        );
        let post = &ops["POST /users"];
        assert_eq!(post.request_schema, Some(json!({"type": "string"})));
        assert!(!post.deprecated);
        assert_eq!(ops.len(), 3);
    }

    #[test]
    fn a_document_without_paths_has_no_operations() {
        assert!(operations(&json!({})).is_empty());
    }
}
