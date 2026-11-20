//! `aws-secrets-manager` and `aws-ssm` (cargo feature `secrets-aws`):
//! `GetSecretValue` and `GetParameter` (with decryption) over the AWS JSON
//! protocol, signed with the engine's SigV4 signer, with keys from the
//! AWS default credential chain.

mod credentials;

use rustman_core::model::{Auth, AwsCredentials, AwsSigV4};
use rustman_core::secret::Secret;
use rustman_core::secrets::provider::{FetchFuture, LoginFuture, ProviderError, SecretProvider};
use serde_json::{Value, json};

pub use credentials::{Chain, Keys};

use crate::context::Context;
use crate::rest::{Call, Reply, Rest};

/// The AWS service a provider reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    SecretsManager,
    Ssm,
}

impl Service {
    /// The provider type.
    pub fn kind(self) -> &'static str {
        match self {
            Self::SecretsManager => "aws-secrets-manager",
            Self::Ssm => "aws-ssm",
        }
    }

    /// The SigV4 signing name and endpoint prefix.
    fn signing_name(self) -> &'static str {
        match self {
            Self::SecretsManager => "secretsmanager",
            Self::Ssm => "ssm",
        }
    }

    /// The service's own endpoint variable (`AWS_ENDPOINT_URL_<SERVICE>`).
    fn endpoint_var(self) -> &'static str {
        match self {
            Self::SecretsManager => "AWS_ENDPOINT_URL_SECRETS_MANAGER",
            Self::Ssm => "AWS_ENDPOINT_URL_SSM",
        }
    }

    fn target(self) -> &'static str {
        match self {
            Self::SecretsManager => "secretsmanager.GetSecretValue",
            Self::Ssm => "AmazonSSM.GetParameter",
        }
    }

    fn body(self, path: &str) -> Value {
        match self {
            Self::SecretsManager => json!({"SecretId": path}),
            Self::Ssm => json!({"Name": parameter_name(path), "WithDecryption": true}),
        }
    }
}

/// Whether `region` is a region name such as `eu-west-1`.
fn valid_region(region: &str) -> bool {
    !region.is_empty()
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A hierarchical parameter name starts with `/`, which a reference
/// (`ssm/app/key`) cannot carry: it is added back.
fn parameter_name(path: &str) -> String {
    if path.contains('/') && !path.starts_with('/') {
        format!("/{path}")
    } else {
        path.to_owned()
    }
}

/// The name probed by `secret test`: it does not exist.
const PROBE_NAME: &str = "rustman-secret-test-probe";

/// Settings of an AWS provider.
pub struct Settings {
    pub region: Option<String>,
    pub profile: Option<String>,
    /// From the user config only.
    pub endpoint: Option<String>,
}

pub struct Aws {
    service: Service,
    region: Option<String>,
    endpoint: Option<String>,
    chain: Chain,
    rest: Rest,
}

impl Aws {
    pub fn new(
        service: Service,
        settings: Settings,
        ctx: Context,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            service,
            region: settings.region,
            endpoint: settings.endpoint,
            chain: Chain::new(service.kind(), ctx, settings.profile),
            rest: Rest::new(timeout),
        }
    }

    fn kind(&self) -> &'static str {
        self.service.kind()
    }

    /// `region`, else `AWS_REGION`, else `AWS_DEFAULT_REGION`. It becomes
    /// part of the endpoint host, so only `[a-z0-9-]` is accepted: a
    /// workspace must not steer signed requests to another host.
    fn region(&self) -> Result<String, ProviderError> {
        let region = self
            .region
            .clone()
            .or_else(|| self.chain.var("AWS_REGION"))
            .or_else(|| self.chain.var("AWS_DEFAULT_REGION"))
            .ok_or_else(|| {
                ProviderError::Config(format!(
                    "{}: no region (set 'region' or AWS_REGION)",
                    self.kind()
                ))
            })?;
        if !valid_region(&region) {
            return Err(ProviderError::Config(format!(
                "{}: invalid region '{region}' (expected letters, digits and '-')",
                self.kind()
            )));
        }
        Ok(region)
    }

    /// `endpoint` from the user config, else the SDKs' endpoint variables,
    /// else the regional endpoint.
    fn endpoint(&self, region: &str) -> String {
        let configured = self
            .endpoint
            .clone()
            .or_else(|| self.chain.var(self.service.endpoint_var()))
            .or_else(|| self.chain.var("AWS_ENDPOINT_URL"));
        match configured {
            Some(url) => url.trim_end_matches('/').to_owned(),
            None => format!(
                "https://{}.{region}.amazonaws.com",
                self.service.signing_name()
            ),
        }
    }

    /// Send the service's read for `path`.
    async fn call(&self, path: &str) -> Result<Reply, ProviderError> {
        let region = self.region()?;
        let keys = self.chain.keys().await?;
        let auth = AwsSigV4 {
            region: region.clone(),
            service: self.service.signing_name().to_owned(),
            credentials: AwsCredentials::Fields {
                access_key: keys.access_key.clone(),
                secret_key: keys.secret_key.expose().clone(),
                session_token: keys.session_token.as_ref().map(|t| t.expose().clone()),
            },
            unsigned_payload: false,
        };
        let mut call = Call::post(
            format!("{}/", self.endpoint(&region)),
            self.service.body(path),
        )
        .header("Content-Type", "application/x-amz-json-1.1".to_owned())
        .header("X-Amz-Target", self.service.target().to_owned());
        call.auth = Some(Auth::AwsSigV4(Box::new(auth)));
        call.secrets.push(keys.secret_key.clone());
        call.secrets.extend(keys.session_token.clone());
        self.rest
            .send(call)
            .await
            .map_err(|e| ProviderError::Unavailable(format!("{}: {e}", self.kind())))
    }

    async fn read(&self, path: &str) -> Result<String, ProviderError> {
        let reply = self.call(path).await?;
        if reply.status != 200 {
            return Err(self.error(&reply, path));
        }
        let body = reply.json().ok_or_else(|| self.invalid())?;
        match self.service {
            Service::SecretsManager => secret_value(self.kind(), &body),
            Service::Ssm => body
                .pointer("/Parameter/Value")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
        .ok_or_else(|| self.invalid())
    }

    fn invalid(&self) -> ProviderError {
        ProviderError::Unavailable(format!("{}: invalid response", self.kind()))
    }

    /// The error of a failed call, from the AWS error type (the message
    /// is not echoed).
    fn error(&self, reply: &Reply, path: &str) -> ProviderError {
        let kind = self.kind();
        let code = error_code(reply);
        match code.as_str() {
            "ResourceNotFoundException" | "ParameterNotFound" => {
                ProviderError::NotFound(format!("{kind}: secret {path} not found"))
            }
            "AccessDeniedException" => {
                ProviderError::Auth(format!("{kind}: permission denied for {path}"))
            }
            c if is_auth_failure(c) => {
                ProviderError::Auth(format!("{kind}: authentication failed"))
            }
            "" => ProviderError::Unavailable(format!(
                "{kind}: server returned {} for {path}",
                reply.status
            )),
            c => ProviderError::Unavailable(format!("{kind}: {c} for {path}")),
        }
    }
}

