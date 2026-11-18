//! Mock responses (design D6, spec: Mock responses): an ordered list read
//! from `webhook/mocks.yaml`, matched by method and path glob, first match
//! wins. Reloading keeps the previous mocks when the file is invalid.
//!
//! ```yaml
//! mocks:
//!   - method: POST            # or "*" (the default)
//!     path: /hooks/github     # exact, or a glob: * within a segment, ** across
//!     status: 500             # default 200
//!     headers: { X-Mock: "1" }
//!     body: '{"error":"boom"}'  # or body_file: github.json (inside mocks.yaml's directory)
//!     delay_ms: 2000          # at most 60000
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bytes::Bytes;
use globset::{GlobBuilder, GlobMatcher};
use http::{HeaderName, HeaderValue, Response, StatusCode};
use http_body_util::Full;
use rustman_core::LoadError;
use serde::Deserialize;

/// The mocks file, relative to the workspace root.
pub const MOCKS_FILE: &str = "webhook/mocks.yaml";

/// The longest allowed `delay_ms`.
pub const MAX_DELAY_MS: u64 = 60_000;

/// The largest `body_file`, the default body limit of the listener.
pub const MAX_BODY_FILE: u64 = 10 * 1024 * 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct MocksFile {
    #[serde(default)]
    mocks: Vec<MockSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MockSpec {
    #[serde(default = "any_method")]
    method: String,
    path: String,
    #[serde(default = "ok_status")]
    status: u16,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    body: Option<String>,
    body_file: Option<PathBuf>,
    #[serde(default)]
    delay_ms: u64,
}

fn any_method() -> String {
    "*".to_owned()
}

fn ok_status() -> u16 {
    200
}

/// One configured response.
#[derive(Clone, Debug)]
pub struct Mock {
    /// An upper-case method, or `*` for any.
    pub method: String,
    /// The path pattern as written.
    pub pattern: String,
    matcher: GlobMatcher,
    pub status: StatusCode,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub body: Bytes,
    pub delay: Duration,
}

impl Mock {
    /// Whether this mock answers `method` on `path` (without query).
    pub fn matches(&self, method: &str, path: &str) -> bool {
        (self.method == "*" || self.method == method) && self.matcher.is_match(path)
    }

    /// The configured response, with a correct `Content-Length`.
    pub fn response(&self) -> Response<Full<Bytes>> {
        let mut response = Response::new(Full::new(self.body.clone()));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        for (name, value) in &self.headers {
            headers.append(name.clone(), value.clone());
        }
        headers.insert(
            http::header::CONTENT_LENGTH,
            HeaderValue::from(self.body.len()),
        );
        response
    }
}

/// Parse the mocks in `text`, the contents of `file`. `body_file` paths are
/// read relative to the directory of `file`.
pub fn parse(file: &Path, text: &str) -> Result<Vec<Mock>, LoadError> {
    let parsed: MocksFile = rustman_core::format::from_yaml(file, text)?;
    let dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    parsed
        .mocks
        .into_iter()
        .enumerate()
        .map(|(i, spec)| {
            build(spec, dir).map_err(|e| LoadError::new(file, None, format!("mock {}: {e}", i + 1)))
        })
        .collect()
}

/// Read the mocks of `file`; a missing file has none.
pub fn load(file: &Path) -> Result<Vec<Mock>, LoadError> {
    match std::fs::read_to_string(file) {
        Ok(text) => parse(file, &text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(LoadError::new(file, None, e.to_string())),
    }
}

fn build(spec: MockSpec, dir: &Path) -> Result<Mock, String> {
    if spec.delay_ms > MAX_DELAY_MS {
        return Err(format!("delay_ms {} exceeds {MAX_DELAY_MS}", spec.delay_ms));
    }
    let status =
        StatusCode::from_u16(spec.status).map_err(|_| format!("invalid status {}", spec.status))?;
    let matcher = GlobBuilder::new(&spec.path)
        .literal_separator(true)
        .build()
        .map_err(|e| format!("invalid path pattern {:?}: {}", spec.path, e.kind()))?
        .compile_matcher();
    Ok(Mock {
        method: method(&spec.method)?,
        matcher,
        status,
        headers: headers(&spec.headers)?,
        body: body(spec.body, spec.body_file.as_deref(), dir)?,
        delay: Duration::from_millis(spec.delay_ms),
        pattern: spec.path,
    })
}

fn method(text: &str) -> Result<String, String> {
    if text == "*" {
        return Ok(text.to_owned());
    }
    http::Method::from_bytes(text.to_ascii_uppercase().as_bytes())
        .map(|m| m.as_str().to_owned())
        .map_err(|_| format!("invalid method {text:?}"))
}

/// Header names a mock may not set: hop-by-hop and framing headers, which
/// the listener computes itself (the set replay drops).
fn reserved(name: &HeaderName) -> bool {
    crate::replay::NOT_REPLAYED.contains(&name.as_str())
}

fn headers(map: &BTreeMap<String, String>) -> Result<Vec<(HeaderName, HeaderValue)>, String> {
    map.iter()
        .map(|(name, value)| {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| format!("invalid header name {name:?}"))?;
            if reserved(&name) {
                return Err(format!("header {name} is set by the listener"));
            }
            let value = HeaderValue::from_str(value)
                .map_err(|_| format!("invalid value for header {name}"))?;
            Ok((name, value))
        })
        .collect()
}

fn body(inline: Option<String>, file: Option<&Path>, dir: &Path) -> Result<Bytes, String> {
    match (inline, file) {
        (Some(_), Some(_)) => Err("set body or body_file, not both".to_owned()),
        (Some(text), None) => Ok(Bytes::from(text)),
        (None, Some(file)) => read_body_file(file, dir)
            .map_err(|e| format!("cannot read body_file {}: {e}", file.display())),
        (None, None) => Ok(Bytes::new()),
    }
}

/// Read `file`, which must stay inside `dir` (the directory of the mocks
/// file, symlinks resolved) and be at most [`MAX_BODY_FILE`] bytes.
fn read_body_file(file: &Path, dir: &Path) -> Result<Bytes, String> {
    use std::path::Component;
    if !file.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("must be a relative path inside the mocks directory".to_owned());
    }
    let root = dir.canonicalize().map_err(|e| e.to_string())?;
    let path = root.join(file).canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(&root) {
        return Err("must be a relative path inside the mocks directory".to_owned());
    }
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_BODY_FILE {
        return Err(format!("larger than {MAX_BODY_FILE} bytes"));
    }
    std::fs::read(&path)
        .map(Bytes::from)
        .map_err(|e| e.to_string())
}

