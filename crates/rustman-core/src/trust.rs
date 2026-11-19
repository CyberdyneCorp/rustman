//! Workspace trust: this machine's approvals of what a workspace asks it to
//! run, such as the command of a stdio MCP server (mcp-client spec "Stdio
//! command trust").
//!
//! Approvals live in `.rustman/trust.yaml`, which Git must ignore: trust is
//! per machine and never shared through the repository. Each approval maps
//! a subject (for example `mcp/files`) to the HMAC-SHA256, under this
//! machine's [`TrustKey`], of the workspace's location, the subject and the
//! fingerprint of what was approved; the caller computes fingerprints, so
//! any feature can reuse the store with its own notion of "the same thing".
//! The key lives outside every workspace (in the user's data directory), so
//! an approval written by a repository, copied from another machine or from
//! another workspace matches nothing, with or without Git; moving a
//! workspace asks again. As a second line of defense, a trust file that Git
//! tracks or does not ignore, or that is a symbolic link, is refused when
//! read as well as when written.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::error::LoadError;
use crate::format::from_yaml;
use crate::format::write::{Node, render};
use crate::history::rustman_data_dir;
use crate::secrets::{SecretError, ensure_private, write_private, write_private_new};

/// The trust file, relative to the workspace root.
pub const TRUST_FILE: &str = ".rustman/trust.yaml";

/// The machine key file, in the user's data directory.
pub const TRUST_KEY_FILE: &str = "trust.key";

/// The size of the machine key, in bytes.
const KEY_LEN: usize = 32;

/// Whether a subject's current fingerprint is approved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustState {
    /// Never approved on this machine.
    Unknown,
    /// Approved, but with another fingerprint: it changed since.
    Changed,
    Trusted,
}

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// `.rustman/` is unsafe to write, or Git does not ignore the file.
    #[error(transparent)]
    State(#[from] SecretError),
    #[error(transparent)]
    Invalid(#[from] LoadError),
    /// The trust file came with the workspace rather than from this
    /// machine's approvals.
    #[error("refusing {TRUST_FILE}: {0}; delete it and approve the servers again")]
    Refused(&'static str),
    /// This machine's trust key cannot be read or created.
    #[error("trust key {}: {message}", path.display())]
    Key { path: PathBuf, message: String },
    /// There is no user data directory to keep the trust key in.
    #[error("cannot approve: no user data directory for the trust key (set RUSTMAN_DATA_DIR)")]
    NoKeyDir,
}

/// Where this machine's trust key is kept: a file of 32 random bytes (hex),
/// readable by the current user only, created on the first approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustKey {
    /// The folder holding [`TRUST_KEY_FILE`]; `None` without a data directory.
    dir: Option<PathBuf>,
}

impl Default for TrustKey {
    fn default() -> Self {
        Self::machine()
    }
}

impl TrustKey {
    /// The key in the user's data directory (`$RUSTMAN_DATA_DIR` when set).
    pub fn machine() -> Self {
        Self {
            dir: rustman_data_dir(),
        }
    }

    /// The key in `dir` (tests, sandboxes).
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
        }
    }

    fn path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|dir| dir.join(TRUST_KEY_FILE))
    }

    /// The key when it exists; `None` before the first approval.
    fn read(&self) -> Result<Option<[u8; KEY_LEN]>, TrustError> {
        let Some(path) = self.path() else {
            return Ok(None);
        };
        match fs::read_to_string(&path) {
            Ok(text) => parse_key(&text)
                .map(Some)
                .ok_or_else(|| key_error(&path, "not 64 hex digits")),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(key_error(&path, &e.to_string())),
        }
    }

    /// The key, created (readable by the current user only) when missing.
    fn read_or_create(&self) -> Result<[u8; KEY_LEN], TrustError> {
        let path = self.path().ok_or(TrustError::NoKeyDir)?;
        if let Some(key) = self.read()? {
            return Ok(key);
        }
        let key: [u8; KEY_LEN] = rand::random();
        match write_private_new(&path, hex(&key).as_bytes()) {
            Ok(()) => Ok(key),
            // Another process created it first: use theirs.
            Err(e) if e.kind() == ErrorKind::AlreadyExists => self
                .read()?
                .ok_or_else(|| key_error(&path, "vanished after it was created")),
            Err(e) => Err(key_error(&path, &e.to_string())),
        }
    }
}

