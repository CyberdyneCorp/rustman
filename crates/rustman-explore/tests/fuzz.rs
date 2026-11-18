//! Task 3.6: fuzz values from schemas. Valid-mode values validate against
//! their schema; boundary-mode values sit on the minimum or maximum.

use proptest::prelude::*;
use rustman_explore::explore::fuzz::{Fuzzer, Mode, runner};
use rustman_explore::openapi::schema::{validator, violations};
use serde_json::{Value, json};

fn components() -> Value {
    json!({"schemas": {
        "Address": {"type": "object", "required": ["city"], "properties": {
            "city": {"type": "string", "minLength": 1, "maxLength": 20},
            "zip": {"type": "string", "pattern": "^[0-9]{5}$"},
        }},
        "Base": {"type": "object", "required": ["id"], "properties": {"id": {"type": "string", "format": "uuid"}}},
    }})
}

fn schemas() -> Vec<Value> {
    vec![
        json!({"type": "integer", "minimum": -5, "maximum": 5}),
        json!({"type": "integer", "exclusiveMinimum": 0, "multipleOf": 7}),
        json!({"type": "integer", "format": "int32"}),
        json!({"type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1}),
        json!({"type": "number", "minimum": 2.5}),
        json!({"type": "string", "minLength": 3, "maxLength": 8}),
        json!({"type": "string", "pattern": "^[A-Z]{3}-[0-9]{2}$"}),
        json!({"type": "string", "format": "date-time"}),
        json!({"type": "string", "format": "email", "maxLength": 40}),
        json!({"type": ["string", "null"], "enum": ["a", "b", null]}),
        json!({"type": "boolean"}),
        json!({"type": "array", "items": {"type": "integer", "minimum": 0}, "minItems": 1, "maxItems": 4}),
        json!({"type": "array", "items": {"enum": [1, 2, 3, 4, 5]}, "uniqueItems": true, "maxItems": 3}),
        json!({"$ref": "#/components/schemas/Address"}),
        json!({"type": "object", "additionalProperties": false, "required": ["name", "address"], "properties": {
            "name": {"type": "string", "maxLength": 10},
            "age": {"type": "integer", "minimum": 0, "maximum": 130},
            "address": {"$ref": "#/components/schemas/Address"},
            "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 2},
        }}),
        json!({"allOf": [{"$ref": "#/components/schemas/Base"}, {"type": "object", "required": ["n"], "properties": {"n": {"type": "integer", "maximum": -1}}}]}),
        json!({"oneOf": [{"type": "null"}, {"type": "integer", "minimum": 10, "maximum": 10}]}),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn valid_mode_values_validate(seed in any::<u64>()) {
        let components = components();
        let fuzzer = Fuzzer::new(&components);
        for (i, schema) in schemas().iter().enumerate() {
            let value = fuzzer.sample(schema, Mode::Valid, &mut runner(seed, &i.to_string()));
            let check = validator(schema, &components).unwrap();
            let found = violations(&check, &value);
            prop_assert!(found.is_empty(), "schema {i}: {value} breaks {found:?}");
        }
    }

    #[test]
    fn boundary_mode_values_hit_min_or_max(seed in any::<u64>()) {
        let components = components();
        let fuzzer = Fuzzer::new(&components);
        let draw = |schema: Value| fuzzer.sample(&schema, Mode::Boundary, &mut runner(seed, "b"));
        let n = draw(json!({"type": "integer", "minimum": 3, "maximum": 9}));
        prop_assert!(n == json!(3) || n == json!(9), "{n}");
        let x = draw(json!({"type": "number", "minimum": -1.5, "maximum": 2.5}));
        prop_assert!(x == json!(-1.5) || x == json!(2.5), "{x}");
        let s = draw(json!({"type": "string", "minLength": 2, "maxLength": 5}));
        let len = s.as_str().unwrap().chars().count();
        prop_assert!(len == 2 || len == 5, "{s}");
        let a = draw(json!({"type": "array", "items": {"type": "boolean"}, "minItems": 1, "maxItems": 4}));
        let len = a.as_array().unwrap().len();
        prop_assert!(len == 1 || len == 4, "{a}");
        let o = draw(json!({"type": "object", "properties": {"q": {"type": "integer", "minimum": 0, "maximum": 10}}}));
        prop_assert!(o["q"] == json!(0) || o["q"] == json!(10), "{o}");
    }

    #[test]
    fn boundary_mode_values_validate(seed in any::<u64>()) {
        let components = components();
        let fuzzer = Fuzzer::new(&components);
        for (i, schema) in schemas().iter().enumerate() {
            let value = fuzzer.sample(schema, Mode::Boundary, &mut runner(seed, &i.to_string()));
            let check = validator(schema, &components).unwrap();
            let found = violations(&check, &value);
            prop_assert!(found.is_empty(), "schema {i}: {value} breaks {found:?}");
        }
    }
}

#[test]
fn both_bounds_are_reached() {
    let components = json!({});
    let fuzzer = Fuzzer::new(&components);
    let schema = json!({"type": "integer", "minimum": 3, "maximum": 9});
    let drawn: Vec<Value> = (0..64)
        .map(|seed| fuzzer.sample(&schema, Mode::Boundary, &mut runner(seed, "x")))
        .collect();
    assert!(drawn.contains(&json!(3)) && drawn.contains(&json!(9)));
}
