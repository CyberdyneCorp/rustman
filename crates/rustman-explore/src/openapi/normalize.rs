//! The OpenAPI 3.0 → 3.1 schema rewrite (design D1): `nullable` becomes a
//! `null` type, and boolean `exclusiveMinimum`/`exclusiveMaximum` become the
//! numeric 2020-12 form. Later stages only ever see 3.1 schemas.

use serde_json::{Map, Value, json};

/// Rewrite one 3.0 schema object in place (not its subschemas).
pub fn schema_3_0(schema: &mut Value) {
    let Some(map) = schema.as_object_mut() else {
        return;
    };
    exclusive_bound(map, "exclusiveMinimum", "minimum");
    exclusive_bound(map, "exclusiveMaximum", "maximum");
    if let Some(Value::Bool(true)) = map.remove("nullable") {
        nullable(schema);
    }
}

/// `exclusiveMinimum: true` with `minimum: n` becomes `exclusiveMinimum: n`.
fn exclusive_bound(map: &mut Map<String, Value>, exclusive: &str, bound: &str) {
    match map.get(exclusive) {
        Some(Value::Bool(true)) => match map.remove(bound) {
            Some(limit) => {
                map.insert(exclusive.to_owned(), limit);
            }
            None => {
                map.remove(exclusive);
            }
        },
        Some(Value::Bool(false)) => {
            map.remove(exclusive);
        }
        _ => {}
    }
}

/// Let `schema` (with `nullable` already removed) accept `null`.
fn nullable(schema: &mut Value) {
    let Some(map) = schema.as_object_mut() else {
        return;
    };
    if let Some(Value::String(single)) = map.get("type") {
        let single = Value::String(single.clone());
        map.insert("type".into(), json!([single, "null"]));
        add_null_to_enum(map);
    } else if map.contains_key("enum") {
        add_null_to_enum(map);
    } else if !map.contains_key("type") {
        let inner = Value::Object(std::mem::take(map));
        *schema = json!({"anyOf": [inner, {"type": "null"}]});
    }
}

fn add_null_to_enum(map: &mut Map<String, Value>) {
    if let Some(Value::Array(values)) = map.get_mut("enum")
        && !values.contains(&Value::Null)
    {
        values.push(Value::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(mut schema: Value) -> Value {
        schema_3_0(&mut schema);
        schema
    }

    #[test]
    fn nullable_becomes_a_null_type() {
        assert_eq!(
            rewritten(json!({"type": "string", "nullable": true})),
            json!({"type": ["string", "null"]})
        );
        assert_eq!(
            rewritten(json!({"type": "string", "enum": ["a"], "nullable": true})),
            json!({"type": ["string", "null"], "enum": ["a", null]})
        );
        assert_eq!(
            rewritten(json!({"$ref": "#/components/schemas/Pet", "nullable": true})),
            json!({"anyOf": [{"$ref": "#/components/schemas/Pet"}, {"type": "null"}]})
        );
        assert_eq!(
            rewritten(json!({"type": "string", "nullable": false})),
            json!({"type": "string"})
        );
    }

    #[test]
    fn boolean_exclusive_bounds_become_numbers() {
        assert_eq!(
            rewritten(
                json!({"minimum": 0, "exclusiveMinimum": true, "maximum": 9, "exclusiveMaximum": false})
            ),
            json!({"exclusiveMinimum": 0, "maximum": 9})
        );
        assert_eq!(
            rewritten(json!({"exclusiveMaximum": 5})),
            json!({"exclusiveMaximum": 5})
        );
    }
}
