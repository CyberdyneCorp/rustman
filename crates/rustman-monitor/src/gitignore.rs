//! The store must stay out of Git (monitors "Store not ignored"): warn when
//! `.rustman/` is not git-ignored.

use std::path::Path;
use std::process::{Command, Stdio};

use rustman_core::init::ignores_rustman_dir;

/// The warning printed when a result is stored in a store Git would track.
pub const NOT_IGNORED: &str = ".rustman/ is not git-ignored";

/// [`NOT_IGNORED`] when the store of the workspace at `root` is not
/// ignored. Git decides when the workspace is in a repository (any
/// `.gitignore` level, `info/exclude`, global excludes); otherwise the
/// workspace's own `.gitignore` must list `.rustman/`.
pub fn warning(root: &Path) -> Option<&'static str> {
    let ignored = git_ignores(root).unwrap_or_else(|| gitignore_lists(root));
    (!ignored).then_some(NOT_IGNORED)
}

/// What `git check-ignore` says about the store, or `None` without Git or
/// outside a repository.
fn git_ignores(root: &Path) -> Option<bool> {
    let status = Command::new("git")
        .args(["check-ignore", "-q", ".rustman/monitors/"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn gitignore_lists(root: &Path) -> bool {
    std::fs::read_to_string(root.join(".gitignore")).is_ok_and(|text| ignores_rustman_dir(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_without_the_entry_is_warned_about() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(warning(tmp.path()), Some(NOT_IGNORED));
        std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();
        assert_eq!(warning(tmp.path()), Some(NOT_IGNORED));
        std::fs::write(tmp.path().join(".gitignore"), "target/\n.rustman/\n").unwrap();
        assert_eq!(warning(tmp.path()), None);
    }

    /// Inside a repository Git decides, whichever file ignores the store.
    #[test]
    fn git_decides_inside_a_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if !git(&["init", "-q"]) {
            return; // No git on this machine: the fallback test covers it.
        }
        assert_eq!(git_ignores(tmp.path()), Some(false));
        assert_eq!(warning(tmp.path()), Some(NOT_IGNORED));
        std::fs::create_dir_all(tmp.path().join(".git/info")).unwrap();
        std::fs::write(tmp.path().join(".git/info/exclude"), ".rustman\n").unwrap();
        assert_eq!(git_ignores(tmp.path()), Some(true));
        assert_eq!(warning(tmp.path()), None);
    }
}
