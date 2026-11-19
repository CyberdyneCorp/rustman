//! Hand-written mock files (spec: Hand-written mock files): every
//! `*.rman-mock.yaml` in the mocks directory (`mocks/` by default,
//! `[mock] dir`), read in file-name order; overrides are tried in that
//! order, then declaration order, and the first match wins. An override may
//! target a path the document lacks.
//!
//! ```yaml
//! overrides:
//!   - name: big payment             # optional, shown in the request log
//!     match:
//!       method: POST                # default: any
//!       path: /payments             # exact, a template (/users/{id}) or a glob (*, **)
//!       query: {currency: EUR}      # every pair must be present
//!       headers: {X-Tenant: acme}   # names in any case
//!       body:                       # JSONPath conditions, all must hold
//!         $.amount: {gt: 10000}     # eq, ne, gt, gte, lt, lte, contains, exists
//!     respond:
//!       status: 402                 # default 200
//!       headers: {X-Mock: "1"}      # values may be templates
//!       body: {error: "{{request.body.$.currency}} limit"}   # or body_file: payment.json
//!     delay_ms: 2000                # jitter_ms adds a uniform ± range; both at most 60000
//!     fault: {error_rate: 0.1, status: 503}  # or drop_connection: true, slow_body_bps: 100
//! ```

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use bytes::Bytes;
use globset::{GlobBuilder, GlobMatcher};
use http::{HeaderName, HeaderValue, StatusCode};
use rustman_core::LoadError;
use serde::Deserialize;
use serde_json::Value;
use serde_json_path::JsonPath;

use crate::faults::{Fault, Latency};
use crate::request::RequestView;
use crate::template::{self, Context, Rendered};

/// The default mocks directory, relative to the workspace root.
pub const MOCKS_DIR: &str = "mocks";

/// The suffix of mock files.
pub const MOCK_SUFFIX: &str = ".rman-mock.yaml";

/// The largest `body_file`, the default request body limit.
pub const MAX_BODY_FILE: u64 = 10 * 1024 * 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSpec {
    #[serde(default)]
    overrides: Vec<OverrideSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideSpec {
    name: Option<String>,
    #[serde(rename = "match")]
    matches: MatchSpec,
    #[serde(default)]
    respond: RespondSpec,
    #[serde(default)]
    delay_ms: u64,
    #[serde(default)]
    jitter_ms: u64,
    #[serde(default)]
    fault: Option<Fault>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchSpec {
    method: Option<String>,
    path: String,
    #[serde(default)]
    query: BTreeMap<String, String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: BTreeMap<String, ConditionSpec>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RespondSpec {
    status: Option<u16>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    body: Option<Value>,
    body_file: Option<PathBuf>,
}

/// A condition as written: operators, or a bare value meaning `eq`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConditionSpec {
    Ops(Ops),
    Value(Value),
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ops {
    eq: Option<Value>,
    ne: Option<Value>,
    gt: Option<f64>,
    gte: Option<f64>,
    lt: Option<f64>,
    lte: Option<f64>,
    contains: Option<String>,
    exists: Option<bool>,
}

impl Ops {
    /// Whether the first node `found` (if any) satisfies every operator.
    fn holds(&self, found: Option<&Value>) -> bool {
        if let Some(exists) = self.exists
            && exists != found.is_some()
        {
            return false;
        }
        let number = found.and_then(Value::as_f64);
        let compare = |bound: Option<f64>, ok: fn(f64, f64) -> bool| {
            bound.is_none_or(|b| number.is_some_and(|n| ok(n, b)))
        };
        self.eq.as_ref().is_none_or(|v| found == Some(v))
            && self.ne.as_ref().is_none_or(|v| found != Some(v))
            && compare(self.gt, |n, b| n > b)
            && compare(self.gte, |n, b| n >= b)
            && compare(self.lt, |n, b| n < b)
            && compare(self.lte, |n, b| n <= b)
            && self
                .contains
                .as_ref()
                .is_none_or(|needle| contains(found, needle))
    }
}

fn contains(found: Option<&Value>, needle: &str) -> bool {
    match found {
        Some(Value::String(text)) => text.contains(needle),
        Some(Value::Array(items)) => items.iter().any(|i| i.as_str() == Some(needle)),
        _ => false,
    }
}

/// How an override's `path` matches.
#[derive(Clone, Debug)]
enum PathPattern {
    Exact(String),
    /// Segments; `Some(name)` captures one segment.
    Template(Vec<(String, Option<String>)>),
    Glob(GlobMatcher),
}

impl PathPattern {
    fn parse(path: &str) -> Result<Self, String> {
        if path.contains('{') {
            let segments = path
                .split('/')
                .map(
                    |s| match s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                        Some(name) if !name.is_empty() => (String::new(), Some(name.to_owned())),
                        _ => (s.to_owned(), None),
                    },
                )
                .collect();
            return Ok(Self::Template(segments));
        }
        if path.contains(['*', '?', '[']) {
            let glob = GlobBuilder::new(path)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("invalid path pattern {path:?}: {}", e.kind()))?;
            return Ok(Self::Glob(glob.compile_matcher()));
        }
        Ok(Self::Exact(path.to_owned()))
    }

    /// The captured parameters when `path` matches.
    fn capture(&self, path: &str) -> Option<Vec<(String, String)>> {
        match self {
            Self::Exact(exact) => (exact == path).then(Vec::new),
            Self::Glob(glob) => glob.is_match(path).then(Vec::new),
            Self::Template(segments) => capture(segments, path),
        }
    }
}

fn capture(segments: &[(String, Option<String>)], path: &str) -> Option<Vec<(String, String)>> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != segments.len() {
        return None;
    }
    let mut params = Vec::new();
    for ((literal, name), part) in segments.iter().zip(parts) {
        match name {
            Some(name) if !part.is_empty() => {
                let value = percent_encoding::percent_decode_str(part).decode_utf8_lossy();
                params.push((name.clone(), value.into_owned()));
            }
            Some(_) => return None,
            None if literal != part => return None,
            None => {}
        }
    }
    Some(params)
}

