//! Small types shared by request files and `_collection.yaml`.

use std::fmt;
use std::ops::Deref;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::error::Position;
use crate::format::{Located, MaybeFloat, error_at, exact_located};

/// One `{name, value}` pair. Used for headers, query parameters and form fields,
/// where order matters and names may repeat.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NameValue {
    pub name: String,
    pub value: String,
}

impl NameValue {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// A variable value: a string, a number or a boolean.
#[derive(Clone, Debug, PartialEq)]
pub enum VarValue {
    String(String),
    Integer(i64),
    Float(f64),
    Bool(bool),
}

impl fmt::Display for VarValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(s) => f.write_str(s),
            Self::Integer(n) => write!(f, "{n}"),
            Self::Float(n) => write!(f, "{n}"),
            Self::Bool(b) => write!(f, "{b}"),
        }
    }
}

impl<'de> Deserialize<'de> for VarValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(VarValueVisitor)
    }
}

const EXPECTED_VAR: &str = "a string, number or boolean";

struct VarValueVisitor;

impl Visitor<'_> for VarValueVisitor {
    type Value = VarValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(EXPECTED_VAR)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<VarValue, E> {
        Ok(VarValue::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<VarValue, E> {
        Ok(VarValue::Integer(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<VarValue, E> {
        i64::try_from(v)
            .map(VarValue::Integer)
            .map_err(|_| E::custom(format!("integer {v} is too large")))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<VarValue, E> {
        Ok(VarValue::Float(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<VarValue, E> {
        Ok(VarValue::String(v.to_owned()))
    }
}

/// Variables in file order. Names are unique (duplicate keys are rejected by the parser).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Variables(Vec<(String, VarValue)>);

impl Variables {
    pub fn new(entries: Vec<(String, VarValue)>) -> Self {
        Self(entries)
    }

    pub fn get(&self, name: &str) -> Option<&VarValue> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &VarValue)> {
        self.0.iter().map(|(n, v)| (n.as_str(), v))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for Variables {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(VariablesVisitor)
    }
}

struct VariablesVisitor;

impl<'de> Visitor<'de> for VariablesVisitor {
    type Value = Variables;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a mapping of variable names to values")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Variables, A::Error> {
        let mut entries = Vec::new();
        while let Some((name, value)) = map.next_entry::<String, ExactVar>()? {
            let at = value.0.position();
            let value = value
                .0
                .into_inner()
                .0
                .map_err(|msg| de::Error::custom(error_at(at, msg)))?;
            entries.push((name, value));
        }
        Ok(Variables(entries))
    }
}

const VAR_TOO_LARGE: &str =
    "variable has an integer too large to use exactly; quote it to keep it as a string";

/// A variable value with its position, refusing an integer literal too large
/// for 64 bits that YAML read as a float.
struct ExactVar(Located<CheckedVar>);

impl<'de> Deserialize<'de> for ExactVar {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        exact_located(deserializer, VAR_TOO_LARGE).map(ExactVar)
    }
}

impl MaybeFloat for CheckedVar {
    fn as_float(&self) -> Option<f64> {
        match self.0 {
            Ok(VarValue::Float(f)) => Some(f),
            _ => None,
        }
    }
}

/// A variable value whose type error is kept, so it can be reported at the
/// value's own position rather than where the parser stopped.
struct CheckedVar(Result<VarValue, String>);

impl<'de> Deserialize<'de> for CheckedVar {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_any(CheckedVarVisitor)
            .map(CheckedVar)
    }
}

struct CheckedVarVisitor;

impl CheckedVarVisitor {
    fn invalid(unexpected: de::Unexpected<'_>) -> VarCheck {
        Err(format!(
            "invalid type: {unexpected}, expected {EXPECTED_VAR}"
        ))
    }
}

type VarCheck = Result<VarValue, String>;

impl<'de> Visitor<'de> for CheckedVarVisitor {
    type Value = VarCheck;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(EXPECTED_VAR)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<VarCheck, E> {
        Ok(Ok(VarValue::Bool(v)))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<VarCheck, E> {
        Ok(Ok(VarValue::Integer(v)))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<VarCheck, E> {
        Ok(VarValueVisitor
            .visit_u64::<de::value::Error>(v)
            .map_err(|e| e.to_string()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<VarCheck, E> {
        Ok(Ok(VarValue::Float(v)))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<VarCheck, E> {
        Ok(Ok(VarValue::String(v.to_owned())))
    }

    fn visit_unit<E: de::Error>(self) -> Result<VarCheck, E> {
        Ok(Self::invalid(de::Unexpected::Unit))
    }

    fn visit_none<E: de::Error>(self) -> Result<VarCheck, E> {
        Ok(Self::invalid(de::Unexpected::Option))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<VarCheck, A::Error> {
        while seq.next_element::<de::IgnoredAny>()?.is_some() {}
        Ok(Self::invalid(de::Unexpected::Seq))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<VarCheck, A::Error> {
        while map
            .next_entry::<de::IgnoredAny, de::IgnoredAny>()?
            .is_some()
        {}
        Ok(Self::invalid(de::Unexpected::Map))
    }
}

/// A rule checked after a value is deserialized. The error is reported at the
/// start of the value in the source file.
pub trait Validate {
    fn validate(&self) -> Result<(), String>;
}

/// A value that passed [`Validate`] while being loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Checked<T>(T);

impl<T: Validate> Checked<T> {
    /// Validate a value built in code.
    pub fn new(value: T) -> Result<Self, String> {
        value.validate()?;
        Ok(Self(value))
    }
}

impl<T> Checked<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Checked<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<'de, T: Deserialize<'de> + Validate> Deserialize<'de> for Checked<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = T::deserialize(deserializer)?;
        value.validate().map_err(de::Error::custom)?;
        Ok(Self(value))
    }
}

/// Whether a field of a mapping was given, and if so where its value was read
/// (`Some(None)` for a value built in code).
pub(crate) type Seen = Option<Option<Position>>;

/// Where `field` was read, when it was given.
pub(crate) fn seen<T>(field: &Option<Located<T>>) -> Seen {
    field.as_ref().map(Located::position)
}

/// A field given without a known position (reported at the mapping's start).
pub(crate) fn given(is_present: bool) -> Seen {
    is_present.then_some(None)
}

/// Check which fields a `type:`-discriminated mapping may carry.
///
/// `what` names the variant for the message, e.g. `body type 'json'`. A field
/// that is not accepted is reported where it was written; a missing one at the
/// start of the mapping.
pub(crate) fn check_fields(
    what: &str,
    present: &[(&str, Seen)],
    required: &[&str],
    optional: &[&str],
) -> Result<(), String> {
    for (field, at) in present {
        if let Some(at) = at
            && !required.contains(field)
            && !optional.contains(field)
        {
            return Err(error_at(*at, format!("{what} does not accept '{field}'")));
        }
    }
    for field in required {
        let is_present = present.iter().any(|(f, at)| f == field && at.is_some());
        if !is_present {
            return Err(format!("{what} requires '{field}'"));
        }
    }
    Ok(())
}

/// Require exactly one of the named options to be set. A second option is
/// reported where it was written.
pub(crate) fn exactly_one(what: &str, options: &[(&str, Seen)]) -> Result<(), String> {
    let mut given = options.iter().filter_map(|(_, at)| *at);
    let first = given.next();
    let second = given.next();
    if first.is_some() && second.is_none() {
        return Ok(());
    }
    let names: Vec<&str> = options.iter().map(|(name, _)| *name).collect();
    let message = format!("{what} needs exactly one of: {}", names.join(", "));
    Err(error_at(second.flatten(), message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_fields_reports_extra_then_missing() {
        let present = [("content", Some(None)), ("fields", Some(None))];
        assert_eq!(
            check_fields("body type 'json'", &present, &["content"], &[]),
            Err("body type 'json' does not accept 'fields'".to_owned())
        );
        assert_eq!(
            check_fields("body type 'json'", &[("content", None)], &["content"], &[]),
            Err("body type 'json' requires 'content'".to_owned())
        );
    }

    #[test]
    fn exactly_one_lists_the_options() {
        assert!(exactly_one("x", &[("a", Some(None)), ("b", None)]).is_ok());
        assert_eq!(
            exactly_one("x", &[("a", Some(None)), ("b", Some(None))]),
            Err("x needs exactly one of: a, b".to_owned())
        );
    }

    #[test]
    fn variables_keep_file_order_and_types() {
        let vars: Variables = crate::format::from_yaml(
            std::path::Path::new("v.yaml"),
            "z: 1\na: x\nm: true\nf: 1.5\nport: 0808\n",
        )
        .unwrap();
        let names: Vec<_> = vars.iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["z", "a", "m", "f", "port"]);
        assert_eq!(vars.get("z"), Some(&VarValue::Integer(1)));
        assert_eq!(vars.get("m"), Some(&VarValue::Bool(true)));
        assert_eq!(vars.get("f"), Some(&VarValue::Float(1.5)));
        assert_eq!(vars.get("port"), Some(&VarValue::String("0808".into())));
    }

    #[test]
    fn variable_type_errors_point_at_the_value() {
        let err = |text: &str| {
            crate::format::from_yaml::<Variables>(std::path::Path::new("v.yaml"), text).unwrap_err()
        };
        let e = err("ok: 1\na:   [1]\n");
        assert_eq!((e.line(), e.column()), (Some(2), Some(6)));
        assert_eq!(
            e.message,
            "invalid type: sequence, expected a string, number or boolean"
        );
        let e = err("b:\n  c: d\n");
        assert_eq!((e.line(), e.column()), (Some(2), Some(3)));
        assert_eq!(
            e.message,
            "invalid type: map, expected a string, number or boolean"
        );
        let e = err("n: 18446744073709551615\n");
        assert_eq!((e.line(), e.column()), (Some(1), Some(4)));
        assert_eq!(e.message, "integer 18446744073709551615 is too large");
    }

    #[test]
    fn integers_too_large_for_64_bits_are_refused_at_the_value() {
        let load = |text: &str| {
            crate::format::from_yaml::<Variables>(std::path::Path::new("v.yaml"), text)
        };
        for text in [
            "ok: 1\na:   12345678901234567890123\n",
            "ok: 1\na:   -99999999999999999999\n",
        ] {
            let e = load(text).unwrap_err();
            assert_eq!((e.line(), e.column()), (Some(2), Some(6)), "{text}");
            assert_eq!(e.message, VAR_TOO_LARGE);
        }
        let vars = load("a: 1e22\nb: '12345678901234567890123'\n").unwrap();
        assert_eq!(vars.get("a"), Some(&VarValue::Float(1e22)));
        assert_eq!(
            vars.get("b"),
            Some(&VarValue::String("12345678901234567890123".into()))
        );
    }
}
