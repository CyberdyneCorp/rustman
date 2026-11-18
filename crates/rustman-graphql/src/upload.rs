//! File uploads per the GraphQL multipart request specification: every
//! `{ "$file": "path" }` marker in the variables becomes `null`, a `map`
//! entry and a file part.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::GraphqlError;

/// The key of a file marker in the variables.
pub const FILE_MARKER: &str = "$file";

/// One file to upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// The part name and `map` key: `"0"`, `"1"`, ...
    pub key: String,
    /// The object path the file replaces, such as `variables.file`.
    pub object_path: String,
    /// The path as written in the request.
    pub raw: String,
    /// The contained path to read.
    pub file: PathBuf,
    /// Guessed from the file extension.
    pub content_type: &'static str,
}

/// Replace every file marker in `variables` with `null` and return the
/// uploads in document order. `resolve` turns a written path into the file
/// to read (relative to the request file, inside the workspace). A file
/// that does not exist fails with `graphql: upload file not found`.
pub fn extract_uploads(
    variables: &mut Map<String, Value>,
    resolve: impl Fn(&str) -> Result<PathBuf, GraphqlError>,
) -> Result<Vec<Upload>, GraphqlError> {
    let mut found = Vec::new();
    for (name, value) in variables.iter_mut() {
        collect(value, format!("variables.{name}"), &mut found);
    }
    found
        .into_iter()
        .enumerate()
        .map(|(index, (object_path, raw))| {
            let file = resolve(&raw)?;
            if !file.is_file() {
                return Err(GraphqlError::UploadNotFound(raw));
            }
            Ok(Upload {
                key: index.to_string(),
                object_path,
                content_type: mime_type(&file),
                raw,
                file,
            })
        })
        .collect()
}

/// The `map` part: each key to the one object path it fills.
pub fn upload_map(uploads: &[Upload]) -> Value {
    let map: Map<String, Value> = uploads
        .iter()
        .map(|u| (u.key.clone(), Value::from(vec![u.object_path.clone()])))
        .collect();
    Value::Object(map)
}

fn collect(value: &mut Value, path: String, found: &mut Vec<(String, String)>) {
    if let Some(raw) = file_marker(value) {
        found.push((path, raw));
        *value = Value::Null;
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                collect(item, format!("{path}.{key}"), found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                collect(item, format!("{path}.{index}"), found);
            }
        }
        _ => {}
    }
}

/// The path of `{ "$file": "path" }`, the marker's only shape.
fn file_marker(value: &Value) -> Option<String> {
    let Value::Object(map) = value else {
        return None;
    };
    if map.len() != 1 {
        return None;
    }
    map.get(FILE_MARKER)?.as_str().map(str::to_owned)
}

/// The MIME type of `path` by extension; `application/octet-stream` when
/// unknown.
pub fn mime_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("an object")
        };
        map
    }

    #[test]
    fn markers_become_null_and_map_entries() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.png", "b.PDF", "c.bin"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let mut variables = object(json!({
            "file": {"$file": "a.png"},
            "input": {"docs": [{"$file": "b.PDF"}, {"$file": "c.bin"}], "title": "t"},
            "other": {"$file": "a.png", "extra": 1},
        }));
        let resolve = |raw: &str| Ok(dir.path().join(raw));
        let uploads = extract_uploads(&mut variables, resolve).unwrap();
        let summary: Vec<(&str, &str, &str)> = uploads
            .iter()
            .map(|u| (u.key.as_str(), u.object_path.as_str(), u.content_type))
            .collect();
        assert_eq!(
            summary,
            [
                ("0", "variables.file", "image/png"),
                ("1", "variables.input.docs.0", "application/pdf"),
                ("2", "variables.input.docs.1", "application/octet-stream"),
            ]
        );
        assert_eq!(
            Value::Object(variables),
            json!({
                "file": null,
                "input": {"docs": [null, null], "title": "t"},
                "other": {"$file": "a.png", "extra": 1},
            })
        );
        assert_eq!(
            upload_map(&uploads),
            json!({"0": ["variables.file"], "1": ["variables.input.docs.0"], "2": ["variables.input.docs.1"]})
        );
    }

    #[test]
    fn a_missing_file_fails_before_send() {
        let dir = tempfile::tempdir().unwrap();
        let mut variables = object(json!({"file": {"$file": "./avatar.png"}}));
        let err = extract_uploads(&mut variables, |raw| Ok(dir.path().join(raw))).unwrap_err();
        assert_eq!(
            err.to_string(),
            "graphql: upload file not found: ./avatar.png"
        );
    }

    #[test]
    fn resolve_errors_are_returned() {
        let mut variables = object(json!({"file": {"$file": "../x"}}));
        let err = extract_uploads(&mut variables, |_| {
            Err(GraphqlError::PathEscape("escapes".into()))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "graphql: escapes");
    }

    #[test]
    fn no_markers_means_no_uploads() {
        let mut variables = object(json!({"id": "1", "list": [1, {"a": "$file"}]}));
        let before = variables.clone();
        assert!(
            extract_uploads(&mut variables, |_| unreachable!())
                .unwrap()
                .is_empty()
        );
        assert_eq!(variables, before);
    }
}
