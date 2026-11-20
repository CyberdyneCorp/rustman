//! Response templates (spec: Response templates): override bodies and
//! header values may use request data and the `environments` dynamic
//! variables. Templates are substitution only, parsed by the same
//! `{{...}}` parser as request files:
//!
//! - `{{request.path.<name>}}`, `{{request.query.<name>}}`,
//!   `{{request.header.<name>}}` and `{{request.body.<JSONPath>}}`;
//! - `{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt}}` and the other
//!   dynamic variables, except `{{$env ...}}`, which would expose the
//!   environment of the machine running the mock.
//!
//! An unresolved reference renders as an empty string and is reported.

use rustman_core::vars::dynamic::{Dynamic, process_env};
use rustman_core::vars::{Segment, Template, TemplateError};
use serde_json::Value;
use serde_json_path::JsonPath;

use crate::request::RequestView;

/// What a template can read: the request and its JSON body, if any.
pub struct Context<'r, 'a> {
    request: &'r RequestView<'a>,
    json: Option<Value>,
}

impl<'r, 'a> Context<'r, 'a> {
    pub fn new(request: &'r RequestView<'a>) -> Self {
        Self {
            json: request.json(),
            request,
        }
    }
}

/// A rendered template and the references that did not resolve.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rendered<T> {
    pub value: T,
    pub unresolved: Vec<String>,
}

/// Check that `text` parses as a template, for errors at load time.
pub fn check(text: &str) -> Result<(), TemplateError> {
    Template::parse(text).map(|_| ())
}

/// Render `text`; text that is not a valid template is kept as is.
pub fn render_text(text: &str, context: &Context<'_, '_>) -> Rendered<String> {
    let Ok(template) = Template::parse(text) else {
        return Rendered {
            value: text.to_owned(),
            unresolved: Vec::new(),
        };
    };
    let mut out = Rendered::<String>::default();
    for segment in template.segments() {
        match segment {
            Segment::Literal(literal) => out.value.push_str(literal),
            Segment::Var(name) => match lookup(name, context) {
                Some(value) => out.value.push_str(&value),
                None => out.unresolved.push(name.clone()),
            },
            Segment::Dynamic(dynamic) => match dynamic_value(dynamic) {
                Some(value) => out.value.push_str(&value),
                None => out.unresolved.push(dynamic.to_string()),
            },
            // Mock responses never read provider secrets.
            Segment::Secret(reference) => out.unresolved.push(format!("secret:{reference}")),
        }
    }
    out
}

/// Render every string inside `value` (object keys stay as written).
pub fn render_value(value: &Value, context: &Context<'_, '_>) -> Rendered<Value> {
    let mut unresolved = Vec::new();
    let value = walk(value, context, &mut unresolved);
    Rendered { value, unresolved }
}

fn walk(value: &Value, context: &Context<'_, '_>, unresolved: &mut Vec<String>) -> Value {
    match value {
        Value::String(text) => {
            let rendered = render_text(text, context);
            unresolved.extend(rendered.unresolved);
            Value::String(rendered.value)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| walk(item, context, unresolved))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), walk(v, context, unresolved)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn dynamic_value(dynamic: &Dynamic) -> Option<String> {
    match dynamic {
        Dynamic::Env(_) => None,
        other => other.evaluate(process_env),
    }
}

fn lookup(name: &str, context: &Context<'_, '_>) -> Option<String> {
    let rest = name.strip_prefix("request.")?;
    let (kind, key) = rest.split_once('.')?;
    let request = context.request;
    match kind {
        "path" => request.param(key).map(str::to_owned),
        "query" => request.query_values(key).first().map(|v| (*v).to_owned()),
        "header" => request.header(key).map(str::to_owned),
        "body" => body_value(context.json.as_ref()?, key),
        _ => None,
    }
}

/// The first node `path` selects in `body`: strings as is, other values
/// as JSON.
fn body_value(body: &Value, path: &str) -> Option<String> {
    let path = JsonPath::parse(path).ok()?;
    let node = path.query(body).first()?;
    Some(match node {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue};
    use serde_json::json;

    fn with_request<T>(f: impl FnOnce(&Context<'_, '_>) -> T) -> T {
        let mut headers = HeaderMap::new();
        headers.insert("x-tenant", HeaderValue::from_static("acme"));
        let body = Bytes::from_static(br#"{"amount": 5, "user": {"name": "Ana"}}"#);
        let mut request = RequestView::new("POST", "/users/42", Some("q=hi"), &headers, &body);
        request.params.push(("id".into(), "42".into()));
        f(&Context::new(&request))
    }

    #[test]
    fn request_values_are_substituted() {
        let rendered = with_request(|c| {
            render_text(
                "{{request.path.id}} {{request.query.q}} {{request.header.X-Tenant}} {{request.body.$.user.name}} {{request.body.$.amount}}",
                c,
            )
        });
        assert_eq!(rendered.value, "42 hi acme Ana 5");
        assert!(rendered.unresolved.is_empty());
    }

    #[test]
    fn structured_bodies_render_their_strings() {
        let body = json!({"id": "{{request.path.id}}", "n": 1, "list": ["{{request.query.q}}"]});
        let rendered = with_request(|c| render_value(&body, c));
        assert_eq!(rendered.value, json!({"id": "42", "n": 1, "list": ["hi"]}));
    }

    #[test]
    fn unresolved_references_render_empty_and_are_reported() {
        let rendered = with_request(|c| {
            render_text(
                "[{{request.path.nope}}|{{token}}|{{$env HOME}}|{{request.body.$.x}}]",
                c,
            )
        });
        assert_eq!(rendered.value, "[|||]");
        assert_eq!(
            rendered.unresolved,
            [
                "request.path.nope",
                "token",
                "$env HOME",
                "request.body.$.x"
            ]
        );
    }

    #[test]
    fn dynamic_variables_are_generated() {
        let rendered = with_request(|c| render_text("{{$uuid}}", c));
        assert_eq!(rendered.value.len(), 36);
        assert!(check("{{unclosed").is_err());
        assert!(check("{{$nope}}").is_err());
    }
}
