//! The redirect policy: which responses are followed and what the next
//! request looks like.
//!
//! `301`/`302`/`303` turn any method but `GET`/`HEAD` into `GET` and drop the
//! body; `307`/`308` keep both. Credentials are dropped when a hop leaves
//! the origin (scheme, host or port), so they never follow a redirect to
//! another server or onto cleartext: `Authorization`, `Proxy-Authorization`,
//! an explicit `Cookie` (the jar adds its own cookies for each hop) and the
//! `api_key` header or query parameter.

use bytes::Bytes;
use http::header::{
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, LOCATION, PROXY_AUTHORIZATION,
    TRANSFER_ENCODING,
};
use http::{HeaderMap, HeaderName, Method, Request, StatusCode, Uri};
use http_body_util::Full;
use serde::Serialize;

use crate::error::RequestError;
use crate::wire;

/// One followed redirect: the response status and the URL that returned it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RedirectHop {
    pub status: u16,
    pub url: String,
}

/// The request's own credentials, besides the standard headers, that are
/// dropped when a redirect leaves the origin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Credentials {
    /// The `api_key` header.
    pub header: Option<HeaderName>,
    /// The `api_key` query parameter.
    pub query: Option<String>,
}

/// The request for the next hop when `status` and `headers` are a redirect
/// to follow, else `None`.
pub(crate) fn next_request(
    current: &Request<Full<Bytes>>,
    status: StatusCode,
    headers: &HeaderMap,
    credentials: &Credentials,
) -> Result<Option<Request<Full<Bytes>>>, RequestError> {
    if !is_redirect(status) {
        return Ok(None);
    }
    let Some(location) = headers.get(LOCATION) else {
        return Ok(None);
    };
    let uri = resolve(current.uri(), location.as_bytes())?;
    let mut next = wire::duplicate(current);
    *next.uri_mut() = uri;
    if rewrites_to_get(status, current.method()) {
        *next.method_mut() = Method::GET;
        *next.body_mut() = Full::new(Bytes::new());
        for name in [CONTENT_TYPE, CONTENT_LENGTH, TRANSFER_ENCODING] {
            next.headers_mut().remove(name);
        }
    }
    if !same_origin(current.uri(), next.uri()) {
        strip_credentials(&mut next, credentials)?;
    }
    Ok(Some(next))
}

fn strip_credentials(
    request: &mut Request<Full<Bytes>>,
    credentials: &Credentials,
) -> Result<(), RequestError> {
    let headers = request.headers_mut();
    for name in [AUTHORIZATION, PROXY_AUTHORIZATION, COOKIE] {
        headers.remove(name);
    }
    if let Some(name) = &credentials.header {
        headers.remove(name);
    }
    if let Some(name) = &credentials.query {
        *request.uri_mut() = without_query_param(request.uri(), name)?;
    }
    Ok(())
}

