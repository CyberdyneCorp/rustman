//! Where an imported API lives in a workspace: its collection folder, its
//! environments under `environments/`, and the import snapshot under
//! `.rustman/openapi/` that later syncs merge against (the `openapi-import`
//! spec, "Import snapshot").

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rustman_core::Environment;
use rustman_core::collection::CollectionSource;
use rustman_core::environment::ENVIRONMENTS_DIR;
use rustman_core::format::write::environment_to_yaml;
use serde_json::Value;

use crate::Imported;
use crate::collection::{GenerateOptions, Generated, WriteError, generate_with, names, write};
use crate::openapi::Location;
use crate::openapi::source::shown;

/// The `source.format` of collections imported from OpenAPI.
pub const SOURCE_FORMAT: &str = "openapi";

/// The folder, under the workspace root, holding import snapshots.
pub const SNAPSHOT_DIR: &str = ".rustman/openapi";

/// A collection folder inside a workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The workspace root.
    pub root: PathBuf,
    /// The collection folder.
    pub dir: PathBuf,
}

impl Target {
    pub fn new(root: impl Into<PathBuf>, dir: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            dir: dir.into(),
        }
    }

    /// The collection's name for its snapshot file: its path relative to
    /// the workspace root, in kebab-case (`apis/shop` → `apis-shop`).
    pub fn snapshot_name(&self) -> String {
        let root = canonical(&self.root);
        let dir = canonical(&self.dir);
        let relative = dir.strip_prefix(&root).unwrap_or(&dir);
        let text = relative.to_string_lossy();
        names::file_name(&text, "api")
    }

    /// `.rustman/openapi/<collection>.json`.
    pub fn snapshot_path(&self) -> PathBuf {
        let file = format!("{}.json", self.snapshot_name());
        SNAPSHOT_DIR
            .split('/')
            .fold(self.root.clone(), |p, s| p.join(s))
            .join(file)
    }

    /// `environments/<name>.yaml`.
    pub fn environment_path(&self, name: &str) -> PathBuf {
        self.root
            .join(ENVIRONMENTS_DIR)
            .join(format!("{name}.yaml"))
    }
}

/// `path` canonicalized, or its parent canonicalized when it does not exist
/// yet (so `/var/...` and `/private/var/...` compare equal on macOS).
fn canonical(path: &Path) -> PathBuf {
    if let Ok(path) = dunce::canonicalize(path) {
        return path;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical(parent).join(name),
        _ => path.to_path_buf(),
    }
}

/// The lower-case hex SHA-256 of `text`.
pub fn sha256(text: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    digest.as_ref().iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The `source:` recorded for `imported`: a URL without credentials, or a
/// file path relative to the workspace root when it is inside it.
pub fn source_of(imported: &Imported, root: &Path) -> CollectionSource {
    let location = match &imported.location {
        Location::Url(url) => shown(url),
        Location::File(path) => relative_path(path, root),
    };
    CollectionSource {
        format: SOURCE_FORMAT.to_owned(),
        location,
        sha256: sha256(&imported.text),
    }
}

/// `path` relative to `root` with `/` separators, or as given.
fn relative_path(path: &Path, root: &Path) -> String {
    match canonical(path).strip_prefix(canonical(root)) {
        Ok(relative) => relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// The snapshot text of a document: pretty JSON with a final newline.
pub fn snapshot_text(document: &Value) -> String {
    let mut text = serde_json::to_string_pretty(document).unwrap_or_default();
    text.push('\n');
    text
}

/// The collection generated for `imported` at `target`, with its source.
pub fn generate_for(imported: &Imported, target: &Target) -> Generated {
    let options = GenerateOptions {
        source: Some(source_of(imported, &target.root)),
        location: Some(&imported.location),
    };
    generate_with(&imported.model, &options)
}

/// What a first import wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    /// Environments created.
    pub created: Vec<String>,
    /// Environments left alone because a file with that name exists.
    pub kept: Vec<String>,
    pub snapshot: PathBuf,
}

/// Write a first import: the collection folder (all files or none), every
/// environment that does not exist yet, and the snapshot.
pub fn write_import(
    imported: &Imported,
    generated: &Generated,
    target: &Target,
) -> Result<ImportOutcome, WriteError> {
    write(generated, &target.dir)?;
    let mut outcome = ImportOutcome::default();
    for env in &generated.environments {
        let path = target.environment_path(&env.name);
        if path.exists() {
            outcome.kept.push(env.name.clone());
        } else {
            write_file(&path, &environment_to_yaml(env))?;
            outcome.created.push(env.name.clone());
        }
    }
    outcome.snapshot = target.snapshot_path();
    write_file(&outcome.snapshot, &snapshot_text(&imported.document))?;
    Ok(outcome)
}

/// Write `text` to `path`, creating its folder.
pub fn write_file(path: &Path, text: &str) -> Result<(), WriteError> {
    let io = |source| WriteError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    std::fs::write(path, text).map_err(io)
}

/// Read the environment file `name` under `target`, if there is one.
pub fn read_environment(target: &Target, name: &str) -> Option<Result<Environment, String>> {
    let path = target.environment_path(name);
    let text = std::fs::read_to_string(&path).ok()?;
    let rel = Path::new(ENVIRONMENTS_DIR).join(format!("{name}.yaml"));
    Some(rustman_core::environment::parse(&rel, name, &text).map_err(|e| e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_lower_case_hex() {
        assert_eq!(
            sha256("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn snapshot_names_follow_the_collection_path() {
        let root = Path::new("ws");
        let target = Target::new(root, root.join("apis").join("Shop"));
        assert_eq!(target.snapshot_name(), "apis-shop");
        assert_eq!(
            target.snapshot_path(),
            root.join(".rustman").join("openapi").join("apis-shop.json")
        );
    }
}
