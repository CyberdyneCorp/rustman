//! Mutations of a saved JSON body for stress fuzzing (stress-testing
//! "Mutation sources", design D4).
//!
//! Without an OpenAPI document, [`candidates`] walks the body and lists
//! every single mutation it can make; [`apply`] makes one. Each variant
//! differs from the original in exactly one place: a type flipped, a value
//! nulled, a key removed, a number at the 64-bit boundary, a long string,
//! unicode edge cases, a value wrapped in an array, or a key duplicated.
//! [`variant`] picks one with a seeded pick, so the same seed and slot give
//! the same variant. With a document, [`schema`] generates variants that
//! break exactly one schema constraint instead.

pub mod schema;

use serde_json::{Map, Number, Value};

/// A mutation of one place of the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MutationKind {
    /// A number becomes a string, a string a number, a boolean a string.
    TypeFlip,
    Null,
    RemoveKey,
    /// A number becomes `9223372036854775808` (`i64::MAX + 1`).
    BoundaryNumber,
    /// A string of [`LONG_STRING`] characters.
    LongString,
    /// A string of unicode edge cases ([`UNICODE_EDGES`]).
    Unicode,
    /// A value wrapped in a one-item array.
    ArrayWrap,
    /// A member repeated with a different value (the body becomes text:
    /// a JSON value cannot hold duplicate keys).
    DuplicateKey,
}

impl MutationKind {
    pub const ALL: [Self; 8] = [
        Self::TypeFlip,
        Self::Null,
        Self::RemoveKey,
        Self::BoundaryNumber,
        Self::LongString,
        Self::Unicode,
        Self::ArrayWrap,
        Self::DuplicateKey,
    ];

    /// The name used in findings, such as `type-flip`.
    pub fn name(self) -> &'static str {
        match self {
            Self::TypeFlip => "type-flip",
            Self::Null => "null",
            Self::RemoveKey => "remove-key",
            Self::BoundaryNumber => "boundary-number",
            Self::LongString => "long-string",
            Self::Unicode => "unicode",
            Self::ArrayWrap => "array-wrap",
            Self::DuplicateKey => "duplicate-key",
        }
    }

    /// Whether this mutation applies to `value`, a member of an object
    /// (`member`) or an array item or the root.
    fn applies(self, value: &Value, member: bool) -> bool {
        match self {
            Self::TypeFlip => matches!(value, Value::Number(_) | Value::String(_) | Value::Bool(_)),
            Self::Null => !value.is_null(),
            Self::RemoveKey | Self::DuplicateKey => member,
            Self::BoundaryNumber => value.is_number(),
            Self::LongString | Self::Unicode => value.is_string(),
            Self::ArrayWrap => true,
        }
    }
}

/// The length of a [`MutationKind::LongString`] value.
pub const LONG_STRING: usize = 10_000;

/// The value of a [`MutationKind::Unicode`] mutation: NUL, a byte order
/// mark, a right-to-left override, an astral character, a combining mark
/// alone and a noncharacter.
pub const UNICODE_EDGES: &str = "\u{0}\u{FEFF}\u{202E}\u{1D518}\u{301}\u{FFFF}";

/// At most this many places of a body are mutated (in walk order), so a
/// large body costs no more than a small one.
pub const MAX_PLACES: usize = 256;

/// One mutation to make: what, and where (a JSON pointer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub kind: MutationKind,
    pub pointer: String,
}

/// A mutated body.
#[derive(Clone, Debug, PartialEq)]
pub enum VariantBody {
    Json(Value),
    /// JSON text a value cannot represent (duplicate keys).
    Text(String),
}

impl VariantBody {
    /// The body as JSON text.
    pub fn to_text(&self) -> String {
        match self {
            Self::Json(value) => value.to_string(),
            Self::Text(text) => text.clone(),
        }
    }
}

/// A body with one mutation.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub kind: MutationKind,
    /// Where the mutation is, as a JSON pointer (`""` for the root).
    pub pointer: String,
    pub body: VariantBody,
}

