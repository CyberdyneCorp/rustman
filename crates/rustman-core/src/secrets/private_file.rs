//! Writing a file that only the current user can read.
//!
//! The content goes to a temporary file in the same folder, which is made
//! private before anything is written to it, and is then renamed over the
//! target, so readers never see a partial or a world-readable file.
//! - Unix: the file is created with mode `0600` (and its folder, when created,
//!   with `0700`).
//! - Windows: inherited permissions are removed and the current user is granted
//!   full control, using the built-in `icacls` tool. A rename keeps the
//!   file's own access list.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Atomically replace `path` with `contents`, readable by the current user only.
pub fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let tmp = temp_path(path);
    let result = write_temp(&tmp, contents).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Create `path` with `contents`, readable by the current user only, unless it
/// already exists (`ErrorKind::AlreadyExists`). The content is complete before
/// the file appears, because the finished temporary file is hard-linked into
/// place; linking fails, rather than replacing, when `path` exists.
pub fn write_new(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let tmp = temp_path(path);
    let result = write_temp(&tmp, contents).and_then(|()| fs::hard_link(&tmp, path));
    let _ = fs::remove_file(&tmp);
    result
}

/// A temporary name unique to this process and call, so concurrent writers
/// never share (or delete) each other's temporary file.
fn temp_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{n}.tmp", std::process::id()));
    path.with_file_name(name)
}

fn write_temp(tmp: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = create_private(tmp)?;
    restrict_to_user(tmp)?;
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.is_dir() {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn restrict_to_user(_path: &Path) -> io::Result<()> {
    // The mode given at creation already restricts it.
    Ok(())
}

#[cfg(windows)]
fn restrict_to_user(path: &Path) -> io::Result<()> {
    use std::process::{Command, Stdio};
    let user = windows_user()?;
    let status = Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:F"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "could not restrict access to the current user (icacls exited with {status})"
        )))
    }
}

#[cfg(windows)]
fn windows_user() -> io::Result<String> {
    let name = std::env::var("USERNAME")
        .map_err(|_| io::Error::other("USERNAME is not set; cannot restrict file access"))?;
    Ok(match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => format!("{domain}\\{name}"),
        _ => name,
    })
}

#[cfg(not(any(unix, windows)))]
fn restrict_to_user(_path: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "restricting file access is not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_target_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/secret.yaml");
        write(&path, b"one").unwrap();
        write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["secret.yaml"]);
    }

    #[test]
    fn write_new_never_replaces_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id");
        write_new(&path, b"one").unwrap();
        let err = write_new(&path, b"two").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"one");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["id"]);
    }

    #[cfg(unix)]
    #[test]
    fn new_folder_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/secret.yaml");
        write(&path, b"x").unwrap();
        let mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[cfg(windows)]
    #[test]
    fn only_the_current_user_has_access_on_windows() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.yaml");
        write(&path, b"x").unwrap();
        let out = Command::new("icacls").arg(&path).output().unwrap();
        let listing = String::from_utf8_lossy(&out.stdout).to_string();
        let user = windows_user().unwrap();
        let grants: Vec<&str> = listing
            .lines()
            .map(|l| l.trim_start_matches(path.to_str().unwrap()).trim())
            .filter(|l| l.contains(":("))
            .collect();
        assert_eq!(grants.len(), 1, "{listing}");
        assert!(
            grants[0].to_lowercase().starts_with(&user.to_lowercase()),
            "{listing}"
        );
    }
}
