use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};

use super::git::tests::git;
use super::workspace_id::WORKSPACE_ID_FILE;
use super::*;

/// An in-memory keychain shared with the test, or one that always fails.
#[derive(Clone, Default)]
struct FakeKeychain {
    entries: Arc<Mutex<BTreeMap<String, String>>>,
    error: Option<KeychainError>,
    calls: Arc<Mutex<usize>>,
}

impl FakeKeychain {
    fn failing(error: KeychainError) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }

    fn check(&self) -> Result<(), KeychainError> {
        *self.calls.lock().unwrap() += 1;
        self.error.clone().map_or(Ok(()), Err)
    }

    fn entries(&self) -> BTreeMap<String, String> {
        self.entries.lock().unwrap().clone()
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl Keychain for FakeKeychain {
    fn get(&self, account: &str) -> Result<Option<String>, KeychainError> {
        self.check()?;
        Ok(self.entries.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), KeychainError> {
        self.check()?;
        self.entries
            .lock()
            .unwrap()
            .insert(account.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<bool, KeychainError> {
        self.check()?;
        Ok(self.entries.lock().unwrap().remove(account).is_some())
    }
}

fn token(value: &str) -> Secret<String> {
    Secret::new("api_token", value.to_owned())
}

fn store_with(keychain: &FakeKeychain) -> (tempfile::TempDir, SecretStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), Some(Box::new(keychain.clone())));
    (dir, store)
}

fn unavailable() -> FakeKeychain {
    FakeKeychain::failing(KeychainError::Unavailable("no secret service".into()))
}

fn value(store: &SecretStore, env: &str, name: &str) -> Option<String> {
    store.get(env, name).unwrap().map(|s| s.expose().clone())
}

#[test]
fn keychain_is_used_when_available() {
    let keychain = FakeKeychain::default();
    let (dir, store) = store_with(&keychain);
    assert_eq!(
        store.set("prod", &token("s3cr3t")).unwrap(),
        StoredIn::Keychain
    );
    let id = workspace_id::load(dir.path()).unwrap().unwrap();
    assert_eq!(
        keychain.entries(),
        BTreeMap::from([(format!("{id}/prod/api_token"), "s3cr3t".to_owned())])
    );
    assert!(!dir.path().join(SECRETS_FILE).exists());
    assert_eq!(
        value(&store, "prod", "api_token").as_deref(),
        Some("s3cr3t")
    );
    assert_eq!(value(&store, "local", "api_token"), None);
    assert!(store.take_warnings().is_empty());
}

#[test]
fn unavailable_keychain_falls_back_to_the_file_with_one_warning() {
    let keychain = unavailable();
    let (dir, store) = store_with(&keychain);
    assert_eq!(store.set("prod", &token("s3cr3t")).unwrap(), StoredIn::File);
    store
        .set("prod", &Secret::new("other", "x".to_owned()))
        .unwrap();
    assert_eq!(store.take_warnings(), [FALLBACK_WARNING]);
    assert_eq!(
        FALLBACK_WARNING,
        "OS keychain unavailable; storing secrets in .rustman/secrets.yaml"
    );
    let text = fs::read_to_string(dir.path().join(SECRETS_FILE)).unwrap();
    assert!(text.contains("api_token: s3cr3t"), "{text}");
    assert_eq!(
        value(&store, "prod", "api_token").as_deref(),
        Some("s3cr3t")
    );
    assert!(store.take_warnings().is_empty());
    // Once known to be down, the keychain is not asked again.
    assert_eq!(keychain.calls(), 1);
}

#[test]
fn warning_is_given_even_when_a_read_found_the_keychain_down() {
    let keychain = unavailable();
    let (dir, store) = store_with(&keychain);
    workspace_id::load_or_create(dir.path()).unwrap();
    assert_eq!(value(&store, "prod", "api_token"), None);
    assert_eq!(store.set("prod", &token("s3cr3t")).unwrap(), StoredIn::File);
    assert_eq!(store.take_warnings(), [FALLBACK_WARNING]);
}

#[test]
fn file_backend_never_touches_the_keychain_and_does_not_warn() {
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::open(dir.path(), SecretBackend::File);
    assert_eq!(store.set("prod", &token("s3cr3t")).unwrap(), StoredIn::File);
    assert!(store.take_warnings().is_empty());
    assert!(!dir.path().join(WORKSPACE_ID_FILE).exists());
    assert_eq!(
        value(&store, "prod", "api_token").as_deref(),
        Some("s3cr3t")
    );
}

#[test]
fn keychain_errors_are_reported_without_fallback() {
    let keychain = FakeKeychain::failing(KeychainError::Failed("denied".into()));
    let (dir, store) = store_with(&keychain);
    let err = store.set("prod", &token("s3cr3t")).unwrap_err();
    assert_eq!(err.to_string(), "OS keychain error: denied");
    assert!(!dir.path().join(SECRETS_FILE).exists());
}

#[test]
fn reads_do_not_create_the_workspace_id() {
    let keychain = FakeKeychain::default();
    let (dir, store) = store_with(&keychain);
    assert_eq!(value(&store, "prod", "api_token"), None);
    assert!(!dir.path().join(WORKSPACE_ID_FILE).exists());
    assert_eq!(keychain.calls(), 0);
}

#[test]
fn storing_in_the_keychain_removes_a_stale_file_copy() {
    let keychain = FakeKeychain::default();
    let (dir, store) = store_with(&keychain);
    FileSecretStore::new(dir.path())
        .set("prod", "api_token", "old")
        .unwrap();
    store.set("prod", &token("new")).unwrap();
    assert!(!dir.path().join(SECRETS_FILE).exists());
    assert_eq!(value(&store, "prod", "api_token").as_deref(), Some("new"));
}

#[test]
fn remove_deletes_from_keychain_and_file() {
    let keychain = FakeKeychain::default();
    let (dir, store) = store_with(&keychain);
    store.set("prod", &token("k")).unwrap();
    FileSecretStore::new(dir.path())
        .set("prod", "file_only", "f")
        .unwrap();
    assert!(store.remove("prod", "api_token").unwrap());
    assert!(store.remove("prod", "file_only").unwrap());
    assert!(!store.remove("prod", "api_token").unwrap());
    assert!(keychain.entries().is_empty());
    assert_eq!(value(&store, "prod", "file_only"), None);
}

#[test]
fn not_ignored_file_is_refused_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
    let store = SecretStore::with_keychain(dir.path(), Some(Box::new(unavailable())));
    let err = store.set("prod", &token("s3cr3t")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "refusing to write secrets: .rustman/ is not git-ignored"
    );
    assert!(!dir.path().join(SECRETS_FILE).exists());
}

#[test]
fn empty_values_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::open(dir.path(), SecretBackend::File);
    assert!(matches!(
        store.set("prod", &token("")),
        Err(SecretError::EmptyValue)
    ));
}

