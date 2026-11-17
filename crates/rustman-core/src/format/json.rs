//! JSON values read from YAML, for JSON request bodies.
//!
//! YAML reads an integer too large for 64 bits as a float, so the JSON sent
//! would silently differ from the file. Such literals are refused; float
//! literals (`1e300`, `1.0e20`) are kept. The parser cannot tell the two
//! apart, so the literal's source text is checked.

use std::cell::RefCell;
use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use super::{error_at, position_of};

const TOO_LARGE: &str = "body content has an integer too large to send exactly; quote it, or give the JSON body as a string";

/// Smallest magnitude a YAML integer literal turned float can have: anything
/// below fits in `i64`/`u64` and is read exactly.
const MIN_OVERFLOWED: f64 = 9_223_372_036_854_775_808.0;

thread_local! {
    static SOURCE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Keeps the text being parsed reachable from [`JsonValue`] while it lives.
pub(super) struct SourceGuard(Option<String>);

impl SourceGuard {
    pub(super) fn set(text: &str) -> Self {
        Self(SOURCE.with(|s| s.replace(Some(text.to_owned()))))
    }
}

impl Drop for SourceGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        SOURCE.with(|s| *s.borrow_mut() = previous);
    }
}

/// A JSON value read from YAML.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonValue(pub Value);

impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spanned = serde_saphyr::Spanned::<Node>::deserialize(deserializer)?;
        match spanned.value {
            Node::Value(value) => Ok(Self(value)),
            Node::Float(f) if is_overflowed_integer(f, spanned.referenced) => Err(
                de::Error::custom(error_at(position_of(spanned.referenced), TOO_LARGE)),
            ),
            Node::Float(f) => Ok(Self(Number::from_f64(f).map_or(Value::Null, Value::Number))),
        }
    }
}

fn is_overflowed_integer(f: f64, at: serde_saphyr::Location) -> bool {
    if f.fract() != 0.0 || f.abs() < MIN_OVERFLOWED {
        return false;
    }
    let span = at.span();
    let (Some(start), Some(len)) = (span.byte_offset(), span.byte_len()) else {
        return true;
    };
    let literal = SOURCE.with_borrow(|source| {
        let start = usize::try_from(start).ok()?;
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        source.as_deref()?.get(start..end).map(str::to_owned)
    });
    literal.is_none_or(|text| is_integer_literal(&text))
}

fn is_integer_literal(text: &str) -> bool {
    let digits = text.trim().trim_start_matches(['+', '-']);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

enum Node {
    Value(Value),
    Float(f64),
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
        Ok(Node::Value(Value::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Node, E> {
        Ok(Node::Value(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
        Ok(Node::Value(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Node, E> {
        Ok(Node::Float(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Node, E> {
        Ok(Node::Value(Value::String(v.to_owned())))
    }

    fn visit_string<E>(self, v: String) -> Result<Node, E> {
        Ok(Node::Value(Value::String(v)))
    }

    fn visit_unit<E>(self) -> Result<Node, E> {
        Ok(Node::Value(Value::Null))
    }

    fn visit_none<E>(self) -> Result<Node, E> {
        Ok(Node::Value(Value::Null))
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        JsonValue::deserialize(d).map(|v| Node::Value(v.0))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let mut items = Vec::new();
        while let Some(JsonValue(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Node::Value(Value::Array(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut object = Map::new();
        while let Some((key, JsonValue(value))) = map.next_entry::<String, JsonValue>()? {
            object.insert(key, value);
        }
        Ok(Node::Value(Value::Object(object)))
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(NodeVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn load(text: &str) -> Result<Value, crate::error::LoadError> {
        super::super::from_yaml::<JsonValue>(Path::new("j.yaml"), text).map(|v| v.0)
    }

    #[test]
    fn values_convert_like_serde_json() {
        assert_eq!(
            load("{a: [1, -2, 1.5, true, null, x], b: {c: ''}}").unwrap(),
            serde_json::json!({"a": [1, -2, 1.5, true, null, "x"], "b": {"c": ""}})
        );
    }

    #[test]
    fn large_float_literals_are_kept() {
        assert_eq!(
            load("[1e300, 1.0e20, -2.5E19, 9.3e18]").unwrap(),
            serde_json::json!([1e300, 1.0e20, -2.5e19, 9.3e18])
        );
    }

    #[test]
    fn overflowing_integer_literals_are_refused_at_the_literal() {
        for text in ["[1, 12345678901234567890123]", "[1, -99999999999999999999]"] {
            let err = load(text).unwrap_err();
            assert_eq!((err.line(), err.column()), (Some(1), Some(5)), "{text}");
            assert_eq!(err.message, TOO_LARGE);
        }
    }

    #[test]
    fn integer_literal_detection() {
        assert!(is_integer_literal("+123"));
        assert!(!is_integer_literal("1e20"));
        assert!(!is_integer_literal("-"));
    }
}
