//! The secrets of a capture that [`crate::Capture::redactor`] redacts
//! wherever a capture is shown or exported (live lines, `--json`, the TUI,
//! HAR). They are found with the rules the save path's secret extraction
//! (the importers' `secrets::extract`) uses, so nothing that a save would
//! turn into a placeholder is ever shown raw:
//!
//! - headers with a secret name (`X-Api-Key`) or a well-known token value;
//! - query parameters and form or JSON body fields with a secret name
//!   (`token`, `password`, `api_key`) or a well-known token value;
//! - values that start with an auth scheme (`Bearer x`), whatever they are
//!   called;
//! - well-known tokens (GitHub, AWS, Slack, JWTs...) anywhere in the URL,
//!   a header value or a body;
//! - URL path segments that saving turns into secret variables (see
//!   [`crate::save::template`]);
//! - JSON body fields that are secrets by where they sit: a neutral name
//!   (`value`, `current`) or a random-looking value inside an object called
//!   like a credential (`{"credentials": {"value": ...}}`), see
//!   [`nested_secrets`].

use rustman_core::Redactor;
use rustman_core::redact::MIN_SECRET_PIECE;
use rustman_import::secrets::{
    is_auth_scheme_value, is_known_token, is_secret_header_name, is_secret_name, known_tokens,
};
use serde_json::Value;

use crate::capture::Exchange;
use crate::save::secrets::looks_random;
use crate::save::template::templatize;

/// Teach `redactor` every secret of `exchange` the save path would move
/// (see the module documentation).
pub(crate) fn add_named_secrets(redactor: &mut Redactor, exchange: &Exchange) {
    let mut found = Found(redactor);
    let headers = exchange
        .request_headers
        .iter()
        .chain(&exchange.response_headers);
    for (name, value) in headers {
        found.field(name, value, is_secret_header_name);
        found.tokens(value);
    }
    for (name, value) in query_pairs(&exchange.url) {
        found.field(&name, &value, is_secret_name);
    }
    found.tokens(&exchange.url);
    for (name, value) in templatize(crate::save::split(&exchange.url).path).secrets {
        found.add(&name, &value);
    }
    for body in [&exchange.request_body, &exchange.response_body] {
        found.body(&body.bytes);
    }
}

/// Whether the save path parameterizes a header called `name` holding
/// `value`: a secret header name, a well-known token or an auth scheme.
pub(crate) fn is_secret_header(name: &str, value: &str) -> bool {
    is_secret_header_name(name) || is_secret_value(value)
}

/// Whether the save path parameterizes a query parameter or field called
/// `name` holding `value`.
pub(crate) fn is_secret_field(name: &str, value: &str) -> bool {
    is_secret_name(name) || is_secret_value(value)
}

/// A value that is a secret whatever it is called: one holding a
/// well-known token, or one that starts with an auth scheme.
fn is_secret_value(value: &str) -> bool {
    is_known_token(value) || is_auth_scheme_value(value)
}

/// The redactor being taught.
struct Found<'a>(&'a mut Redactor);

impl Found<'_> {
    fn add(&mut self, name: &str, value: &str) {
        if value.len() >= MIN_SECRET_PIECE {
            self.0.add_value(name, value);
        }
    }

    /// A header, parameter or field: its whole value when it is a secret,
    /// and also the credential after the scheme of `Bearer x` (which, when
    /// too short to redact alone, is redacted with its scheme).
    fn field(&mut self, name: &str, value: &str, is_name: fn(&str) -> bool) {
        if !is_name(name) && !is_secret_value(value) {
            return;
        }
        self.add(name, value);
        if is_auth_scheme_value(value)
            && let Some((_, credentials)) = value.trim().split_once(' ')
        {
            self.add(name, credentials.trim());
        }
    }

    /// Every well-known token in `text`.
    fn tokens(&mut self, text: &str) {
        for token in known_tokens(text) {
            self.add(token.name, &text[token.range]);
        }
    }

    /// The secret fields of a JSON or form body (by name, value or, for
    /// JSON, by the object they sit in), and the well-known tokens anywhere
    /// in it.
    fn body(&mut self, bytes: &[u8]) {
        if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
            self.json("", &value);
            for nested in nested_secrets(&value) {
                self.add(&nested.name, &nested.value);
            }
        } else if let Ok(text) = std::str::from_utf8(bytes) {
            for (name, value) in text.split('&').filter_map(|p| p.split_once('=')) {
                self.field(name, value, is_secret_name);
            }
        }
        self.tokens(&String::from_utf8_lossy(bytes));
    }

    /// The secret string fields of a JSON value; the strings of an array
    /// are judged under the array's name.
    fn json(&mut self, name: &str, value: &Value) {
        match value {
            Value::String(s) => self.field(name, s, is_secret_name),
            Value::Object(map) => map.iter().for_each(|(key, field)| self.json(key, field)),
            Value::Array(items) => items.iter().for_each(|item| self.json(name, item)),
            _ => {}
        }
    }
}

