//! Negative-input probes: requests that omit required fields or
//! parameters, use wrong types, or violate bounds, length, pattern or enum.
//! Every body or parameter value is checked against its schema, so a
//! variant is planned only when it really is invalid.

use serde_json::{Map, Value, json};

use super::inputs::{Inputs, PathValues, fill_path, segment, text};
use super::single::probe;
use super::{AuthMode, Check, Family, Probe, Step, escape, escape_json};
use crate::openapi::model::{Operation, ParameterLocation};
use crate::openapi::schema::validator;
use crate::synth::type_of;

/// At most this many negative-input probes per operation.
const MAX_VARIANTS: usize = 20;

/// What one variant changes in the valid request.
enum Change {
    Body(Option<Value>),
    Query(Vec<(String, String)>),
    /// The whole path, parameters filled in.
    Path(String),
}

struct Variant {
    label: String,
    change: Change,
}

pub fn probes(inputs: &Inputs<'_>, ops: &[&Operation]) -> Vec<Probe> {
    let mut probes = Vec::new();
    for op in ops {
        let mut variants = body_variants(inputs, op);
        variants.extend(query_variants(inputs, op));
        variants.extend(path_variants(inputs, op));
        variants.truncate(MAX_VARIANTS);
        let path = inputs.loose_path(op);
        for variant in variants {
            let step = apply(
                inputs.request(
                    "probe",
                    op,
                    &path,
                    AuthMode::Identity,
                    Some(Check::RejectsInput),
                ),
                variant.change,
            );
            probes.push(probe(
                Family::NegativeInput,
                op,
                &variant.label,
                "4xx",
                Ok(vec![step]),
            ));
        }
    }
    probes
}

fn apply(mut step: Step, change: Change) -> Step {
    match change {
        Change::Body(body) => step.body = body.as_ref().map(escape_json),
        Change::Query(query) => step.query = query,
        Change::Path(path) => step.path = path,
    }
    step
}

fn body_variants(inputs: &Inputs<'_>, op: &Operation) -> Vec<Variant> {
    let (Some(schema), Some(Value::Object(base))) = (Inputs::body_schema(op), inputs.body(op))
    else {
        return Vec::new();
    };
    let Ok(check) = validator(schema, &inputs.model.components) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if op.request_body.as_ref().is_some_and(|b| b.required) {
        out.push(Variant {
            label: "missing required body".into(),
            change: Change::Body(None),
        });
    }
    let object = inputs.synth.object_view(schema).unwrap_or_default();
    for name in required(&object) {
        let mut body = base.clone();
        if body.remove(&name).is_some() {
            out.push(body_variant(
                format!("missing required field '{name}'"),
                body,
            ));
        }
    }
    let properties = object.get("properties").and_then(Value::as_object);
    for (name, property) in properties.into_iter().flatten() {
        for (what, value) in invalid_values(inputs, property, false) {
            let mut body = base.clone();
            body.insert(name.clone(), value);
            out.push(body_variant(format!("{what} for field '{name}'"), body));
        }
    }
    out.retain(|v| match &v.change {
        Change::Body(Some(body)) => !check.is_valid(body),
        _ => true,
    });
    out
}

fn body_variant(label: String, body: Map<String, Value>) -> Variant {
    Variant {
        label,
        change: Change::Body(Some(Value::Object(body))),
    }
}

fn query_variants(inputs: &Inputs<'_>, op: &Operation) -> Vec<Variant> {
    let base = inputs.params(op, ParameterLocation::Query);
    let mut out = Vec::new();
    for param in op.parameters_in(ParameterLocation::Query) {
        if param.required {
            let query = base
                .iter()
                .filter(|(n, _)| *n != param.name)
                .cloned()
                .collect();
            out.push(Variant {
                label: format!("missing required query parameter '{}'", param.name),
                change: Change::Query(query),
            });
        }
        for (what, value) in param_values(inputs, param.schema.as_ref()) {
            let mut query: Vec<(String, String)> = base
                .iter()
                .filter(|(n, _)| *n != param.name)
                .cloned()
                .collect();
            query.push((param.name.clone(), escape(&text(&value))));
            out.push(Variant {
                label: format!("{what} for query parameter '{}'", param.name),
                change: Change::Query(query),
            });
        }
    }
    out
}

