//! Keeping file references inside the workspace.

use std::io;
use std::path::{Component, Path, PathBuf};

/// A file reference that resolves outside the workspace root.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("file path escapes workspace: {0}")]
pub struct PathEscape(pub String);

/// Resolve `raw` relative to `base_dir` and check it stays under `root`.
///
/// The check is done twice: lexically (so `../..` cannot climb out) and on the
/// real filesystem for the part of the path that exists (so a symlink cannot
/// point out). A path that does not exist yet passes when its existing
/// ancestors are inside the workspace.
pub fn contain(root: &Path, base_dir: &Path, raw: &str) -> Result<PathBuf, PathEscape> {
    let escape = || PathEscape(raw.to_owned());
    let joined = normalize(&base_dir.join(raw)).ok_or_else(escape)?;
    let lexical_root = normalize(root).ok_or_else(escape)?;
    if !joined.starts_with(&lexical_root) {
        return Err(escape());
    }
    let Ok(real_root) = dunce::canonicalize(root) else {
        return Ok(joined);
    };
    match canonical_existing(&joined) {
        Ok(real) if real.starts_with(&real_root) => Ok(joined),
        _ => Err(escape()),
    }
}

/// Remove `.` and resolve `..` without touching the filesystem.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other),
        }
    }
    Some(out)
}

/// Canonicalize the longest existing prefix of `path`, then re-append the rest.
/// Fails on a dangling symlink, whose target cannot be checked.
fn canonical_existing(path: &Path) -> io::Result<PathBuf> {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if existing.symlink_metadata().is_ok() {
            let mut real = dunce::canonicalize(existing)?;
            real.extend(rest.iter().rev());
            return Ok(real);
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return Ok(path.to_path_buf());
        };
        rest.push(name);
        existing = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn relative_paths_inside_the_workspace_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let base = root.join("users");
        fs::create_dir_all(&base).unwrap();
        assert_eq!(
            contain(root, &base, "./fixtures/a.png").unwrap(),
            base.join("fixtures/a.png")
        );
        assert_eq!(
            contain(root, &base, "../certs/ca.pem").unwrap(),
            root.join("certs/ca.pem")
        );
    }

    #[test]
    fn parent_traversal_out_of_the_workspace_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        let base = root.join("users");
        fs::create_dir_all(&base).unwrap();
        let err = contain(&root, &base, "../../etc/passwd").unwrap_err();
        assert_eq!(
            err.to_string(),
            "file path escapes workspace: ../../etc/passwd"
        );
        assert!(contain(&root, &root, "/etc/passwd").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_pointing_outside_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "x").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("dir")).unwrap();
        assert!(contain(&root, &root, "link.txt").is_err());
        assert!(contain(&root, &root, "dir/secret.txt").is_err());
        assert!(contain(&root, &root, "dir/missing.txt").is_err());
        std::os::unix::fs::symlink(tmp.path().join("gone"), root.join("dangling")).unwrap();
        assert!(contain(&root, &root, "dangling").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_inside_the_workspace_are_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("real.txt"), "x").unwrap();
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();
        assert!(contain(root, root, "link.txt").is_ok());
    }
}
