//! Document validation before anything is generated: structural errors
//! (which stop the import) and warnings (which do not), each located by a
//! JSON pointer.

use std::collections::HashMap;

use serde_json::Value;

use super::diagnostic::Diagnostic;
use super::document::SpecVersion;
use super::lower::{METHODS, deref};
use super::pointer;
use super::schema;
use super::walk::{Mode, walk};

/// Every problem found in `doc`, a bundled, 3.1-shaped document.
pub fn validate(doc: &Value, version: SpecVersion) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if !doc.is_object() {
        out.push(Diagnostic::error("", "the document is not an object"));
        return out;
    }
    info(doc, &mut out);
    references(doc, &mut out);
    paths(doc, version, &mut out);
    examples(doc, &mut out);
    out
}

fn info(doc: &Value, out: &mut Vec<Diagnostic>) {
    let Some(info) = doc.get("info").filter(|i| i.is_object()) else {
        out.push(Diagnostic::error(
            "/info",
            "required object 'info' is missing",
        ));
        return;
    };
    for field in ["title", "version"] {
        if !info.get(field).is_some_and(Value::is_string) {
            let at = pointer::push("/info", field);
            out.push(Diagnostic::error(
                at,
                format!("required string '{field}' is missing"),
            ));
        }
    }
}

/// Every local `$ref` must point at something.
fn references(doc: &Value, out: &mut Vec<Diagnostic>) {
    let mut copy = doc.clone();
    let _ = walk::<()>(&mut copy, Mode::Doc, "", &mut |node, _, at| {
        if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            let found = reference
                .strip_prefix('#')
                .and_then(pointer::from_fragment)
                .is_some_and(|p| doc.pointer(&p).is_some());
            if !found {
                let message = format!("unresolved reference '{reference}'");
                out.push(Diagnostic::error(pointer::push(at, "$ref"), message));
            }
        }
        Ok(())
    });
}

fn paths(doc: &Value, version: SpecVersion, out: &mut Vec<Diagnostic>) {
    let Some(paths) = doc.get("paths") else {
        if version == SpecVersion::V3_0 {
            out.push(Diagnostic::error(
                "/paths",
                "required object 'paths' is missing",
            ));
        }
        return;
    };
    let Some(paths) = paths.as_object() else {
        out.push(Diagnostic::error("/paths", "'paths' is not an object"));
        return;
    };
    let mut ids = HashMap::new();
    for path in paths.keys() {
        let at = pointer::push("/paths", path);
        if !path.starts_with('/') {
            out.push(Diagnostic::error(&at, "a path must start with '/'"));
            continue;
        }
        path_item(doc, path, &at, &mut ids, out);
    }
}

fn path_item(
    doc: &Value,
    path: &str,
    at: &str,
    ids: &mut HashMap<String, String>,
    out: &mut Vec<Diagnostic>,
) {
    let Ok((item_at, item)) = deref(doc, at) else {
        return;
    };
    if !item.is_object() {
        out.push(Diagnostic::error(at, "a path item must be an object"));
        return;
    }
    let shared = parameters(doc, &pointer::push(&item_at, "parameters"), out);
    for method in METHODS {
        let op_at = pointer::push(&item_at, method);
        match item.get(method) {
            Some(op) if op.is_object() => {
                let own = parameters(doc, &pointer::push(&op_at, "parameters"), out);
                operation(op, &op_at, ids, out);
                path_parameters(path, &op_at, &shared, &own, out);
            }
            Some(_) => out.push(Diagnostic::error(&op_at, "an operation must be an object")),
            None => {}
        }
    }
}

fn operation(op: &Value, at: &str, ids: &mut HashMap<String, String>, out: &mut Vec<Diagnostic>) {
    match op.get("operationId").and_then(Value::as_str) {
        None => out.push(Diagnostic::warning(
            at,
            "operation has no operationId; its request file is named from the method and path",
        )),
        Some(id) => {
            if let Some(first) = ids.insert(id.to_owned(), at.to_owned()) {
                let message = format!("operationId '{id}' is also used at {first}");
                out.push(Diagnostic::warning(at, message));
            }
        }
    }
    if op.get("responses").is_none() {
        out.push(Diagnostic::warning(at, "operation declares no responses"));
    }
}

/// A parameter's name and location (`path`, `query`, ...).
type ParameterKey = (String, String);

