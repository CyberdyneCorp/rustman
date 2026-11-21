//! `rustman import openapi <src> --sync`: bring an imported collection up
//! to date with a new version of its document without losing the user's
//! edits (the `openapi-import` spec, "Sync preserves user edits").
//!
//! [`plan`] computes every file change and the [`SyncSummary`] without
//! touching the disk; [`apply`] performs them. `--dry-run` stops after
//! the plan.

pub mod local;
pub mod merge;
pub mod summary;

use std::path::{Path, PathBuf};

use rustman_core::Environment;
use rustman_core::collection::{COLLECTION_FILE, CollectionMeta};
use rustman_core::format::from_yaml;
use rustman_core::format::write::{collection_to_yaml, environment_to_yaml, request_to_yaml};
use rustman_core::model::{REQUEST_FILE_SUFFIX, Request, VarValue, Variables};

use crate::Imported;
use crate::collection::{GenerateOptions, Generated, GeneratedRequest, WriteError, generate_with};
use crate::openapi::{check_and_lower, document};
use crate::store::{self, Target, snapshot_text};
use local::{LocalRequest, REMOVED_DIR, join, operation_key};
use merge::{KEPT_LOCAL, merge_request, merge_rows, merge_value};
pub use summary::SyncSummary;
use summary::{ChangedOperation, Conflict, EnvironmentChange, OperationFile};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(
        "{} has no import snapshot ({}); import it again into an empty folder instead of syncing",
        .dir.display(), .snapshot.display()
    )]
    NoSnapshot { dir: PathBuf, snapshot: PathBuf },
    #[error("the import snapshot {} cannot be used: {message}", .path.display())]
    Snapshot { path: PathBuf, message: String },
    #[error("{0}")]
    Collection(String),
}

/// One change to the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Write { path: PathBuf, text: String },
    Move { from: PathBuf, to: PathBuf },
}

/// Everything a sync will do.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncPlan {
    pub actions: Vec<Action>,
    pub summary: SyncSummary,
    /// Problems that did not stop the sync, such as a request file that
    /// does not load.
    pub warnings: Vec<String>,
    /// The new document's generation warnings.
    pub generated: Generated,
}

/// Plan the sync of the collection at `target` with `imported`.
pub fn plan(imported: &Imported, target: &Target) -> Result<SyncPlan, SyncError> {
    let base = generate_base(imported, target)?;
    let remote = store::generate_for(imported, target);
    let mut planner = Planner::new(target);
    let local = local::scan(&target.dir, &mut planner.warnings);
    planner.taken = local.iter().map(|l| l.path.clone()).collect();
    planner.operations(&base, &remote, &local);
    planner.removed(&remote, &local);
    planner.root(&base, &remote)?;
    planner.environments(&base, &remote);
    let snapshot = target.snapshot_path();
    let text = snapshot_text(&imported.document);
    if std::fs::read_to_string(&snapshot).ok().as_deref() != Some(text.as_str()) {
        planner.actions.push(Action::Write {
            path: snapshot,
            text,
        });
    }
    Ok(SyncPlan {
        actions: planner.actions,
        summary: planner.summary,
        warnings: planner.warnings,
        generated: remote,
    })
}

/// Perform `plan`'s actions in order.
pub fn apply(plan: &SyncPlan) -> Result<(), WriteError> {
    for action in &plan.actions {
        match action {
            Action::Write { path, text } => store::write_file(path, text)?,
            Action::Move { from, to } => move_file(from, to)?,
        }
    }
    Ok(())
}

fn move_file(from: &Path, to: &Path) -> Result<(), WriteError> {
    let io = |source| WriteError::Io {
        path: to.to_path_buf(),
        source,
    };
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    std::fs::rename(from, to).map_err(io)
}

/// The collection generated from the snapshot: the merge base.
fn generate_base(imported: &Imported, target: &Target) -> Result<Generated, SyncError> {
    let path = target.snapshot_path();
    let text = std::fs::read_to_string(&path).map_err(|_| SyncError::NoSnapshot {
        dir: target.dir.clone(),
        snapshot: path.clone(),
    })?;
    let failed = |message: String| SyncError::Snapshot {
        path: path.clone(),
        message,
    };
    let document: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| failed(e.to_string()))?;
    let version = document::detect_version(&document, &path.to_string_lossy())
        .map_err(|e| failed(e.to_string()))?;
    let (model, _) = check_and_lower(&document, version).map_err(|e| failed(e.to_string()))?;
    let options = GenerateOptions {
        source: None,
        location: Some(&imported.location),
    };
    Ok(generate_with(&model, &options))
}

struct Planner<'a> {
    target: &'a Target,
    actions: Vec<Action>,
    summary: SyncSummary,
    warnings: Vec<String>,
    /// Request file paths in use, relative to the collection.
    taken: Vec<String>,
    /// Stems added to existing folders, for their `order`.
    added_stems: Vec<(String, String)>,
    /// Stems moved out of their folders, taken off their `order`.
    removed_stems: Vec<(String, String)>,
    /// Folders the sync creates.
    new_folders: Vec<String>,
}

