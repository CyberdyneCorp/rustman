//! The redaction step every value of the page model passes (design D1).
//!
//! [`Rendered`] has a private field and no conversion from strings, so the
//! only way to make one is [`DocsRedactor::render`] (or a method built on
//! it). Renderers consume only the model, so they never see raw workspace
//! values.

use std::fmt;

use rustman_core::model::{Auth, NameValue, Request};
use rustman_core::redact::{self, MIN_HEADER_CREDENTIAL};
use rustman_core::secret::placeholder;
use rustman_core::{Redactor, Secret, Workspace};
use serde::{Serialize, Serializer};

/// Headers whose values never appear in generated docs, in their canonical
/// spelling (api-docs-generation "Redaction in generated docs").
pub const DOCS_SENSITIVE_HEADERS: [&str; 5] = [
    "Authorization",
    "Cookie",
    "Set-Cookie",
    "Proxy-Authorization",
    "X-Api-Key",
];

/// The canonical name of `name` when its value is always redacted.
pub fn sensitive_header(name: &str) -> Option<&'static str> {
    DOCS_SENSITIVE_HEADERS
        .into_iter()
        .find(|h| h.eq_ignore_ascii_case(name.trim()))
}

/// Words that make a header a credential header beyond
/// [`DOCS_SENSITIVE_HEADERS`] (`X-Auth-Token`, `X-Client-Secret`), split
/// on `-` and `_`.
const CREDENTIAL_WORDS: [&str; 7] = [
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "auth",
    "credential",
];

/// The name a header's value is redacted under when it carries a
/// credential: the canonical name of a [`DOCS_SENSITIVE_HEADERS`] entry,
/// or the name as written when one of its words names a credential
/// (`X-Auth-Token`, `X-Client-Secret`, `Api-Key`).
pub fn credential_header(name: &str) -> Option<String> {
    if let Some(canonical) = sensitive_header(name) {
        return Some(canonical.to_owned());
    }
    let name = name.trim();
    let lower = name.to_ascii_lowercase();
    let words: Vec<&str> = lower.split(['-', '_']).collect();
    let named = words.iter().any(|w| CREDENTIAL_WORDS.contains(w))
        || words.windows(2).any(|pair| pair == ["api", "key"]);
    named.then(|| name.to_owned())
}

/// Text that passed the redaction step.
///
/// It cannot be built from a raw string:
///
/// ```compile_fail
/// let leaked = rustman_docs::Rendered("token".to_owned());
/// ```
///
/// ```compile_fail
/// let leaked: rustman_docs::Rendered = String::from("token").into();
/// ```
///
/// ```compile_fail
/// let leaked: rustman_docs::Rendered = serde_json::from_str("\"token\"").unwrap();
/// ```
///
/// Only through the redactor:
///
/// ```
/// let redactor = rustman_docs::DocsRedactor::new(rustman_core::Redactor::new());
/// assert_eq!(redactor.render("{{base_url}}/users").as_str(), "{{base_url}}/users");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rendered(String);

impl Rendered {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// A shorter copy: the first `max` bytes (on a character boundary),
    /// already redacted. Returns whether anything was cut.
    pub(crate) fn truncated(&self, max: usize) -> (Self, bool) {
        if self.0.len() <= max {
            return (self.clone(), false);
        }
        let mut end = max;
        while !self.0.is_char_boundary(end) {
            end -= 1;
        }
        (Self(self.0[..end].to_owned()), true)
    }
}

impl fmt::Display for Rendered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Rendered {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// The redactor of a docs build: every secret value of the workspace's
/// environments, and the literal credentials written in its files.
#[derive(Clone, Debug, Default)]
pub struct DocsRedactor {
    redactor: Redactor,
}

impl DocsRedactor {
    pub fn new(redactor: Redactor) -> Self {
        Self { redactor }
    }

    /// The redactor of `workspace` with `secrets` (the values of every
    /// environment's secrets) and the literal credentials of its requests
    /// and collections.
    pub fn for_workspace(workspace: &Workspace, secrets: &[Secret<String>]) -> Self {
        let mut redactor = Redactor::new();
        for secret in secrets {
            redactor.add(secret);
        }
        let collections = std::iter::once(&workspace.tree).chain(workspace.collections());
        for collection in collections {
            add_literals(
                &mut redactor,
                collection.meta.headers.as_deref(),
                collection.meta.auth.as_ref(),
            );
        }
        for request in workspace.requests() {
            let headers = request.effective_headers();
            add_literals(&mut redactor, Some(&headers), request.effective_auth());
            if let Request::Grpc(grpc) = request.request() {
                add_literals(&mut redactor, grpc.metadata.as_deref(), None);
            }
            crate::literals::add_request_literals(&mut redactor, request.request());
        }
        Self { redactor }
    }

    /// `text` with every known secret value replaced.
    pub fn render(&self, text: &str) -> Rendered {
        Rendered(self.redactor.text(text).into_owned())
    }

    /// A header value as a page shows it: a sensitive header's value only
    /// when it is a variable reference (`Bearer {{token}}`), never a
    /// literal credential.
    pub fn header_value(&self, name: &str, value: &str) -> Rendered {
        match credential_header(name) {
            Some(shown) if !is_reference(value) => Rendered(placeholder(&shown)),
            _ => self.render(value),
        }
    }

    /// A recorded header value: always redacted for sensitive headers.
    pub fn recorded_header(&self, name: &str, value: &str) -> Rendered {
        match credential_header(name) {
            Some(shown) => Rendered(placeholder(&shown)),
            None => self.render(value),
        }
    }

