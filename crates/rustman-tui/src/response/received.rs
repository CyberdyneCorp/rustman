//! A finished exchange as the response viewer shows it, prepared off the
//! render loop: the redacted record, the body (redacted, parsed into a JSON
//! tree, highlighted, or kept on disk when large), captured values and the
//! latencies of earlier exchanges (spec: Response viewer tabs, JSON tree
//! viewer, Large response handling, Latency sparkline from history).
//!
//! Everything here is already redacted: the body is copied through the
//! exchange's redactor before it is kept, in memory or in a spool file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use rustman_core::run::{RequestRecord, RequestStatus, SkipReason};
use tempfile::NamedTempFile;

use super::json_tree::JsonTree;
use super::text::{self, StyledLine, TextLines};

/// Above this size the tree view is off and the body is paged from disk
/// (spec: Large response handling).
pub const TREE_LIMIT: u64 = 50 * 1024 * 1024;

/// How many bytes one page of the paged raw view holds.
pub const PAGE_BYTES: u64 = 64 * 1024;

/// Lines longer than this are cut into pieces in the paged view.
const PAGE_LINE: usize = 256;

/// Where the redacted body lives.
#[derive(Debug, Clone)]
pub enum Source {
    Memory(Arc<[u8]>),
    /// Spooled to a temp file (deleted when the last copy is dropped).
    Spool(Arc<NamedTempFile>),
}

impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Memory(a), Self::Memory(b)) => Arc::ptr_eq(a, b),
            (Self::Spool(a), Self::Spool(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for Source {}

impl Source {
    /// Read the whole body.
    pub fn read_all(&self) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Memory(bytes) => Ok(bytes.to_vec()),
            Self::Spool(file) => std::fs::read(file.path()),
        }
    }

    /// Read `len` bytes from `offset` (fewer at the end).
    pub fn read_at(&self, offset: u64, len: u64) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Memory(bytes) => {
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let end = start.saturating_add(usize::try_from(len).unwrap_or(usize::MAX));
                Ok(bytes[start..end.min(bytes.len())].to_vec())
            }
            Self::Spool(file) => {
                let mut f = File::open(file.path())?;
                f.seek(SeekFrom::Start(offset))?;
                let mut out = Vec::new();
                f.take(len).read_to_end(&mut out)?;
                Ok(out)
            }
        }
    }
}

/// How the body is shown.
#[derive(Debug, Clone)]
pub enum BodyKind {
    Empty,
    Json {
        tree: Arc<JsonTree>,
        /// The pretty-printed text for the raw toggle.
        pretty: Arc<TextLines>,
    },
    Text {
        lines: Arc<TextLines>,
        styled: Option<Arc<Vec<StyledLine>>>,
    },
    /// The first 4 KiB, for the hex view.
    Binary {
        head: Arc<[u8]>,
    },
    /// Above [`TREE_LIMIT`]: pages read from the source on demand.
    Paged,
}

/// A captured variable and its new value (`None` when secret: masked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub var: String,
    pub value: Option<String>,
    pub secret: bool,
    /// Why nothing was captured.
    pub message: Option<String>,
}

/// One finished exchange.
#[derive(Debug, Clone)]
pub struct Received {
    pub record: RequestRecord,
    pub content_type: Option<String>,
    pub size: u64,
    pub source: Option<Source>,
    pub body: BodyKind,
    pub captured: Vec<Captured>,
    /// Total times of the latest exchanges of the same request file, oldest
    /// first, this one last (spec: Latency sparkline from history).
    pub history: Vec<f64>,
    /// The GraphQL result, for a GraphQL exchange.
    pub graphql: Option<Box<super::graphql::GraphqlView>>,
    /// The failed, missing and new snapshots, for the Snapshot tab.
    pub snapshots: crate::review::Reviews,
}

/// The redacted body as received.
pub struct RawBody {
    pub source: Source,
    pub size: u64,
    /// The engine found the body not to be UTF-8.
    pub binary: bool,
    pub content_type: Option<String>,
}

impl Received {
    /// An exchange with no body to show: an error, a cancel, or a skip.
    pub fn without_body(record: RequestRecord) -> Self {
        Self {
            record,
            content_type: None,
            size: 0,
            source: None,
            body: BodyKind::Empty,
            captured: Vec::new(),
            history: Vec::new(),
            graphql: None,
            snapshots: crate::review::Reviews::default(),
        }
    }