/// Every single mutation of `body`, place by place in walk order (at most
/// [`MAX_PLACES`] places), each place in [`MutationKind::ALL`] order.
pub fn candidates(body: &Value) -> Vec<Candidate> {
    let mut places = Vec::new();
    walk(body, String::new(), false, &mut places);
    let mut out = Vec::new();
    for (pointer, value, member) in places {
        for kind in MutationKind::ALL {
            if kind.applies(value, member) {
                out.push(Candidate {
                    kind,
                    pointer: pointer.clone(),
                });
            }
        }
    }
    out
}

fn walk<'v>(
    value: &'v Value,
    pointer: String,
    member: bool,
    out: &mut Vec<(String, &'v Value, bool)>,
) {
    if out.len() >= MAX_PLACES {
        return;
    }
    out.push((pointer.clone(), value, member));
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                walk(child, format!("{pointer}/{}", escape(key)), true, out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                walk(child, format!("{pointer}/{i}"), false, out);
            }
        }
        _ => {}
    }
}

/// The variant of `body` for a seeded `pick`: candidate `pick % n`, or
/// `None` when nothing can be mutated.
pub fn variant(body: &Value, pick: u64) -> Option<Variant> {
    let all = candidates(body);
    let n = u64::try_from(all.len()).ok().filter(|n| *n > 0)?;
    let index = usize::try_from(pick % n).ok()?;
    apply(body, &all[index])
}

/// `body` with `candidate` applied; `None` when it does not apply or would
/// change nothing.
pub fn apply(body: &Value, candidate: &Candidate) -> Option<Variant> {
    let out = match candidate.kind {
        MutationKind::RemoveKey => remove(body, &candidate.pointer).map(VariantBody::Json)?,
        MutationKind::DuplicateKey => duplicate(body, &candidate.pointer).map(VariantBody::Text)?,
        kind => {
            let mut copy = body.clone();
            let place = copy.pointer_mut(&candidate.pointer)?;
            let replaced = replacement(kind, place)?;
            if replaced == *place {
                return None;
            }
            *place = replaced;
            VariantBody::Json(copy)
        }
    };
    Some(Variant {
        kind: candidate.kind,
        pointer: candidate.pointer.clone(),
        body: out,
    })
}

/// The new value of a place for the in-place mutations.
fn replacement(kind: MutationKind, value: &Value) -> Option<Value> {
    Some(match kind {
        MutationKind::TypeFlip => flip(value)?,
        MutationKind::Null => Value::Null,
        MutationKind::BoundaryNumber => Value::Number(Number::from(i64::MAX.unsigned_abs() + 1)),
        MutationKind::LongString => Value::String("A".repeat(LONG_STRING)),
        MutationKind::Unicode => Value::String(UNICODE_EDGES.to_owned()),
        MutationKind::ArrayWrap => Value::Array(vec![value.clone()]),
        MutationKind::RemoveKey | MutationKind::DuplicateKey => return None,
    })
}

/// The same value as another type: `2` and `"2"`, `"7"` and `7`, `true`
/// and `"true"`; a string that is no number becomes `0`.
fn flip(value: &Value) -> Option<Value> {
    match value {
        Value::Number(n) => Some(Value::String(n.to_string())),
        Value::String(s) => Some(match serde_json::from_str::<Number>(s.trim()) {
            Ok(n) => Value::Number(n),
            Err(_) => Value::Number(Number::from(0)),
        }),
        Value::Bool(b) => Some(Value::String(b.to_string())),
        _ => None,
    }
}

/// A value of another type than `value`, for a duplicated key.
fn different(value: &Value) -> Value {
    match flip(value) {
        Some(other) if other != *value => other,
        _ if value.is_null() => Value::String("null".into()),
        _ => Value::Null,
    }
}

/// The parent of `pointer` and the last key, unescaped.
fn split(pointer: &str) -> Option<(&str, String)> {
    let (parent, last) = pointer.rsplit_once('/')?;
    Some((parent, unescape(last)))
}

