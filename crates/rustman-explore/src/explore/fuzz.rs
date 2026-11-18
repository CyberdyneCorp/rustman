//! Seeded fuzz values from JSON Schemas (design D7): `proptest` strategies
//! built from a schema, sampled with a ChaCha `TestRng` derived from the
//! exploration seed. Only proptest's value generation is used, not its test
//! runner or shrinking.
//!
//! [`Mode::Valid`] yields schema-shaped values inside every bound;
//! [`Mode::Boundary`] yields values on the bounds: numbers at their minimum
//! or maximum, strings and arrays at their minimum or maximum length.

use proptest::prelude::*;
use proptest::strategy::{BoxedStrategy, Just, ValueTree};
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::{Map, Number, Value};

use crate::synth::{MAX_DEPTH, Synthesizer, first_branch, type_of};

/// What kind of values to generate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Valid,
    Boundary,
}

/// Unbounded strings get at most this many characters more than their
/// minimum in valid mode, and exactly this many in boundary mode.
const OPEN_LENGTH: usize = 64;

/// Unbounded arrays get at most this many items more than their minimum.
const OPEN_ITEMS: usize = 3;

/// Unbounded integers are drawn from `-OPEN_RANGE..=OPEN_RANGE` in valid mode.
const OPEN_RANGE: i64 = 1000;

/// Samples for formats a regular expression can describe.
const FORMAT_PATTERNS: [(&str, &str); 6] = [
    (
        "uuid",
        "[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}",
    ),
    ("email", "[a-z]{1,12}@example\\.com"),
    ("date", "20[0-9]{2}-(0[1-9]|1[0-2])-(0[1-9]|1[0-9]|2[0-8])"),
    (
        "date-time",
        "20[0-9]{2}-(0[1-9]|1[0-2])-(0[1-9]|1[0-9]|2[0-8])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]Z",
    ),
    (
        "ipv4",
        "(1[0-9]{2}|2[0-4][0-9]|[1-9][0-9]?)(\\.(1[0-9]{2}|2[0-4][0-9]|[1-9]?[0-9])){3}",
    ),
    ("hostname", "[a-z]{1,16}\\.example\\.com"),
];

/// A deterministic proptest runner for `seed`, varied by `salt` (such as an
/// operation key and case number) so each use draws its own stream.
pub fn runner(seed: u64, salt: &str) -> TestRunner {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    bytes[8..16].copy_from_slice(&fnv1a(salt.as_bytes()).to_le_bytes());
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &bytes);
    TestRunner::new_with_rng(Config::default(), rng)
}

/// FNV-1a: a stable hash (unlike `DefaultHasher`, whose output may change
/// between Rust releases).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Builds strategies for schemas whose `$ref`s point into `components`.
#[derive(Clone, Copy, Debug)]
pub struct Fuzzer<'a> {
    synth: Synthesizer<'a>,
}

impl<'a> Fuzzer<'a> {
    pub fn new(components: &'a Value) -> Self {
        Self {
            synth: Synthesizer::new(components),
        }
    }

    /// One value for `schema` drawn from `runner`. When the strategy cannot
    /// produce a value (too many rejected draws), the synthesized example.
    pub fn sample(&self, schema: &Value, mode: Mode, runner: &mut TestRunner) -> Value {
        self.strategy(schema, mode)
            .new_tree(runner)
            .map(|tree| tree.current())
            .unwrap_or_else(|_| self.synth.value(schema))
    }

    /// The strategy for `schema`.
    pub fn strategy(&self, schema: &Value, mode: Mode) -> BoxedStrategy<Value> {
        self.at(schema, mode, 0)
    }