/// A JSON string field that is a secret by where it sits.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NestedSecret {
    /// The variable base: the container and the field, `credentials_value`.
    pub name: String,
    pub value: String,
}

/// Field names that only hold a value, whatever it is: inside a credential
/// object (`{"credentials": {"value": ...}}`) they hold the credential.
const VALUE_HOLDERS: &[&str] = &[
    "value",
    "val",
    "raw",
    "data",
    "current",
    "new",
    "old",
    "previous",
    "next",
    "plain",
    "plaintext",
    "encoded",
    "hash",
    "access",
    "primary",
    "secondary",
];

/// Object names that mark what they hold as credentials: secret names
/// (`credentials`, `token`, `secret`) and the auth words the secret names
/// leave out (`authentication`, `authorization`, `oauth`, `creds`).
fn is_credential_container(name: &str) -> bool {
    const AUTH_WORDS: &[&str] = &[
        "authentication",
        "authorization",
        "oauth",
        "oauth2",
        "creds",
        "cred",
    ];
    is_secret_name(name) || AUTH_WORDS.iter().any(|w| w.eq_ignore_ascii_case(name))
}

/// The string fields of `value` that are secrets only because of the
/// object or array they sit in, at any depth: inside one called like a
/// credential ([`is_credential_container`]), a field with a
/// [`VALUE_HOLDERS`] name or a random-looking value. Fields already secret
/// by their own name or value are left out (the importer's extraction and
/// [`Found::field`] catch them), as are descriptive siblings such as
/// `type` or `user`.
pub(crate) fn nested_secrets(value: &Value) -> Vec<NestedSecret> {
    let mut out = Vec::new();
    walk_nested(value, "", None, &mut out);
    out
}

fn walk_nested(value: &Value, name: &str, container: Option<&str>, out: &mut Vec<NestedSecret>) {
    match value {
        Value::String(s) => {
            let Some(container) = container else {
                return;
            };
            if is_secret_field(name, s) || s.len() < MIN_SECRET_PIECE {
                return;
            }
            let holder = VALUE_HOLDERS.iter().any(|h| h.eq_ignore_ascii_case(name));
            if holder || looks_random(s) {
                let base = if name.is_empty() || name == container {
                    container.to_owned()
                } else {
                    format!("{container}_{name}")
                };
                out.push(NestedSecret {
                    name: base,
                    value: s.clone(),
                });
            }
        }
        Value::Object(map) => {
            let inner = match container {
                Some(c) => Some(c),
                None if !name.is_empty() && is_credential_container(name) => Some(name),
                None => None,
            };
            for (key, field) in map {
                walk_nested(field, key, inner, out);
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| walk_nested(item, name, container, out)),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn found(json: &str) -> Vec<(String, String)> {
        let value: Value = serde_json::from_str(json).unwrap();
        nested_secrets(&value)
            .into_iter()
            .map(|n| (n.name, n.value))
            .collect()
    }

    #[test]
    fn neutral_fields_inside_credential_objects_are_secrets() {
        let random = "q8ZK3vLw0pTn6RxYb2HsJm9CdFgA5eUi7oQ1";
        let json = format!(
            r#"{{"credentials": {{"value": "v4lue-1", "type": "basic"}},
               "auth": {{"oauth": {{"new": "n3w-2", "expires_in": "3600"}}}},
               "token": [{{"data": "d4ta-3"}}],
               "oauth2": {{"opaque": "{random}", "scope": "read write"}}}}"#
        );
        assert_eq!(
            found(&json),
            [
                ("credentials_value".to_owned(), "v4lue-1".to_owned()),
                ("auth_new".to_owned(), "n3w-2".to_owned()),
                ("token_data".to_owned(), "d4ta-3".to_owned()),
                ("oauth2_opaque".to_owned(), random.to_owned()),
            ]
        );
    }

    #[test]
    fn ordinary_nested_values_are_not_secrets() {
        let json = r#"{"profile": {"value": "blue", "data": "x-y-z", "new": "true"},
            "author": {"value": "Alice Smith"},
            "settings": {"theme": {"current": "dark"}},
            "auth": {"type": "bearer", "user": "alice", "method": "oauth2"},
            "credentials": {"password": "named-already"}}"#;
        assert_eq!(found(json), Vec::<(String, String)>::new());
    }
}
