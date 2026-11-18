//! Data rows (runner parity D1): the CSV and JSON reader behind
//! `rustman run --data`, shared with load testing's `--data`, so a data file
//! means the same thing everywhere.
//!
//! - CSV follows RFC 4180. The first record is the header, a UTF-8 BOM is
//!   stripped and every value is a string. Every row must have as many
//!   fields as the header.
//! - JSON must be an array of objects. Scalars become strings (`null`
//!   becomes empty text) and nested values become compact JSON text.
//!
//! Rows are numbered from 1, without the header: "row 1" is the first data
//! row, as in iteration labels.

use std::path::Path;
use std::sync::Arc;

use serde_json::value::RawValue;

use crate::secret::Secret;
use crate::vars::{Layer, Lookup, VarSource};

/// Why a data file could not be read. Messages name no file: callers prefix
/// them (`--data users.csv: no rows`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DataError {
    #[error("no rows")]
    NoRows,
    #[error("row {row} has {found} field{}, expected {expected}", plural(*.found))]
    Ragged {
        row: usize,
        found: usize,
        expected: usize,
    },
    #[error("expected an array of objects")]
    NotArrayOfObjects,
    #[error("duplicate column '{0}'")]
    DuplicateColumn(String),
    #[error("column {0} of the header has no name")]
    EmptyColumn(usize),
    #[error("row {0} is not valid UTF-8")]
    NotUtf8(usize),
    #[error("invalid CSV: {0}")]
    Csv(String),
    #[error("invalid JSON: {0}")]
    Json(String),
    #[error("unsupported data file (use .csv or .json)")]
    UnsupportedFormat,
    #[error("cannot read: {0}")]
    Io(String),
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The format of a data file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataFormat {
    Csv,
    Json,
}

impl DataFormat {
    /// The format named by the extension of `path` (`.csv` or `.json`,
    /// in any case).
    pub fn of_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "csv" => Some(Self::Csv),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// One row: its values, in column order. Every value is text. Column
/// names are shared by every row, not copied into each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataRow {
    values: Vec<(Arc<str>, String)>,
}

impl DataRow {
    /// The value of `column`, `None` when this row does not have it.
    pub fn get(&self, column: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(name, _)| &**name == column)
            .map(|(_, value)| value.as_str())
    }

    /// Every `(column, value)` pair, in column order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(n, v)| (&**n, v.as_str()))
    }
}

/// The rows of a data file: at least one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataRows {
    columns: Vec<String>,
    rows: Vec<DataRow>,
}

impl DataRows {
    /// Read `path` as CSV or JSON, by its extension.
    pub fn read(path: &Path) -> Result<Self, DataError> {
        let format = DataFormat::of_path(path).ok_or(DataError::UnsupportedFormat)?;
        let bytes = std::fs::read(path).map_err(|e| DataError::Io(e.to_string()))?;
        Self::parse(format, &bytes)
    }

    /// Parse `bytes` in `format`.
    pub fn parse(format: DataFormat, bytes: &[u8]) -> Result<Self, DataError> {
        match format {
            DataFormat::Csv => Self::from_csv(bytes),
            DataFormat::Json => Self::from_json(bytes),
        }
    }

    /// Parse RFC 4180 CSV with a header row.
    pub fn from_csv(bytes: &[u8]) -> Result<Self, DataError> {
        let bytes = strip_bom(bytes);
        if let Some(error) = quote_error(bytes) {
            return Err(DataError::Csv(error));
        }
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(bytes);
        let mut records = reader.byte_records();
        let Some(header) = records.next() else {
            return Err(DataError::NoRows);
        };
        let columns = header_columns(&header.map_err(csv_error)?)?;
        let names: Vec<Arc<str>> = columns.iter().map(|c| Arc::from(c.as_str())).collect();
        let mut rows = Vec::new();
        for (index, record) in records.enumerate() {
            let row = index + 1;
            let record = record.map_err(csv_error)?;
            if record.len() != columns.len() {
                return Err(DataError::Ragged {
                    row,
                    found: record.len(),
                    expected: columns.len(),
                });
            }
            let values = names
                .iter()
                .zip(record.iter())
                .map(|(column, field)| Ok((Arc::clone(column), text(field, row)?)))
                .collect::<Result<_, DataError>>()?;
            rows.push(DataRow { values });
        }
        Self::new(columns, rows)
    }