    fn at(&self, schema: &Value, mode: Mode, depth: usize) -> BoxedStrategy<Value> {
        if depth >= MAX_DEPTH {
            return Just(self.synth.value_at(schema, depth)).boxed();
        }
        let Some(object) = self.synth.object_view(schema) else {
            return Just(Value::Null).boxed();
        };
        if let Some(value) = object.get("const") {
            return Just(value.clone()).boxed();
        }
        if let Some(values) = object.get("enum").and_then(Value::as_array) {
            return choose(values.iter().filter(|v| !v.is_null()).cloned().collect());
        }
        if let Some(branch) = first_branch(&object) {
            return self.at(branch, mode, depth + 1);
        }
        match type_of(&object) {
            "string" => string(&object, mode).unwrap_or_else(|| self.fallback(schema)),
            "integer" => integer(&object, mode).unwrap_or_else(|| self.fallback(schema)),
            "number" => number(&object, mode).unwrap_or_else(|| self.fallback(schema)),
            "boolean" => any::<bool>().prop_map(Value::Bool).boxed(),
            "array" => self.array(&object, mode, depth),
            "object" => self.object(&object, mode, depth),
            _ => Just(Value::Null).boxed(),
        }
    }

    fn fallback(&self, schema: &Value) -> BoxedStrategy<Value> {
        Just(self.synth.value(schema)).boxed()
    }

    fn array(&self, schema: &Map<String, Value>, mode: Mode, depth: usize) -> BoxedStrategy<Value> {
        let min = count(schema, "minItems").unwrap_or(0);
        let max = count(schema, "maxItems").unwrap_or(min + OPEN_ITEMS);
        if max < min {
            return Just(Value::Array(Vec::new())).boxed();
        }
        let items = schema.get("items").cloned().unwrap_or(Value::Bool(true));
        let item = self.at(&items, mode, depth + 1);
        let unique = schema.get("uniqueItems").and_then(Value::as_bool) == Some(true);
        let lengths = match mode {
            Mode::Valid => (min..=max).boxed(),
            Mode::Boundary => choose_usize(vec![min, max]),
        };
        lengths
            .prop_flat_map(move |n| proptest::collection::vec(item.clone(), n))
            .prop_filter("unique items", move |v| !unique || all_unique(v))
            .prop_map(Value::Array)
            .boxed()
    }

    fn object(
        &self,
        schema: &Map<String, Value>,
        mode: Mode,
        depth: usize,
    ) -> BoxedStrategy<Value> {
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let properties = schema.get("properties").and_then(Value::as_object);
        let mut fields: Vec<BoxedStrategy<Option<(String, Value)>>> = Vec::new();
        for (name, property) in properties.into_iter().flatten() {
            let is_required = required.contains(&name.as_str());
            let read_only = property.get("readOnly").and_then(Value::as_bool) == Some(true);
            if read_only && !is_required {
                continue;
            }
            let name = name.clone();
            let value = self.at(property, mode, depth + 1);
            let field = value.prop_map(move |v| Some((name.clone(), v)));
            fields.push(if is_required || mode == Mode::Boundary {
                field.boxed()
            } else {
                prop_oneof![Just(None), field].boxed()
            });
        }
        for name in required {
            if !properties.is_some_and(|p| p.contains_key(name)) {
                fields.push(Just(Some((name.to_owned(), Value::String(String::new())))).boxed());
            }
        }
        fields
            .prop_map(|fields| Value::Object(fields.into_iter().flatten().collect()))
            .boxed()
    }
}

fn choose(values: Vec<Value>) -> BoxedStrategy<Value> {
    if values.is_empty() {
        return Just(Value::Null).boxed();
    }
    proptest::sample::select(values).boxed()
}

fn choose_usize(mut values: Vec<usize>) -> BoxedStrategy<usize> {
    values.dedup();
    proptest::sample::select(values).boxed()
}

fn all_unique(values: &[Value]) -> bool {
    values
        .iter()
        .enumerate()
        .all(|(i, v)| !values[i + 1..].contains(v))
}

