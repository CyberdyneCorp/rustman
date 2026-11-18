//! Hot reload of `webhook/mocks.yaml` (design D6): file changes under the
//! workspace's `webhook/` folder reload the mocks after a 200 ms debounce.
//! Each reload is reported on the event log; an invalid file keeps the
//! previous mocks.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher as _};
use tokio::sync::mpsc;

use crate::event::Event;
use crate::mocks::Mocks;
use crate::store::CaptureStore;

/// Quiet time after the last change before reloading.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// A live watch; dropping it stops reloading.
pub struct MockWatcher {
    _inner: Arc<Mutex<notify::RecommendedWatcher>>,
}

impl std::fmt::Debug for MockWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MockWatcher")
    }
}

/// Watch the workspace at `root` and reload `mocks` when anything under its
/// `webhook/` folder changes, reporting on `store`'s event log. Must be
/// called inside a Tokio runtime.
///
/// Only the root itself (to see `webhook/` appear) and `webhook/` are
/// watched, never the rest of the workspace: a large tree such as
/// `target/` costs nothing.
pub fn watch(root: &Path, mocks: Mocks, store: CaptureStore) -> notify::Result<MockWatcher> {
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let dir = root.join("webhook");
    let (tx, rx) = mpsc::unbounded_channel();
    let filter = dir.clone();
    let mut inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if !matches!(event.kind, EventKind::Access(_))
            && event.paths.iter().any(|p| under(&filter, p))
        {
            let _ = tx.send(());
        }
    })?;
    inner.watch(&root, RecursiveMode::NonRecursive)?;
    let inner = Arc::new(Mutex::new(inner));
    let mut folder = Folder {
        dir,
        watcher: Arc::downgrade(&inner),
        watched: false,
    };
    folder.follow();
    tokio::spawn(reload_on_change(rx, folder, mocks, store));
    Ok(MockWatcher { _inner: inner })
}

/// The `webhook/` folder, watched recursively while it exists.
struct Folder {
    dir: PathBuf,
    /// Weak, so dropping the [`MockWatcher`] ends the reload task.
    watcher: Weak<Mutex<notify::RecommendedWatcher>>,
    watched: bool,
}

impl Folder {
    /// Watch the folder once it exists; forget the watch once it is gone
    /// (a recreated folder is watched again).
    fn follow(&mut self) {
        let exists = self.dir.is_dir();
        if exists == self.watched {
            return;
        }
        let Some(watcher) = self.watcher.upgrade() else {
            return;
        };
        let mut watcher = watcher.lock().unwrap_or_else(PoisonError::into_inner);
        self.watched = if exists {
            watcher.watch(&self.dir, RecursiveMode::Recursive).is_ok()
        } else {
            let _ = watcher.unwatch(&self.dir);
            false
        };
    }
}

fn under(dir: &Path, path: &Path) -> bool {
    path.starts_with(dir) || canonical_parent(path).is_some_and(|p| p.starts_with(dir))
}

/// The canonical form of `path`'s parent (the path itself may be gone).
fn canonical_parent(path: &Path) -> Option<PathBuf> {
    let parent = dunce::canonicalize(path.parent()?).ok()?;
    Some(parent.join(path.file_name()?))
}

async fn reload_on_change(
    mut changes: mpsc::UnboundedReceiver<()>,
    mut folder: Folder,
    mocks: Mocks,
    store: CaptureStore,
) {
    while changes.recv().await.is_some() {
        tokio::time::sleep(DEBOUNCE).await;
        while changes.try_recv().is_ok() {}
        folder.follow();
        store.emit(reload(&mocks));
    }
}

/// Reload `mocks` and describe the outcome as an event.
pub fn reload(mocks: &Mocks) -> Event {
    match mocks.reload() {
        Ok(count) => Event::MocksReloaded { count },
        Err(e) => Event::MocksInvalid {
            message: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changes_under_the_webhook_folder_count() {
        let dir = Path::new("/w/webhook");
        assert!(under(dir, Path::new("/w/webhook/mocks.yaml")));
        assert!(under(dir, Path::new("/w/webhook")));
        assert!(!under(dir, Path::new("/w/requests/a.rman.yaml")));
        assert!(!under(dir, Path::new("/w/webhooks.yaml")));
    }

    #[test]
    fn reload_reports_count_or_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mocks.yaml");
        std::fs::write(&file, "mocks:\n- {path: /a}\n").unwrap();
        let mocks = Mocks::from_file(&file).unwrap();
        assert_eq!(reload(&mocks), Event::MocksReloaded { count: 1 });
        std::fs::write(&file, "nope: 1\n").unwrap();
        assert!(
            matches!(reload(&mocks), Event::MocksInvalid { message } if message.contains("nope"))
        );
    }
}