#[test]
fn backend_is_chosen_by_the_environment_variable() {
    assert_eq!(
        SecretBackend::from_env_value(None).unwrap(),
        SecretBackend::Keychain
    );
    assert_eq!(
        SecretBackend::from_env_value(Some("")).unwrap(),
        SecretBackend::Keychain
    );
    assert_eq!(
        SecretBackend::from_env_value(Some("keychain")).unwrap(),
        SecretBackend::Keychain
    );
    assert_eq!(
        SecretBackend::from_env_value(Some("file")).unwrap(),
        SecretBackend::File
    );
    assert_eq!(
        SecretBackend::from_env_value(Some("vault"))
            .unwrap_err()
            .to_string(),
        "invalid RUSTMAN_SECRET_STORE 'vault': expected 'keychain' or 'file'"
    );
}

#[test]
fn store_is_a_secret_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::open(dir.path(), SecretBackend::File);
    store.set("prod", &token("s3cr3t")).unwrap();
    assert_eq!(
        store.secret("prod", "api_token").unwrap().as_deref(),
        Some("s3cr3t")
    );
    assert_eq!(store.secret("prod", "missing"), Ok(None));
    fs::write(dir.path().join(SECRETS_FILE), "[").unwrap();
    let err = store.secret("prod", "api_token").unwrap_err();
    assert!(err.starts_with(".rustman/secrets.yaml:"), "{err}");
    assert!(store.take_warnings().is_empty());
}

