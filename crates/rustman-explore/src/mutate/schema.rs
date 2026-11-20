//! Schema-aware variants for stress fuzzing (`rustman stress --spec`): a
//! valid body drawn from the operation's JSON Schema with the seeded
//! `api-exploration` generators ([`crate::explore::fuzz`]), then one
//! variant per broken constraint (a required field missing, a wrong type,
//! a bound, a length, a pattern or an enum), each kept only when the
//! schema reports exactly one violation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use crate::ApiModel;
use crate::explore::fuzz::{Fuzzer, Mode, runner};
use crate::explore::plan::{Inputs, invalid_values, required};
use crate::openapi::model::Operation;
use crate::openapi::schema::validator;
use crate::synth::Synthesizer;

/// At most this many variants per operation.
pub const MAX_VARIANTS: usize = 64;

/// A body that breaks one constraint of its schema.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaVariant {
    /// What it breaks, such as `longer than maxLength for field 'name'`.
    pub label: String,
    /// Whether it is over a documented `maxLength` or `maxItems`.
    pub oversized: bool,
    pub body: Value,
}

/// The variants of every operation of a document, generated once per
/// operation for a seed.
pub struct SchemaMutator {
    model: ApiModel,
    seed: u64,
    cache: Mutex<HashMap<String, Arc<Vec<SchemaVariant>>>>,
}

impl std::fmt::Debug for SchemaMutator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaMutator")
            .field("title", &self.model.title)
            .field("seed", &self.seed)
            .finish_non_exhaustive()
    }
}