/// An override's body: structured (sent as JSON), text, or file bytes.
#[derive(Clone, Debug, PartialEq)]
enum BodySource {
    Json(Value),
    Text(String),
    Bytes(Bytes),
}

/// One loaded override.
#[derive(Clone, Debug)]
pub struct Override {
    /// `name`, else `<file>#<n>`.
    pub name: String,
    method: Option<String>,
    path: PathPattern,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<(JsonPath, Ops)>,
    status: StatusCode,
    response_headers: Vec<(HeaderName, String)>,
    content: Option<(BodySource, Option<String>)>,
    pub latency: Latency,
    pub fault: Option<Fault>,
}

/// An override's response, rendered for one request.
#[derive(Clone, Debug, PartialEq)]
pub struct OverrideReply {
    pub status: StatusCode,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub body: Bytes,
    /// Template references that rendered as empty strings.
    pub unresolved: Vec<String>,
}

impl Override {
    /// The path parameters when this override answers `request` (whose
    /// path is tried as sent and without the server base path `stripped`).
    pub fn matches(
        &self,
        request: &RequestView<'_>,
        stripped: &str,
    ) -> Option<Vec<(String, String)>> {
        if self.method.as_ref().is_some_and(|m| m != request.method) {
            return None;
        }
        let params = self
            .path
            .capture(request.path)
            .or_else(|| self.path.capture(stripped))?;
        let query = self
            .query
            .iter()
            .all(|(n, v)| request.query_values(n).contains(&v.as_str()));
        let headers = self.headers.iter().all(|(n, v)| {
            request
                .headers
                .get_all(n.as_str())
                .iter()
                .any(|h| h == v.as_str())
        });
        (query && headers && self.body_holds(request)).then_some(params)
    }

    fn body_holds(&self, request: &RequestView<'_>) -> bool {
        if self.body.is_empty() {
            return true;
        }
        let Some(json) = request.json() else {
            return false;
        };
        self.body.iter().all(|(path, ops)| {
            let nodes = path.query(&json);
            ops.holds(nodes.first())
        })
    }

    /// The response for `request` (with its path parameters set).
    pub fn reply(&self, request: &RequestView<'_>) -> OverrideReply {
        let context = Context::new(request);
        let mut unresolved = Vec::new();
        let mut headers = Vec::new();
        let mut content_type = None;
        for (name, value) in &self.response_headers {
            let rendered = template::render_text(value, &context);
            unresolved.extend(rendered.unresolved);
            if let Ok(value) = HeaderValue::from_str(&rendered.value) {
                if name == http::header::CONTENT_TYPE {
                    content_type = Some(value.clone());
                }
                headers.push((name.clone(), value));
            }
        }
        let (body, default_type) = match &self.content {
            Some((source, default_type)) => {
                let rendered = render_body(source, &context);
                unresolved.extend(rendered.unresolved);
                (rendered.value, default_type.clone())
            }
            None => (Bytes::new(), None),
        };
        if content_type.is_none()
            && let Some(value) = default_type.and_then(|t| HeaderValue::from_str(&t).ok())
        {
            headers.push((http::header::CONTENT_TYPE, value));
        }
        OverrideReply {
            status: self.status,
            headers,
            body,
            unresolved,
        }
    }
}

