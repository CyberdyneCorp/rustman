//! The file fallback: `.rustman/secrets.yaml`, readable by the current user
//! only, and never written unless Git ignores it.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::{SecretError, git, private_file};
use crate::error::LoadError;
use crate::format::from_yaml;
use crate::format::write::{Node, render};

/// The fallback file, relative to the workspace root.
pub const SECRETS_FILE: &str = ".rustman/secrets.yaml";

/// Serializes updates of [`SECRETS_FILE`] across processes.
const LOCK_FILE: &str = ".rustman/secrets.yaml.lock";

/// Environment name → secret name → value.
type Table = BTreeMap<String, BTreeMap<String, String>>;

/// Secrets stored in `.rustman/secrets.yaml`.
#[derive(Clone, Debug)]
pub struct FileSecretStore {
    root: PathBuf,
}

impl FileSecretStore {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// The absolute path of the file.
    pub fn path(&self) -> PathBuf {
        self.root.join(SECRETS_FILE)
    }

    /// Whether the file exists and users other than its owner may access it,
    /// as after a copy or restore that loosened its mode.
    #[cfg(unix)]
    pub fn is_exposed(&self) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(self.path()).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
    }

    /// Whether the file exists and users other than its owner may access it.
    #[cfg(not(unix))]
    pub fn is_exposed(&self) -> bool {
        false
    }

    pub fn get(&self, env: &str, name: &str) -> Result<Option<String>, SecretError> {
        let mut table = self.read()?;
        Ok(table.get_mut(env).and_then(|secrets| secrets.remove(name)))
    }

    pub fn set(&self, env: &str, name: &str, value: &str) -> Result<(), SecretError> {
        git::ensure_private(&self.root, SECRETS_FILE)?;
        let _lock = self.lock()?;
        let mut table = self.read()?;
        table
            .entry(env.to_owned())
            .or_default()
            .insert(name.to_owned(), value.to_owned());
        self.write(&table)
    }

    /// Store every `(env, name, value)` with one read and one write of the
    /// file, under one lock.
    pub fn set_many(&self, entries: &[(&str, &str, &str)]) -> Result<(), SecretError> {
        if entries.is_empty() {
            return Ok(());
        }
        git::ensure_private(&self.root, SECRETS_FILE)?;
        let _lock = self.lock()?;
        let mut table = self.read()?;
        for (env, name, value) in entries {
            table
                .entry((*env).to_owned())
                .or_default()
                .insert((*name).to_owned(), (*value).to_owned());
        }
        self.write(&table)
    }

    /// Delete every `(env, name)` with at most one read and one write; the
    /// file is only touched when it held one of them.
    pub fn remove_many(&self, entries: &[(&str, &str)]) -> Result<(), SecretError> {
        if entries.is_empty() || !self.path().exists() {
            return Ok(());
        }
        let _lock = self.lock()?;
        let mut table = self.read()?;
        let mut changed = false;
        for (env, name) in entries {
            if let Some(secrets) = table.get_mut(*env) {
                changed |= secrets.remove(*name).is_some();
                if secrets.is_empty() {
                    table.remove(*env);
                }
            }
        }
        if changed {
            self.write(&table)?;
        }
        Ok(())
    }

    /// Delete `env`/`name`; the file is only touched when it held the entry.
    pub fn remove(&self, env: &str, name: &str) -> Result<bool, SecretError> {
        if !self.path().exists() {
            return Ok(false);
        }
        let _lock = self.lock()?;
        let mut table = self.read()?;
        let Some(secrets) = table.get_mut(env) else {
            return Ok(false);
        };
        if secrets.remove(name).is_none() {
            return Ok(false);
        }
        if secrets.is_empty() {
            table.remove(env);
        }
        self.write(&table)?;
        Ok(true)
    }

    /// Hold an exclusive lock on `.rustman/secrets.yaml.lock` for one
    /// read-modify-write, so concurrent `rustman secret set` runs do not lose
    /// updates. The lock is released when the returned file is dropped.
    fn lock(&self) -> Result<fs::File, SecretError> {
        git::ensure_state_dir(&self.root)?;
        let path = self.root.join(LOCK_FILE);
        let open = || {
            private_file::create_private_dir(path.parent().unwrap_or(&self.root))?;
            let file = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)?;
            file.lock()?;
            Ok(file)
        };
        open().map_err(|e| SecretError::io(&path, e))
    }

    fn read(&self) -> Result<Table, SecretError> {
        let path = self.path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Table::new()),
            Err(e) => return Err(SecretError::io(&path, e)),
        };
        if text.trim().is_empty() {
            return Ok(Table::new());
        }
        // Parser messages can quote a scalar, so only the position is kept.
        from_yaml(Path::new(SECRETS_FILE), &text).map_err(|e| {
            LoadError::new(
                &e.file,
                e.position,
                "invalid secrets file: expected a mapping of environment to secret name to string value",
            )
            .into()
        })
    }

    /// Replace the file with `table`, or delete it when `table` is empty.
    fn write(&self, table: &Table) -> Result<(), SecretError> {
        let path = self.path();
        if table.is_empty() {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() != ErrorKind::NotFound => Err(SecretError::io(&path, e)),
                _ => Ok(()),
            };
        }
        git::ensure_private(&self.root, SECRETS_FILE)?;
        private_file::write(&path, render(&to_node(table)).as_bytes())
            .map_err(|e| SecretError::io(&path, e))
    }
}

