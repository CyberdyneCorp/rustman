//! What one sent request produced.

use std::borrow::Cow;
use std::fmt;
use std::io;
use std::path::PathBuf;

use bytes::Bytes;
use http::{HeaderMap, Version};
use rustman_core::Redactor;
use rustman_core::assertions::ResponseView;
use rustman_core::vars::ResolvedRequest;
use serde::{Serialize, Serializer};

use crate::redirect::RedirectHop;
use crate::timing::Timings;

/// The HTTP version a response arrived over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpVersion {
    Http10,
    Http11,
    Http2,
}

impl HttpVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
            Self::Http2 => "HTTP/2",
        }
    }

    pub(crate) fn from_http(version: Version) -> Self {
        match version {
            Version::HTTP_2 => Self::Http2,
            Version::HTTP_10 | Version::HTTP_09 => Self::Http10,
            _ => Self::Http11,
        }
    }
}

impl fmt::Display for HttpVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for HttpVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// A response and how it was obtained. With redirects, the response and its
/// timings are the final hop's.
#[derive(Clone, Debug)]
pub struct Exchange {
    /// A unique id; a spilled body is saved as `<id>.bin`.
    pub id: String,
    /// The URL of the final hop.
    pub url: String,
    /// Every redirect followed, in order.
    pub redirects: Vec<RedirectHop>,
    pub http_version: HttpVersion,
    pub status: u16,
    pub headers: HeaderMap,
    /// The decoded body, or its first 64 KiB when it was spilled to disk.
    pub body: Bytes,
    /// The size of the whole decoded body.
    pub size_bytes: u64,
    /// The file holding the whole body, when it exceeded `max_body_bytes`.
    pub spill: Option<PathBuf>,
    /// The body is not valid UTF-8; it must never be decoded lossily.
    pub binary: bool,
    pub timings: Timings,
    /// A hop was sent over TLS with certificate verification off.
    pub insecure: bool,
    /// Non-fatal problems, such as a stale `Content-Length` that was replaced.
    pub warnings: Vec<String>,
    /// The credentials of the sensitive headers sent or received on any hop
    /// (jar cookies, proxy credentials, `Set-Cookie`), which the request
    /// itself does not name. Merge it into the request's redactor so an
    /// echoing server cannot leak them.
    pub credentials: Redactor,
}

impl Exchange {
    /// The redactor for everything recorded about this exchange of
    /// `request`: the request's own, plus the credentials sent or received
    /// on the wire.
    pub fn redactor(&self, request: &ResolvedRequest) -> Redactor {
        let mut redactor = Redactor::for_request(request);
        redactor.merge(&self.credentials);
        redactor
    }

    /// The response as assertions and captures read it. With `full_body`, a
    /// body spilled to disk is read back whole; otherwise the view holds
    /// the in-memory body (or its first 64 KiB), which is enough for
    /// assertions that do not read the body (see
    /// [`rustman_core::assertions::needs_body`]).
    pub fn view(&self, full_body: bool) -> io::Result<ResponseView<'_>> {
        let body = match &self.spill {
            Some(path) if full_body => Cow::Owned(std::fs::read(path)?),
            _ => Cow::Borrowed(self.body.as_ref()),
        };
        let headers = self
            .headers
            .iter()
            .map(|(name, value)| {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                (name.as_str().to_owned(), value)
            })
            .collect();
        Ok(ResponseView {
            status: self.status,
            headers,
            body,
            binary: self.binary,
            ttfb: self.timings.ttfb,
            total: self.timings.total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_print_as_in_reports() {
        assert_eq!(
            HttpVersion::from_http(Version::HTTP_2).to_string(),
            "HTTP/2"
        );
        assert_eq!(
            HttpVersion::from_http(Version::HTTP_11).to_string(),
            "HTTP/1.1"
        );
        assert_eq!(
            HttpVersion::from_http(Version::HTTP_10).to_string(),
            "HTTP/1.0"
        );
        assert_eq!(
            serde_json::to_value(HttpVersion::Http2).unwrap(),
            serde_json::json!("HTTP/2")
        );
    }
}
