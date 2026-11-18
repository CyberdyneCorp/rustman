//! The request files of an imported collection, matched to operations by
//! the `openapi:operationId=` / `openapi:path=` tags the import wrote.

use std::path::Path;

use rustman_core::model::{HttpRequest, KindRegistry, REQUEST_FILE_SUFFIX, Request};

use crate::collection::{OPERATION_TAG, PATH_TAG};

/// The folder removed operations are moved to.
pub const REMOVED_DIR: &str = "_removed";

/// One request file that came from an operation.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalRequest {
    /// `/`-separated, relative to the collection folder.
    pub path: String,
    pub key: String,
    pub request: HttpRequest,
}

/// The operation a request was generated from: its `operationId`, or
/// `METHOD /path` when it had none.
pub fn operation_key(request: &HttpRequest) -> Option<String> {
    let tags = request.tags.as_deref().unwrap_or_default();
    let tagged = |prefix: &str| tags.iter().find_map(|t| t.strip_prefix(prefix));
    if let Some(id) = tagged(OPERATION_TAG) {
        return Some(id.to_owned());
    }
    let path = tagged(PATH_TAG)?;
    Some(format!("{} {path}", request.effective_method()))
}

/// Every request file under `dir` (except in `_removed/` and hidden
/// folders) that records an operation, in path order. Files that do not
/// load are reported in `warnings` and left alone.
pub fn scan(dir: &Path, warnings: &mut Vec<String>) -> Vec<LocalRequest> {
    let mut out = Vec::new();
    let mut pending = vec![String::new()];
    let registry = KindRegistry::default();
    while let Some(rel) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(join(dir, &rel)) else {
            continue;
        };
        let mut names: Vec<(String, bool)> = entries
            .filter_map(Result::ok)
            .map(|e| {
                let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                (e.file_name().to_string_lossy().into_owned(), is_dir)
            })
            .collect();
        names.sort();
        for (name, is_dir) in names {
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if is_dir && !name.starts_with('.') && !(rel.is_empty() && name == REMOVED_DIR) {
                pending.push(child);
            } else if !is_dir && name.ends_with(REQUEST_FILE_SUFFIX) {
                out.extend(load(dir, &child, &registry, warnings));
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn load(
    dir: &Path,
    rel: &str,
    registry: &KindRegistry,
    warnings: &mut Vec<String>,
) -> Option<LocalRequest> {
    let path = join(dir, rel);
    let text = std::fs::read_to_string(&path).ok()?;
    match registry.load(Path::new(rel), &text) {
        Ok(Request::Http(request)) => {
            let key = operation_key(&request)?;
            Some(LocalRequest {
                path: rel.to_owned(),
                key,
                request: *request,
            })
        }
        Ok(Request::Graphql(_) | Request::Opaque(_)) => None,
        Err(e) => {
            warnings.push(format!("{e}; the file is left as it is"));
            None
        }
    }
}

/// `dir` joined with the `/`-separated `rel`.
pub fn join(dir: &Path, rel: &str) -> std::path::PathBuf {
    rel.split('/')
        .filter(|s| !s.is_empty())
        .fold(dir.to_path_buf(), |p, s| p.join(s))
}
