//! Application Default Credentials, in the order the Google client
//! libraries use: the JSON file named by `GOOGLE_APPLICATION_CREDENTIALS`,
//! the file `gcloud auth application-default login` writes, then the
//! metadata server of a Google Cloud machine.
//!
//! Supported files are `service_account` keys (a JWT signed with the key,
//! exchanged for a token) and `authorized_user` credentials (a refresh
//! token). Workload identity federation files are refused.

use std::path::PathBuf;

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::ProviderError;
use serde_json::{Value, json};

use crate::cloud::{Expiring, METADATA_TIMEOUT, bearer};
use crate::context::Context;
use crate::rest::{Call, Rest};

pub const KIND: &str = "gcp-secret-manager";

/// The variable naming a credentials file.
pub const CREDENTIALS_VAR: &str = "GOOGLE_APPLICATION_CREDENTIALS";

const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const METADATA_HOST: &str = "169.254.169.254";

/// A credentials file, parsed.
pub struct CredentialsFile {
    json: Value,
}

impl CredentialsFile {
    /// The project a service account key belongs to, or a user's quota
    /// project.
    pub fn project(&self) -> Option<String> {
        ["project_id", "quota_project_id"]
            .iter()
            .find_map(|k| self.json.get(*k).and_then(Value::as_str))
            .map(str::to_owned)
    }

    fn field(&self, name: &str) -> Result<&str, ProviderError> {
        self.json
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| config(format!("the credentials file has no '{name}'")))
    }

    fn token_uri(&self) -> &str {
        self.json
            .get("token_uri")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_TOKEN_URI)
    }
}

fn config(message: String) -> ProviderError {
    ProviderError::Config(format!("{KIND}: {message}"))
}

/// The credentials file of the chain, if there is one.
pub fn credentials_file(ctx: &Context) -> Result<Option<CredentialsFile>, ProviderError> {
    let (path, explicit) = match ctx.var(CREDENTIALS_VAR) {
        Some(path) => (PathBuf::from(path), true),
        None => match well_known_file(ctx) {
            Some(path) => (path, false),
            None => return Ok(None),
        },
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) if !explicit => return Ok(None),
        Err(e) => return Err(config(format!("cannot read {CREDENTIALS_VAR} file: {e}"))),
    };
    let json = serde_json::from_str(&text)
        .map_err(|_| config(format!("{} is not a credentials file", path.display())))?;
    Ok(Some(CredentialsFile { json }))
}

/// `application_default_credentials.json` in the gcloud config directory.
fn well_known_file(ctx: &Context) -> Option<PathBuf> {
    let dir = match ctx.var("CLOUDSDK_CONFIG") {
        Some(dir) => PathBuf::from(dir),
        None if cfg!(windows) => PathBuf::from(ctx.var("APPDATA")?).join("gcloud"),
        None => ctx.home()?.join(".config").join("gcloud"),
    };
    Some(dir.join("application_default_credentials.json"))
}

/// Obtains access tokens from the chain.
pub struct Adc {
    ctx: Context,
    rest: Rest,
    probe: Rest,
}

impl Adc {
    pub fn new(ctx: Context, timeout: std::time::Duration) -> Self {
        Self {
            ctx,
            rest: Rest::new(timeout),
            probe: Rest::exactly(METADATA_TIMEOUT),
        }
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// The client calls to Google APIs use.
    pub fn rest(&self) -> &Rest {
        &self.rest
    }

    /// A new access token, as an `Authorization` header value.
    pub async fn token(&self) -> Result<Expiring<Secret<String>>, ProviderError> {
        if let Some(file) = credentials_file(&self.ctx)? {
            return self.file_token(&file).await;
        }
        if let Some(token) = self.metadata_token().await {
            return Ok(token);
        }
        Err(ProviderError::Auth(format!(
            "{KIND}: no credentials found (Application Default Credentials)"
        )))
    }

    async fn file_token(
        &self,
        file: &CredentialsFile,
    ) -> Result<Expiring<Secret<String>>, ProviderError> {
        let call = match file.field("type")? {
            "service_account" => self.service_account(file)?,
            "authorized_user" => authorized_user(file)?,
            other => {
                return Err(config(format!(
                    "credentials of type '{other}' are not supported (use a service account key or 'gcloud auth application-default login')"
                )));
            }
        };
        let reply = self
            .rest
            .send(call)
            .await
            .map_err(|e| ProviderError::Unavailable(format!("{KIND}: {e}")))?;
        bearer(KIND, &reply)
    }

    /// A JWT signed with the service account's key, for its token.
    fn service_account(&self, file: &CredentialsFile) -> Result<Call<'static>, ProviderError> {
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(file.field("private_key")?.as_bytes())
            .map_err(|_| config("the service account private key is not a PEM RSA key".into()))?;
        let token_uri = file.token_uri();
        let now = now();
        let claims = json!({
            "iss": file.field("client_email")?,
            "scope": SCOPE,
            "aud": token_uri,
            "iat": now,
            "exp": now + 3600,
        });
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        let assertion = jsonwebtoken::encode(&header, &claims, &key)
            .map_err(|_| config("cannot sign with the service account key".into()))?;
        let fields = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", assertion.as_str()),
        ];
        let secrets = vec![Secret::new("gcp_assertion", assertion.clone())];
        Ok(Call::form(token_uri.to_owned(), &fields, secrets))
    }

    /// The token of the metadata server, `None` off Google Cloud.
    async fn metadata_token(&self) -> Option<Expiring<Secret<String>>> {
        let host = self
            .ctx
            .var("GCE_METADATA_HOST")
            .unwrap_or_else(|| METADATA_HOST.to_owned());
        let url =
            format!("http://{host}/computeMetadata/v1/instance/service-accounts/default/token");
        let call = Call::get(url).header("Metadata-Flavor", "Google".to_owned());
        let reply = self.probe.send(call).await.ok()?;
        bearer(KIND, &reply).ok()
    }
}

/// The refresh token of `gcloud auth application-default login`.
fn authorized_user(file: &CredentialsFile) -> Result<Call<'static>, ProviderError> {
    let client_secret = file.field("client_secret")?;
    let refresh_token = file.field("refresh_token")?;
    let fields = [
        ("grant_type", "refresh_token"),
        ("client_id", file.field("client_id")?),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
    ];
    let secrets = vec![
        Secret::new("gcp_client_secret", client_secret.to_owned()),
        Secret::new("gcp_refresh_token", refresh_token.to_owned()),
    ];
    Ok(Call::form(file.token_uri().to_owned(), &fields, secrets))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