    /// Parse a JSON array of objects. Values keep their original text:
    /// `1.10` stays `1.10` and `1e2` stays `1e2`, at any precision.
    pub fn from_json(bytes: &[u8]) -> Result<Self, DataError> {
        let bytes = strip_bom(bytes);
        let items: Vec<&RawValue> = serde_json::from_slice(bytes).map_err(json_shape_error)?;
        let mut names: Vec<Arc<str>> = Vec::new();
        let mut rows = Vec::with_capacity(items.len());
        for item in items {
            let RawObject(fields) =
                serde_json::from_str(item.get()).map_err(|_| DataError::NotArrayOfObjects)?;
            let mut values: Vec<(Arc<str>, String)> = Vec::with_capacity(fields.len());
            for (key, raw) in fields {
                let value = scalar_text(raw)?;
                // A repeated key keeps its last value, as JSON parsers do.
                match values.iter_mut().find(|(name, _)| **name == *key) {
                    Some(slot) => slot.1 = value,
                    None => values.push((column_name(&mut names, &key), value)),
                }
            }
            rows.push(DataRow { values });
        }
        let columns = names.iter().map(|n| n.to_string()).collect();
        Self::new(columns, rows)
    }

    fn new(columns: Vec<String>, rows: Vec<DataRow>) -> Result<Self, DataError> {
        if rows.is_empty() {
            return Err(DataError::NoRows);
        }
        Ok(Self { columns, rows })
    }

    /// Every column, in header order (JSON: in order of first appearance).
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    pub fn rows(&self) -> &[DataRow] {
        &self.rows
    }

    /// The number of rows: never zero.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Always `false`: a data file without rows is an error.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Every non-empty value of a secret column, as a secret named after
    /// its column, so output sinks can redact it before it is used.
    pub fn secrets(&self, secret: &SecretColumns) -> Vec<Secret<String>> {
        self.rows
            .iter()
            .flat_map(DataRow::iter)
            .filter(|(column, value)| !value.is_empty() && secret.is_secret(column))
            .map(|(column, value)| Secret::new(column, value.to_owned()))
            .collect()
    }
}

fn header_columns(header: &csv::ByteRecord) -> Result<Vec<String>, DataError> {
    let mut columns: Vec<String> = Vec::with_capacity(header.len());
    for (index, field) in header.iter().enumerate() {
        let name = std::str::from_utf8(field)
            .map_err(|_| DataError::Csv("the header row is not valid UTF-8".into()))?;
        if name.is_empty() {
            return Err(DataError::EmptyColumn(index + 1));
        }
        if columns.iter().any(|c| c == name) {
            return Err(DataError::DuplicateColumn(name.to_owned()));
        }
        columns.push(name.to_owned());
    }
    Ok(columns)
}

/// The first misplaced quote, as RFC 4180 reads them: a quoted field that
/// opens and never closes, or text after the quote that closes one
/// (`"a"b`). The `csv` crate accepts both, reading up to the end of the
/// file or joining the text (`ab`). A quote opens a quoted field only at
/// the start of a field; inside one, `""` is an escaped quote and a lone
/// `"` closes it. A quote inside an unquoted field (`5" monitor`) is a
/// literal character, as the `csv` crate reads it.
fn quote_error(bytes: &[u8]) -> Option<String> {
    let mut record = 0;
    let mut opened_in = None;
    let mut field_start = true;
    let mut bytes = bytes.iter().peekable();
    while let Some(&byte) = bytes.next() {
        if opened_in.is_some() {
            if byte == b'"' && bytes.next_if_eq(&&b'"').is_none() {
                opened_in = None;
                if bytes
                    .peek()
                    .is_some_and(|b| !matches!(b, b',' | b'\n' | b'\r'))
                {
                    return Some(format!(
                        "text after the closing quote of a quoted field in {}",
                        row_label(record)
                    ));
                }
            }
            continue;
        }
        if byte == b'"' && field_start {
            opened_in = Some(record);
        }
        if byte == b'\n' {
            record += 1;
        }
        field_start = matches!(byte, b',' | b'\n' | b'\r');
    }
    opened_in.map(|row| format!("unterminated quoted field in {}", row_label(row)))
}