fn render_body(source: &BodySource, context: &Context<'_, '_>) -> Rendered<Bytes> {
    match source {
        BodySource::Json(value) => {
            let rendered = template::render_value(value, context);
            Rendered {
                value: Bytes::from(rendered.value.to_string()),
                unresolved: rendered.unresolved,
            }
        }
        BodySource::Text(text) => {
            let rendered = template::render_text(text, context);
            Rendered {
                value: Bytes::from(rendered.value),
                unresolved: rendered.unresolved,
            }
        }
        BodySource::Bytes(bytes) => Rendered {
            value: bytes.clone(),
            unresolved: Vec::new(),
        },
    }
}

/// Every override of a mocks directory, in evaluation order.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    list: Vec<Override>,
}

impl Overrides {
    pub fn new(list: Vec<Override>) -> Self {
        Self { list }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// The first override answering `request`, with its path parameters.
    pub fn find(
        &self,
        request: &RequestView<'_>,
        stripped: &str,
    ) -> Option<(&Override, Vec<(String, String)>)> {
        self.list
            .iter()
            .find_map(|o| o.matches(request, stripped).map(|params| (o, params)))
    }

    /// Read every `*.rman-mock.yaml` of `dir` in file-name order; a
    /// missing directory has none.
    pub fn load_dir(dir: &Path) -> Result<Self, LoadError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(LoadError::new(dir, None, e.to_string())),
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_mock_file(p))
            .collect();
        files.sort();
        let mut list = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file)
                .map_err(|e| LoadError::new(&file, None, e.to_string()))?;
            list.extend(parse(&file, &text)?);
        }
        Ok(Self { list })
    }
}

fn is_mock_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(MOCK_SUFFIX))
}

/// Parse the overrides of `file` (its contents are `text`); `body_file`
/// paths are read relative to the directory of `file`.
pub fn parse(file: &Path, text: &str) -> Result<Vec<Override>, LoadError> {
    let spec: FileSpec = rustman_core::format::from_yaml(file, text)?;
    let dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stem = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    spec.overrides
        .into_iter()
        .enumerate()
        .map(|(i, spec)| {
            let fallback = format!("{stem}#{}", i + 1);
            build(spec, dir, fallback)
                .map_err(|e| LoadError::new(file, None, format!("override {}: {e}", i + 1)))
        })
        .collect()
}

fn build(spec: OverrideSpec, dir: &Path, fallback: String) -> Result<Override, String> {
    let latency = Latency {
        delay_ms: spec.delay_ms,
        jitter_ms: spec.jitter_ms,
    };
    latency.check()?;
    if let Some(fault) = &spec.fault {
        fault.check()?;
    }
    let matches = spec.matches;
    let respond = spec.respond;
    let status = respond.status.unwrap_or(200);
    Ok(Override {
        name: spec.name.unwrap_or(fallback),
        method: matches.method.as_deref().map(method).transpose()?,
        path: PathPattern::parse(&matches.path)?,
        query: matches.query.into_iter().collect(),
        headers: matches.headers.into_iter().collect(),
        body: conditions(matches.body)?,
        status: StatusCode::from_u16(status).map_err(|_| format!("invalid status {status}"))?,
        response_headers: response_headers(respond.headers)?,
        content: content(respond.body, respond.body_file.as_deref(), dir)?,
        latency,
        fault: spec.fault,
    })
}

fn method(text: &str) -> Result<String, String> {
    http::Method::from_bytes(text.to_ascii_uppercase().as_bytes())
        .map(|m| m.as_str().to_owned())
        .map_err(|_| format!("invalid method {text:?}"))
}

fn conditions(specs: BTreeMap<String, ConditionSpec>) -> Result<Vec<(JsonPath, Ops)>, String> {
    specs
        .into_iter()
        .map(|(path, spec)| {
            let parsed =
                JsonPath::parse(&path).map_err(|e| format!("invalid JSONPath {path:?}: {e}"))?;
            let ops = match spec {
                ConditionSpec::Ops(ops) => ops,
                ConditionSpec::Value(value) => Ops {
                    eq: Some(value),
                    ..Ops::default()
                },
            };
            Ok((parsed, ops))
        })
        .collect()
}