/// The active mocks, shared by the listener and the front-ends; cloning
/// shares them. Replacing them takes effect on the next request.
#[derive(Clone, Debug, Default)]
pub struct Mocks {
    current: Arc<RwLock<Arc<[Arc<Mock>]>>>,
    file: Option<PathBuf>,
}

impl Mocks {
    /// No mocks: every request gets the default acknowledgement.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The mocks of `file` (none when it does not exist yet), reloadable.
    pub fn from_file(file: &Path) -> Result<Self, LoadError> {
        let mocks = Self {
            current: Arc::default(),
            file: Some(file.to_path_buf()),
        };
        mocks.replace(load(file)?);
        Ok(mocks)
    }

    /// The file these mocks are read from, if any.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Make `mocks` the active list.
    pub fn replace(&self, mocks: Vec<Mock>) {
        let list: Arc<[Arc<Mock>]> = mocks.into_iter().map(Arc::new).collect();
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = list;
    }

    /// Read the file again. On error the previous mocks stay active.
    pub fn reload(&self) -> Result<usize, LoadError> {
        let Some(file) = &self.file else {
            return Ok(self.len());
        };
        let mocks = load(file)?;
        let count = mocks.len();
        self.replace(mocks);
        Ok(count)
    }

    fn list(&self) -> Arc<[Arc<Mock>]> {
        Arc::clone(
            &self
                .current
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// The first mock that answers `method` on `path`.
    pub fn find(&self, method: &str, path: &str) -> Option<Arc<Mock>> {
        self.list()
            .iter()
            .find(|m| m.matches(method, path))
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.list().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mocks(yaml: &str) -> Result<Vec<Mock>, String> {
        parse(Path::new("webhook/mocks.yaml"), yaml).map_err(|e| e.to_string())
    }

    fn set(yaml: &str) -> Mocks {
        let set = Mocks::empty();
        set.replace(mocks(yaml).unwrap());
        set
    }

    #[test]
    fn first_match_wins_by_method_and_glob() {
        let set = set("mocks:\n\
             - {method: post, path: /hooks/github, status: 500}\n\
             - {path: '/hooks/*', status: 202}\n\
             - {method: GET, path: '/deep/**', status: 204}\n");
        let status = |m: &str, p: &str| set.find(m, p).map(|mock| mock.status.as_u16());
        assert_eq!(status("POST", "/hooks/github"), Some(500));
        assert_eq!(status("PUT", "/hooks/github"), Some(202));
        assert_eq!(
            status("POST", "/hooks/a/b"),
            None,
            "* stays within a segment"
        );
        assert_eq!(status("GET", "/deep/a/b/c"), Some(204));
        assert_eq!(status("POST", "/deep/a"), None);
    }

    #[test]
    fn response_carries_status_headers_body_and_length() {
        let set = set(
            "mocks:\n- path: /x\n  status: 201\n  headers: {X-Mock: yes}\n  body: hello\n  delay_ms: 5\n",
        );
        let mock = set.find("POST", "/x").unwrap();
        assert_eq!(mock.delay, Duration::from_millis(5));
        let response = mock.response();
        assert_eq!(response.status(), 201);
        assert_eq!(response.headers()["x-mock"], "yes");
        assert_eq!(response.headers()["content-length"], "5");
    }

    #[test]
    fn invalid_mocks_are_reported() {
        let err = |yaml: &str| mocks(yaml).unwrap_err();
        assert!(
            err("mocks:\n- {path: /x, delay_ms: 60001}\n")
                .contains("mock 1: delay_ms 60001 exceeds 60000")
        );
        assert!(err("mocks:\n- {path: /x, status: 42}\n").contains("invalid status 42"));
        assert!(err("mocks:\n- {path: /x, body: a, body_file: b}\n").contains("not both"));
        assert!(err("mocks:\n- {path: /x, colour: red}\n").contains("colour"));
        assert!(err("mocks:\n- {path: '/x[', status: 200}\n").contains("invalid path pattern"));
        assert!(mocks("mocks:\n- {path: /x, delay_ms: 60000}\n").is_ok());
    }

    #[test]
    fn body_file_is_read_next_to_the_mocks_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.bin"), [0xFF, 0x00]).unwrap();
        let file = dir.path().join("mocks.yaml");
        std::fs::write(&file, "mocks:\n- {path: /b, body_file: b.bin}\n").unwrap();
        let set = Mocks::from_file(&file).unwrap();
        assert_eq!(set.find("GET", "/b").unwrap().body.as_ref(), [0xFF, 0x00]);
    }

    #[test]
    fn body_file_must_stay_inside_the_mocks_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("outside.txt"), "secret").unwrap();
        let dir = root.path().join("webhook");
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("mocks.yaml");
        let outside = root.path().join("outside.txt");
        for target in ["../outside.txt", outside.to_str().unwrap()] {
            let yaml = format!("mocks:\n- {{path: /b, body_file: '{target}'}}\n");
            let err = parse(&file, &yaml).unwrap_err().to_string();
            assert!(
                err.contains("inside the mocks directory"),
                "{target}: {err}"
            );
        }
    }

