//! What several renderers derive from an exploration report: its operations
//! grouped by tag, and each operation's JSON pointer in the document.

use std::collections::BTreeSet;

use crate::exploration::ExplorationReport;

/// The group of operations that have no tag.
pub const UNTAGGED: &str = "untagged";
/// The group of methods the document does not list (found by probes).
pub const UNDOCUMENTED: &str = "undocumented";

/// One operation as reports group and locate it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationView {
    /// `METHOD /path`.
    pub operation: String,
    /// Its first tag, [`UNTAGGED`] or [`UNDOCUMENTED`].
    pub group: String,
    /// Its JSON pointer: the operation, or the path item of an
    /// undocumented method.
    pub pointer: String,
}

/// Every operation of `report`: the documented ones in document order (the
/// coverage matrix for reports without them), then the undocumented ones
/// findings name.
pub fn operations(report: &ExplorationReport) -> Vec<OperationView> {
    let mut views: Vec<OperationView> = if report.operations.is_empty() {
        report
            .coverage
            .matrix
            .iter()
            .map(|row| view(&row.operation, UNTAGGED, true))
            .collect()
    } else {
        report
            .operations
            .iter()
            .map(|op| {
                let group = op.tags.first().map_or(UNTAGGED, String::as_str);
                view(&op.operation, group, true)
            })
            .collect()
    };
    let known: BTreeSet<String> = views.iter().map(|v| v.operation.clone()).collect();
    let extra: BTreeSet<&str> = report
        .findings
        .iter()
        .map(|f| f.operation.as_str())
        .filter(|op| !known.contains(*op))
        .collect();
    views.extend(extra.into_iter().map(|op| view(op, UNDOCUMENTED, false)));
    views
}

fn view(operation: &str, group: &str, documented: bool) -> OperationView {
    OperationView {
        operation: operation.to_owned(),
        group: group.to_owned(),
        pointer: pointer(operation, documented),
    }
}

/// `GET /users/{id}` as `/paths/~1users~1{id}/get`; without `documented`,
/// the path item only.
pub fn pointer(operation: &str, documented: bool) -> String {
    let (method, path) = operation.split_once(' ').unwrap_or(("", operation));
    let escaped = path.replace('~', "~0").replace('/', "~1");
    if documented {
        format!("/paths/{escaped}/{}", method.to_ascii_lowercase())
    } else {
        format!("/paths/{escaped}")
    }
}

/// The 1-based line of `operation` in the OpenAPI `document` (YAML or
/// pretty JSON): the method key under the path key, else the path key.
pub fn line_of(document: &str, operation: &str) -> Option<usize> {
    let (method, path) = operation.split_once(' ')?;
    let method = method.to_ascii_lowercase();
    let lines: Vec<&str> = document.lines().collect();
    let start = lines.iter().position(|l| is_key(l, path))?;
    let indent = indent_of(lines[start]);
    let found = lines[start + 1..]
        .iter()
        .take_while(|l| l.trim().is_empty() || indent_of(l) > indent)
        .position(|l| is_key(l, &method));
    Some(found.map_or(start + 1, |i| start + i + 2))
}

/// `line` is the key `key`, bare or quoted.
fn is_key(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    [
        format!("{key}:"),
        format!("\"{key}\":"),
        format!("'{key}':"),
    ]
    .iter()
    .any(|k| trimmed.starts_with(k.as_str()))
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointers_escape_the_path() {
        assert_eq!(pointer("GET /users/{id}", true), "/paths/~1users~1{id}/get");
        assert_eq!(pointer("PUT /users", false), "/paths/~1users");
        assert_eq!(pointer("GET /a~b", true), "/paths/~1a~0b/get");
    }

    #[test]
    fn lines_are_found_in_yaml_and_json() {
        let yaml = "paths:\n  /users:\n    post:\n      x: 1\n  /users/{id}:\n    get:\n      x: 1\n    delete:\n      x: 1\n";
        assert_eq!(line_of(yaml, "GET /users/{id}"), Some(6));
        assert_eq!(line_of(yaml, "DELETE /users/{id}"), Some(8));
        assert_eq!(line_of(yaml, "PUT /users"), Some(2));
        assert_eq!(line_of(yaml, "GET /nope"), None);
        let json = "{\n  \"paths\": {\n    \"/users\": {\n      \"get\": {}\n    }\n  }\n}\n";
        assert_eq!(line_of(json, "GET /users"), Some(4));
    }
}
