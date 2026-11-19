//! Example values: picking a documented example, or synthesizing a value
//! from a schema when there is none (the `openapi-import` spec, "Parameters
//! and example bodies").
//!
//! The synthesizer fills required properties (every property when none is
//! required), honors `type`, `format`, `pattern`, `enum`/`const`, length,
//! item-count and numeric bounds, and stops nesting optional members at
//! [`MAX_DEPTH`] and required ones at [`MAX_REQUIRED_DEPTH`], so circular
//! schemas terminate. A value holds at most
//! [`MAX_MEMBERS`] properties and items in all, so wide schemas stay small.

mod example;
mod number;
mod pattern;
mod string;
mod vary;

use std::cell::Cell;

use serde_json::{Map, Value};

pub use example::{BodyExample, body_example, is_json_media_type};

/// Objects nested this deep get only their required properties, and
/// arrays only their `minItems` items.
pub const MAX_DEPTH: usize = 3;

/// Objects and arrays nested this deep are synthesized empty: a schema
/// whose required properties recurse without end has no finite instance.
pub const MAX_REQUIRED_DEPTH: usize = 10;

/// Properties and items one synthesized value holds at most, in all.
pub const MAX_MEMBERS: usize = 512;

/// `$ref`, `allOf` and `oneOf`/`anyOf` hops followed for one value before
/// giving up (alias loops such as `A: {$ref: A}` or `A: {allOf: [A]}`).
const MAX_HOPS: usize = 32;

/// What an unresolvable schema stands for: anything.
static ANYTHING: Value = Value::Bool(true);

/// Synthesizes values for schemas whose `$ref`s point into `components`.
#[derive(Clone, Copy, Debug)]
pub struct Synthesizer<'a> {
    components: &'a Value,
    seed: Option<u64>,
    /// Objects and arrays nested this deep are synthesized empty.
    cutoff: usize,
}

impl<'a> Synthesizer<'a> {
    pub fn new(components: &'a Value) -> Self {
        Self {
            components,
            seed: None,
            cutoff: MAX_REQUIRED_DEPTH,
        }
    }

    /// Synthesize every object and array nested [`MAX_DEPTH`] deep empty,
    /// required members or not: for callers that synthesize many nested
    /// values one by one and must keep each one small.
    #[must_use]
    pub fn shallow(mut self) -> Self {
        self.cutoff = MAX_DEPTH;
        self
    }

    /// Vary free-form strings, numbers and booleans with `seed`, inside
    /// the same constraints: the same seed gives the same value, another
    /// seed (usually) another one. Without a seed values are fixed samples.
    #[must_use]
    pub fn seeded(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// A value for `schema`.
    pub fn value(&self, schema: &Value) -> Value {
        self.value_at(schema, 0)
    }

    /// A value for `schema` nested `depth` deep in another value: nesting
    /// stops at [`MAX_DEPTH`] counted from the outer value, so a caller
    /// that already nested does not multiply the size.
    pub fn value_at(&self, schema: &Value, depth: usize) -> Value {
        let budget = Budget {
            members: Cell::new(MAX_MEMBERS),
            stream: self.seed.map(vary::Stream::new),
        };
        self.at(schema, depth, MAX_HOPS, &budget)
    }

    /// `schema` with its `$ref` chain followed (siblings of `$ref` are not
    /// merged). A reference that points at nothing resolves to `true`.
    pub fn resolve<'s>(&'s self, schema: &'s Value) -> &'s Value {
        let mut current = schema;
        for _ in 0..MAX_HOPS {
            let Some(reference) = current.get("$ref").and_then(Value::as_str) else {
                return current;
            };
            match self.lookup(reference) {
                Some(target) => current = target,
                None => return &ANYTHING,
            }
        }
        &ANYTHING
    }

    /// The object schema `schema` stands for: its `$ref` chain followed and
    /// its `allOf` members merged. `None` when it is not an object schema.
    pub fn object_view(&self, schema: &Value) -> Option<Map<String, Value>> {
        let object = self.resolve(schema).as_object()?;
        if object.contains_key("allOf") {
            Some(self.merge_all_of(object))
        } else {
            Some(object.clone())
        }
    }

    fn lookup(&self, reference: &str) -> Option<&'a Value> {
        let pointer = reference.strip_prefix("#/components")?;
        self.components.pointer(pointer)
    }

