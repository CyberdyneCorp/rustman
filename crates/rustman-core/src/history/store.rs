//! The history store: one JSON file per exchange, named by its ULID (design
//! D5). Each entry is written like the secrets file: to a private temporary
//! file (current user only, on Windows too), synced, then renamed into
//! place, so readers never see a partial entry; pruning runs under an exclusive lock
//! on `.lock`, so concurrent writers never prune the same entries twice.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use super::data_dir::rustman_data_dir;
use super::entry::Entry;
use super::ulid::is_ulid;
use crate::secrets::{SecretError, create_private_dir, ensure_private, write_private};
use crate::settings::HistorySettings;

/// Where a workspace keeps its history, relative to its root.
pub const HISTORY_DIR: &str = ".rustman/history";

/// Where spilled response bodies live, relative to the workspace root.
const BODIES_DIR: &str = ".rustman/bodies";

const LOCK_FILE: &str = ".lock";
const SUFFIX: &str = ".json";

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Unsafe(#[from] SecretError),
    #[error("no history entry '{0}' (see 'rustman history list')")]
    NotFound(String),
    #[error("a history id cannot be empty (see 'rustman history list')")]
    EmptyId,
    #[error("cannot read history entry {}: {reason}", path.display())]
    Unreadable { path: PathBuf, reason: String },
    #[error("history id '{id}' is ambiguous: {}", matches.join(", "))]
    Ambiguous { id: String, matches: Vec<String> },
    #[error("no data directory for ad-hoc history (set RUSTMAN_DATA_DIR)")]
    NoDataDir,
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> HistoryError + '_ {
    move |source| HistoryError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The entries of a store, newest first, and why any were skipped.
#[derive(Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
}

/// The extension of spill files in the bodies folder.
const SPILL_SUFFIX: &str = ".bin";

/// How old a spill file no entry names must be before pruning deletes it.
const ORPHAN_AGE: Duration = Duration::from_secs(60 * 60);

/// A folder of history entries.
#[derive(Debug)]
pub struct HistoryStore {
    dir: PathBuf,
    /// Where the spill files that entries name live.
    bodies: Option<PathBuf>,
    /// The workspace root, checked once before the first write.
    workspace: Option<PathBuf>,
    max_entries: usize,
    prepared: OnceLock<()>,
}

impl HistoryStore {
    /// `.rustman/history/` of the workspace at `root`.
    pub fn for_workspace(root: &Path, settings: &HistorySettings) -> Self {
        Self {
            dir: root.join(HISTORY_DIR),
            bodies: Some(root.join(BODIES_DIR)),
            workspace: Some(root.to_path_buf()),
            max_entries: settings.max_entries,
            prepared: OnceLock::new(),
        }
    }

    /// `<data_dir>/rustman/history/`, for ad-hoc requests outside any workspace.
    pub fn global(settings: &HistorySettings) -> Result<Self, HistoryError> {
        let dir = rustman_data_dir().ok_or(HistoryError::NoDataDir)?;
        Ok(Self::at(&dir.join("history"), settings.max_entries))
    }

