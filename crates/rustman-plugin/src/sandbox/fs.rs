//! `fs-read`: workspace files under a granted folder (design D3). Paths
//! are resolved lexically first, then canonically, so neither `..` nor a
//! symbolic link can reach outside the granted folder, outside the
//! workspace, or into `.rustman/`. The granted folder itself is
//! canonicalized too and must stay inside the workspace: a committed
//! symlink `data -> /home/me` does not turn `fs:read:data` into a grant
//! on the home folder.

use std::path::{Path, PathBuf};

use rustman_core::secrets::STATE_DIR;

use crate::grant::{Grant, normalize_relative};

/// Why a path was refused or could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum FsRefusal {
    /// No `fs:read` grant covers the path: `capability_denied: fs <path>`.
    Denied,
    /// The path is allowed but cannot be read.
    Failed(String),
}

/// The file or folder `path` names, when a grant covers it.
pub fn resolve(root: &Path, grants: &[Grant], path: &str) -> Result<PathBuf, FsRefusal> {
    let parts = normalize_relative(path).map_err(|_| FsRefusal::Denied)?;
    if parts
        .first()
        .is_some_and(|first| first.eq_ignore_ascii_case(STATE_DIR))
    {
        return Err(FsRefusal::Denied);
    }
    let grant = grants
        .iter()
        .filter_map(|grant| match grant {
            Grant::FsRead(dir) => Some(dir.as_str()),
            _ => None,
        })
        .find(|dir| covers(dir, &parts))
        .ok_or(FsRefusal::Denied)?;
    let canonical = |p: &Path| dunce::canonicalize(p).map_err(|e| FsRefusal::Failed(e.to_string()));
    let root = canonical(root)?;
    let target = canonical(&root.join(parts.join("/")))?;
    let granted = canonical(&root.join(grant))?;
    let state = dunce::canonicalize(root.join(STATE_DIR)).unwrap_or_else(|_| root.join(STATE_DIR));
    if !contained(&root, &state, &granted) || !contained(&granted, &state, &target) {
        return Err(FsRefusal::Denied);
    }
    Ok(target)
}

/// Whether the canonical `path` lies under the canonical `folder` and not
/// in the workspace state folder `state`.
fn contained(folder: &Path, state: &Path, path: &Path) -> bool {
    path.starts_with(folder) && !path.starts_with(state)
}

/// Whether the folder `dir` (normalized, empty for the root) holds `parts`.
fn covers(dir: &str, parts: &[&str]) -> bool {
    let dir: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    parts.len() >= dir.len() && parts[..dir.len()] == dir[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(dirs: &[&str]) -> Vec<Grant> {
        dirs.iter()
            .map(|d| Grant::FsRead((*d).to_owned()))
            .collect()
    }

    #[test]
    fn granted_folders_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("data/in")).unwrap();
        std::fs::create_dir_all(root.join(".rustman")).unwrap();
        std::fs::write(root.join("data/in/a.txt"), "a").unwrap();
        std::fs::write(root.join("secret.txt"), "s").unwrap();
        std::fs::write(root.join(".rustman/secrets.yaml"), "s").unwrap();
        let data = grants(&["data"]);
        assert!(resolve(root, &data, "data/in/a.txt").is_ok());
        assert!(resolve(root, &data, "./data/x/../in/a.txt").is_ok());
        for refused in [
            "secret.txt",
            "data/../secret.txt",
            "../x",
            "/etc/passwd",
            "C:/x",
        ] {
            assert_eq!(
                resolve(root, &data, refused),
                Err(FsRefusal::Denied),
                "{refused}"
            );
        }
        let everything = grants(&[""]);
        assert!(resolve(root, &everything, "secret.txt").is_ok());
        assert_eq!(
            resolve(root, &everything, ".rustman/secrets.yaml"),
            Err(FsRefusal::Denied)
        );
        assert_eq!(
            resolve(root, &[Grant::Clock], "data/in/a.txt"),
            Err(FsRefusal::Denied)
        );
        assert!(matches!(
            resolve(root, &data, "data/missing.txt"),
            Err(FsRefusal::Failed(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_the_grant_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join(".rustman")).unwrap();
        std::fs::write(outside.path().join("passwd"), "x").unwrap();
        std::fs::write(root.join("top.txt"), "x").unwrap();
        std::fs::write(root.join(".rustman/trust.yaml"), "x").unwrap();
        let link = |target: &Path, name: &str| {
            std::os::unix::fs::symlink(target, root.join("data").join(name)).unwrap();
        };
        link(&outside.path().join("passwd"), "out");
        link(&root.join("top.txt"), "up");
        link(&root.join(".rustman/trust.yaml"), "state");
        let data = grants(&["data"]);
        for name in ["data/out", "data/up", "data/state"] {
            assert_eq!(resolve(root, &data, name), Err(FsRefusal::Denied), "{name}");
        }
    }

    /// A symlink, or `false` where the platform refuses to create one
    /// (Windows without the privilege).
    fn symlink_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(target, link);
        made.is_ok()
    }

    /// A granted folder that is itself a symlink out of the workspace
    /// grants nothing outside it.
    #[test]
    fn a_granted_folder_symlinked_out_of_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(outside.path().join("private.txt"), "x").unwrap();
        std::fs::create_dir_all(root.join(".rustman")).unwrap();
        std::fs::write(root.join(".rustman/secrets.yaml"), "x").unwrap();
        if !symlink_dir(outside.path(), &root.join("data"))
            || !symlink_dir(&root.join(".rustman"), &root.join("state"))
        {
            eprintln!("skipped: cannot create symlinks here");
            return;
        }
        assert_eq!(
            resolve(root, &grants(&["data"]), "data/private.txt"),
            Err(FsRefusal::Denied)
        );
        assert_eq!(
            resolve(root, &grants(&["data"]), "data"),
            Err(FsRefusal::Denied)
        );
        assert_eq!(
            resolve(root, &grants(&["state"]), "state/secrets.yaml"),
            Err(FsRefusal::Denied)
        );
    }
}