/// `SecretString`, else `SecretBinary` as UTF-8 text.
fn secret_value(kind: &str, body: &Value) -> Option<String> {
    if let Some(text) = body.get("SecretString").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    let binary = body.get("SecretBinary").and_then(Value::as_str)?;
    crate::cloud::utf8_text(kind, binary).ok()
}

/// The error type of an AWS JSON error body (`__type`, without its
/// namespace).
fn error_code(reply: &Reply) -> String {
    let body = reply.json();
    let code = body
        .as_ref()
        .and_then(|b| b.get("__type").or_else(|| b.get("code")))
        .and_then(Value::as_str)
        .unwrap_or_default();
    code.rsplit('#').next().unwrap_or(code).to_owned()
}

fn is_auth_failure(code: &str) -> bool {
    matches!(
        code,
        "UnrecognizedClientException"
            | "InvalidSignatureException"
            | "ExpiredTokenException"
            | "IncompleteSignature"
            | "InvalidClientTokenId"
            | "MissingAuthenticationToken"
    )
}

impl SecretProvider for Aws {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move { Ok(Secret::new(path, self.read(path).await?)) })
    }

    /// Reads a name that does not exist: a not-found (or access denied)
    /// answer proves the keys were accepted.
    fn check<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async move {
            let reply = self.call(PROBE_NAME).await?;
            let accepted = matches!(
                error_code(&reply).as_str(),
                "ResourceNotFoundException" | "ParameterNotFound" | "AccessDeniedException"
            );
            if reply.status == 200 || accepted {
                return Ok(());
            }
            Err(self.error(&reply, PROBE_NAME))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_names_get_their_leading_slash() {
        assert_eq!(parameter_name("app/key"), "/app/key");
        assert_eq!(parameter_name("/app/key"), "/app/key");
        assert_eq!(parameter_name("plain"), "plain");
    }

    #[test]
    fn error_types_are_classified() {
        let reply = |body: &str| Reply {
            status: 400,
            body: body.as_bytes().to_vec(),
        };
        let r = reply(r#"{"__type":"com.amazonaws.x#ResourceNotFoundException","message":"m"}"#);
        assert_eq!(error_code(&r), "ResourceNotFoundException");
        assert!(is_auth_failure("UnrecognizedClientException"));
        assert!(!is_auth_failure("ThrottlingException"));
        let body = json!({"SecretBinary": "c2VjcmV0"});
        assert_eq!(secret_value("k", &body).as_deref(), Some("secret"));
    }
}
