//! Conformance: the status is documented, and the body matches the schema
//! documented for its status and media type.

use serde_json::Value;

use super::{Detected, Observed};
use crate::openapi::model::{MediaType, Operation, Response};
use crate::openapi::schema::{errors, validator};
use crate::synth::is_json_media_type;

/// The response `op` documents for `status`: the exact code, its range
/// (`4XX`), or `default`.
pub fn documented(op: &Operation, status: u16) -> Option<&Response> {
    let code = status.to_string();
    let range = format!("{}XX", status / 100);
    let find = |want: &str| {
        op.responses
            .iter()
            .find(|r| r.status.eq_ignore_ascii_case(want))
    };
    find(&code)
        .or_else(|| find(&range))
        .or_else(|| find("default"))
}

/// `undocumented-status` when `op` documents nothing for the status.
pub fn undocumented_status(op: Option<&Operation>, response: &Observed<'_>) -> Option<Detected> {
    let op = op?;
    if documented(op, response.status).is_some() {
        return None;
    }
    let documented: Vec<&str> = op.responses.iter().map(|r| r.status.as_str()).collect();
    Some(Detected::new(
        "undocumented-status",
        format!(
            "status {} is not documented (documented: {})",
            response.status,
            documented.join(", ")
        ),
    ))
}

/// `server-error` for an undocumented 5xx, and `response-schema-violation`
/// when the body breaks the documented schema.
pub fn conformance(op: &Operation, components: &Value, response: &Observed<'_>) -> Vec<Detected> {
    let Some(documented) = documented(op, response.status) else {
        if response.status >= 500 {
            let message = format!("a valid request got status {}", response.status);
            return vec![Detected::new("server-error", message)];
        }
        return Vec::new();
    };
    schema_violation(documented, components, response)
        .into_iter()
        .collect()
}

/// The JSON schema `op` documents for `status`, with `components` alongside
/// so its references resolve: a self-contained schema for a `schema`
/// assertion.
pub fn schema_for(op: &Operation, status: u16, components: &Value) -> Option<Value> {
    let response = documented(op, status)?;
    let media = response
        .content
        .iter()
        .find(|m| is_json_media_type(&m.media_type))?;
    let schema = media.schema.as_ref()?;
    Some(serde_json::json!({"allOf": [schema], "components": components}))
}

fn schema_violation(
    documented: &Response,
    components: &Value,
    response: &Observed<'_>,
) -> Option<Detected> {
    let content_type = response
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let media = media_for(&documented.content, content_type)?;
    let schema = media.schema.as_ref()?;
    if !is_json_media_type(&media.media_type) || response.body.is_empty() {
        return None;
    }
    let Some(instance) = response.json() else {
        let message = "the body is not valid JSON".to_owned();
        return Some(Detected::new("response-schema-violation", message));
    };
    let validator = validator(schema, components).ok()?;
    let first = errors(&validator, &instance).into_iter().next()?;
    Some(Detected {
        rule: "response-schema-violation",
        message: format!("at '{}': {}", first.instance, first.message),
        pointers: Some((first.instance, first.schema)),
    })
}

/// The documented media type matching `content_type`: the same essence,
/// a matching range (`application/*`, `*/*`), or any JSON one for a JSON
/// response.
fn media_for<'a>(content: &'a [MediaType], content_type: &str) -> Option<&'a MediaType> {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let matches = |m: &&MediaType| {
        let range = m
            .media_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        range == essence
            || range == "*/*"
            || range
                .strip_suffix("/*")
                .is_some_and(|t| essence.starts_with(&format!("{t}/")))
    };
    content.iter().find(matches).or_else(|| {
        is_json_media_type(&essence)
            .then(|| content.iter().find(|m| is_json_media_type(&m.media_type)))
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::model::Response;

    fn op(statuses: &[&str]) -> Operation {
        Operation {
            pointer: String::new(),
            operation_id: None,
            method: "GET".into(),
            path: "/x".into(),
            summary: None,
            description: None,
            tags: Vec::new(),
            parameters: Vec::new(),
            request_body: None,
            responses: statuses
                .iter()
                .map(|s| Response {
                    status: (*s).into(),
                    description: None,
                    content: Vec::new(),
                    headers: Vec::new(),
                    links: Vec::new(),
                })
                .collect(),
            security: None,
            deprecated: false,
        }
    }

    #[test]
    fn statuses_match_codes_ranges_and_default() {
        let op = op(&["200", "4XX"]);
        assert_eq!(documented(&op, 200).unwrap().status, "200");
        assert_eq!(documented(&op, 404).unwrap().status, "4XX");
        assert!(documented(&op, 500).is_none());
        assert!(documented(&self::op(&["default"]), 500).is_some());
    }

    #[test]
    fn media_ranges_match() {
        let media = |t: &str| MediaType {
            media_type: t.into(),
            schema: None,
            example: None,
            examples: Vec::new(),
        };
        let content = [media("text/*"), media("application/problem+json")];
        assert_eq!(
            media_for(&content, "text/plain; charset=utf-8")
                .unwrap()
                .media_type,
            "text/*"
        );
        assert_eq!(
            media_for(&content, "application/json").unwrap().media_type,
            "application/problem+json"
        );
        assert!(media_for(&content, "image/png").is_none());
    }
}
