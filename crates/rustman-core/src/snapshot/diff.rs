//! Comparing a stored snapshot with a new one (snapshot-testing "Structural
//! diff on failure", "Binary bodies", "Snapshot of subscription events";
//! design D4).
//!
//! JSON documents are walked side by side into `(path, kind, old, new)`
//! changes; arrays compare by index. Text bodies get a unified line diff with
//! 3 lines of context from `similar`. At most [`MAX_CHANGES`] changes are
//! kept, with a count of the rest.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::TextDiff;

/// How many changes a diff keeps and shows.
pub const MAX_CHANGES: usize = 50;

/// What happened at one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

/// One difference: a JSONPath into the body (`$.user.role`), or a snapshot
/// key such as `status` or `headers.content-type`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<Value>,
}

/// Old and new size and hash of a binary body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryChange {
    pub old_size: u64,
    pub old_sha256: String,
    pub new_size: u64,
    pub new_sha256: String,
}

/// Old and new value of a count or a label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair<T> {
    pub old: T,
    pub new: T,
}

/// Every difference between two snapshot documents.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDiff {
    /// The first [`MAX_CHANGES`] structural changes.
    pub changes: Vec<Change>,
    /// How many more changes there were.
    pub more: usize,
    /// The unified diff of text bodies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<BinaryChange>,
    /// Set when the number of collected events changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_count: Option<Pair<usize>>,
    /// Set when the body changed form (`json`, `text`, `binary`, `events`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_type: Option<Pair<String>>,
}

impl SnapshotDiff {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self.text.is_none()
            && self.binary.is_none()
            && self.event_count.is_none()
            && self.body_type.is_none()
    }
}

/// The differences between two snapshot documents.
pub fn diff_documents(old: &Value, new: &Value) -> SnapshotDiff {
    let mut diff = SnapshotDiff::default();
    let mut changes = Vec::new();
    let without_body = |v: &Value| match v {
        Value::Object(map) => {
            let mut map = map.clone();
            map.remove("body");
            Value::Object(map)
        }
        other => other.clone(),
    };
    walk(
        &without_body(old),
        &without_body(new),
        &mut Path::meta(),
        &mut changes,
    );
    diff_body(old.get("body"), new.get("body"), &mut diff, &mut changes);
    diff.more = changes.len().saturating_sub(MAX_CHANGES);
    changes.truncate(MAX_CHANGES);
    diff.changes = changes;
    diff
}

fn diff_body(
    old: Option<&Value>,
    new: Option<&Value>,
    diff: &mut SnapshotDiff,
    out: &mut Vec<Change>,
) {
    let (Some(old), Some(new)) = (old, new) else {
        if old != new {
            out.push(change("body", old.cloned(), new.cloned()));
        }
        return;
    };
    let (old_type, old_value) = body_form(old);
    let (new_type, new_value) = body_form(new);
    if old_type != new_type {
        diff.body_type = Some(Pair {
            old: old_type,
            new: new_type,
        });
        return;
    }
    match old_type.as_str() {
        "text" => diff.text = text_diff(old_value, new_value),
        "binary" => diff.binary = binary_diff(old_value, new_value),
        "events" => {
            diff.event_count = event_count(old_value, new_value);
            walk(old_value, new_value, &mut Path::body(), out);
        }
        _ => walk(old_value, new_value, &mut Path::body(), out),
    }
}

/// The body's form (its single key) and content.
fn body_form(body: &Value) -> (String, &Value) {
    match body.as_object().and_then(|m| m.iter().next()) {
        Some((form, value)) => (form.clone(), value),
        None => ("unknown".to_owned(), body),
    }
}

fn change(path: &str, old: Option<Value>, new: Option<Value>) -> Change {
    let kind = match (&old, &new) {
        (None, _) => ChangeKind::Added,
        (_, None) => ChangeKind::Removed,
        _ => ChangeKind::Changed,
    };
    Change {
        path: path.to_owned(),
        kind,
        old,
        new,
    }
}

fn text_diff(old: &Value, new: &Value) -> Option<String> {
    let (old, new) = (
        old.as_str().unwrap_or_default(),
        new.as_str().unwrap_or_default(),
    );
    if old == new {
        return None;
    }
    let diff = TextDiff::from_lines(old, new);
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header("snapshot", "received")
        .to_string();
    Some(unified)
}