    /// A store in `dir`, with no spill files and no workspace checks.
    pub fn at(dir: &Path, max_entries: usize) -> Self {
        Self {
            dir: dir.to_path_buf(),
            bodies: None,
            workspace: None,
            max_entries,
            prepared: OnceLock::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The spill file `name` of an entry, if this store keeps spill files.
    pub fn spill_path(&self, name: &str) -> Option<PathBuf> {
        self.bodies
            .as_ref()
            .filter(|_| is_plain_file_name(name))
            .map(|d| d.join(name))
    }

    /// The spill file name to record for a body spilled to `path`: only one
    /// in this store's bodies folder.
    pub fn spill_name(&self, path: &Path) -> Option<String> {
        let bodies = self.bodies.as_deref()?;
        (path.parent()? == bodies)
            .then(|| path.file_name()?.to_str().map(str::to_owned))
            .flatten()
    }

    /// Write `entry`, then prune the oldest entries beyond the limit.
    pub fn record(&self, entry: &Entry) -> Result<(), HistoryError> {
        self.prepare()?;
        let json = serde_json::to_vec_pretty(entry).map_err(io::Error::from);
        let path = self.entry_path(&entry.id);
        json.and_then(|json| write_private(&path, &json))
            .map_err(io_error(&path))?;
        self.prune()
    }

    /// Every readable entry, newest first. Unreadable ones are skipped with
    /// a warning; ones removed meanwhile are skipped silently.
    pub fn list(&self) -> Result<Listing, HistoryError> {
        let mut listing = Listing::default();
        for id in self.ids()?.iter().rev() {
            match self.read(id) {
                Ok(Some(entry)) => listing.entries.push(entry),
                Ok(None) => {}
                Err(reason) => listing.warnings.push(format!(
                    "skipping unreadable history entry {}: {reason}",
                    self.entry_path(id).display()
                )),
            }
        }
        Ok(listing)
    }

    /// The entry `id`, or the one entry whose id starts with `id`.
    pub fn get(&self, id: &str) -> Result<Entry, HistoryError> {
        let id = id.trim().to_ascii_uppercase();
        if id.is_empty() {
            return Err(HistoryError::EmptyId);
        }
        let ids = self.ids()?;
        let matches: Vec<&String> = match ids.iter().find(|i| **i == id) {
            Some(exact) => vec![exact],
            None => ids.iter().filter(|i| i.starts_with(&id)).collect(),
        };
        match matches.as_slice() {
            [one] => match self.read(one) {
                Ok(Some(entry)) => Ok(entry),
                Ok(None) => Err(HistoryError::NotFound(id)),
                Err(reason) => Err(HistoryError::Unreadable {
                    path: self.entry_path(one),
                    reason,
                }),
            },
            [] => Err(HistoryError::NotFound(id)),
            many => Err(HistoryError::Ambiguous {
                id,
                matches: many.iter().map(|s| (*s).clone()).collect(),
            }),
        }
    }

    /// Delete every entry and every spill file; returns how many entries
    /// were deleted.
    pub fn clear(&self) -> Result<usize, HistoryError> {
        if !self.dir.is_dir() {
            self.remove_orphan_spills(Duration::ZERO);
            return Ok(0);
        }
        let _lock = self.lock()?;
        let ids = self.ids()?;
        for id in &ids {
            self.remove(id)?;
        }
        self.remove_orphan_spills(Duration::ZERO);
        Ok(ids.len())
    }

    fn prepare(&self) -> Result<(), HistoryError> {
        if self.prepared.get().is_some() {
            return Ok(());
        }
        if let Some(root) = &self.workspace {
            ensure_private(root, HISTORY_DIR)?;
        }
        create_private_dir(&self.dir).map_err(io_error(&self.dir))?;
        let _ = self.prepared.set(());
        Ok(())
    }

    fn prune(&self) -> Result<(), HistoryError> {
        let _lock = self.lock()?;
        let ids = self.ids()?;
        let excess = ids.len().saturating_sub(self.max_entries);
        for id in &ids[..excess] {
            match self.try_remove(id) {
                Err((path, e)) if !busy(&e) => return Err(io_error(&path)(e)),
                // Busy: skipped now, removed by the next write's pruning.
                _ => {}
            }
        }
        self.remove_orphan_spills(ORPHAN_AGE);
        Ok(())
    }

    /// Delete the spill files no entry names that were last written at
    /// least `min_age` ago (a younger one may belong to a request still
    /// running). Best effort: a file that cannot be deleted stays for the
    /// next write.
    fn remove_orphan_spills(&self, min_age: Duration) {
        let Some(bodies) = &self.bodies else {
            return;
        };
        let candidates = spill_files(bodies, min_age);
        if candidates.is_empty() {
            return;
        }
        let ids = self.ids().unwrap_or_default();
        let named: HashSet<String> = ids
            .iter()
            .filter_map(|id| self.read(id).ok().flatten()?.spill)
            .collect();
        for name in candidates.iter().filter(|n| !named.contains(*n)) {
            let _ = remove_if_present(&bodies.join(name));
        }
    }

    /// An exclusive lock on `.lock`, released when the file is dropped.
    fn lock(&self) -> Result<File, HistoryError> {
        let path = self.dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_error(&path))?;
        file.lock().map_err(io_error(&path))?;
        Ok(file)
    }

    /// Entry ids, oldest first.
    fn ids(&self) -> Result<Vec<String>, HistoryError> {
        let read = match fs::read_dir(&self.dir) {
            Ok(read) => read,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error(&self.dir)(e)),
        };
        let mut ids: Vec<String> = read
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(SUFFIX).map(str::to_owned))
            .filter(|id| is_ulid(id))
            .collect();
        ids.sort();
        Ok(ids)
    }

    fn entry_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}{SUFFIX}"))
    }

    /// `Ok(None)` when the entry is gone; `Err` holds why it is unreadable.
    fn read(&self, id: &str) -> Result<Option<Entry>, String> {
        match fs::read(self.entry_path(id)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| e.to_string()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Delete entry `id` and its spill file.
    fn remove(&self, id: &str) -> Result<(), HistoryError> {
        self.try_remove(id).map_err(|(path, e)| io_error(&path)(e))
    }

    /// Delete entry `id`, then its spill file, so an entry never names a
    /// missing spill file; on failure, the file that could not be deleted
    /// and why. A spill file left behind is an orphan a later pruning
    /// deletes.
    fn try_remove(&self, id: &str) -> Result<(), (PathBuf, io::Error)> {
        let spill = self.read(id).ok().flatten().and_then(|e| e.spill);
        let path = self.entry_path(id);
        remove_if_present(&path).map_err(|e| (path, e))?;
        if let Some(path) = spill.as_deref().and_then(|name| self.spill_path(name)) {
            remove_if_present(&path).map_err(|e| (path, e))?;
        }
        Ok(())
    }
}

/// Whether `name` is one plain file name: no separator, no `..`, no drive
/// or root prefix (`C:x` on Windows), so joining it to a folder stays inside.
fn is_plain_file_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    let one_normal =
        matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none();
    one_normal && !name.contains(['/', '\\', ':'])
}

/// The `.bin` files of `bodies` last written at least `min_age` ago.
fn spill_files(bodies: &Path, min_age: Duration) -> Vec<String> {
    let Ok(read) = fs::read_dir(bodies) else {
        return Vec::new();
    };
    let old_enough = |entry: &fs::DirEntry| {
        let modified = entry.metadata().and_then(|m| m.modified());
        modified.is_ok_and(|t| t.elapsed().unwrap_or_default() >= min_age)
    };
    read.filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && old_enough(e))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.ends_with(SPILL_SUFFIX))
        .collect()
}

/// Delete `path`; a file already gone is not an error.
fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Whether deleting a file failed only because something holds it open.
/// On Windows that is also access denied or a sharing or lock violation,
/// while another process (a reader, an antivirus scan) has the file.
fn busy(e: &io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    let held_open = e.kind() == ErrorKind::PermissionDenied
        || matches!(
            e.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        );
    e.kind() == ErrorKind::ResourceBusy || (cfg!(windows) && held_open)
}

#[cfg(test)]
mod tests;
