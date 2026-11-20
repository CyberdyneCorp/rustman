//! `rustman.lock`: the tracked record of every plugin's version, source,
//! sha256 and API major (plugins spec, "Integrity pinning and lockfile").
//! `rustman.toml` holds the pins; the lockfile records what they resolved
//! to, so a review shows the version and API next to a changed hash.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::LoadError;

/// The lockfile name, at the workspace root.
pub const LOCK_FILE: &str = "rustman.lock";

/// The lockfile format this build reads and writes.
pub const LOCK_VERSION: u32 = 1;

const HEADER: &str = "# Written by rustman (plugin add, plugin verify). Do not edit by hand.\n";

/// One locked plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedPlugin {
    pub name: String,
    pub version: String,
    pub source: String,
    pub sha256: String,
    pub api: u64,
}

/// The lockfile's content, sorted by plugin name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lockfile {
    pub version: u32,
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<LockedPlugin>,
}

impl Lockfile {
    pub fn parse(text: &str) -> Result<Self, LoadError> {
        let lock: Self = toml::from_str(text).map_err(|e| lock_error(e.message().trim_end()))?;
        if lock.version != LOCK_VERSION {
            let message = format!(
                "unsupported version {} (expected {LOCK_VERSION})",
                lock.version
            );
            return Err(lock_error(&message));
        }
        Ok(lock)
    }

    /// The lockfile of the workspace at `root`; empty when there is none.
    pub fn read(root: &Path) -> Result<Self, LoadError> {
        let path = root.join(LOCK_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(lock_error(&e.to_string())),
        }
    }

    pub fn new() -> Self {
        Self {
            version: LOCK_VERSION,
            plugins: Vec::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&LockedPlugin> {
        self.plugins.iter().find(|p| p.name == name)
    }

    /// Add `plugin`, or replace the entry of the same name.
    pub fn upsert(&mut self, plugin: LockedPlugin) {
        self.remove(&plugin.name);
        self.plugins.push(plugin);
        self.plugins.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Remove the entry of `name`; whether there was one.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.plugins.len();
        self.plugins.retain(|p| p.name != name);
        self.plugins.len() != before
    }

    /// The TOML text, with `\n` line endings on every platform.
    pub fn render(&self) -> String {
        let body = toml::to_string(self).expect("the lockfile serializes");
        format!("{HEADER}{body}")
    }

    /// Write the lockfile of the workspace at `root`.
    pub fn write(&self, root: &Path) -> Result<(), LoadError> {
        std::fs::write(root.join(LOCK_FILE), self.render()).map_err(|e| lock_error(&e.to_string()))
    }
}

fn lock_error(message: &str) -> LoadError {
    LoadError::Workspace(format!("{LOCK_FILE}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked(name: &str, sha: char) -> LockedPlugin {
        LockedPlugin {
            name: name.into(),
            version: "0.2.0".into(),
            source: format!("plugins/{name}"),
            sha256: sha.to_string().repeat(64),
            api: 1,
        }
    }

    #[test]
    fn the_lockfile_round_trips_sorted() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Lockfile::read(dir.path()).unwrap(), Lockfile::new());
        let mut lock = Lockfile::new();
        lock.upsert(locked("hmac", 'a'));
        lock.upsert(locked("faker", 'b'));
        lock.upsert(locked("hmac", 'c'));
        lock.write(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(LOCK_FILE)).unwrap();
        assert!(text.starts_with("# Written by rustman"), "{text}");
        assert!(!text.contains('\r'));
        let read = Lockfile::read(dir.path()).unwrap();
        assert_eq!(read, lock);
        let names: Vec<&str> = read.plugins.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["faker", "hmac"]);
        assert_eq!(read.get("hmac").unwrap().sha256, "c".repeat(64));
        assert!(lock.remove("faker"));
        assert!(!lock.remove("faker"));
    }

    #[test]
    fn unknown_versions_are_refused() {
        let err = Lockfile::parse("version = 2\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "rustman.lock: unsupported version 2 (expected 1)"
        );
        assert!(Lockfile::parse("version = 1\nextra = 1\n").is_err());
    }
}
