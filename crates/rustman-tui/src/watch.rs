//! Watching the open workspace for changes made by other programs (spec:
//! Unsaved change tracking, "External edit conflict"). Changed workspace
//! files arrive as [`Action::FilesChanged`] with workspace-relative paths.

use std::path::{Component, Path, PathBuf};

use notify::{EventKind, RecursiveMode, Watcher as _};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::Action;

/// A live watch on one workspace; dropping it stops watching.
pub struct Watcher {
    pub root: PathBuf,
    _inner: notify::RecommendedWatcher,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").field("root", &self.root).finish()
    }
}

/// Watch `root` recursively, sending changed workspace files to `actions`.
pub fn watch(root: &Path, actions: UnboundedSender<Action>) -> notify::Result<Watcher> {
    let canonical = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let base = canonical.clone();
    let mut inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let paths: Vec<PathBuf> = event
            .paths
            .iter()
            .filter_map(|p| relevant(&base, p))
            .collect();
        if !paths.is_empty() {
            let _ = actions.send(Action::FilesChanged(paths));
        }
    })?;
    inner.watch(&canonical, RecursiveMode::Recursive)?;
    Ok(Watcher {
        root: root.to_path_buf(),
        _inner: inner,
    })
}

/// The workspace-relative path of a changed file the TUI cares about:
/// request files, `_collection.yaml`, environments and `rustman.toml`;
/// nothing under `.rustman/` or `.git/`.
pub fn relevant(root: &Path, path: &Path) -> Option<PathBuf> {
    let path = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let rel = path.strip_prefix(root).ok()?;
    let first = rel.components().next()?;
    if let Component::Normal(name) = first
        && (name == ".rustman" || name == ".git")
    {
        return None;
    }
    let name = rel.file_name()?.to_string_lossy();
    let wanted = name.ends_with(rustman_core::model::REQUEST_FILE_SUFFIX)
        || name == "_collection.yaml"
        || name == "rustman.toml"
        || rel.starts_with("environments");
    wanted.then(|| rel.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn only_workspace_files_are_relevant() {
        let root = Path::new("/w");
        let rel = |p: &str| relevant(root, Path::new(p));
        assert_eq!(
            rel("/w/a/b.rman.yaml"),
            Some(PathBuf::from("a/b.rman.yaml"))
        );
        assert_eq!(
            rel("/w/a/_collection.yaml"),
            Some(PathBuf::from("a/_collection.yaml"))
        );
        assert_eq!(
            rel("/w/environments/dev.yaml"),
            Some(PathBuf::from("environments/dev.yaml"))
        );
        assert_eq!(rel("/w/rustman.toml"), Some(PathBuf::from("rustman.toml")));
        assert_eq!(rel("/w/.rustman/history/x.rman.yaml"), None);
        assert_eq!(rel("/w/.git/index"), None);
        assert_eq!(rel("/w/notes.txt"), None);
        assert_eq!(rel("/elsewhere/a.rman.yaml"), None);
    }

    #[tokio::test]
    async fn an_external_modification_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("get-user.rman.yaml");
        std::fs::write(&file, "version: 1\n").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _watcher = watch(tmp.path(), tx).unwrap();
        // Writes keep coming until one is seen: some backends start late.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            std::fs::write(&file, "version: 1\nkind: http\n").unwrap();
            let got = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
            if let Ok(Some(Action::FilesChanged(paths))) = got {
                assert!(
                    paths.contains(&PathBuf::from("get-user.rman.yaml")),
                    "{paths:?}"
                );
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "no change event");
        }
    }
}