fn key_error(path: &Path, message: &str) -> TrustError {
    TrustError::Key {
        path: path.to_path_buf(),
        message: message.to_owned(),
    }
}

fn parse_key(text: &str) -> Option<[u8; KEY_LEN]> {
    let text = text.trim();
    if text.len() != KEY_LEN * 2 || !text.is_ascii() {
        return None;
    }
    let mut key = [0; KEY_LEN];
    for (byte, pair) in key.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The approval recorded under `key` for `fingerprint` of `subject` in the
/// workspace at `scope`.
fn seal(key: &[u8; KEY_LEN], scope: &str, subject: &str, fingerprint: &str) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key");
    for part in [scope, subject, fingerprint] {
        mac.update(part.as_bytes());
        mac.update(b"\0");
    }
    hex(&mac.finalize().into_bytes())
}

/// Where the workspace at `root` is, as approvals are bound to it.
fn scope(root: &Path) -> String {
    dunce::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Why a trust file on disk is not this machine's own.
fn refused(err: SecretError) -> TrustError {
    match err {
        SecretError::NotIgnored => TrustError::Refused("Git tracks it or does not ignore it"),
        SecretError::UnsafeStateDir(_) => {
            TrustError::Refused("it or .rustman is a symbolic link or not a plain folder")
        }
        SecretError::GitCheckFailed => TrustError::Refused("could not check that Git ignores it"),
        other => TrustError::State(other),
    }
}

/// The approvals of one workspace on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustStore {
    root: PathBuf,
    /// Subject → the approval's seal (see [`seal`]).
    entries: BTreeMap<String, String>,
    key: TrustKey,
    /// The key's bytes, once it exists.
    secret: Option<[u8; KEY_LEN]>,
    /// The workspace's location (see [`scope`]).
    scope: String,
}

impl TrustStore {
    /// Read `.rustman/trust.yaml` under `root` with this machine's key
    /// ([`TrustKey::machine`]); see [`TrustStore::load_with_key`].
    pub fn load(root: &Path) -> Result<Self, TrustError> {
        Self::load_with_key(root, TrustKey::machine())
    }

    /// Read `.rustman/trust.yaml` under `root`, checking approvals with
    /// `key`; a missing file is empty. Fails when the file came with the
    /// repository: Git tracks it or does not ignore it, or it (or
    /// `.rustman`) is a symbolic link.
    pub fn load_with_key(root: &Path, key: TrustKey) -> Result<Self, TrustError> {
        let path = root.join(TRUST_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(SecretError::Io { path, source: e }.into()),
        };
        if path.symlink_metadata().is_ok() {
            ensure_private(root, TRUST_FILE).map_err(refused)?;
        }
        let entries = if text.trim().is_empty() {
            BTreeMap::new()
        } else {
            from_yaml(Path::new(TRUST_FILE), &text).map_err(|e| {
                LoadError::new(
                    &e.file,
                    e.position,
                    "invalid trust file: expected a mapping of subject to fingerprint",
                )
            })?
        };
        let secret = key.read()?;
        Ok(Self {
            root: root.to_path_buf(),
            entries,
            key,
            secret,
            scope: scope(root),
        })
    }

    /// An approval this machine did not seal for this workspace (written by
    /// a repository, copied from elsewhere) counts as [`TrustState::Changed`].
    pub fn state(&self, subject: &str, fingerprint: &str) -> TrustState {
        let Some(approved) = self.entries.get(subject) else {
            return TrustState::Unknown;
        };
        match &self.secret {
            Some(key) if *approved == seal(key, &self.scope, subject, fingerprint) => {
                TrustState::Trusted
            }
            _ => TrustState::Changed,
        }
    }

    pub fn is_trusted(&self, subject: &str, fingerprint: &str) -> bool {
        self.state(subject, fingerprint) == TrustState::Trusted
    }

    /// Approve `fingerprint` for `subject`, replacing an older approval;
    /// creates this machine's key on the first approval. Call
    /// [`TrustStore::save`] to keep it.
    pub fn approve(&mut self, subject: &str, fingerprint: &str) -> Result<(), TrustError> {
        let key = match self.secret {
            Some(key) => key,
            None => *self.secret.insert(self.key.read_or_create()?),
        };
        self.entries.insert(
            subject.to_owned(),
            seal(&key, &self.scope, subject, fingerprint),
        );
        Ok(())
    }

    /// Forget `subject`; true when it was approved.
    pub fn revoke(&mut self, subject: &str) -> bool {
        self.entries.remove(subject).is_some()
    }

