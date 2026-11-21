//! Test hooks, in debug builds only, so integration tests can run
//! `rustman record --intercept` and the `ca` commands on every CI platform
//! (Windows included, where a test cannot send Ctrl+C to a child) without
//! touching the real keychain. They are inert in release builds: there
//! the CA key never falls back to a file (design D3).
//!
//! - [`VAR`]: a stand-in keychain, each entry a file readable by the user
//!   only in that directory.
//! - [`STOP_AFTER_VAR`]: stop recording, as Ctrl+C does, after that many
//!   captures.

use std::path::PathBuf;

use rustman_core::secrets::{Keychain, KeychainError};

/// The directory holding the stand-in keychain's entries.
pub const VAR: &str = "RUSTMAN_RECORD_TEST_KEYCHAIN";

/// The number of captures after which recording stops.
pub const STOP_AFTER_VAR: &str = "RUSTMAN_RECORD_TEST_STOP_AFTER";

/// The capture count [`STOP_AFTER_VAR`] sets, if any (debug builds only).
pub fn stop_after() -> Option<u64> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(STOP_AFTER_VAR).ok()?.parse().ok()
}

pub struct DirKeychain {
    dir: PathBuf,
}

impl DirKeychain {
    /// The stand-in keychain [`VAR`] points at, if set (debug builds only).
    pub fn from_env() -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        std::env::var_os(VAR).map(|dir| Self { dir: dir.into() })
    }

    fn path(&self, account: &str) -> PathBuf {
        self.dir.join(account.replace(['/', '\\', ':'], "_"))
    }
}

fn failed(err: &std::io::Error) -> KeychainError {
    KeychainError::Failed(err.to_string())
}

impl Keychain for DirKeychain {
    fn get(&self, account: &str) -> Result<Option<String>, KeychainError> {
        match std::fs::read_to_string(self.path(account)) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(failed(&e)),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), KeychainError> {
        rustman_core::secrets::write_private(&self.path(account), value.as_bytes())
            .map_err(|e| failed(&e))
    }

    fn delete(&self, account: &str) -> Result<bool, KeychainError> {
        match std::fs::remove_file(self.path(account)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(failed(&e)),
        }
    }
}
