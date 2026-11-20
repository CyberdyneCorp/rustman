//! The normalizer (snapshot-testing "Normalization order"): secret
//! redaction, then `ignore`, `presets`, `replace`, `sort`, and finally the
//! canonical form (object keys sorted). The result is used both to store and
//! to compare, so normalizing twice gives the same bytes.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{Map, Value};
use serde_json_path::JsonPath;

use super::presets;
use super::{Exchange, SnapshotError};
use crate::model::snapshot::compile_regex;
use crate::model::{ComparedPart, Preset, SnapshotAssertion};
use crate::redact::Redactor;

/// What every ignored value becomes.
pub const IGNORED: &str = "«ignored»";

/// What an included header listed in `headers.redact` becomes.
pub const REDACTED_HEADER: &str = "«redacted»";

/// Bodies larger than this after normalization are stored as size and hash.
pub const MAX_STORED_BODY: usize = 1024 * 1024;

/// A normalized exchange: only the compared parts are present.
#[derive(Clone, Debug, PartialEq)]
pub struct Normalized {
    pub status: Option<u16>,
    /// Included headers, lower-cased and sorted.
    pub headers: Option<BTreeMap<String, String>>,
    pub body: Option<NormalizedBody>,
    /// Notes for the user, such as a body stored as a hash.
    pub warnings: Vec<String>,
}

/// A normalized body.
#[derive(Clone, Debug, PartialEq)]
pub enum NormalizedBody {
    Json(Value),
    Text(String),
    /// Non-UTF-8 bodies, non-textual content types and bodies over 1 MiB.
    Binary {
        size: u64,
        sha256: String,
    },
    /// The bounded event list of a subscription, each event normalized.
    Events(Vec<Value>),
}

/// The rules of one snapshot assertion, compiled once.
pub struct Normalizer<'a> {
    spec: &'a SnapshotAssertion,
    redactor: &'a Redactor,
    presets: &'a [Preset],
    ignore: Vec<JsonPath>,
    replace: Vec<(Regex, &'a str)>,
    sort: Vec<(JsonPath, Option<&'a str>)>,
}

impl<'a> Normalizer<'a> {
    pub fn new(spec: &'a SnapshotAssertion, redactor: &'a Redactor) -> Result<Self, SnapshotError> {
        let path = |p: &str| {
            JsonPath::parse(p)
                .map_err(|e| SnapshotError::Rule(format!("invalid JSONPath '{p}': {e}")))
        };
        let ignore = spec
            .ignore
            .iter()
            .flatten()
            .map(|p| path(p))
            .collect::<Result<_, _>>()?;
        let sort = spec
            .sort
            .iter()
            .flatten()
            .map(|r| Ok((path(&r.path)?, r.by.as_deref())))
            .collect::<Result<_, SnapshotError>>()?;
        let replace = spec
            .replace
            .iter()
            .flatten()
            .map(|r| {
                Ok((
                    compile_regex(&r.pattern).map_err(SnapshotError::Rule)?,
                    r.with.as_str(),
                ))
            })
            .collect::<Result<_, SnapshotError>>()?;
        Ok(Self {
            spec,
            redactor,
            presets: spec.effective_presets(),
            ignore,
            replace,
            sort,
        })
    }

    /// Normalize the compared parts of `exchange`.
    pub fn normalize(&self, exchange: &Exchange<'_>) -> Normalized {
        let mut warnings = Vec::new();
        let status = self
            .spec
            .compares(ComparedPart::Status)
            .then_some(exchange.status)
            .flatten();
        let headers = self
            .spec
            .compares(ComparedPart::Headers)
            .then(|| self.headers(exchange.headers));
        let body = self
            .spec
            .compares(ComparedPart::Body)
            .then(|| self.body(exchange, &mut warnings));
        Normalized {
            status,
            headers,
            body,
            warnings,
        }
    }

    fn headers(&self, headers: &[(String, String)]) -> BTreeMap<String, String> {
        let mut stored: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let included = self.spec.included_headers();
        for (name, value) in headers {
            let name = name.to_ascii_lowercase();
            if included.contains(&name) {
                let value = self.header_value(&name, value);
                stored.entry(name).or_default().push(value);
            }
        }
        stored.into_iter().map(|(n, v)| (n, v.join(", "))).collect()
    }

    fn header_value(&self, name: &str, value: &str) -> String {
        let redacted = self.redactor.header_value(name, value);
        if self.spec.redacts_header(name) {
            return REDACTED_HEADER.to_owned();
        }
        self.string(&redacted)
    }

    fn body(&self, exchange: &Exchange<'_>, warnings: &mut Vec<String>) -> NormalizedBody {
        if let Some(events) = exchange.events {
            return NormalizedBody::Events(events.iter().map(|e| self.json(e.clone())).collect());
        }
        // JSON is parsed before redaction and redacted value by value, so a
        // secret echoed as a number cannot break the document.
        let text = match std::str::from_utf8(exchange.body) {
            Ok(text) if is_textual(exchange.content_type()) => text,
            _ => return binary(&self.redactor.bytes(exchange.body)),
        };
        let normalized = match parse_json(text, exchange.content_type()) {
            Some(value) => NormalizedBody::Json(self.json(value)),
            None => NormalizedBody::Text(self.string(&self.redactor.text(text))),
        };
        cap_size(normalized, warnings)
    }

    /// The JSON pipeline: redact, ignore, presets, replace, sort, canonical.
    pub fn json(&self, mut value: Value) -> Value {
        self.redactor.json(&mut value);
        for path in &self.ignore {
            for pointer in pointers(path, &value) {
                if let Some(slot) = value.pointer_mut(&pointer) {
                    *slot = Value::String(IGNORED.to_owned());
                }
            }
        }
        self.rewrite_strings(&mut value, None);
        for (path, by) in &self.sort {
            for pointer in pointers(path, &value) {
                if let Some(Value::Array(items)) = value.pointer_mut(&pointer) {
                    items.sort_by(|a, b| compare_items(a, b, *by));
                }
            }
        }
        canonical(value)
    }

    /// Presets then replacements over a text (redaction already done).
    fn string(&self, text: &str) -> String {
        let mut out = presets::apply(self.presets, text);
        for (pattern, with) in &self.replace {
            if pattern.is_match(&out) {
                out = pattern.replace_all(&out, *with).into_owned();
            }
        }
        out
    }

    /// Apply presets and replacements to every string, and `epoch_ms` to
    /// numbers under time-like keys.
    fn rewrite_strings(&self, value: &mut Value, key: Option<&str>) {
        match value {
            Value::String(s) if self.is_epoch(key, s) => *s = presets::EPOCH_MS.to_owned(),
            Value::String(s) => *s = self.string(s),
            Value::Number(n) if self.is_epoch(key, &n.to_string()) => {
                *value = Value::String(presets::EPOCH_MS.to_owned());
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.rewrite_strings(v, None)),
            Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    self.rewrite_strings(v, Some(k));
                }
            }
            _ => {}
        }
    }

    fn is_epoch(&self, key: Option<&str>, digits: &str) -> bool {
        self.presets.contains(&Preset::EpochMs)
            && key.is_some_and(presets::epoch_ms_field)
            && presets::is_epoch_ms(digits)
    }
}