fn binary_diff(old: &Value, new: &Value) -> Option<BinaryChange> {
    if old == new {
        return None;
    }
    let size = |v: &Value| v.get("size").and_then(Value::as_u64).unwrap_or(0);
    let hash = |v: &Value| {
        v.get("sha256")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Some(BinaryChange {
        old_size: size(old),
        old_sha256: hash(old),
        new_size: size(new),
        new_sha256: hash(new),
    })
}

fn event_count(old: &Value, new: &Value) -> Option<Pair<usize>> {
    let count = |v: &Value| v.as_array().map_or(0, Vec::len);
    let (old, new) = (count(old), count(new));
    (old != new).then_some(Pair { old, new })
}

/// A path being walked: JSONPath segments under `$` for bodies, or dotted
/// keys for the rest of the document.
struct Path {
    root: &'static str,
    segments: Vec<Segment>,
}

enum Segment {
    Key(String),
    Index(usize),
}

impl Path {
    fn meta() -> Self {
        Self {
            root: "",
            segments: Vec::new(),
        }
    }

    fn body() -> Self {
        Self {
            root: "$",
            segments: Vec::new(),
        }
    }

    fn render(&self) -> String {
        let mut out = self.root.to_owned();
        for segment in &self.segments {
            match segment {
                Segment::Index(i) => out.push_str(&format!("[{i}]")),
                Segment::Key(k) if out.is_empty() => out.push_str(k),
                Segment::Key(k) if self.root.is_empty() || is_identifier(k) => {
                    out.push('.');
                    out.push_str(k);
                }
                Segment::Key(k) => out.push_str(&format!("['{}']", k.replace('\'', "\\'"))),
            }
        }
        out
    }
}

fn is_identifier(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Record every difference between `old` and `new` under `path`.
fn walk(old: &Value, new: &Value, path: &mut Path, out: &mut Vec<Change>) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for key in keys {
                path.segments.push(Segment::Key(key.clone()));
                child(a.get(key.as_str()), b.get(key.as_str()), path, out);
                path.segments.pop();
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for i in 0..a.len().max(b.len()) {
                path.segments.push(Segment::Index(i));
                child(a.get(i), b.get(i), path, out);
                path.segments.pop();
            }
        }
        _ if old != new => out.push(change(&path.render(), Some(old.clone()), Some(new.clone()))),
        _ => {}
    }
}

fn child(old: Option<&Value>, new: Option<&Value>, path: &mut Path, out: &mut Vec<Change>) {
    match (old, new) {
        (Some(old), Some(new)) => walk(old, new, path, out),
        (old, new) => out.push(change(&path.render(), old.cloned(), new.cloned())),
    }
}

/// Values longer than this are cut in messages.
const SHOWN_VALUE: usize = 120;

fn shown(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() <= SHOWN_VALUE {
        return text;
    }
    let cut: String = text.chars().take(SHOWN_VALUE).collect();
    format!("{cut}…")
}

/// The first 4 hex digits of a hash, as failures show it.
fn short_hash(hash: &str) -> String {
    let start: String = hash.chars().take(4).collect();
    format!("sha256:{start}…")
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = self.kind.as_str();
        match (&self.old, &self.new) {
            (Some(old), Some(new)) => {
                write!(f, "{kind} {}: {} → {}", self.path, shown(old), shown(new))
            }
            (Some(value), None) | (None, Some(value)) => {
                write!(f, "{kind} {}: {}", self.path, shown(value))
            }
            (None, None) => write!(f, "{kind} {}", self.path),
        }
    }
}

