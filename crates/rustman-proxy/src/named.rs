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
//!   a header value or a body.

use rustman_core::Redactor;
use rustman_core::redact::MIN_SECRET_PIECE;
use rustman_import::secrets::{
    is_auth_scheme_value, is_known_token, is_secret_header_name, is_secret_name, known_tokens,
};
use serde_json::Value;

use crate::capture::Exchange;

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

    /// The secret fields of a JSON or form body, and the well-known tokens
    /// anywhere in it.
    fn body(&mut self, bytes: &[u8]) {
        if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
            self.json("", &value);
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