    #[test]
    fn body_file_over_the_cap_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let big = std::fs::File::create(dir.path().join("big.bin")).unwrap();
        big.set_len(MAX_BODY_FILE + 1).unwrap();
        let file = dir.path().join("mocks.yaml");
        let err = parse(&file, "mocks:\n- {path: /b, body_file: big.bin}\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("larger than 10485760 bytes"), "{err}");
    }

    #[test]
    fn mocks_cannot_set_framing_or_hop_by_hop_headers() {
        for name in [
            "Transfer-Encoding",
            "content-length",
            "Connection",
            "Upgrade",
        ] {
            let yaml = format!("mocks:\n- {{path: /te, headers: {{{name}: x}}, body: hello}}\n");
            let err = mocks(&yaml).unwrap_err();
            assert!(err.contains("is set by the listener"), "{name}: {err}");
        }
        assert!(mocks("mocks:\n- {path: /ok, headers: {X-Mock: x}}\n").is_ok());
    }

    #[test]
    fn reload_keeps_previous_mocks_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mocks.yaml");
        assert!(
            Mocks::from_file(&file).unwrap().is_empty(),
            "missing file: no mocks"
        );
        std::fs::write(&file, "mocks:\n- {path: /a, status: 500}\n").unwrap();
        let set = Mocks::from_file(&file).unwrap();
        std::fs::write(&file, "mocks: [").unwrap();
        assert!(set.reload().is_err());
        assert_eq!(set.find("POST", "/a").unwrap().status, 500);
        std::fs::write(&file, "mocks:\n- {path: /a, status: 503}\n").unwrap();
        assert_eq!(set.reload().unwrap(), 1);
        assert_eq!(set.find("POST", "/a").unwrap().status, 503);
    }
}
