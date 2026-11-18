//! Carrying out a [`Plan`] all or nothing: the files are first written
//! next to their targets as temporary files, then the secrets are stored,
//! then the temporary files are moved into place. When staging or storing
//! fails, the temporary files and the folders made for them are removed,
//! so a failed import leaves the workspace as it was. A dry run never
//! calls this.

use std::path::{Path, PathBuf};

use rustman_core::{Secret, SecretError};

use crate::error::ImportError;
use crate::plan::Plan;

/// Stores secret values, each for its environment, in one batch (the
/// CLI's [`rustman_core::SecretStore::set_many`]), so storing thousands
/// stays linear.
pub type StoreSecret<'a> = &'a dyn Fn(&[(&str, &Secret<String>)]) -> Result<(), SecretError>;

/// The suffix of a staged file, next to its target.
const STAGED: &str = ".rmtmp";

/// Write the plan's files under `root` and store its secrets with `store`.
pub fn apply(plan: &Plan, root: &Path, store: StoreSecret<'_>) -> Result<(), ImportError> {
    let mut staging = Staging::default();
    let staged = staging.stage(plan, root).and_then(|()| {
        let batch: Vec<(&str, &Secret<String>)> = plan
            .secrets
            .iter()
            .map(|moved| (moved.environment.as_str(), &moved.secret))
            .collect();
        if batch.is_empty() {
            return Ok(());
        }
        store(&batch).map_err(ImportError::from)
    });
    if let Err(e) = staged {
        staging.undo();
        return Err(e);
    }
    staging.commit()
}

/// Files written beside their targets, and the folders made for them.
#[derive(Default)]
struct Staging {
    /// (staged file, target).
    files: Vec<(PathBuf, PathBuf)>,
    /// Folders that did not exist, outermost first.
    dirs: Vec<PathBuf>,
}

impl Staging {
    fn stage(&mut self, plan: &Plan, root: &Path) -> Result<(), ImportError> {
        for file in plan.files.iter().filter(|f| f.action.writes()) {
            let target = native(root, &file.path);
            if let Some(parent) = target.parent() {
                self.make_dirs(parent)?;
            }
            let mut name = target.clone().into_os_string();
            name.push(STAGED);
            let staged = PathBuf::from(name);
            std::fs::write(&staged, &file.text).map_err(|e| ImportError::io(&target, e))?;
            self.files.push((staged, target));
        }
        Ok(())
    }

    /// Create `dir` and its missing parents, remembering each one made.
    fn make_dirs(&mut self, dir: &Path) -> Result<(), ImportError> {
        let missing: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|d| !d.as_os_str().is_empty() && !d.exists())
            .map(Path::to_path_buf)
            .collect();
        for d in missing.into_iter().rev() {
            std::fs::create_dir(&d).map_err(|e| ImportError::io(&d, e))?;
            self.dirs.push(d);
        }
        Ok(())
    }

    /// Remove the staged files and the folders made for them.
    fn undo(self) {
        for (staged, _) in &self.files {
            let _ = std::fs::remove_file(staged);
        }
        for dir in self.dirs.iter().rev() {
            let _ = std::fs::remove_dir(dir);
        }
    }

    fn commit(self) -> Result<(), ImportError> {
        for (i, (staged, target)) in self.files.iter().enumerate() {
            if let Err(e) = std::fs::rename(staged, target) {
                for (left, _) in &self.files[i..] {
                    let _ = std::fs::remove_file(left);
                }
                return Err(ImportError::io(target, e));
            }
        }
        Ok(())
    }
}

/// `root` joined with a `/`-separated relative path.
fn native(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}
