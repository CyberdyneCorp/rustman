//! Locating the workspace root.

use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::settings::SETTINGS_FILE;

/// Find the workspace root.
///
/// With `explicit` (`--workspace <dir>`), that directory must contain
/// `rustman.toml`; a relative `dir` is taken from `start`, and the root is
/// returned canonical so `..` and symlinks in it are resolved. Otherwise
/// `start` and then its ancestors are searched, and the first directory
/// containing `rustman.toml` wins.
pub fn find_root(start: &Path, explicit: Option<&Path>) -> Result<PathBuf, ConfigError> {
    if let Some(dir) = explicit {
        let dir = start.join(dir);
        return dir
            .canonicalize()
            .ok()
            .filter(|root| is_workspace(root))
            .ok_or(ConfigError::NoWorkspace);
    }
    start
        .ancestors()
        .find(|dir| is_workspace(dir))
        .map(Path::to_path_buf)
        .ok_or(ConfigError::NoWorkspace)
}

pub fn is_workspace(dir: &Path) -> bool {
    dir.join(SETTINGS_FILE).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn nested_folder_finds_the_nearest_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let nested = root.join("api/users");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join(SETTINGS_FILE), "").unwrap();
        assert_eq!(find_root(&nested, None).unwrap(), root);
        assert_eq!(find_root(&root, None).unwrap(), root);
    }

    #[test]
    fn no_workspace_is_an_error_with_the_spec_text() {
        let tmp = tempfile::tempdir().unwrap();
        let err = find_root(tmp.path(), None).unwrap_err();
        assert!(matches!(err, ConfigError::NoWorkspace));
        assert_eq!(
            err.to_string(),
            "no rustman workspace found (run 'rustman init')"
        );
    }

    #[test]
    fn explicit_workspace_overrides_the_search() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        let other = outer.join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(outer.join(SETTINGS_FILE), "").unwrap();
        fs::write(other.join(SETTINGS_FILE), "").unwrap();
        let other_real = other.canonicalize().unwrap();
        fs::create_dir_all(other.join("sub")).unwrap();
        assert_eq!(find_root(outer, Some(&other)).unwrap(), other_real);
        assert_eq!(
            find_root(&other.join("sub"), Some(Path::new(".."))).unwrap(),
            other_real
        );
        let nested = other.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            find_root(&nested, Some(Path::new("../.."))).unwrap(),
            other_real
        );
        assert_eq!(
            find_root(&nested, Some(Path::new("../../../other"))).unwrap(),
            other_real
        );
        let empty = outer.join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(matches!(
            find_root(outer, Some(&empty)),
            Err(ConfigError::NoWorkspace)
        ));
    }
}
