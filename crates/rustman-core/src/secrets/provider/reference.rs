//! Secret references: `{{secret:<provider>/<path>[#field]}}` (secret-providers
//! spec "Secret reference syntax").

use std::fmt;

/// What a template placeholder starts with to name a provider secret.
pub const PREFIX: &str = "secret:";

/// A reference to a secret held by a configured provider.
///
/// Its text form (`<provider>/<path>[#field]`) is also its redaction label.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretRef {
    pub provider: String,
    /// Passed to the provider as is (it may carry `?version=<n>`).
    pub path: String,
    /// A field of a structured (JSON or YAML) secret, as a dotted key path.
    pub field: Option<String>,
}

impl SecretRef {
    /// Parse `<provider>/<path>[#field]`, the text after `secret:`. The
    /// path and field may hold spaces (1Password item names), but not at
    /// either end; no part may hold other whitespace or braces.
    pub fn parse(text: &str) -> Result<Self, String> {
        let invalid = || invalid_reference(&format!("{PREFIX}{text}"));
        let (location, field) = match text.split_once('#') {
            Some((location, field)) if !field.is_empty() => (location, Some(field.to_owned())),
            Some(_) => return Err(invalid()),
            None => (text, None),
        };
        let (provider, path) = location.split_once('/').ok_or_else(invalid)?;
        let well_formed = !provider.is_empty()
            && !path.is_empty()
            && !provider.contains(' ')
            && text.trim() == text
            && !text
                .chars()
                .any(|c| (c.is_whitespace() && c != ' ') || c == '{' || c == '}');
        if !well_formed {
            return Err(invalid());
        }
        Ok(Self {
            provider: provider.to_owned(),
            path: path.to_owned(),
            field,
        })
    }

    /// The provider and path without the field: the cache key.
    pub fn location(&self) -> String {
        format!("{}/{}", self.provider, self.path)
    }
}

impl fmt::Display for SecretRef {
    /// `<provider>/<path>[#field]`, the reference text and redaction label.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.path)?;
        match &self.field {
            Some(field) => write!(f, "#{field}"),
            None => Ok(()),
        }
    }
}

/// The message for a reference that cannot be parsed; `text` starts with `secret:`.
pub fn invalid_reference(text: &str) -> String {
    format!("invalid secret reference '{text}' (expected {PREFIX}<provider>/<path>)")
}

/// Whether `text` holds anything that reads as a `{{secret:...}}`
/// placeholder, well-formed or not (whitespace after `{{` allowed, escapes
/// ignored): for refusing references where they are not allowed.
pub fn mentions_reference(text: &str) -> bool {
    text.match_indices("{{")
        .any(|(at, _)| text[at + 2..].trim_start().starts_with(PREFIX))
}

/// Every malformed `{{secret:...}}` placeholder of a file's raw text, with
/// its 1-based line, so loading a workspace reports them before any send.
/// An escaped `\{{` is a literal, as in templates.
pub fn malformed_in(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut offset = 0;
    while let Some(start) = text[offset..].find("{{") {
        let open = offset + start + 2;
        if text[..offset + start].ends_with('\\') {
            offset = open;
            continue;
        }
        let Some(len) = text[open..].find("}}") else {
            break;
        };
        let inner = text[open..open + len].trim();
        if let Some(rest) = inner.strip_prefix(PREFIX)
            && let Err(message) = SecretRef::parse(rest)
        {
            let line = text[..open].matches('\n').count() + 1;
            found.push((line, message));
        }
        offset = open + len + 2;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_parse_provider_path_and_field() {
        let r = SecretRef::parse("vault/kv/app/stripe#api_key").unwrap();
        assert_eq!(r.provider, "vault");
        assert_eq!(r.path, "kv/app/stripe");
        assert_eq!(r.field.as_deref(), Some("api_key"));
        assert_eq!(r.to_string(), "vault/kv/app/stripe#api_key");
        assert_eq!(r.location(), "vault/kv/app/stripe");
        let r = SecretRef::parse("vault/kv/app/db?version=3#password").unwrap();
        assert_eq!(r.path, "kv/app/db?version=3");
        // 1Password names may hold spaces.
        let r = SecretRef::parse("op/Eng Team/Stripe Key/api token").unwrap();
        assert_eq!(r.path, "Eng Team/Stripe Key/api token");
        let r = SecretRef::parse("vault/kv/app#db password").unwrap();
        assert_eq!(r.field.as_deref(), Some("db password"));
        let r = SecretRef::parse("ci/STRIPE_KEY").unwrap();
        assert_eq!(r.to_string(), "ci/STRIPE_KEY");
        assert_eq!(r.field, None);
    }

    #[test]
    fn malformed_reference_names_the_expected_form() {
        assert_eq!(
            SecretRef::parse("vault").unwrap_err(),
            "invalid secret reference 'secret:vault' (expected secret:<provider>/<path>)"
        );
        for bad in [
            "vault/", "/path", "vault/x#", "a b/c", "", " op/a", "op/a ", "op/a\tb", "op/a\nb",
        ] {
            assert!(SecretRef::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn references_are_mentioned_whatever_their_form() {
        for text in [
            "{{secret:op/a/b}}",
            "Bearer {{ secret:op/a/b }}",
            "\\{{secret:x}}",
            "{{\n\tsecret:bad",
        ] {
            assert!(mentions_reference(text), "{text}");
        }
        for text in [
            "{{base_url}}",
            "secret:op/a/b",
            "{secret:op/a}",
            "{{ secrets }}",
        ] {
            assert!(!mentions_reference(text), "{text}");
        }
    }

    #[test]
    fn malformed_placeholders_are_found_with_their_line() {
        let text = "url: '{{secret:vault}}'\nheaders:\n  a: '{{secret:ok/x}} {{ secret:bad }}'\n";
        let found = malformed_in(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, 1);
        assert_eq!(
            found[0].1,
            "invalid secret reference 'secret:vault' (expected secret:<provider>/<path>)"
        );
        assert_eq!(found[1].0, 3);
        assert!(malformed_in("{{base_url}} {{secret:a/b#c}} {{open").is_empty());
        assert!(malformed_in("{{ secret:op/Eng Team/Stripe/token }}").is_empty());
        assert!(malformed_in("'\\{{secret:env:HOME}}' \\{{secret:vault}}").is_empty());
        assert_eq!(malformed_in("\\{{secret:ok/x}} {{secret:vault}}").len(), 1);
    }
}
