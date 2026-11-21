//! Where the local CA lives (design D3): the private key in the OS keychain
//! (service `rustman`, account `record-ca/<workspace-id>`), the public
//! certificate in `<config_dir>/record-ca/<workspace-id>.pem`. Nothing is
//! written under the workspace, and without a keychain there is no CA: the
//! key never falls back to a file.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rustman_core::secrets::{Keychain, KeychainError};

use super::{CaError, LocalCa};

/// The keychain account prefix of CA keys.
pub const ACCOUNT_PREFIX: &str = "record-ca/";

/// The folder, under the user config directory, holding CA certificates
/// and install records.
pub const CA_DIR: &str = "record-ca";

/// The local CA of one workspace.
pub struct CaStore {
    keychain: Option<Box<dyn Keychain>>,
    dir: PathBuf,
    id: String,
}

impl CaStore {
    /// The CA of workspace `workspace_id`, its key in `keychain` (`None`:
    /// no keychain) and its certificate under `config_dir` (the user's
    /// `rustman` config directory).
    pub fn new(keychain: Option<Box<dyn Keychain>>, config_dir: &Path, workspace_id: &str) -> Self {
        Self {
            keychain,
            dir: config_dir.join(CA_DIR),
            id: workspace_id.to_owned(),
        }
    }

    pub fn workspace_id(&self) -> &str {
        &self.id
    }

    /// The keychain account holding the private key.
    pub fn account(&self) -> String {
        format!("{ACCOUNT_PREFIX}{}", self.id)
    }

    /// The public certificate.
    pub fn cert_path(&self) -> PathBuf {
        self.dir.join(format!("{}.pem", self.id))
    }

    /// The record of the trust stores Rustman installed the CA in.
    pub fn installed_path(&self) -> PathBuf {
        self.dir.join(format!("{}.installed", self.id))
    }

    /// The CA's common name, which names the workspace it belongs to.
    pub fn common_name(&self) -> String {
        let short: String = self.id.chars().take(8).collect();
        format!("Rustman Local CA ({short})")
    }

    /// Generate the CA: the key goes to the keychain, the certificate to
    /// the config directory. Refused when a CA exists.
    pub fn init(&self) -> Result<LocalCa, CaError> {
        if let Some(pem) = self.certificate()? {
            return Err(CaError::AlreadyExists(super::info_from_pem(&pem)?.sha256));
        }
        let keychain = self.keychain()?;
        if keychain_result(keychain.get(&self.account()))?.is_some() {
            return Err(CaError::Mismatch);
        }
        let ca = LocalCa::generate(&self.common_name())?;
        keychain_result(keychain.set(&self.account(), ca.key_pem()))?;
        if let Err(err) = self.write_certificate(ca.cert_pem()) {
            let _ = keychain.delete(&self.account());
            return Err(err);
        }
        Ok(ca)
    }

    /// The CA, or `None` when it was never created.
    pub fn load(&self) -> Result<Option<LocalCa>, CaError> {
        let Some(cert) = self.certificate()? else {
            return Ok(None);
        };
        let key = keychain_result(self.keychain()?.get(&self.account()))?;
        let key = key.ok_or(CaError::Mismatch)?;
        LocalCa::from_pem(&cert, &key).map(Some)
    }

    /// The public certificate, PEM encoded, without touching the keychain.
    pub fn certificate(&self) -> Result<Option<String>, CaError> {
        let path = self.cert_path();
        match fs::read_to_string(&path) {
            Ok(pem) => Ok(Some(pem)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CaError::Io { path, source }),
        }
    }

    /// Delete the keychain entry and the certificate.
    pub fn remove(&self) -> Result<(), CaError> {
        keychain_result(self.keychain()?.delete(&self.account()))?;
        let path = self.cert_path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CaError::Io { path, source }),
        }
    }

    fn keychain(&self) -> Result<&dyn Keychain, CaError> {
        self.keychain.as_deref().ok_or(CaError::NoKeychain)
    }

    fn write_certificate(&self, pem: &str) -> Result<(), CaError> {
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |source| CaError::Io { path, source }
        };
        fs::create_dir_all(&self.dir).map_err(io(&self.dir))?;
        let path = self.cert_path();
        fs::write(&path, pem).map_err(io(&path))
    }
}

