//! Declarative assertions. Each list item has one key naming its type
//! (`- status: 200`, `- json: {...}`). A hand-written `Deserialize` reads that
//! key and then the matching value, which keeps error positions exact (D4)
//! and gives a clear message for an item with more than one key.

use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::common::{Checked, Validate, exactly_one, given};
use crate::format::Located;

#[derive(Clone, Debug, PartialEq)]
pub enum Assertion {
    Status(StatusExpectation),
    Header(Checked<HeaderAssertion>),
    Json(Box<Checked<JsonAssertion>>),
    Schema(Checked<SchemaAssertion>),
    Body(Checked<BodyAssertion>),
    Latency(Checked<LatencyAssertion>),
}

/// The key naming an assertion's type.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AssertionKey {
    Status,
    Header,
    Json,
    Schema,
    Body,
    Latency,
}

impl<'de> Deserialize<'de> for Assertion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(AssertionVisitor)
    }
}

struct AssertionVisitor;

impl<'de> Visitor<'de> for AssertionVisitor {
    type Value = Assertion;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an assertion such as `status: 200` (one key naming its type)")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Assertion, A::Error> {
        let Some(key) = map.next_key::<AssertionKey>()? else {
            return Err(de::Error::custom(
                "empty assertion (expected one key naming its type)",
            ));
        };
        let assertion = match key {
            AssertionKey::Status => Assertion::Status(map.next_value()?),
            AssertionKey::Header => Assertion::Header(map.next_value()?),
            AssertionKey::Json => Assertion::Json(map.next_value()?),
            AssertionKey::Schema => Assertion::Schema(map.next_value()?),
            AssertionKey::Body => Assertion::Body(map.next_value()?),
            AssertionKey::Latency => Assertion::Latency(map.next_value()?),
        };
        if let Some(extra) = map.next_key::<String>()? {
            return Err(de::Error::custom(format!(
                "an assertion has exactly one key; found `{extra}` after `{}` (start a new `- ` item)",
                key.name()
            )));
        }
        Ok(assertion)
    }
}

impl AssertionKey {
    fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Header => "header",
            Self::Json => "json",
            Self::Schema => "schema",
            Self::Body => "body",
            Self::Latency => "latency",
        }
    }
}

/// `status: 200`, `status: [200, 201]` or `status: "2xx"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusExpectation {
    Code(u16),
    AnyOf(Vec<u16>),
    /// The leading digit of a class pattern such as `2xx`.
    Class(u8),
}

impl<'de> Deserialize<'de> for StatusExpectation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StatusVisitor)
    }
}

struct StatusVisitor;

fn status_code<E: de::Error>(value: u64) -> Result<u16, E> {
    u16::try_from(value)
        .ok()
        .filter(|code| (100..=599).contains(code))
        .ok_or_else(|| E::custom(format!("invalid status code {value} (expected 100-599)")))
}

impl<'de> Visitor<'de> for StatusVisitor {
    type Value = StatusExpectation;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a status code, a list of codes or a class such as \"2xx\"")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        status_code(v).map(StatusExpectation::Code)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        let v = u64::try_from(v).map_err(|_| E::custom(format!("invalid status code {v}")))?;
        self.visit_u64(v)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        let bytes = v.as_bytes();
        if let [digit @ b'1'..=b'5', b'x' | b'X', b'x' | b'X'] = bytes {
            return Ok(StatusExpectation::Class(digit - b'0'));
        }
        Err(E::custom(format!(
            "invalid status pattern '{v}' (expected a code or a class such as 2xx)"
        )))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut codes = Vec::new();
        while let Some(code) = seq.next_element::<u64>()? {
            codes.push(status_code(code)?);
        }
        Ok(StatusExpectation::AnyOf(codes))
    }
}

/// `header: {name, equals | contains | matches | exists}`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderAssertion {
    pub name: String,
    pub equals: Option<String>,
    pub contains: Option<String>,
    pub matches: Option<String>,
    pub exists: Option<bool>,
}

impl Validate for HeaderAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "header assertion",
            &[
                ("equals", given(self.equals.is_some())),
                ("contains", given(self.contains.is_some())),
                ("matches", given(self.matches.is_some())),
                ("exists", given(self.exists.is_some())),
            ],
        )
    }
}

