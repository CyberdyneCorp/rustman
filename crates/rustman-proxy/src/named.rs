//! The values of secret-named headers (`X-Api-Key`), query parameters
//! and form or JSON body fields (`token`, `password`, `api_key`), which
//! [`crate::Capture::redactor`] redacts wherever a capture is shown or
//! exported. Names are judged by the importers' rules.

use rustman_core::Redactor;
use rustman_core::redact::MIN_SECRET_PIECE;
use rustman_import::secrets::{is_secret_header_name, is_secret_name};
use serde_json::Value;

use crate::capture::Exchange;

/// Teach `redactor` the values of secret-named headers (`X-Api-Key`),
/// query parameters and form or JSON body fields (`token`, `password`).
pub(crate) fn add_named_secrets(redactor: &mut Redactor, exchange: &Exchange) {
    let mut add = |name: &str, value: &str| {
        if value.len() >= MIN_SECRET_PIECE {
            redactor.add_value(name, value);
        }
    };
    let headers = exchange
        .request_headers
        .iter()
        .chain(&exchange.response_headers);
    for (name, value) in headers.filter(|(n, _)| is_secret_header_name(n)) {
        add(name, value);
    }
    for (name, value) in query_pairs(&exchange.url) {
        if is_secret_name(&name) {
            add(&name, &value);
        }
    }
    for body in [&exchange.request_body, &exchange.response_body] {
        for (name, value) in body_fields(&body.bytes) {
            add(&name, &value);
        }
    }
}

/// The secret-named fields of a JSON or form body.
fn body_fields(bytes: &[u8]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        json_fields(&value, &mut found);
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        let pairs = text.split('&').filter_map(|p| p.split_once('='));
        found.extend(
            pairs
                .filter(|(name, _)| is_secret_name(name))
                .map(|(n, v)| (n.to_owned(), v.to_owned())),
        );
    }
    found
}

fn json_fields(value: &Value, found: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, field) in map {
                match field {
                    Value::String(s) if is_secret_name(key) => {
                        found.push((key.clone(), s.clone()));
                    }
                    other => json_fields(other, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| json_fields(v, found)),
        _ => {}
    }
}

/// The query parameters of `url`, as written (not decoded).
pub(crate) fn query_pairs(url: &str) -> Vec<(String, String)> {
    let Some((_, query)) = url.split_once('?') else {
        return Vec::new();
    };
    let query = query.split('#').next().unwrap_or_default();
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) => (name.to_owned(), value.to_owned()),
            None => (pair.to_owned(), String::new()),
        })
        .collect()
}
