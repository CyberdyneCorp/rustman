//! `.rustman/workspace-id`: a UUID generated once per clone, so two clones of
//! one workspace on the same machine never share keychain entries (design D6).

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use super::{SecretError, git, private_file};
use crate::error::LoadError;

/// The id file, relative to the workspace root.
pub const WORKSPACE_ID_FILE: &str = ".rustman/workspace-id";

/// The workspace id, or `None` when it was never created.
pub fn load(root: &Path) -> Result<Option<String>, SecretError> {
    let path = root.join(WORKSPACE_ID_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(SecretError::io(&path, e)),
    };
    let id = text.trim();
    match uuid::Uuid::try_parse(id) {
        Ok(uuid) => Ok(Some(uuid.hyphenated().to_string())),
        Err(_) => Err(LoadError::new(
            Path::new(WORKSPACE_ID_FILE),
            None,
            "invalid workspace id: expected a UUID",
        )
        .into()),
    }
}

/// The workspace id, created on first use.
pub fn load_or_create(root: &Path) -> Result<String, SecretError> {
    if let Some(id) = load(root)? {
        return Ok(id);
    }
    git::ensure_private(root, WORKSPACE_ID_FILE)?;
    let path = root.join(WORKSPACE_ID_FILE);
    let id = uuid::Uuid::new_v4().hyphenated().to_string();
    match private_file::write_new(&path, format!("{id}\n").as_bytes()) {
        Ok(()) => Ok(id),
        // Another process created it first, complete: use theirs.
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            load(root)?.ok_or_else(|| SecretError::io(&path, ErrorKind::NotFound.into()))
        }
        Err(e) => Err(SecretError::io(&path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::STATE_DIR;

    #[test]
    fn created_once_then_reused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), None);
        let id = load_or_create(dir.path()).unwrap();
        assert!(uuid::Uuid::try_parse(&id).is_ok());
        assert_eq!(load_or_create(dir.path()).unwrap(), id);
        assert_eq!(load(dir.path()).unwrap(), Some(id.clone()));
        assert_eq!(
            fs::read_to_string(dir.path().join(WORKSPACE_ID_FILE)).unwrap(),
            format!("{id}\n")
        );
    }

    #[test]
    fn concurrent_creators_agree_on_one_id() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let ids: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| load_or_create(&root).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(ids.iter().all(|id| *id == ids[0]), "{ids:?}");
        assert_eq!(load(&root).unwrap(), Some(ids[0].clone()));
    }

    #[test]
    fn two_clones_get_different_ids() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(
            load_or_create(a.path()).unwrap(),
            load_or_create(b.path()).unwrap()
        );
    }

    #[test]
    fn corrupt_id_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(STATE_DIR)).unwrap();
        fs::write(dir.path().join(WORKSPACE_ID_FILE), "nope\n").unwrap();
        assert_eq!(
            load(dir.path()).unwrap_err().to_string(),
            ".rustman/workspace-id: invalid workspace id: expected a UUID"
        );
    }

    #[test]
    fn not_created_when_the_state_folder_is_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        git::tests::git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        assert!(matches!(
            load_or_create(dir.path()),
            Err(SecretError::NotIgnored)
        ));
        assert!(!dir.path().join(STATE_DIR).exists());
    }
}