/// Run manually on macOS and Windows:
/// `cargo test -p rustman-core -- --ignored keychain_store`.
#[test]
#[ignore = "touches the real OS keychain"]
fn keychain_store_set_and_use_changes_no_workspace_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("rustman.toml"), "").unwrap();
    let store = SecretStore::open(dir.path(), SecretBackend::Keychain);
    assert_eq!(
        store.set("prod", &token("s3cr3t")).unwrap(),
        StoredIn::Keychain
    );
    assert_eq!(
        value(&store, "prod", "api_token").as_deref(),
        Some("s3cr3t")
    );
    assert!(!dir.path().join(SECRETS_FILE).exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("rustman.toml")).unwrap(),
        ""
    );
    assert!(store.remove("prod", "api_token").unwrap());
    assert_eq!(value(&store, "prod", "api_token"), None);
}

#[cfg(unix)]
#[test]
fn reading_from_an_exposed_file_warns_once() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::open(dir.path(), SecretBackend::File);
    store.set("prod", &token("s3cr3t")).unwrap();
    assert_eq!(
        value(&store, "prod", "api_token").as_deref(),
        Some("s3cr3t")
    );
    assert!(store.take_warnings().is_empty());
    let path = dir.path().join(SECRETS_FILE);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        value(&store, "prod", "api_token").as_deref(),
        Some("s3cr3t")
    );
    assert_eq!(
        value(&store, "prod", "api_token").as_deref(),
        Some("s3cr3t")
    );
    assert_eq!(store.take_warnings(), [EXPOSED_WARNING]);
}

#[cfg(unix)]
#[test]
fn symlinked_state_folder_is_refused_for_every_backend() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    fs::write(dir.path().join(".gitignore"), ".rustman/\n").unwrap();
    fs::create_dir_all(dir.path().join("public")).unwrap();
    std::os::unix::fs::symlink("public", dir.path().join(".rustman")).unwrap();
    let keychain = FakeKeychain::default();
    let stores = [
        SecretStore::open(dir.path(), SecretBackend::File),
        SecretStore::with_keychain(dir.path(), Some(Box::new(keychain.clone()))),
    ];
    for store in stores {
        let err = store.set("prod", &token("s3cr3t")).unwrap_err();
        assert!(matches!(err, SecretError::UnsafeStateDir(_)), "{err}");
    }
    assert!(keychain.entries().is_empty());
    assert_eq!(fs::read_dir(dir.path().join("public")).unwrap().count(), 0);
}

#[test]
fn set_many_stores_in_the_file_with_one_write_and_stays_linear() {
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::open(dir.path(), SecretBackend::File);
    let secrets: Vec<Secret<String>> = (0..5_000)
        .map(|i| Secret::new(format!("s{i}"), format!("v-{i}")))
        .collect();
    let batch: Vec<(&str, &Secret<String>)> = secrets.iter().map(|s| ("prod", s)).collect();
    let start = std::time::Instant::now();
    store.set_many(&batch).unwrap();
    // One write instead of 5,000 read-modify-writes; a generous bound.
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(value(&store, "prod", "s4999").as_deref(), Some("v-4999"));
    assert!(store.set_many(&[("prod", &token(""))]).is_err());
}

#[test]
fn set_many_uses_the_keychain_and_clears_stale_file_copies() {
    let keychain = FakeKeychain::default();
    let (dir, store) = store_with(&keychain);
    FileSecretStore::new(dir.path())
        .set("prod", "api_token", "old")
        .unwrap();
    let other = Secret::new("other", "o".to_owned());
    store
        .set_many(&[("prod", &token("new")), ("dev", &other)])
        .unwrap();
    assert_eq!(keychain.entries().len(), 2);
    assert!(!dir.path().join(SECRETS_FILE).exists());
    assert_eq!(value(&store, "prod", "api_token").as_deref(), Some("new"));
}

#[test]
fn set_many_falls_back_to_the_file_when_the_keychain_is_down() {
    let keychain = unavailable();
    let (dir, store) = store_with(&keychain);
    let other = Secret::new("other", "o".to_owned());
    store
        .set_many(&[("prod", &token("s3cr3t")), ("prod", &other)])
        .unwrap();
    assert_eq!(store.take_warnings(), [FALLBACK_WARNING]);
    let text = fs::read_to_string(dir.path().join(SECRETS_FILE)).unwrap();
    assert!(
        text.contains("api_token: s3cr3t") && text.contains("other: o"),
        "{text}"
    );
    assert_eq!(keychain.calls(), 1);
}
