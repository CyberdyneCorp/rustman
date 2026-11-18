//! Reading and writing snapshot files (design "Risks": approving in the TUI
//! while a run is writing).
//!
//! A write holds a per-file lock, compares under the lock, and skips the
//! write when the file already has the same content (line endings aside), so
//! a matching snapshot keeps its bytes and modification time. A write also
//! names what it expects on disk (no file, or the text it compared against):
//! when another writer got there first, the file is left alone and the write
//! reports a [`WriteResult::Conflict`]. Otherwise the text goes to a
//! temporary file in the same folder that is renamed over the snapshot, so
//! readers never see half a file.
//!
//! Lock files live in a per-user temporary folder, keyed by a hash of the
//! snapshot's canonical path, so `__snapshots__/` only ever holds snapshots.
//! They are empty and are never deleted: removing a lock file another
//! process is waiting on would let two writers hold "the" lock at once.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::normalize::sha256_hex;
use crate::secrets::{ensure_private_dir, user_temp_dir};

/// The folder of lock files, inside the per-user temporary folder.
const LOCK_DIR: &str = "rustman-snapshot-locks";

/// What [`write_snapshot`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteResult {
    Written,
    /// The file already had this content and was left untouched.
    Unchanged,
    /// The file no longer held what the caller expected (another writer
    /// changed it); it was left untouched.
    Conflict,
}

/// The text of the snapshot at `path`, or `None` when there is none.
pub fn read_snapshot(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write `text` to the snapshot at `path` unless it already holds it, and
/// only while the file still holds `expected` (`None`: no file).
pub fn write_snapshot(path: &Path, expected: Option<&str>, text: &str) -> io::Result<WriteResult> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(dir) = dir {
        fs::create_dir_all(dir)?;
    }
    let _lock = SnapshotLock::acquire(path)?;
    let current = read_snapshot(path)?;
    if current.as_deref().is_some_and(|old| same_text(old, text)) {
        return Ok(WriteResult::Unchanged);
    }
    let as_expected = match (current.as_deref(), expected) {
        (None, None) => true,
        (Some(old), Some(expected)) => same_text(old, expected),
        _ => false,
    };
    if !as_expected {
        return Ok(WriteResult::Conflict);
    }
    write_atomic(path, text)?;
    Ok(WriteResult::Written)
}

/// Equal content, whatever the line endings (Git may check files out with CRLF).
pub fn same_text(a: &str, b: &str) -> bool {
    a == b || a.replace("\r\n", "\n") == b.replace("\r\n", "\n")
}

/// An exclusive lock on one snapshot path, released on drop.
pub struct SnapshotLock {
    _file: File,
}

impl SnapshotLock {
    /// Wait for and take the lock of the snapshot at `path` (its folder
    /// must exist).
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let dir = user_temp_dir(LOCK_DIR);
        ensure_private_dir(&dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(lock_name(path)))?;
        file.lock()?;
        Ok(Self { _file: file })
    }
}

/// The lock file name of `path`: a hash of its canonical form, lower-cased
/// on Windows where paths are case-insensitive.
fn lock_name(path: &Path) -> String {
    let key = canonical(path).to_string_lossy().into_owned();
    let key = if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    };
    format!("{}.lock", &sha256_hex(key.as_bytes())[..32])
}

/// `path` with its folder resolved (the file itself may not exist yet).
fn canonical(path: &Path) -> PathBuf {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    let dir = dir.unwrap_or(Path::new("."));
    let resolved = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    match path.file_name() {
        Some(name) => resolved.join(name),
        None => resolved,
    }
}