fn path_variants(inputs: &Inputs<'_>, op: &Operation) -> Vec<Variant> {
    let base = inputs.loose_path(op);
    let mut out = Vec::new();
    for param in op.parameters_in(ParameterLocation::Path) {
        for (what, value) in param_values(inputs, param.schema.as_ref()) {
            let values: PathValues = base
                .iter()
                .map(|(n, v)| {
                    if *n == param.name {
                        (n.clone(), segment(&text(&value)))
                    } else {
                        (n.clone(), v.clone())
                    }
                })
                .collect();
            out.push(Variant {
                label: format!("{what} for path parameter '{}'", param.name),
                change: Change::Path(fill_path(&op.path, &values)),
            });
        }
    }
    out
}

/// Invalid values of a parameter, sent as text: a wrong type is no wrong
/// type for a string parameter, since every parameter travels as text.
fn param_values(inputs: &Inputs<'_>, schema: Option<&Value>) -> Vec<(&'static str, Value)> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    let Ok(check) = validator(schema, &inputs.model.components) else {
        return Vec::new();
    };
    invalid_values(inputs, schema, true)
        .into_iter()
        .filter(|(_, v)| !check.is_valid(v))
        .collect()
}

fn required(object: &Map<String, Value>) -> Vec<String> {
    object
        .get("required")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Candidate invalid values for `schema`, each with what it violates.
fn invalid_values(
    inputs: &Inputs<'_>,
    schema: &Value,
    as_text: bool,
) -> Vec<(&'static str, Value)> {
    let Some(object) = inputs.synth.object_view(schema) else {
        return Vec::new();
    };
    let kind = type_of(&object).to_owned();
    let mut out = Vec::new();
    if !(as_text && kind == "string") {
        out.push(("wrong type", wrong_type(&kind)));
    }
    if object.contains_key("enum") {
        out.push(("undocumented enum value", json!("not-a-documented-value")));
    }
    let number = |key: &str| object.get(key).and_then(Value::as_f64);
    let length = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
    };
    if let Some(min) = number("minimum") {
        out.push(("below minimum", json!(min - 1.0)));
    }
    if let Some(min) = number("exclusiveMinimum") {
        out.push(("at exclusive minimum", json!(min)));
    }
    if let Some(max) = number("maximum") {
        out.push(("above maximum", json!(max + 1.0)));
    }
    if let Some(max) = number("exclusiveMaximum") {
        out.push(("at exclusive maximum", json!(max)));
    }
    if let Some(min) = length("minLength").filter(|n| *n > 0) {
        out.push(("shorter than minLength", json!("x".repeat(min - 1))));
    }
    if let Some(max) = length("maxLength") {
        out.push(("longer than maxLength", json!("x".repeat(max + 1))));
    }
    if object.contains_key("pattern") {
        out.push(("pattern mismatch", json!("!~ not matching ~!")));
    }
    if let Some(max) = length("maxItems") {
        let item = object
            .get("items")
            .map_or(Value::Null, |i| inputs.synth.value(i));
        out.push(("more than maxItems", Value::Array(vec![item; max + 1])));
    }
    if length("minItems").is_some_and(|n| n > 0) {
        out.push(("fewer than minItems", json!([])));
    }
    out.into_iter()
        .map(|(w, v)| (w, whole_numbers(&kind, v)))
        .collect()
}

/// Integer bounds stay integers (`-1`, not `-1.0`).
fn whole_numbers(kind: &str, value: Value) -> Value {
    match value.as_f64() {
        Some(f) if kind == "integer" && value.is_f64() && f.fract() == 0.0 && f.abs() < 9.0e15 => {
            #[allow(clippy::cast_possible_truncation)]
            let whole = f as i64;
            json!(whole)
        }
        _ => value,
    }
}

fn wrong_type(kind: &str) -> Value {
    match kind {
        "string" => json!(12345),
        "integer" | "number" => json!("not-a-number"),
        "boolean" => json!("not-a-boolean"),
        "array" => json!("not-an-array"),
        _ => json!("not-an-object"),
    }
}
