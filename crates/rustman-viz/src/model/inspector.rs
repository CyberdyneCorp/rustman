//! The response inspector's data (spec "Large-response inspector"): the
//! top-level JSON array, or the array a JSONPath selects, as a sortable
//! table with virtual rows whose columns are the union of the keys of the
//! first 1,000 elements; and a read-only JSON text view. A body that holds
//! no array shows the text view only. Bodies may arrive inline or by
//! `blob_ref` (a temp file the producer owns and removes).

use std::cmp::Ordering;
use std::sync::Arc;

use rustman_viz_protocol::{Body, InspectResponse};
use serde_json::Value;
use serde_json_path::JsonPath;

/// Elements whose keys make the columns.
const KEY_SAMPLE: usize = 1000;

/// How much of the body the text view shows.
pub const TEXT_LIMIT: usize = 1 << 20;

/// A cell longer than this is cut.
const CELL_LIMIT: usize = 200;

/// The column of an element that is not an object.
const VALUE_COLUMN: &str = "value";

#[derive(Debug, Clone)]
pub struct JsonTable {
    items: Arc<Vec<Value>>,
    pub columns: Vec<String>,
    /// Display order: indexes into `items`.
    order: Vec<usize>,
}

impl JsonTable {
    fn new(items: Vec<Value>) -> Self {
        let mut columns: Vec<String> = Vec::new();
        for item in items.iter().take(KEY_SAMPLE) {
            match item {
                Value::Object(map) => {
                    for key in map.keys() {
                        if !columns.iter().any(|c| c == key) {
                            columns.push(key.clone());
                        }
                    }
                }
                _ => {
                    if !columns.iter().any(|c| c == VALUE_COLUMN) {
                        columns.push(VALUE_COLUMN.to_owned());
                    }
                }
            }
        }
        let order = (0..items.len()).collect();
        Self {
            items: Arc::new(items),
            columns,
            order,
        }
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    fn value(&self, row: usize, column: usize) -> Option<&Value> {
        let item = self.items.get(*self.order.get(row)?)?;
        let name = self.columns.get(column)?;
        match item {
            Value::Object(map) => map.get(name),
            other if name == VALUE_COLUMN => Some(other),
            _ => None,
        }
    }

    /// The text of a cell: strings as they are, other values as compact
    /// JSON, a missing key as empty.
    pub fn cell(&self, row: usize, column: usize) -> String {
        let text = match self.value(row, column) {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        };
        if text.len() > CELL_LIMIT {
            let mut end = CELL_LIMIT;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &text[..end])
        } else {
            text
        }
    }

    /// Sort the rows by `column`: numbers by value, then text, missing last.
    pub fn sort(&mut self, column: usize, descending: bool) {
        let Some(name) = self.columns.get(column).cloned() else {
            return;
        };
        let items = Arc::clone(&self.items);
        let key = |i: &usize| -> Option<&Value> {
            match &items[*i] {
                Value::Object(map) => map.get(&name),
                other if name == VALUE_COLUMN => Some(other),
                _ => None,
            }
        };
        self.order.sort_by(|a, b| {
            let ordering = compare(key(a), key(b));
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
    }
}

fn compare(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Greater,
        (_, None) => Ordering::Less,
        (Some(Value::Number(x)), Some(Value::Number(y))) => {
            let (x, y) = (x.as_f64().unwrap_or(0.0), y.as_f64().unwrap_or(0.0));
            x.total_cmp(&y)
        }
        (Some(Value::String(x)), Some(Value::String(y))) => x.cmp(y),
        (Some(x), Some(y)) => x.to_string().cmp(&y.to_string()),
    }
}

#[derive(Debug, Clone, Default)]
pub struct InspectorModel {
    pub title: String,
    /// Status, content type and headers, one per line.
    pub summary: String,
    /// The text view (at most [`TEXT_LIMIT`] bytes of the body).
    pub text: String,
    /// The tabulated array, when the body holds one.
    pub table: Option<JsonTable>,
    /// Why the body could not be read or tabulated.
    pub note: Option<String>,
}

impl InspectorModel {
    pub fn new(title: impl Into<String>, response: &InspectResponse) -> Self {
        let mut model = Self {
            title: title.into(),
            summary: summary(response),
            ..Self::default()
        };
        let body = match &response.body {
            Body::Inline { text } => Ok(text.clone()),
            Body::BlobRef(blob) => std::fs::read(&blob.path)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|e| format!("cannot read {}: {e}", blob.path)),
        };
        let body = match body {
            Ok(body) => body,
            Err(e) => {
                model.note = Some(e);
                return model;
            }
        };
        model.text = text_view(&body);
        match tabulate(&body, response.selector.as_deref()) {
            Ok(table) => model.table = table,
            Err(e) => model.note = Some(e),
        }
        model
    }