    /// A value for `schema` at nesting `depth`, with `hops` composition
    /// steps (`allOf`, `oneOf`, `anyOf`) left.
    fn at(&self, schema: &Value, depth: usize, hops: usize, budget: &Budget) -> Value {
        let schema = self.resolve(schema);
        let Some(object) = schema.as_object() else {
            return Value::Null;
        };
        if let Some(value) = documented(object) {
            return value;
        }
        let composed = object.contains_key("allOf") || first_branch(object).is_some();
        if composed && hops == 0 {
            return Value::Null;
        }
        if object.contains_key("allOf") {
            let merged = Value::Object(self.merge_all_of(object));
            return self.at(&merged, depth, hops - 1, budget);
        }
        if let Some(branch) = first_branch(object) {
            return self.at(branch, depth, hops - 1, budget);
        }
        let value = match type_of(object) {
            "object" => return self.object(object, depth, budget),
            "array" => return self.array(object, depth, budget),
            "string" => Value::String(string::synthesize_patterned(object, || {
                budget.stream.as_ref().map_or(0, vary::Stream::next)
            })),
            "integer" => number::integer(object),
            "number" => number::number(object),
            "boolean" => Value::Bool(true),
            _ => Value::Null,
        };
        match &budget.stream {
            Some(stream) => vary::vary(object, value, stream),
            None => value,
        }
    }

    fn object(&self, schema: &Map<String, Value>, depth: usize, budget: &Budget) -> Value {
        let mut out = Map::new();
        if depth >= self.cutoff {
            return Value::Object(out);
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for name in property_names(schema, properties, depth >= MAX_DEPTH) {
            if !budget.take() {
                break;
            }
            let property = properties.and_then(|p| p.get(&name));
            let value = match property.or_else(|| schema.get("additionalProperties")) {
                Some(s) if s.is_object() => self.at(s, depth + 1, MAX_HOPS, budget),
                _ => Value::String(String::new()),
            };
            out.insert(name, value);
        }
        Value::Object(out)
    }

    fn array(&self, schema: &Map<String, Value>, depth: usize, budget: &Budget) -> Value {
        if depth >= self.cutoff {
            return Value::Array(Vec::new());
        }
        let count = item_count(schema, depth >= MAX_DEPTH);
        let items = schema.get("items").unwrap_or(&Value::Bool(true));
        Value::Array(
            (0..count)
                .take_while(|_| budget.take())
                .map(|_| self.at(items, depth + 1, MAX_HOPS, budget))
                .collect(),
        )
    }

    /// One schema with the properties and `required` of every `allOf`
    /// member, nested members included (and of `schema` itself).
    fn merge_all_of(&self, schema: &Map<String, Value>) -> Map<String, Value> {
        let mut merged = schema.clone();
        let mut pending: Vec<Value> = match merged.remove("allOf") {
            Some(Value::Array(members)) => members.into_iter().rev().collect(),
            _ => Vec::new(),
        };
        let mut hops = MAX_HOPS;
        while let Some(member) = pending.pop() {
            let Some(member) = self.resolve(&member).as_object() else {
                continue;
            };
            for (key, value) in member {
                match (key.as_str(), value) {
                    ("allOf", Value::Array(nested)) if hops > 0 => {
                        hops -= 1;
                        pending.extend(nested.iter().rev().cloned());
                    }
                    _ => merge_key(&mut merged, key, value),
                }
            }
        }
        merged
    }
}

/// What one synthesized value may still use: members, and the seeded
/// stream of picks when the synthesizer is seeded.
struct Budget {
    members: Cell<usize>,
    stream: Option<vary::Stream>,
}

impl Budget {
    /// Take one member; `false` when none is left.
    fn take(&self) -> bool {
        let left = self.members.get();
        self.members.set(left.saturating_sub(1));
        left > 0
    }
}

/// A value the schema itself documents: `const`, the first non-null `enum`
/// member, the first `examples` entry, `example` or `default`.
fn documented(schema: &Map<String, Value>) -> Option<Value> {
    if let Some(value) = schema.get("const") {
        return Some(value.clone());
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let first = values.iter().find(|v| !v.is_null()).or(values.first());
        return first.cloned();
    }
    let examples = schema.get("examples").and_then(Value::as_array);
    examples
        .and_then(|e| e.first())
        .or_else(|| schema.get("example"))
        .or_else(|| schema.get("default"))
        .cloned()
}

/// The first `oneOf`/`anyOf` branch that is not just `null`.
pub(crate) fn first_branch(schema: &Map<String, Value>) -> Option<&Value> {
    let branches = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))?
        .as_array()?;
    let is_null = |b: &&Value| b.get("type").and_then(Value::as_str) == Some("null");
    branches.iter().find(|b| !is_null(b)).or(branches.first())
}

