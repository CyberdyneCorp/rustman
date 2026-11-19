//! Loading the two documents of a diff (design D1). Loading, bundling, the
//! 3.0 → 3.1 schema rewrite, the path-item parameter merge and validation
//! are `rustman_explore::openapi`'s; this module adds what comparison needs
//! on top: path-template keys that ignore parameter names, `$ref`
//! following that keeps the real pointer, and cycle-aware inlining.

use serde_json::{Map, Value};

use rustman_explore::openapi::bundle::{self, BundleOptions};
use rustman_explore::openapi::{self, Location, document, lower, pointer};
use rustman_explore::{ApiModel, ImportError, ImportOptions, Imported};

/// The most `$ref`s inlined along one branch before giving up on depth.
const MAX_DEPTH: usize = 64;

/// The marker that replaces a reference to a schema already being inlined.
pub const CYCLE: &str = "x-rustman-cycle";

/// One side of a diff: the bundled, 3.1-shaped document and its model.
#[derive(Clone, Debug)]
pub struct Spec {
    /// How the document is named in messages (its path or URL).
    pub name: String,
    pub document: Value,
    pub model: ApiModel,
}

impl Spec {
    /// Read `source` (a path or an `http(s)` URL) as `openapi-import` does.
    /// Invalid documents fail with [`ImportError::Invalid`], whose
    /// diagnostics `openapi::diagnostic::render` prints.
    pub async fn load(source: &str, options: &ImportOptions) -> Result<Spec, ImportError> {
        let imported = rustman_explore::import(source, options).await?;
        Ok(Spec::from_imported(&imported))
    }

    /// Read the local file `source` without opening any network
    /// connection (passive conformance): a URL is refused before anything
    /// is sent, and so are references to remote documents.
    pub async fn load_local(source: &str) -> Result<Spec, ImportError> {
        if let Location::Url(url) = Location::parse(source) {
            return Err(ImportError::Source(format!(
                "{url}: this command reads a local file and opens no network connection; \
                 download the document first"
            )));
        }
        let options = ImportOptions {
            allow_remote_refs: false,
            ..ImportOptions::default()
        };
        Spec::load(source, &options).await
    }

    pub fn from_imported(imported: &Imported) -> Spec {
        Spec {
            name: imported.location.to_string(),
            document: imported.document.clone(),
            model: imported.model.clone(),
        }
    }

    /// A document already parsed (tests, other front-ends). Only internal
    /// references are resolved.
    pub async fn from_value(name: &str, raw: Value) -> Result<Spec, ImportError> {
        let version = document::detect_version(&raw, name)?;
        let options = BundleOptions {
            allow_remote_refs: false,
            version,
            fetch: Default::default(),
        };
        let document = bundle::bundle(&Location::parse(name), raw, &options).await?;
        let (model, _) = openapi::check_and_lower(&document, version)?;
        Ok(Spec {
            name: name.to_owned(),
            document,
            model,
        })
    }
}

/// A path template with its parameter names dropped: `/users/{id}` and
/// `/users/{userId}` both become `/users/{}`.
pub fn template_key(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_name = false;
    for c in path.chars() {
        match c {
            '{' => {
                in_name = true;
                out.push('{');
            }
            '}' if in_name => {
                in_name = false;
                out.push('}');
            }
            _ if in_name => {}
            _ => out.push(c),
        }
    }
    out
}

/// A value of a document with the pointer it is at.
#[derive(Clone, Copy, Debug)]
pub struct Located<'a> {
    pub pointer: &'a str,
    pub value: &'a Value,
}

/// A value with an owned pointer (children and dereferenced targets).
#[derive(Clone, Debug)]
pub struct Node<'a> {
    pub pointer: String,
    pub value: &'a Value,
}

impl<'a> Node<'a> {
    /// The value at `pointer` in `doc`.
    pub fn at(doc: &'a Value, pointer: &str) -> Option<Node<'a>> {
        doc.pointer(pointer).map(|value| Node {
            pointer: pointer.to_owned(),
            value,
        })
    }

    /// The node itself, or the end of its `$ref` chain. A reference that
    /// cannot be followed is kept as it is.
    pub fn resolved(self, doc: &'a Value) -> Node<'a> {
        if self.value.get("$ref").is_none() {
            return self;
        }
        match lower::deref(doc, &self.pointer) {
            Ok((pointer, value)) => Node { pointer, value },
            Err(_) => self,
        }
    }

    /// The child `key` (an object member), not resolved.
    pub fn child(&self, key: &str) -> Option<Node<'a>> {
        self.value.get(key).map(|value| Node {
            pointer: pointer::push(&self.pointer, key),
            value,
        })
    }

    /// The element `index` of an array, not resolved.
    pub fn item(&self, index: usize) -> Option<Node<'a>> {
        self.value.get(index).map(|value| Node {
            pointer: pointer::push(&self.pointer, &index.to_string()),
            value,
        })
    }

