//! A walk over an OpenAPI document that knows which objects are schemas, so
//! reference rewriting and the 3.0 → 3.1 schema rewrite never touch example
//! data, and schema keywords are only interpreted inside schemas.

use serde_json::Value;

use super::pointer;

/// What a value is, as far as the walk is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// An OpenAPI object (paths, operations, parameters, ...).
    Doc,
    /// A JSON Schema.
    Schema,
    /// A mapping whose values are schemas (`properties`, `components/schemas`).
    SchemaMap,
}

/// Visit every OpenAPI and schema object under `value`, children first.
/// `at` is the pointer of `value`. Example and default values, enums,
/// constants and `x-` extensions are not entered.
pub fn walk<E>(
    value: &mut Value,
    mode: Mode,
    at: &str,
    visit: &mut dyn FnMut(&mut Value, Mode, &str) -> Result<(), E>,
) -> Result<(), E> {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if let Some(child_mode) = child_mode(mode, at, key) {
                    walk(child, child_mode, &pointer::push(at, key), visit)?;
                }
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                walk(item, mode, &pointer::push(at, &i.to_string()), visit)?;
            }
            return Ok(());
        }
        _ => return Ok(()),
    }
    match mode {
        Mode::SchemaMap => Ok(()),
        mode => visit(value, mode, at),
    }
}

/// The mode of the value under `key` in an object of mode `mode` at `at`;
/// `None` for values that are data, not OpenAPI or schema objects.
fn child_mode(mode: Mode, at: &str, key: &str) -> Option<Mode> {
    match mode {
        Mode::Doc => doc_child(at, key),
        Mode::Schema => schema_child(key),
        Mode::SchemaMap => Some(Mode::Schema),
    }
}

fn doc_child(at: &str, key: &str) -> Option<Mode> {
    match key {
        "example" | "default" | "value" | "enum" | "const" => None,
        _ if key.starts_with("x-") => None,
        "schema" => Some(Mode::Schema),
        "schemas" if at == "/components" => Some(Mode::SchemaMap),
        _ => Some(Mode::Doc),
    }
}

fn schema_child(key: &str) -> Option<Mode> {
    match key {
        "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas" => {
            Some(Mode::SchemaMap)
        }
        "items"
        | "additionalProperties"
        | "not"
        | "if"
        | "then"
        | "else"
        | "contains"
        | "propertyNames"
        | "unevaluatedProperties"
        | "unevaluatedItems"
        | "additionalItems"
        | "contentSchema"
        | "allOf"
        | "anyOf"
        | "oneOf"
        | "prefixItems" => Some(Mode::Schema),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn visited(mut doc: Value) -> Vec<(String, Mode)> {
        let mut seen = Vec::new();
        walk::<()>(&mut doc, Mode::Doc, "", &mut |_, mode, at| {
            seen.push((at.to_owned(), mode));
            Ok(())
        })
        .unwrap();
        seen
    }

    #[test]
    fn schemas_are_told_apart_from_data() {
        let doc = json!({
            "components": {"schemas": {"Pet": {"properties": {"value": {"type": "string"}}}}},
            "paths": {"/p": {"get": {"responses": {"200": {"content": {"a/json": {
                "schema": {"items": {"type": "integer"}},
                "example": {"schema": {"nullable": true}},
            }}}}}}},
            "x-ext": {"schema": {}},
        });
        let seen = visited(doc);
        let schemas: Vec<&str> = seen
            .iter()
            .filter(|(_, m)| *m == Mode::Schema)
            .map(|(at, _)| at.as_str())
            .collect();
        assert_eq!(
            schemas,
            [
                "/components/schemas/Pet/properties/value",
                "/components/schemas/Pet",
                "/paths/~1p/get/responses/200/content/a~1json/schema/items",
                "/paths/~1p/get/responses/200/content/a~1json/schema",
            ]
        );
        assert!(
            !seen
                .iter()
                .any(|(at, _)| at.contains("example") || at.contains("x-ext"))
        );
        assert_eq!(seen.last().unwrap(), &(String::new(), Mode::Doc));
    }
}
