//! Test hooks, compiled only with the `record-test-hooks` feature, so
//! integration tests can run `rustman record --intercept` and the `ca`
//! commands on every CI platform (Windows included, where a test cannot
//! send Ctrl+C to a child) without touching the real keychain. Only this
//! crate's test builds enable the feature (a dev-dependency on itself);
//! any other build (`cargo build`, `cargo run`, `cargo install`, debug or
//! release) has no hook at all and ignores the variables, so the CA key
//! never falls back to a file (design D3).
//!
//! - `RUSTMAN_RECORD_TEST_KEYCHAIN`: a stand-in keychain, each entry a
//!   file readable by the user only in that directory.
//! - `RUSTMAN_RECORD_TEST_STOP_AFTER`: stop recording, as Ctrl+C does,
//!   after that many captures.

use rustman_core::secrets::Keychain;

/// The capture count after which recording stops, if a test sets one.
#[cfg(feature = "record-test-hooks")]
pub fn stop_after() -> Option<u64> {
    std::env::var(hooks::STOP_AFTER_VAR).ok()?.parse().ok()
}

/// Without the test hooks, recording stops on Ctrl+C only.
#[cfg(not(feature = "record-test-hooks"))]
pub fn stop_after() -> Option<u64> {
    None
}

/// The stand-in keychain a test points at, if any.
#[cfg(feature = "record-test-hooks")]
pub fn keychain() -> Option<Box<dyn Keychain>> {
    let dir = std::env::var_os(hooks::VAR)?;
    Some(Box::new(hooks::DirKeychain { dir: dir.into() }))
}

/// Without the test hooks, the CA key lives in the OS keychain only.
#[cfg(not(feature = "record-test-hooks"))]
pub fn keychain() -> Option<Box<dyn Keychain>> {
    None
}

#[cfg(feature = "record-test-hooks")]
mod hooks {
    use std::path::PathBuf;

    use rustman_core::secrets::{Keychain, KeychainError};

    /// The directory holding the stand-in keychain's entries.
    pub const VAR: &str = "RUSTMAN_RECORD_TEST_KEYCHAIN";

    /// The number of captures after which recording stops.
    pub const STOP_AFTER_VAR: &str = "RUSTMAN_RECORD_TEST_STOP_AFTER";

    pub struct DirKeychain {
        pub dir: PathBuf,
    }

    impl DirKeychain {
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
}