    /// The pointer of the child `key`, whether or not it exists.
    pub fn child_pointer(&self, key: &str) -> String {
        pointer::push(&self.pointer, key)
    }

    pub fn get(&self, key: &str) -> Option<&'a Value> {
        self.value.get(key)
    }

    pub fn as_located(&self) -> Located<'_> {
        Located {
            pointer: &self.pointer,
            value: self.value,
        }
    }
}

/// `value` with every `#/...` reference replaced by its target. A reference
/// to a schema already being inlined on the same branch becomes
/// `{"x-rustman-cycle": "<pointer>"}`, so recursive schemas stay finite.
/// With `strip_docs`, documentation keys and vendor extensions are dropped,
/// so two inlined schemas compare equal when only their documentation
/// differs.
pub fn inline(doc: &Value, value: &Value, strip_docs: bool) -> Value {
    let mut stack = Vec::new();
    inline_value(doc, value, strip_docs, &mut stack)
}

fn inline_value(doc: &Value, value: &Value, strip: bool, stack: &mut Vec<String>) -> Value {
    match value {
        Value::Object(map) => inline_object(doc, map, strip, stack),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| inline_value(doc, item, strip, stack))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn inline_object(
    doc: &Value,
    map: &Map<String, Value>,
    strip: bool,
    stack: &mut Vec<String>,
) -> Value {
    if let Some(target) = reference_target(map) {
        if stack.contains(&target) || stack.len() >= MAX_DEPTH {
            return serde_json::json!({ CYCLE: target });
        }
        let Some(resolved) = doc.pointer(&target) else {
            return Value::Object(map.clone());
        };
        stack.push(target);
        let mut inlined = inline_value(doc, resolved, strip, stack);
        stack.pop();
        overlay_siblings(&mut inlined, map, doc, strip, stack);
        return inlined;
    }
    let mut out = Map::new();
    for (key, child) in map {
        if strip && is_doc_key(key) {
            continue;
        }
        out.insert(key.clone(), inline_value(doc, child, strip, stack));
    }
    Value::Object(out)
}

/// The pointer a local `$ref` names.
fn reference_target(map: &Map<String, Value>) -> Option<String> {
    let reference = map.get("$ref")?.as_str()?;
    reference.strip_prefix('#').and_then(pointer::from_fragment)
}

/// Keys next to a `$ref` (allowed in 3.1) win over the target's.
fn overlay_siblings(
    inlined: &mut Value,
    map: &Map<String, Value>,
    doc: &Value,
    strip: bool,
    stack: &mut Vec<String>,
) {
    let Some(target) = inlined.as_object_mut() else {
        return;
    };
    for (key, child) in map {
        if key != "$ref" && !(strip && is_doc_key(key)) {
            target.insert(key.clone(), inline_value(doc, child, strip, stack));
        }
    }
}

/// Keys that only document a schema or object.
pub const DOC_KEYS: [&str; 7] = [
    "summary",
    "description",
    "title",
    "example",
    "examples",
    "externalDocs",
    "$comment",
];

fn is_doc_key(key: &str) -> bool {
    DOC_KEYS.contains(&key) || key.starts_with("x-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn template_keys_ignore_parameter_names() {
        assert_eq!(template_key("/users/{id}"), "/users/{}");
        assert_eq!(template_key("/users/{userId}"), template_key("/users/{id}"));
        assert_eq!(
            template_key("/a/{x}/b/{y}.json"),
            template_key("/a/{p}/b/{q}.json")
        );
        assert_ne!(template_key("/users/{id}"), template_key("/users/me"));
    }

    #[test]
    fn recursive_schemas_inline_to_a_cycle_marker() {
        let doc = json!({"components": {"schemas": {"Node": {
            "type": "object",
            "properties": {"children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}}}
        }}}});
        let inlined = inline(&doc, &json!({"$ref": "#/components/schemas/Node"}), false);
        assert_eq!(
            inlined.pointer("/properties/children/items"),
            Some(&json!({CYCLE: "/components/schemas/Node"}))
        );
    }

    #[test]
    fn stripped_inlining_ignores_documentation() {
        let doc = json!({"components": {"schemas": {"A": {"type": "string", "description": "a"}}}});
        let inlined = inline(
            &doc,
            &json!({"$ref": "#/components/schemas/A", "x-note": 1}),
            true,
        );
        assert_eq!(inlined, json!({"type": "string"}));
    }

    #[test]
    fn nodes_resolve_to_the_target_pointer() {
        let doc = json!({"a": {"$ref": "#/b"}, "b": {"$ref": "#/c"}, "c": {"type": "string"}});
        let node = Node::at(&doc, "/a").unwrap().resolved(&doc);
        assert_eq!(node.pointer, "/c");
        let broken = json!({"a": {"$ref": "#/missing"}});
        assert_eq!(
            Node::at(&broken, "/a").unwrap().resolved(&broken).pointer,
            "/a"
        );
    }
}
