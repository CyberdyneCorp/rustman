//! JSON Schema for tool arguments, tool output and elicitation (design D4):
//! validation with the `jsonschema` crate (as the `schema` assertion uses)
//! reported as `<json-pointer>: <message>`, and the form model a front-end
//! renders from a normalized subset of the schema.

use std::fmt;

use jsonschema::error::{TypeKind, ValidationErrorKind};
use jsonschema::{JsonType, ValidationError};
use serde::Serialize;
use serde_json::{Map, Value};

/// One way an instance breaks its schema, e.g. `/path: required property
/// missing`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Violation {
    /// A JSON pointer into the instance; `/` for the root.
    pub pointer: String,
    pub message: String,
}

impl Violation {
    pub fn new(pointer: impl Into<String>, message: impl Into<String>) -> Self {
        let pointer = pointer.into();
        Self {
            pointer: if pointer.is_empty() {
                "/".to_owned()
            } else {
                pointer
            },
            message: message.into(),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.pointer, self.message)
    }
}

/// Every violation of `schema` by `instance`, formats included. An invalid
/// schema is an error naming the problem.
pub fn validate(schema: &Value, instance: &Value) -> Result<Vec<Violation>, String> {
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .map_err(|e| format!("invalid schema: {e}"))?;
    Ok(validator.iter_errors(instance).map(violation).collect())
}

fn violation(error: ValidationError<'_>) -> Violation {
    let at = error.instance_path().as_str().to_owned();
    match error.kind() {
        ValidationErrorKind::Required { property } => {
            let name = property
                .as_str()
                .map_or_else(|| property.to_string(), str::to_owned);
            let escaped = name.replace('~', "~0").replace('/', "~1");
            Violation::new(format!("{at}/{escaped}"), "required property missing")
        }
        ValidationErrorKind::Type { kind } => Violation::new(at, expected(kind)),
        _ => Violation::new(at, error.to_string()),
    }
}

fn expected(kind: &TypeKind) -> String {
    let names: Vec<&str> = match kind {
        TypeKind::Single(t) => vec![t.as_str()],
        TypeKind::Multiple(set) => set.iter().map(JsonType::as_str).collect(),
    };
    format!("expected {}", names.join(" or "))
}

/// A form generated from a schema.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Field {
    /// The property name; empty for the root and for array items.
    pub name: String,
    /// Where the value goes, as a JSON pointer.
    pub pointer: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub required: bool,
    pub default: Option<Value>,
    pub kind: FieldKind,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    String {
        format: Option<String>,
    },
    Number,
    Integer,
    Boolean,
    /// A picker with exactly these values (`enum`, or one `const`).
    Enum {
        values: Vec<Value>,
    },
    Object {
        fields: Vec<Field>,
    },
    /// Items are added and removed; each follows `item`.
    Array {
        item: Box<Field>,
    },
    /// `oneOf` / `anyOf`: pick a variant, then fill it.
    Variant {
        variants: Vec<Field>,
    },
    /// Not representable as a form: edited as raw JSON.
    Raw {
        schema: Value,
    },
}

/// The form of `schema`, rooted at `/`.
pub fn form(schema: &Value) -> Field {
    field(schema, String::new(), String::new(), false)
}

fn field(schema: &Value, name: String, pointer: String, required: bool) -> Field {
    let text = |key: &str| schema.get(key).and_then(Value::as_str).map(str::to_owned);
    Field {
        title: text("title"),
        description: text("description"),
        default: schema.get("default").cloned(),
        kind: kind(schema, &pointer),
        name,
        pointer,
        required,
    }
}

/// Keywords a form can render; any other one sends the subtree to the raw
/// editor.
const FORM_KEYWORDS: &[&str] = &[
    "type",
    "enum",
    "const",
    "properties",
    "required",
    "items",
    "default",
    "description",
    "title",
    "format",
    "oneOf",
    "anyOf",
    "$schema",
    "additionalProperties",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "pattern",
    "examples",
];

fn kind(schema: &Value, pointer: &str) -> FieldKind {
    let Some(object) = schema.as_object() else {
        return FieldKind::Raw {
            schema: schema.clone(),
        };
    };
    if object.keys().any(|k| !FORM_KEYWORDS.contains(&k.as_str())) {
        return FieldKind::Raw {
            schema: schema.clone(),
        };
    }
    if let Some(values) = choices(object) {
        return FieldKind::Enum { values };
    }
    if let Some(variants) = object.get("oneOf").or_else(|| object.get("anyOf")) {
        return variant(variants, pointer);
    }
    typed(object, pointer)
}

fn choices(object: &Map<String, Value>) -> Option<Vec<Value>> {
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return Some(values.clone());
    }
    object.get("const").map(|value| vec![value.clone()])
}

fn variant(variants: &Value, pointer: &str) -> FieldKind {
    let Some(list) = variants.as_array() else {
        return FieldKind::Raw {
            schema: variants.clone(),
        };
    };
    let variants = list
        .iter()
        .map(|schema| field(schema, String::new(), pointer.to_owned(), false))
        .collect();
    FieldKind::Variant { variants }
}