fn key_of(op: &GeneratedRequest) -> String {
    operation_key(&op.request).unwrap_or_default()
}

impl<'a> Planner<'a> {
    fn new(target: &'a Target) -> Self {
        Self {
            target,
            actions: Vec::new(),
            summary: SyncSummary::default(),
            warnings: Vec::new(),
            taken: Vec::new(),
            added_stems: Vec::new(),
            removed_stems: Vec::new(),
            new_folders: Vec::new(),
        }
    }

    fn write(&mut self, path: PathBuf, text: String) {
        self.actions.push(Action::Write { path, text });
    }

    fn operations(&mut self, base: &Generated, remote: &Generated, local: &[LocalRequest]) {
        for op in &remote.operations {
            let key = key_of(op);
            let base_op = base.operations.iter().find(|b| key_of(b) == key);
            match local.iter().find(|l| l.key == key) {
                Some(l) => self.merge(base_op, l, op),
                None => self.add(&key, op, remote),
            }
        }
    }

    fn merge(
        &mut self,
        base: Option<&GeneratedRequest>,
        local: &LocalRequest,
        remote: &GeneratedRequest,
    ) {
        let merged = merge_request(base.map(|b| &b.request), &local.request, &remote.request);
        if merged.request != local.request {
            let yaml = request_to_yaml(&Request::Http(Box::new(merged.request.clone())));
            self.write(join(&self.target.dir, &local.path), yaml);
        }
        if merged.updated.is_empty() && merged.kept_local.is_empty() {
            self.summary.unchanged += 1;
        }
        if !merged.updated.is_empty() {
            self.summary.changed.push(ChangedOperation {
                operation_id: local.key.clone(),
                file: local.path.clone(),
                fields: merged.updated.iter().map(|f| (*f).to_owned()).collect(),
            });
        }
        for field in merged.kept_local {
            self.summary.conflicts.push(Conflict {
                operation_id: local.key.clone(),
                file: local.path.clone(),
                field: field.to_owned(),
                resolution: KEPT_LOCAL.to_owned(),
            });
        }
    }

    fn add(&mut self, key: &str, op: &GeneratedRequest, remote: &Generated) {
        let path = self.free_path(&op.path);
        let (folder, file) = path.split_once('/').unwrap_or(("", path.as_str()));
        let stem = file
            .strip_suffix(REQUEST_FILE_SUFFIX)
            .unwrap_or(file)
            .to_owned();
        let folder = folder.to_owned();
        if !folder.is_empty() && !self.target.dir.join(&folder).exists() {
            if !self.new_folders.contains(&folder) {
                self.new_folders.push(folder.clone());
                self.add_folder_meta(&folder, remote);
            }
        } else if !folder.is_empty() {
            self.added_stems.push((folder, stem));
        }
        let yaml = request_to_yaml(&Request::Http(Box::new(op.request.clone())));
        self.write(join(&self.target.dir, &path), yaml);
        self.summary.added.push(OperationFile {
            operation_id: key.to_owned(),
            file: path,
        });
    }

    fn add_folder_meta(&mut self, folder: &str, remote: &Generated) {
        let meta_path = format!("{folder}/{COLLECTION_FILE}");
        if let Some(file) = remote.files.iter().find(|f| f.path == meta_path) {
            let text = file.contents.clone();
            self.write(join(&self.target.dir, &meta_path), text);
        }
    }

    /// `path`, or `path` with `-2`, `-3`, ... before the suffix, whichever
    /// is not used yet.
    fn free_path(&mut self, path: &str) -> String {
        let stem = path.strip_suffix(REQUEST_FILE_SUFFIX).unwrap_or(path);
        let mut candidate = path.to_owned();
        let mut n = 1;
        while self.taken.contains(&candidate) || join(&self.target.dir, &candidate).exists() {
            n += 1;
            candidate = format!("{stem}-{n}{REQUEST_FILE_SUFFIX}");
        }
        self.taken.push(candidate.clone());
        candidate
    }

    /// Move the request files of operations the new document no longer has
    /// to `_removed/`.
    fn removed(&mut self, remote: &Generated, local: &[LocalRequest]) {
        let mut moved: Vec<String> = Vec::new();
        for file in local {
            if remote.operations.iter().any(|op| key_of(op) == file.key) {
                continue;
            }
            let name = file.path.rsplit('/').next().unwrap_or(&file.path);
            let stem = name.strip_suffix(REQUEST_FILE_SUFFIX).unwrap_or(name);
            let mut to = format!("{REMOVED_DIR}/{name}");
            let mut n = 1;
            while moved.contains(&to) || join(&self.target.dir, &to).exists() {
                n += 1;
                to = format!("{REMOVED_DIR}/{stem}-{n}{REQUEST_FILE_SUFFIX}");
            }
            moved.push(to.clone());
            if let Some((folder, _)) = file.path.rsplit_once('/') {
                self.removed_stems
                    .push((folder.to_owned(), stem.to_owned()));
            }
            self.actions.push(Action::Move {
                from: join(&self.target.dir, &file.path),
                to: join(&self.target.dir, &to),
            });
            self.summary.removed.push(OperationFile {
                operation_id: file.key.clone(),
                file: to,
            });
        }
    }

