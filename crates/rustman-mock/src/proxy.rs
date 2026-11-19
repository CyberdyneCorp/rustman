//! Forwarding unmatched requests (spec: Proxy unmatched routes). The
//! request goes to the one configured upstream through `rustman-http`,
//! with the webhook inspector's replay hygiene ([`sanitize_for_replay`]);
//! redirects are not followed, so no other origin is ever contacted. The
//! upstream response comes back as received, without its hop-by-hop and
//! framing headers. `Proxy-Authorization` is addressed to the mock as the
//! proxy hop, so it is neither forwarded nor recorded.

use std::net::SocketAddr;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, Response, StatusCode, Version};
use http_body_util::Full;
use rustman_http::{Exchange, HttpEngine, SendOptions};
use rustman_webhook::replay::NOT_REPLAYED;
use rustman_webhook::{Capture, HeaderEntry, replay, sanitize_for_replay};
use time::OffsetDateTime;

use crate::problem::Problem;

/// Upstream bodies up to this size are returned whole.
const MAX_BODY: u64 = 64 * 1024 * 1024;

/// Why an upstream URL is refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "--proxy-unmatched {0:?}: expected an http(s) URL without a query, such as https://staging.example.com"
)]
pub struct InvalidUpstream(pub String);

/// The `--proxy-unmatched` upstream: its origin and an optional base path,
/// without a trailing `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upstream {
    base: String,
}

impl Upstream {
    pub fn parse(url: &str) -> Result<Self, InvalidUpstream> {
        let invalid = || InvalidUpstream(url.to_owned());
        let rest = url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"))
            .ok_or_else(invalid)?;
        let host = rest.split('/').next().unwrap_or_default();
        if host.is_empty() || host.contains('@') || url.contains(['?', '#']) {
            return Err(invalid());
        }
        Ok(Self {
            base: url.trim_end_matches('/').to_owned(),
        })
    }

    /// The upstream URL for a request target (`/legacy/report?x=1`).
    pub fn url(&self, path_and_query: &str) -> String {
        format!("{}{path_and_query}", self.base)
    }

    pub fn as_str(&self) -> &str {
        &self.base
    }
}

/// A credential for the proxy hop itself, never passed on.
const PROXY_AUTHORIZATION: &str = "proxy-authorization";

/// One request to forward.
pub struct Outgoing<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub version: Version,
    pub headers: &'a HeaderMap,
    pub body: Bytes,
}

impl Outgoing<'_> {
    /// The request as a webhook capture, so replay's hygiene applies;
    /// without `Proxy-Authorization`.
    pub fn capture(&self) -> Capture {
        Capture {
            id: 0,
            received_at: OffsetDateTime::now_utc(),
            remote: SocketAddr::from(([127, 0, 0, 1], 0)),
            version: self.version,
            method: self.method.to_owned(),
            path: self.path.to_owned(),
            query: self.query.map(str::to_owned),
            headers: self
                .headers
                .iter()
                .filter(|(n, _)| n.as_str() != PROXY_AUTHORIZATION)
                .map(|(n, v)| HeaderEntry::new(n.as_str(), Bytes::copy_from_slice(v.as_bytes())))
                .collect(),
            body: self.body.clone(),
            status: 0,
            mock: None,
            signature: None,
        }
    }
}

/// Forwards to one upstream.
pub struct Proxy {
    pub upstream: Upstream,
    engine: HttpEngine,
    options: SendOptions,
}

impl Proxy {
    pub fn new(upstream: Upstream) -> Self {
        let mut options = SendOptions {
            follow_redirects: false,
            ..SendOptions::default()
        };
        options.body.decompress = false;
        options.body.max_in_memory = MAX_BODY;
        Self {
            upstream,
            engine: HttpEngine::new(),
            options,
        }
    }

    /// Send `capture` upstream; the response to return, or a `502`.
    pub async fn forward(&self, capture: &Capture) -> Result<Response<Full<Bytes>>, Problem> {
        let url = self.upstream.url(&capture.target());
        match replay(&self.engine, capture, &url, &self.options).await {
            Ok(exchange) => Ok(response(&exchange)),
            Err(err) => Err(Problem::new(
                StatusCode::BAD_GATEWAY,
                "proxy-failed",
                "Upstream unreachable",
                format!("{} {url}: {err}", capture.method),
            )),
        }
    }
}

/// The headers `sanitize_for_replay` keeps of the request, for recording.
pub fn kept_headers(capture: &Capture) -> Vec<HeaderEntry> {
    sanitize_for_replay(&capture.headers)
}

/// The upstream response without hop-by-hop and framing headers.
fn response(exchange: &Exchange) -> Response<Full<Bytes>> {
    let mut reply = Response::new(Full::new(exchange.body.clone()));
    *reply.status_mut() = StatusCode::from_u16(exchange.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = reply.headers_mut();
    for (name, value) in &exchange.headers {
        if !is_hop(name) {
            headers.append(name.clone(), value.clone());
        }
    }
    reply
}

fn is_hop(name: &HeaderName) -> bool {
    NOT_REPLAYED.contains(&name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstreams_are_http_origins_with_an_optional_base() {
        let up = Upstream::parse("https://staging.example.com/").unwrap();
        assert_eq!(
            up.url("/legacy/report?x=1"),
            "https://staging.example.com/legacy/report?x=1"
        );
        let based = Upstream::parse("http://127.0.0.1:9000/api").unwrap();
        assert_eq!(based.url("/a"), "http://127.0.0.1:9000/api/a");
        for bad in [
            "ftp://x",
            "staging.example.com",
            "http://",
            "http://u:p@x",
            "http://x/?a=1",
        ] {
            assert!(Upstream::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_capture_keeps_the_request_as_received() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "mock".parse().unwrap());
        headers.insert("x-a", "1".parse().unwrap());
        let out = Outgoing {
            method: "GET",
            path: "/legacy/report",
            query: Some("x=1"),
            version: Version::HTTP_11,
            headers: &headers,
            body: Bytes::new(),
        };
        let capture = out.capture();
        assert_eq!(capture.target(), "/legacy/report?x=1");
        let kept: Vec<_> = kept_headers(&capture).into_iter().map(|h| h.name).collect();
        assert_eq!(kept, ["x-a"]);
    }

    #[test]
    fn proxy_authorization_is_neither_forwarded_nor_recorded() {
        let mut headers = HeaderMap::new();
        headers.insert("proxy-authorization", "Basic cHJveHk6cHc=".parse().unwrap());
        headers.insert("x-a", "1".parse().unwrap());
        let out = Outgoing {
            method: "GET",
            path: "/legacy/b",
            query: None,
            version: Version::HTTP_11,
            headers: &headers,
            body: Bytes::new(),
        };
        let capture = out.capture();
        let sent: Vec<_> = capture.headers.iter().map(|h| h.name.clone()).collect();
        assert_eq!(sent, ["x-a"]);
        let kept: Vec<_> = kept_headers(&capture).into_iter().map(|h| h.name).collect();
        assert_eq!(kept, ["x-a"]);
    }
}
