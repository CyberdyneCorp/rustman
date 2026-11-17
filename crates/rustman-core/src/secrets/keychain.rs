//! OS keychains: macOS Keychain, Windows Credential Manager and the Linux
//! Secret Service. Every backend is pure Rust and needs no system library.

/// The keychain service every Rustman secret is stored under.
pub const SERVICE: &str = "rustman";

/// Why a keychain operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeychainError {
    /// No usable keychain (no Secret Service running, keychain locked or
    /// inaccessible). Callers fall back to the file store.
    Unavailable(String),
    /// The keychain answered with an error.
    Failed(String),
}

/// A credential store holding one value per account under [`SERVICE`].
pub trait Keychain: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, KeychainError>;
    fn set(&self, account: &str, value: &str) -> Result<(), KeychainError>;
    /// Returns whether an entry was deleted.
    fn delete(&self, account: &str) -> Result<bool, KeychainError>;
}

/// The keychain of this OS, or `None` where there is none.
pub fn os_keychain() -> Option<Box<dyn Keychain>> {
    platform::keychain()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod platform {
    use std::sync::Arc;

    use keyring_core::{CredentialStore, Entry, Error};

    use super::{Keychain, KeychainError, SERVICE};

    pub fn keychain() -> Option<Box<dyn Keychain>> {
        Some(Box::new(CoreKeychain { store: store() }))
    }

    #[cfg(target_os = "macos")]
    fn store() -> Result<Arc<CredentialStore>, KeychainError> {
        apple_native_keyring_store::keychain::Store::new()
            .map(|s| s as Arc<CredentialStore>)
            .map_err(classify)
    }

    #[cfg(target_os = "windows")]
    fn store() -> Result<Arc<CredentialStore>, KeychainError> {
        windows_native_keyring_store::Store::new()
            .map(|s| s as Arc<CredentialStore>)
            .map_err(classify)
    }

    /// A keyring-core credential store.
    struct CoreKeychain {
        store: Result<Arc<CredentialStore>, KeychainError>,
    }

    impl CoreKeychain {
        fn entry(&self, account: &str) -> Result<Entry, KeychainError> {
            let store = self.store.as_ref().map_err(Clone::clone)?;
            store.build(SERVICE, account, None).map_err(classify)
        }
    }

    impl Keychain for CoreKeychain {
        fn get(&self, account: &str) -> Result<Option<String>, KeychainError> {
            match self.entry(account)?.get_password() {
                Ok(value) => Ok(Some(value)),
                Err(Error::NoEntry) => Ok(None),
                Err(e) => Err(classify(e)),
            }
        }

        fn set(&self, account: &str, value: &str) -> Result<(), KeychainError> {
            self.entry(account)?.set_password(value).map_err(classify)
        }

        fn delete(&self, account: &str) -> Result<bool, KeychainError> {
            match self.entry(account)?.delete_credential() {
                Ok(()) => Ok(true),
                Err(Error::NoEntry) => Ok(false),
                Err(e) => Err(classify(e)),
            }
        }
    }

    fn classify(err: Error) -> KeychainError {
        match err {
            Error::PlatformFailure(_) | Error::NoStorageAccess(_) | Error::NoDefaultStore => {
                KeychainError::Unavailable(err.to_string())
            }
            other => KeychainError::Failed(other.to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use futures_lite::future::block_on;
    use oo7::Keyring;

    use super::{Keychain, KeychainError, SERVICE};

    pub fn keychain() -> Option<Box<dyn Keychain>> {
        Some(Box::new(SecretService))
    }

    /// The freedesktop Secret Service over D-Bus, with attributes compatible
    /// with other keyring clients (`service`, `username`).
    struct SecretService;

    fn attributes(account: &str) -> [(&'static str, &str); 2] {
        [("service", SERVICE), ("username", account)]
    }

    /// Any D-Bus or Secret Service error means the keychain cannot be used
    /// (no session bus, no service, a locked collection on a headless host).
    fn unavailable(err: oo7::Error) -> KeychainError {
        KeychainError::Unavailable(err.to_string())
    }

    async fn open() -> Result<Keyring, KeychainError> {
        let keyring = Keyring::host().await.map_err(unavailable)?;
        keyring.unlock().await.map_err(unavailable)?;
        Ok(keyring)
    }

    impl Keychain for SecretService {
        fn get(&self, account: &str) -> Result<Option<String>, KeychainError> {
            block_on(async {
                let keyring = open().await?;
                let items = keyring
                    .search_items(&attributes(account))
                    .await
                    .map_err(unavailable)?;
                let Some(item) = items.first() else {
                    return Ok(None);
                };
                item.unlock().await.map_err(unavailable)?;
                let secret = item.secret().await.map_err(unavailable)?;
                String::from_utf8(secret.to_vec())
                    .map(Some)
                    .map_err(|_| KeychainError::Failed("stored secret is not UTF-8".to_owned()))
            })
        }

        fn set(&self, account: &str, value: &str) -> Result<(), KeychainError> {
            block_on(async {
                let keyring = open().await?;
                let label = format!("{SERVICE}: {account}");
                keyring
                    .create_item(&label, &attributes(account), value, true)
                    .await
                    .map_err(unavailable)
            })
        }

        fn delete(&self, account: &str) -> Result<bool, KeychainError> {
            block_on(async {
                let keyring = open().await?;
                let attributes = attributes(account);
                let found = !keyring
                    .search_items(&attributes)
                    .await
                    .map_err(unavailable)?
                    .is_empty();
                keyring.delete(&attributes).await.map_err(unavailable)?;
                Ok(found)
            })
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    use super::Keychain;

    pub fn keychain() -> Option<Box<dyn Keychain>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run manually on macOS, Windows and a Linux desktop:
    /// `cargo test -p rustman-core -- --ignored os_keychain`.
    #[test]
    #[ignore = "touches the real OS keychain"]
    fn os_keychain_round_trip() {
        let keychain = os_keychain().expect("this OS has a keychain");
        let account = format!("rustman-test-{}/prod/api_token", uuid::Uuid::new_v4());
        assert_eq!(keychain.get(&account), Ok(None));
        keychain.set(&account, "s3cr3t").unwrap();
        assert_eq!(keychain.get(&account), Ok(Some("s3cr3t".to_owned())));
        keychain.set(&account, "rotated").unwrap();
        assert_eq!(keychain.get(&account), Ok(Some("rotated".to_owned())));
        assert_eq!(keychain.delete(&account), Ok(true));
        assert_eq!(keychain.delete(&account), Ok(false));
        assert_eq!(keychain.get(&account), Ok(None));
    }
}
