//! A read-only view of one received request: what validation, security,
//! overrides, templates and the stateful store look at.

use bytes::Bytes;
use http::HeaderMap;
use http::header::{CONTENT_TYPE, COOKIE};
use percent_encoding::percent_decode_str;
use serde_json::Value;

/// One request, with its body already received.
#[derive(Debug)]
pub struct RequestView<'a> {
    /// Upper case.
    pub method: &'a str,
    /// The request path, without the query.
    pub path: &'a str,
    /// Decoded query pairs in request order.
    pub query: Vec<(String, String)>,
    pub headers: &'a HeaderMap,
    pub body: &'a Bytes,
    /// Path parameters of the matched template, by name.
    pub params: Vec<(String, String)>,
}

impl<'a> RequestView<'a> {
    /// A view of a request to `path` with the raw `query` string.
    pub fn new(
        method: &'a str,
        path: &'a str,
        query: Option<&str>,
        headers: &'a HeaderMap,
        body: &'a Bytes,
    ) -> Self {
        Self {
            method,
            path,
            query: query.map(parse_query).unwrap_or_default(),
            headers,
            body,
            params: Vec::new(),
        }
    }

    /// The first value of header `name` (any case) that is text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Every value of query parameter `name`.
    pub fn query_values(&self, name: &str) -> Vec<&str> {
        self.query
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The value of cookie `name`.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.headers
            .get_all(COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
    }

    /// The `Content-Type` header.
    pub fn content_type(&self) -> Option<&str> {
        self.header(CONTENT_TYPE.as_str())
    }

    /// The body parsed as JSON, when it is JSON.
    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(self.body).ok()
    }
}

/// `a=1&b=x%20y&c` as decoded pairs (`+` is a space).
pub fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect()
}

fn decode(text: &str) -> String {
    percent_decode_str(&text.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn query_pairs_are_decoded() {
        assert_eq!(
            parse_query("q=a+b&x=%2F&flag&&q=2"),
            vec![
                ("q".into(), "a b".into()),
                ("x".into(), "/".into()),
                ("flag".into(), String::new()),
                ("q".into(), "2".into()),
            ]
        );
    }

    #[test]
    fn cookies_and_headers_are_found() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static("a=1; session=xyz"));
        headers.insert("x-key", HeaderValue::from_static("k"));
        let body = Bytes::new();
        let view = RequestView::new("GET", "/", Some("q=1"), &headers, &body);
        assert_eq!(view.cookie("session"), Some("xyz"));
        assert_eq!(view.cookie("nope"), None);
        assert_eq!(view.header("X-Key"), Some("k"));
        assert_eq!(view.query_values("q"), vec!["1"]);
    }
}
