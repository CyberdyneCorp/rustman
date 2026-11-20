//! Vault auth: a token from `VAULT_TOKEN` or `~/.vault-token`, an AppRole
//! login with ids from the environment, or an OIDC browser login. Login
//! tokens live in memory for the session only.

use std::time::Duration;

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::ProviderError;
use rustman_http::Loopback;
use serde_json::{Value, json};

use super::Vault;
use crate::rest::Call;

pub const TOKEN_VAR: &str = "VAULT_TOKEN";
pub const ROLE_ID_VAR: &str = "VAULT_ROLE_ID";
pub const SECRET_ID_VAR: &str = "VAULT_SECRET_ID";

/// The file `vault login` writes the token to, in the home directory.
const TOKEN_FILE: &str = ".vault-token";
/// How long the browser has to complete an OIDC login.
const OIDC_LOGIN_TIMEOUT: Duration = Duration::from_secs(120);
/// The port of Vault's default OIDC redirect URI.
pub const DEFAULT_OIDC_PORT: u16 = 8250;

/// `auth` of a `vault` provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMethod {
    Token,
    AppRole {
        mount: String,
    },
    Oidc {
        mount: String,
        role: Option<String>,
        port: u16,
    },
}

impl AuthMethod {
    /// `auth` (default `token`), with the optional `mount`, `role` and
    /// `oidc_port` settings.
    pub fn parse(
        auth: Option<&str>,
        mount: Option<&str>,
        role: Option<&str>,
        port: Option<u16>,
    ) -> Result<Self, ProviderError> {
        match auth.unwrap_or("token") {
            "token" => Ok(Self::Token),
            "approle" => Ok(Self::AppRole {
                mount: mount.unwrap_or("approle").to_owned(),
            }),
            "oidc" => Ok(Self::Oidc {
                mount: mount.unwrap_or("oidc").to_owned(),
                role: role.map(str::to_owned),
                port: port.unwrap_or(DEFAULT_OIDC_PORT),
            }),
            other => Err(ProviderError::Config(format!(
                "vault: unknown auth '{other}' (expected token, approle or oidc)"
            ))),
        }
    }
}

impl Vault {
    /// The token every request carries.
    pub(super) async fn token(&self) -> Result<Secret<String>, ProviderError> {
        let address = self.address()?;
        if !self.trusted && !matches!(self.auth, AuthMethod::Oidc { .. }) {
            return Err(ProviderError::Config(format!(
                "vault: address {} is set in rustman.toml; set {} to it, or 'address' in user config, to send your Vault credentials there",
                address,
                super::ADDR_VAR
            )));
        }
        if self.auth == AuthMethod::Token {
            return self.static_token();
        }
        if let Some(token) = self.token.lock().await.clone() {
            return Ok(token);
        }
        let token = match &self.auth {
            AuthMethod::AppRole { mount } => self.approle_login(mount).await?,
            _ => return Err(ProviderError::Auth("vault: not logged in".into())),
        };
        *self.token.lock().await = Some(token.clone());
        Ok(token)
    }

    /// The OIDC browser login, once per session; nothing for other methods.
    pub(super) async fn login_if_needed(&self) -> Result<(), ProviderError> {
        let AuthMethod::Oidc { mount, role, port } = &self.auth else {
            return Ok(());
        };
        let mut held = self.token.lock().await;
        if held.is_none() {
            *held = Some(self.oidc_login(mount, role.as_deref(), *port).await?);
        }
        Ok(())
    }

    fn static_token(&self) -> Result<Secret<String>, ProviderError> {
        let from_file = || {
            let file = self.ctx.home()?.join(TOKEN_FILE);
            let text = std::fs::read_to_string(file).ok()?;
            Some(text.trim().to_owned()).filter(|t| !t.is_empty())
        };
        let token = self.ctx.var(TOKEN_VAR).or_else(from_file).ok_or_else(|| {
            ProviderError::Auth(format!(
                "vault: no token (set {TOKEN_VAR} or run 'vault login')"
            ))
        })?;
        Ok(Secret::new("vault_token", token))
    }

