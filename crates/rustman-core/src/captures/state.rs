//! Persisted captures: `.rustman/state/<env>.yaml` (assertions spec "Capture
//! persistence scope"). The file is git-ignored runtime state; environment
//! files are never written.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use crate::format::from_yaml;
use crate::format::write::{Node, render};
use crate::model::Variables;
use crate::secrets::{SecretError, create_private_dir, ensure_private, write_private};
use crate::vars::{Layer, VariablesSource};

/// The folder of state files, relative to the workspace root.
pub const STATE_FILES_DIR: &str = ".rustman/state";

/// The state file name used when no environment is active.
pub const NO_ENVIRONMENT: &str = "@none";

/// The persisted captures of one environment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersistedState {
    vars: Variables,
}

impl PersistedState {
    /// The state file of environment `env` (`None`: no active environment).
    pub fn path(root: &Path, env: Option<&str>) -> PathBuf {
        root.join(STATE_FILES_DIR)
            .join(format!("{}.yaml", env.unwrap_or(NO_ENVIRONMENT)))
    }

    /// Load the persisted captures of `env`; a missing file holds none.
    pub fn load(root: &Path, env: Option<&str>) -> Result<Self, SecretError> {
        let path = Self::path(root, env);
        let table = read(&path)?;
        let entries = table
            .into_iter()
            .map(|(k, v)| (k, crate::model::VarValue::String(v)))
            .collect();
        Ok(Self {
            vars: Variables::new(entries),
        })
    }

    pub fn variables(&self) -> &Variables {
        &self.vars
    }

    /// The precedence-chain source for these values.
    pub fn source(&self) -> VariablesSource<'_> {
        VariablesSource::new(Layer::PersistedCapture, &self.vars)
    }

    /// Store `name = value` in the state file of `env`, keeping its other
    /// entries. Concurrent writers are serialized by a lock file, and the
    /// file is replaced atomically.
    pub fn persist(
        root: &Path,
        env: Option<&str>,
        name: &str,
        value: &str,
    ) -> Result<(), SecretError> {
        ensure_private(root, STATE_FILES_DIR)?;
        let path = Self::path(root, env);
        let _lock = lock(&path).map_err(|e| SecretError::io(&path, e))?;
        let mut table = read(&path)?;
        table.insert(name.to_owned(), value.to_owned());
        let node = Node::Map(table.into_iter().map(|(k, v)| (k, Node::str(v))).collect());
        write_private(&path, render(&node).as_bytes()).map_err(|e| SecretError::io(&path, e))
    }
}

/// Hold an exclusive lock on `<path>.lock` until the file is dropped.
fn lock(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(lock_path))?;
    file.lock()?;
    Ok(file)
}

fn read(path: &Path) -> Result<BTreeMap<String, String>, SecretError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(SecretError::io(path, e)),
    };
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let vars: Variables = from_yaml(path, &text)?;
    Ok(vars
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_string()))
        .collect())
}
