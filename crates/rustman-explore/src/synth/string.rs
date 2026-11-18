//! Synthesized strings: a sample per `format`, fitted to the length bounds.

use serde_json::{Map, Value};

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

/// A string for `schema`: the sample of its format (or `string`), padded
/// with `x` up to `minLength` and cut down to `maxLength`.
pub fn synthesize(schema: &Map<String, Value>) -> String {
    let format = schema.get("format").and_then(Value::as_str);
    let sample = format
        .and_then(|f| FORMATS.iter().find(|(name, _)| *name == f))
        .map_or("string", |(_, sample)| sample);
    let length = |key: &str| {
        schema
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
    };
    fit(sample, length("minLength"), length("maxLength"))
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
        assert_eq!(string(json!({"maxLength": 3})), "str");
        assert_eq!(string(json!({"maxLength": 0})), "");
    }
}
