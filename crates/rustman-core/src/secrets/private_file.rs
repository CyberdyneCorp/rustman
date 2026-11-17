//! Writing a file that only the current user can read.
//!
//! The content goes to a temporary file in the same folder, which is made
//! private before anything is written to it, and is then renamed over the
//! target, so readers never see a partial or a world-readable file.
//! - Unix: the file is created with mode `0600` (and its folder, when created,
//!   with `0700`).
//! - Windows: inherited permissions are removed, the broad groups (Everyone,
//!   Authenticated Users, Users) are removed, and the current user is granted
//!   full control, using the built-in `icacls` tool. `SYSTEM` and
//!   `Administrators` may keep access, as root does on Unix. A rename keeps
//!   the file's own access list.

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
///
/// On a filesystem without hard links (FAT32, exFAT, some network shares) the
/// file is created in place instead, still failing when it exists. Readers can
/// then briefly see it empty or partial, and must retry.
pub fn write_new(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_new_with(path, contents, |from, to| fs::hard_link(from, to))
}

/// How [`write_new`] links the finished temporary file into place.
pub(crate) type LinkFn = fn(&Path, &Path) -> io::Result<()>;

/// [`write_new`] with the link operation given, so tests can make it fail.
pub(crate) fn write_new_with(path: &Path, contents: &[u8], link: LinkFn) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let tmp = temp_path(path);
    let linked = write_temp(&tmp, contents).and_then(|()| link(&tmp, path));
    let _ = fs::remove_file(&tmp);
    match linked {
        Err(e) if links_unsupported(&e) => create_in_place(path, contents),
        other => other,
    }
}

/// Create `path` directly, removing it again if writing fails part way.
fn create_in_place(path: &Path, contents: &[u8]) -> io::Result<()> {
    let result = write_temp(path, contents);
    if let Err(e) = &result
        && e.kind() != io::ErrorKind::AlreadyExists
    {
        let _ = fs::remove_file(path);
    }
    result
}

/// Whether a failed hard link means the filesystem cannot make one. Never
/// true for `AlreadyExists`, which means another writer got there first.
fn links_unsupported(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Unsupported
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::CrossesDevices
    ) || e
        .raw_os_error()
        .is_some_and(|code| NO_LINK_ERRORS.contains(&code))
}

/// Raw OS errors a filesystem without hard links reports.
#[cfg(any(target_os = "linux", target_os = "android"))]
const NO_LINK_ERRORS: &[i32] = &[1, 18, 95]; // EPERM, EXDEV, EOPNOTSUPP
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
const NO_LINK_ERRORS: &[i32] = &[1, 18, 45, 102]; // EPERM, EXDEV, ENOTSUP, EOPNOTSUPP
#[cfg(windows)]
const NO_LINK_ERRORS: &[i32] = &[1, 17, 50]; // INVALID_FUNCTION, NOT_SAME_DEVICE, NOT_SUPPORTED
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    windows
)))]
const NO_LINK_ERRORS: &[i32] = &[];

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

/// Well-known SIDs that must never keep access: Everyone, Authenticated Users, Users.
#[cfg(windows)]
const BROAD_GROUPS: [&str; 3] = ["*S-1-1-0", "*S-1-5-11", "*S-1-5-32-545"];

#[cfg(windows)]
fn restrict_to_user(path: &Path) -> io::Result<()> {
    let user = windows_user()?;
    let grant = format!("{user}:F");
    icacls(path, &["/inheritance:r", "/grant:r", &grant])?;
    let mut remove = vec!["/remove:g"];
    remove.extend(BROAD_GROUPS);
    icacls(path, &remove)
}

#[cfg(windows)]
fn icacls(path: &Path, args: &[&str]) -> io::Result<()> {
    use std::process::{Command, Stdio};
    let status = Command::new("icacls")
        .arg(path)
        .args(args)
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

    fn no_hard_links(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    fn raw_no_hard_links(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(NO_LINK_ERRORS[0]))
    }

    #[test]
    fn without_hard_links_the_file_is_created_in_place() {
        for link in [no_hard_links as LinkFn, raw_no_hard_links] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state/id");
            write_new_with(&path, b"one", link).unwrap();
            let err = write_new_with(&path, b"two", link).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(&path).unwrap(), b"one");
            let names: Vec<_> = fs::read_dir(path.parent().unwrap())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(names, ["id"]);
        }
    }

    #[test]
    fn a_link_that_finds_the_target_is_not_retried_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id");
        fs::write(&path, "theirs").unwrap();
        let err = write_new_with(&path, b"mine", |_, _| {
            Err(io::Error::from(io::ErrorKind::AlreadyExists))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"theirs");
    }

    #[cfg(unix)]
    #[test]
    fn created_in_place_file_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id");
        write_new_with(&path, b"x", no_hard_links).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
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
        let principals: Vec<String> = listing
            .lines()
            .map(|l| l.trim_start_matches(path.to_str().unwrap()).trim())
            .filter_map(|l| l.split_once(":(").map(|(who, _)| who.to_lowercase()))
            .collect();
        let allowed = [
            user.to_lowercase(),
            "nt authority\\system".to_string(),
            "builtin\\administrators".to_string(),
        ];
        assert!(principals.contains(&user.to_lowercase()), "{listing}");
        for who in &principals {
            assert!(
                allowed.contains(who),
                "unexpected grant for {who}:\n{listing}"
            );
        }
    }
}