/// `json: {path, <op>: value}` with one operator.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonAssertion {
    pub path: String,
    pub equals: Option<serde_json::Value>,
    pub not_equals: Option<serde_json::Value>,
    pub contains: Option<serde_json::Value>,
    pub matches: Option<String>,
    pub gt: Option<serde_json::Value>,
    pub gte: Option<serde_json::Value>,
    pub lt: Option<serde_json::Value>,
    pub lte: Option<serde_json::Value>,
    pub exists: Option<bool>,
    pub length: Option<serde_json::Value>,
    #[serde(rename = "type")]
    pub value_type: Option<String>,
}

impl Validate for JsonAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "json assertion",
            &[
                ("equals", given(self.equals.is_some())),
                ("not_equals", given(self.not_equals.is_some())),
                ("contains", given(self.contains.is_some())),
                ("matches", given(self.matches.is_some())),
                ("gt", given(self.gt.is_some())),
                ("gte", given(self.gte.is_some())),
                ("lt", given(self.lt.is_some())),
                ("lte", given(self.lte.is_some())),
                ("exists", given(self.exists.is_some())),
                ("length", given(self.length.is_some())),
                ("type", given(self.value_type.is_some())),
            ],
        )
    }
}

/// `schema: {file: <path>}` or `schema: {inline: {...}}`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaAssertion {
    pub file: Option<Located<String>>,
    pub inline: Option<serde_json::Value>,
}

impl Validate for SchemaAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "schema assertion",
            &[
                ("file", given(self.file.is_some())),
                ("inline", given(self.inline.is_some())),
            ],
        )
    }
}

/// `body: {contains | not_contains | matches | equals}`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyAssertion {
    pub contains: Option<String>,
    pub not_contains: Option<String>,
    pub matches: Option<String>,
    pub equals: Option<String>,
}

impl Validate for BodyAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "body assertion",
            &[
                ("contains", given(self.contains.is_some())),
                ("not_contains", given(self.not_contains.is_some())),
                ("matches", given(self.matches.is_some())),
                ("equals", given(self.equals.is_some())),
            ],
        )
    }
}

/// `latency: {total_lt: <ms>, ttfb_lt: <ms>}`; at least one bound.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyAssertion {
    pub total_lt: Option<u64>,
    pub ttfb_lt: Option<u64>,
}

impl Validate for LatencyAssertion {
    fn validate(&self) -> Result<(), String> {
        if self.total_lt.is_none() && self.ttfb_lt.is_none() {
            return Err("latency assertion needs total_lt or ttfb_lt".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn list(text: &str) -> Result<Vec<Assertion>, crate::error::LoadError> {
        crate::format::from_yaml(Path::new("a.yaml"), text)
    }

    #[test]
    fn status_forms() {
        let got = list("- status: 200\n- status: [200, 201]\n- status: \"2xx\"\n").unwrap();
        assert_eq!(
            got,
            [
                Assertion::Status(StatusExpectation::Code(200)),
                Assertion::Status(StatusExpectation::AnyOf(vec![200, 201])),
                Assertion::Status(StatusExpectation::Class(2)),
            ]
        );
        let err = list("- status: 999\n").unwrap_err();
        assert_eq!(err.message, "invalid status code 999 (expected 100-599)");
    }

    #[test]
    fn every_assertion_type_loads() {
        let text = "\
- header: {name: x-request-id, exists: true}
- json: {path: $.items, length: 3}
- schema: {file: schemas/user.json}
- schema: {inline: {type: object}}
- body: {matches: 'order-[0-9]{6}'}
- latency: {total_lt: 200}
";
        assert_eq!(list(text).unwrap().len(), 6);
    }

    #[test]
    fn an_item_with_two_keys_is_explained() {
        let err = list("- status: 200\n  header: {name: x, exists: true}\n").unwrap_err();
        assert_eq!(err.line(), Some(1), "the item's line: {err}");
        assert!(
            err.message
                .starts_with("an assertion has exactly one key; found `header` after `status`"),
            "{err}"
        );
        let err = list("- {}\n").unwrap_err();
        assert!(err.message.starts_with("empty assertion"), "{err}");
    }

    #[test]
    fn errors_point_at_the_assertion() {
        let err = list("- status: 200\n- jsn: {path: $.a}\n").unwrap_err();
        assert_eq!((err.line(), err.column()), (Some(2), Some(3)));
        assert!(err.message.starts_with("unknown variant `jsn`"), "{err}");

        let err =
            list("- status: 200\n- json:\n    path: $.a\n    equals: 1\n    lt: 2\n").unwrap_err();
        assert_eq!(err.line(), Some(3));
        assert!(
            err.message
                .starts_with("json assertion needs exactly one of: equals"),
            "{err}"
        );
    }
}