fn response_headers(map: BTreeMap<String, String>) -> Result<Vec<(HeaderName, String)>, String> {
    map.into_iter()
        .map(|(name, value)| {
            let header = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| format!("invalid header name {name:?}"))?;
            let framing = header == http::header::CONTENT_LENGTH;
            if framing || rustman_webhook::replay::NOT_REPLAYED.contains(&header.as_str()) {
                return Err(format!("header {header} is set by the server"));
            }
            template::check(&value).map_err(|e| format!("header {header}: {e}"))?;
            Ok((header, value))
        })
        .collect()
}

type Content = Option<(BodySource, Option<String>)>;

/// The body and its default `Content-Type`.
fn content(body: Option<Value>, file: Option<&Path>, dir: &Path) -> Result<Content, String> {
    match (body, file) {
        (Some(_), Some(_)) => Err("set body or body_file, not both".to_owned()),
        (Some(Value::String(text)), None) => {
            template::check(&text).map_err(|e| format!("body: {e}"))?;
            let kind = if serde_json::from_str::<Value>(&text).is_ok() {
                "application/json"
            } else {
                "text/plain; charset=utf-8"
            };
            Ok(Some((BodySource::Text(text), Some(kind.to_owned()))))
        }
        (Some(value), None) => {
            check_strings(&value)?;
            Ok(Some((
                BodySource::Json(value),
                Some("application/json".to_owned()),
            )))
        }
        (None, Some(file)) => read_body_file(file, dir)
            .map(Some)
            .map_err(|e| format!("cannot read body_file {}: {e}", file.display())),
        (None, None) => Ok(None),
    }
}

fn check_strings(value: &Value) -> Result<(), String> {
    match value {
        Value::String(text) => template::check(text).map_err(|e| format!("body: {e}")),
        Value::Array(items) => items.iter().try_for_each(check_strings),
        Value::Object(map) => map.values().try_for_each(check_strings),
        _ => Ok(()),
    }
}