/// A keychain answer, an unavailable keychain meaning there is none.
fn keychain_result<T>(result: Result<T, KeychainError>) -> Result<T, CaError> {
    result.map_err(|e| match e {
        KeychainError::Unavailable(_) => CaError::NoKeychain,
        KeychainError::Failed(reason) => CaError::Keychain(reason),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// An in-memory keychain shared with the test that made it.
    #[derive(Clone, Default)]
    pub struct MockKeychain {
        pub entries: Arc<Mutex<HashMap<String, String>>>,
        pub unavailable: bool,
    }

    impl Keychain for MockKeychain {
        fn get(&self, account: &str) -> Result<Option<String>, KeychainError> {
            self.check()?;
            Ok(self.entries.lock().unwrap().get(account).cloned())
        }

        fn set(&self, account: &str, value: &str) -> Result<(), KeychainError> {
            self.check()?;
            let mut entries = self.entries.lock().unwrap();
            entries.insert(account.to_owned(), value.to_owned());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<bool, KeychainError> {
            self.check()?;
            Ok(self.entries.lock().unwrap().remove(account).is_some())
        }
    }

    impl MockKeychain {
        fn check(&self) -> Result<(), KeychainError> {
            if self.unavailable {
                Err(KeychainError::Unavailable("no secret service".into()))
            } else {
                Ok(())
            }
        }
    }

    /// Every file under `dir`, relative.
    pub fn files(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(next) = stack.pop() {
            for entry in fs::read_dir(&next).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path.strip_prefix(dir).unwrap().to_path_buf());
                }
            }
        }
        out.sort();
        out
    }

    pub const ID: &str = "0b5c0b9e-58c4-4c1e-9f0e-6f1f4b7f2a10";

    pub fn store(keychain: &MockKeychain, config: &Path) -> CaStore {
        CaStore::new(Some(Box::new(keychain.clone())), config, ID)
    }

    #[test]
    fn init_puts_the_key_in_the_keychain_and_nothing_in_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("rustman.toml"), "").unwrap();
        let before = files(workspace.path());
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let ca = store(&keychain, config.path()).init().unwrap();
        assert_eq!(files(workspace.path()), before);
        let entries = keychain.entries.lock().unwrap();
        let key = &entries[&format!("record-ca/{ID}")];
        assert!(key.contains("PRIVATE KEY"));
        assert_eq!(
            files(config.path()),
            [PathBuf::from(format!("record-ca/{ID}.pem"))]
        );
        let cert = fs::read_to_string(config.path().join(format!("record-ca/{ID}.pem"))).unwrap();
        assert_eq!(cert, ca.cert_pem());
        assert!(!cert.contains("PRIVATE KEY"));
    }

    #[test]
    fn no_keychain_means_no_ca() {
        let config = tempfile::tempdir().unwrap();
        let none = CaStore::new(None, config.path(), ID);
        let err = none.init().unwrap_err();
        assert_eq!(
            err.to_string(),
            "record: a keychain is required to store the CA private key"
        );
        let down = MockKeychain {
            unavailable: true,
            ..MockKeychain::default()
        };
        let err = store(&down, config.path()).init().unwrap_err();
        assert!(matches!(err, CaError::NoKeychain));
        assert!(files(config.path()).is_empty(), "nothing written");
    }

    #[test]
    fn a_second_init_is_refused_and_load_returns_the_ca() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let store = store(&keychain, config.path());
        assert!(store.load().unwrap().is_none());
        let ca = store.init().unwrap();
        let err = store.init().unwrap_err();
        assert!(matches!(err, CaError::AlreadyExists(_)), "{err}");
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.info().unwrap(), ca.info().unwrap());
    }

    #[test]
    fn a_certificate_without_its_key_is_a_mismatch() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let store = store(&keychain, config.path());
        store.init().unwrap();
        keychain.entries.lock().unwrap().clear();
        assert!(matches!(store.load(), Err(CaError::Mismatch)));
    }

    #[test]
    fn remove_deletes_the_key_and_the_certificate() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let store = store(&keychain, config.path());
        store.init().unwrap();
        store.remove().unwrap();
        assert!(keychain.entries.lock().unwrap().is_empty());
        assert!(store.certificate().unwrap().is_none());
    }

    #[test]
    #[ignore = "uses the real OS keychain; run by hand with --ignored"]
    fn the_os_keychain_holds_the_key() {
        let config = tempfile::tempdir().unwrap();
        let id = format!("test-{}", std::process::id());
        let store = CaStore::new(
            rustman_core::secrets::keychain::os_keychain(),
            config.path(),
            &id,
        );
        let ca = store.init().unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.info().unwrap(), ca.info().unwrap());
        store.remove().unwrap();
        assert!(store.certificate().unwrap().is_none());
    }
}
