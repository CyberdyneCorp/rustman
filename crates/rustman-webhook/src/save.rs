//! Saving captures (spec: Save captures): as a `kind: http` request file
//! that targets a chosen URL, or as an inbound entry in `request-history`.
//!
//! Request files are meant for Git, so they get the headers
//! [`sanitize_for_replay`] keeps minus the always-sensitive ones
//! (`Authorization`, `Cookie`, ...), which are reported as withheld; a
//! known secret value anywhere in them refuses the save. History entries
//! are redacted like every other entry.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rustman_core::format::Located;
use rustman_core::format::write::request_to_yaml;
use rustman_core::history::{ENTRY_SCHEMA, Entry, HistoryError, HistoryStore, new_ulid};
use rustman_core::model::{
    Body, HttpRequest, JsonContent, Method, NameValue, REQUEST_VERSION, Request,
};
use rustman_core::redact::sensitive_header;
use rustman_core::run::{HeaderRecord, SentRequest};
use rustman_core::{ConfigError, Redactor};
use time::UtcOffset;
use time::format_description::well_known::Rfc3339;

use crate::capture::Capture;
use crate::replay::sanitize_for_replay;

/// What a history entry made from a capture has as its `request`.
pub const INBOUND_REQUEST: &str = "webhook";

/// Why a capture could not be saved.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("cannot save capture #{id}: {part} holds a secret value")]
    Secret { id: u64, part: String },
    #[error("cannot save capture #{id}: header '{name}' is not text")]
    BinaryHeader { id: u64, name: String },
    #[error("cannot save capture #{id}: invalid method: {message}")]
    Method { id: u64, message: String },
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    History(#[from] HistoryError),
}

/// A request file made from a capture.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestFile {
    /// The YAML text of the request file.
    pub yaml: String,
    /// A body that is not UTF-8, to be written next to the request file
    /// under the name its `body.path` uses.
    pub body_file: Option<(String, Vec<u8>)>,
    /// Captured headers left out because their values are credentials.
    pub withheld: Vec<String>,
}

/// `text` with every `{{` escaped, so a request file sends it literally.
fn literal(text: &str) -> String {
    text.replace("{{", "\\{{")
}

/// The headers a request file keeps, and the names of those withheld.
fn file_headers(capture: &Capture) -> Result<(Vec<NameValue>, Vec<String>), SaveError> {
    let mut kept = Vec::new();
    let mut withheld = Vec::new();
    for header in sanitize_for_replay(&capture.headers) {
        if sensitive_header(&header.name).is_some() {
            withheld.push(header.name);
            continue;
        }
        let value =
            String::from_utf8(header.value.to_vec()).map_err(|_| SaveError::BinaryHeader {
                id: capture.id,
                name: header.name.clone(),
            })?;
        kept.push(NameValue::new(header.name, literal(&value)));
    }
    Ok((kept, withheld))
}

fn is_json(capture: &Capture) -> bool {
    capture.content_type().is_some_and(|t| {
        let essence = t
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        essence == "application/json" || essence.ends_with("+json")
    })
}

/// The file's body, and the bytes of a sidecar body file named
/// `body_file_name` when the body is not UTF-8.
fn file_body(capture: &Capture, body_file_name: &str) -> (Option<Body>, Option<(String, Vec<u8>)>) {
    if capture.body.is_empty() {
        return (None, None);
    }
    match std::str::from_utf8(&capture.body) {
        Ok(text) if is_json(capture) => (Some(Body::Json(JsonContent::Text(literal(text)))), None),
        Ok(text) => (Some(Body::Text(literal(text))), None),
        Err(_) => {
            let path = Located::new(body_file_name.to_owned());
            let sidecar = (body_file_name.to_owned(), capture.body.to_vec());
            (Some(Body::File(path)), Some(sidecar))
        }
    }
}

/// The request file for `capture`, sending it to `url` (which may hold
/// `{{variables}}`). A body that is not UTF-8 goes to `body_file_name`,
/// relative to the request file. `redactor` knows the secrets that must
/// not be written (the verifier keys).
pub fn request_file(
    capture: &Capture,
    url: &str,
    name: &str,
    body_file_name: &str,
    redactor: &Redactor,
) -> Result<RequestFile, SaveError> {
    let method = Method::parse(&capture.method).map_err(|message| SaveError::Method {
        id: capture.id,
        message,
    })?;
    let (headers, withheld) = file_headers(capture)?;
    let (body, body_file) = file_body(capture, body_file_name);
    let request = HttpRequest {
        version: REQUEST_VERSION,
        kind: "http".to_owned(),
        name: name.to_owned(),
        description: Some(format!(
            "Saved from webhook capture #{} ({} {})",
            capture.id,
            capture.method,
            capture.target()
        )),
        tags: None,
        docs: None,
        method: Some(method),
        url: url.to_owned(),
        query: None,
        headers: (!headers.is_empty()).then_some(headers),
        body,
        auth: None,
        settings: None,
        variables: None,
        assertions: None,
        captures: None,
    };
    let yaml = request_to_yaml(&Request::Http(Box::new(request)));
    let leaks = |bytes: &[u8]| redactor.bytes(bytes).as_ref() != bytes;
    if leaks(yaml.as_bytes()) || body_file.as_ref().is_some_and(|(_, b)| leaks(b)) {
        return Err(SaveError::Secret {
            id: capture.id,
            part: "the request".to_owned(),
        });
    }
    Ok(RequestFile {
        yaml,
        body_file,
        withheld,
    })
}