fn remove(body: &Value, pointer: &str) -> Option<Value> {
    let (parent, key) = split(pointer)?;
    let mut copy = body.clone();
    copy.pointer_mut(parent)?
        .as_object_mut()?
        .shift_remove(&key)?;
    Some(copy)
}

/// Placeholder for the object whose key is duplicated, while the rest of
/// the body is serialized.
const SPLICE: &str = "\u{0}rustman-duplicate-key\u{0}";

/// `body` as JSON text with the member at `pointer` repeated right after
/// itself with a [`different`] value: last-wins parsers see the new value.
fn duplicate(body: &Value, pointer: &str) -> Option<String> {
    let (parent, key) = split(pointer)?;
    let object = body.pointer(parent)?.as_object()?;
    let text = object_with_duplicate(object, &key)?;
    if parent.is_empty() {
        return Some(text);
    }
    let mut copy = body.clone();
    *copy.pointer_mut(parent)? = Value::String(SPLICE.to_owned());
    let quoted = Value::String(SPLICE.to_owned()).to_string();
    Some(copy.to_string().replacen(&quoted, &text, 1))
}

fn object_with_duplicate(object: &Map<String, Value>, key: &str) -> Option<String> {
    let original = object.get(key)?;
    let member = |k: &str, v: &Value| format!("{}:{v}", Value::String(k.to_owned()));
    let mut members = Vec::new();
    for (k, v) in object {
        members.push(member(k, v));
        if k == key {
            members.push(member(k, &different(original)));
        }
    }
    Some(format!("{{{}}}", members.join(",")))
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn unescape(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

/// SplitMix64 of `seed` and `slot`: the seeded pick of slot `slot`, the
/// same algorithm as the synthesizer's variation stream.
pub fn pick(seed: u64, slot: u64) -> u64 {
    let state = seed
        .wrapping_add(slot.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// How many places `a` and `b` differ in: values of different types, or
/// unequal scalars, count once; objects compare key by key (a key on one
/// side only counts once); arrays of the same length item by item.
pub fn differences(a: &Value, b: &Value) -> usize {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let only = x.keys().filter(|k| !y.contains_key(*k)).count()
                + y.keys().filter(|k| !x.contains_key(*k)).count();
            let common: usize = x
                .iter()
                .filter_map(|(k, v)| y.get(k).map(|w| differences(v, w)))
                .sum();
            only + common
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            x.iter().zip(y).map(|(v, w)| differences(v, w)).sum()
        }
        _ => usize::from(a != b),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    fn body() -> Value {
        json!({"qty": 2, "sku": "A1"})
    }

    fn all(body: &Value) -> Vec<Variant> {
        candidates(body)
            .iter()
            .filter_map(|c| apply(body, c))
            .collect()
    }

    fn texts(body: &Value) -> Vec<String> {
        all(body).iter().map(|v| v.body.to_text()).collect()
    }

    #[test]
    fn the_spec_examples_are_among_the_variants() {
        let texts = texts(&body());
        for expected in [
            r#"{"qty":"2","sku":"A1"}"#,
            r#"{"sku":"A1"}"#,
            r#"{"qty":9223372036854775808,"sku":"A1"}"#,
        ] {
            assert!(texts.iter().any(|t| t == expected), "{expected}: {texts:?}");
        }
    }

    #[test]
    fn type_flip_swaps_numbers_strings_and_booleans() {
        let flipped = |v: Value| {
            let body = json!({ "a": v });
            let c = Candidate {
                kind: MutationKind::TypeFlip,
                pointer: "/a".into(),
            };
            apply(&body, &c).map(|v| v.body)
        };
        assert_eq!(
            flipped(json!(2)),
            Some(VariantBody::Json(json!({"a": "2"})))
        );
        assert_eq!(
            flipped(json!("7")),
            Some(VariantBody::Json(json!({"a": 7})))
        );
        assert_eq!(
            flipped(json!("A1")),
            Some(VariantBody::Json(json!({"a": 0})))
        );
        assert_eq!(
            flipped(json!(true)),
            Some(VariantBody::Json(json!({"a": "true"})))
        );
        assert_eq!(flipped(json!(null)), None);
    }

    #[test]
    fn null_remove_and_wrap_change_one_member() {
        let one = |kind, pointer: &str| {
            let c = Candidate {
                kind,
                pointer: pointer.into(),
            };
            apply(&body(), &c).unwrap().body
        };
        assert_eq!(
            one(MutationKind::Null, "/sku"),
            VariantBody::Json(json!({"qty": 2, "sku": null}))
        );
        assert_eq!(
            one(MutationKind::RemoveKey, "/qty"),
            VariantBody::Json(json!({"sku": "A1"}))
        );
        assert_eq!(
            one(MutationKind::ArrayWrap, "/qty"),
            VariantBody::Json(json!({"qty": [2], "sku": "A1"}))
        );
    }

    #[test]
    fn strings_get_long_and_unicode_values() {
        let c = |kind| Candidate {
            kind,
            pointer: "/sku".into(),
        };
        let VariantBody::Json(long) = apply(&body(), &c(MutationKind::LongString)).unwrap().body
        else {
            panic!("json")
        };
        assert_eq!(long["sku"].as_str().unwrap().len(), LONG_STRING);
        let VariantBody::Json(edges) = apply(&body(), &c(MutationKind::Unicode)).unwrap().body
        else {
            panic!("json")
        };
        assert_eq!(edges["sku"], UNICODE_EDGES);
    }

    #[test]
    fn duplicate_keys_are_written_as_text_last_one_differing() {
        let c = Candidate {
            kind: MutationKind::DuplicateKey,
            pointer: "/role".into(),
        };
        let body = json!({"role": "user", "n": 1});
        let variant = apply(&body, &c).unwrap();
        assert_eq!(
            variant.body,
            VariantBody::Text(r#"{"role":"user","role":0,"n":1}"#.into())
        );
        let nested = json!({"order": {"role": "user"}, "z": [1]});
        let c = Candidate {
            kind: MutationKind::DuplicateKey,
            pointer: "/order/role".into(),
        };
        assert_eq!(
            apply(&nested, &c).unwrap().body.to_text(),
            r#"{"order":{"role":"user","role":0},"z":[1]}"#
        );
    }

    #[test]
    fn a_null_body_can_only_be_wrapped() {
        let kinds: Vec<MutationKind> = candidates(&json!(null)).iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [MutationKind::ArrayWrap]);
        assert!(variant(&body(), 3).is_some());
    }

    #[test]
    fn the_same_seed_and_slot_give_the_same_variant() {
        let a: Vec<_> = (0..50).map(|s| variant(&body(), pick(42, s))).collect();
        let b: Vec<_> = (0..50).map(|s| variant(&body(), pick(42, s))).collect();
        let c: Vec<_> = (0..50).map(|s| variant(&body(), pick(43, s))).collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn pointers_escape_keys() {
        let body = json!({"a/b": {"~": 1}});
        let pointers: Vec<String> = candidates(&body).into_iter().map(|c| c.pointer).collect();
        assert!(pointers.contains(&"/a~1b/~0".to_owned()), "{pointers:?}");
        let c = Candidate {
            kind: MutationKind::RemoveKey,
            pointer: "/a~1b/~0".into(),
        };
        assert_eq!(
            apply(&body, &c).unwrap().body,
            VariantBody::Json(json!({"a/b": {}}))
        );
    }

    fn json_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i32>().prop_map(|n| json!(n)),
            "[a-z0-9]{0,6}".prop_map(Value::String),
        ];
        leaf.prop_recursive(3, 24, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::btree_map("[a-c]{1,2}", inner, 0..4)
                    .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #[test]
        fn every_variant_differs_in_exactly_one_place(body in json_value()) {
            for variant in all(&body) {
                let parsed: Value = match &variant.body {
                    VariantBody::Json(v) => v.clone(),
                    // Last-wins: the duplicate's value is what a parser keeps.
                    VariantBody::Text(t) => serde_json::from_str(t).unwrap(),
                };
                prop_assert_eq!(differences(&body, &parsed), 1, "{:?}", variant);
            }
        }
    }
}
