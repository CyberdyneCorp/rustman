//! Guards run before anything is written under `.rustman/`: the folder must
//! be a real folder of the workspace, and Git must ignore it (environments spec
//! "Secrets file is git-ignored").

use std::io::{self, ErrorKind};
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
/// (a tracked file counts as not ignored). Without `git`, or when Git says
/// `root` is not in a work tree, the check passes; any other outcome is a
/// failure, so a check Git cannot answer never lets a write through.
pub fn ensure_ignored(root: &Path, rel: &str) -> Result<(), SecretError> {
    ensure_ignored_with(root, rel, &run_git)
}

/// What a `git` run produced: its exit code (`None` when killed by a signal)
/// and its standard output and error.
struct GitRun {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Runs `git -C <root> <args>`.
type GitRunner = dyn Fn(&Path, &[&str]) -> io::Result<GitRun>;

fn ensure_ignored_with(root: &Path, rel: &str, git: &GitRunner) -> Result<(), SecretError> {
    match git(root, &["check-ignore", "-q", "--", rel]) {
        Ok(run) if run.code == Some(0) => return Ok(()),
        Ok(run) if run.code == Some(1) => return Err(SecretError::NotIgnored),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        _ => {}
    }
    if outside_work_tree(root, git) {
        Ok(())
    } else {
        Err(SecretError::GitCheckFailed)
    }
}

/// Whether Git positively says `root` is not inside a work tree: it is not
/// in a repository at all, or only inside a `.git` folder or a bare one.
fn outside_work_tree(root: &Path, git: &GitRunner) -> bool {
    match git(root, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(run) if run.code == Some(0) => run.stdout.trim_ascii() == b"false",
        Ok(run) => contains(&run.stderr, b"not a git repository"),
        Err(_) => false,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn run_git(root: &Path, args: &[&str]) -> io::Result<GitRun> {
    // Untranslated messages, so "not a git repository" can be recognized.
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        .stdin(Stdio::null())
        .output()?;
    Ok(GitRun {
        code: out.status.code(),
        stdout: out.stdout,
        stderr: out.stderr,
    })
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

    fn fake_git(
        check_ignore: io::Result<GitRun>,
        rev_parse: io::Result<GitRun>,
    ) -> impl Fn(&Path, &[&str]) -> io::Result<GitRun> {
        let check_ignore = std::cell::RefCell::new(Some(check_ignore));
        let rev_parse = std::cell::RefCell::new(Some(rev_parse));
        move |_, args| {
            let slot = if args[0] == "check-ignore" {
                &check_ignore
            } else {
                &rev_parse
            };
            slot.borrow_mut().take().expect("git run once per command")
        }
    }

    fn exited(code: i32, stdout: &str, stderr: &str) -> io::Result<GitRun> {
        Ok(GitRun {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }

    fn unused() -> io::Result<GitRun> {
        Err(io::Error::other("not run"))
    }

    fn check(
        check_ignore: io::Result<GitRun>,
        rev_parse: io::Result<GitRun>,
    ) -> Result<(), SecretError> {
        ensure_ignored_with(Path::new("ws"), FILE, &fake_git(check_ignore, rev_parse))
    }

    #[test]
    fn check_ignore_answers_are_final() {
        assert!(check(exited(0, "", ""), unused()).is_ok());
        assert!(matches!(
            check(exited(1, "", ""), unused()),
            Err(SecretError::NotIgnored)
        ));
    }

    #[test]
    fn missing_git_passes() {
        let missing = || Err(io::Error::from(ErrorKind::NotFound));
        assert!(check(missing(), missing()).is_ok());
    }

    #[test]
    fn only_a_reported_missing_repository_passes() {
        let not_repo = "fatal: not a git repository (or any of the parent directories): .git\n";
        assert!(check(exited(128, "", not_repo), exited(128, "", not_repo)).is_ok());
        assert!(check(exited(128, "", ""), exited(0, "false\n", "")).is_ok());
    }

    #[test]
    fn other_git_failures_fail_closed() {
        let failed = |rev_parse| {
            matches!(
                check(exited(128, "", "fatal: x"), rev_parse),
                Err(SecretError::GitCheckFailed)
            )
        };
        assert!(failed(exited(0, "true\n", "")));
        assert!(failed(exited(
            128,
            "",
            "fatal: cannot change to '\\\\?\\C:\\ws': Invalid argument\n"
        )));
        assert!(failed(exited(
            128,
            "",
            "fatal: detected dubious ownership in repository\n"
        )));
        assert!(failed(Ok(GitRun {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new()
        })));
        assert!(failed(Err(io::Error::other("spawn failed"))));
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