    /// Write the approvals, readable by the current user only. Fails when
    /// Git would track the file.
    pub fn save(&self) -> Result<(), TrustError> {
        ensure_private(&self.root, TRUST_FILE)?;
        let path = self.root.join(TRUST_FILE);
        let node = Node::Map(
            self.entries
                .iter()
                .map(|(subject, fingerprint)| (subject.clone(), Node::str(fingerprint)))
                .collect(),
        );
        write_private(&path, render(&node).as_bytes())
            .map_err(|source| SecretError::Io { path, source })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace folder and a key folder of its own, never the user's.
    struct Place {
        dir: tempfile::TempDir,
        keys: tempfile::TempDir,
    }

    impl Place {
        fn new() -> Self {
            Self::at(tempfile::tempdir().unwrap())
        }

        fn at(dir: tempfile::TempDir) -> Self {
            Self {
                dir,
                keys: tempfile::tempdir().unwrap(),
            }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        fn load(&self) -> Result<TrustStore, TrustError> {
            TrustStore::load_with_key(self.path(), TrustKey::in_dir(self.keys.path()))
        }
    }

    #[test]
    fn missing_file_trusts_nothing() {
        let dir = Place::new();
        let store = dir.load().unwrap();
        assert_eq!(store.state("mcp/files", "abc"), TrustState::Unknown);
    }

    #[test]
    fn approvals_survive_a_reload() {
        let dir = Place::new();
        let mut store = dir.load().unwrap();
        store.approve("mcp/files", "secret-xyz").unwrap();
        store.save().unwrap();
        let text = fs::read_to_string(dir.path().join(TRUST_FILE)).unwrap();
        assert!(text.starts_with("mcp/files: "), "{text}");
        assert!(
            !text.contains("secret-xyz"),
            "only the seal is written; the value is not hex so it cannot show up by chance: {text}"
        );
        let store = dir.load().unwrap();
        assert!(store.is_trusted("mcp/files", "secret-xyz"));
        assert_eq!(store.state("mcp/files", "def"), TrustState::Changed);
        assert_eq!(store.state("mcp/other", "secret-xyz"), TrustState::Unknown);
    }

    #[test]
    fn revoke_forgets_a_subject() {
        let dir = Place::new();
        let mut store = dir.load().unwrap();
        store.approve("mcp/files", "abc").unwrap();
        assert!(store.revoke("mcp/files"));
        assert!(!store.revoke("mcp/files"));
        assert_eq!(store.state("mcp/files", "abc"), TrustState::Unknown);
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git is installed")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    /// A repository that ignores `.rustman/` yet ships an approval in it.
    fn repo_with_committed_trust() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        fs::write(root.join(".gitignore"), ".rustman/\n").unwrap();
        fs::create_dir(root.join(".rustman")).unwrap();
        fs::write(root.join(TRUST_FILE), "mcp/evil: abc\n").unwrap();
        git(root, &["add", "-f", ".gitignore", TRUST_FILE]);
        git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "-m",
                "ship trust",
            ],
        );
        dir
    }

    #[test]
    fn a_committed_trust_file_is_refused() {
        let dir = Place::at(repo_with_committed_trust());
        let err = dir.load().unwrap_err();
        assert_eq!(
            err.to_string(),
            "refusing .rustman/trust.yaml: Git tracks it or does not ignore it; \
             delete it and approve the servers again"
        );
    }

    #[test]
    fn a_trust_file_git_does_not_ignore_is_refused() {
        let dir = Place::new();
        git(dir.path(), &["init", "-q"]);
        fs::create_dir(dir.path().join(".rustman")).unwrap();
        fs::write(dir.path().join(TRUST_FILE), "mcp/evil: abc\n").unwrap();
        let err = dir.load().unwrap_err();
        assert!(matches!(err, TrustError::Refused(_)), "{err}");
    }