fn to_node(table: &Table) -> Node {
    Node::Map(
        table
            .iter()
            .map(|(env, secrets)| {
                let secrets = secrets
                    .iter()
                    .map(|(name, value)| (name.clone(), Node::str(value)))
                    .collect();
                (env.clone(), Node::Map(secrets))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, FileSecretStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn set_get_remove() {
        let (_dir, store) = store();
        assert_eq!(store.get("prod", "api_token").unwrap(), None);
        store.set("prod", "api_token", "s3cr3t").unwrap();
        store.set("prod", "other", "x").unwrap();
        store.set("local", "api_token", "dev").unwrap();
        assert_eq!(
            store.get("prod", "api_token").unwrap().as_deref(),
            Some("s3cr3t")
        );
        assert_eq!(
            store.get("local", "api_token").unwrap().as_deref(),
            Some("dev")
        );
        assert_eq!(
            fs::read_to_string(store.path()).unwrap(),
            "local:\n  api_token: dev\nprod:\n  api_token: s3cr3t\n  other: x\n"
        );
        assert!(store.remove("prod", "api_token").unwrap());
        assert!(!store.remove("prod", "api_token").unwrap());
        assert_eq!(store.get("prod", "api_token").unwrap(), None);
        assert_eq!(store.get("prod", "other").unwrap().as_deref(), Some("x"));
    }

    #[test]
    fn concurrent_updates_are_not_lost() {
        let (_dir, store) = store();
        std::thread::scope(|scope| {
            for t in 0..8 {
                let store = &store;
                scope.spawn(move || {
                    for i in 0..10 {
                        store.set("prod", &format!("s{t}_{i}"), "v").unwrap();
                    }
                });
            }
        });
        let table = store.read().unwrap();
        assert_eq!(table["prod"].len(), 80);
    }

    #[test]
    fn removing_the_last_secret_deletes_the_file() {
        let (_dir, store) = store();
        store.set("prod", "a", "1").unwrap();
        assert!(store.remove("prod", "a").unwrap());
        assert!(!store.path().exists());
    }

    #[test]
    fn remove_without_entry_does_not_touch_the_file() {
        let (_dir, store) = store();
        assert!(!store.remove("prod", "a").unwrap());
        assert!(!store.path().exists());
    }

    #[test]
    fn awkward_values_round_trip() {
        let (_dir, store) = store();
        let values = [
            "true",
            "no",
            "0755",
            "1e3",
            "null",
            "~",
            "a: b",
            "- x",
            "#x",
            " lead",
            "trail ",
            "two\nlines",
            "two\nlines\n",
            "\n\n",
            "tab\there",
            "quote'\"",
            "«x»",
            "{a}",
            "[b]",
            "&anchor",
            "*alias",
            "!tag",
            "%dir",
            "@at",
            "`tick",
            "|",
            ">",
            "'",
            "\"",
            "\\",
            "\u{7f}\u{1b}",
            "é",
            "---",
            "...",
            "?",
            ":",
            ",",
            "a #b",
            "x\r\ny",
        ];
        for (i, value) in values.iter().enumerate() {
            store.set("prod", &format!("s{i}"), value).unwrap();
        }
        for (i, value) in values.iter().enumerate() {
            assert_eq!(
                store.get("prod", &format!("s{i}")).unwrap().as_deref(),
                Some(*value),
                "{value:?}"
            );
        }
    }

    #[test]
    fn invalid_file_error_never_quotes_values() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), "prod:\n  api_token: [s3cr3t]\n").unwrap();
        let err = store.get("prod", "api_token").unwrap_err().to_string();
        assert!(!err.contains("s3cr3t"), "{err}");
        assert!(
            err.starts_with(".rustman/secrets.yaml:2: invalid secrets file"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store) = store();
        store.set("prod", "a", "1").unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!store.is_exposed());
        // A file loosened by hand is reported, and tightened on the next write.
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.is_exposed());
        store.set("prod", "b", "2").unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