/// The name of the sidecar body file of the request file `path`:
/// `stripe.rman.yaml` → `stripe.body.bin`.
pub fn body_file_name(path: &Path) -> String {
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = file
        .strip_suffix(".rman.yaml")
        .or_else(|| file.strip_suffix(".yaml"))
        .unwrap_or(&file);
    format!("{stem}.body.bin")
}

/// Write the request file for `capture` to `path` (a new file), plus its
/// sidecar body file when needed. Returns what was written.
pub fn save_request_file(
    capture: &Capture,
    url: &str,
    path: &Path,
    redactor: &Redactor,
) -> Result<SavedFile, SaveError> {
    let sidecar_name = body_file_name(path);
    let name = format!("{} {}", capture.method, capture.path);
    let file = request_file(capture, url, &name, &sidecar_name, redactor)?;
    let mut written = Vec::new();
    if let Some((sidecar, bytes)) = &file.body_file {
        let sidecar = path.with_file_name(sidecar);
        write_new(&sidecar, bytes)?;
        written.push(sidecar);
    }
    write_new(path, file.yaml.as_bytes())?;
    written.insert(0, path.to_path_buf());
    Ok(SavedFile {
        written,
        withheld: file.withheld,
    })
}

/// The outcome of [`save_request_file`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedFile {
    /// The request file first, then its body file if any.
    pub written: Vec<PathBuf>,
    pub withheld: Vec<String>,
}

/// Create `path` with `bytes`, refusing to replace an existing file.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io)?;
    file.write_all(bytes).map_err(io)
}

/// A history entry for `capture`: the request as received (redacted with
/// `redactor`, sensitive headers always), the status answered, and the
/// capture details under `record.inbound`. Inbound entries cannot be
/// replayed from history; replay the capture instead.
pub fn history_entry(capture: &Capture, redactor: &Redactor) -> Entry {
    let headers: Vec<HeaderRecord> = capture
        .headers
        .iter()
        .map(|h| HeaderRecord {
            name: h.name.clone(),
            value: redactor.header_value(&h.name, &h.value_text()),
        })
        .collect();
    let text = std::str::from_utf8(&capture.body).ok();
    let body_base64 = match text {
        Some(_) => None,
        None => Some(STANDARD.encode(redactor.bytes(&capture.body))),
    };
    let record = serde_json::json!({
        "inbound": {
            "capture_id": capture.id,
            "received_at": capture.timestamp(),
            "remote": capture.remote.to_string(),
            "http_version": capture.version_text(),
            "mock": capture.mock,
            "signature": capture.signature,
            "body_size": capture.body_size(),
            "body_base64": body_base64,
        }
    });
    Entry {
        schema: ENTRY_SCHEMA.to_owned(),
        id: new_ulid(),
        timestamp: utc_timestamp(capture),
        request: INBOUND_REQUEST.to_owned(),
        environment: None,
        sent: SentRequest {
            method: capture.method.clone(),
            url: redactor.text(&capture.target()).into_owned(),
            headers,
            body: text.map(|t| redactor.text(t).into_owned()),
        },
        status: Some(capture.status),
        total_ms: None,
        error: None,
        spill: None,
        record,
        replay: None,
    }
}

