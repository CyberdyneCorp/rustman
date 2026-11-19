//! Hot reload (spec: Hot reload; design D2): changes to the document or
//! to the mocks directory reload the served state after a 200 ms quiet
//! time. Only the document's folder (not recursively) and the mocks
//! directory are watched. A document given as a URL is not watched.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher as _};
use tokio::sync::mpsc;

use crate::server::Reloader;

/// Quiet time after the last change before reloading.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// A live watch; dropping it stops reloading.
pub struct ReloadWatcher {
    _inner: Arc<Mutex<notify::RecommendedWatcher>>,
}

impl std::fmt::Debug for ReloadWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReloadWatcher")
    }
}

/// What a change must touch to cause a reload.
#[derive(Clone, Debug)]
struct Targets {
    document: Option<PathBuf>,
    mocks: Option<PathBuf>,
}

impl Targets {
    fn wanted(&self, path: &Path) -> bool {
        let path = canonical(path);
        self.document.as_ref().is_some_and(|d| *d == path)
            || self.mocks.as_ref().is_some_and(|m| path.starts_with(m))
    }
}

/// The canonical form of `path`, or of its parent joined with its name
/// when the path itself is gone (a deleted or renamed file).
fn canonical(path: &Path) -> PathBuf {
    if let Ok(found) = dunce::canonicalize(path) {
        return found;
    }
    let parent = path.parent().and_then(|p| dunce::canonicalize(p).ok());
    match (parent, path.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// Watch the files of `reloader`'s source and reload on change. Must be
/// called inside a Tokio runtime.
pub fn watch(reloader: Reloader) -> notify::Result<ReloadWatcher> {
    let source = &reloader.source;
    let document = Path::new(&source.document);
    let document = document.is_file().then(|| canonical(document));
    let mocks = source.mocks_dir.as_deref().map(canonical);
    let targets = Targets {
        document: document.clone(),
        mocks: mocks.clone(),
    };
    let (tx, rx) = mpsc::unbounded_channel();
    let filter = targets.clone();
    let mut inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if !matches!(event.kind, EventKind::Access(_))
            && event.paths.iter().any(|p| filter.wanted(p))
        {
            let _ = tx.send(());
        }
    })?;
    let mut folders: Vec<&Path> = [document.as_deref(), mocks.as_deref()]
        .into_iter()
        .flatten()
        .filter_map(Path::parent)
        .collect();
    folders.dedup();
    for folder in folders {
        inner.watch(folder, RecursiveMode::NonRecursive)?;
    }
    let inner = Arc::new(Mutex::new(inner));
    let mut folder = Folder {
        dir: mocks,
        watcher: Arc::downgrade(&inner),
        watched: false,
    };
    folder.follow();
    tokio::spawn(reload_on_change(rx, folder, reloader));
    Ok(ReloadWatcher { _inner: inner })
}

/// The mocks directory, watched recursively while it exists.
struct Folder {
    dir: Option<PathBuf>,
    /// Weak, so dropping the [`ReloadWatcher`] ends the reload task.
    watcher: Weak<Mutex<notify::RecommendedWatcher>>,
    watched: bool,
}

impl Folder {
    fn follow(&mut self) {
        let Some(dir) = &self.dir else { return };
        let exists = dir.is_dir();
        if exists == self.watched {
            return;
        }
        let Some(watcher) = self.watcher.upgrade() else {
            return;
        };
        let mut watcher = watcher.lock().unwrap_or_else(PoisonError::into_inner);
        self.watched = if exists {
            watcher.watch(dir, RecursiveMode::Recursive).is_ok()
        } else {
            let _ = watcher.unwatch(dir);
            false
        };
    }
}

async fn reload_on_change(
    mut changes: mpsc::UnboundedReceiver<()>,
    mut folder: Folder,
    reloader: Reloader,
) {
    while changes.recv().await.is_some() {
        tokio::time::sleep(DEBOUNCE).await;
        while changes.try_recv().is_ok() {}
        if folder.watcher.strong_count() == 0 {
            return;
        }
        folder.follow();
        reloader.reload().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_document_and_the_mocks_folder_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical(dir.path());
        std::fs::create_dir(root.join("mocks")).unwrap();
        let targets = Targets {
            document: Some(root.join("api.yaml")),
            mocks: Some(root.join("mocks")),
        };
        assert!(targets.wanted(&root.join("api.yaml")));
        assert!(targets.wanted(&root.join("mocks").join("a.rman-mock.yaml")));
        assert!(!targets.wanted(&root.join("other.yaml")));
        assert!(!targets.wanted(&root.join("mocks-old")));
    }
}