    /// The exchange with `raw` prepared for viewing. Parsing and
    /// highlighting happen here, so call it off the render loop.
    pub fn with_body(record: RequestRecord, raw: RawBody) -> Self {
        let body = classify(&raw);
        Self {
            record,
            content_type: raw.content_type,
            size: raw.size,
            source: Some(raw.source),
            body,
            captured: Vec::new(),
            history: Vec::new(),
            graphql: None,
            snapshots: crate::review::Reviews::default(),
        }
    }

    /// A send cancelled after `elapsed_ms` (spec: Cancel in-flight request).
    pub fn cancelled(mut record: RequestRecord, elapsed_ms: f64) -> Self {
        record = record.skipped(SkipReason::Interrupted);
        record.duration_ms = elapsed_ms;
        Self::without_body(record)
    }

    pub fn is_cancelled(&self) -> bool {
        self.record.status == RequestStatus::Skipped
            && self.record.skip_reason == Some(SkipReason::Interrupted)
    }
}

/// Whether the content type says the body is not text.
fn binary_type(content_type: Option<&str>) -> bool {
    let Some(ct) = content_type else { return false };
    let ct = ct.trim().to_ascii_lowercase();
    ["image/", "audio/", "video/", "font/"]
        .iter()
        .any(|p| ct.starts_with(p))
        || [
            "application/octet-stream",
            "application/pdf",
            "application/zip",
        ]
        .iter()
        .any(|p| ct.starts_with(p))
}

fn json_type(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|ct| {
        let mime = ct.split(';').next().unwrap_or_default().trim();
        mime.ends_with("/json") || mime.ends_with("+json")
    })
}