/// Replace `path` through a uniquely named temporary file in its folder.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    remove_leftovers(path, &name);
    let temp = path.with_file_name(format!(".{name}.{}-{n}.tmp", std::process::id()));
    let result = write_temp(&temp, text).and_then(|()| rename_retrying(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Delete the temporary files an interrupted write of this snapshot left
/// behind. Every writer holds the lock from creating its temporary file to
/// renaming it, so while the caller holds it no such file is in use.
fn remove_leftovers(path: &Path, name: &str) {
    let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let Ok(items) = fs::read_dir(dir) else {
        return;
    };
    let prefix = format!(".{name}.");
    for item in items.flatten() {
        let file = item.file_name().to_string_lossy().into_owned();
        if file.starts_with(&prefix) && file.ends_with(".tmp") {
            let _ = fs::remove_file(item.path());
        }
    }
}

fn write_temp(temp: &Path, text: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(temp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// Rename `from` to `to`. On Windows a target another process has open (an
/// editor, an antivirus scan) refuses the rename for a moment: retry briefly.
fn rename_retrying(from: &Path, to: &Path) -> io::Result<()> {
    const ATTEMPTS: u32 = if cfg!(windows) { 20 } else { 1 };
    let mut attempt = 1;
    loop {
        match fs::rename(from, to) {
            Err(e) if attempt < ATTEMPTS && is_transient(&e) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

/// Whether a failed rename may succeed a moment later: access denied, or on
/// Windows a sharing violation (another process has the file open without
/// sharing it, which Rust reports with an uncategorized kind).
fn is_transient(e: &io::Error) -> bool {
    /// `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION`.
    const WINDOWS_SHARING: [i32; 2] = [32, 33];
    e.kind() == io::ErrorKind::PermissionDenied
        || (cfg!(windows)
            && e.raw_os_error()
                .is_some_and(|c| WINDOWS_SHARING.contains(&c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn missing_snapshots_read_as_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            read_snapshot(&tmp.path().join("x.snap.yaml")).unwrap(),
            None
        );
    }

    #[test]
    fn writes_create_the_folder_and_leave_no_temporary_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("__snapshots__").join("a.default.snap.yaml");
        assert_eq!(
            write_snapshot(&path, None, "version: 1\n").unwrap(),
            WriteResult::Written
        );
        assert_eq!(
            read_snapshot(&path).unwrap().as_deref(),
            Some("version: 1\n")
        );
        assert_eq!(names(path.parent().unwrap()), ["a.default.snap.yaml"]);
    }

    #[test]
    fn matching_content_is_not_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.default.snap.yaml");
        fs::write(&path, "version: 1\r\nname: a\r\n").unwrap();
        let old = File::open(&path).unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(past)
            .unwrap();
        drop(old);
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        let result = write_snapshot(&path, None, "version: 1\nname: a\n").unwrap();
        assert_eq!(result, WriteResult::Unchanged);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
        assert_eq!(fs::read(&path).unwrap(), b"version: 1\r\nname: a\r\n");
    }

    #[test]
    fn concurrent_writes_end_with_one_complete_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("__snapshots__").join("c.default.snap.yaml");
        let texts: Vec<String> = (0..8)
            .map(|i| format!("version: 1\nwriter: {i}\n{}\n", "x".repeat(64 * 1024)))
            .collect();
        std::thread::scope(|scope| {
            for text in &texts {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..5 {
                        let current = read_snapshot(path).unwrap();
                        write_snapshot(path, current.as_deref(), text).unwrap();
                        let seen = read_snapshot(path).unwrap().unwrap();
                        assert!(texts_contains(&seen), "a reader saw a partial file");
                    }
                });
            }
        });
        let last = read_snapshot(&path).unwrap().unwrap();
        assert!(texts.contains(&last));
        assert_eq!(names(path.parent().unwrap()), ["c.default.snap.yaml"]);

        fn texts_contains(seen: &str) -> bool {
            seen.starts_with("version: 1\nwriter: ")
                && seen.ends_with(&format!("{}\n", "x".repeat(64 * 1024)))
        }
    }

    #[test]
    fn lock_names_depend_on_the_resolved_path() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.snap.yaml");
        let dotted = tmp.path().join(".").join("a.snap.yaml");
        assert_eq!(lock_name(&a), lock_name(&dotted));
        assert_ne!(lock_name(&a), lock_name(&tmp.path().join("b.snap.yaml")));
        let held = SnapshotLock::acquire(&a).unwrap();
        drop(held);
        SnapshotLock::acquire(&a).unwrap();
    }

    #[test]
    fn a_write_leaves_a_file_another_writer_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.default.snap.yaml");
        assert_eq!(
            write_snapshot(&path, None, "one\n").unwrap(),
            WriteResult::Written
        );
        // Recording expects no file: the one now on disk wins.
        assert_eq!(
            write_snapshot(&path, None, "two\n").unwrap(),
            WriteResult::Conflict
        );
        // Updating expects the text it compared against.
        assert_eq!(
            write_snapshot(&path, Some("stale\n"), "two\n").unwrap(),
            WriteResult::Conflict
        );
        assert_eq!(read_snapshot(&path).unwrap().as_deref(), Some("one\n"));
        assert_eq!(
            write_snapshot(&path, Some("one\r\n"), "two\n").unwrap(),
            WriteResult::Written
        );
        assert_eq!(read_snapshot(&path).unwrap().as_deref(), Some("two\n"));
        // A file deleted since it was read is not written back.
        fs::remove_file(&path).unwrap();
        assert_eq!(
            write_snapshot(&path, Some("two\n"), "three\n").unwrap(),
            WriteResult::Conflict
        );
        assert_eq!(read_snapshot(&path).unwrap(), None);
    }

    #[test]
    fn renames_retry_on_access_and_sharing_errors() {
        assert!(is_transient(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(!is_transient(&io::Error::from(io::ErrorKind::NotFound)));
        let sharing = io::Error::from_raw_os_error(32);
        assert_eq!(is_transient(&sharing), cfg!(windows));
    }

    #[test]
    fn a_write_removes_temporary_files_an_interrupted_write_left() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.default.snap.yaml");
        fs::write(tmp.path().join(".a.default.snap.yaml.999-0.tmp"), "half").unwrap();
        fs::write(tmp.path().join(".b.default.snap.yaml.999-0.tmp"), "other").unwrap();
        write_snapshot(&path, None, "version: 1\n").unwrap();
        assert_eq!(
            names(tmp.path()),
            [".b.default.snap.yaml.999-0.tmp", "a.default.snap.yaml"]
        );
    }
}
