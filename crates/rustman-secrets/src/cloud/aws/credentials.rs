//! The AWS default credential chain, in the SDKs' order: the environment
//! (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`), the
//! shared profile (`AWS_PROFILE`, else `default`) in `~/.aws/credentials`
//! and `~/.aws/config`, container credentials (ECS, EKS Pod Identity) and
//! the EC2 instance metadata service (IMDSv2). A configured `profile` is
//! the only source used.
//!
//! Profiles are read by the `aws_sigv4` auth's reader in `rustman-http`,
//! which refuses SSO, role and process profiles.

use std::time::Duration;

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::ProviderError;
use serde_json::Value;

use crate::cloud::{Expiring, Held, METADATA_TIMEOUT};
use crate::context::Context;
use crate::rest::{Call, Rest};

/// The container credentials endpoint for a relative URI.
const CONTAINER_HOST: &str = "http://169.254.170.2";
/// The EC2 instance metadata service.
const IMDS_HOST: &str = "http://169.254.169.254";

/// AWS keys as the SigV4 signer of `rustman-http` reads them
/// ([`rustman_http::AwsKeys`]), with the secret parts held as [`Secret`]s
/// so they join the redaction set.
#[derive(Clone)]
pub struct Keys {
    pub access_key: String,
    pub secret_key: Secret<String>,
    pub session_token: Option<Secret<String>>,
}

impl Keys {
    fn new(access_key: String, secret_key: String, session_token: Option<String>) -> Self {
        Self {
            access_key,
            secret_key: Secret::new("aws_secret_key", secret_key),
            session_token: session_token
                .filter(|t| !t.is_empty())
                .map(|t| Secret::new("aws_session_token", t)),
        }
    }
}

impl From<rustman_http::AwsKeys> for Keys {
    fn from(keys: rustman_http::AwsKeys) -> Self {
        Self::new(keys.access_key, keys.secret_key, keys.session_token)
    }
}

/// Where a provider's keys come from.
pub struct Chain {
    /// The provider type, which starts every message.
    kind: &'static str,
    ctx: Context,
    profile: Option<String>,
    probe: Rest,
    /// Keys from a container or instance endpoint, which expire.
    remote: Held<Keys>,
}

impl Chain {
    pub fn new(kind: &'static str, ctx: Context, profile: Option<String>) -> Self {
        Self {
            kind,
            ctx,
            profile,
            probe: Rest::exactly(METADATA_TIMEOUT),
            remote: Held::default(),
        }
    }

    /// The first keys the chain finds.
    pub async fn keys(&self) -> Result<Keys, ProviderError> {
        if let Some(name) = &self.profile {
            return self.profile(name)?.ok_or_else(|| self.no_profile(name));
        }
        if let Some(keys) = self.env_keys() {
            return Ok(keys);
        }
        let named = self.ctx.var("AWS_PROFILE");
        let name = named.as_deref().unwrap_or("default");
        match self.profile(name)? {
            Some(keys) => Ok(keys),
            None if named.is_some() => Err(self.no_profile(name)),
            None => self.remote.get(|| self.endpoint_keys()).await,
        }
    }

    /// The environment variable `name` of the provider's context.
    pub fn var(&self, name: &str) -> Option<String> {
        self.ctx.var(name)
    }

    fn no_profile(&self, name: &str) -> ProviderError {
        ProviderError::Auth(format!(
            "{}: profile '{name}' not found in the shared AWS files",
            self.kind
        ))
    }

    fn env_keys(&self) -> Option<Keys> {
        rustman_http::aws_env_keys(&|var: &str| self.ctx.var(var)).map(Keys::from)
    }

    fn profile(&self, name: &str) -> Result<Option<Keys>, ProviderError> {
        let env = |var: &str| self.ctx.var(var);
        let found = rustman_http::aws_profile_keys(name, &env).map_err(|e| {
            let message = e.to_string();
            let message = message.trim_start_matches("aws_sigv4: ");
            ProviderError::Auth(format!("{}: {message}", self.kind))
        })?;
        Ok(found.map(Keys::from))
    }

    async fn endpoint_keys(&self) -> Result<Expiring<Keys>, ProviderError> {
        if let Some(keys) = self.container_keys().await? {
            return Ok(keys);
        }
        if let Some(keys) = self.instance_keys().await {
            return Ok(keys);
        }
        Err(ProviderError::Auth(format!(
            "{}: no credentials found in the default chain",
            self.kind
        )))
    }