/// The shared name of JSON key `key`, added to `names` (the columns, in
/// order of first appearance) when it is new.
fn column_name(names: &mut Vec<Arc<str>>, key: &str) -> Arc<str> {
    if let Some(name) = names.iter().find(|n| &***n == key) {
        return Arc::clone(name);
    }
    let name: Arc<str> = Arc::from(key);
    names.push(Arc::clone(&name));
    name
}

/// `bytes` without a leading UTF-8 byte order mark, which Windows editors
/// and PowerShell 5 write.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
}

fn row_label(record: usize) -> String {
    if record == 0 {
        "the header".to_owned()
    } else {
        format!("row {record}")
    }
}

fn text(field: &[u8], row: usize) -> Result<String, DataError> {
    String::from_utf8(field.to_vec()).map_err(|_| DataError::NotUtf8(row))
}

fn csv_error(e: csv::Error) -> DataError {
    DataError::Csv(e.to_string())
}

/// The members of one JSON object, in document order, with their values
/// as unparsed text.
struct RawObject<'a>(Vec<(String, &'a RawValue)>);

impl<'de: 'a, 'a> serde::Deserialize<'de> for RawObject<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Members<'a>(std::marker::PhantomData<&'a ()>);
        impl<'de: 'a, 'a> serde::de::Visitor<'de> for Members<'a> {
            type Value = RawObject<'a>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry::<String, &'de RawValue>()? {
                    members.push(member);
                }
                Ok(RawObject(members))
            }
        }
        deserializer.deserialize_map(Members(std::marker::PhantomData))
    }
}

/// A JSON value as row text: strings unquoted, `null` empty, numbers and
/// booleans exactly as written, and arrays and objects as their original
/// text without insignificant whitespace (compact JSON).
fn scalar_text(raw: &RawValue) -> Result<String, DataError> {
    let text = raw.get();
    match text.as_bytes().first() {
        Some(b'"') => serde_json::from_str(text).map_err(|e| DataError::Json(e.to_string())),
        Some(b'[' | b'{') => Ok(compact(text)),
        _ if text == "null" => Ok(String::new()),
        _ => Ok(text.to_owned()),
    }
}

/// Valid JSON `text` without whitespace outside strings.
fn compact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut in_string, mut escaped) = (false, false);
    for c in text.chars() {
        if in_string {
            in_string = escaped || c != '"';
            escaped = !escaped && c == '\\';
        } else if c.is_ascii_whitespace() {
            continue;
        } else {
            in_string = c == '"';
        }
        out.push(c);
    }
    out
}

/// A JSON document that parses but is not an array (of anything) is the
/// wrong shape; anything else is malformed.
fn json_shape_error(e: serde_json::Error) -> DataError {
    if e.classify() == serde_json::error::Category::Data {
        DataError::NotArrayOfObjects
    } else {
        DataError::Json(e.to_string())
    }
}

/// Which columns hold secrets: the ones named explicitly (`--secret-columns`,
/// `_collection.yaml` `secret_columns`) and any whose name contains
/// `password`, `token`, `secret` or `api_key` (ignoring case, with `-` read
/// as `_`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecretColumns {
    named: Vec<String>,
}

/// Name fragments that make a column secret on their own.
const SECRET_NAME_PARTS: [&str; 4] = ["password", "token", "secret", "api_key"];

impl SecretColumns {
    pub fn new(named: impl IntoIterator<Item = String>) -> Self {
        Self {
            named: named.into_iter().collect(),
        }
    }

    pub fn is_secret(&self, column: &str) -> bool {
        self.named.iter().any(|n| n == column) || secret_by_name(column)
    }
}

/// Whether `column` is named like a secret (`password`, `token`, `secret`
/// or `api_key`).
pub fn secret_by_name(column: &str) -> bool {
    let name = column.to_ascii_lowercase().replace('-', "_");
    SECRET_NAME_PARTS.iter().any(|part| name.contains(part))
}