fn typed(object: &Map<String, Value>, pointer: &str) -> FieldKind {
    match object.get("type").and_then(Value::as_str) {
        Some("string") => FieldKind::String {
            format: object
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        Some("number") => FieldKind::Number,
        Some("integer") => FieldKind::Integer,
        Some("boolean") => FieldKind::Boolean,
        Some("array") => array(object, pointer),
        Some("object") => FieldKind::Object {
            fields: properties(object, pointer),
        },
        None if object.contains_key("properties") => FieldKind::Object {
            fields: properties(object, pointer),
        },
        _ => FieldKind::Raw {
            schema: Value::Object(object.clone()),
        },
    }
}

fn array(object: &Map<String, Value>, pointer: &str) -> FieldKind {
    let items = object.get("items").cloned().unwrap_or(Value::Bool(true));
    FieldKind::Array {
        item: Box::new(field(&items, String::new(), format!("{pointer}/-"), false)),
    }
}

fn properties(object: &Map<String, Value>, pointer: &str) -> Vec<Field> {
    let required: Vec<&str> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = object.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, schema)| {
            let escaped = name.replace('~', "~0").replace('/', "~1");
            field(
                schema,
                name.clone(),
                format!("{pointer}/{escaped}"),
                required.contains(&name.as_str()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn messages(schema: Value, instance: Value) -> Vec<String> {
        validate(&schema, &instance)
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_missing_required_property_is_named_by_pointer() {
        let schema = json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}});
        assert_eq!(
            messages(schema, json!({})),
            ["/path: required property missing"]
        );
    }

    #[test]
    fn wrong_types_name_the_expected_type() {
        let schema = json!({"type": "object", "properties": {"count": {"type": "integer"}}});
        assert_eq!(
            messages(schema, json!({"count": "3"})),
            ["/count: expected integer"]
        );
        let nested =
            json!({"type": "object", "properties": {"a": {"type": "object", "required": ["b"]}}});
        assert_eq!(
            messages(nested, json!({"a": {}})),
            ["/a/b: required property missing"]
        );
    }

    #[test]
    fn formats_are_checked() {
        let schema = json!({"type": "object", "properties": {"email": {"type": "string", "format": "email"}}});
        assert!(messages(schema.clone(), json!({"email": "a@example.com"})).is_empty());
        let bad = messages(schema, json!({"email": "not an email"}));
        assert_eq!(bad.len(), 1);
        assert!(bad[0].starts_with("/email: "), "{bad:?}");
    }

    #[test]
    fn an_invalid_schema_is_an_error() {
        let err = validate(&json!({"type": 12}), &json!(1)).unwrap_err();
        assert!(err.starts_with("invalid schema: "), "{err}");
    }

    #[test]
    fn enums_become_pickers_with_exactly_their_values() {
        let schema = json!({"type": "object", "properties": {"mode": {"enum": ["fast", "safe"]}}});
        let FieldKind::Object { fields } = form(&schema).kind else {
            panic!("an object");
        };
        assert_eq!(fields[0].name, "mode");
        assert_eq!(
            fields[0].kind,
            FieldKind::Enum {
                values: vec![json!("fast"), json!("safe")]
            }
        );
    }

    #[test]
    fn nested_objects_arrays_required_and_defaults() {
        let schema = json!({
            "type": "object",
            "required": ["user"],
            "properties": {
                "user": {
                    "type": "object",
                    "description": "who",
                    "properties": {"age": {"type": "integer", "default": 3}}
                },
                "tags": {"type": "array", "items": {"type": "string"}},
                "flag": {"type": "boolean"}
            }
        });
        let FieldKind::Object { fields } = form(&schema).kind else {
            panic!("an object");
        };
        let user = fields.iter().find(|f| f.name == "user").unwrap();
        assert!(user.required);
        assert_eq!(user.description.as_deref(), Some("who"));
        let FieldKind::Object { fields: inner } = &user.kind else {
            panic!("a nested object");
        };
        assert_eq!(inner[0].pointer, "/user/age");
        assert_eq!(inner[0].kind, FieldKind::Integer);
        assert_eq!(inner[0].default, Some(json!(3)));
        assert!(!inner[0].required);
        let tags = fields.iter().find(|f| f.name == "tags").unwrap();
        let FieldKind::Array { item } = &tags.kind else {
            panic!("an array");
        };
        assert_eq!(item.kind, FieldKind::String { format: None });
        assert_eq!(item.pointer, "/tags/-");
    }

    #[test]
    fn variants_and_unsupported_keywords() {
        let schema = json!({"oneOf": [{"type": "string"}, {"type": "integer"}]});
        let FieldKind::Variant { variants } = form(&schema).kind else {
            panic!("a variant picker");
        };
        assert_eq!(variants.len(), 2);
        let raw = json!({"type": "object", "patternProperties": {"^x": {}}});
        assert!(matches!(form(&raw).kind, FieldKind::Raw { .. }));
    }
}