    async fn approle_login(&self, mount: &str) -> Result<Secret<String>, ProviderError> {
        let (Some(role_id), Some(secret_id)) =
            (self.ctx.var(ROLE_ID_VAR), self.ctx.var(SECRET_ID_VAR))
        else {
            return Err(ProviderError::Auth(format!(
                "vault: approle needs {ROLE_ID_VAR} and {SECRET_ID_VAR}"
            )));
        };
        let url = format!("{}/v1/auth/{mount}/login", self.address()?);
        let mut call = Call::post(url, json!({"role_id": role_id, "secret_id": secret_id}));
        call.secrets.push(Secret::new("vault_secret_id", secret_id));
        let reply = self.send(self.with_namespace(call)).await?;
        if !(200..300).contains(&reply.status) {
            return Err(ProviderError::Auth("vault: approle login failed".into()));
        }
        client_token(reply.json())
    }

    async fn oidc_login(
        &self,
        mount: &str,
        role: Option<&str>,
        port: u16,
    ) -> Result<Secret<String>, ProviderError> {
        let failed = |e: String| ProviderError::Auth(format!("vault: oidc login failed: {e}"));
        let loopback = Loopback::bind_on(port, "/oidc")
            .await
            .map_err(|e| failed(e.to_string()))?;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let body = json!({
            "role": role.unwrap_or_default(),
            "redirect_uri": loopback.redirect_uri(),
            "client_nonce": nonce,
        });
        let url = format!("{}/v1/auth/{mount}/oidc/auth_url", self.address()?);
        let reply = self
            .send(self.with_namespace(Call::post(url, body)))
            .await?;
        let auth_url = reply
            .json()
            .and_then(|j| {
                j.pointer("/data/auth_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|u| !u.is_empty())
            .ok_or_else(|| {
                failed(format!(
                    "no auth URL (is {} an allowed redirect URI of the role?)",
                    loopback.redirect_uri()
                ))
            })?;
        let state = query_param(&auth_url, "state").ok_or_else(|| failed("no state".into()))?;
        self.ctx
            .open_browser(&auth_url)
            .map_err(|e| failed(format!("cannot open a browser: {e}")))?;
        let code = loopback
            .wait_for_code(&state, OIDC_LOGIN_TIMEOUT)
            .await
            .map_err(|e| failed(e.to_string()))?;
        let query: String = form_urlencoded::Serializer::new(String::new())
            .append_pair("state", &state)
            .append_pair("code", &code)
            .append_pair("client_nonce", &nonce)
            .finish();
        let url = format!("{}/v1/auth/{mount}/oidc/callback?{query}", self.address()?);
        let reply = self.send(self.with_namespace(Call::get(url))).await?;
        if !(200..300).contains(&reply.status) {
            return Err(failed(format!("callback returned {}", reply.status)));
        }
        client_token(reply.json())
    }

    fn with_namespace<'a>(&self, call: Call<'a>) -> Call<'a> {
        match &self.namespace {
            Some(ns) => call.header("X-Vault-Namespace", ns.clone()),
            None => call,
        }
    }
}

fn client_token(body: Option<Value>) -> Result<Secret<String>, ProviderError> {
    body.as_ref()
        .and_then(|b| b.pointer("/auth/client_token"))
        .and_then(Value::as_str)
        .map(|t| Secret::new("vault_token", t.to_owned()))
        .ok_or_else(|| ProviderError::Auth("vault: login returned no token".into()))
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_methods_parse_with_defaults() {
        assert_eq!(
            AuthMethod::parse(None, None, None, None).unwrap(),
            AuthMethod::Token
        );
        assert_eq!(
            AuthMethod::parse(Some("approle"), None, None, None).unwrap(),
            AuthMethod::AppRole {
                mount: "approle".into()
            }
        );
        assert_eq!(
            AuthMethod::parse(Some("oidc"), None, Some("dev"), None).unwrap(),
            AuthMethod::Oidc {
                mount: "oidc".into(),
                role: Some("dev".into()),
                port: 8250
            }
        );
        assert!(AuthMethod::parse(Some("ldap"), None, None, None).is_err());
        assert_eq!(
            query_param("https://x/a?b=1&state=s%201", "state").as_deref(),
            Some("s 1")
        );
    }
}