fn classify(raw: &RawBody) -> BodyKind {
    if raw.size == 0 {
        return BodyKind::Empty;
    }
    if raw.binary || binary_type(raw.content_type.as_deref()) {
        let head = raw
            .source
            .read_at(0, text::HEX_LIMIT as u64)
            .unwrap_or_default();
        return BodyKind::Binary { head: head.into() };
    }
    if raw.size > TREE_LIMIT {
        return BodyKind::Paged;
    }
    let Ok(bytes) = raw.source.read_all() else {
        return BodyKind::Paged;
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    drop(bytes);
    let looks_json = text.trim_start().starts_with(['{', '[']);
    let parsed = (json_type(raw.content_type.as_deref()) || looks_json)
        .then(|| serde_json::from_str::<serde_json::Value>(&text).ok())
        .flatten();
    if let Some(value) = parsed {
        let pretty = serde_json::to_string_pretty(&value).unwrap_or(text);
        return BodyKind::Json {
            tree: Arc::new(JsonTree::from_value(&value)),
            pretty: Arc::new(TextLines::new(pretty)),
        };
    }
    let styled = raw
        .content_type
        .as_deref()
        .and_then(text::language)
        .and_then(|language| text::highlight(&text, language))
        .map(Arc::new);
    BodyKind::Text {
        lines: Arc::new(TextLines::new(text)),
        styled,
    }
}

/// The lines of page `page` of the paged raw view. A page starts after the
/// first line break in it (except the first page) and runs to the end of
/// its last line, so lines are not split between pages.
pub fn read_page(source: &Source, page: u64) -> std::io::Result<Vec<String>> {
    let start = page * PAGE_BYTES;
    let bytes = source.read_at(start, PAGE_BYTES + 4096)?;
    let body = &bytes[..bytes
        .len()
        .min(usize::try_from(PAGE_BYTES).unwrap_or(usize::MAX))];
    let skip = if page == 0 {
        0
    } else {
        body.iter()
            .position(|&b| b == b'\n')
            .map_or(body.len(), |i| i + 1)
    };
    let tail = bytes[body.len()..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len() - body.len(), |i| i);
    let text = String::from_utf8_lossy(&bytes[skip..body.len() + tail]).into_owned();
    Ok(text
        .lines()
        .flat_map(|line| chunks(line, PAGE_LINE))
        .collect())
}

/// `line` cut into pieces of at most `width` characters.
fn chunks(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let chars: Vec<char> = line.chars().collect();
    chars
        .chunks(width)
        .map(|c| c.iter().collect::<String>())
        .collect()
}

/// How many pages a body of `size` bytes has.
pub fn page_count(size: u64) -> u64 {
    size.div_ceil(PAGE_BYTES).max(1)
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Exchanges for the viewer tests.

    use rustman_core::run::{
        ExchangeRecord, HeaderRecord, RequestRecord, RequestStatus, ResponseRecord, SentRequest,
        TimingsRecord,
    };

    use super::*;

    /// A record of a `status` response with `headers`, `body_bytes` long.
    pub fn record(status: u16, headers: &[(&str, &str)], body_bytes: u64) -> RequestRecord {
        let mut record = RequestRecord::new(
            "users/get-user".into(),
            "Get user".into(),
            "users/get-user.rman.yaml".into(),
        );
        record.status = RequestStatus::Passed;
        record.duration_ms = 84.0;
        record.exchange = Some(ExchangeRecord {
            request: SentRequest {
                method: "GET".into(),
                url: "http://localhost/users/42".into(),
                headers: Vec::new(),
                body: None,
            },
            response: ResponseRecord {
                status,
                http_version: "HTTP/1.1".into(),
                headers: headers
                    .iter()
                    .map(|(n, v)| HeaderRecord {
                        name: (*n).into(),
                        value: (*v).into(),
                    })
                    .collect(),
                body: None,
                body_bytes,
                body_truncated: false,
                binary: false,
            },
            timings: TimingsRecord {
                auth_ms: None,
                dns_ms: Some(2.0),
                connect_ms: Some(3.0),
                tls_ms: None,
                ttfb_ms: 70.0,
                download_ms: 9.0,
                total_ms: 84.0,
            },
            redirects: Vec::new(),
            insecure: false,
        });
        record
    }

    /// A received `status` response with `body` of `content_type`.
    pub fn received(status: u16, content_type: &str, body: &[u8]) -> Received {
        let mut record = record(status, &[("Content-Type", content_type)], body.len() as u64);
        if let (Some(exchange), Ok(text)) = (&mut record.exchange, std::str::from_utf8(body)) {
            exchange.response.body = Some(text.to_owned());
        }
        Received::with_body(
            record,
            RawBody {
                source: Source::Memory(body.into()),
                size: body.len() as u64,
                binary: std::str::from_utf8(body).is_err(),
                content_type: Some(content_type.into()),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::received;
    use super::*;
    use std::io::Write;

    #[test]
    fn json_bodies_become_a_tree_and_pretty_text() {
        let r = received(200, "application/json", br#"{"a":[1,2]}"#);
        let BodyKind::Json { tree, pretty } = &r.body else {
            panic!("expected JSON, got {:?}", r.body);
        };
        assert_eq!(tree.len(), 4);
        assert_eq!(pretty.line(1), "  \"a\": [");
        // JSON is recognised without the content type, and invalid JSON
        // stays text.
        let r = received(200, "text/plain", b"[1]");
        assert!(matches!(r.body, BodyKind::Json { .. }));
        let r = received(200, "application/json", b"{oops");
        assert!(matches!(r.body, BodyKind::Text { styled: None, .. }));
    }

    #[test]
    fn xml_is_highlighted_and_png_is_binary() {
        let r = received(200, "application/xml", b"<a>1</a>\n");
        assert!(matches!(
            r.body,
            BodyKind::Text {
                styled: Some(_),
                ..
            }
        ));
        let png = vec![0x89; 300 * 1024];
        let r = received(200, "image/png", &png);
        let BodyKind::Binary { head } = &r.body else {
            panic!("expected binary");
        };
        assert_eq!(head.len(), text::HEX_LIMIT);
        assert_eq!(r.size, 300 * 1024);
        // A text type that is not UTF-8 is binary too.
        let r = received(200, "text/plain", b"\xff\xfe");
        assert!(matches!(r.body, BodyKind::Binary { .. }));
        assert!(matches!(
            received(204, "text/plain", b"").body,
            BodyKind::Empty
        ));
    }

    #[test]
    fn pages_end_on_line_breaks_and_long_lines_are_cut() {
        let mut file = NamedTempFile::new().unwrap();
        let line = format!("{}\n", "x".repeat(99));
        for _ in 0..2000 {
            file.write_all(line.as_bytes()).unwrap();
        }
        let size = 2000 * 100;
        let source = Source::Spool(Arc::new(file));
        assert_eq!(page_count(size), 4);
        let first = read_page(&source, 0).unwrap();
        let second = read_page(&source, 1).unwrap();
        assert!(first.iter().all(|l| l.len() == 99));
        assert_eq!(first.len() + second.len() + 1, 2 * 656);
        assert_eq!(chunks(&"y".repeat(600), 256).len(), 3);
        assert_eq!(read_page(&source, 9).unwrap(), Vec::<String>::new());
    }
}
