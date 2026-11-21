//! HAR 1.2 export of captures (traffic-recorder spec: HAR export): every
//! capture becomes one entry. Secrets are redacted with the capture's
//! [`Capture::redactor`] (sensitive headers and the values of secret-named
//! headers, query parameters and body fields), unless the caller asks to
//! show them. Tunnels and rejected handshakes are `CONNECT` entries
//! without a response; they, and truncated bodies, say so in `comment`.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustman_core::Redactor;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;

use crate::capture::{BodyCapture, Capture, Exchange, Header, Recorded, TLS_REJECTED_HINT};
use crate::named::query_pairs;

/// The HAR format version written.
pub const HAR_VERSION: &str = "1.2";

/// All `captures` as one HAR document; redacted unless `show_secrets`.
pub fn har<C: AsRef<Capture>>(captures: &[C], show_secrets: bool) -> Value {
    let entries: Vec<Value> = captures
        .iter()
        .map(|c| entry(c.as_ref(), show_secrets))
        .collect();
    json!({
        "log": {
            "version": HAR_VERSION,
            "creator": { "name": "rustman", "version": env!("CARGO_PKG_VERSION") },
            "entries": entries,
        }
    })
}

/// Write [`har`] of `captures` to `path`, pretty-printed, readable by the
/// current user only (with `show_secrets` it holds raw credentials).
pub fn write_har<C: AsRef<Capture>>(
    path: &Path,
    captures: &[C],
    show_secrets: bool,
) -> io::Result<()> {
    let mut text = serde_json::to_string_pretty(&har(captures, show_secrets))?;
    text.push('\n');
    let path = match path.parent() {
        Some(dir) if dir.as_os_str().is_empty() => Path::new(".").join(path),
        _ => path.to_path_buf(),
    };
    rustman_core::secrets::write_private(&path, text.as_bytes())
}

fn entry(capture: &Capture, show_secrets: bool) -> Value {
    let started = capture.started;
    match &capture.recorded {
        Recorded::Exchange(exchange) => {
            let shown = Shown::new(capture, show_secrets);
            exchange_entry(started, exchange, &shown)
        }
        Recorded::Tunnel(t) => {
            let mut comment = format!(
                "tunnel: not decrypted; {} B up, {} B down",
                t.bytes_up, t.bytes_down
            );
            if let Some(error) = &t.error {
                comment.push_str(&format!("; {error}"));
            }
            connect_entry(started, &t.host, t.port, t.duration, comment)
        }
        Recorded::TlsRejected { host, port } => {
            let comment = format!("TLS REJECTED: {TLS_REJECTED_HINT}");
            connect_entry(started, host, *port, Duration::ZERO, comment)
        }
    }
}

/// How values are shown: redacted, or as they are.
struct Shown {
    redactor: Option<Redactor>,
}

impl Shown {
    fn new(capture: &Capture, show_secrets: bool) -> Self {
        if show_secrets {
            return Self { redactor: None };
        }
        Self {
            redactor: Some(capture.redactor()),
        }
    }

    fn header(&self, name: &str, value: &str) -> String {
        match &self.redactor {
            Some(r) => r.header_value(name, value),
            None => value.to_owned(),
        }
    }

    fn text(&self, text: &str) -> String {
        match &self.redactor {
            Some(r) => r.text(text).into_owned(),
            None => text.to_owned(),
        }
    }

    fn bytes(&self, bytes: &[u8]) -> Vec<u8> {
        match &self.redactor {
            Some(r) => r.bytes(bytes).into_owned(),
            None => bytes.to_vec(),
        }
    }
}

fn exchange_entry(started: SystemTime, e: &Exchange, shown: &Shown) -> Value {
    let wait = millis(e.duration);
    let mut entry = json!({
        "startedDateTime": iso8601(started),
        "time": wait,
        "request": request(e, shown),
        "response": response(e, shown),
        "cache": {},
        "timings": { "send": 0, "wait": wait, "receive": 0 },
    });
    let mut comments = Vec::new();
    if e.intercepted {
        comments.push("decrypted by the Rustman local CA".to_owned());
    }
    comments.extend(truncation("request", &e.request_body));
    comments.extend(truncation("response", &e.response_body));
    if let Some(error) = &e.error {
        comments.push(format!("error: {}", shown.text(error)));
    }
    if !comments.is_empty() {
        entry["comment"] = json!(comments.join("; "));
    }
    entry
}

