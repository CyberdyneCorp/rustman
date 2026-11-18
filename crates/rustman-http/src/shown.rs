//! How exports (`export curl`, snippets) show request values: through the
//! [`Redactor`], or as they are when secrets are shown on purpose.

use std::borrow::Cow;

use rustman_core::Redactor;
use rustman_core::redact::sensitive_header;
use rustman_core::secret::{placeholder, placeholders};
use rustman_core::vars::{Resolved, ResolvedRequest};

/// Redacted values, or the values as they are with `--show-secrets`.
pub(crate) struct Shown {
    redactor: Option<Redactor>,
}

impl Shown {
    pub fn new(request: &ResolvedRequest, show_secrets: bool) -> Self {
        Self {
            redactor: (!show_secrets).then(|| Redactor::for_request(request)),
        }
    }

    pub fn text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match &self.redactor {
            Some(redactor) => redactor.text(text),
            None => Cow::Borrowed(text),
        }
    }

    pub fn resolved(&self, value: &Resolved) -> String {
        match &self.redactor {
            Some(redactor) => redactor.resolved(value),
            None => value.text.clone(),
        }
    }

    /// A header value; credentials (`Authorization`, `Cookie`, ...) are
    /// replaced whole, by the placeholders of the secrets they contain, or by
    /// the header's own placeholder.
    pub fn header(&self, name: &str, value: &Resolved) -> String {
        let redacted = self.resolved(value);
        match sensitive_header(name) {
            Some(canonical) if self.redactor.is_some() => {
                or_placeholder(placeholders(&redacted), canonical)
            }
            _ => redacted,
        }
    }

    /// Like [`Shown::header`], but a credential that is only an auth scheme
    /// and placeholders (`Bearer «redacted:api_token»`) keeps its scheme.
    pub fn header_with_scheme(&self, name: &str, value: &Resolved) -> String {
        let redacted = self.resolved(value);
        match sensitive_header(name) {
            Some(_) if self.redactor.is_some() && is_scheme_and_placeholders(&redacted) => redacted,
            _ => self.header(name, value),
        }
    }

    /// An auth credential (a password, a secret key): its placeholders, or
    /// `«redacted:<name>»` when it holds none, whatever its length.
    pub fn credential(&self, value: &str, name: &str) -> String {
        match &self.redactor {
            Some(redactor) => or_placeholder(placeholders(&redactor.text(value)), name),
            None => value.to_owned(),
        }
    }
}

fn or_placeholder(found: String, name: &str) -> String {
    if found.is_empty() {
        placeholder(name)
    } else {
        found
    }
}

const START: &str = "«redacted:";

/// Whether `text` is placeholders (or, in placeholder mode, `{{name}}`
/// markers), optionally after one auth scheme word.
fn is_scheme_and_placeholders(text: &str) -> bool {
    let (scheme, rest) = text.split_once(' ').unwrap_or(("", text));
    let scheme_ok = scheme
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-');
    scheme_ok && !rest.is_empty() && is_tokens(rest)
}

/// Whether `text` is only `«redacted:...»` and `{{...}}` tokens.
fn is_tokens(text: &str) -> bool {
    let mut rest = text;
    while !rest.is_empty() {
        let close = if rest.starts_with(START) {
            "»"
        } else if rest.starts_with("{{") {
            "}}"
        } else {
            return false;
        };
        let Some(end) = rest.find(close) else {
            return false;
        };
        rest = &rest[end + close.len()..];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_scheme_may_stay_next_to_placeholders() {
        assert!(is_scheme_and_placeholders("Bearer «redacted:t»"));
        assert!(is_scheme_and_placeholders("«redacted:t»"));
        assert!(!is_scheme_and_placeholders("Bearer abc«redacted:t»"));
        assert!(!is_scheme_and_placeholders("Bearer abc"));
        assert!(!is_scheme_and_placeholders("a=b; c=«redacted:t»"));
        assert!(is_scheme_and_placeholders("Bearer {{api_token}}"));
        assert!(!is_scheme_and_placeholders("Bearer {{a}}x"));
        assert!(!is_scheme_and_placeholders("Bearer "));
    }
}
