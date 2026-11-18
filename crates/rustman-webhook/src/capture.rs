//! One captured request (spec: Capture any method and path, Binary-safe
//! bodies). Header values and the body are raw bytes; nothing is decoded,
//! replaced or dropped.

use std::net::SocketAddr;

use bytes::Bytes;
use http::Version;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::signature::Signature;

/// One header field as received: the name in the sender's case (HTTP/1) or
/// lowercase (HTTP/2), the value as raw bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderEntry {
    pub name: String,
    pub value: Bytes,
}

impl HeaderEntry {
    pub fn new(name: impl Into<String>, value: impl Into<Bytes>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    /// The value as text; bytes that are not UTF-8 are shown as U+FFFD.
    pub fn value_text(&self) -> String {
        String::from_utf8_lossy(&self.value).into_owned()
    }
}

/// A captured request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// Sequence number, assigned by the store (1, 2, ...).
    pub id: u64,
    pub received_at: OffsetDateTime,
    pub remote: SocketAddr,
    pub version: Version,
    pub method: String,
    /// The path without the query string.
    pub path: String,
    /// The query string without `?`, when the target had one.
    pub query: Option<String>,
    /// Every header field in arrival order, duplicates included.
    pub headers: Vec<HeaderEntry>,
    pub body: Bytes,
    /// The status the listener answered with.
    pub status: u16,
    /// The path pattern of the mock that answered, if one did.
    pub mock: Option<String>,
    /// The verdict of the verifier configured for the path, if one is.
    pub signature: Option<Signature>,
}

impl Capture {
    /// The receive time in RFC 3339 with the listener's offset and
    /// millisecond precision, e.g. `2026-11-18T10:15:30.125+01:00`.
    pub fn timestamp(&self) -> String {
        let millis = self.received_at.millisecond();
        self.received_at
            .replace_millisecond(millis)
            .unwrap_or(self.received_at)
            .format(&Rfc3339)
            .unwrap_or_default()
    }

    pub fn body_size(&self) -> usize {
        self.body.len()
    }

    /// The path and query as sent, e.g. `/hooks/stripe?debug=1`.
    pub fn target(&self) -> String {
        match &self.query {
            Some(query) => format!("{}?{query}", self.path),
            None => self.path.clone(),
        }
    }

    /// `HTTP/1.1`, `HTTP/2.0`, ...
    pub fn version_text(&self) -> String {
        format!("{:?}", self.version)
    }

    /// The values of the header `name` (any case), in arrival order.
    pub fn header_values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Bytes> + 'a {
        self.headers
            .iter()
            .filter(move |h| h.name.eq_ignore_ascii_case(name))
            .map(|h| &h.value)
    }

    /// The first `Content-Type` value, as text.
    pub fn content_type(&self) -> Option<String> {
        self.header_values("content-type")
            .next()
            .map(|v| String::from_utf8_lossy(v).into_owned())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn capture(path: &str, body: &'static [u8]) -> Capture {
        Capture {
            id: 0,
            received_at: OffsetDateTime::UNIX_EPOCH,
            remote: "127.0.0.1:5000".parse().unwrap(),
            version: Version::HTTP_11,
            method: "POST".into(),
            path: path.into(),
            query: None,
            headers: vec![HeaderEntry::new("Content-Type", "application/json")],
            body: Bytes::from_static(body),
            status: 200,
            mock: None,
            signature: None,
        }
    }

    #[test]
    fn timestamp_is_rfc3339_with_the_offset_and_milliseconds() {
        let mut c = capture("/a", b"");
        let offset = time::UtcOffset::from_hms(1, 0, 0).unwrap();
        c.received_at = (OffsetDateTime::UNIX_EPOCH + time::Duration::nanoseconds(125_456_789))
            .to_offset(offset);
        assert_eq!(c.timestamp(), "1970-01-01T01:00:00.125+01:00");
    }

    #[test]
    fn target_joins_path_and_query() {
        let mut c = capture("/hooks/stripe", b"");
        assert_eq!(c.target(), "/hooks/stripe");
        c.query = Some("debug=1".into());
        assert_eq!(c.target(), "/hooks/stripe?debug=1");
        assert_eq!(c.version_text(), "HTTP/1.1");
    }

    #[test]
    fn header_lookup_ignores_case_and_keeps_order() {
        let mut c = capture("/a", b"");
        c.headers.push(HeaderEntry::new("x-a", "1"));
        c.headers.push(HeaderEntry::new("X-A", "2"));
        let values: Vec<_> = c.header_values("X-a").cloned().collect();
        assert_eq!(values, [Bytes::from("1"), Bytes::from("2")]);
        assert_eq!(c.content_type().as_deref(), Some("application/json"));
    }
}
