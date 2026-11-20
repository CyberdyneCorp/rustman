//! Typed provider errors (design D2). Messages never contain a secret value
//! or a credential.

use super::reference::SecretRef;

/// The exit code of a configuration problem.
pub const EXIT_CONFIG: i32 = 2;
/// The exit code of every other resolution failure (a request error).
pub const EXIT_REQUEST: i32 = 3;

/// Why a provider could not produce a value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// The secret does not exist; the message names the provider and path.
    #[error("{0}")]
    NotFound(String),
    /// The secret exists but has no such field.
    #[error("field '{0}' not found")]
    FieldNotFound(String),
    /// Missing, expired or rejected credentials.
    #[error("{0}")]
    Auth(String),
    #[error("{provider}: timed out after {ms} ms")]
    Timeout { provider: String, ms: u64 },
    /// The provider could not be reached or failed.
    #[error("{0}")]
    Unavailable(String),
    /// A reference to a provider no configuration defines.
    #[error("unknown secret provider '{name}' (configured: {})", list(.configured))]
    UnknownProvider {
        name: String,
        configured: Vec<String>,
    },
    /// The provider is misconfigured, or its type is not in this build.
    #[error("{0}")]
    Config(String),
}

impl ProviderError {
    /// 2 for a configuration problem, 3 for everything else: the kind of
    /// failure. A failure that stops a reference from resolving is still a
    /// request error (exit code 3) for `send`, `run` and `secret get`.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Config(_) => EXIT_CONFIG,
            _ => EXIT_REQUEST,
        }
    }

    /// The error for a provider type compiled out of this build.
    pub fn not_built(kind: &str, feature: &str) -> Self {
        Self::Config(format!(
            "secret provider type '{kind}' is not available in this build (feature {feature})"
        ))
    }
}

fn list(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

/// A reference that could not be resolved: the request is not sent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unresolved secret {reference}: {error}")]
pub struct UnresolvedSecret {
    pub reference: SecretRef,
    pub error: ProviderError,
}

impl UnresolvedSecret {
    /// The kind of its failure ([`ProviderError::exit_code`]). The CLI
    /// reports every unresolved reference as a request error, exit code 3.
    pub fn exit_code(&self) -> i32 {
        self.error.exit_code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unresolved(text: &str, error: ProviderError) -> UnresolvedSecret {
        UnresolvedSecret {
            reference: SecretRef::parse(text).unwrap(),
            error,
        }
    }

    #[test]
    fn every_kind_maps_to_its_message_and_exit_code() {
        let cases = [
            (
                ProviderError::NotFound("vault: kv/app/db not found".into()),
                "vault: kv/app/db not found",
                3,
            ),
            (
                ProviderError::FieldNotFound("user".into()),
                "field 'user' not found",
                3,
            ),
            (
                ProviderError::Auth("vault: authentication failed".into()),
                "vault: authentication failed",
                3,
            ),
            (
                ProviderError::Timeout {
                    provider: "vault".into(),
                    ms: 10_000,
                },
                "vault: timed out after 10000 ms",
                3,
            ),
            (
                ProviderError::Unavailable("vault: connection refused".into()),
                "vault: connection refused",
                3,
            ),
            (
                ProviderError::UnknownProvider {
                    name: "lastpass".into(),
                    configured: vec!["ci".into(), "vault".into()],
                },
                "unknown secret provider 'lastpass' (configured: ci, vault)",
                3,
            ),
            (
                ProviderError::not_built("aws-secrets-manager", "secrets-aws"),
                "secret provider type 'aws-secrets-manager' is not available in this build (feature secrets-aws)",
                2,
            ),
        ];
        for (error, message, code) in cases {
            assert_eq!(error.to_string(), message);
            assert_eq!(error.exit_code(), code, "{message}");
        }
    }

    #[test]
    fn unresolved_secrets_name_the_reference() {
        let err = unresolved(
            "vault/kv/app/db#user",
            ProviderError::FieldNotFound("user".into()),
        );
        assert_eq!(
            err.to_string(),
            "unresolved secret vault/kv/app/db#user: field 'user' not found"
        );
        assert_eq!(err.exit_code(), 3);
        let none = ProviderError::UnknownProvider {
            name: "x".into(),
            configured: Vec::new(),
        };
        assert_eq!(
            none.to_string(),
            "unknown secret provider 'x' (configured: none)"
        );
    }
}
