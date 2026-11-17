//! Guards run before anything is written under `.rustman/`: the folder must
//! be a real folder of the workspace, and Git must ignore it (environments spec
//! "Secrets file is git-ignored").

use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, Stdio};

use super::{STATE_DIR, SecretError};

/// Fail unless `rel` (a path under `.rustman/`, relative to `root`) can hold
/// private state: `.rustman` and `rel` are not symbolic links, and Git
/// ignores `rel`.
pub fn ensure_private(root: &Path, rel: &str) -> Result<(), SecretError> {
    ensure_state_dir(root)?;
    ensure_not_symlink(root, rel)?;
    ensure_ignored(root, rel)
}

/// Fail when `.rustman` exists but is a symbolic link or not a folder, so a
/// write can never land outside the workspace or in a folder Git tracks.
pub fn ensure_state_dir(root: &Path) -> Result<(), SecretError> {
    let path = root.join(STATE_DIR);
    match path.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(SecretError::UnsafeStateDir("is a symbolic link".to_owned()))
        }
        Ok(meta) if !meta.is_dir() => {
            Err(SecretError::UnsafeStateDir("is not a folder".to_owned()))
        }
        Err(e) if e.kind() != ErrorKind::NotFound => Err(SecretError::io(&path, e)),
        _ => Ok(()),
    }
}

fn ensure_not_symlink(root: &Path, rel: &str) -> Result<(), SecretError> {
    match root.join(rel).symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => Err(SecretError::UnsafeStateDir(format!(
            "holds a symbolic link: {rel}"
        ))),
        _ => Ok(()),
    }
}

/// Fail unless Git would ignore `rel` (a path relative to `root`).
///
/// `git check-ignore` exits 0 when the path is ignored and 1 when it is not
/// (a tracked file counts as not ignored). Outside a work tree, or without a
/// usable `git`, the check passes; inside one, any other outcome is a failure.
pub fn ensure_ignored(root: &Path, rel: &str) -> Result<(), SecretError> {
    match git_status(root, &["check-ignore", "-q", "--", rel]) {
        Some(0) => Ok(()),
        Some(1) => Err(SecretError::NotIgnored),
        _ if inside_work_tree(root) => Err(SecretError::GitCheckFailed),
        _ => Ok(()),
    }
}

fn inside_work_tree(root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success() && out.stdout.trim_ascii() == b"true")
}

fn git_status(root: &Path, args: &[&str]) -> Option<i32> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .and_then(|status| status.code())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    /// Run `git` in `dir`, panicking on failure.
    pub(crate) fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git is installed")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    const FILE: &str = ".rustman/secrets.yaml";

    #[test]
    fn outside_a_repository_the_check_passes() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure_ignored(dir.path(), FILE).is_ok());
    }

    #[test]
    fn ignored_path_passes() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join(".gitignore"), "target/\n.rustman/\n").unwrap();
        assert!(ensure_ignored(dir.path(), FILE).is_ok());
    }

    #[test]
    fn not_ignored_path_is_refused_with_the_spec_text() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        let err = ensure_ignored(dir.path(), FILE).unwrap_err();
        assert!(matches!(err, SecretError::NotIgnored));
        assert_eq!(
            err.to_string(),
            "refusing to write secrets: .rustman/ is not git-ignored"
        );
    }

    #[test]
    fn workspace_in_a_subfolder_of_the_repository_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        let ws = dir.path().join("api");
        fs::create_dir_all(&ws).unwrap();
        assert!(ensure_ignored(&ws, FILE).is_err());
        fs::write(ws.join(".gitignore"), ".rustman/\n").unwrap();
        assert!(ensure_ignored(&ws, FILE).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_state_dir_is_refused_inside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join(".gitignore"), ".rustman/\n").unwrap();
        fs::create_dir_all(dir.path().join("public")).unwrap();
        std::os::unix::fs::symlink("public", dir.path().join(".rustman")).unwrap();
        // Git cannot answer for a path beyond a symlink: that fails closed.
        assert!(matches!(
            ensure_ignored(dir.path(), FILE),
            Err(SecretError::GitCheckFailed)
        ));
        let err = ensure_private(dir.path(), FILE).unwrap_err();
        assert_eq!(
            err.to_string(),
            "refusing to write secrets: .rustman is a symbolic link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_state_dir_is_refused_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(".rustman")).unwrap();
        assert!(ensure_private(dir.path(), FILE).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_state_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".rustman")).unwrap();
        std::os::unix::fs::symlink("/tmp/elsewhere", dir.path().join(FILE)).unwrap();
        let err = ensure_private(dir.path(), FILE).unwrap_err();
        assert_eq!(
            err.to_string(),
            "refusing to write secrets: .rustman holds a symbolic link: .rustman/secrets.yaml"
        );
    }

    #[test]
    fn state_path_that_is_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".rustman"), "x").unwrap();
        let err = ensure_state_dir(dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "refusing to write secrets: .rustman is not a folder"
        );
    }
}
