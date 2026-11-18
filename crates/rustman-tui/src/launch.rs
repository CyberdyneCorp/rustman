//! Finding the workspace the TUI opens.

use std::path::{Path, PathBuf};

use rustman_core::discovery::find_root;

/// Where the TUI starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    /// `rustman.toml` was found in this directory (the workspace root).
    Workspace(PathBuf),
    /// No `rustman.toml` in the start directory or any ancestor; the user is
    /// offered to create one in this directory or open a scratch session.
    NoWorkspace(PathBuf),
}

/// Search `start` and its ancestors for `rustman.toml`. `start` is made
/// absolute and canonical first, so `.` reaches the real parents and `..`
/// cannot open a workspace that is not `start` or one of its ancestors.
pub fn discover(start: &Path) -> Launch {
    let start = dunce::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    find_root(&start, None).map_or_else(|_| Launch::NoWorkspace(start), Launch::Workspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_subdirectory_opens_the_enclosing_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let nested = root.join("api/users");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("rustman.toml"), "").unwrap();
        let root = dunce::canonicalize(root).unwrap();
        assert_eq!(discover(&nested), Launch::Workspace(root));
    }

    #[test]
    fn dot_dot_is_resolved_before_searching() {
        // rustman.toml only in outer/inner; `outer/inner/..` is `outer`,
        // whose ancestors do not include `inner`.
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("rustman.toml"), "").unwrap();
        let outer = dunce::canonicalize(outer).unwrap();
        assert_eq!(discover(&inner.join("..")), Launch::NoWorkspace(outer));
    }

    #[test]
    fn a_relative_start_reaches_the_real_parents() {
        let cwd = dunce::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let found = match discover(Path::new(".")) {
            Launch::Workspace(dir) | Launch::NoWorkspace(dir) => dir,
        };
        assert!(found.is_absolute(), "{}", found.display());
        assert!(cwd.starts_with(&found), "{}", found.display());
    }

    #[test]
    fn no_workspace_anywhere_offers_the_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let start = tmp.path().join("empty");
        fs::create_dir_all(&start).unwrap();
        let canonical = dunce::canonicalize(&start).unwrap();
        assert_eq!(discover(&start), Launch::NoWorkspace(canonical));
    }
}
