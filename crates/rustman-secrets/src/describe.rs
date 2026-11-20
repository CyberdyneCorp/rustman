//! What `rustman secret get|test|list-providers` show: never a value.

use rustman_core::secrets::provider::{ProviderError, ProviderInfo, SecretRef, config};

/// What a masked value prints as.
pub const MASK: &str = "••••••••";

/// `•••••••• (<n> chars, <provider>)`: the length of a value, never the value.
pub fn masked(value: &str, provider: &str) -> String {
    format!("{MASK} ({} chars, {provider})", value.chars().count())
}

/// The reference a user typed: `<provider>/<path>[#field]`, optionally
/// written `secret:...` or `{{secret:...}}`.
pub fn reference(text: &str) -> Result<SecretRef, String> {
    let text = text.trim();
    let text = text
        .strip_prefix("{{")
        .and_then(|t| t.strip_suffix("}}"))
        .map_or(text, str::trim);
    let text = text
        .strip_prefix(rustman_core::secrets::provider::PREFIX)
        .unwrap_or(text);
    SecretRef::parse(text)
}

/// Whether the provider's type is in this build, as `secret list-providers`
/// shows it.
pub fn status(info: &ProviderInfo) -> String {
    match config::feature_of(&info.kind) {
        Some(feature) if !crate::is_built(feature) => {
            format!("not in this build (feature {feature})")
        }
        _ => "available".to_owned(),
    }
}

/// One row of `secret list-providers`: name, type, auth, origin, status.
pub fn row(info: &ProviderInfo) -> [String; 5] {
    [
        info.name.clone(),
        info.kind.clone(),
        info.auth.clone().unwrap_or_else(|| "-".to_owned()),
        info.origin.to_string(),
        status(info),
    ]
}

/// The line `secret test` prints for a failed check of provider `name`:
/// the message, prefixed with the name unless it already starts with it.
pub fn failure(name: &str, error: &ProviderError) -> String {
    let message = error.to_string();
    if message.starts_with(&format!("{name}:")) {
        message
    } else {
        format!("{name}: {message}")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rustman_core::secrets::provider::Origin;

    use super::*;

    #[test]
    fn values_are_masked_with_their_length() {
        assert_eq!(
            masked("abcdefghijklmnopqrstuvwx", "vault"),
            "•••••••• (24 chars, vault)"
        );
        assert_eq!(masked("é", "op"), "•••••••• (1 chars, op)");
    }

    #[test]
    fn references_accept_the_template_forms() {
        for text in [
            "vault/kv/app#pw",
            "secret:vault/kv/app#pw",
            "{{secret:vault/kv/app#pw}}",
            "{{ secret:vault/kv/app#pw }}",
        ] {
            assert_eq!(reference(text).unwrap().to_string(), "vault/kv/app#pw");
        }
        assert_eq!(
            reference("vault").unwrap_err(),
            "invalid secret reference 'secret:vault' (expected secret:<provider>/<path>)"
        );
    }

    #[test]
    fn rows_show_settings_and_build_status_only() {
        let info = |kind: &str, auth: Option<&str>| ProviderInfo {
            name: "p".into(),
            kind: kind.into(),
            origin: Origin::User,
            auth: auth.map(str::to_owned),
            timeout: Duration::from_secs(1),
            ttl: Duration::from_secs(1),
        };
        assert_eq!(
            row(&info("vault", Some("oidc"))),
            ["p", "vault", "oidc", "user", "available"].map(String::from)
        );
        let aws = status(&info("aws-ssm", None));
        if crate::is_built("secrets-aws") {
            assert_eq!(aws, "available");
        } else {
            assert_eq!(aws, "not in this build (feature secrets-aws)");
        }
        let err = ProviderError::Auth("vault: authentication failed".into());
        assert_eq!(failure("vault", &err), "vault: authentication failed");
        assert_eq!(failure("hcv", &err), "hcv: vault: authentication failed");
    }
}
