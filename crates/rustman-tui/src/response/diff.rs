//! Diffing two exchanges (spec: Diff two exchanges): status, headers, and
//! the body, structurally by JSONPath for JSON (key order ignored) and line
//! by line otherwise (`similar`).

use std::collections::BTreeMap;

use serde_json::Value;
use similar::{ChangeTag, TextDiff};

use rustman_core::run::HeaderRecord;

/// One exchange as the diff compares it (already redacted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    pub status: Option<u16>,
    pub headers: Vec<HeaderRecord>,
    pub body: Option<String>,
}

/// One difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// `$.b: 2 → 3`, `status: 200 → 404`.
    Changed {
        what: String,
        from: String,
        to: String,
    },
    /// Only in the other exchange.
    Added(String),
    /// Only in this exchange.
    Removed(String),
}

impl Change {
    /// The line the diff view shows.
    pub fn text(&self) -> String {
        match self {
            Self::Changed { what, from, to } => format!("{what}: {from} → {to}"),
            Self::Added(what) => format!("+ {what}"),
            Self::Removed(what) => format!("- {what}"),
        }
    }
}

/// Every difference from `a` to `b`: status, then headers, then the body.
pub fn diff(a: &Side, b: &Side) -> Vec<Change> {
    let mut changes = Vec::new();
    if a.status != b.status {
        let show = |s: Option<u16>| s.map_or_else(|| "none".to_owned(), |s| s.to_string());
        changes.push(Change::Changed {
            what: "status".into(),
            from: show(a.status),
            to: show(b.status),
        });
    }
    changes.extend(header_diff(&a.headers, &b.headers));
    changes.extend(body_diff(
        a.body.as_deref().unwrap_or_default(),
        b.body.as_deref().unwrap_or_default(),
    ));
    changes
}

fn header_diff(a: &[HeaderRecord], b: &[HeaderRecord]) -> Vec<Change> {
    let text = |h: &HeaderRecord| format!("{}: {}", h.name.to_ascii_lowercase(), h.value);
    let a: Vec<String> = a.iter().map(text).collect();
    let mut b: Vec<String> = b.iter().map(text).collect();
    let mut changes = Vec::new();
    for header in a {
        match b.iter().position(|h| *h == header) {
            Some(i) => {
                b.remove(i);
            }
            None => changes.push(Change::Removed(format!("header {header}"))),
        }
    }
    changes.extend(b.into_iter().map(|h| Change::Added(format!("header {h}"))));
    changes
}

/// JSON bodies by path; anything else by line.
pub fn body_diff(a: &str, b: &str) -> Vec<Change> {
    match (
        serde_json::from_str::<Value>(a),
        serde_json::from_str::<Value>(b),
    ) {
        (Ok(a), Ok(b)) => json_diff(&a, &b),
        _ => line_diff(a, b),
    }
}

/// Leaves (and empty containers) by JSONPath.
fn flatten(value: &Value, path: String, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                flatten(child, format!("{path}{}", member(key)), out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (i, child) in items.iter().enumerate() {
                flatten(child, format!("{path}[{i}]"), out);
            }
        }
        leaf => {
            out.insert(path, leaf.to_string());
        }
    }
}

fn member(key: &str) -> String {
    let plain = !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key.chars().all(|c| c.is_alphanumeric() || c == '_');
    if plain {
        format!(".{key}")
    } else {
        format!("[{}]", Value::String(key.to_owned()))
    }
}

/// The structural diff: changed, removed and added paths, by path.
pub fn json_diff(a: &Value, b: &Value) -> Vec<Change> {
    let (mut left, mut right) = (BTreeMap::new(), BTreeMap::new());
    flatten(a, "$".into(), &mut left);
    flatten(b, "$".into(), &mut right);
    let mut changes = Vec::new();
    for (path, from) in &left {
        match right.remove(path) {
            Some(to) if to == *from => {}
            Some(to) => changes.push(Change::Changed {
                what: path.clone(),
                from: from.clone(),
                to,
            }),
            None => changes.push(Change::Removed(format!("{path}: {from}"))),
        }
    }
    changes.extend(
        right
            .into_iter()
            .map(|(path, value)| Change::Added(format!("{path}: {value}"))),
    );
    changes
}

fn line_diff(a: &str, b: &str) -> Vec<Change> {
    TextDiff::from_lines(a, b)
        .iter_all_changes()
        .filter_map(|change| {
            let line = change.value().trim_end_matches(['\n', '\r']).to_owned();
            match change.tag() {
                ChangeTag::Equal => None,
                ChangeTag::Delete => Some(Change::Removed(line)),
                ChangeTag::Insert => Some(Change::Added(line)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(status: u16, body: &str) -> Side {
        Side {
            status: Some(status),
            headers: vec![HeaderRecord {
                name: "Content-Type".into(),
                value: "application/json".into(),
            }],
            body: Some(body.into()),
        }
    }

    #[test]
    fn json_is_diffed_by_path_ignoring_key_order() {
        // Scenario "Structural JSON diff".
        let changes = diff(
            &side(200, r#"{"a":1,"b":2}"#),
            &side(200, r#"{"b":3,"a":1}"#),
        );
        let lines: Vec<String> = changes.iter().map(Change::text).collect();
        assert_eq!(lines, ["$.b: 2 → 3"]);
    }

    #[test]
    fn status_headers_added_and_removed_paths() {
        let mut b = side(404, r#"{"a":1,"c":{"d":[true]},"odd key":null}"#);
        b.headers.push(HeaderRecord {
            name: "Set-Cookie".into(),
            value: "x=1".into(),
        });
        let lines: Vec<String> = diff(&side(200, r#"{"a":1,"b":[]}"#), &b)
            .iter()
            .map(Change::text)
            .collect();
        assert_eq!(
            lines,
            [
                "status: 200 → 404",
                "+ header set-cookie: x=1",
                "- $.b: []",
                "+ $.c.d[0]: true",
                "+ $[\"odd key\"]: null",
            ]
        );
    }

    #[test]
    fn other_bodies_are_diffed_by_line() {
        let lines: Vec<String> = body_diff("a\nb\nc\n", "a\nB\nc\n")
            .iter()
            .map(Change::text)
            .collect();
        assert_eq!(lines, ["- b", "+ B"]);
        assert!(body_diff("same", "same").is_empty());
    }
}
