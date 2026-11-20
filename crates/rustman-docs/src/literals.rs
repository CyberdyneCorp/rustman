//! Literal credentials a request file carries outside its headers and
//! auth: URL userinfo, credential-named query parameters and
//! credential-named body fields. Generated docs are published, so these
//! are redacted like the literal credential headers (design D1).

use rustman_core::Redactor;
use rustman_core::data_rows::secret_by_name;
use rustman_core::model::{Body, JsonContent, NameValue, PartSource, Request};
use serde_json::Value;

use crate::rendered::{add_literal, credential_header};

/// The name a URL's `user:password` part is redacted under.
const USERINFO: &str = "userinfo";

/// Whether a query parameter or body field named `name` carries a
/// credential: the data-row secret names (`password`, `token`, `secret`,
/// `api_key`) and the credential header words (`passwd`, `apikey`, ...).
pub fn credential_name(name: &str) -> bool {
    secret_by_name(name) || credential_header(name).is_some()
}

/// Register the literal credentials of `request`'s URL, query and body.
pub fn add_request_literals(redactor: &mut Redactor, request: &Request) {
    match request {
        Request::Http(r) => {
            add_url(redactor, &r.url);
            add_pairs(redactor, r.query.as_deref());
            add_body(redactor, r.body.as_ref());
        }
        Request::Graphql(r) => {
            add_url(redactor, &r.endpoint);
            for map in [&r.variables, &r.connection_params].into_iter().flatten() {
                map.iter().for_each(|(k, v)| add_field(redactor, k, v));
            }
        }
        Request::Jsonrpc(r) => {
            add_url(redactor, &r.endpoint);
            for params in r.calls().into_iter().filter_map(|c| c.params) {
                add_json(redactor, params);
            }
        }
        Request::Mcp(r) => {
            let params = r.params.iter().flatten();
            params.for_each(|(k, v)| add_field(redactor, k, v));
        }
        Request::Grpc(r) => {
            let messages = r.messages.iter().flat_map(|m| m.iter());
            messages
                .chain(r.message.as_deref())
                .for_each(|m| add_json(redactor, m));
        }
        Request::Ws(r) => add_url(redactor, &r.url),
        Request::Sse(r) => {
            add_url(redactor, &r.url);
            add_body(redactor, r.body.as_ref());
        }
        Request::Opaque(_) => {}
    }
}

/// The userinfo (`user:password@`) and credential-named query parameters
/// of a URL, which may still hold `{{variables}}`.
fn add_url(redactor: &mut Redactor, url: &str) {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if let Some((userinfo, _)) = authority.rsplit_once('@') {
        add_literal(redactor, USERINFO, userinfo);
        if let Some((_, password)) = userinfo.split_once(':') {
            add_literal(redactor, "password", password);
        }
    }
    let query = url.split_once('?').map(|(_, q)| q.split('#').next());
    for pair in query.flatten().unwrap_or_default().split('&') {
        if let Some((name, value)) = pair.split_once('=') {
            add_named(redactor, name, value);
        }
    }
}

fn add_pairs(redactor: &mut Redactor, pairs: Option<&[NameValue]>) {
    for pair in pairs.into_iter().flatten() {
        add_named(redactor, &pair.name, &pair.value);
    }
}

fn add_body(redactor: &mut Redactor, body: Option<&Body>) {
    match body {
        Some(Body::Json(JsonContent::Value(value))) => add_json(redactor, value),
        Some(Body::Json(JsonContent::Text(text))) => {
            if let Ok(value) = serde_json::from_str::<Value>(text) {
                add_json(redactor, &value);
            }
        }
        Some(Body::Form(fields)) => add_pairs(redactor, Some(fields)),
        Some(Body::Multipart(parts)) => {
            for part in parts {
                if let PartSource::Text(value) = &part.source {
                    add_named(redactor, &part.name, value);
                }
            }
        }
        _ => {}
    }
}

/// Every credential-named string field of a JSON value, at any depth.
fn add_json(redactor: &mut Redactor, value: &Value) {
    match value {
        Value::Object(map) => map.iter().for_each(|(k, v)| add_field(redactor, k, v)),
        Value::Array(items) => items.iter().for_each(|v| add_json(redactor, v)),
        _ => {}
    }
}

fn add_field(redactor: &mut Redactor, name: &str, value: &Value) {
    match value {
        Value::String(text) => add_named(redactor, name, text),
        Value::Number(number) => add_named(redactor, name, &number.to_string()),
        _ => add_json(redactor, value),
    }
}

fn add_named(redactor: &mut Redactor, name: &str, value: &str) {
    if credential_name(name) {
        add_literal(redactor, name.trim(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_names_cover_query_and_body_fields() {
        for name in [
            "api_key",
            "API-KEY",
            "password",
            "client_secret",
            "token",
            "passwd",
        ] {
            assert!(credential_name(name), "{name}");
        }
        for name in ["page", "user", "email", "author", "keyword"] {
            assert!(!credential_name(name), "{name}");
        }
    }

    #[test]
    fn url_userinfo_and_credential_query_values_are_redacted() {
        let mut redactor = Redactor::new();
        add_url(
            &mut redactor,
            "https://user:pw-literal-123@api.example/x?page=2&api_key=querysecret123#top",
        );
        let text =
            redactor.text("https://user:pw-literal-123@api.example/x?api_key=querysecret123");
        assert_eq!(
            text,
            "https://«redacted:userinfo»@api.example/x?api_key=«redacted:api_key»"
        );
        assert_eq!(redactor.text("-a pw-literal-123"), "-a «redacted:password»");
        assert_eq!(redactor.text("page=2"), "page=2");
    }

    #[test]
    fn references_and_short_values_stay() {
        let mut redactor = Redactor::new();
        add_url(&mut redactor, "{{base}}/x?token={{token}}&secret=abc");
        assert!(redactor.is_empty());
    }

    #[test]
    fn credential_body_fields_are_redacted_at_any_depth() {
        let mut redactor = Redactor::new();
        let body = serde_json::json!({
            "user": {"name": "ada", "password": "hunter2literal"},
            "items": [{"client_secret": "nested-secret-9"}],
        });
        add_json(&mut redactor, &body);
        assert_eq!(redactor.text("hunter2literal"), "«redacted:password»");
        assert_eq!(redactor.text("nested-secret-9"), "«redacted:client_secret»");
        assert_eq!(redactor.text("ada"), "ada");
    }
}