fn count(schema: &Map<String, Value>, key: &str) -> Option<usize> {
    schema
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

/// Strings: from `pattern` when there is one, a format's sample pattern,
/// or alphanumerics; always within the length bounds.
fn string(schema: &Map<String, Value>, mode: Mode) -> Option<BoxedStrategy<Value>> {
    let min = count(schema, "minLength").unwrap_or(0);
    let max = count(schema, "maxLength");
    if max.is_some_and(|m| m < min) {
        return None;
    }
    let pattern = schema.get("pattern").and_then(Value::as_str);
    let format = schema.get("format").and_then(Value::as_str);
    let format_pattern = format.and_then(|f| FORMAT_PATTERNS.iter().find(|(n, _)| *n == f));
    let shaped = pattern.or(format_pattern.map(|(_, p)| *p));
    let strategy = match (shaped, mode) {
        (Some(regex), _) => {
            let regex = regex.trim_start_matches('^').trim_end_matches('$');
            let fits = move |s: &String| {
                let n = s.chars().count();
                n >= min && max.is_none_or(|m| n <= m)
            };
            proptest::string::string_regex(regex)
                .ok()?
                .prop_filter("length bounds", fits)
                .boxed()
        }
        (None, _) if format.is_some() => return None,
        (None, Mode::Valid) => {
            let high = max.unwrap_or(min + OPEN_LENGTH);
            proptest::string::string_regex(&format!("[a-zA-Z0-9]{{{min},{high}}}"))
                .ok()?
                .boxed()
        }
        (None, Mode::Boundary) => {
            let high = max.unwrap_or(min + OPEN_LENGTH);
            choose_usize(vec![min, high])
                .prop_flat_map(|n| {
                    proptest::string::string_regex(&format!("[a-zA-Z0-9]{{{n}}}"))
                        .expect("a fixed-length class is a valid regex")
                })
                .boxed()
        }
    };
    Some(strategy.prop_map(Value::String).boxed())
}

/// The inclusive integer range of a schema, with open sides filled in.
fn integer_range(schema: &Map<String, Value>, mode: Mode) -> (Option<i64>, Option<i64>, i64, i64) {
    let get = |key: &str| schema.get(key).and_then(Value::as_f64);
    let low = max_opt(
        get("minimum").map(f64::ceil),
        get("exclusiveMinimum").map(|m| m.floor() + 1.0),
    );
    let high = min_opt(
        get("maximum").map(f64::floor),
        get("exclusiveMaximum").map(|m| m.ceil() - 1.0),
    );
    let low = low.map(clamp_i64);
    let high = high.map(clamp_i64);
    let (open_low, open_high) = match (mode, schema.get("format").and_then(Value::as_str)) {
        (Mode::Boundary, Some("int32")) => (i64::from(i32::MIN), i64::from(i32::MAX)),
        (Mode::Boundary, Some("int64")) => (i64::MIN, i64::MAX),
        _ => (-OPEN_RANGE, OPEN_RANGE),
    };
    let lo = low.unwrap_or_else(|| high.map_or(open_low, |h| h.saturating_sub(2 * OPEN_RANGE)));
    let hi = high.unwrap_or_else(|| low.map_or(open_high, |l| l.saturating_add(2 * OPEN_RANGE)));
    (low, high, lo, hi)
}

fn integer(schema: &Map<String, Value>, mode: Mode) -> Option<BoxedStrategy<Value>> {
    let (_, _, lo, hi) = integer_range(schema, mode);
    let step = schema.get("multipleOf").and_then(Value::as_f64);
    let step = match step {
        Some(s) if s.fract() != 0.0 || s < 1.0 => return None,
        Some(s) => clamp_i64(s),
        None => 1,
    };
    let (k_lo, k_hi) = (
        lo.div_euclid(step) + i64::from(lo.rem_euclid(step) != 0),
        hi.div_euclid(step),
    );
    if k_lo > k_hi {
        return None;
    }
    let ks = match mode {
        Mode::Valid => (k_lo..=k_hi).boxed(),
        Mode::Boundary => {
            let mut ends = vec![k_lo, k_hi];
            ends.dedup();
            proptest::sample::select(ends).boxed()
        }
    };
    Some(
        ks.prop_map(move |k| Value::Number(Number::from(k * step)))
            .boxed(),
    )
}

fn number(schema: &Map<String, Value>, mode: Mode) -> Option<BoxedStrategy<Value>> {
    if schema.contains_key("multipleOf") {
        return None;
    }
    let get = |key: &str| schema.get(key).and_then(Value::as_f64);
    let low = max_opt(get("minimum"), get("exclusiveMinimum").map(f64::next_up));
    let high = min_opt(get("maximum"), get("exclusiveMaximum").map(f64::next_down));
    let open = OPEN_RANGE as f64;
    let lo = low.unwrap_or_else(|| high.map_or(-open, |h| h - 2.0 * open));
    let hi = high.unwrap_or_else(|| low.map_or(open, |l| l + 2.0 * open));
    if lo > hi || !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let values = match mode {
        Mode::Valid if lo < hi => (lo..=hi).boxed(),
        Mode::Valid => Just(lo).boxed(),
        Mode::Boundary => proptest::sample::select(vec![lo, hi]).boxed(),
    };
    Some(
        values
            .prop_map(|v| Number::from_f64(v).map_or(Value::Null, Value::Number))
            .boxed(),
    )
}

#[allow(clippy::cast_possible_truncation)]
fn clamp_i64(v: f64) -> i64 {
    // `as` saturates at the i64 bounds and maps NaN to 0.
    v as i64
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn min_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn draw(schema: Value, mode: Mode, seed: u64) -> Value {
        let components = json!({});
        Fuzzer::new(&components).sample(&schema, mode, &mut runner(seed, "t"))
    }

    fn nesting(value: &Value) -> usize {
        match value {
            Value::Object(map) => 1 + map.values().map(nesting).max().unwrap_or(0),
            Value::Array(items) => 1 + items.iter().map(nesting).max().unwrap_or(0),
            _ => 0,
        }
    }

    #[test]
    fn nesting_stops_at_the_synthesizer_depth() {
        // Twenty required objects per level: a value nested past the cap
        // would hold millions of nodes.
        let components = crate::openapi::schema::tests::fan_out_components();
        let schema = json!({"$ref": "#/components/schemas/S0"});
        let value = Fuzzer::new(&components).sample(&schema, Mode::Valid, &mut runner(1, "t"));
        assert!(nesting(&value) <= MAX_DEPTH + 1, "{}", nesting(&value));
    }

    #[test]
    fn the_same_seed_draws_the_same_values() {
        let schema = json!({"type": "object", "properties": {"a": {"type": "string"}, "b": {"type": "integer"}}});
        assert_eq!(
            draw(schema.clone(), Mode::Valid, 7),
            draw(schema.clone(), Mode::Valid, 7)
        );
        let differ = (0..8)
            .any(|s| draw(schema.clone(), Mode::Valid, s) != draw(schema.clone(), Mode::Valid, 7));
        assert!(differ);
    }

    #[test]
    fn integer_multiples_and_formats() {
        for seed in 0..20 {
            let v = draw(
                json!({"type": "integer", "minimum": 3, "maximum": 40, "multipleOf": 5}),
                Mode::Valid,
                seed,
            );
            let n = v.as_i64().unwrap();
            assert!((5..=40).contains(&n) && n % 5 == 0, "{n}");
            let b = draw(
                json!({"type": "integer", "format": "int32"}),
                Mode::Boundary,
                seed,
            );
            assert!(b == json!(i32::MIN) || b == json!(i32::MAX), "{b}");
        }
    }

    #[test]
    fn exclusive_number_bounds_are_stepped_past() {
        for seed in 0..20 {
            let v = draw(
                json!({"type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1}),
                Mode::Boundary,
                seed,
            );
            let n = v.as_f64().unwrap();
            assert!(n > 0.0 && n < 1.0, "{n}");
        }
    }

    #[test]
    fn unsatisfiable_schemas_fall_back_to_the_synthesized_value() {
        assert_eq!(
            draw(
                json!({"type": "integer", "minimum": 5, "maximum": 1}),
                Mode::Valid,
                1
            ),
            json!(5)
        );
        assert_eq!(
            draw(
                json!({"type": "string", "format": "binary"}),
                Mode::Valid,
                1
            ),
            json!("string")
        );
    }
}
