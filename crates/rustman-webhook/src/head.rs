//! The header fields of a request as the sender wrote them (design D2).
//!
//! hyper keeps the original header-name case private and merges different
//! names out of arrival order, so for HTTP/1 the head is parsed again from
//! the connection's raw bytes with `httparse`, the parser hyper itself uses.
//! hyper has already validated the head; this pass only reads it. It also
//! reveals every `Content-Length` that hyper drops when `Transfer-Encoding` is
//! present, which the listener rejects (spec: Standards-compliant HTTP
//! handling). HTTP/2 headers come from hyper's map (lowercase by protocol).

use bytes::Bytes;
use http::{HeaderMap, Version};

use crate::capture::HeaderEntry;

/// The connection's first bytes did not hold a complete HTTP/1 head.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UnreadableHead;

/// Header fields of a request: from `raw` (the connection's first bytes)
/// for HTTP/1, else from `map`. hyper has already refused heads with more
/// than `max_headers` fields, so that many slots always suffice; if the raw
/// head still cannot be read, the request fails closed rather than falling
/// back to hyper's map, which hides a `Content-Length` that came with
/// `Transfer-Encoding`.
pub(crate) fn headers(
    version: Version,
    map: &HeaderMap,
    raw: &[u8],
    max_headers: usize,
) -> Result<Vec<HeaderEntry>, UnreadableHead> {
    if matches!(version, Version::HTTP_10 | Version::HTTP_11) {
        parse_raw(raw, max_headers.saturating_add(1)).ok_or(UnreadableHead)
    } else {
        Ok(from_map(map))
    }
}

fn parse_raw(raw: &[u8], capacity: usize) -> Option<Vec<HeaderEntry>> {
    let mut storage = vec![httparse::EMPTY_HEADER; capacity];
    let mut request = httparse::Request::new(&mut storage);
    match request.parse(raw) {
        Ok(httparse::Status::Complete(_)) => Some(
            request
                .headers
                .iter()
                .map(|h| HeaderEntry::new(h.name, Bytes::copy_from_slice(h.value)))
                .collect(),
        ),
        _ => None,
    }
}

fn from_map(map: &HeaderMap) -> Vec<HeaderEntry> {
    map.iter()
        .map(|(name, value)| {
            HeaderEntry::new(name.as_str(), Bytes::copy_from_slice(value.as_bytes()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http1_headers_keep_case_order_and_duplicates() {
        let raw = b"POST /x HTTP/1.1\r\nX-A: 1\r\nHost: h\r\nx-a: 2\r\n\r\nbody";
        let mut map = HeaderMap::new();
        map.append("x-a", "1".parse().unwrap());
        map.append("x-a", "2".parse().unwrap());
        map.append("host", "h".parse().unwrap());
        let names: Vec<_> = headers(Version::HTTP_11, &map, raw, 100)
            .unwrap()
            .into_iter()
            .map(|h| format!("{}={}", h.name, h.value_text()))
            .collect();
        assert_eq!(names, ["X-A=1", "Host=h", "x-a=2"]);
    }

    #[test]
    fn http2_uses_the_map_and_unreadable_http1_heads_fail_closed() {
        let mut map = HeaderMap::new();
        map.append("x-a", "1".parse().unwrap());
        let raw = b"POST /x HTTP/1.1\r\nX-A: 1\r\n\r\n";
        assert_eq!(
            headers(Version::HTTP_2, &map, raw, 100).unwrap()[0].name,
            "x-a"
        );
        assert_eq!(
            headers(Version::HTTP_11, &map, b"garbage", 100),
            Err(UnreadableHead)
        );
    }

    #[test]
    fn fields_hyper_drops_from_its_map_are_still_read() {
        // hyper removes every Content-Length when Transfer-Encoding is present.
        let mut raw = String::from("POST /b HTTP/1.1\r\nTransfer-Encoding: chunked\r\n");
        raw.push_str(&"Content-Length: 3\r\n".repeat(12));
        raw.push_str("\r\n");
        let mut map = HeaderMap::new();
        map.append("transfer-encoding", "chunked".parse().unwrap());
        let fields = headers(Version::HTTP_11, &map, raw.as_bytes(), 100).unwrap();
        assert_eq!(fields.len(), 13);
        assert_eq!(fields[1].name, "Content-Length");
    }

    #[test]
    fn non_utf8_values_stay_raw() {
        let raw = b"POST / HTTP/1.1\r\nX-B: \xff\xfe\r\n\r\n";
        let entry = &headers(Version::HTTP_11, &HeaderMap::new(), raw, 100).unwrap()[0];
        assert_eq!(entry.value.as_ref(), [0xFF, 0xFE]);
    }
}
