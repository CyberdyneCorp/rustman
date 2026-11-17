//! What one sent request produced.

use std::fmt;
use std::path::PathBuf;

use bytes::Bytes;
use http::{HeaderMap, Version};
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