    /// `value` with every known secret value replaced, as pretty JSON.
    pub fn json(&self, value: &serde_json::Value) -> Rendered {
        let mut value = value.clone();
        self.redactor.json(&mut value);
        let text = serde_json::to_string_pretty(&value).unwrap_or_default();
        self.render(&text)
    }

    /// Whether `text` holds a known secret value in any form (the leak scan).
    pub fn finds_secret(&self, text: &str) -> bool {
        self.redactor.finds_secret(text)
    }
}

/// Whether a header value carries only a variable reference, optionally
/// after a scheme: `{{token}}` or `Bearer {{token}}`.
fn is_reference(value: &str) -> bool {
    let value = value.trim();
    let credential = value.split_once(' ').map_or(value, |(_, rest)| rest.trim());
    credential
        .strip_prefix("{{")
        .and_then(|rest| rest.strip_suffix("}}"))
        .is_some_and(|name| !name.is_empty() && !name.contains(['{', '}']))
}

/// Redact the literal credentials of `headers` and `auth`: a value pasted
/// into a tracked file is never a secret of the store.
fn add_literals(redactor: &mut Redactor, headers: Option<&[NameValue]>, auth: Option<&Auth>) {
    for header in headers.into_iter().flatten() {
        add_header_literal(redactor, &header.name, &header.value);
    }
    match auth {
        Some(Auth::ApiKey { name, value, .. }) => {
            let name = credential_header(name).unwrap_or_else(|| name.trim().to_owned());
            add_literal(redactor, &name, value);
        }
        Some(auth) => redactor.add_auth(auth),
        None => {}
    }
}

/// The literal credential of a header: the core redactor reads its own
/// sensitive headers (schemes, cookies); the other credential headers
/// (`X-Api-Key`, `X-Auth-Token`) carry the credential as their value.
fn add_header_literal(redactor: &mut Redactor, name: &str, value: &str) {
    if value.contains("{{") {
        return;
    }
    if redact::sensitive_header(name).is_some() {
        redactor.add_header_value(name, value);
    } else if let Some(shown) = credential_header(name) {
        add_literal(redactor, &shown, value);
    }
}

/// Redact `value` as `«redacted:<name>»`, with its encoded forms (base64
/// for a `user:password` sent as Basic auth), unless it is a reference, a
/// placeholder or too short to be a credential.
pub(crate) fn add_literal(redactor: &mut Redactor, name: &str, value: &str) {
    let value = value.trim();
    let marker = value.contains("{{") || value.contains("«redacted:");
    if !marker && value.chars().count() >= MIN_HEADER_CREDENTIAL {
        redactor.add(&Secret::new(name, value.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> DocsRedactor {
        let secret = Secret::new("token", "CANARY-tok-123456".to_owned());
        DocsRedactor::new(Redactor::new().with(&secret))
    }

    #[test]
    fn secrets_become_placeholders() {
        let r = redactor();
        assert_eq!(
            r.render("a CANARY-tok-123456 b").as_str(),
            "a «redacted:token» b"
        );
        let json = serde_json::json!({"t": "CANARY-tok-123456"});
        assert!(r.json(&json).as_str().contains("«redacted:token»"));
        assert!(r.finds_secret("x CANARY-tok-123456"));
        assert!(!r.finds_secret("x «redacted:token»"));
    }

    #[test]
    fn sensitive_headers_show_references_only() {
        let r = redactor();
        assert_eq!(
            r.header_value("authorization", "Bearer {{token}}").as_str(),
            "Bearer {{token}}"
        );
        assert_eq!(r.header_value("X-API-KEY", "{{key}}").as_str(), "{{key}}");
        assert_eq!(
            r.header_value("Authorization", "Bearer abc").as_str(),
            "«redacted:Authorization»"
        );
        assert_eq!(
            r.header_value("Cookie", "a={{x}}; b=1").as_str(),
            "«redacted:Cookie»"
        );
        assert_eq!(r.header_value("Accept", "text/html").as_str(), "text/html");
        assert_eq!(
            r.recorded_header("Set-Cookie", "{{x}}").as_str(),
            "«redacted:Set-Cookie»"
        );
        assert_eq!(r.recorded_header("Content-Type", "a/b").as_str(), "a/b");
    }

    #[test]
    fn credential_headers_are_named_by_their_words() {
        assert_eq!(credential_header("x-api-key").as_deref(), Some("X-Api-Key"));
        for name in ["X-Auth-Token", "X-Custom-Token", "Client_Secret", "Api-Key"] {
            assert_eq!(credential_header(name).as_deref(), Some(name));
        }
        for name in ["Accept", "Idempotency-Key", "X-Author", "X-Request-Id"] {
            assert_eq!(credential_header(name), None, "{name}");
        }
        let r = DocsRedactor::default();
        assert_eq!(
            r.header_value("X-Custom-Token", "abc123").as_str(),
            "«redacted:X-Custom-Token»"
        );
        assert_eq!(r.header_value("X-Custom-Token", "{{t}}").as_str(), "{{t}}");
    }

    #[test]
    fn truncation_keeps_character_boundaries() {
        let r = DocsRedactor::default();
        let (cut, was_cut) = r.render("aé").truncated(2);
        assert_eq!((cut.as_str(), was_cut), ("a", true));
        let (same, was_cut) = r.render("ab").truncated(2);
        assert_eq!((same.as_str(), was_cut), ("ab", false));
    }
}
