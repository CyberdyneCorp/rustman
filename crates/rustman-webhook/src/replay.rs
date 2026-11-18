//! Replay hygiene in one function (design D5, spec: Replay with header
//! hygiene) and replaying a capture through `rustman-http`.
//!
//! [`sanitize_for_replay`] is the only place that decides which captured
//! headers may be sent again; replay and "save as request file" both call
//! it, so a stale `Content-Length` or `Host` (Ghostman B1) cannot come back
//! through either path.

use std::io::Write as _;

use rustman_core::model::RequestSettings;
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedFile, ResolvedPair, ResolvedRequest, TlsFiles,
};
use rustman_http::{Exchange, HttpEngine, RequestError, SendOptions};
use tempfile::NamedTempFile;

use crate::capture::{Capture, HeaderEntry};

/// Hop-by-hop headers (RFC 9110 §7.6.1, plus the common
/// `Proxy-Connection`) and the framing headers the engine computes for the
/// new target, lowercase.
pub const NOT_REPLAYED: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

/// The names a `Connection` header lists, lowercase.
fn connection_options(headers: &[HeaderEntry]) -> Vec<String> {
    headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("connection"))
        .flat_map(|h| {
            h.value_text()
                .split(',')
                .map(|token| token.trim().to_ascii_lowercase())
                .filter(|token| !token.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `headers` without hop-by-hop headers, the headers `Connection` names,
/// `Host`, `Content-Length` and `Transfer-Encoding`, in the original order.
pub fn sanitize_for_replay(headers: &[HeaderEntry]) -> Vec<HeaderEntry> {
    let listed = connection_options(headers);
    headers
        .iter()
        .filter(|h| {
            let name = h.name.to_ascii_lowercase();
            !NOT_REPLAYED.contains(&name.as_str()) && !listed.contains(&name)
        })
        .cloned()
        .collect()
}

/// A capture turned into a request for `url`. A body that is not UTF-8 is
/// sent byte for byte from a private temporary file, which lives as long
/// as this value.
pub struct ReplayRequest {
    pub request: ResolvedRequest,
    _body_file: Option<NamedTempFile>,
}

impl ReplayRequest {
    /// The method, sanitized headers and body of `capture`, sent to `url`.
    /// Header values must be text to be sent again.
    pub fn new(capture: &Capture, url: &str) -> Result<Self, RequestError> {
        let headers = sanitize_for_replay(&capture.headers)
            .into_iter()
            .map(|h| pair(&h))
            .collect::<Result<Vec<_>, _>>()?;
        let (body, body_file) = body(capture)?;
        let request = ResolvedRequest {
            method: capture.method.clone(),
            url: Resolved::literal(url),
            query: Vec::new(),
            headers,
            body,
            auth: None,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets: Vec::new(),
        };
        Ok(Self {
            request,
            _body_file: body_file,
        })
    }
}

fn pair(header: &HeaderEntry) -> Result<ResolvedPair, RequestError> {
    let value = String::from_utf8(header.value.to_vec()).map_err(|_| {
        RequestError::Invalid(format!(
            "header '{}' is not text and cannot be replayed",
            header.name
        ))
    })?;
    Ok(ResolvedPair {
        name: Resolved::literal(header.name.as_str()),
        value: Resolved::literal(value),
    })
}

type Body = (ResolvedBody, Option<NamedTempFile>);

fn body(capture: &Capture) -> Result<Body, RequestError> {
    if capture.body.is_empty() {
        return Ok((ResolvedBody::None, None));
    }
    if let Ok(text) = std::str::from_utf8(&capture.body) {
        return Ok((ResolvedBody::Text(Resolved::literal(text)), None));
    }
    let io = |e: std::io::Error| RequestError::Io(format!("cannot stage the replay body: {e}"));
    let mut file = tempfile::Builder::new()
        .prefix("rustman-replay-")
        .tempfile()
        .map_err(io)?;
    file.write_all(&capture.body).map_err(io)?;
    file.flush().map_err(io)?;
    let staged = ResolvedFile {
        raw: "<captured body>".to_owned(),
        path: file.path().to_path_buf(),
    };
    Ok((ResolvedBody::File(staged), Some(file)))
}

/// Send `capture` again to `url` through `engine`: original method, body
/// bytes and the headers [`sanitize_for_replay`] keeps. `Host` and
/// `Content-Length` are computed for the new target and body, so a capture
/// whose body was edited replays with correct framing.
pub async fn replay(
    engine: &HttpEngine,
    capture: &Capture,
    url: &str,
    options: &SendOptions,
) -> Result<Exchange, RequestError> {
    let replay = ReplayRequest::new(capture, url)?;
    engine.send(&replay.request, options).await
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::capture::tests::capture;

    fn names(headers: &[HeaderEntry]) -> Vec<&str> {
        headers.iter().map(|h| h.name.as_str()).collect()
    }

    #[test]
    fn hop_by_hop_framing_and_connection_listed_headers_are_dropped() {
        let headers: Vec<_> = [
            ("Host", "127.0.0.1:9090"),
            ("Content-Length", "7"),
            ("Transfer-Encoding", "chunked"),
            ("Connection", "keep-alive, X-Trace-Hop"),
            ("connection", "Upgrade"),
            ("Keep-Alive", "timeout=5"),
            ("Proxy-Connection", "keep-alive"),
            ("TE", "trailers"),
            ("Trailer", "Expires"),
            ("Upgrade", "h2c"),
            ("X-Trace-Hop", "1"),
            ("X-GitHub-Event", "push"),
            ("Content-Type", "application/json"),
            ("X-A", "1"),
            ("X-A", "2"),
        ]
        .into_iter()
        .map(|(n, v)| HeaderEntry::new(n, v))
        .collect();
        let kept = sanitize_for_replay(&headers);
        assert_eq!(
            names(&kept),
            ["X-GitHub-Event", "Content-Type", "X-A", "X-A"]
        );
        assert_eq!(kept[3].value, Bytes::from("2"));
    }

    /// Ghostman B1: replaying an edited body used to resend the captured
    /// `Content-Length` and fail on framing.
    #[test]
    fn edited_body_replays_without_the_stale_content_length() {
        let mut c = capture("/hooks/x", b"{\"a\":1}");
        c.headers.push(HeaderEntry::new("Content-Length", "7"));
        c.headers.push(HeaderEntry::new("Host", "127.0.0.1:9090"));
        c.body = Bytes::from_static(b"{\"a\":12345}");
        let replay = ReplayRequest::new(&c, "http://example.test/hook").unwrap();
        let sent: Vec<_> = replay
            .request
            .headers
            .iter()
            .map(|p| p.name.text.as_str())
            .collect();
        assert_eq!(sent, ["Content-Type"]);
        assert!(matches!(&replay.request.body, ResolvedBody::Text(t) if t.text == "{\"a\":12345}"));
        assert_eq!(replay.request.method, "POST");
        assert_eq!(replay.request.url.text, "http://example.test/hook");
    }

    #[test]
    fn binary_bodies_are_staged_byte_for_byte_and_empty_bodies_are_none() {
        let c = capture("/b", b"\xff\xfe\x00");
        let replay = ReplayRequest::new(&c, "http://example.test/").unwrap();
        let ResolvedBody::File(file) = &replay.request.body else {
            panic!("expected a file body");
        };
        assert_eq!(std::fs::read(&file.path).unwrap(), b"\xff\xfe\x00");
        let path = file.path.clone();
        drop(replay);
        assert!(!path.exists(), "the staged body is removed");
        let empty = ReplayRequest::new(&capture("/e", b""), "http://example.test/").unwrap();
        assert!(matches!(empty.request.body, ResolvedBody::None));
    }

    #[test]
    fn non_text_header_values_are_refused() {
        let mut c = capture("/b", b"");
        c.headers
            .push(HeaderEntry::new("X-Bin", Bytes::from_static(b"\xff")));
        let err = ReplayRequest::new(&c, "http://example.test/")
            .err()
            .unwrap();
        assert_eq!(
            err.to_string(),
            RequestError::Invalid("header 'X-Bin' is not text and cannot be replayed".into())
                .to_string()
        );
    }
}
