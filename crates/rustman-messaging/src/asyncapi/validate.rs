//! Payload validation against an AsyncAPI message's payload schema
//! (messaging-requests "Payload schema validation"): produced values are
//! checked before anything is sent; consumed values are checked after,
//! and their violations fail the implicit `asyncapi_schema` assertion.

use jsonschema::error::ValidationErrorKind;
use rustman_core::assertions::{AssertionResult, MAX_VIOLATIONS, Outcome};
use serde_json::Value;

use super::{AsyncApiError, Document};
use crate::{ConsumedMessage, MessagingError, MessagingExchange};

/// The kind of the implicit assertion.
pub const ASSERTION_KIND: &str = "asyncapi_schema";

/// A compiled payload schema.
pub struct Validator {
    pub message: String,
    schema: jsonschema::Validator,
}

impl Validator {
    /// The validator of message `name`; `None` when it has no payload
    /// schema. The schema is the whole bundled document with a `$ref` to
    /// the payload, so references into `components` resolve.
    pub fn new(document: &Document, name: &str) -> Result<Option<Self>, AsyncApiError> {
        let message = document.message(name)?;
        let Some(at) = message.payload_pointer() else {
            return Ok(None);
        };
        let mut schema = document.bundled.clone();
        if let Some(root) = schema.as_object_mut() {
            root.insert("$ref".to_owned(), Value::String(format!("#{at}")));
        }
        let schema = jsonschema::validator_for(&schema).map_err(|e| {
            AsyncApiError::Invalid(format!("the payload schema of '{name}' is invalid: {e}"))
        })?;
        Ok(Some(Self {
            message: name.to_owned(),
            schema,
        }))
    }

    /// Every violation of `value`, as `<instance path>: <problem>`.
    pub fn violations(&self, value: &Value) -> Vec<String> {
        self.schema
            .iter_errors(value)
            .map(|e| {
                let path = e.instance_path().as_str();
                let path = if path.is_empty() { "/" } else { path };
                let problem = match e.kind() {
                    ValidationErrorKind::Required { property } => {
                        let name = property
                            .as_str()
                            .map_or_else(|| property.to_string(), str::to_owned);
                        format!("missing required property '{name}'")
                    }
                    _ => e.to_string(),
                };
                format!("{path}: {problem}")
            })
            .collect()
    }
}

/// Up to [`MAX_VIOLATIONS`] violations joined, then how many more.
fn listed(violations: &[String]) -> String {
    let shown = violations.len().min(MAX_VIOLATIONS);
    let mut text = violations[..shown].join("; ");
    if violations.len() > shown {
        text.push_str(&format!("; and {} more", violations.len() - shown));
    }
    text
}

/// Refuse the produce when a value breaks the schema. `values` are the
/// encoded message values in order.
pub fn validate_produce(
    validator: &Validator,
    values: &[Option<Vec<u8>>],
) -> Result<(), MessagingError> {
    for (i, value) in values.iter().enumerate() {
        let parsed = payload_value(value.as_deref().unwrap_or_default());
        let violations = validator.violations(&parsed);
        if !violations.is_empty() {
            return Err(MessagingError::Invalid(format!(
                "asyncapi: message {} does not match '{}', nothing was sent: {}",
                i + 1,
                validator.message,
                listed(&violations)
            )));
        }
    }
    Ok(())
}

/// The bytes of a value as the schema sees them: JSON, else a string.
fn payload_value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

/// Record each consumed message's violations. A value cut at the size
/// limit is not checked.
pub fn consumed_violations(validator: &Validator, messages: &mut [ConsumedMessage]) {
    for message in messages.iter_mut().filter(|m| !m.truncated) {
        let value = message.value.clone().unwrap_or(Value::Null);
        message.schema_violations = validator.violations(&value);
    }
}

/// The implicit `asyncapi_schema` assertion of a consume validated
/// against a message; `None` when the exchange was not validated.
pub fn schema_assertion(exchange: &MessagingExchange) -> Option<AssertionResult> {
    let name = exchange.asyncapi.as_ref()?;
    let label = format!("asyncapi_schema {name}");
    let failures: Vec<String> = exchange
        .messages
        .iter()
        .enumerate()
        .flat_map(|(i, m)| {
            m.schema_violations
                .iter()
                .map(move |v| format!("message {}: {v}", i + 1))
        })
        .collect();
    let (outcome, message, actual) = if failures.is_empty() {
        (Outcome::Pass, String::new(), None)
    } else {
        let actual = listed(&failures);
        (
            Outcome::Fail,
            format!("asyncapi_schema: expected payloads matching '{name}', got {actual}"),
            Some(actual),
        )
    };
    Some(AssertionResult {
        kind: ASSERTION_KIND,
        outcome,
        label,
        expected: (outcome == Outcome::Fail).then(|| format!("payloads matching '{name}'")),
        actual,
        message,
        snapshot: None,
    })
}
