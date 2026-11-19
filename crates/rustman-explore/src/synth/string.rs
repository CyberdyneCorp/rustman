//! Synthesized strings: a sample per `format`, fitted to the length bounds,
//! or a string generated from the `pattern` when the sample does not match.

use serde_json::{Map, Value};

use super::pattern;

/// A sample value for each well-known `format`.
const FORMATS: [(&str, &str); 16] = [
    ("date-time", "2024-01-01T00:00:00Z"),
    ("date", "2024-01-01"),
    ("time", "00:00:00Z"),
    ("duration", "P1D"),
    ("email", "user@example.com"),
    ("idn-email", "user@example.com"),
    ("uuid", "00000000-0000-4000-8000-000000000000"),
    ("uri", "https://example.com"),
    ("url", "https://example.com"),
    ("iri", "https://example.com"),
    ("uri-reference", "/"),
    ("hostname", "example.com"),
    ("ipv4", "192.0.2.1"),
    ("ipv6", "2001:db8::1"),
    ("byte", "AAAA"),
    ("password", "password"),
];

/// The longest padding a `minLength` gets, so a hostile document cannot
/// make one synthesized string megabytes long.
pub const MAX_MIN_LENGTH: usize = 4096;

/// A string for `schema`: the sample of its format (or `string`), padded
/// with `x` up to `minLength` (at most [`MAX_MIN_LENGTH`]) and cut down to
/// `maxLength`.
pub fn synthesize(schema: &Map<String, Value>) -> String {
    synthesize_with(schema, "")
}

/// [`synthesize`] with `suffix` after the sample, before fitting.
pub fn synthesize_with(schema: &Map<String, Value>, suffix: &str) -> String {
    let format = schema.get("format").and_then(Value::as_str);
    let sample = format
        .and_then(|f| FORMATS.iter().find(|(name, _)| *name == f))
        .map_or("string", |(_, sample)| sample);
    let sample = format!("{sample}{suffix}");
    fit(&sample, min_length(schema), length(schema, "maxLength"))
}

/// [`synthesize`], then, when `schema` has a `pattern` the sample does not
/// match, a string generated from the pattern with `seed`.
pub fn synthesize_patterned(schema: &Map<String, Value>, seed: impl FnOnce() -> u64) -> String {
    let sample = synthesize(schema);
    match schema.get("pattern").and_then(Value::as_str) {
        Some(regex) => {
            let min = min_length(schema).unwrap_or(0);
            pattern::fit(regex, sample, min, length(schema, "maxLength"), seed())
        }
        None => sample,
    }
}

fn min_length(schema: &Map<String, Value>) -> Option<usize> {
    length(schema, "minLength").map(|n| n.min(MAX_MIN_LENGTH))
}

fn length(schema: &Map<String, Value>, key: &str) -> Option<usize> {
    schema
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

fn fit(sample: &str, min: Option<usize>, max: Option<usize>) -> String {
    let mut out: String = sample.to_owned();
    let count = out.chars().count();
    if let Some(min) = min.filter(|m| *m > count) {
        out.extend(std::iter::repeat_n('x', min - count));
    }
    if let Some(max) = max {
        out = out.chars().take(max).collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn string(schema: Value) -> String {
        synthesize(schema.as_object().unwrap())
    }

    #[test]
    fn formats_and_lengths() {
        assert_eq!(string(json!({"format": "uuid"})).len(), 36);
        assert_eq!(string(json!({"format": "email"})), "user@example.com");
        assert_eq!(string(json!({"format": "unknown"})), "string");
        assert_eq!(string(json!({"minLength": 10})), "stringxxxx");
        let huge = string(json!({"minLength": 9_999_999u64}));
        assert_eq!(huge.len(), MAX_MIN_LENGTH);
        assert_eq!(string(json!({"maxLength": 3})), "str");
        assert_eq!(string(json!({"maxLength": 0})), "");
    }

    #[test]
    fn patterns_shape_the_string() {
        let schema = json!({"pattern": "^[A-Z]{3}-[0-9]{4}$", "maxLength": 8});
        let code = synthesize_patterned(schema.as_object().unwrap(), || 0);
        assert!(
            regex::Regex::new("^[A-Z]{3}-[0-9]{4}$")
                .unwrap()
                .is_match(&code),
            "{code}"
        );
        let plain = json!({"pattern": "^s"});
        assert_eq!(
            synthesize_patterned(plain.as_object().unwrap(), || 0),
            "string"
        );
    }
}