/// `uri` without the query parameter `name` (unchanged when it has none).
fn without_query_param(uri: &Uri, name: &str) -> Result<Uri, RequestError> {
    let invalid = |e: &dyn std::fmt::Display| RequestError::Invalid(format!("invalid URL: {e}"));
    let mut url = url::Url::parse(&uri.to_string()).map_err(|e| invalid(&e))?;
    if !url.query_pairs().any(|(key, _)| key == name) {
        return Ok(uri.clone());
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != name)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    url.as_str().parse().map_err(|e| invalid(&e))
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn rewrites_to_get(status: StatusCode, method: &Method) -> bool {
    matches!(status.as_u16(), 301..=303) && method != Method::GET && method != Method::HEAD
}

/// `location` resolved against `base`, without a fragment.
fn resolve(base: &Uri, location: &[u8]) -> Result<Uri, RequestError> {
    let location = String::from_utf8_lossy(location);
    let invalid = |e: &dyn std::fmt::Display| {
        RequestError::Invalid(format!("invalid redirect location '{location}': {e}"))
    };
    let base = url::Url::parse(&base.to_string()).map_err(|e| invalid(&e))?;
    let mut next = base.join(location.trim()).map_err(|e| invalid(&e))?;
    next.set_fragment(None);
    next.as_str().parse().map_err(|e| invalid(&e))
}

fn same_origin(a: &Uri, b: &Uri) -> bool {
    a.scheme() == b.scheme() && a.host() == b.host() && port(a) == port(b)
}

fn port(uri: &Uri) -> Option<u16> {
    uri.port_u16().or(match uri.scheme_str() {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, url: &str) -> Request<Full<Bytes>> {
        Request::builder()
            .method(method)
            .uri(url)
            .header(AUTHORIZATION, "Bearer t")
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from_static(b"{}")))
            .unwrap()
    }

    fn location(to: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(LOCATION, to.parse().unwrap());
        headers
    }

    fn follow(req: &Request<Full<Bytes>>, code: u16, to: &str) -> Request<Full<Bytes>> {
        follow_with(req, code, to, &Credentials::default())
    }

    fn follow_with(
        req: &Request<Full<Bytes>>,
        code: u16,
        to: &str,
        credentials: &Credentials,
    ) -> Request<Full<Bytes>> {
        let status = StatusCode::from_u16(code).unwrap();
        next_request(req, status, &location(to), credentials)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn locations_resolve_against_the_current_url() {
        let req = request("GET", "http://h:8080/a/b?x=1");
        assert_eq!(follow(&req, 302, "c").uri(), "http://h:8080/a/c");
        assert_eq!(follow(&req, 302, "/d#frag").uri(), "http://h:8080/d");
        assert_eq!(follow(&req, 302, "//o/e").uri(), "http://o/e");
        assert_eq!(follow(&req, 302, "https://o/f").uri(), "https://o/f");
    }

    #[test]
    fn see_other_turns_post_into_get_without_a_body() {
        for code in [301, 302, 303] {
            let next = follow(&request("POST", "http://h/a"), code, "/b");
            assert_eq!(next.method(), Method::GET, "{code}");
            assert!(next.headers().get(CONTENT_TYPE).is_none());
            assert_eq!(next.headers()[AUTHORIZATION], "Bearer t");
        }
        let head = follow(&request("HEAD", "http://h/a"), 303, "/b");
        assert_eq!(head.method(), Method::HEAD);
    }

    #[test]
    fn temporary_and_permanent_keep_method_and_body() {
        for code in [307, 308] {
            let next = follow(&request("PUT", "http://h/a"), code, "/b");
            assert_eq!(next.method(), Method::PUT);
            assert_eq!(next.headers()[CONTENT_TYPE], "application/json");
        }
    }

    #[test]
    fn authorization_is_dropped_when_the_origin_changes() {
        let req = request("GET", "https://api.example.com/x");
        let keep = follow(&req, 302, "https://api.example.com:443/y");
        assert!(keep.headers().contains_key(AUTHORIZATION));
        for to in [
            "https://cdn.other.com/x",
            "http://api.example.com/x",
            "https://api.example.com:8443/x",
        ] {
            assert!(
                !follow(&req, 302, to).headers().contains_key(AUTHORIZATION),
                "{to}"
            );
        }
    }

    #[test]
    fn every_credential_is_dropped_when_the_origin_changes() {
        let mut req = request("GET", "https://api.example.com/x");
        for (name, value) in [
            ("cookie", "s=1"),
            ("proxy-authorization", "Basic p"),
            ("x-api-key", "k"),
        ] {
            req.headers_mut().insert(name, value.parse().unwrap());
        }
        let credentials = Credentials {
            header: Some(HeaderName::from_static("x-api-key")),
            query: Some("key".into()),
        };
        let same = follow_with(&req, 302, "/y?key=k&a=1", &credentials);
        assert_eq!(same.headers().len(), req.headers().len());
        assert_eq!(same.uri(), "https://api.example.com/y?key=k&a=1");
        let other = follow_with(&req, 302, "https://cdn.other.com/y?key=k&a=1", &credentials);
        for name in [
            "authorization",
            "cookie",
            "proxy-authorization",
            "x-api-key",
        ] {
            assert!(!other.headers().contains_key(name), "{name}");
        }
        assert_eq!(other.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(other.uri(), "https://cdn.other.com/y?a=1");
        let only_key = follow_with(&req, 302, "https://cdn.other.com/z?key=k", &credentials);
        assert_eq!(only_key.uri(), "https://cdn.other.com/z");
    }

    #[test]
    fn other_statuses_or_a_missing_location_are_final() {
        let req = request("GET", "http://h/a");
        let none = Credentials::default();
        let next = next_request(&req, StatusCode::NOT_MODIFIED, &location("/b"), &none).unwrap();
        assert!(next.is_none());
        let next = next_request(&req, StatusCode::FOUND, &HeaderMap::new(), &none).unwrap();
        assert!(next.is_none());
    }

    #[test]
    fn an_unparsable_location_is_an_error() {
        let req = request("GET", "http://h/a");
        let none = Credentials::default();
        let err =
            next_request(&req, StatusCode::FOUND, &location("http://[bad"), &none).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("invalid redirect location 'http://[bad'"),
            "{err}"
        );
    }
}
