//! Neutralizing `{{secret:...}}` references taken from the document (issue
//! #95). An OpenAPI document is external input: a parameter name, an
//! example or a server URL holding `{{secret:vault/kv/prod/db#password}}`
//! must not become a saved request that reads a local secret and sends it
//! to the document's server. Every such reference is written escaped
//! (`\{{secret:...}}`, literal text) with a warning; the `{{variables}}`
//! the generator writes itself stay.

use rustman_core::model::{Auth, Body, JsonContent, NameValue, PartSource};
use rustman_core::secrets::provider::neutralize_references;
use serde_json::Value;

use crate::openapi::Diagnostic;

/// Neutralize the references in every templated field of `request`, with
/// one warning per reference at `pointer`.
pub(super) fn request(
    request: &mut rustman_core::model::HttpRequest,
    pointer: &str,
    warnings: &mut Vec<Diagnostic>,
) {
    let mut found = neutralize_references(&mut request.url);
    for rows in [&mut request.query, &mut request.headers]
        .into_iter()
        .flatten()
    {
        pairs(rows, &mut found);
    }
    match &mut request.body {
        Some(Body::Json(JsonContent::Value(value))) => json(value, &mut found),
        Some(Body::Json(JsonContent::Text(text)) | Body::Text(text)) => {
            found.extend(neutralize_references(text));
        }
        Some(Body::Form(fields)) => pairs(fields, &mut found),
        Some(Body::Multipart(parts)) => {
            for part in parts {
                found.extend(neutralize_references(&mut part.name));
                if let PartSource::Text(text) = &mut part.source {
                    found.extend(neutralize_references(text));
                }
            }
        }
        Some(Body::None | Body::File(_)) | None => {}
    }
    if let Some(a) = &mut request.auth {
        found.extend(auth(a));
    }
    warn(found, pointer, warnings);
}

/// Neutralize the references in the strings of `auth`: the kinds the
/// generator writes (basic, bearer, API key, OAuth2).
pub(super) fn auth(auth: &mut Auth) -> Vec<String> {
    let mut found = Vec::new();
    let mut n = |s: &mut String| found.extend(neutralize_references(s));
    match auth {
        Auth::Basic { username, password } | Auth::Digest { username, password } => {
            n(username);
            n(password);
        }
        Auth::Bearer { token } => n(token),
        Auth::ApiKey { name, value, .. } => {
            n(name);
            n(value);
        }
        Auth::OAuth2(o) => {
            n(&mut o.token_url);
            o.auth_url.iter_mut().for_each(&mut n);
            n(&mut o.client_id);
            o.client_secret.iter_mut().for_each(&mut n);
            o.scopes.iter_mut().flatten().for_each(&mut n);
        }
        Auth::None | Auth::AwsSigV4(_) | Auth::Jwt(_) | Auth::Plugin(_) => {}
    }
    found
}

/// Neutralize the references in `text`, with one warning per reference
/// at `pointer`.
pub(super) fn text(text: &mut String, pointer: &str, warnings: &mut Vec<Diagnostic>) {
    let found = neutralize_references(text);
    warn(found, pointer, warnings);
}

/// One warning per distinct reference, at `pointer`.
pub(super) fn warn(found: Vec<String>, pointer: &str, warnings: &mut Vec<Diagnostic>) {
    for reference in found {
        let message = format!(
            "secret reference {reference} from the document neutralized: it is kept as literal text and never resolved"
        );
        let warning = Diagnostic::warning(pointer, message);
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
}

fn pairs(rows: &mut [NameValue], found: &mut Vec<String>) {
    for row in rows {
        found.extend(neutralize_references(&mut row.name));
        found.extend(neutralize_references(&mut row.value));
    }
}

/// Strings and object keys alike: both are interpolated.
fn json(value: &mut Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) => found.extend(neutralize_references(text)),
        Value::Array(items) => items.iter_mut().for_each(|v| json(v, found)),
        Value::Object(map) => {
            for (mut key, mut v) in std::mem::take(map) {
                found.extend(neutralize_references(&mut key));
                json(&mut v, found);
                map.insert(key, v);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