/// The type to synthesize: the first non-`null` declared type, otherwise
/// one inferred from the keywords present (an object when nothing says).
pub(crate) fn type_of(schema: &Map<String, Value>) -> &str {
    match schema.get("type") {
        Some(Value::String(t)) => return t,
        Some(Value::Array(types)) => {
            let names = types.iter().filter_map(Value::as_str);
            let mut names = names.peekable();
            let first = names.peek().copied();
            return names.find(|t| *t != "null").or(first).unwrap_or("null");
        }
        _ => {}
    }
    let has = |k: &str| schema.contains_key(k);
    if has("items") || has("minItems") || has("maxItems") {
        "array"
    } else if has("minLength") || has("maxLength") || has("format") || has("pattern") {
        "string"
    } else if has("minimum") || has("maximum") || has("multipleOf") {
        "number"
    } else {
        "object"
    }
}

/// The properties to fill: the required ones, or every declared property
/// that is not `readOnly` when nothing is required (unless `required_only`).
fn property_names(
    schema: &Map<String, Value>,
    properties: Option<&Map<String, Value>>,
    required_only: bool,
) -> Vec<String> {
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if !required.is_empty() || required_only {
        return required;
    }
    let read_only = |s: &Value| s.get("readOnly").and_then(Value::as_bool) == Some(true);
    properties
        .map(|p| {
            p.iter()
                .filter(|(_, s)| !read_only(s))
                .map(|(n, _)| n.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// How many items an array gets: one (none when `required_only`), raised
/// to `minItems` and lowered to `maxItems`.
fn item_count(schema: &Map<String, Value>, required_only: bool) -> u64 {
    let min = schema.get("minItems").and_then(Value::as_u64).unwrap_or(0);
    let max = schema.get("maxItems").and_then(Value::as_u64);
    let count = if required_only { min } else { min.max(1) };
    max.map_or(count, |m| count.min(m))
}

/// Merge one `allOf` member's keyword into the merged schema: properties
/// and `required` accumulate, other keywords are kept from the first.
fn merge_key(merged: &mut Map<String, Value>, key: &str, value: &Value) {
    match (key, merged.get_mut(key), value) {
        ("properties", Some(Value::Object(into)), Value::Object(from)) => {
            for (name, schema) in from {
                into.entry(name.clone()).or_insert_with(|| schema.clone());
            }
        }
        ("required", Some(Value::Array(into)), Value::Array(from)) => {
            for name in from {
                if !into.contains(name) {
                    into.push(name.clone());
                }
            }
        }
        (_, None, _) => {
            merged.insert(key.to_owned(), value.clone());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn synth(schema: Value) -> Value {
        Synthesizer::new(&json!({})).value(&schema)
    }

    #[test]
    fn required_properties_types_and_bounds() {
        let schema = json!({"type": "object", "required": ["name", "age"], "properties": {
            "name": {"type": "string"},
            "age": {"type": "integer", "minimum": 0},
            "nickname": {"type": "string"},
        }});
        assert_eq!(synth(schema), json!({"name": "string", "age": 0}));
    }

    #[test]
    fn documented_values_win() {
        assert_eq!(
            synth(json!({"type": "string", "enum": [null, "a"]})),
            json!("a")
        );
        assert_eq!(synth(json!({"const": 7})), json!(7));
        assert_eq!(synth(json!({"type": "integer", "default": 3})), json!(3));
        assert_eq!(synth(json!({"type": "integer", "examples": [5]})), json!(5));
    }

    #[test]
    fn without_required_every_writable_property_is_filled() {
        let schema = json!({"properties": {
            "id": {"type": "integer", "readOnly": true},
            "tag": {"type": "string"},
        }});
        assert_eq!(synth(schema), json!({"tag": "string"}));
    }

    #[test]
    fn all_of_members_are_merged() {
        let components = json!({"schemas": {
            "Base": {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer"}}},
        }});
        let schema = json!({"allOf": [
            {"$ref": "#/components/schemas/Base"},
            {"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}},
        ]});
        let value = Synthesizer::new(&components).value(&schema);
        assert_eq!(value, json!({"id": 0, "name": "string"}));
    }

    #[test]
    fn nullable_types_and_branches_pick_the_non_null_one() {
        assert_eq!(synth(json!({"type": ["null", "boolean"]})), json!(true));
        let branches = json!({"anyOf": [{"type": "null"}, {"type": "integer"}]});
        assert_eq!(synth(branches), json!(0));
    }

    #[test]
    fn circular_schemas_stop_at_the_depth_cap() {
        let components = json!({"schemas": {"Node": {"type": "object", "properties": {
            "children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}},
        }}}});
        let value =
            Synthesizer::new(&components).value(&json!({"$ref": "#/components/schemas/Node"}));
        assert_eq!(value, json!({"children": [{"children": []}]}));
        let alias = json!({"schemas": {"A": {"$ref": "#/components/schemas/A"}}});
        let looped = Synthesizer::new(&alias).value(&json!({"$ref": "#/components/schemas/A"}));
        assert_eq!(looped, Value::Null);
    }

    #[test]
    fn required_members_continue_past_the_depth_cap() {
        let components = json!({"schemas": {"Leaf": {"type": "object", "required": ["id"], "properties": {
            "id": {"type": "integer"},
            "note": {"type": "string"},
        }}}});
        let leaf = json!({"$ref": "#/components/schemas/Leaf"});
        let deep = json!({"properties": {"a": {"properties": {"b": {"properties": {
            "leaf": leaf,
            "list": {"type": "array", "minItems": 1, "items": leaf},
        }}}}}});
        let value = Synthesizer::new(&components).value(&deep);
        assert_eq!(value["a"]["b"]["leaf"], json!({"id": 0}));
        assert_eq!(value["a"]["b"]["list"], json!([{"id": 0}]));
    }

    #[test]
    fn endlessly_required_recursion_stops_at_the_hard_cap() {
        let components = json!({"schemas": {"Node": {"type": "object", "required": ["child", "name"],
        "properties": {
            "child": {"$ref": "#/components/schemas/Node"},
            "name": {"type": "string"},
        }}}});
        let value =
            Synthesizer::new(&components).value(&json!({"$ref": "#/components/schemas/Node"}));
        let mut node = &value;
        for _ in 0..MAX_REQUIRED_DEPTH {
            assert_eq!(node["name"], json!("string"));
            node = &node["child"];
        }
        assert_eq!(node, &json!({}));
    }

    fn members(value: &Value) -> usize {
        match value {
            Value::Object(map) => map.len() + map.values().map(members).sum::<usize>(),
            Value::Array(items) => items.len() + items.iter().map(members).sum::<usize>(),
            _ => 0,
        }
    }

    #[test]
    fn wide_schemas_stop_at_the_member_cap() {
        let components = crate::openapi::schema::tests::fan_out_components();
        let started = std::time::Instant::now();
        let value =
            Synthesizer::new(&components).value(&json!({"$ref": "#/components/schemas/S0"}));
        assert_eq!(members(&value), MAX_MEMBERS);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn array_item_counts() {
        let items = json!({"type": "integer"});
        assert_eq!(synth(json!({"type": "array", "items": items})), json!([0]));
        assert_eq!(
            synth(json!({"type": "array", "items": items, "minItems": 2})),
            json!([0, 0])
        );
        assert_eq!(
            synth(json!({"type": "array", "items": items, "maxItems": 0})),
            json!([])
        );
    }
}