impl SchemaMutator {
    pub fn new(model: ApiModel, seed: u64) -> Self {
        Self {
            model,
            seed,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The variants of the operation `method path` matches (`path` is a
    /// request path such as `/orders/7`); `None` when no operation with a
    /// JSON body matches.
    pub fn variants(&self, method: &str, path: &str) -> Option<Arc<Vec<SchemaVariant>>> {
        let op = self.operation(method, path)?;
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = cache
            .entry(op.pointer.clone())
            .or_insert_with(|| Arc::new(body_variants(&self.model, op, self.seed)));
        Some(entry.clone()).filter(|v| !v.is_empty())
    }

    /// The operation of `method` whose path template matches the end of
    /// `path` (a server base path may come first).
    pub fn operation(&self, method: &str, path: &str) -> Option<&Operation> {
        self.model
            .operations
            .iter()
            .find(|op| op.method.eq_ignore_ascii_case(method) && matches_template(&op.path, path))
    }
}

/// Whether `path` ends with the segments of `template`, `{name}` matching
/// any one segment.
pub fn matches_template(template: &str, path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let want: Vec<&str> = template.trim_matches('/').split('/').collect();
    let have: Vec<&str> = path.trim_matches('/').split('/').collect();
    if have.len() < want.len() {
        return false;
    }
    let tail = &have[have.len() - want.len()..];
    want.iter()
        .zip(tail)
        .all(|(w, h)| (w.starts_with('{') && w.ends_with('}') && !h.is_empty()) || w == h)
}

/// The variants of `op`'s JSON body, from a valid body drawn with `seed`.
pub fn body_variants(model: &ApiModel, op: &Operation, seed: u64) -> Vec<SchemaVariant> {
    let Some(schema) = Inputs::body_schema(op) else {
        return Vec::new();
    };
    let Ok(check) = validator(schema, &model.components) else {
        return Vec::new();
    };
    let base = Fuzzer::new(&model.components).sample(
        schema,
        Mode::Valid,
        &mut runner(seed, &format!("stress {}", op.pointer)),
    );
    let synth = Synthesizer::new(&model.components);
    let base = if check.is_valid(&base) {
        base
    } else {
        synth.value(schema)
    };
    let Value::Object(object) = &base else {
        return Vec::new();
    };
    let view = synth.object_view(schema).unwrap_or_default();
    let mut out: Vec<SchemaVariant> = required(&view)
        .into_iter()
        .filter_map(|name| {
            let mut body = object.clone();
            body.shift_remove(&name)?;
            Some(variant(
                format!("missing required field '{name}'"),
                false,
                body,
            ))
        })
        .collect();
    let properties = view.get("properties").and_then(Value::as_object);
    for (name, property) in properties.into_iter().flatten() {
        for (what, value) in invalid_values(&synth, property, false) {
            let mut body = object.clone();
            body.insert(name.clone(), value);
            let oversized = matches!(what, "longer than maxLength" | "more than maxItems");
            out.push(variant(
                format!("{what} for field '{name}'"),
                oversized,
                body,
            ));
        }
    }
    out.retain(|v| check.iter_errors(&v.body).count() == 1);
    out.truncate(MAX_VARIANTS);
    out
}

fn variant(label: String, oversized: bool, body: serde_json::Map<String, Value>) -> SchemaVariant {
    SchemaVariant {
        label,
        oversized,
        body: Value::Object(body),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::{ImportOptions, import};

    const SPEC: &str = r#"
openapi: 3.0.3
info: {title: Orders, version: "1"}
paths:
  /orders:
    post:
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [sku, qty]
              properties:
                sku: {type: string, minLength: 2, maxLength: 8}
                code: {type: string, pattern: "^[A-Z][0-9]+$"}
                qty: {type: integer, minimum: 1, maximum: 100}
                tags: {type: array, maxItems: 2, items: {type: string}}
                color: {type: string, enum: [red, blue]}
      responses:
        "201": {description: created}
  /orders/{id}:
    get:
      responses:
        "200": {description: ok}
"#;

    fn model() -> ApiModel {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openapi.yaml");
        std::fs::write(&path, SPEC).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(import(path.to_str().unwrap(), &ImportOptions::default()))
            .unwrap()
            .model
    }

    #[test]
    fn every_variant_breaks_exactly_one_constraint() {
        let model = model();
        let op = &model.operations[0];
        let variants = body_variants(&model, op, 42);
        let schema = Inputs::body_schema(op).unwrap();
        let check = validator(schema, &model.components).unwrap();
        assert!(variants.len() >= 8, "{variants:#?}");
        for v in &variants {
            assert_eq!(check.iter_errors(&v.body).count(), 1, "{v:?}");
        }
        let labels: Vec<&str> = variants.iter().map(|v| v.label.as_str()).collect();
        for expected in [
            "missing required field 'sku'",
            "missing required field 'qty'",
            "wrong type for field 'qty'",
            "above maximum for field 'qty'",
            "longer than maxLength for field 'sku'",
            "more than maxItems for field 'tags'",
            "undocumented enum value for field 'color'",
            "pattern mismatch for field 'code'",
        ] {
            assert!(labels.contains(&expected), "{expected}: {labels:?}");
        }
        let oversized: Vec<&str> = variants
            .iter()
            .filter(|v| v.oversized)
            .map(|v| v.label.as_str())
            .collect();
        assert_eq!(
            oversized,
            [
                "longer than maxLength for field 'sku'",
                "more than maxItems for field 'tags'"
            ]
        );
    }

    #[test]
    fn the_same_seed_gives_the_same_variants() {
        let model = model();
        let op = &model.operations[0];
        assert_eq!(body_variants(&model, op, 7), body_variants(&model, op, 7));
    }

    #[test]
    fn operations_are_found_by_method_and_path_template() {
        let mutator = SchemaMutator::new(model(), 1);
        assert!(mutator.variants("POST", "/orders").is_some());
        assert!(mutator.variants("post", "/api/v1/orders?x=1").is_some());
        assert!(mutator.operation("GET", "/orders/7").is_some());
        // No JSON body: no variants.
        assert!(mutator.variants("GET", "/orders/7").is_none());
        assert!(mutator.variants("DELETE", "/orders").is_none());
        assert!(matches_template("/orders/{id}", "/orders/7"));
        assert!(!matches_template("/orders/{id}", "/orders/"));
        assert!(!matches_template("/orders", "/items"));
    }
}
