//! File operations behind the sidebar and saving (run by the effect runner,
//! never on the render loop).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rustman_core::model::{KindRegistry, Request};

/// A change to the workspace's files. Paths are absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Write a request file, replacing it (saving a tab). Missing folders
    /// are created.
    Write { path: PathBuf, text: String },
    /// Write a new request file; fails if it exists. Missing folders are
    /// created.
    Create { path: PathBuf, text: String },
    /// Rename a request file or folder; fails if the target exists.
    Rename { from: PathBuf, to: PathBuf },
    /// Delete a request file, or a folder with everything in it.
    Delete { path: PathBuf },
}

impl FileOp {
    /// Perform the operation.
    pub fn apply(&self) -> io::Result<()> {
        match self {
            Self::Write { path, text } => write(path, text),
            Self::Create { path, text } => create(path, text),
            Self::Rename { from, to } => rename(from, to),
            Self::Delete { path } => delete(path),
        }
    }
}

fn write(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)
}

fn create(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    io::Write::write_all(&mut file, text.as_bytes())
}

fn rename(from: &Path, to: &Path) -> io::Result<()> {
    if to.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        ));
    }
    fs::rename(from, to)
}

fn delete(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// The registry the TUI loads request files with: unknown kinds load as
/// opaque requests (shown read-only) instead of failing.
pub fn registry() -> KindRegistry {
    KindRegistry::default().with_opaque_fallback()
}

/// Read and parse the request file `rel` of the workspace at `root`.
pub fn read_request(root: &Path, rel: &Path) -> Result<Request, String> {
    let text = fs::read_to_string(root.join(rel)).map_err(|e| e.to_string())?;
    registry().load(rel, &text).map_err(|e| e.to_string())
}

/// A request file name from what the user typed: `.rman.yaml` added, `/`
/// as the separator on every OS. `None` when it is empty or leaves the
/// folder (`..`, absolute).
pub fn request_file_name(typed: &str) -> Option<PathBuf> {
    let typed = typed
        .trim()
        .trim_end_matches(rustman_core::model::REQUEST_FILE_SUFFIX);
    let parts: Vec<&str> = typed.split(['/', '\\']).collect();
    let bad = |p: &&str| p.is_empty() || *p == "." || *p == ".." || p.contains(':');
    if typed.is_empty() || parts.iter().any(bad) {
        return None;
    }
    let mut path: PathBuf = parts.iter().collect();
    let name = format!(
        "{}{}",
        path.file_name()?.to_string_lossy(),
        rustman_core::model::REQUEST_FILE_SUFFIX
    );
    path.set_file_name(name);
    Some(path)
}

/// The text of a new request file named `name`.
pub fn new_request_text(name: &str) -> String {
    let name = name.replace('\'', "''");
    format!("version: 1\nkind: http\nname: '{name}'\nmethod: GET\nurl: ''\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_rename_write_and_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a/b/new.rman.yaml");
        let create = FileOp::Create {
            path: file.clone(),
            text: new_request_text("It's new"),
        };
        create.apply().unwrap();
        assert!(create.apply().is_err(), "never overwrites");
        let request = read_request(tmp.path(), Path::new("a/b/new.rman.yaml")).unwrap();
        assert_eq!(request.name(), "It's new");
        let to = tmp.path().join("a/c");
        FileOp::Rename {
            from: tmp.path().join("a/b"),
            to: to.clone(),
        }
        .apply()
        .unwrap();
        let moved = to.join("new.rman.yaml");
        FileOp::Write {
            path: moved.clone(),
            text: "x".into(),
        }
        .apply()
        .unwrap();
        assert_eq!(fs::read_to_string(&moved).unwrap(), "x");
        let clash = FileOp::Rename {
            from: moved.clone(),
            to: moved.clone(),
        };
        assert!(clash.apply().is_err());
        FileOp::Delete { path: to.clone() }.apply().unwrap();
        assert!(!to.exists());
        assert!(read_request(tmp.path(), Path::new("gone.rman.yaml")).is_err());
    }

    #[test]
    fn unknown_kinds_read_as_opaque() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("g.rman.yaml"),
            "version: 1\nkind: grpc\nname: G\n",
        )
        .unwrap();
        let request = read_request(tmp.path(), Path::new("g.rman.yaml")).unwrap();
        assert_eq!(request.kind(), "grpc");
    }

    #[test]
    fn typed_names_become_request_files() {
        let name = |s: &str| request_file_name(s).map(|p| p.to_string_lossy().replace('\\', "/"));
        assert_eq!(name("get-user").as_deref(), Some("get-user.rman.yaml"));
        assert_eq!(
            name("admin/list.rman.yaml").as_deref(),
            Some("admin/list.rman.yaml")
        );
        for bad in ["", "  ", "../x", "a//b", "/abs", "c:x"] {
            assert_eq!(name(bad), None, "{bad}");
        }
    }
}