    /// The root and folder `_collection.yaml`s: the new source, merged
    /// auth and description, and the new folders and requests in `order`.
    fn root(&mut self, base: &Generated, remote: &Generated) -> Result<(), SyncError> {
        let root = self.read_meta("")?;
        let mut merged = root.clone();
        merged.source.clone_from(&remote.root.source);
        merged.auth = self.merge_meta(
            "auth",
            base.root.auth.as_ref(),
            &root.auth,
            &remote.root.auth,
        );
        merged.description = self.merge_meta(
            "description",
            base.root.description.as_ref(),
            &root.description,
            &remote.root.description,
        );
        if let Some(order) = merged.order.as_mut() {
            order.extend(self.new_folders.iter().cloned());
        }
        self.write_meta("", &root, &merged);
        self.folder_orders()
    }

    /// Add new request stems to, and take removed ones off, the `order`
    /// of the folders they are in.
    fn folder_orders(&mut self) -> Result<(), SyncError> {
        let mut folders: Vec<String> = Vec::new();
        for (folder, _) in self.added_stems.iter().chain(&self.removed_stems) {
            if !folders.contains(folder) {
                folders.push(folder.clone());
            }
        }
        for folder in folders {
            let meta = self.read_meta(&folder)?;
            let mut updated = meta.clone();
            if let Some(order) = updated.order.as_mut() {
                let in_folder = |list: &[(String, String)]| -> Vec<String> {
                    list.iter()
                        .filter(|(f, _)| *f == folder)
                        .map(|(_, s)| s.clone())
                        .collect()
                };
                let removed = in_folder(&self.removed_stems);
                order.retain(|stem| !removed.contains(stem));
                order.extend(in_folder(&self.added_stems));
            }
            self.write_meta(&folder, &meta, &updated);
        }
        Ok(())
    }

    fn merge_meta<T: Clone + PartialEq>(
        &mut self,
        field: &str,
        base: Option<&T>,
        local: &Option<T>,
        remote: &Option<T>,
    ) -> Option<T> {
        let base = Some(base.cloned());
        let (value, outcome) = merge_value(base.as_ref(), local, remote);
        if outcome == merge::Outcome::KeptLocal {
            self.warnings.push(format!(
                "the collection's {field} changed in the document and in {COLLECTION_FILE}; kept local"
            ));
        }
        value
    }

    fn read_meta(&self, folder: &str) -> Result<CollectionMeta, SyncError> {
        let rel = if folder.is_empty() {
            COLLECTION_FILE.to_owned()
        } else {
            format!("{folder}/{COLLECTION_FILE}")
        };
        let Ok(text) = std::fs::read_to_string(join(&self.target.dir, &rel)) else {
            return Ok(CollectionMeta::default());
        };
        from_yaml(Path::new(&rel), &text).map_err(|e| SyncError::Collection(e.to_string()))
    }

    fn write_meta(&mut self, folder: &str, before: &CollectionMeta, after: &CollectionMeta) {
        if before != after {
            let path = join(&self.target.dir, folder).join(COLLECTION_FILE);
            self.write(path, collection_to_yaml(after));
        }
    }

    /// Create missing environments; merge existing ones variable by
    /// variable, keeping the user's values and additions.
    fn environments(&mut self, base: &Generated, remote: &Generated) {
        for env in &remote.environments {
            let base_env = base.environments.iter().find(|b| b.name == env.name);
            let change = match store::read_environment(self.target, &env.name) {
                None => Some((env.clone(), "created")),
                Some(Ok(local)) => merge_environment(base_env, &local, env).map(|e| (e, "updated")),
                Some(Err(message)) => {
                    self.warnings
                        .push(format!("{message}; the environment is left as it is"));
                    None
                }
            };
            if let Some((merged, change)) = change {
                self.write(
                    self.target.environment_path(&env.name),
                    environment_to_yaml(&merged),
                );
                self.summary.environments.push(EnvironmentChange {
                    name: env.name.clone(),
                    change: change.to_owned(),
                });
            }
        }
    }
}

/// `local` with the remote variables and secrets merged in, or `None` when
/// nothing changes.
fn merge_environment(
    base: Option<&Environment>,
    local: &Environment,
    remote: &Environment,
) -> Option<Environment> {
    let entries = |e: &Environment| -> Vec<(String, VarValue)> {
        e.variables
            .iter()
            .map(|(n, v)| (n.to_owned(), v.clone()))
            .collect()
    };
    let base_vars = base.map(entries).unwrap_or_default();
    let (variables, _) = merge_rows(&base_vars, &entries(local), &entries(remote), |r| {
        r.0.clone()
    });
    let base_secrets = base.map(|b| b.secrets.clone()).unwrap_or_default();
    let (secrets, _) = merge_rows(&base_secrets, &local.secrets, &remote.secrets, Clone::clone);
    let merged = Environment {
        name: local.name.clone(),
        variables: Variables::new(variables),
        secrets,
        production: local.production,
        trace: Default::default(),
        chain_id: None,
    };
    (merged != *local).then_some(merged)
}