    /// ECS task or EKS Pod Identity credentials.
    async fn container_keys(&self) -> Result<Option<Expiring<Keys>>, ProviderError> {
        let url = match (
            self.ctx.var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI"),
            self.ctx.var("AWS_CONTAINER_CREDENTIALS_FULL_URI"),
        ) {
            (Some(relative), _) => format!("{CONTAINER_HOST}{relative}"),
            (None, Some(full)) => full,
            (None, None) => return Ok(None),
        };
        let mut call = Call::get(url);
        if let Some(token) = self.container_token() {
            call = call.credential("Authorization", &token);
        }
        let reply = self.probe.send(call).await.map_err(|e| {
            ProviderError::Unavailable(format!("{}: container credentials: {e}", self.kind))
        })?;
        match reply.json().filter(|_| reply.status == 200) {
            Some(body) => Ok(Some(self.parse_keys(&body)?)),
            None => Err(ProviderError::Auth(format!(
                "{}: container credentials endpoint returned {}",
                self.kind, reply.status
            ))),
        }
    }

    fn container_token(&self) -> Option<Secret<String>> {
        let token = self
            .ctx
            .var("AWS_CONTAINER_AUTHORIZATION_TOKEN")
            .or_else(|| {
                let file = self.ctx.var("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE")?;
                let text = std::fs::read_to_string(file).ok()?;
                Some(text.trim().to_owned())
            })?;
        Some(Secret::new("aws_container_token", token))
    }

    /// IMDSv2: a session token, the instance role, then its keys. `None`
    /// when there is no metadata service (not an EC2 machine).
    async fn instance_keys(&self) -> Option<Expiring<Keys>> {
        if self
            .ctx
            .var("AWS_EC2_METADATA_DISABLED")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        {
            return None;
        }
        let host = self
            .ctx
            .var("AWS_EC2_METADATA_SERVICE_ENDPOINT")
            .unwrap_or_else(|| IMDS_HOST.to_owned());
        let host = host.trim_end_matches('/');
        let put = Call::get(format!("{host}/latest/api/token"))
            .method("PUT")
            .header("X-aws-ec2-metadata-token-ttl-seconds", "21600".to_owned());
        let token = self.text(put).await?;
        let token = Secret::new("aws_imds_token", token);
        let base = format!("{host}/latest/meta-data/iam/security-credentials/");
        let get = |url: String| Call::get(url).credential("X-aws-ec2-metadata-token", &token);
        let roles = self.text(get(base.clone())).await?;
        let role = roles.lines().next()?.trim().to_owned();
        let keys = self.text(get(format!("{base}{role}"))).await?;
        let body: Value = serde_json::from_str(&keys).ok()?;
        self.parse_keys(&body).ok()
    }

    async fn text(&self, call: Call<'_>) -> Option<String> {
        let reply = self.probe.send(call).await.ok()?;
        (reply.status == 200)
            .then(|| String::from_utf8(reply.body).ok())
            .flatten()
    }

    fn parse_keys(&self, body: &Value) -> Result<Expiring<Keys>, ProviderError> {
        let field = |name: &str| body.get(name).and_then(Value::as_str).map(str::to_owned);
        let (Some(access_key), Some(secret_key)) = (field("AccessKeyId"), field("SecretAccessKey"))
        else {
            return Err(ProviderError::Auth(format!(
                "{}: the credentials endpoint returned no keys",
                self.kind
            )));
        };
        let keys = Keys::new(access_key, secret_key, field("Token"));
        let lifetime = field("Expiration").and_then(|e| lifetime_until(&e));
        Ok(Expiring::new(keys, lifetime))
    }
}

/// The time left until the RFC 3339 instant `expiration`.
fn lifetime_until(expiration: &str) -> Option<Duration> {
    use time::format_description::well_known::Rfc3339;
    let at = time::OffsetDateTime::parse(expiration, &Rfc3339).ok()?;
    let left = at - time::OffsetDateTime::now_utc();
    Duration::try_from(left).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expirations_become_lifetimes() {
        let later = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
        let text = later
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let left = lifetime_until(&text).unwrap();
        assert!(left > Duration::from_secs(3500) && left <= Duration::from_secs(3600));
        assert_eq!(lifetime_until("2001-01-01T00:00:00Z"), None);
        assert_eq!(lifetime_until("soon"), None);
    }
}
