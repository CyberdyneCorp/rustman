//! Task 1.7: synthesized values validate against the schema they were made
//! from (a property test over generated schemas), and the circular-schema
//! scenario stops at the depth cap.

use proptest::prelude::*;
use rustman_explore::openapi::schema::{validator, violations};
use rustman_explore::synth::{MAX_DEPTH, Synthesizer, body_example};
use serde_json::{Map, Value, json};

const FORMATS: [&str; 8] = [
    "date-time",
    "date",
    "email",
    "uuid",
    "uri",
    "ipv4",
    "ipv6",
    "hostname",
];

fn string_schema() -> impl Strategy<Value = Value> {
    (
        prop::option::of(0u64..12),
        prop::option::of(0u64..12),
        prop::option::of(prop::sample::select(FORMATS.to_vec())),
    )
        .prop_map(|(min, extra, format)| {
            let mut s = Map::new();
            s.insert("type".into(), json!("string"));
            if let Some(format) = format {
                s.insert("format".into(), json!(format));
            } else {
                if let Some(min) = min {
                    s.insert("minLength".into(), json!(min));
                }
                if let Some(extra) = extra {
                    s.insert("maxLength".into(), json!(min.unwrap_or(0) + extra));
                }
            }
            Value::Object(s)
        })
}

fn integer_schema() -> impl Strategy<Value = Value> {
    (
        prop::option::of(-50i64..50),
        prop::option::of(0i64..50),
        any::<bool>(),
        prop::option::of(1i64..7),
    )
        .prop_map(|(low, span, exclusive, multiple)| {
            let mut s = Map::new();
            s.insert("type".into(), json!("integer"));
            let (min_key, max_key) = if exclusive {
                ("exclusiveMinimum", "exclusiveMaximum")
            } else {
                ("minimum", "maximum")
            };
            if let Some(low) = low {
                s.insert(min_key.into(), json!(low));
                match (span, multiple) {
                    (_, Some(m)) => {
                        s.insert("multipleOf".into(), json!(m));
                    }
                    (Some(span), None) => {
                        s.insert(max_key.into(), json!(low + span + 2));
                    }
                    (None, None) => {}
                }
            } else if let Some(span) = span {
                s.insert(max_key.into(), json!(span - 25));
            }
            Value::Object(s)
        })
}

fn number_schema() -> impl Strategy<Value = Value> {
    (
        prop::option::of(-100.0f64..100.0),
        0.5f64..50.0,
        any::<bool>(),
    )
        .prop_map(|(low, span, exclusive)| match (low, exclusive) {
            (None, _) => json!({"type": "number"}),
            (Some(low), false) => {
                json!({"type": "number", "minimum": low, "maximum": low + span})
            }
            (Some(low), true) => {
                json!({"type": "number", "exclusiveMinimum": low, "exclusiveMaximum": low + span})
            }
        })
}

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        string_schema(),
        integer_schema(),
        number_schema(),
        Just(json!({"type": "boolean"})),
        Just(json!({"type": ["string", "null"]})),
        prop::collection::vec("[a-z]{1,6}", 1..4).prop_map(|v| json!({"enum": v})),
    ]
}

/// Schemas nested at most two containers deep, within the depth cap.
fn schema() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(2, 24, 5, |inner| {
        prop_oneof![
            (inner.clone(), 0u64..3, prop::option::of(0u64..3)).prop_map(|(items, min, extra)| {
                let mut s = json!({"type": "array", "items": items, "minItems": min});
                if let Some(extra) = extra {
                    s["maxItems"] = json!(min + extra);
                }
                s
            }),
            prop::collection::btree_map("[a-z]{1,8}", (inner, any::<bool>()), 0..5).prop_map(
                |props| {
                    let required: Vec<&String> = props
                        .iter()
                        .filter(|(_, (_, r))| *r)
                        .map(|(n, _)| n)
                        .collect();
                    let properties: Map<String, Value> = props
                        .iter()
                        .map(|(n, (s, _))| (n.clone(), s.clone()))
                        .collect();
                    json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": required,
                        "properties": properties,
                    })
                }
            ),
        ]
    })
}

/// `schema` behind a `$ref`, an `allOf` or as is.
fn wrapped() -> impl Strategy<Value = (Value, Value)> {
    (schema(), 0u8..3).prop_map(|(schema, how)| match how {
        0 => (schema, json!({})),
        1 => (
            json!({"$ref": "#/components/schemas/S"}),
            json!({"schemas": {"S": schema}}),
        ),
        _ => (json!({"allOf": [schema]}), json!({})),
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn synthesized_values_validate_against_their_schema((schema, components) in wrapped()) {
        let value = Synthesizer::new(&components).value(&schema);
        let validator = validator(&schema, &components).unwrap();
        let found = violations(&validator, &value);
        prop_assert!(found.is_empty(), "{schema} -> {value}: {found:?}");
    }
}

/// How deeply objects and arrays nest in `value`.
fn depth(value: &Value) -> usize {
    match value {
        Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

#[test]
fn the_circular_schema_stops_at_a_nesting_depth_of_3() {
    let components = json!({"schemas": {"Node": {
        "type": "object",
        "required": ["name"],
        "properties": {
            "name": {"type": "string"},
            "children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}},
        },
    }}});
    let node = json!({"$ref": "#/components/schemas/Node"});
    let content = vec![rustman_explore::openapi::model::MediaType {
        media_type: "application/json".into(),
        schema: Some(json!({"type": "array", "items": node})),
        example: None,
        examples: Vec::new(),
    }];
    let chosen = body_example(&content, &components).unwrap();
    assert!(chosen.synthesized);
    assert_eq!(chosen.value, json!([{"name": "string"}]));
    let without_required = json!({"schemas": {"Node": {"type": "object", "properties": {
        "children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}},
    }}}});
    let value = Synthesizer::new(&without_required).value(&node);
    assert_eq!(depth(&value), MAX_DEPTH + 1, "{value}");
}

#[test]
fn the_spec_scenario_body_is_valid() {
    let schema = json!({"type": "object", "required": ["name", "age"], "properties": {
        "name": {"type": "string"},
        "age": {"type": "integer", "minimum": 0},
    }});
    let value = Synthesizer::new(&json!({})).value(&schema);
    assert!(value["name"].is_string());
    assert!(value["age"].as_i64().is_some_and(|a| a >= 0));
}
