//! Writing a generated collection: all files appear at once, or none do.

use std::path::{Path, PathBuf};

use super::Generated;

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("{} already exists and is not empty; import into another folder with --into", .0.display())]
    Exists(PathBuf),
    #[error("cannot write {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Write `generated` as the folder `target`, which must not exist or be
/// empty. Files are written to a temporary sibling folder that is then
/// renamed, so a failure leaves nothing behind.
pub fn write(generated: &Generated, target: &Path) -> Result<(), WriteError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| WriteError::Io { path, source }
    };
    if target.exists() {
        let mut entries = std::fs::read_dir(target).map_err(io(target))?;
        if entries.next().is_some() {
            return Err(WriteError::Exists(target.to_path_buf()));
        }
    }
    let parent = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent).map_err(io(&parent))?;
    let staging = tempfile::Builder::new()
        .prefix(".rustman-import-")
        .tempdir_in(&parent)
        .map_err(io(&parent))?;
    for file in &generated.files {
        let path = file
            .path
            .split('/')
            .fold(staging.path().to_path_buf(), |p, s| p.join(s));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io(dir))?;
        }
        std::fs::write(&path, &file.contents).map_err(io(&path))?;
    }
    if target.exists() {
        std::fs::remove_dir(target).map_err(io(target))?;
    }
    std::fs::rename(staging.path(), target).map_err(io(target))?;
    // The folder now lives at `target`; nothing is left to clean up.
    let _ = staging.keep();
    Ok(())
}
