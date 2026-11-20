//! `DefaultAzureCredential`'s chain, in its order: a service principal
//! from the environment (`AZURE_TENANT_ID`, `AZURE_CLIENT_ID`,
//! `AZURE_CLIENT_SECRET`), workload identity (`AZURE_FEDERATED_TOKEN_FILE`),
//! managed identity (App Service's `IDENTITY_ENDPOINT`, else the instance
//! metadata service), then the Azure CLI (`az account get-access-token`).

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::ProviderError;
use serde_json::Value;

use crate::cloud::{Expiring, METADATA_TIMEOUT, bearer};
use crate::context::Context;
use crate::process;
use crate::rest::{Call, Rest};

pub const KIND: &str = "azure-key-vault";

/// The resource (and scope) of Key Vault tokens.
const RESOURCE: &str = "https://vault.azure.net";
const AUTHORITY: &str = "https://login.microsoftonline.com";
const IMDS_HOST: &str = "http://169.254.169.254";

/// Obtains Key Vault access tokens.
pub struct Credential {
    ctx: Context,
    rest: Rest,
    probe: Rest,
}

impl Credential {
    pub fn new(ctx: Context, timeout: std::time::Duration) -> Self {
        Self {
            ctx,
            rest: Rest::new(timeout),
            probe: Rest::exactly(METADATA_TIMEOUT),
        }
    }

    pub fn rest(&self) -> &Rest {
        &self.rest
    }

    /// A new token, as an `Authorization` header value.
    pub async fn token(&self) -> Result<Expiring<Secret<String>>, ProviderError> {
        if let Some(call) = self.service_principal()? {
            return self.exchange(&self.rest, call).await;
        }
        if let Some(token) = self.managed_identity().await {
            return Ok(token);
        }
        if let Some(token) = self.azure_cli().await {
            return Ok(token);
        }
        Err(ProviderError::Auth(format!(
            "{KIND}: no credentials found (DefaultAzureCredential)"
        )))
    }

    /// The client-credentials token request of a service principal with a
    /// secret, or of a workload identity with its federated token.
    fn service_principal(&self) -> Result<Option<Call<'static>>, ProviderError> {
        let (Some(tenant), Some(client_id)) = (
            self.ctx.var("AZURE_TENANT_ID"),
            self.ctx.var("AZURE_CLIENT_ID"),
        ) else {
            return Ok(None);
        };
        let (field, value) = match self.client_proof()? {
            Some(proof) => proof,
            None => return Ok(None),
        };
        let authority = self
            .ctx
            .var("AZURE_AUTHORITY_HOST")
            .unwrap_or_else(|| AUTHORITY.to_owned());
        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            authority.trim_end_matches('/'),
            crate::rest::segment(&tenant)
        );
        let scope = format!("{RESOURCE}/.default");
        let mut fields = vec![
            ("grant_type", "client_credentials"),
            ("client_id", client_id.as_str()),
            ("scope", scope.as_str()),
            (field, value.expose().as_str()),
        ];
        if field == "client_assertion" {
            fields.push((
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            ));
        }
        Ok(Some(Call::form(url, &fields, vec![value.clone()])))
    }

    /// The client secret, else the federated token of workload identity.
    fn client_proof(&self) -> Result<Option<(&'static str, Secret<String>)>, ProviderError> {
        if let Some(secret) = self.ctx.var("AZURE_CLIENT_SECRET") {
            return Ok(Some((
                "client_secret",
                Secret::new("azure_client_secret", secret),
            )));
        }
        let Some(file) = self.ctx.var("AZURE_FEDERATED_TOKEN_FILE") else {
            return Ok(None);
        };
        let token = std::fs::read_to_string(&file).map_err(|e| {
            ProviderError::Auth(format!(
                "{KIND}: cannot read AZURE_FEDERATED_TOKEN_FILE: {e}"
            ))
        })?;
        Ok(Some((
            "client_assertion",
            Secret::new("azure_federated_token", token.trim().to_owned()),
        )))
    }

    async fn exchange(
        &self,
        rest: &Rest,
        call: Call<'_>,
    ) -> Result<Expiring<Secret<String>>, ProviderError> {
        let reply = rest
            .send(call)
            .await
            .map_err(|e| ProviderError::Unavailable(format!("{KIND}: {e}")))?;
        bearer(KIND, &reply)
    }

    /// App Service's identity endpoint, else the instance metadata
    /// service; `None` off Azure.
    async fn managed_identity(&self) -> Option<Expiring<Secret<String>>> {
        let client_id = self.ctx.var("AZURE_CLIENT_ID");
        let client = client_id.map_or(String::new(), |id| format!("&client_id={id}"));
        let call = match (
            self.ctx.var("IDENTITY_ENDPOINT"),
            self.ctx.var("IDENTITY_HEADER"),
        ) {
            (Some(endpoint), Some(header)) => {
                let url = format!("{endpoint}?api-version=2019-08-01&resource={RESOURCE}{client}");
                Call::get(url).credential(
                    "X-IDENTITY-HEADER",
                    &Secret::new("azure_identity_header", header),
                )
            }
            _ => {
                let host = self
                    .ctx
                    .var("AZURE_POD_IDENTITY_AUTHORITY_HOST")
                    .unwrap_or_else(|| IMDS_HOST.to_owned());
                let url = format!(
                    "{}/metadata/identity/oauth2/token?api-version=2018-02-01&resource={RESOURCE}{client}",
                    host.trim_end_matches('/')
                );
                Call::get(url).header("Metadata", "true".to_owned())
            }
        };
        self.exchange(&self.probe, call).await.ok()
    }

    /// `az account get-access-token`, `None` when the CLI is missing or
    /// not logged in.
    async fn azure_cli(&self) -> Option<Expiring<Secret<String>>> {
        let az = if cfg!(windows) { "az.cmd" } else { "az" };
        let argv: Vec<String> = [
            az,
            "account",
            "get-access-token",
            "--resource",
            RESOURCE,
            "--output",
            "json",
        ]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
        let output = process::run(&self.ctx, &argv, None).await.ok()?;
        if !output.success {
            return None;
        }
        let body: Value = serde_json::from_slice(&output.stdout).ok()?;
        let token = body.get("accessToken").and_then(Value::as_str)?;
        let header = Secret::new("azure_token", format!("Bearer {token}"));
        let lifetime = body
            .get("expires_on")
            .and_then(Value::as_u64)
            .and_then(seconds_until);
        Some(Expiring::new(header, lifetime))
    }
}

/// The time left until the Unix time `at`.
fn seconds_until(at: u64) -> Option<std::time::Duration> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    at.checked_sub(now).map(std::time::Duration::from_secs)
}