/// Read `file`, which must stay inside `dir` and be at most
/// [`MAX_BODY_FILE`] bytes. UTF-8 files are templates.
fn read_body_file(file: &Path, dir: &Path) -> Result<(BodySource, Option<String>), String> {
    if !file.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("must be a relative path inside the mocks directory".to_owned());
    }
    let root = dir.canonicalize().map_err(|e| e.to_string())?;
    let path = root.join(file).canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(&root) {
        return Err("must stay inside the mocks directory".to_owned());
    }
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_BODY_FILE {
        return Err(format!("larger than {MAX_BODY_FILE} bytes"));
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let is_json = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"));
    match String::from_utf8(bytes) {
        Ok(text) => {
            let kind = if is_json {
                "application/json"
            } else {
                "text/plain; charset=utf-8"
            };
            Ok((BodySource::Text(text), Some(kind.to_owned())))
        }
        Err(e) => Ok((
            BodySource::Bytes(Bytes::from(e.into_bytes())),
            Some("application/octet-stream".to_owned()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;

    fn load(text: &str) -> Result<Vec<Override>, LoadError> {
        parse(Path::new("mocks/a.rman-mock.yaml"), text)
    }

    fn view<'a>(
        method: &'a str,
        path: &'a str,
        query: &str,
        headers: &'a HeaderMap,
        body: &'a Bytes,
    ) -> RequestView<'a> {
        RequestView::new(method, path, Some(query), headers, body)
    }

    #[test]
    fn body_conditions_select_the_override() {
        let list = load(
            "overrides:\n  - match:\n      method: post\n      path: /payments\n      body:\n        $.amount: {gt: 10000}\n    respond: {status: 402}\n",
        )
        .unwrap();
        let headers = HeaderMap::new();
        let big = Bytes::from_static(br#"{"amount": 50000}"#);
        let small = Bytes::from_static(br#"{"amount": 5}"#);
        let o = &list[0];
        assert_eq!(o.name, "a.rman-mock.yaml#1");
        assert!(
            o.matches(&view("POST", "/payments", "", &headers, &big), "/payments")
                .is_some()
        );
        assert!(
            o.matches(
                &view("POST", "/payments", "", &headers, &small),
                "/payments"
            )
            .is_none()
        );
        assert!(
            o.matches(&view("GET", "/payments", "", &headers, &big), "/payments")
                .is_none()
        );
        assert_eq!(
            o.reply(&view("POST", "/payments", "", &headers, &big))
                .status,
            402
        );
    }

    #[test]
    fn operators() {
        let n = serde_json::json!(5);
        let s = serde_json::json!("abc");
        let ops = |text: &str| serde_json::from_str::<Ops>(text).unwrap();
        assert!(ops(r#"{"gte": 5, "lt": 6}"#).holds(Some(&n)));
        assert!(!ops(r#"{"gt": 5}"#).holds(Some(&n)));
        assert!(ops(r#"{"lte": 5, "ne": 4}"#).holds(Some(&n)));
        assert!(ops(r#"{"contains": "b"}"#).holds(Some(&s)));
        assert!(ops(r#"{"exists": false}"#).holds(None));
        assert!(!ops(r#"{"eq": 5}"#).holds(None));
        assert!(!ops(r#"{"gt": 1}"#).holds(Some(&s)));
    }

    #[test]
    fn paths_query_and_headers() {
        let list = load(concat!(
            "overrides:\n",
            "  - match: {path: '/users/{id}', query: {v: '2'}, headers: {X-Tenant: acme}}\n",
            "    respond: {body: {id: '{{request.path.id}}'}}\n",
            "  - match: {path: '/files/**'}\n",
        ))
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-tenant", HeaderValue::from_static("acme"));
        let body = Bytes::new();
        let request = view("GET", "/users/42", "v=2", &headers, &body);
        let params = list[0].matches(&request, "/users/42").unwrap();
        assert_eq!(params, [("id".to_owned(), "42".to_owned())]);
        let mut request = request;
        request.params = params;
        let reply = list[0].reply(&request);
        assert_eq!(reply.body.as_ref(), br#"{"id":"42"}"#);
        assert_eq!(reply.headers[0].1, "application/json");
        let other = view("GET", "/users/42", "v=3", &headers, &body);
        assert!(list[0].matches(&other, "/users/42").is_none());
        let file = view("GET", "/files/a/b.txt", "", &headers, &body);
        assert!(list[1].matches(&file, "/files/a/b.txt").is_some());
        assert!(
            list[1]
                .matches(&view("GET", "/filesx", "", &headers, &body), "/filesx")
                .is_none()
        );
    }

    #[test]
    fn invalid_overrides_name_their_position() {
        let err = load("overrides:\n  - match: {path: /a}\n    delay_ms: 60001\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("override 1: delay_ms 60001 exceeds 60000"),
            "{err}"
        );
        let err = load(
            "overrides:\n  - match: {path: /a}\n    respond: {headers: {Content-Length: '1'}}\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("set by the server"), "{err}");
        let err =
            load("overrides:\n  - match: {path: /a}\n    respond: {body: '{{oops'}\n").unwrap_err();
        assert!(err.to_string().contains("body:"), "{err}");
        let err = load("overrides:\n  - match: {path: /a, nope: 1}\n").unwrap_err();
        assert!(err.to_string().contains("a.rman-mock.yaml"), "{err}");
    }

    #[test]
    fn files_load_in_name_order_and_body_files_stay_inside() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
        write(
            "b.rman-mock.yaml",
            "overrides:\n  - name: second\n    match: {path: /x}\n",
        );
        write(
            "a.rman-mock.yaml",
            "overrides:\n  - name: first\n    match: {path: /x}\n    respond: {body_file: ok.json}\n",
        );
        write("ignored.yaml", "nope: [");
        write("ok.json", r#"{"ok": "{{request.query.q}}"}"#);
        let overrides = Overrides::load_dir(dir.path()).unwrap();
        assert_eq!(overrides.len(), 2);
        let headers = HeaderMap::new();
        let body = Bytes::new();
        let request = view("GET", "/x", "q=1", &headers, &body);
        let (found, _) = overrides.find(&request, "/x").unwrap();
        assert_eq!(found.name, "first");
        assert_eq!(found.reply(&request).body.as_ref(), br#"{"ok": "1"}"#);
        write(
            "c.rman-mock.yaml",
            "overrides:\n  - match: {path: /x}\n    respond: {body_file: ../secret}\n",
        );
        let err = Overrides::load_dir(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("inside the mocks directory"),
            "{err}"
        );
        assert!(
            Overrides::load_dir(&dir.path().join("missing"))
                .unwrap()
                .is_empty()
        );
    }
}
