//! The `env`, `keychain` and `cmd` providers, and the stand-in for a type
//! compiled out of this build.

use std::sync::Arc;

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::{FetchFuture, LoginFuture, ProviderError, SecretProvider};
use rustman_core::secrets::{BACKEND_VAR, SecretBackend, SecretStore};

use crate::context::Context;
use crate::process::{self, RunError};

/// `env`: the process environment variable named by the path.
pub struct EnvProvider {
    pub ctx: Context,
}

impl SecretProvider for EnvProvider {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            let value = self.ctx.var(path).ok_or_else(|| {
                ProviderError::NotFound(format!("env: variable '{path}' is not set"))
            })?;
            Ok(Secret::new(path, value))
        })
    }
}

/// `keychain`: what `rustman secret set` stored, at `<env>/<name>`.
pub struct KeychainProvider {
    store: Arc<SecretStore>,
}

impl KeychainProvider {
    /// The store of the workspace in `ctx`, with the backend
    /// `RUSTMAN_SECRET_STORE` selects.
    pub fn new(ctx: &Context) -> Result<Self, ProviderError> {
        let backend = SecretBackend::from_env_value(ctx.var(BACKEND_VAR).as_deref())
            .map_err(|e| ProviderError::Config(format!("keychain: {e}")))?;
        Ok(Self::with_store(Arc::new(SecretStore::open(
            ctx.root(),
            backend,
        ))))
    }

    pub fn with_store(store: Arc<SecretStore>) -> Self {
        Self { store }
    }
}

impl SecretProvider for KeychainProvider {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            let Some((env, name)) = path.split_once('/') else {
                return Err(ProviderError::Config(format!(
                    "keychain: path '{path}' must be <env>/<name>"
                )));
            };
            let store = Arc::clone(&self.store);
            let (env_owned, name_owned) = (env.to_owned(), name.to_owned());
            let read = tokio::task::spawn_blocking(move || store.get(&env_owned, &name_owned));
            let found = read
                .await
                .map_err(|e| ProviderError::Unavailable(format!("keychain: {e}")))?
                .map_err(|e| ProviderError::Unavailable(format!("keychain: {e}")))?;
            found.ok_or_else(|| {
                ProviderError::NotFound(format!(
                    "keychain: no secret {path} (run 'rustman secret set {name} --env {env}')"
                ))
            })
        })
    }
}

/// `cmd` (user config only): runs `command` plus the path, with no shell,
/// and uses its trimmed stdout.
pub struct CmdProvider {
    pub name: String,
    pub command: Vec<String>,
    pub ctx: Context,
}

impl SecretProvider for CmdProvider {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            let name = &self.name;
            // The path comes from the workspace: it must not pass an option
            // to the user's command.
            if path.starts_with('-') {
                return Err(ProviderError::Config(format!(
                    "{name}: path '{path}' must not start with '-'"
                )));
            }
            let mut argv = self.command.clone();
            argv.push(path.to_owned());
            let output = process::run_group(&self.ctx, &argv)
                .await
                .map_err(|e| match e {
                    RunError::NotFound => ProviderError::Config(format!(
                        "{name}: command '{}' not found",
                        self.command[0]
                    )),
                    RunError::Failed(e) => ProviderError::Unavailable(format!("{name}: {e}")),
                })?;
            if !output.success {
                let status = output.code.map_or("a signal".to_owned(), |c| c.to_string());
                return Err(ProviderError::Unavailable(format!(
                    "{name}: command failed (exit status {status})"
                )));
            }
            let value = process::trimmed(&output.stdout).map_err(|_| {
                ProviderError::Unavailable(format!("{name}: command output is not UTF-8"))
            })?;
            if value.is_empty() {
                return Err(ProviderError::NotFound(format!(
                    "{name}: command printed nothing for {path}"
                )));
            }
            Ok(Secret::new(path, value))
        })
    }
}

/// A type this build was compiled without.
pub struct NotBuilt {
    pub kind: String,
    pub feature: &'static str,
}

impl SecretProvider for NotBuilt {
    fn fetch<'a>(&'a self, _path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move { Err(ProviderError::not_built(&self.kind, self.feature)) })
    }

    fn check<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async move { Err(ProviderError::not_built(&self.kind, self.feature)) })
    }
}