    /// The line above the views.
    pub fn heading(&self) -> String {
        let mut heading = match &self.table {
            Some(table) => format!("{} rows × {} columns", table.len(), table.columns.len()),
            None => "No array to tabulate: JSON view only".to_owned(),
        };
        if let Some(note) = &self.note {
            heading.push_str(" — ");
            heading.push_str(note);
        }
        heading
    }
}

fn summary(response: &InspectResponse) -> String {
    let mut lines = Vec::new();
    if let Some(status) = response.status {
        lines.push(format!("Status {status}"));
    }
    if let Some(content_type) = &response.content_type {
        lines.push(format!("Content-Type: {content_type}"));
    }
    for header in &response.headers {
        lines.push(format!("{}: {}", header.name, header.value));
    }
    lines.join("\n")
}

/// The body pretty-printed when it is JSON small enough, else as received;
/// cut at [`TEXT_LIMIT`].
fn text_view(body: &str) -> String {
    let pretty = (body.len() <= TEXT_LIMIT)
        .then(|| serde_json::from_str::<Value>(body).ok())
        .flatten()
        .and_then(|v| serde_json::to_string_pretty(&v).ok());
    let text = pretty.unwrap_or_else(|| body.to_owned());
    if text.len() <= TEXT_LIMIT {
        return text;
    }
    let mut end = TEXT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… (showing the first {} KiB of {} KiB)",
        &text[..end],
        TEXT_LIMIT / 1024,
        text.len() / 1024
    )
}

/// The array to tabulate: the one `selector` selects, else the top level.
fn tabulate(body: &str, selector: Option<&str>) -> Result<Option<JsonTable>, String> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Ok(None);
    };
    let array = match selector {
        Some(selector) => {
            let path = JsonPath::parse(selector)
                .map_err(|e| format!("invalid selector {selector}: {e}"))?;
            let found = path.query(&value).all().into_iter().find_map(|v| match v {
                Value::Array(items) => Some(items.clone()),
                _ => None,
            });
            match found {
                Some(items) => items,
                None => return Err(format!("{selector} selects no array")),
            }
        }
        None => match value {
            Value::Array(items) => items,
            _ => return Ok(None),
        },
    };
    Ok(Some(JsonTable::new(array)))
}

#[cfg(test)]
mod tests {
    use rustman_viz_protocol::{BlobRef, Header};

    use super::*;

    fn response(body: Body, selector: Option<&str>) -> InspectResponse {
        InspectResponse {
            request: Some("users/list".into()),
            status: Some(200),
            content_type: Some("application/json".into()),
            headers: vec![Header {
                name: "Authorization".into(),
                value: "Bearer «redacted:token»".into(),
            }],
            selector: selector.map(str::to_owned),
            body,
        }
    }

    fn inline(text: &str) -> Body {
        Body::Inline { text: text.into() }
    }

