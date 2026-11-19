//! Seeded variation of synthesized values: free-form strings get a suffix,
//! numbers move up inside their bounds and booleans flip, each driven by
//! the next pick of a deterministic stream. Values with a `format` or a
//! `pattern`, and documented values, are left alone.

use std::cell::Cell;

use serde_json::{Map, Value};

use super::{number, string};

/// A deterministic stream of picks (SplitMix64).
pub(super) struct Stream(Cell<u64>);

impl Stream {
    pub fn new(seed: u64) -> Self {
        Self(Cell::new(seed))
    }

    pub fn next(&self) -> u64 {
        let state = self.0.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.0.set(state);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// `value`, synthesized for `schema`, varied by the next pick of `stream`.
pub(super) fn vary(schema: &Map<String, Value>, value: Value, stream: &Stream) -> Value {
    let pick = stream.next();
    match value {
        Value::String(_) if is_free_form(schema) => {
            let suffix = format!("-{:06x}", pick & 0xFF_FFFF);
            Value::String(string::synthesize_with(schema, &suffix))
        }
        Value::Number(_) => number::shift(schema, &value, pick),
        Value::Bool(_) => Value::Bool(pick & 1 == 0),
        other => other,
    }
}

/// A string whose content nothing constrains but its length.
fn is_free_form(schema: &Map<String, Value>) -> bool {
    !schema.contains_key("format") && !schema.contains_key("pattern")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::Synthesizer;

    #[test]
    fn the_same_seed_gives_the_same_value_and_another_seed_another() {
        let schema = json!({"type": "object", "required": ["name", "age", "ok", "id"], "properties": {
            "name": {"type": "string", "maxLength": 20},
            "age": {"type": "integer", "minimum": 0, "maximum": 120},
            "ok": {"type": "boolean"},
            "id": {"type": "string", "format": "uuid"},
        }});
        let components = json!({});
        let synth = Synthesizer::new(&components);
        let a = synth.seeded(1).value(&schema);
        assert_eq!(a, synth.seeded(1).value(&schema));
        assert_ne!(a, synth.seeded(2).value(&schema));
        assert_eq!(a["id"], json!("00000000-0000-4000-8000-000000000000"));
        let name = a["name"].as_str().unwrap();
        assert!(name.starts_with("string-") && name.len() <= 20, "{name}");
        let age = a["age"].as_i64().unwrap();
        assert!((0..=120).contains(&age), "{age}");
    }

    #[test]
    fn unseeded_values_are_the_fixed_samples() {
        let components = json!({});
        let value = Synthesizer::new(&components).value(&json!({"type": "string"}));
        assert_eq!(value, json!("string"));
    }
}