    #[test]
    fn an_ignored_untracked_trust_file_is_read() {
        let dir = Place::new();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join(".gitignore"), ".rustman/\n").unwrap();
        let mut store = dir.load().unwrap();
        store.approve("mcp/files", "abc").unwrap();
        store.save().unwrap();
        let store = dir.load().unwrap();
        assert!(store.is_trusted("mcp/files", "abc"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_trust_file_is_refused() {
        let dir = Place::new();
        fs::create_dir(dir.path().join(".rustman")).unwrap();
        fs::write(dir.path().join("elsewhere.yaml"), "mcp/evil: abc\n").unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("elsewhere.yaml"),
            dir.path().join(TRUST_FILE),
        )
        .unwrap();
        let err = dir.load().unwrap_err();
        assert!(matches!(err, TrustError::Refused(_)), "{err}");
    }

    #[test]
    fn invalid_file_is_reported_without_its_content() {
        let dir = Place::new();
        fs::create_dir(dir.path().join(".rustman")).unwrap();
        fs::write(dir.path().join(TRUST_FILE), "- not\n- a map\n").unwrap();
        let err = dir.load().unwrap_err();
        assert!(err.to_string().contains("invalid trust file"), "{err}");
    }

    /// A trust file is only as good as the key that sealed it: one written
    /// under another machine's key, or by hand, trusts nothing here, even
    /// outside any Git work tree.
    #[test]
    fn approvals_sealed_with_another_key_trust_nothing() {
        let elsewhere = Place::new();
        let mut store = elsewhere.load().unwrap();
        store.approve("mcp/files", "abc").unwrap();
        store.save().unwrap();
        let here = Place::new();
        // This machine has a key of its own.
        TrustKey::in_dir(here.keys.path()).read_or_create().unwrap();
        fs::create_dir(here.path().join(".rustman")).unwrap();
        fs::copy(
            elsewhere.path().join(TRUST_FILE),
            here.path().join(TRUST_FILE),
        )
        .unwrap();
        let copied = here.load().unwrap();
        assert_eq!(copied.state("mcp/files", "abc"), TrustState::Changed);
        fs::write(here.path().join(TRUST_FILE), "mcp/files: abc\n").unwrap();
        let forged = here.load().unwrap();
        assert_eq!(forged.state("mcp/files", "abc"), TrustState::Changed);
    }

    /// The same key, but another workspace: a trust file copied from one
    /// workspace to another (or a moved workspace) trusts nothing.
    #[test]
    fn approvals_are_bound_to_their_workspace() {
        let first = Place::new();
        let mut store = first.load().unwrap();
        store.approve("mcp/files", "abc").unwrap();
        store.save().unwrap();
        let second = tempfile::tempdir().unwrap();
        fs::create_dir(second.path().join(".rustman")).unwrap();
        fs::copy(
            first.path().join(TRUST_FILE),
            second.path().join(TRUST_FILE),
        )
        .unwrap();
        let key = TrustKey::in_dir(first.keys.path());
        let copied = TrustStore::load_with_key(second.path(), key).unwrap();
        assert_eq!(copied.state("mcp/files", "abc"), TrustState::Changed);
        assert!(first.load().unwrap().is_trusted("mcp/files", "abc"));
    }

    #[test]
    fn the_key_is_created_once_and_private() {
        let dir = Place::new();
        let key_file = dir.keys.path().join("nested").join(TRUST_KEY_FILE);
        let key = TrustKey::in_dir(key_file.parent().unwrap());
        let mut store = TrustStore::load_with_key(dir.path(), key.clone()).unwrap();
        assert!(!key_file.exists(), "loading creates nothing");
        store.approve("mcp/files", "abc").unwrap();
        let text = fs::read_to_string(&key_file).unwrap();
        assert!(parse_key(&text).is_some(), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&key_file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store.approve("mcp/other", "def").unwrap();
        store.save().unwrap();
        assert_eq!(fs::read_to_string(&key_file).unwrap(), text);
        let reloaded = TrustStore::load_with_key(dir.path(), key).unwrap();
        assert!(reloaded.is_trusted("mcp/files", "abc"));
        assert!(reloaded.is_trusted("mcp/other", "def"));
    }

    #[test]
    fn a_corrupt_key_is_an_error() {
        let dir = Place::new();
        fs::write(dir.keys.path().join(TRUST_KEY_FILE), "short").unwrap();
        let err = dir.load().unwrap_err();
        assert!(err.to_string().contains("not 64 hex digits"), "{err}");
    }

    #[test]
    fn without_a_data_directory_nothing_can_be_approved() {
        let dir = Place::new();
        let mut store = TrustStore::load_with_key(dir.path(), TrustKey { dir: None }).unwrap();
        assert_eq!(store.state("mcp/files", "abc"), TrustState::Unknown);
        assert!(matches!(
            store.approve("mcp/files", "abc"),
            Err(TrustError::NoKeyDir)
        ));
    }
}
