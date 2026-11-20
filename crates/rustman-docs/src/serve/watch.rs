//! Rebuilding on workspace changes: the debounced `notify` watcher of
//! `rustman mock` (`crates/rustman-mock/src/watch.rs`), over the whole
//! workspace folder except `.git`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher as _};
use tokio::sync::mpsc;

use super::state::Shared;
use crate::error::DocsError;

/// Quiet time after the last change before rebuilding.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// A live watch; dropping it stops rebuilding.
pub struct PreviewWatcher {
    _inner: notify::RecommendedWatcher,
}

impl std::fmt::Debug for PreviewWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreviewWatcher")
    }
}

/// Changes that never affect the docs.
fn ignored(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".git")
}

pub(crate) fn watch(root: &Path, shared: Arc<Shared>) -> Result<PreviewWatcher, DocsError> {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if !matches!(event.kind, EventKind::Access(_)) && event.paths.iter().any(|p| !ignored(p)) {
            let _ = tx.send(());
        }
    })
    .map_err(|e| watch_error(root, &e))?;
    inner
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| watch_error(root, &e))?;
    tokio::spawn(rebuild_on_change(rx, shared));
    Ok(PreviewWatcher { _inner: inner })
}

/// Rebuild once changes stop for [`DEBOUNCE`]. Ends when the watcher (the
/// only sender) is dropped.
async fn rebuild_on_change(mut changes: mpsc::UnboundedReceiver<()>, shared: Arc<Shared>) {
    while changes.recv().await.is_some() {
        loop {
            match tokio::time::timeout(DEBOUNCE, changes.recv()).await {
                Ok(Some(())) => {}
                Ok(None) => return,
                Err(_) => break,
            }
        }
        shared.rebuild().await;
    }
}

fn watch_error(root: &Path, err: &notify::Error) -> DocsError {
    DocsError::Usage(format!("cannot watch {}: {err}", root.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_internals_are_ignored() {
        assert!(ignored(Path::new("/w/.git/index")));
        assert!(!ignored(Path::new("/w/users/list.rman.yaml")));
        assert!(!ignored(Path::new("/w/.gitignore")));
    }
}
