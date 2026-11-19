//! Which snapshot files a workspace holds and whether a request still uses
//! them (snapshot-testing "Snapshot list command", "Prune obsolete
//! snapshots"; design D7). Nothing is sent.
//!
//! A file `D/__snapshots__/<stem>.<name>.snap.yaml` belongs to the request
//! file `D/<stem>.rman.yaml`. It is:
//! - `ok` when that request declares a snapshot named `<name>`, or when its
//!   snapshots cannot be known (a kind this build cannot read): such a file
//!   is never pruned;
//! - `orphaned` when no such request file exists;
//! - `unreferenced name` when the request no longer declares `<name>`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::file::{SNAPSHOT_DIR, SNAPSHOT_SUFFIX};
use super::store::{SnapshotLock, read_snapshot, same_text};
use crate::collection::is_tool_dir;
use crate::model::{Assertion, REQUEST_FILE_SUFFIX, Request};
use crate::workspace::Workspace;

/// Whether a snapshot file is still used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    Ok,
    /// Its request file no longer exists.
    Orphaned,
    /// Its request file no longer declares its name.
    UnreferencedName,
}

impl EntryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Orphaned => "orphaned",
            Self::UnreferencedName => "unreferenced name",
        }
    }

    /// Whether `prune` removes it.
    pub fn is_obsolete(self) -> bool {
        self != Self::Ok
    }
}

/// One snapshot file of the workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotEntry {
    /// The snapshot file, relative to the workspace, with `/`.
    pub path: String,
    /// The request file it belongs to, relative to the workspace, with `/`.
    pub request: String,
    pub name: String,
    pub state: EntryState,
    /// The snapshot file on disk.
    #[serde(skip)]
    pub file: PathBuf,
    /// Its text when listed (`None`: unreadable). `prune` keeps a file whose
    /// text changed since, as another writer may have just recorded it.
    #[serde(skip)]
    pub content: Option<String>,
}

