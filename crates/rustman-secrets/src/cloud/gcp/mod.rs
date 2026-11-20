//! `gcp-secret-manager` (cargo feature `secrets-gcp`): `<secret>[@version]`
//! (default `latest`) from Secret Manager's REST API, with an access token
//! from Application Default Credentials.

mod adc;

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::{FetchFuture, LoginFuture, ProviderError, SecretProvider};
use serde_json::Value;

pub use adc::{Adc, CREDENTIALS_VAR, KIND};

use crate::cloud::{Held, utf8_text};
use crate::context::Context;
use crate::rest::{Call, Reply, segment};

const DEFAULT_ENDPOINT: &str = "https://secretmanager.googleapis.com";

pub struct Gcp {
    project: Option<String>,
    endpoint: String,
    adc: Adc,
    token: Held<Secret<String>>,
}

impl Gcp {
    /// `endpoint` comes from the user config only.
    pub fn new(
        project: Option<String>,
        endpoint: Option<String>,
        ctx: Context,
        timeout: std::time::Duration,
    ) -> Self {
        let endpoint = endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());
        Self {
            project,
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            adc: Adc::new(ctx, timeout),
            token: Held::default(),
        }
    }

    /// `project`, else `GOOGLE_CLOUD_PROJECT`, else the credentials file's.
    fn project(&self) -> Result<String, ProviderError> {
        let ctx = self.adc.context();
        let from_file = || {
            adc::credentials_file(ctx)
                .ok()
                .flatten()
                .and_then(|f| f.project())
        };
        self.project
            .clone()
            .or_else(|| ctx.var("GOOGLE_CLOUD_PROJECT"))
            .or_else(|| ctx.var("CLOUDSDK_CORE_PROJECT"))
            .or_else(from_file)
            .ok_or_else(|| {
                ProviderError::Config(format!(
                    "{KIND}: no project (set 'project' or GOOGLE_CLOUD_PROJECT)"
                ))
            })
    }

    async fn token(&self) -> Result<Secret<String>, ProviderError> {
        self.token.get(|| self.adc.token()).await
    }

    async fn read(&self, path: &str) -> Result<String, ProviderError> {
        let (name, version) = path.split_once('@').unwrap_or((path, "latest"));
        if name.is_empty() || version.is_empty() || name.contains('/') {
            return Err(ProviderError::Config(format!(
                "{KIND}: path '{path}' must be <secret>[@version]"
            )));
        }
        let project = self.project()?;
        let token = self.token().await?;
        let url = format!(
            "{}/v1/projects/{}/secrets/{}/versions/{}:access",
            self.endpoint,
            segment(&project),
            segment(name),
            segment(version)
        );
        let reply = self
            .send(Call::get(url).credential("Authorization", &token))
            .await?;
        let body = check(&reply, path)?;
        let data = body
            .pointer("/payload/data")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderError::Unavailable(format!("{KIND}: invalid response")))?;
        utf8_text(KIND, data)
    }

    async fn send(&self, call: Call<'_>) -> Result<Reply, ProviderError> {
        self.adc
            .rest()
            .send(call)
            .await
            .map_err(|e| ProviderError::Unavailable(format!("{KIND}: {e}")))
    }
}

/// The body of a successful reply, else the error for `path`.
fn check(reply: &Reply, path: &str) -> Result<Value, ProviderError> {
    match reply.status {
        200..=299 => reply
            .json()
            .ok_or_else(|| ProviderError::Unavailable(format!("{KIND}: invalid response"))),
        401 => Err(ProviderError::Auth(format!(
            "{KIND}: authentication failed"
        ))),
        403 => Err(ProviderError::Auth(format!(
            "{KIND}: permission denied for {path}"
        ))),
        404 => Err(ProviderError::NotFound(format!(
            "{KIND}: secret {path} not found"
        ))),
        status => Err(ProviderError::Unavailable(format!(
            "{KIND}: server returned {status} for {path}"
        ))),
    }
}

impl SecretProvider for Gcp {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move { Ok(Secret::new(path, self.read(path).await?)) })
    }

    /// Obtains an access token.
    fn check<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async move { self.token().await.map(|_| ()) })
    }
}