/// One row as a source of the precedence chain, at [`Layer::DataRow`]:
/// values of secret columns are secrets named after their column.
pub struct RowSource<'a> {
    row: &'a DataRow,
    secret: &'a SecretColumns,
}

impl<'a> RowSource<'a> {
    pub fn new(row: &'a DataRow, secret: &'a SecretColumns) -> Self {
        Self { row, secret }
    }
}

impl VarSource for RowSource<'_> {
    fn layer(&self) -> Layer {
        Layer::DataRow
    }

    fn lookup(&self, name: &str) -> Option<Lookup> {
        let value = self.row.get(name)?.to_owned();
        Some(if self.secret.is_secret(name) {
            Lookup::Secret(Secret::new(name, value))
        } else {
            Lookup::Value(value)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv(text: &str) -> Result<DataRows, DataError> {
        DataRows::from_csv(text.as_bytes())
    }

    fn json(text: &str) -> Result<DataRows, DataError> {
        DataRows::from_json(text.as_bytes())
    }

    fn values(rows: &DataRows, column: &str) -> Vec<String> {
        rows.rows()
            .iter()
            .map(|r| r.get(column).unwrap_or("<none>").to_owned())
            .collect()
    }

    #[test]
    fn csv_rows_follow_the_header() {
        let rows = csv("email,role\r\nana@example.com,admin\r\nbo@example.com,user\r\n").unwrap();
        assert_eq!(rows.columns(), ["email", "role"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            values(&rows, "email"),
            ["ana@example.com", "bo@example.com"]
        );
        assert_eq!(values(&rows, "role"), ["admin", "user"]);
    }

    #[test]
    fn csv_strips_a_utf8_bom() {
        let rows = DataRows::from_csv(b"\xEF\xBB\xBFid,name\n1,ana\n").unwrap();
        assert_eq!(rows.columns(), ["id", "name"]);
        assert_eq!(rows.rows()[0].get("id"), Some("1"));
    }

    #[test]
    fn csv_quoted_fields_keep_commas_quotes_and_newlines() {
        let rows = csv("name,note\n\"Doe, Jane\",\"say \"\"hi\"\"\nthen go\"\n").unwrap();
        assert_eq!(rows.rows()[0].get("name"), Some("Doe, Jane"));
        assert_eq!(rows.rows()[0].get("note"), Some("say \"hi\"\nthen go"));
    }

    #[test]
    fn unterminated_quoted_fields_are_invalid_csv() {
        let err = csv("user,jk\nx,\"unterminated\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid CSV: unterminated quoted field in row 1"
        );
        let err = csv("ok\n1\n\"a\"\"b\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid CSV: unterminated quoted field in row 2"
        );
        let err = csv("\"id\n1\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid CSV: unterminated quoted field in the header"
        );
    }

    /// Regression: the `csv` crate read `"a"b` as `ab`; RFC 4180 allows
    /// nothing between a closing quote and the next separator.
    #[test]
    fn text_after_a_closing_quote_is_invalid_csv() {
        let err = csv("id,name\n1,ok\n2,\"a\"b\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid CSV: text after the closing quote of a quoted field in row 2"
        );
        let err = csv("id,note\n1,\"multi\nline\" x\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid CSV: text after the closing quote of a quoted field in row 1"
        );
        let err = csv("\"id\"x\n1\n").unwrap_err();
        assert!(err.to_string().ends_with("in the header"), "{err}");
        let rows = csv("a,b\n\"x\",\"y\"\r\n\"\",\"\"\"\"").unwrap();
        assert_eq!(values(&rows, "a"), ["x", ""]);
        assert_eq!(values(&rows, "b"), ["y", "\""]);
    }

    /// A quote inside an unquoted field is a literal character, as the `csv`
    /// crate reads it; only a quote opening a field starts a quoted field.
    #[test]
    fn quotes_inside_unquoted_fields_are_literal() {
        let rows = csv("user,item\nana,5\" monitor\nbo,\"a, \"\"b\"\"\"\r\n").unwrap();
        assert_eq!(values(&rows, "item"), ["5\" monitor", "a, \"b\""]);
        let err = csv("user,item\nana,5\" TV\nbo,\"open\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid CSV: unterminated quoted field in row 2"
        );
    }

    #[test]
    fn json_strips_a_utf8_bom() {
        let rows = DataRows::from_json(b"\xEF\xBB\xBF[{\"user\": \"x\"}]").unwrap();
        assert_eq!(values(&rows, "user"), ["x"]);
    }

    #[test]
    fn empty_csv_cells_are_defined_as_empty_text() {
        let rows = csv("email,role\nbo@x.com,\n").unwrap();
        assert_eq!(rows.rows()[0].get("role"), Some(""));
    }

    #[test]
    fn csv_values_stay_strings() {
        let rows = csv("n,flag\n007,true\n").unwrap();
        assert_eq!(rows.rows()[0].get("n"), Some("007"));
        assert_eq!(rows.rows()[0].get("flag"), Some("true"));
    }

    #[test]
    fn ragged_csv_rows_name_the_row_and_counts() {
        let err = csv("a,b\n1,2\n3,4\n5,6\n7,8,9\n").unwrap_err();
        assert_eq!(err.to_string(), "row 4 has 3 fields, expected 2");
        let err = csv("a,b\n1\n").unwrap_err();
        assert_eq!(err.to_string(), "row 1 has 1 field, expected 2");
    }

    #[test]
    fn empty_data_files_have_no_rows() {
        assert_eq!(csv("email,role\n").unwrap_err().to_string(), "no rows");
        assert_eq!(csv("").unwrap_err().to_string(), "no rows");
        assert_eq!(json("[]").unwrap_err().to_string(), "no rows");
    }

    #[test]
    fn bad_csv_headers_are_rejected() {
        assert_eq!(
            csv("a,a\n1,2\n").unwrap_err().to_string(),
            "duplicate column 'a'"
        );
        assert_eq!(
            csv("a,\n1,2\n").unwrap_err().to_string(),
            "column 2 of the header has no name"
        );
    }

    #[test]
    fn csv_values_must_be_utf8() {
        let err = DataRows::from_csv(b"a\nok\n\xFF\n").unwrap_err();
        assert_eq!(err.to_string(), "row 2 is not valid UTF-8");
    }

    #[test]
    fn json_scalars_become_strings_and_nested_values_compact_json() {
        let rows = json(r#"[{"id": 1, "tags": ["a"], "ok": true, "n": null, "o": {"k": 1.5}}, {"id": 2, "tags": []}]"#)
            .unwrap();
        let first = &rows.rows()[0];
        assert_eq!(first.get("id"), Some("1"));
        assert_eq!(first.get("tags"), Some(r#"["a"]"#));
        assert_eq!(first.get("ok"), Some("true"));
        assert_eq!(first.get("n"), Some(""));
        assert_eq!(first.get("o"), Some(r#"{"k":1.5}"#));
        assert_eq!(rows.rows()[1].get("tags"), Some("[]"));
        assert_eq!(rows.rows()[1].get("ok"), None);
        assert_eq!(rows.columns(), ["id", "tags", "ok", "n", "o"]);
    }

    /// Regression: numbers went through f64/u64 and lost their text.
    #[test]
    fn json_numbers_keep_their_original_text() {
        let rows = json(
            r#"[{"price": 1.10, "e": 1e2, "big": 12345678901234567890123, "neg": -0.0,
                "nested": {"a": [1.10, 1E+2], "s": "x y\" }"}}]"#,
        )
        .unwrap();
        let row = &rows.rows()[0];
        assert_eq!(row.get("price"), Some("1.10"));
        assert_eq!(row.get("e"), Some("1e2"));
        assert_eq!(row.get("big"), Some("12345678901234567890123"));
        assert_eq!(row.get("neg"), Some("-0.0"));
        assert_eq!(
            row.get("nested"),
            Some(r#"{"a":[1.10,1E+2],"s":"x y\" }"}"#)
        );
    }

    #[test]
    fn repeated_json_keys_keep_the_last_value() {
        let rows = json(r#"[{"a": 1, "b": 2, "a": 3}]"#).unwrap();
        assert_eq!(
            rows.rows()[0].iter().collect::<Vec<_>>(),
            [("a", "3"), ("b", "2")]
        );
    }

    #[test]
    fn json_must_be_an_array_of_objects() {
        for text in [r#"{"id": 1}"#, "[1, 2]", r#"[{"id": 1}, "x"]"#, "\"x\""] {
            assert_eq!(
                json(text).unwrap_err().to_string(),
                "expected an array of objects",
                "{text}"
            );
        }
        assert!(
            json("[{")
                .unwrap_err()
                .to_string()
                .starts_with("invalid JSON: "),
            "malformed JSON names the parser error"
        );
    }

    #[test]
    fn files_are_read_by_extension() {
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("users.CSV");
        std::fs::write(&csv_path, "id\n1\n").unwrap();
        assert_eq!(DataRows::read(&csv_path).unwrap().len(), 1);
        let json_path = dir.path().join("cases.json");
        std::fs::write(&json_path, r#"[{"id": 1}]"#).unwrap();
        assert_eq!(DataRows::read(&json_path).unwrap().len(), 1);
        let txt = dir.path().join("rows.txt");
        std::fs::write(&txt, "id\n1\n").unwrap();
        assert_eq!(
            DataRows::read(&txt).unwrap_err().to_string(),
            "unsupported data file (use .csv or .json)"
        );
        let missing = DataRows::read(&dir.path().join("missing.csv")).unwrap_err();
        assert!(missing.to_string().starts_with("cannot read: "));
    }

    #[test]
    fn secret_columns_are_named_or_named_like_secrets() {
        let secret = SecretColumns::new(["pin".to_owned()]);
        for column in [
            "pin",
            "password",
            "user_password",
            "API_KEY",
            "api-key",
            "access_token",
            "client_secret",
        ] {
            assert!(secret.is_secret(column), "{column}");
        }
        for column in ["email", "role", "key", "pins"] {
            assert!(!secret.is_secret(column), "{column}");
        }
    }

    /// The precedence table of the eight layers (runner parity D2), with a
    /// real data row: each row of the table removes the winner above it.
    #[test]
    fn the_data_row_sits_directly_below_in_run_captures() {
        use crate::model::{VarValue, Variables};
        use crate::vars::{Resolver, VariablesSource};
        let table = [
            (Layer::Override, "override"),
            (Layer::Capture, "capture"),
            (Layer::DataRow, "row"),
            (Layer::Request, "request"),
            (Layer::Collection, "collection"),
            (Layer::PersistedCapture, "persisted"),
            (Layer::Environment, "environment"),
            (Layer::Settings, "settings"),
        ];
        let rows = csv("x\nrow\n").unwrap();
        let secret = SecretColumns::default();
        for (i, (_, winner)) in table.iter().enumerate() {
            let mut resolver = Resolver::new();
            for (layer, value) in &table[i..] {
                if *layer == Layer::DataRow {
                    resolver.push(RowSource::new(&rows.rows()[0], &secret));
                } else {
                    let vars =
                        Variables::new(vec![("x".into(), VarValue::String((*value).into()))]);
                    resolver.push(VariablesSource::owned(*layer, vars));
                }
            }
            assert_eq!(resolver.resolve("{{x}}").unwrap().text, *winner, "row {i}");
        }
    }

    #[test]
    fn rows_resolve_at_the_data_row_layer_with_secret_columns_as_secrets() {
        let rows = csv("email,password\nana@example.com,hunter2\n,\n").unwrap();
        let secret = SecretColumns::default();
        let source = RowSource::new(&rows.rows()[0], &secret);
        assert_eq!(source.layer(), Layer::DataRow);
        assert_eq!(
            source.lookup("email"),
            Some(Lookup::Value("ana@example.com".into()))
        );
        assert_eq!(
            source.lookup("password"),
            Some(Lookup::Secret(Secret::new("password", "hunter2".into())))
        );
        assert_eq!(source.lookup("missing"), None);
        let secrets = rows.secrets(&secret);
        assert_eq!(secrets.len(), 1, "empty values are not secrets");
        assert_eq!(secrets[0].name(), "password");
    }
}
