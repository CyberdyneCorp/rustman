//! Roots (mcp-client spec "Roots"): the `file://` URIs answered to
//! `roots/list`, from the definition's `roots` (default
//! `["{{workspace}}"]`), relative paths taken from the workspace root.

use std::path::Path;

use rmcp::model::Root;
use url::Url;

use super::error::ClientError;

/// The template of the workspace root in `roots`.
pub const WORKSPACE: &str = "{{workspace}}";

/// The roots of a definition without `roots`.
pub fn default_roots() -> Vec<String> {
    vec![WORKSPACE.to_owned()]
}

/// `written` as root URIs: `file://` URIs are kept, `{{workspace}}` is the
/// workspace root, and other paths are taken from it.
pub fn root_uris(written: &[String], workspace: &Path) -> Result<Vec<Root>, ClientError> {
    written.iter().map(|w| root(w, workspace)).collect()
}

fn root(written: &str, workspace: &Path) -> Result<Root, ClientError> {
    if written.starts_with("file://") {
        let uri = Url::parse(written)
            .map_err(|e| ClientError::Invalid(format!("invalid root {written}: {e}")))?;
        return Ok(Root::new(uri.as_str()));
    }
    let path = match written.strip_prefix(WORKSPACE) {
        Some(rest) => workspace.join(rest.trim_start_matches(['/', '\\'])),
        None => workspace.join(written),
    };
    let invalid = || ClientError::Invalid(format!("invalid root {written}"));
    let path = std::path::absolute(&path).map_err(|_| invalid())?;
    let uri = Url::from_file_path(&path).map_err(|()| invalid())?;
    Ok(Root::new(uri.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_root_is_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let roots = root_uris(&default_roots(), dir.path()).unwrap();
        assert_eq!(roots.len(), 1);
        let expected = Url::from_file_path(dir.path()).unwrap();
        assert_eq!(roots[0].uri, expected.as_str());
        assert!(roots[0].uri.starts_with("file://"));
    }

    #[test]
    fn relative_paths_and_uris() {
        let dir = tempfile::tempdir().unwrap();
        let written = vec![
            "docs".to_owned(),
            "{{workspace}}/src".to_owned(),
            "file:///srv/data".to_owned(),
        ];
        let roots = root_uris(&written, dir.path()).unwrap();
        assert_eq!(
            roots[0].uri,
            Url::from_file_path(dir.path().join("docs"))
                .unwrap()
                .as_str()
        );
        assert_eq!(
            roots[1].uri,
            Url::from_file_path(dir.path().join("src"))
                .unwrap()
                .as_str()
        );
        assert_eq!(roots[2].uri, "file:///srv/data");
        assert!(root_uris(&["file://[bad".to_owned()], dir.path()).is_err());
        let relative = root_uris(&default_roots(), Path::new(".")).unwrap();
        assert!(relative[0].uri.starts_with("file://"), "{:?}", relative[0]);
    }
}
