//! `vault`: KV v1 and v2 secrets from HashiCorp Vault over its HTTP API
//! (design D3), with token, AppRole or OIDC auth.

mod auth;

use std::collections::HashMap;
use std::time::Duration;

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::{FetchFuture, LoginFuture, ProviderError, SecretProvider};
use serde_json::Value;
use tokio::sync::Mutex;

pub use auth::{AuthMethod, ROLE_ID_VAR, SECRET_ID_VAR, TOKEN_VAR};

use crate::context::Context;
use crate::rest::{Call, Reply, Rest};

/// The environment variable with the server address, when `address` is unset.
pub const ADDR_VAR: &str = "VAULT_ADDR";

/// How a mount stores secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Mount {
    /// With a trailing `/`, such as `kv/`.
    path: String,
    v2: bool,
}

pub struct Vault {
    name: String,
    /// `None` when neither `address` nor `VAULT_ADDR` is set: every fetch
    /// then fails, so a workspace that names Vault still loads without it.
    address: Option<String>,
    namespace: Option<String>,
    auth: AuthMethod,
    /// Whether the user chose `address` (user config or `VAULT_ADDR`), so
    /// their own token and AppRole ids may be sent there.
    trusted: bool,
    ctx: Context,
    rest: Rest,
    /// The token of an AppRole or OIDC login, for the life of the process.
    token: Mutex<Option<Secret<String>>>,
    /// KV versions per mount, detected once.
    mounts: Mutex<HashMap<String, Mount>>,
}

/// A secret path with its `?version=<n>`.
struct Target<'a> {
    path: &'a str,
    version: Option<u64>,
}

impl Vault {
    pub fn new(
        name: &str,
        address: Option<&str>,
        namespace: Option<&str>,
        auth: AuthMethod,
        ctx: Context,
        timeout: Duration,
    ) -> Self {
        let address = address.map(str::to_owned).or_else(|| ctx.var(ADDR_VAR));
        Self {
            name: name.to_owned(),
            address: address.map(|a| a.trim_end_matches('/').to_owned()),
            namespace: namespace.map(str::to_owned),
            auth,
            trusted: true,
            ctx,
            rest: Rest::new(timeout),
            token: Mutex::new(None),
            mounts: Mutex::new(HashMap::new()),
        }
    }

    /// Mark `address` as chosen by the workspace: the user's own token or
    /// AppRole ids are then never sent to it (an OIDC login still works,
    /// since its token is issued by that server).
    #[must_use]
    pub fn untrusted_address(mut self) -> Self {
        self.trusted = false;
        self
    }

    /// The provider name used in messages.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The server address, checked when a secret is fetched.
    pub(super) fn address(&self) -> Result<&str, ProviderError> {
        self.address.as_deref().ok_or_else(|| {
            ProviderError::Config(format!("vault: no address (set 'address' or {ADDR_VAR})"))
        })
    }

    async fn read(&self, path: &str) -> Result<String, ProviderError> {
        let target = parse_target(path)?;
        let token = self.token().await?;
        let mount = self.mount(&token, target.path).await?;
        let url = match (&mount, target.version) {
            (Some(m), version) if m.v2 => {
                let rest = target.path.strip_prefix(&m.path).unwrap_or(target.path);
                let query = version.map_or(String::new(), |v| format!("?version={v}"));
                format!("{}/v1/{}data/{rest}{query}", self.address()?, m.path)
            }
            (_, Some(_)) => {
                return Err(ProviderError::Config(format!(
                    "vault: ?version needs a KV v2 mount ({})",
                    target.path
                )));
            }
            _ => format!("{}/v1/{}", self.address()?, target.path),
        };
        let reply = self.send(self.call(Call::get(url), &token)).await?;
        let body = self.check(&reply, target.path)?;
        let data = body.get("data");
        let secret = match &mount {
            Some(m) if m.v2 => data.and_then(|d| d.get("data")),
            _ => data,
        };
        let secret = secret.filter(|s| !s.is_null()).ok_or_else(|| {
            ProviderError::NotFound(format!("vault: secret {} not found", target.path))
        })?;
        Ok(secret.to_string())
    }

