//! `rustman contract diff --base <ref>` (design D3): the old document and
//! the relative `$ref` files it uses are read from a Git tree by running
//! `git cat-file -s <ref>:<path>` (the size, checked against the limit
//! before anything is read) and `git cat-file blob <ref>:<path>`, through the importer's pluggable
//! [`FileSource`], so bundling resolves references at the same ref.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use rustman_explore::openapi::source::FileSource;
use rustman_explore::{ImportError, ImportOptions};

use crate::spec::Spec;

/// Reads files at one Git ref of the repository containing a folder.
#[derive(Clone, Debug)]
pub struct GitTree {
    /// The repository's top-level folder, canonical.
    root: PathBuf,
    /// The folder relative paths are joined to, canonical.
    cwd: PathBuf,
    reference: String,
}

impl GitTree {
    /// The tree of `reference` in the repository that contains `cwd`. Fails
    /// when `git` cannot run, `cwd` is not in a repository or the ref does
    /// not name a commit.
    pub fn open(reference: &str, cwd: &Path) -> Result<GitTree, ImportError> {
        if reference.is_empty() || reference.starts_with('-') {
            return Err(source_error(format!("invalid git ref '{reference}'")));
        }
        let cwd = canonical(cwd);
        let top = git(&cwd, &["rev-parse", "--show-toplevel"])
            .map_err(|e| source_error(format!("--base needs a git repository: {e}")))?;
        let root = canonical(Path::new(String::from_utf8_lossy(&top).trim()));
        let commit = format!("{reference}^{{commit}}");
        git(&root, &["rev-parse", "--verify", "--quiet", &commit]).map_err(|_| {
            source_error(format!(
                "unknown git ref '{reference}' (in a shallow clone, fetch it first, \
                 as in git fetch origin main --depth=1)"
            ))
        })?;
        Ok(GitTree {
            root,
            cwd,
            reference: reference.to_owned(),
        })
    }

    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// `path` relative to the repository root, with `/`, as `git cat-file`
    /// takes it.
    pub fn repo_path(&self, path: &Path) -> Result<String, ImportError> {
        let full = normalize(&self.cwd.join(path));
        let relative = full.strip_prefix(&self.root).map_err(|_| {
            source_error(format!("{} is outside the git repository", path.display()))
        })?;
        let parts: Vec<String> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        Ok(parts.join("/"))
    }

    /// Load the document at `path` from this tree, as `openapi-import`
    /// loads a file; it is named `<ref>:<path>`.
    pub async fn load(&self, path: &str, options: &ImportOptions) -> Result<Spec, ImportError> {
        let mut options = options.clone();
        options.fetch.files = Some(std::sync::Arc::new(self.clone()));
        let mut spec = Spec::load(path, &options).await?;
        spec.name = format!("{}:{}", self.reference, self.repo_path(Path::new(path))?);
        Ok(spec)
    }
}

impl FileSource for GitTree {
    fn read(&self, path: &Path, max_bytes: u64) -> Result<String, ImportError> {
        let shown = self.repo_path(path)?;
        let object = format!("{}:{shown}", self.reference);
        let not_found = |_| source_error(format!("{shown} not found at {}", self.reference));
        let size = git(&self.root, &["cat-file", "-s", &object]).map_err(not_found)?;
        let size: u64 = String::from_utf8_lossy(&size)
            .trim()
            .parse()
            .unwrap_or(u64::MAX);
        if size > max_bytes {
            return Err(ImportError::TooLarge {
                name: object,
                limit: max_bytes,
            });
        }
        let bytes = git(&self.root, &["cat-file", "blob", &object]).map_err(not_found)?;
        String::from_utf8(bytes)
            .map_err(|_| source_error(format!("{object} is not valid UTF-8 text")))
    }
}

fn source_error(message: String) -> ImportError {
    ImportError::Source(message)
}

/// Run `git <args>` in `dir`; its stdout, or its stderr as the error.
fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `path` with `.` and `a/..` segments removed, without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_that_look_like_options_are_refused() {
        let dir = std::env::temp_dir();
        for bad in ["", "--output=x", "-p"] {
            let err = GitTree::open(bad, &dir).unwrap_err();
            assert!(err.to_string().contains("invalid git ref"), "{err}");
        }
    }

    /// Run git in `dir`, failing the test on error.
    fn run(dir: &Path, args: &[&str]) {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    }

    #[test]
    fn a_blob_over_the_limit_is_refused_by_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(root, &["init", "--quiet"]);
        std::fs::write(root.join("openapi.yaml"), "0123456789").unwrap();
        run(root, &["add", "openapi.yaml"]);
        let who = [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ];
        run(
            root,
            &[&who[..], &["commit", "--quiet", "-m", "c"]].concat(),
        );
        let tree = GitTree::open("HEAD", root).unwrap();
        let path = Path::new("openapi.yaml");
        assert_eq!(tree.read(path, 10).unwrap(), "0123456789");
        let err = tree.read(path, 9).unwrap_err();
        assert!(
            matches!(err, ImportError::TooLarge { limit: 9, .. }),
            "{err}"
        );
        let missing = tree.read(Path::new("none.yaml"), 10).unwrap_err();
        assert!(
            missing.to_string().contains("none.yaml not found at HEAD"),
            "{missing}"
        );
    }

    #[test]
    fn paths_are_relative_to_the_repository_with_slashes() {
        let root = canonical(&std::env::temp_dir());
        let tree = GitTree {
            root: root.clone(),
            cwd: root.join("specs"),
            reference: "main".into(),
        };
        let nested = Path::new("api").join("..").join("common").join("pet.yaml");
        assert_eq!(tree.repo_path(&nested).unwrap(), "specs/common/pet.yaml");
        assert_eq!(
            tree.repo_path(&root.join("openapi.yaml")).unwrap(),
            "openapi.yaml"
        );
        let outside = Path::new("..").join("..").join("x.yaml");
        assert!(tree.repo_path(&outside).is_err());
    }
}
