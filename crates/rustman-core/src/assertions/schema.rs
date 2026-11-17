//! `schema: {file}` or `schema: {inline}`: JSON Schema validation with the
//! `jsonschema` crate. Draft 2020-12 is the default; `$schema` selects
//! draft-07 (or another supported draft). Remote `$ref`s are not fetched.

use std::fs;

use serde_json::Value;

use super::response::Parsed;
use super::{AssertionResult, Check};
use crate::model::SchemaAssertion;
use crate::workspace::RequestRef;

/// At most this many violations are reported.
pub const MAX_VIOLATIONS: usize = 10;

pub(super) fn evaluate(
    schema: &SchemaAssertion,
    response: &Parsed<'_, '_>,
    request: Option<&RequestRef<'_>>,
) -> AssertionResult {
    let label = match &schema.file {
        Some(file) => format!("schema {}", file.as_str()),
        None => "schema (inline)".to_owned(),
    };
    let check = Check::new("schema", label);
    let schema = match load(schema, request) {
        Ok(schema) => schema,
        Err(e) => return check.error(format!("schema: {e}")),
    };
    let validator = match jsonschema::validator_for(&schema) {
        Ok(validator) => validator,
        Err(e) => return check.error(format!("schema: invalid schema: {e}")),
    };
    let document = match response.json() {
        Ok(document) => document,
        Err(e) => return check.error(e),
    };
    let violations: Vec<String> = validator
        .iter_errors(document)
        .map(|e| {
            let path = e.instance_path().as_str();
            let path = if path.is_empty() { "/" } else { path };
            format!("{path}: {e}")
        })
        .collect();
    if violations.is_empty() {
        return check.pass();
    }
    check.fail(
        "schema",
        "a valid document".to_owned(),
        summary(&violations),
    )
}

/// Up to [`MAX_VIOLATIONS`] violations, then how many more there are.
fn summary(violations: &[String]) -> String {
    let shown = violations.len().min(MAX_VIOLATIONS);
    let mut text = format!(
        "{} violation{}: {}",
        violations.len(),
        if violations.len() == 1 { "" } else { "s" },
        violations[..shown].join("; ")
    );
    if violations.len() > shown {
        text.push_str(&format!("; and {} more", violations.len() - shown));
    }
    text
}

fn load(schema: &SchemaAssertion, request: Option<&RequestRef<'_>>) -> Result<Value, String> {
    let Some(file) = &schema.file else {
        return Ok(schema.inline.clone().unwrap_or(Value::Bool(true)));
    };
    let request = request.ok_or("a schema file needs a workspace")?;
    let path = request.contain_file(file).map_err(|e| e.to_string())?;
    let text =
        fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", file.as_str()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON: {e}", file.as_str()))
}