    /// The KV mount `path` is under, detected once per mount; `None` when
    /// it cannot be told (read as KV v1).
    async fn mount(
        &self,
        token: &Secret<String>,
        path: &str,
    ) -> Result<Option<Mount>, ProviderError> {
        let first = path.split('/').next().unwrap_or_default().to_owned();
        if let Some(mount) = self.mounts.lock().await.get(&first) {
            return Ok(Some(mount.clone()));
        }
        let url = format!("{}/v1/sys/internal/ui/mounts/{path}", self.address()?);
        let reply = self.send(self.call(Call::get(url), token)).await?;
        if !(200..300).contains(&reply.status) {
            return Ok(None);
        }
        let data = reply.json().and_then(|j| j.get("data").cloned());
        let Some(mount_path) = data
            .as_ref()
            .and_then(|d| d.get("path"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let version = data
            .as_ref()
            .and_then(|d| d.pointer("/options/version"))
            .and_then(Value::as_str);
        let mount = Mount {
            path: mount_path.to_owned(),
            v2: version == Some("2"),
        };
        let key = mount_path.trim_end_matches('/').to_owned();
        self.mounts.lock().await.insert(key, mount.clone());
        Ok(Some(mount))
    }

    fn call<'a>(&self, call: Call<'a>, token: &Secret<String>) -> Call<'a> {
        let call = call.credential("X-Vault-Token", token);
        match &self.namespace {
            Some(ns) => call.header("X-Vault-Namespace", ns.clone()),
            None => call,
        }
    }

    async fn send(&self, call: Call<'_>) -> Result<Reply, ProviderError> {
        self.rest
            .send(call)
            .await
            .map_err(|e| ProviderError::Unavailable(format!("vault: {e}")))
    }

    /// The body of a successful reply, else the error for `path`.
    fn check(&self, reply: &Reply, path: &str) -> Result<Value, ProviderError> {
        match reply.status {
            200..=299 => reply
                .json()
                .ok_or_else(|| ProviderError::Unavailable("vault: invalid response".into())),
            401 | 403 => Err(ProviderError::Auth(format!(
                "vault: permission denied for {path}"
            ))),
            404 => Err(ProviderError::NotFound(format!(
                "vault: secret {path} not found"
            ))),
            status => Err(ProviderError::Unavailable(format!(
                "vault: server returned {status} for {path}"
            ))),
        }
    }
}

impl SecretProvider for Vault {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move { Ok(Secret::new(path, self.read(path).await?)) })
    }

    fn login<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async move { self.login_if_needed().await })
    }

    /// Look the token up: an expired or revoked token fails.
    fn check<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async move {
            let token = self.token().await?;
            let url = format!("{}/v1/auth/token/lookup-self", self.address()?);
            let reply = self.send(self.call(Call::get(url), &token)).await?;
            match reply.status {
                200..=299 => Ok(()),
                401 | 403 => Err(ProviderError::Auth("vault: authentication failed".into())),
                status => Err(ProviderError::Unavailable(format!(
                    "vault: server returned {status} for the token lookup"
                ))),
            }
        })
    }
}

fn parse_target(path: &str) -> Result<Target<'_>, ProviderError> {
    let Some((path, query)) = path.split_once('?') else {
        return Ok(Target {
            path,
            version: None,
        });
    };
    let version = query
        .strip_prefix("version=")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| {
            ProviderError::Config(format!(
                "vault: invalid query '?{query}' (expected ?version=<n>)"
            ))
        })?;
    Ok(Target {
        path,
        version: Some(version),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_the_path() {
        let t = parse_target("kv/app/db?version=3").unwrap();
        assert_eq!((t.path, t.version), ("kv/app/db", Some(3)));
        let t = parse_target("kv/app/db").unwrap();
        assert_eq!((t.path, t.version), ("kv/app/db", None));
        assert!(parse_target("kv/app?v=1").is_err());
    }
}
