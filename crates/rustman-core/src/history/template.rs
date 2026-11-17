//! Turning resolved text back into a template: every byte range that came
//! from a secret becomes `{{name}}` again, so replay can re-resolve it from
//! the secret store, and nothing else is a placeholder (`{{` in literal text
//! is escaped as `\{{`).

use std::ops::Range;

use crate::secret::Secret;
use crate::vars::{Resolved, SecretSpan};

/// Rebuilds templates from resolved text and the secrets used to build it.
pub(crate) struct Templater<'a> {
    secrets: &'a [Secret<String>],
}

/// A field whose literal value is never stored, such as an `Authorization`
/// header written without a secret variable.
pub(crate) struct Withheld;

impl<'a> Templater<'a> {
    pub(crate) fn new(secrets: &'a [Secret<String>]) -> Self {
        Self { secrets }
    }

    /// `resolved` as a template.
    pub(crate) fn resolved(&self, resolved: &Resolved) -> String {
        self.parts(&resolved.text, &resolved.secret_spans).render()
    }

    /// Text without recorded spans (a JSON string, an auth field) as a
    /// template: each occurrence of a known secret value becomes its name.
    pub(crate) fn text(&self, text: &str) -> String {
        self.parts(text, &[]).render()
    }

    /// A credential (a sensitive header value, a password, a token): kept
    /// only when its value comes from secrets, with at most an auth scheme
    /// such as `Bearer ` written literally before them.
    pub(crate) fn credential(&self, text: &str, spans: &[SecretSpan]) -> Result<String, Withheld> {
        let parts = self.parts(text, spans);
        if parts.only_secrets() {
            Ok(parts.render())
        } else {
            Err(Withheld)
        }
    }

    fn parts<'t>(&self, text: &'t str, spans: &[SecretSpan]) -> Parts<'t> {
        let mut found: Vec<(Range<usize>, String)> = spans
            .iter()
            .filter(|s| s.range.end <= text.len() && !s.range.is_empty())
            .map(|s| (s.range.clone(), s.name.clone()))
            .collect();
        for secret in self.secrets.iter().filter(|s| !s.expose().is_empty()) {
            let value = secret.expose().as_str();
            found.extend(
                text.match_indices(value)
                    .map(|(at, _)| (at..at + value.len(), secret.name().to_owned())),
            );
        }
        found.sort_by_key(|(range, _)| (range.start, std::cmp::Reverse(range.end)));
        let mut parts = Parts {
            text,
            pieces: Vec::new(),
        };
        let mut copied = 0;
        for (range, name) in found {
            if range.start < copied {
                continue;
            }
            parts.pieces.push(Piece::Literal(copied..range.start));
            parts.pieces.push(Piece::Secret(name));
            copied = range.end;
        }
        parts.pieces.push(Piece::Literal(copied..text.len()));
        parts
    }
}

enum Piece {
    Literal(Range<usize>),
    Secret(String),
}

struct Parts<'t> {
    text: &'t str,
    pieces: Vec<Piece>,
}

impl Parts<'_> {
    fn render(&self) -> String {
        let mut out = String::with_capacity(self.text.len());
        for piece in &self.pieces {
            match piece {
                Piece::Literal(range) => {
                    out.push_str(&self.text[range.clone()].replace("{{", "\\{{"))
                }
                Piece::Secret(name) => {
                    out.push_str("{{");
                    out.push_str(name);
                    out.push_str("}}");
                }
            }
        }
        out
    }

    /// Every literal piece is empty, except perhaps a leading auth scheme
    /// (`Bearer `) when secrets follow it.
    fn only_secrets(&self) -> bool {
        let has_secret = self.pieces.iter().any(|p| matches!(p, Piece::Secret(_)));
        let literals = self.pieces.iter().filter_map(|p| match p {
            Piece::Literal(range) => Some(&self.text[range.clone()]),
            Piece::Secret(_) => None,
        });
        literals
            .enumerate()
            .all(|(i, text)| text.is_empty() || (i == 0 && has_secret && is_auth_scheme(text)))
    }
}

/// `Bearer `, `Basic `, `Token `: one token and the spaces after it.
fn is_auth_scheme(text: &str) -> bool {
    let scheme = text.trim_end_matches(' ');
    scheme.len() < text.len()
        && !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~+/".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> Vec<Secret<String>> {
        vec![Secret::new("token", "s3cr3t".to_owned())]
    }

    fn spanned(text: &str, name: &str, range: Range<usize>) -> Resolved {
        Resolved {
            text: text.to_owned(),
            secret_spans: vec![SecretSpan {
                name: name.to_owned(),
                range,
            }],
        }
    }

    #[test]
    fn secret_spans_and_values_become_placeholders() {
        let secrets = secrets();
        let t = Templater::new(&secrets);
        assert_eq!(t.resolved(&spanned("a-xyz-b", "k", 2..5)), "a-{{k}}-b");
        assert_eq!(t.text("x s3cr3t y s3cr3t"), "x {{token}} y {{token}}");
        assert_eq!(t.text("literal {{braces}}"), "literal \\{{braces}}");
    }

    #[test]
    fn credentials_keep_only_secrets_and_a_scheme() {
        let secrets = secrets();
        let t = Templater::new(&secrets);
        assert_eq!(
            t.credential("Bearer s3cr3t", &[]).ok().as_deref(),
            Some("Bearer {{token}}")
        );
        assert_eq!(
            t.credential("s3cr3t", &[]).ok().as_deref(),
            Some("{{token}}")
        );
        assert_eq!(t.credential("", &[]).ok().as_deref(), Some(""));
        assert!(t.credential("Bearer abc123", &[]).is_err());
        assert!(t.credential("s3cr3t; session=abc", &[]).is_err());
        assert!(t.credential("Bearer x s3cr3t", &[]).is_err());
    }
}