/// JSON pointers of every node `path` matches.
fn pointers(path: &JsonPath, value: &Value) -> Vec<String> {
    path.query_located(value)
        .locations()
        .map(|location| location.to_json_pointer())
        .collect()
}

/// Order of two array items: by the value at `by` (a dotted key path) or by
/// the whole item. Numbers compare numerically, strings lexically; other
/// values by their canonical JSON.
fn compare_items(a: &Value, b: &Value, by: Option<&str>) -> Ordering {
    let key = |v: &'_ Value| -> Value {
        match by {
            Some(by) => by
                .split('.')
                .try_fold(v, |v, k| v.get(k))
                .cloned()
                .unwrap_or(Value::Null),
            None => v.clone(),
        }
    };
    compare_values(&key(a), &key(b))
}

fn compare_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap_or(0.0), y.as_f64().unwrap_or(0.0));
            x.total_cmp(&y)
        }
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => canonical(a.clone())
            .to_string()
            .cmp(&canonical(b.clone()).to_string()),
    }
}

/// `value` with every object's keys sorted.
pub fn canonical(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let sorted: Map<String, Value> = entries
                .into_iter()
                .map(|(k, v)| (k, canonical(v)))
                .collect();
            Value::Object(sorted)
        }
        other => other,
    }
}

/// Whether a body with this content type is stored as text. A missing
/// content type counts as textual (the bytes must still be UTF-8).
pub fn is_textual(content_type: Option<&str>) -> bool {
    let Some(content_type) = content_type else {
        return true;
    };
    let essence = content_type.split(';').next().unwrap_or_default().trim();
    let essence = essence.to_ascii_lowercase();
    essence.starts_with("text/")
        || [
            "json",
            "xml",
            "yaml",
            "javascript",
            "graphql",
            "csv",
            "x-www-form-urlencoded",
        ]
        .iter()
        .any(|t| essence.contains(t))
}

/// The body as JSON when its content type says JSON, or it has none and
/// looks like an object or array.
fn parse_json(text: &str, content_type: Option<&str>) -> Option<Value> {
    let says_json = content_type.is_some_and(|c| c.to_ascii_lowercase().contains("json"));
    let looks_json = content_type.is_none() && text.trim_start().starts_with(['{', '[']);
    if !says_json && !looks_json {
        return None;
    }
    serde_json::from_str(text).ok()
}

fn binary(bytes: &[u8]) -> NormalizedBody {
    NormalizedBody::Binary {
        size: bytes.len() as u64,
        sha256: sha256_hex(bytes),
    }
}

/// A body over [`MAX_STORED_BODY`] once normalized becomes its size and hash.
fn cap_size(body: NormalizedBody, warnings: &mut Vec<String>) -> NormalizedBody {
    let text = match &body {
        NormalizedBody::Json(value) => value.to_string(),
        NormalizedBody::Text(text) if text.len() > MAX_STORED_BODY => text.clone(),
        _ => return body,
    };
    if text.len() <= MAX_STORED_BODY {
        return body;
    }
    warnings.push(format!(
        "body is {} B after normalization (over 1 MiB): stored as size and sha256; add ignore rules to keep it reviewable",
        text.len()
    ));
    binary(text.as_bytes())
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
