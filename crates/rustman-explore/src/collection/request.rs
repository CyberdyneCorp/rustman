//! The request file of one operation.

use rustman_core::model::{
    Auth, Body, HttpRequest, JsonContent, Method, NameValue, REQUEST_VERSION,
};
use serde_json::Value;

use super::names;
use crate::openapi::Diagnostic;
use crate::openapi::model::{Operation, Parameter, ParameterLocation};
use crate::synth::{body_example, is_json_media_type};

/// The tag prefix recording an operation's `operationId` in its request file.
pub const OPERATION_TAG: &str = "openapi:operationId=";

/// The tag prefix recording an operation's path template.
pub const PATH_TAG: &str = "openapi:path=";

/// The variable every generated URL starts with.
pub const BASE_URL: &str = "baseUrl";

/// Header parameters OpenAPI says to ignore: the request's own `Accept`,
/// `Content-Type` and `Authorization` are set elsewhere.
const IGNORED_HEADERS: [&str; 3] = ["accept", "content-type", "authorization"];

/// What a request needs beyond its operation.
#[derive(Clone, Copy, Debug)]
pub struct RequestContext<'a> {
    /// The document's `components`, for example synthesis.
    pub components: &'a Value,
    /// The request-level auth (`None` inherits the collection's).
    pub auth: Option<&'a Auth>,
}

/// The request for `op`: `{{baseUrl}}` plus its path with `{{param}}`
/// variables, query and header parameters as `{{param}}` rows (optional
/// ones disabled), an example body, and tags recording its `operationId`
/// and path template.
pub fn request_for(
    op: &Operation,
    context: &RequestContext<'_>,
    warnings: &mut Vec<Diagnostic>,
) -> HttpRequest {
    let mut tags = Vec::new();
    if let Some(id) = &op.operation_id {
        tags.push(format!("{OPERATION_TAG}{id}"));
    }
    tags.push(format!("{PATH_TAG}{}", op.path));
    let (body, content_type) = body(op, context.components, warnings);
    let mut headers = rows(
        op.parameters_in(ParameterLocation::Header)
            .filter(|p| !IGNORED_HEADERS.contains(&p.name.to_ascii_lowercase().as_str())),
        op,
        warnings,
    );
    if let Some(content_type) = content_type {
        headers
            .get_or_insert_with(Vec::new)
            .insert(0, NameValue::new("Content-Type", content_type));
    }
    HttpRequest {
        version: REQUEST_VERSION,
        kind: "http".into(),
        name: op
            .operation_id
            .clone()
            .unwrap_or_else(|| format!("{} {}", op.method, op.path)),
        description: description(op),
        tags: Some(tags),
        docs: None,
        method: Method::parse(&op.method).ok(),
        url: url(op, warnings),
        query: rows(op.parameters_in(ParameterLocation::Query), op, warnings),
        headers,
        body,
        auth: context.auth.cloned(),
        settings: None,
        variables: None,
        assertions: None,
        captures: None,
    }
}

/// The example body of `op`, and the `Content-Type` to send when the body
/// type's default (`application/json`, form or `text/plain`) is not it.
fn body(
    op: &Operation,
    components: &Value,
    warnings: &mut Vec<Diagnostic>,
) -> (Option<Body>, Option<String>) {
    let Some(example) = op
        .request_body
        .as_ref()
        .and_then(|b| body_example(&b.content, components))
    else {
        return (None, None);
    };
    let media_type = example.media_type.as_str();
    let essence = media_type.split(';').next().unwrap_or_default().trim();
    let Some((body, default)) = typed_body(media_type, essence, example.value) else {
        let message =
            format!("no example body is generated for '{media_type}'; add the body by hand");
        warnings.push(Diagnostic::warning(&op.pointer, message));
        return (None, None);
    };
    let content_type = (!essence.eq_ignore_ascii_case(default)).then(|| media_type.to_owned());
    (Some(body), content_type)
}

/// The body for an example of `media_type`, with the content type that
/// body type is sent with by default.
fn typed_body(media_type: &str, essence: &str, value: Value) -> Option<(Body, &'static str)> {
    let essence = essence.to_ascii_lowercase();
    if is_json_media_type(media_type) {
        return Some((Body::Json(JsonContent::Value(value)), "application/json"));
    }
    if essence == "application/x-www-form-urlencoded" {
        return form(&value).map(|b| (b, "application/x-www-form-urlencoded"));
    }
    let textual = essence.starts_with("text/") || essence.ends_with("xml");
    match value {
        Value::String(text) if textual => Some((Body::Text(text), "text/plain")),
        _ => None,
    }
}

/// Form fields from an object example; strings as they are, other values
/// as JSON text.
fn form(value: &Value) -> Option<Body> {
    let fields = value
        .as_object()?
        .iter()
        .map(|(name, value)| {
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            NameValue::new(name, text)
        })
        .collect();
    Some(Body::Form(fields))
}

fn description(op: &Operation) -> Option<String> {
    let parts: Vec<&str> = [op.summary.as_deref(), op.description.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// `{{baseUrl}}` and the path, each `{name}` replaced by its variable.
fn url(op: &Operation, warnings: &mut Vec<Diagnostic>) -> String {
    let mut out = format!("{{{{{BASE_URL}}}}}");
    let mut rest = op.path.as_str();
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|c| open + c) else {
            break;
        };
        out.push_str(&rest[..open]);
        let name = &rest[open + 1..close];
        out.push_str(&format!("{{{{{}}}}}", variable(name, op, warnings)));
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

fn rows<'a>(
    params: impl Iterator<Item = &'a Parameter>,
    op: &Operation,
    warnings: &mut Vec<Diagnostic>,
) -> Option<Vec<NameValue>> {
    let rows: Vec<NameValue> = params
        .map(|p| {
            let value = format!("{{{{{}}}}}", variable(&p.name, op, warnings));
            NameValue::new(&p.name, value).enabled(p.required)
        })
        .collect();
    (!rows.is_empty()).then_some(rows)
}

/// The variable for parameter `name`, warning when it had to be renamed.
fn variable(name: &str, op: &Operation, warnings: &mut Vec<Diagnostic>) -> String {
    let variable = names::variable(name);
    if variable != name {
        let message = format!("parameter '{name}' becomes the variable '{{{{{variable}}}}}'");
        let warning = Diagnostic::warning(&op.pointer, message);
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
    variable
}