/// The parameters listed at `at`, checking each one's structure.
fn parameters(doc: &Value, at: &str, out: &mut Vec<Diagnostic>) -> Vec<(ParameterKey, bool)> {
    let Some(list) = doc.pointer(at) else {
        return Vec::new();
    };
    let Some(list) = list.as_array() else {
        out.push(Diagnostic::error(at, "'parameters' is not an array"));
        return Vec::new();
    };
    (0..list.len())
        .filter_map(|i| parameter(doc, &format!("{at}/{i}"), out))
        .collect()
}

fn parameter(doc: &Value, at: &str, out: &mut Vec<Diagnostic>) -> Option<(ParameterKey, bool)> {
    let (_, param) = deref(doc, at).ok()?;
    let name = param.get("name").and_then(Value::as_str);
    let location = param.get("in").and_then(Value::as_str);
    let valid = matches!(location, Some("path" | "query" | "header" | "cookie"));
    let (Some(name), Some(location), true) = (name, location, valid) else {
        let message = "a parameter needs a 'name' and 'in' (path, query, header or cookie)";
        out.push(Diagnostic::error(at, message));
        return None;
    };
    let required = param.get("required").and_then(Value::as_bool) == Some(true);
    if location == "path" && !required {
        let message = format!("path parameter '{name}' must be required");
        out.push(Diagnostic::warning(at, message));
    }
    Some(((name.to_owned(), location.to_owned()), required))
}

/// Every `{name}` in the path template is declared, and the other way round.
fn path_parameters(
    path: &str,
    at: &str,
    shared: &[(ParameterKey, bool)],
    own: &[(ParameterKey, bool)],
    out: &mut Vec<Diagnostic>,
) {
    let declared: Vec<&str> = shared
        .iter()
        .chain(own)
        .filter(|((_, location), _)| location == "path")
        .map(|((name, _), _)| name.as_str())
        .collect();
    let template = template_names(path);
    for name in &template {
        if !declared.contains(name) {
            let message = format!("path parameter '{name}' is not declared");
            out.push(Diagnostic::warning(at, message));
        }
    }
    for name in declared {
        if !template.contains(&name) {
            let message = format!("path parameter '{name}' is not in the path");
            out.push(Diagnostic::warning(at, message));
        }
    }
}

/// The `{name}`s of a path template, in order.
pub fn template_names(path: &str) -> Vec<&str> {
    path.split('{')
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
        .collect()
}

/// Examples of media types and parameters that don't match their schema.
fn examples(doc: &Value, out: &mut Vec<Diagnostic>) {
    let components = doc.get("components").cloned().unwrap_or(Value::Null);
    let mut copy = doc.clone();
    let _ = walk::<()>(&mut copy, Mode::Doc, "", &mut |node, mode, at| {
        if mode == Mode::Doc
            && let Some(schema) = node.get("schema")
        {
            check_examples(doc, node, schema, &components, at, out);
        }
        Ok(())
    });
}

fn check_examples(
    doc: &Value,
    node: &Value,
    schema: &Value,
    components: &Value,
    at: &str,
    out: &mut Vec<Diagnostic>,
) {
    let found = examples_of(doc, node, at);
    if found.is_empty() {
        return;
    }
    let validator = match schema::validator(schema, components) {
        Ok(validator) => validator,
        Err(e) => {
            let message = format!("the schema is not valid: {e}");
            out.push(Diagnostic::warning(pointer::push(at, "schema"), message));
            return;
        }
    };
    for (example_at, value) in found {
        if let Some(first) = schema::violations(&validator, &value).first() {
            let message = format!("example does not match its schema: {first}");
            out.push(Diagnostic::warning(example_at, message));
        }
    }
}

/// The `example` and `examples.*.value` of a media type or parameter at `at`.
fn examples_of(doc: &Value, node: &Value, at: &str) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    if let Some(example) = node.get("example") {
        found.push((pointer::push(at, "example"), example.clone()));
    }
    let names = node.get("examples").and_then(Value::as_object);
    for name in names.into_iter().flat_map(|m| m.keys()) {
        let example_at = pointer::push(&pointer::push(at, "examples"), name);
        if let Ok((target, example)) = deref(doc, &example_at)
            && let Some(value) = example.get("value")
        {
            found.push((pointer::push(&target, "value"), value.clone()));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_names_in_order() {
        assert_eq!(template_names("/a/{x}/b/{y}.json"), ["x", "y"]);
        assert!(template_names("/a").is_empty());
    }
}
