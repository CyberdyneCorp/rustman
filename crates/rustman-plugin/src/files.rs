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

/// `FILE_FLAG_OPEN_REPARSE_POINT`: open a reparse point (a symbolic link,
/// a junction) itself instead of what it points to.
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// `FILE_ATTRIBUTE_REPARSE_POINT`.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// On Windows, a final reparse point is opened itself, never followed, and
/// refused: a path swapped for a link just for the open reads nothing.
#[cfg(windows)]
fn open_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "is a symbolic link",
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
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
/// Unix, volume serial and file index on Windows). A swap between a path
/// check and the open fails one of the two.
pub fn is_checked_file(file: &File, checked: &Path) -> io::Result<bool> {
    if dunce::canonicalize(checked)? != checked {
        return Ok(false);
    }
    same_file(file, checked)
}

/// Whether `file` is the file at `path` (a final symlink not followed).
#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let (a, b) = (file.metadata()?, std::fs::symlink_metadata(path)?);
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

/// Whether `file` is the file at `path` (a final reparse point not
/// followed): the same volume serial number and file index.
#[cfg(windows)]
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use winapi_util::file::information;
    /// Opens a folder too.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let now = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let (a, b) = (information(file)?, information(&now)?);
    Ok(a.volume_serial_number() == b.volume_serial_number() && a.file_index() == b.file_index())
}

#[cfg(not(any(unix, windows)))]
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    let (a, b) = (file.metadata()?, std::fs::symlink_metadata(path)?);
    Ok(a.is_file() == b.is_file() && a.len() == b.len())
}

/// The entry names of the folder at the canonical path `checked`; `None`
/// when the path changed meanwhile (a component swapped for a symlink).
///
/// On Unix the folder is opened without following a final symlink
/// (`O_DIRECTORY | O_NOFOLLOW`), checked against `checked` (device and
/// inode), and listed through that descriptor: a path swapped away and
/// back around the listing cannot show another folder's entries.
#[cfg(unix)]
pub fn list_checked_dir(checked: &Path) -> io::Result<Option<Vec<std::ffi::OsString>>> {
    match open_checked_dir(checked)? {
        Some(dir) => list_open_dir(dir).map(Some),
        None => Ok(None),
    }
}

/// The folder at `checked`, opened without following a final symlink;
/// `None` unless it is still the folder at that canonical path.
#[cfg(unix)]
fn open_checked_dir(checked: &Path) -> io::Result<Option<File>> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(checked);
    let dir = match opened {
        Ok(dir) => dir,
        Err(_) if std::fs::symlink_metadata(checked).is_ok_and(|m| m.file_type().is_symlink()) => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    Ok(is_checked_file(&dir, checked)?.then_some(dir))
}

/// The entry names of the open folder `dir`, read through its descriptor
/// (whatever its path names by now).
#[cfg(unix)]
fn list_open_dir(dir: File) -> io::Result<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::new(std::os::fd::OwnedFd::from(dir))? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            names.push(std::ffi::OsStr::from_bytes(name).to_os_string());
        }
    }
    Ok(names)
}

/// The entry names of the folder at the canonical path `checked`; `None`
/// when the path no longer resolves to itself after the listing.
#[cfg(not(unix))]
pub fn list_checked_dir(checked: &Path) -> io::Result<Option<Vec<std::ffi::OsString>>> {
    let names = std::fs::read_dir(checked)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    Ok((dunce::canonicalize(checked)? == checked).then_some(names))
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
        // Windows refuses to open a folder as a file ("Access is denied").
        assert!(
            ["not a regular file", "Is a directory", "Access is denied"]
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

    /// A folder is listed through the descriptor that was checked: once
    /// open, swapping its path for a symlink to another folder (and back)
    /// cannot list that folder's entries. A path that is a symlink when
    /// opened is refused.
    #[cfg(unix)]
    #[test]
    fn folders_are_listed_through_the_checked_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let data = root.join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("inside.txt"), "i").unwrap();
        std::fs::write(outside.path().join("outside.txt"), "o").unwrap();
        assert_eq!(
            list_checked_dir(&data).unwrap().unwrap(),
            [std::ffi::OsString::from("inside.txt")]
        );

        let opened = open_checked_dir(&data).unwrap().unwrap();
        std::fs::rename(&data, root.join("old")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &data).unwrap();
        assert_eq!(
            list_open_dir(opened).unwrap(),
            [std::ffi::OsString::from("inside.txt")]
        );
        assert_eq!(list_checked_dir(&data).unwrap(), None);
        // Another folder at the path is not the one checked.
        std::fs::remove_file(&data).unwrap();
        std::fs::create_dir(&data).unwrap();
        let file = File::open(root.join("old")).unwrap();
        assert!(!is_checked_file(&file, &data).unwrap());
    }

    /// Windows: a final reparse point is opened itself and refused, so a
    /// path swapped for a link just for the open reads nothing outside.
    #[cfg(windows)]
    #[test]
    fn windows_reparse_points_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "s").unwrap();
        let link = root.join("link.txt");
        if let Err(e) = std::os::windows::fs::symlink_file(&secret, &link) {
            eprintln!("skipped: cannot create a symlink here ({e})");
            return;
        }
        let err = open_no_follow(&link).unwrap_err();
        assert_eq!(err.to_string(), "is a symbolic link");
        let err = open_regular(&link).unwrap_err();
        assert_eq!(err.to_string(), "is a symbolic link");
    }

    /// Windows: an opened file is the file at its path only with the same
    /// volume serial and file index; a file of the same length is not.
    #[cfg(windows)]
    #[test]
    fn windows_files_are_compared_by_file_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let path = root.join("a.txt");
        let other = root.join("b.txt");
        std::fs::write(&path, b"a").unwrap();
        std::fs::write(&other, b"b").unwrap();
        let file = open_regular(&path).unwrap();
        assert!(is_checked_file(&file, &path).unwrap());
        let same_length = open_regular(&other).unwrap();
        assert!(!is_checked_file(&same_length, &path).unwrap());
    }
}