/// Every snapshot file under `scope` (a folder or a request file, relative
/// to the workspace root; empty for all), sorted by path.
pub fn inventory(ws: &Workspace, scope: &Path) -> io::Result<Vec<SnapshotEntry>> {
    let declared = declared_names(ws);
    let full = ws.root.join(scope);
    let (start, only) = if full.is_file() {
        let parent = scope.parent().unwrap_or(Path::new("")).to_path_buf();
        (parent, Some(slash(scope)))
    } else {
        (scope.to_path_buf(), None)
    };
    let mut entries = Vec::new();
    walk(&ws.root, &start, &declared, &mut entries)?;
    if let Some(request) = only {
        entries.retain(|e| e.request == request);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// Delete every obsolete entry of `entries`, then the `__snapshots__`
/// folders left empty. Returns the deleted entries. Each file is deleted
/// under its snapshot lock and only while it still holds the text it was
/// listed with; a file written meanwhile is kept.
pub fn prune(entries: &[SnapshotEntry]) -> io::Result<Vec<SnapshotEntry>> {
    let mut deleted = Vec::new();
    for entry in entries.iter().filter(|e| e.state.is_obsolete()) {
        if !remove_unchanged(entry)? {
            continue;
        }
        if let Some(dir) = entry.file.parent() {
            // Fails, as wanted, while the folder still holds a file.
            let _ = fs::remove_dir(dir);
        }
        deleted.push(entry.clone());
    }
    Ok(deleted)
}

/// Delete the file of `entry` if it still holds its listed text.
fn remove_unchanged(entry: &SnapshotEntry) -> io::Result<bool> {
    let Some(listed) = entry.content.as_deref() else {
        return Ok(false);
    };
    let _lock = SnapshotLock::acquire(&entry.file)?;
    match read_snapshot(&entry.file)? {
        Some(current) if same_text(&current, listed) => {
            fs::remove_file(&entry.file)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The snapshot names each loaded request file declares, by its path
/// relative to the workspace (with `/`); `None` when they cannot be known.
fn declared_names(ws: &Workspace) -> BTreeMap<String, Option<Vec<String>>> {
    ws.requests()
        .iter()
        .map(|r| (slash(&r.entry.path), snapshot_names(r.request())))
        .collect()
}

/// The names of the snapshot assertions of `request`.
pub fn snapshot_names(request: &Request) -> Option<Vec<String>> {
    let assertions = match request {
        Request::Http(http) => http.assertions.as_deref(),
        Request::Graphql(graphql) => graphql.assertions.as_deref(),
        // JSON-RPC and gRPC calls have no snapshot assertions.
        Request::Jsonrpc(_) | Request::Grpc(_) => return Some(Vec::new()),
        Request::Mcp(mcp) => mcp.assertions.as_deref(),
        Request::Opaque(_) => return None,
    };
    let names = assertions
        .unwrap_or_default()
        .iter()
        .filter_map(|a| match a {
            Assertion::Snapshot(spec) => Some(spec.name().to_owned()),
            _ => None,
        });
    Some(names.collect())
}

/// Collect the snapshots of folder `rel` and of every collection folder
/// below it.
fn walk(
    root: &Path,
    rel: &Path,
    declared: &BTreeMap<String, Option<Vec<String>>>,
    out: &mut Vec<SnapshotEntry>,
) -> io::Result<()> {
    let dir = root.join(rel);
    let snapshots = dir.join(SNAPSHOT_DIR);
    if snapshots.is_dir() {
        let stems = request_stems(&dir)?;
        for file in snapshot_files(&snapshots)? {
            out.push(entry(rel, &snapshots, &file, &stems, declared));
        }
    }
    for child in fs::read_dir(&dir)? {
        let child = child?;
        let name = child.file_name().to_string_lossy().into_owned();
        if child.file_type()?.is_dir() && !is_tool_dir(rel, &name) {
            walk(root, &rel.join(&name), declared, out)?;
        }
    }
    Ok(())
}

/// The stems of the request files in `dir` (loaded or not).
fn request_stems(dir: &Path) -> io::Result<Vec<String>> {
    let mut stems = Vec::new();
    for item in fs::read_dir(dir)? {
        let name = item?.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(REQUEST_FILE_SUFFIX) {
            stems.push(stem.to_owned());
        }
    }
    Ok(stems)
}

/// The names of the snapshot files in a `__snapshots__` folder.
fn snapshot_files(dir: &Path) -> io::Result<Vec<String>> {
    let mut files = Vec::new();
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if item.file_type()?.is_file() && name.ends_with(SNAPSHOT_SUFFIX) {
            files.push(name);
        }
    }
    Ok(files)
}

fn entry(
    rel: &Path,
    snapshots: &Path,
    file: &str,
    stems: &[String],
    declared: &BTreeMap<String, Option<Vec<String>>>,
) -> SnapshotEntry {
    let base = file.strip_suffix(SNAPSHOT_SUFFIX).unwrap_or(file);
    // Snapshot names have no dot, so the name follows the last one.
    let (stem, name) = base.rsplit_once('.').unwrap_or((base, ""));
    let request = slash(&rel.join(format!("{stem}{REQUEST_FILE_SUFFIX}")));
    let state = if !stems.iter().any(|s| s == stem) {
        EntryState::Orphaned
    } else if declared
        .get(&request)
        .is_some_and(|names| names.as_ref().is_some_and(|n| !n.iter().any(|x| x == name)))
    {
        EntryState::UnreferencedName
    } else {
        EntryState::Ok
    };
    let file_path = snapshots.join(file);
    SnapshotEntry {
        path: slash(&rel.join(SNAPSHOT_DIR).join(file)),
        request,
        name: name.to_owned(),
        state,
        content: fs::read_to_string(&file_path).ok(),
        file: file_path,
    }
}

/// A relative path with `/` on every platform.
fn slash(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    text.trim_start_matches("./").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("rustman.toml"),
            "[workspace]\nversion = 1\nname = \"t\"\n",
        )
        .unwrap();
        let users = root.join("users");
        fs::create_dir_all(users.join(SNAPSHOT_DIR)).unwrap();
        let request = "version: 1\nkind: http\nname: Get user\nmethod: GET\nurl: http://h/u\nassertions:\n  - snapshot\n  - snapshot: {name: v2_list}\n";
        fs::write(users.join("get-user.rman.yaml"), request).unwrap();
        for file in [
            "get-user.default.snap.yaml",
            "get-user.v2_list.snap.yaml",
            "get-user.gone.snap.yaml",
            "old.default.snap.yaml",
        ] {
            fs::write(users.join(SNAPSHOT_DIR).join(file), "version: 1\n").unwrap();
        }
        fs::write(users.join(SNAPSHOT_DIR).join("notes.txt"), "x").unwrap();
        let ws = Workspace::load(root).unwrap();
        (tmp, ws)
    }

    fn states(entries: &[SnapshotEntry]) -> Vec<(&str, &str, &str)> {
        entries
            .iter()
            .map(|e| (e.path.as_str(), e.name.as_str(), e.state.as_str()))
            .collect()
    }

    #[test]
    fn every_snapshot_file_gets_a_state() {
        let (_tmp, ws) = workspace();
        let entries = inventory(&ws, Path::new("")).unwrap();
        assert_eq!(
            states(&entries),
            [
                (
                    "users/__snapshots__/get-user.default.snap.yaml",
                    "default",
                    "ok"
                ),
                (
                    "users/__snapshots__/get-user.gone.snap.yaml",
                    "gone",
                    "unreferenced name"
                ),
                (
                    "users/__snapshots__/get-user.v2_list.snap.yaml",
                    "v2_list",
                    "ok"
                ),
                (
                    "users/__snapshots__/old.default.snap.yaml",
                    "default",
                    "orphaned"
                ),
            ]
        );
        assert_eq!(entries[3].request, "users/old.rman.yaml");
    }

    #[test]
    fn a_scope_limits_the_listing() {
        let (_tmp, ws) = workspace();
        let file = inventory(&ws, Path::new("users/get-user.rman.yaml")).unwrap();
        assert_eq!(file.len(), 3);
        let folder = inventory(&ws, Path::new("users")).unwrap();
        assert_eq!(folder.len(), 4);
    }

    #[test]
    fn prune_deletes_only_obsolete_files() {
        let (_tmp, ws) = workspace();
        let entries = inventory(&ws, Path::new("")).unwrap();
        let deleted = prune(&entries).unwrap();
        assert_eq!(deleted.len(), 2);
        let left = inventory(&ws, Path::new("")).unwrap();
        assert!(left.iter().all(|e| e.state == EntryState::Ok));
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn prune_keeps_a_file_written_since_it_was_listed() {
        let (_tmp, ws) = workspace();
        let entries = inventory(&ws, Path::new("")).unwrap();
        let obsolete: Vec<_> = entries.iter().filter(|e| e.state.is_obsolete()).collect();
        let (changed, other) = (obsolete[0], obsolete[1]);
        fs::write(&changed.file, "version: 1\nrecorded: meanwhile\n").unwrap();
        let deleted = prune(&entries).unwrap();
        assert_eq!(deleted, vec![other.clone()]);
        assert!(changed.file.exists());
        assert!(!other.file.exists());
    }

    #[test]
    fn an_emptied_snapshot_folder_is_removed() {
        let (tmp, ws) = workspace();
        let dir = tmp.path().join("users").join(SNAPSHOT_DIR);
        for file in [
            "get-user.default.snap.yaml",
            "get-user.v2_list.snap.yaml",
            "notes.txt",
        ] {
            fs::remove_file(dir.join(file)).unwrap();
        }
        let entries = inventory(&ws, Path::new("")).unwrap();
        prune(&entries).unwrap();
        assert!(!dir.exists());
    }
}
