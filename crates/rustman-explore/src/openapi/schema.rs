//! JSON Schema validation of values against the raw schemas of an
//! [`ApiModel`](super::model::ApiModel), with their `$ref`s into
//! `components` resolved.

use jsonschema::{ValidationError, Validator};
use serde_json::{Value, json};

/// A validator for `schema`, whose `#/components/...` references point into
/// `components`. Remote references are never fetched.
pub fn validator(schema: &Value, components: &Value) -> Result<Validator, String> {
    let root = json!({"allOf": [schema], "components": components});
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&root)
        .map_err(|e| e.to_string())
}

/// The first violation of `instance` against `validator`, if any. Only
/// the first is reported: a small schema (nested required `$ref`s under
/// `allOf`) can make the number of violations grow exponentially, and
/// collecting them all would exhaust memory.
fn first_error<'i>(validator: &Validator, instance: &'i Value) -> Option<ValidationError<'i>> {
    validator.validate(instance).err()
}

/// The violation of `instance` against `validator`, as `<path>: <message>`;
/// empty when it is valid. See [`first_error`].
pub fn violations(validator: &Validator, instance: &Value) -> Vec<String> {
    first_error(validator, instance)
        .map(|e| {
            let path = e.instance_path().as_str();
            let path = if path.is_empty() { "/" } else { path };
            format!("{path}: {e}")
        })
        .into_iter()
        .collect()
}

/// One violation, with the pointers that locate it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The JSON pointer of the offending value in the instance (`""` is the
    /// whole instance).
    pub instance: String,
    /// The JSON pointer of the failing keyword: into the schema itself, or
    /// into the document (`/components/...`) when reached through a `$ref`.
    pub schema: String,
    pub message: String,
}

/// The violation of `instance` against `validator`, with its pointers;
/// empty when it is valid. See [`first_error`].
pub fn errors(validator: &Validator, instance: &Value) -> Vec<Violation> {
    first_error(validator, instance)
        .map(|e| {
            let schema = e.schema_path().as_str();
            let schema = schema.strip_prefix("/allOf/0").unwrap_or(schema);
            Violation {
                instance: e.instance_path().as_str().to_owned(),
                schema: schema.to_owned(),
                message: e.to_string(),
            }
        })
        .into_iter()
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn references_resolve_into_components() {
        let components = json!({"schemas": {
            "Pet": {"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}},
        }});
        let v = validator(&json!({"$ref": "#/components/schemas/Pet"}), &components).unwrap();
        assert!(violations(&v, &json!({"name": "Rex"})).is_empty());
        let found = violations(&v, &json!({"name": 3}));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("/name: "), "{found:?}");
    }

    #[test]
    fn errors_carry_instance_and_schema_pointers() {
        let components = json!({"schemas": {
            "User": {"type": "object", "required": ["email"], "properties": {"email": {"type": "string"}}},
        }});
        let v = validator(&json!({"$ref": "#/components/schemas/User"}), &components).unwrap();
        let found = errors(&v, &json!({"name": "Ana"}));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].instance, "");
        assert_eq!(found[0].schema, "/components/schemas/User/required");
        assert!(
            found[0]
                .message
                .contains("\"email\" is a required property"),
            "{found:?}"
        );
        let inline = validator(
            &json!({"type": "object", "properties": {"n": {"type": "integer"}}}),
            &json!({}),
        )
        .unwrap();
        let found = errors(&inline, &json!({"n": "x"}));
        assert_eq!(
            (found[0].instance.as_str(), found[0].schema.as_str()),
            ("/n", "/properties/n/type")
        );
    }

    /// Twelve schemas of twenty required properties, each a `$ref` to the
    /// next and an `allOf` of it: the violations of an empty-ish instance
    /// grow exponentially, so only the first may be computed.
    pub(crate) fn fan_out_components() -> Value {
        let mut schemas = serde_json::Map::new();
        for level in 0..12 {
            let next = json!({"$ref": format!("#/components/schemas/S{}", level + 1)});
            let names: Vec<String> = (0..20).map(|i| format!("f{i}")).collect();
            let properties: serde_json::Map<String, Value> =
                names.iter().map(|n| (n.clone(), next.clone())).collect();
            schemas.insert(
                format!("S{level}"),
                json!({"type": "object", "required": names, "properties": properties, "allOf": [next]}),
            );
        }
        schemas.insert(
            "S12".into(),
            json!({"type": "object", "required": ["leaf"]}),
        );
        json!({"schemas": schemas})
    }

    #[test]
    fn exponential_violations_are_not_collected() {
        let components = fan_out_components();
        let v = validator(&json!({"$ref": "#/components/schemas/S0"}), &components).unwrap();
        let started = std::time::Instant::now();
        let instance = json!({"f0": {"f0": {}}});
        assert_eq!(violations(&v, &instance).len(), 1);
        assert_eq!(errors(&v, &instance).len(), 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn a_circular_schema_compiles() {
        let components = json!({"schemas": {
            "Node": {"type": "object", "properties": {"children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}}}},
        }});
        let v = validator(&json!({"$ref": "#/components/schemas/Node"}), &components).unwrap();
        assert!(violations(&v, &json!({"children": [{"children": []}]})).is_empty());
        assert!(!violations(&v, &json!({"children": [{"children": 1}]})).is_empty());
    }
}
