//! Secret storage (design D6, environments spec "Secret storage").
//!
//! Values of variables listed under an environment's `secrets` live outside
//! the workspace files: in the OS keychain (service `rustman`, account
//! `<workspace-id>/<env>/<name>`), or, when no keychain is available, in the
//! git-ignored `.rustman/secrets.yaml`, readable by the current user only.

mod file;
mod git;
pub mod keychain;
mod private_file;
pub mod workspace_id;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub use file::{FileSecretStore, SECRETS_FILE};
/// Guards and writer for any private state under `.rustman/` (cookie jars,
/// spilled bodies), shared with the secrets file.
pub use git::ensure_private;
pub use keychain::{Keychain, KeychainError, SERVICE};
pub use private_file::{
    create_private_dir, ensure_private_dir, user_temp_dir, write as write_private,
};

use crate::error::LoadError;
use crate::secret::Secret;
use crate::vars::SecretLookup;

/// The folder, under the workspace root, holding per-user state.
pub const STATE_DIR: &str = ".rustman";

/// The OS environment variable that picks the secret backend.
pub const BACKEND_VAR: &str = "RUSTMAN_SECRET_STORE";

/// The warning printed, once, the first time a secret falls back to the file.
pub const FALLBACK_WARNING: &str =
    "OS keychain unavailable; storing secrets in .rustman/secrets.yaml";

/// The warning printed, once, when a secret is read from a secrets file that
/// other users can access.
pub const EXPOSED_WARNING: &str = ".rustman/secrets.yaml is readable by other users; \
     run 'chmod 600 .rustman/secrets.yaml' (the next 'rustman secret set' also fixes it)";

/// Where secrets are stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SecretBackend {
    /// The OS keychain, falling back to `.rustman/secrets.yaml` when it is
    /// unavailable.
    #[default]
    Keychain,
    /// `.rustman/secrets.yaml` only, without touching the keychain.
    File,
}

impl SecretBackend {
    /// The backend named by `RUSTMAN_SECRET_STORE`; unset or empty means the
    /// keychain.
    pub fn from_env_value(value: Option<&str>) -> Result<Self, SecretError> {
        match value.unwrap_or_default() {
            "" | "keychain" => Ok(Self::Keychain),
            "file" => Ok(Self::File),
            other => Err(SecretError::InvalidBackend(other.to_owned())),
        }
    }
}

/// Where a secret was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredIn {
    Keychain,
    File,
}