fn utc_timestamp(capture: &Capture) -> String {
    capture
        .received_at
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Record `capture` in `store`; returns the new entry's id.
pub fn save_to_history(
    capture: &Capture,
    store: &HistoryStore,
    redactor: &Redactor,
) -> Result<String, SaveError> {
    let entry = history_entry(capture, redactor);
    store.record(&entry)?;
    Ok(entry.id)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use rustman_core::Secret;
    use rustman_core::model::KindRegistry;

    use super::*;
    use crate::capture::HeaderEntry;
    use crate::capture::tests::capture;

    fn stripe_capture() -> Capture {
        let mut c = capture("/hooks/stripe", b"{\"id\":\"evt_1\",\"tpl\":\"{{x}}\"}");
        c.id = 4;
        c.query = Some("debug=1".into());
        c.headers.extend([
            HeaderEntry::new("Host", "127.0.0.1:9090"),
            HeaderEntry::new("Content-Length", "29"),
            HeaderEntry::new("Stripe-Signature", "t=1,v1=ab"),
            HeaderEntry::new("Authorization", "Bearer tok-12345678"),
            HeaderEntry::new("Connection", "close"),
        ]);
        c
    }

    #[test]
    fn a_capture_saves_as_a_loadable_http_request_file() {
        let file = request_file(
            &stripe_capture(),
            "{{api}}/hooks/stripe",
            "POST /hooks/stripe",
            "x.body.bin",
            &Redactor::new(),
        )
        .unwrap();
        assert_eq!(file.withheld, ["Authorization"]);
        assert!(file.body_file.is_none());
        let loaded = KindRegistry::default()
            .load(Path::new("stripe.rman.yaml"), &file.yaml)
            .unwrap();
        let Request::Http(http) = loaded else {
            panic!("expected kind: http");
        };
        assert_eq!(http.kind, "http");
        assert_eq!(http.effective_method(), "POST");
        assert_eq!(http.url, "{{api}}/hooks/stripe");
        let names: Vec<_> = http.headers.unwrap().into_iter().map(|h| h.name).collect();
        assert_eq!(names, ["Content-Type", "Stripe-Signature"]);
        assert_eq!(
            http.body,
            Some(Body::Json(JsonContent::Text(
                "{\"id\":\"evt_1\",\"tpl\":\"\\{{x}}\"}".into()
            )))
        );
        assert!(!file.yaml.contains("tok-12345678"));
    }

    #[test]
    fn binary_bodies_go_to_a_sidecar_file_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.rman.yaml");
        let mut c = capture("/b", b"\xff\xfe\x00");
        c.headers.clear();
        let saved = save_request_file(&c, "http://localhost/b", &path, &Redactor::new()).unwrap();
        let sidecar = dir.path().join("bin.body.bin");
        assert_eq!(saved.written, [path.clone(), sidecar.clone()]);
        assert_eq!(std::fs::read(&sidecar).unwrap(), b"\xff\xfe\x00");
        let yaml = std::fs::read_to_string(&path).unwrap();
        assert!(yaml.contains("bin.body.bin"), "{yaml}");
        let again = save_request_file(&c, "http://localhost/b", &path, &Redactor::new());
        assert!(
            matches!(again, Err(SaveError::Config(_))),
            "existing files are kept"
        );
    }

    #[test]
    fn a_known_secret_refuses_the_save() {
        let mut c = capture("/s", b"key=canary-whsec-5f1e2d");
        c.id = 9;
        let redactor = Redactor::new().with(&Secret::new("gh_hook", "canary-whsec-5f1e2d".into()));
        let err = request_file(&c, "http://x/", "n", "b", &redactor).unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot save capture #9: the request holds a secret value"
        );
    }

    #[test]
    fn history_entries_are_redacted_and_keep_binary_bodies() {
        let secret = Secret::new("gh_hook", "canary-whsec-5f1e2d".to_owned());
        let redactor = Redactor::new().with(&secret);
        let mut c = stripe_capture();
        c.body = Bytes::from_static(b"echo canary-whsec-5f1e2d");
        let entry = history_entry(&c, &redactor);
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("canary-whsec-5f1e2d"), "{json}");
        assert!(!json.contains("tok-12345678"), "{json}");
        assert_eq!(entry.request, INBOUND_REQUEST);
        assert_eq!(entry.status, Some(200));
        assert_eq!(entry.sent.url, "/hooks/stripe?debug=1");
        assert_eq!(entry.timestamp, "1970-01-01T00:00:00Z");

        let binary = history_entry(&capture("/b", b"\xff\x00"), &Redactor::new());
        assert_eq!(binary.sent.body, None);
        assert_eq!(binary.record["inbound"]["body_base64"], "/wA=");
    }

    #[test]
    fn history_entries_are_stored_and_listed() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::at(dir.path(), 10);
        let id = save_to_history(&stripe_capture(), &store, &Redactor::new()).unwrap();
        assert_eq!(store.get(&id).unwrap().request, INBOUND_REQUEST);
    }

    #[test]
    fn sidecar_names_follow_the_request_file() {
        assert_eq!(
            body_file_name(Path::new("a/stripe.rman.yaml")),
            "stripe.body.bin"
        );
        assert_eq!(body_file_name(Path::new("x.yaml")), "x.body.bin");
    }
}