impl fmt::Display for SnapshotDiff {
    /// One line per change, then the text diff.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut lines: Vec<String> = Vec::new();
        if let Some(count) = &self.event_count {
            lines.push(format!(
                "event count changed: {} → {}",
                count.old, count.new
            ));
        }
        if let Some(form) = &self.body_type {
            lines.push(format!("body type changed: {} → {}", form.old, form.new));
        }
        if let Some(b) = &self.binary {
            lines.push(format!(
                "binary body changed: {} B {} → {} B {}",
                b.old_size,
                short_hash(&b.old_sha256),
                b.new_size,
                short_hash(&b.new_sha256)
            ));
        }
        lines.extend(self.changes.iter().map(Change::to_string));
        if self.more > 0 {
            lines.push(format!("… and {} more changes", self.more));
        }
        f.write_str(&lines.join("\n"))?;
        if let Some(text) = &self.text {
            if !lines.is_empty() {
                f.write_str("\n")?;
            }
            f.write_str(text.trim_end_matches('\n'))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(body: Value) -> Value {
        json!({"version": 1, "name": "default", "status": 200, "body": body})
    }

    #[test]
    fn changed_field_has_its_path_and_values() {
        let old = doc(json!({"json": {"user": {"role": "admin"}}}));
        let new = doc(json!({"json": {"user": {"role": "viewer"}}}));
        let diff = diff_documents(&old, &new);
        assert_eq!(
            diff.to_string(),
            r#"changed $.user.role: "admin" → "viewer""#
        );
        assert_eq!(diff.changes[0].kind, ChangeKind::Changed);
        assert!(diff_documents(&old, &old).is_empty());
    }

    #[test]
    fn added_removed_and_array_items() {
        let old = doc(json!({"json": {"a": 1, "list": [1, 2, 3], "odd key": true}}));
        let new = doc(json!({"json": {"b": 2, "list": [1, 5], "odd key": false}}));
        let diff = diff_documents(&old, &new);
        let lines: Vec<String> = diff.changes.iter().map(Change::to_string).collect();
        assert_eq!(
            lines,
            [
                "removed $.a: 1",
                "added $.b: 2",
                "changed $.list[1]: 2 → 5",
                "removed $.list[2]: 3",
                "changed $['odd key']: true → false",
            ]
        );
    }

    #[test]
    fn status_and_headers_use_document_keys() {
        let mut old = doc(json!({"json": {}}));
        let mut new = old.clone();
        old["headers"] = json!({"content-type": "application/json"});
        new["headers"] = json!({"content-type": "text/html"});
        new["status"] = json!(404);
        let diff = diff_documents(&old, &new);
        assert_eq!(
            diff.to_string(),
            "changed headers.content-type: \"application/json\" → \"text/html\"\nchanged status: 200 → 404"
        );
    }

    #[test]
    fn large_diffs_are_truncated() {
        let old: serde_json::Map<String, Value> =
            (0..80).map(|i| (format!("k{i:02}"), json!(i))).collect();
        let new: serde_json::Map<String, Value> = (0..80)
            .map(|i| (format!("k{i:02}"), json!(-i - 1)))
            .collect();
        let diff = diff_documents(&doc(json!({"json": old})), &doc(json!({"json": new})));
        assert_eq!(diff.changes.len(), 50);
        assert_eq!(diff.more, 30);
        let text = diff.to_string();
        assert_eq!(text.lines().count(), 51);
        assert!(text.ends_with("… and 30 more changes"), "{text}");
    }

    #[test]
    fn text_bodies_get_a_unified_diff() {
        let old = doc(json!({"text": "a\nb\nc\nd\ne\nf\ng\nh\n"}));
        let new = doc(json!({"text": "a\nb\nc\nd\nE\nf\ng\nh\n"}));
        let text = diff_documents(&old, &new).to_string();
        assert!(
            text.starts_with("--- snapshot\n+++ received\n@@ -2,7 +2,7 @@\n"),
            "{text}"
        );
        assert!(text.contains("\n-e\n+E\n"), "{text}");
    }

    #[test]
    fn binary_bodies_show_size_and_hash() {
        let old = doc(json!({"binary": {"size": 10240, "sha256": "ab12cd"}}));
        let new = doc(json!({"binary": {"size": 10301, "sha256": "9f0c11"}}));
        assert_eq!(
            diff_documents(&old, &new).to_string(),
            "binary body changed: 10240 B sha256:ab12… → 10301 B sha256:9f0c…"
        );
    }

    #[test]
    fn event_count_is_reported_with_the_diff() {
        let old = doc(json!({"events": [{"n": 1}, {"n": 2}, {"n": 3}]}));
        let new = doc(json!({"events": [{"n": 1}, {"n": 2}]}));
        let text = diff_documents(&old, &new).to_string();
        assert_eq!(text, "event count changed: 3 → 2\nremoved $[2]: {\"n\":3}");
    }

    #[test]
    fn body_form_changes_are_named() {
        let old = doc(json!({"json": {}}));
        let new = doc(json!({"text": "x"}));
        assert_eq!(
            diff_documents(&old, &new).to_string(),
            "body type changed: json → text"
        );
        let mut no_body = doc(json!(null));
        no_body.as_object_mut().unwrap().remove("body");
        assert_eq!(
            diff_documents(&no_body, &old).to_string(),
            "added body: {\"json\":{}}"
        );
    }

    #[test]
    fn long_values_are_cut() {
        let long = "x".repeat(500);
        let diff = diff_documents(
            &doc(json!({"json": {"a": long}})),
            &doc(json!({"json": {"a": 1}})),
        );
        assert!(diff.to_string().len() < 200);
        assert!(diff.to_string().contains("…"));
    }
}