    #[test]
    fn an_array_of_objects_has_the_union_of_keys() {
        let model = InspectorModel::new(
            "users",
            &response(
                inline(r#"[{"id":1,"name":"a"},{"id":2,"email":"b@x"}]"#),
                None,
            ),
        );
        let table = model.table.as_ref().unwrap();
        assert_eq!(table.columns, ["id", "name", "email"]);
        assert_eq!(table.cell(0, 1), "a");
        assert_eq!(table.cell(0, 2), "", "missing key: empty cell");
        assert_eq!(table.cell(1, 2), "b@x");
        assert!(model.summary.contains("Bearer «redacted:token»"));
        assert_eq!(model.heading(), "2 rows × 3 columns");
    }

    #[test]
    fn an_object_body_shows_the_text_view_only() {
        let model = InspectorModel::new("ok", &response(inline(r#"{"ok": true}"#), None));
        assert!(model.table.is_none());
        assert_eq!(model.text, "{\n  \"ok\": true\n}");
        assert!(model.heading().starts_with("No array"));
    }

    #[test]
    fn a_selector_picks_the_array() {
        let body = r#"{"items":[{"id":3},{"id":1},{"id":2},{"other":1}],"next":null}"#;
        let mut model = InspectorModel::new("p", &response(inline(body), Some("$.items")));
        let table = model.table.as_mut().unwrap();
        assert_eq!(table.columns, ["id", "other"]);
        table.sort(0, false);
        let ids: Vec<_> = (0..4).map(|r| table.cell(r, 0)).collect();
        assert_eq!(ids, ["1", "2", "3", ""], "missing values sort last");
        table.sort(0, true);
        assert_eq!(table.cell(0, 0), "", "descending reverses");
        let bad = InspectorModel::new("p", &response(inline(body), Some("$.next")));
        assert!(bad.note.unwrap().contains("selects no array"));
    }

    #[test]
    fn scalars_and_long_cells() {
        let long = "x".repeat(500);
        let body = format!(r#"[1,"two",{{"k":"{long}"}}]"#);
        let model = InspectorModel::new("s", &response(inline(&body), None));
        let table = model.table.unwrap();
        assert_eq!(table.columns, ["value", "k"]);
        assert_eq!(table.cell(1, 0), "two");
        assert_eq!(table.cell(2, 1).chars().count(), 201);
    }

    #[test]
    fn a_blob_ref_body_is_read_from_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body.json");
        std::fs::write(&path, r#"[{"a":1}]"#).unwrap();
        let blob = Body::BlobRef(BlobRef {
            path: path.display().to_string(),
            bytes: 9,
            media_type: None,
        });
        let model = InspectorModel::new("b", &response(blob, None));
        assert_eq!(model.table.unwrap().columns, ["a"]);
        let missing = Body::BlobRef(BlobRef {
            path: dir.path().join("gone").display().to_string(),
            bytes: 1,
            media_type: None,
        });
        let model = InspectorModel::new("b", &response(missing, None));
        assert!(model.note.unwrap().starts_with("cannot read"));
    }

    /// Task 4.4: a 200 MiB body by blob reference (slow in debug builds;
    /// run with `cargo test --release -- --ignored`).
    #[test]
    #[ignore = "200 MiB: run in release with --ignored"]
    fn a_200_mib_blob() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.json");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        file.write_all(b"[").unwrap();
        let mut written = 1u64;
        let mut i = 0u64;
        while written < 200 << 20 {
            let row = format!(
                r#"{}{{"id":{i},"name":"user {i}","email":"u{i}@example.com","tags":["a","b"]}}"#,
                if i == 0 { "" } else { "," }
            );
            written += row.len() as u64;
            file.write_all(row.as_bytes()).unwrap();
            i += 1;
        }
        file.write_all(b"]").unwrap();
        drop(file);
        let blob = Body::BlobRef(BlobRef {
            path: path.display().to_string(),
            bytes: written,
            media_type: Some("application/json".into()),
        });
        let model = InspectorModel::new("big", &response(blob, None));
        let table = model.table.unwrap();
        assert_eq!(table.len() as u64, i);
        assert_eq!(table.columns, ["id", "name", "email", "tags"]);
        assert!(model.text.len() <= TEXT_LIMIT + 100);
    }
}
