//! Reading files a workspace commits without trusting them (plugins spec,
//! "Plugin package and manifest"; design D5). Plugin components, manifest
//! files, cached downloads and `rustman.lock` are read before the plugin
//! set is trusted (by `plugin list`, the trust prompt and the pin check of
//! every headless command), so a file the repository planted must not make
//! the host read without end or block:
//!
//! - only a regular file is read: a FIFO, a device (`/dev/zero`,
//!   `/dev/tty`) or a socket is refused, and opening never blocks on one;
//! - a symbolic link is followed only when it resolves to a regular file
//!   inside the workspace;
//! - at most `cap + 1` bytes are read, however large the file is or grows.
//!
//! The sandbox's `fs-read` import opens files with [`open_regular`] and
//! checks the opened file against the path it resolved
//! ([`is_checked_file`]), so a path swapped for a symlink between the
//! check and the open is refused.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// The largest `rustman-plugin.toml` read: 256 KiB.
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// The largest `rustman.lock` read: 1 MiB.
pub const MAX_LOCK_BYTES: u64 = 1024 * 1024;

/// The file `path` of the workspace at `root`, at most `cap` bytes: it
/// must resolve to a regular file inside the workspace. A missing file is
/// [`io::ErrorKind::NotFound`].
pub fn read_workspace_file(root: &Path, path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let target = inside(root, path)?;
    read_capped(open_regular(&target)?, cap)
}

/// The canonical `path`, which must lie inside the canonical `root`.
fn inside(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let root = dunce::canonicalize(root)?;
    let target = dunce::canonicalize(path)?;
    if target.starts_with(&root) {
        Ok(target)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "resolves outside the workspace",
        ))
    }
}

/// Open `path` for reading, refusing a symbolic link (in its last
/// component) and anything but a regular file. Opening never blocks on a
/// FIFO or a device.
pub fn open_regular(path: &Path) -> io::Result<File> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "is a symbolic link",
        ));
    }
    let file = open_no_follow(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// At most `cap` bytes of `file`; more is an error. Reads `cap + 1` bytes
/// at most.
pub fn read_capped(file: File, cap: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than {cap} bytes"),
        ));
    }
    Ok(bytes)
}

/// Whether `file`, opened from the canonical path `checked`, is still the
/// file at that path: `checked` still resolves to itself (no component
/// became a symbolic link) and names the same file (device and inode on
/// Unix). A swap between a path check and the open fails one of the two.
pub fn is_checked_file(file: &File, checked: &Path) -> io::Result<bool> {
    if dunce::canonicalize(checked)? != checked {
        return Ok(false);
    }
    let now = std::fs::symlink_metadata(checked)?;
    Ok(same_file(&file.metadata()?, &now))
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.is_file() == b.is_file() && a.len() == b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regular_files_inside_the_workspace_are_read_up_to_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        assert_eq!(
            read_workspace_file(dir.path(), &path, 10).unwrap(),
            b"0123456789"
        );
        let err = read_workspace_file(dir.path(), &path, 9).unwrap_err();
        assert_eq!(err.to_string(), "larger than 9 bytes");
        let missing = read_workspace_file(dir.path(), &dir.path().join("none"), 9);
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn folders_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_workspace_file(dir.path(), dir.path(), 9).unwrap_err();
        assert!(
            ["not a regular file", "Is a directory"]
                .iter()
                .any(|m| err.to_string().contains(m)),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_resolve_inside_the_workspace_only() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("real.wasm"), b"ok").unwrap();
        std::fs::write(outside.path().join("secret"), b"s").unwrap();
        std::os::unix::fs::symlink(root.join("real.wasm"), root.join("in.wasm")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.join("out.wasm")).unwrap();
        assert_eq!(
            read_workspace_file(root, &root.join("in.wasm"), 9).unwrap(),
            b"ok"
        );
        let err = read_workspace_file(root, &root.join("out.wasm"), 9).unwrap_err();
        assert_eq!(err.to_string(), "resolves outside the workspace");
        // The sandbox opens a resolved path: a final symlink is refused.
        let err = open_regular(&root.join("in.wasm")).unwrap_err();
        assert_eq!(err.to_string(), "is a symbolic link");
    }

    /// A FIFO inside the workspace is refused at once instead of blocking
    /// the open until a writer comes.
    #[cfg(unix)]
    #[test]
    fn fifos_are_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("p.wasm");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("skipped: mkfifo is not available");
            return;
        }
        let started = std::time::Instant::now();
        let err = read_workspace_file(dir.path(), &fifo, 9).unwrap_err();
        assert_eq!(err.to_string(), "not a regular file");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn an_opened_file_is_checked_against_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let path = root.join("a.txt");
        std::fs::write(&path, b"a").unwrap();
        std::fs::write(root.join("b.txt"), b"b").unwrap();
        let file = open_regular(&path).unwrap();
        assert!(is_checked_file(&file, &path).unwrap());
        // Swapped for another file after the open.
        std::fs::rename(root.join("b.txt"), &path).unwrap();
        assert!(!is_checked_file(&file, &path).unwrap());
        // Swapped for a symlink after the open.
        let file = open_regular(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(root.join("c.txt"), b"c").unwrap();
        std::os::unix::fs::symlink(root.join("c.txt"), &path).unwrap();
        assert!(!is_checked_file(&file, &path).unwrap());
    }
}