fn truncation(which: &str, body: &BodyCapture) -> Option<String> {
    body.truncated.then(|| {
        format!(
            "truncated: {which} body stored {} of {} bytes",
            body.bytes.len(),
            body.total
        )
    })
}

fn request(e: &Exchange, shown: &Shown) -> Value {
    let query: Vec<Value> = query_pairs(&e.url)
        .into_iter()
        .map(|(name, value)| json!({ "name": name, "value": shown.text(&value) }))
        .collect();
    let mut request = json!({
        "method": e.method,
        "url": uri_safe(&shown.text(&e.url)),
        "httpVersion": e.version,
        "cookies": [],
        "headers": headers(&e.request_headers, shown),
        "queryString": query,
        "headersSize": -1,
        "bodySize": e.request_body.total,
    });
    if e.request_body.total > 0 {
        let mime = content_type(&e.request_headers);
        let (text, encoding) = body_text(&e.request_body, shown);
        let mut post = json!({ "mimeType": mime, "text": text });
        if encoding.is_some() {
            post["comment"] = json!("text is base64-encoded");
        }
        request["postData"] = post;
    }
    request
}

fn response(e: &Exchange, shown: &Shown) -> Value {
    let status = e.status.unwrap_or(0);
    let status_text = http::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or_default();
    let (text, encoding) = body_text(&e.response_body, shown);
    let mut content = Map::new();
    content.insert("size".into(), json!(e.response_body.total));
    content.insert("mimeType".into(), json!(content_type(&e.response_headers)));
    content.insert("text".into(), json!(text));
    if let Some(encoding) = encoding {
        content.insert("encoding".into(), json!(encoding));
    }
    json!({
        "status": status,
        "statusText": status_text,
        "httpVersion": e.version,
        "cookies": [],
        "headers": headers(&e.response_headers, shown),
        "content": content,
        "redirectURL": header(&e.response_headers, "location").unwrap_or_default(),
        "headersSize": -1,
        "bodySize": e.response_body.total,
    })
}

/// `url` with non-ASCII and control characters percent-encoded, so a
/// redaction placeholder (`«redacted:…»`) keeps it a valid URI.
fn uri_safe(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        if c.is_ascii_graphic() {
            out.push(c);
            continue;
        }
        let mut buf = [0u8; 4];
        for byte in c.encode_utf8(&mut buf).bytes() {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The stored body as text, or base64 with its encoding when not UTF-8.
fn body_text(body: &BodyCapture, shown: &Shown) -> (String, Option<&'static str>) {
    let bytes = shown.bytes(&body.bytes);
    match String::from_utf8(bytes) {
        Ok(text) => (text, None),
        Err(err) => (STANDARD.encode(err.into_bytes()), Some("base64")),
    }
}

fn headers(headers: &[Header], shown: &Shown) -> Vec<Value> {
    headers
        .iter()
        .map(|(name, value)| json!({ "name": name, "value": shown.header(name, value) }))
        .collect()
}

fn header<'a>(headers: &'a [Header], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn content_type(headers: &[Header]) -> &str {
    header(headers, "content-type").unwrap_or_default()
}

/// A `CONNECT` entry for a tunnel or a rejected handshake: no response.
fn connect_entry(
    started: SystemTime,
    host: &str,
    port: u16,
    duration: Duration,
    comment: String,
) -> Value {
    let time = millis(duration);
    json!({
        "startedDateTime": iso8601(started),
        "time": time,
        "request": {
            "method": "CONNECT",
            "url": format!("https://{host}:{port}/"),
            "httpVersion": "HTTP/1.1",
            "cookies": [],
            "headers": [],
            "queryString": [],
            "headersSize": -1,
            "bodySize": 0,
        },
        "response": {
            "status": 0,
            "statusText": "",
            "httpVersion": "HTTP/1.1",
            "cookies": [],
            "headers": [],
            "content": { "size": 0, "mimeType": "" },
            "redirectURL": "",
            "headersSize": -1,
            "bodySize": -1,
        },
        "cache": {},
        "timings": { "send": 0, "wait": time, "receive": 0 },
        "comment": comment,
    })
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// `2023-11-14T22:13:20.123Z`.
fn iso8601(at: SystemTime) -> String {
    let t = OffsetDateTime::from(at);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

#[cfg(test)]
mod tests;