/// Why a secret could not be read or written. Messages never contain values.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("refusing to write secrets: .rustman/ is not git-ignored")]
    NotIgnored,
    #[error("refusing to write secrets: .rustman {0}")]
    UnsafeStateDir(String),
    #[error("refusing to write secrets: could not check that .rustman/ is git-ignored")]
    GitCheckFailed,
    #[error("OS keychain error: {0}")]
    Keychain(String),
    #[error("invalid {BACKEND_VAR} '{0}': expected 'keychain' or 'file'")]
    InvalidBackend(String),
    #[error(
        "'{name}' is not a secret of environment '{env}' (list it under 'secrets' in environments/{env}.yaml)"
    )]
    NotDeclared { env: String, name: String },
    #[error("secret value is empty; nothing was stored")]
    EmptyValue,
    #[error(transparent)]
    Invalid(#[from] LoadError),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl SecretError {
    pub(crate) fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// The secrets of one workspace: the keychain first, then the file fallback.
pub struct SecretStore {
    root: PathBuf,
    keychain: Option<Box<dyn Keychain>>,
    file: FileSecretStore,
    keychain_down: AtomicBool,
    warned: AtomicBool,
    warned_exposed: AtomicBool,
    warnings: Mutex<Vec<String>>,
}

impl SecretStore {
    /// The store for the workspace at `root`, using the OS keychain unless
    /// `backend` is [`SecretBackend::File`].
    pub fn open(root: &Path, backend: SecretBackend) -> Self {
        let keychain = match backend {
            SecretBackend::Keychain => keychain::os_keychain(),
            SecretBackend::File => None,
        };
        Self::with_keychain(root, keychain)
    }

    /// The store for `root` with an explicit keychain (`None`: file only).
    pub fn with_keychain(root: &Path, keychain: Option<Box<dyn Keychain>>) -> Self {
        Self {
            root: root.to_path_buf(),
            keychain,
            file: FileSecretStore::new(root),
            keychain_down: AtomicBool::new(false),
            warned: AtomicBool::new(false),
            warned_exposed: AtomicBool::new(false),
            warnings: Mutex::new(Vec::new()),
        }
    }

    /// The stored value of secret `name` in environment `env`.
    pub fn get(&self, env: &str, name: &str) -> Result<Option<Secret<String>>, SecretError> {
        if let Some(value) = self.keychain_get(env, name)? {
            return Ok(Some(Secret::new(name, value)));
        }
        let value = self.file.get(env, name)?;
        if value.is_some() && self.file.is_exposed() {
            self.warn_exposed_once();
        }
        Ok(value.map(|v| Secret::new(name, v)))
    }

    /// Store `secret` for environment `env`.
    pub fn set(&self, env: &str, secret: &Secret<String>) -> Result<StoredIn, SecretError> {
        if secret.expose().is_empty() {
            return Err(SecretError::EmptyValue);
        }
        if let Some(keychain) = self.keychain() {
            let account = account(
                &workspace_id::load_or_create(&self.root)?,
                env,
                secret.name(),
            );
            match keychain.set(&account, secret.expose()) {
                Ok(()) => {
                    self.file.remove(env, secret.name())?;
                    return Ok(StoredIn::Keychain);
                }
                Err(KeychainError::Unavailable(_)) => {
                    self.keychain_down.store(true, Ordering::Relaxed);
                }
                Err(KeychainError::Failed(msg)) => return Err(SecretError::Keychain(msg)),
            }
        }
        if self.keychain.is_some() {
            self.warn_fallback_once();
        }
        self.file.set(env, secret.name(), secret.expose())?;
        Ok(StoredIn::File)
    }

    /// Store many secrets at once, `(environment, secret)`: in the keychain
    /// one by one, and in the file fallback with a single read and write,
    /// so storing thousands of secrets stays linear.
    pub fn set_many(&self, secrets: &[(&str, &Secret<String>)]) -> Result<(), SecretError> {
        if secrets.iter().any(|(_, s)| s.expose().is_empty()) {
            return Err(SecretError::EmptyValue);
        }
        let mut in_keychain: Vec<(&str, &str)> = Vec::new();
        let mut to_file: Vec<(&str, &str, &str)> = Vec::new();
        let mut id: Option<String> = None;
        for (env, secret) in secrets {
            if let Some(keychain) = self.keychain() {
                if id.is_none() {
                    id = Some(workspace_id::load_or_create(&self.root)?);
                }
                let id = id.as_deref().unwrap_or_default();
                match keychain.set(&account(id, env, secret.name()), secret.expose()) {
                    Ok(()) => {
                        in_keychain.push((env, secret.name()));
                        continue;
                    }
                    Err(KeychainError::Unavailable(_)) => {
                        self.keychain_down.store(true, Ordering::Relaxed);
                    }
                    Err(KeychainError::Failed(msg)) => return Err(SecretError::Keychain(msg)),
                }
            }
            to_file.push((env, secret.name(), secret.expose()));
        }
        self.file.remove_many(&in_keychain)?;
        if !to_file.is_empty() && self.keychain.is_some() {
            self.warn_fallback_once();
        }
        self.file.set_many(&to_file)
    }

    /// Delete secret `name` of environment `env` wherever it is stored.
    /// Returns whether anything was deleted.
    pub fn remove(&self, env: &str, name: &str) -> Result<bool, SecretError> {
        let in_keychain = self.keychain_remove(env, name)?;
        let in_file = self.file.remove(env, name)?;
        Ok(in_keychain || in_file)
    }

    /// Warnings collected since the last call, such as the one-time
    /// keychain-fallback warning.
    pub fn take_warnings(&self) -> Vec<String> {
        let mut warnings = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *warnings)
    }

    fn keychain(&self) -> Option<&dyn Keychain> {
        if self.keychain_down.load(Ordering::Relaxed) {
            return None;
        }
        self.keychain.as_deref()
    }

    /// The account of `env`/`name`, or `None` when nothing can be in the
    /// keychain (no keychain, or no workspace id yet).
    fn existing_account(&self, env: &str, name: &str) -> Result<Option<String>, SecretError> {
        if self.keychain().is_none() {
            return Ok(None);
        }
        Ok(workspace_id::load(&self.root)?.map(|id| account(&id, env, name)))
    }

    fn keychain_get(&self, env: &str, name: &str) -> Result<Option<String>, SecretError> {
        let (Some(keychain), Some(account)) = (self.keychain(), self.existing_account(env, name)?)
        else {
            return Ok(None);
        };
        Ok(self.down_is_none(keychain.get(&account))?.flatten())
    }

    fn keychain_remove(&self, env: &str, name: &str) -> Result<bool, SecretError> {
        let (Some(keychain), Some(account)) = (self.keychain(), self.existing_account(env, name)?)
        else {
            return Ok(false);
        };
        Ok(self
            .down_is_none(keychain.delete(&account))?
            .unwrap_or(false))
    }

    /// An unavailable keychain reads as "nothing stored there".
    fn down_is_none<T>(&self, result: Result<T, KeychainError>) -> Result<Option<T>, SecretError> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(KeychainError::Unavailable(_)) => {
                self.keychain_down.store(true, Ordering::Relaxed);
                Ok(None)
            }
            Err(KeychainError::Failed(msg)) => Err(SecretError::Keychain(msg)),
        }
    }

    fn warn_fallback_once(&self) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            self.warn(FALLBACK_WARNING.to_owned());
        }
    }

    fn warn_exposed_once(&self) {
        if !self.warned_exposed.swap(true, Ordering::Relaxed) {
            self.warn(EXPOSED_WARNING.to_owned());
        }
    }

    fn warn(&self, warning: String) {
        let mut warnings = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        warnings.push(warning);
    }
}

impl SecretLookup for SecretStore {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        self.get(env, name)
            .map(|secret| secret.map(|s| s.expose().clone()))
            .map_err(|err| err.to_string())
    }
}

/// The keychain account of secret `name` in environment `env`.
pub fn account(workspace_id: &str, env: &str, name: &str) -> String {
    format!("{workspace_id}/{env}/{name}")
}

#[cfg(test)]
mod tests;
