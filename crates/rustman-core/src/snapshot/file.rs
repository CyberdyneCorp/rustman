//! Snapshot files (snapshot-testing "Snapshot location and naming" and
//! "Snapshot file format"): `__snapshots__/<stem>.<name>.snap.yaml` next to
//! the request file, written with the workspace's deterministic YAML emitter
//! (design D2), so equal snapshots are equal bytes.
//!
//! ```yaml
//! version: 1
//! request: users/get-user.rman.yaml
//! name: default
//! kind: http
//! compare:
//!   - status
//!   - body
//! status: 200
//! body:
//!   json:
//!     id: 7
//! ```
//!
//! The body is one of `json` (a YAML structure), `text` (a literal block),
//! `binary` (`{size, sha256}`) or `events` (a list).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::normalize::{Normalized, NormalizedBody};
use crate::error::LoadError;
use crate::format::write::{json_node, render};
use crate::model::{ComparedPart, REQUEST_FILE_SUFFIX};

/// The snapshot file format version.
pub const SNAPSHOT_VERSION: u64 = 1;

/// The folder next to a request file that holds its snapshots.
pub const SNAPSHOT_DIR: &str = "__snapshots__";

/// Snapshot files end with this suffix.
pub const SNAPSHOT_SUFFIX: &str = ".snap.yaml";

/// The request file name without `.rman.yaml` (or its last extension).
pub fn request_stem(request_file: &Path) -> String {
    let name = request_file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.strip_suffix(REQUEST_FILE_SUFFIX) {
        Some(stem) => stem.to_owned(),
        None => request_file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(name),
    }
}

/// The file name of snapshot `name` of `request_file`.
pub fn snapshot_file_name(request_file: &Path, name: &str) -> String {
    format!("{}.{name}{SNAPSHOT_SUFFIX}", request_stem(request_file))
}

/// Where snapshot `name` of `request_file` is stored.
pub fn snapshot_path(request_file: &Path, name: &str) -> PathBuf {
    let dir = request_file.parent().unwrap_or(Path::new(""));
    dir.join(SNAPSHOT_DIR)
        .join(snapshot_file_name(request_file, name))
}

/// The snapshot path for display and for the file's `request` key: relative
/// to the workspace, with `/` on every platform. `request` is the request
/// file's workspace-relative path.
pub fn display_path(request: &str, name: &str) -> String {
    let request = request.replace('\\', "/");
    let (dir, file) = match request.rsplit_once('/') {
        Some((dir, file)) => (format!("{dir}/"), file.to_owned()),
        None => (String::new(), request.clone()),
    };
    format!(
        "{dir}{SNAPSHOT_DIR}/{}",
        snapshot_file_name(Path::new(&file), name)
    )
}

/// What a snapshot file says about where it came from.
pub struct SnapshotMeta<'a> {
    /// The request file, workspace-relative.
    pub request: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub compare: &'a [ComparedPart],
}

/// The snapshot document, keys in file order.
pub fn document(meta: &SnapshotMeta<'_>, normalized: &Normalized) -> Value {
    let mut doc = Map::new();
    doc.insert("version".into(), json!(SNAPSHOT_VERSION));
    doc.insert("request".into(), json!(meta.request.replace('\\', "/")));
    doc.insert("name".into(), json!(meta.name));
    doc.insert("kind".into(), json!(meta.kind));
    let compare: Vec<&str> = meta.compare.iter().map(|p| p.as_str()).collect();
    doc.insert("compare".into(), json!(compare));
    if let Some(status) = normalized.status {
        doc.insert("status".into(), json!(status));
    }
    if let Some(headers) = &normalized.headers {
        doc.insert("headers".into(), json!(headers));
    }
    if let Some(body) = &normalized.body {
        doc.insert("body".into(), body_value(body));
    }
    Value::Object(doc)
}

fn body_value(body: &NormalizedBody) -> Value {
    match body {
        NormalizedBody::Json(value) => json!({ "json": value }),
        NormalizedBody::Text(text) => json!({ "text": text }),
        NormalizedBody::Binary { size, sha256 } => {
            json!({ "binary": { "sha256": sha256, "size": size } })
        }
        NormalizedBody::Events(events) => json!({ "events": events }),
    }
}

/// The bytes of a snapshot file: 2-space indentation, final newline.
pub fn to_yaml(document: &Value) -> String {
    render(&json_node(document))
}

/// Read a snapshot file's text back into its document.
pub fn parse(path: &Path, text: &str) -> Result<Value, LoadError> {
    crate::format::from_yaml(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn paths_follow_the_request_file() {
        let path = snapshot_path(Path::new("users/get-user.rman.yaml"), "default");
        assert_eq!(
            path,
            Path::new("users")
                .join("__snapshots__")
                .join("get-user.default.snap.yaml")
        );
        assert_eq!(
            display_path("users/get-user.rman.yaml", "default"),
            "users/__snapshots__/get-user.default.snap.yaml"
        );
        assert_eq!(
            display_path("users\\get-user.rman.yaml", "list"),
            "users/__snapshots__/get-user.list.snap.yaml"
        );
        assert_eq!(
            display_path("ping.rman.yaml", "default"),
            "__snapshots__/ping.default.snap.yaml"
        );
        assert_eq!(request_stem(Path::new("a/b.yaml")), "b");
    }

    fn normalized(body: NormalizedBody) -> Normalized {
        Normalized {
            status: Some(200),
            headers: Some(BTreeMap::from([(
                "content-type".to_owned(),
                "application/json".to_owned(),
            )])),
            body: Some(body),
            warnings: Vec::new(),
        }
    }

    fn meta() -> SnapshotMeta<'static> {
        SnapshotMeta {
            request: "users/get-user.rman.yaml",
            name: "default",
            kind: "http",
            compare: &[
                ComparedPart::Status,
                ComparedPart::Headers,
                ComparedPart::Body,
            ],
        }
    }

    #[test]
    fn text_bodies_are_literal_blocks() {
        let doc = document(&meta(), &normalized(NormalizedBody::Text("a\nb\n".into())));
        let text = to_yaml(&doc);
        assert!(text.ends_with("body:\n  text: |\n    a\n    b\n"), "{text}");
        assert_eq!(parse(Path::new("s.yaml"), &text).unwrap(), doc);
    }

    #[test]
    fn binary_and_events_round_trip() {
        let bin = NormalizedBody::Binary {
            size: 3,
            sha256: "ab".into(),
        };
        let doc = document(&meta(), &normalized(bin));
        let text = to_yaml(&doc);
        assert!(
            text.ends_with("body:\n  binary:\n    sha256: ab\n    size: 3\n"),
            "{text}"
        );
        let events = NormalizedBody::Events(vec![json!({"n": 1}), json!("x")]);
        let doc = document(&meta(), &normalized(events));
        assert_eq!(parse(Path::new("s.yaml"), &to_yaml(&doc)).unwrap(), doc);
    }
}
