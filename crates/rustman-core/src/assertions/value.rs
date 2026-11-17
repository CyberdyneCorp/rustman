//! Comparing JSON values for the `json` assertion operators.

use serde_json::Value;

/// Whether `actual` equals `expected`. Numbers compare by value (`1` equals
/// `1.0`). With `coerce`, a number or boolean also equals a string holding
/// its text form, which is how a `{{captured}}` value compares.
pub(super) fn equal(actual: &Value, expected: &Value, coerce: bool) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => a == b || a.as_f64() == b.as_f64(),
        (Value::Number(_) | Value::Bool(_), Value::String(s)) if coerce => {
            serde_json::to_string(actual).is_ok_and(|text| text == *s)
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| equal(x, y, false))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, x)| b.get(k).is_some_and(|y| equal(x, y, false)))
        }
        _ => actual == expected,
    }
}

/// `actual` contains `expected`: a substring of a string, an element of an
/// array, or a key of an object.
pub(super) fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::String(a), Value::String(b)) => a.contains(b.as_str()),
        (Value::Array(items), _) => items.iter().any(|item| equal(item, expected, true)),
        (Value::Object(map), Value::String(key)) => map.contains_key(key),
        _ => false,
    }
}

/// A number, or a string holding one (an interpolated bound).
pub(super) fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The text a regular expression is matched against: a string as is, a
/// number or boolean in its JSON form.
pub(super) fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

/// The length of an array, object or string (in characters).
pub(super) fn length(value: &Value) -> Option<usize> {
    match value {
        Value::Array(items) => Some(items.len()),
        Value::Object(map) => Some(map.len()),
        Value::String(s) => Some(s.chars().count()),
        _ => None,
    }
}

/// The type names accepted by `type:`.
pub(super) const TYPES: [&str; 7] = [
    "string", "number", "integer", "boolean", "array", "object", "null",
];

/// Whether `value` is of type `name` (one of [`TYPES`]).
pub(super) fn is_type(value: &Value, name: &str) -> bool {
    match name {
        "integer" => value.is_i64() || value.is_u64(),
        _ => type_name(value) == name,
    }
}

pub(super) fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `value` as compact JSON, cut to 200 characters once the message is
/// redacted (see [`super::shown`]).
pub(super) fn show(value: &Value) -> String {
    super::shown::plain(&value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_compare_by_value() {
        assert!(equal(&json!(1), &json!(1.0), false));
        assert!(!equal(&json!(1), &json!("1"), false));
        assert!(equal(&json!(42), &json!("42"), true));
        assert!(equal(&json!(true), &json!("true"), true));
        assert!(equal(
            &json!([1, {"a": 2}]),
            &json!([1.0, {"a": 2.0}]),
            false
        ));
        assert!(!equal(&json!({"a": 1}), &json!({"a": 1, "b": 2}), false));
    }

    #[test]
    fn contains_strings_arrays_and_keys() {
        assert!(contains(&json!("hello"), &json!("ell")));
        assert!(contains(&json!([1, 2]), &json!(2)));
        assert!(contains(&json!({"a": 1}), &json!("a")));
        assert!(!contains(&json!(5), &json!(5)));
    }

    #[test]
    fn types_and_lengths() {
        assert!(is_type(&json!(3), "integer") && !is_type(&json!(3.5), "integer"));
        assert!(is_type(&json!(3.5), "number"));
        assert_eq!(length(&json!("héllo")), Some(5));
        assert_eq!(length(&json!(3)), None);
        assert_eq!(number(&json!(" 7 ")), Some(7.0));
        let shown =
            super::super::shown::finish(&show(&json!("x".repeat(300))), &Default::default());
        assert_eq!(shown.chars().count(), 201);
    }
}
