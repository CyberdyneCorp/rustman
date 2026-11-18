//! The redirect policy: which responses are followed and what the next
//! request looks like.
//!
//! `301`/`302`/`303` turn any method but `GET`/`HEAD` into `GET` and drop the
//! body; `307`/`308` keep both. Credentials are dropped when a hop leaves
//! the origin (scheme, host or port), so they never follow a redirect to
//! another server or onto cleartext: `Authorization`, `Proxy-Authorization`,
//! an explicit `Cookie` (the jar adds its own cookies for each hop) and the
//! `api_key` header or query parameter. Any other header, and a kept
//! `307`/`308` body, that carries one of the request's secrets is dropped
//! too, with a warning naming what was left behind. The engine stops
//! signing SigV4 at the first hop that leaves the origin (see
//! `engine::Hops::follow`).

use bytes::Bytes;
use http::header::{
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, LOCATION, PROXY_AUTHORIZATION,
    TRANSFER_ENCODING,
};
use http::{HeaderMap, HeaderName, Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use rustman_core::Redactor;
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

/// The request for a redirect hop, and the warning when leaving the origin
/// dropped a header or body that carries a secret.
pub(crate) struct Next {
    pub request: Request<Full<Bytes>>,
    pub warning: Option<String>,
}

/// The next hop when `status` and `headers` are a redirect to follow, else
/// `None`. `secrets` finds the request's secrets in any other header or the
/// body.
pub(crate) async fn next_request(
    current: &Request<Full<Bytes>>,
    status: StatusCode,
    headers: &HeaderMap,
    credentials: &Credentials,
    secrets: &Redactor,
) -> Result<Option<Next>, RequestError> {
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
        drop_body(&mut next);
    }
    let mut warning = None;
    if !same_origin(current.uri(), next.uri()) {
        strip_credentials(&mut next, credentials)?;
        let dropped = strip_secrets(&mut next, secrets).await;
        warning = dropped_warning(next.uri(), &dropped);
    }
    Ok(Some(Next {
        request: next,
        warning,
    }))
}

fn drop_body(request: &mut Request<Full<Bytes>>) {
    *request.body_mut() = Full::new(Bytes::new());
    for name in [CONTENT_TYPE, CONTENT_LENGTH, TRANSFER_ENCODING] {
        request.headers_mut().remove(name);
    }
}

/// Drop every header and the body that carry one of `secrets`, returning
/// what was dropped (`header <name>`, `the body`).
async fn strip_secrets(request: &mut Request<Full<Bytes>>, secrets: &Redactor) -> Vec<String> {
    if secrets.is_empty() {
        return Vec::new();
    }
    let carries = |value: &[u8]| matches!(secrets.bytes(value), std::borrow::Cow::Owned(_));
    let mut names: Vec<HeaderName> = Vec::new();
    for (name, value) in request.headers() {
        if carries(value.as_bytes()) && !names.contains(name) {
            names.push(name.clone());
        }
    }
    let mut dropped = Vec::new();
    for name in names {
        request.headers_mut().remove(&name);
        dropped.push(format!("header {name}"));
    }
    let body = match request.body().clone().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(never) => match never {},
    };
    if carries(&body) {
        drop_body(request);
        dropped.push("the body".to_owned());
    }
    dropped
}

fn dropped_warning(uri: &Uri, dropped: &[String]) -> Option<String> {
    if dropped.is_empty() {
        return None;
    }
    let origin = match uri.port_u16() {
        Some(port) => format!(
            "{}://{}:{port}",
            scheme(uri),
            uri.host().unwrap_or_default()
        ),
        None => format!("{}://{}", scheme(uri), uri.host().unwrap_or_default()),
    };
    Some(format!(
        "cross-origin redirect to {origin}: dropped {}, which carry a secret",
        dropped.join(", ")
    ))
}

fn scheme(uri: &Uri) -> &str {
    uri.scheme_str().unwrap_or("http")
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

pub(crate) fn same_origin(a: &Uri, b: &Uri) -> bool {
    a.scheme() == b.scheme() && host(a) == host(b) && port(a) == port(b)
}

/// The host of `uri`, lowercased and without a trailing dot.
fn host(uri: &Uri) -> Option<String> {
    uri.host()
        .map(|h| h.strip_suffix('.').unwrap_or(h).to_ascii_lowercase())
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
        next(req, code, to, credentials, &Redactor::new()).request
    }

    fn next(
        req: &Request<Full<Bytes>>,
        code: u16,
        to: &str,
        credentials: &Credentials,
        secrets: &Redactor,
    ) -> Next {
        let status = StatusCode::from_u16(code).unwrap();
        let to = location(to);
        let next = next_request(req, status, &to, credentials, secrets);
        block_on(next).unwrap().unwrap()
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
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
    fn host_case_and_a_trailing_dot_keep_the_origin() {
        let req = request("GET", "https://API.Example.com/x");
        for to in ["https://api.example.com/y", "https://api.example.com./y"] {
            assert!(
                follow(&req, 302, to).headers().contains_key(AUTHORIZATION),
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
        let (none, secrets) = (Credentials::default(), Redactor::new());
        let (to_b, empty) = (location("/b"), HeaderMap::new());
        let next = next_request(&req, StatusCode::NOT_MODIFIED, &to_b, &none, &secrets);
        assert!(block_on(next).unwrap().is_none());
        let next = next_request(&req, StatusCode::FOUND, &empty, &none, &secrets);
        assert!(block_on(next).unwrap().is_none());
    }

    #[test]
    fn an_unparsable_location_is_an_error() {
        let req = request("GET", "http://h/a");
        let (none, secrets) = (Credentials::default(), Redactor::new());
        let bad = location("http://[bad");
        let next = next_request(&req, StatusCode::FOUND, &bad, &none, &secrets);
        let err = block_on(next).err().unwrap();
        assert!(
            err.to_string()
                .starts_with("invalid redirect location 'http://[bad'"),
            "{err}"
        );
    }

    fn secret_request(method: &str) -> Request<Full<Bytes>> {
        Request::builder()
            .method(method)
            .uri("https://api.example.com/token")
            .header("x-api-key", "k-s3cr3t-value")
            .header("x-trace", "plain")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Full::new(Bytes::from_static(
                b"grant_type=client_credentials&client_secret=k-s3cr3t-value",
            )))
            .unwrap()
    }

    fn secrets() -> Redactor {
        Redactor::new().with(&rustman_core::Secret::new(
            "tok",
            "k-s3cr3t-value".to_owned(),
        ))
    }

    async fn body_of(request: &Request<Full<Bytes>>) -> Bytes {
        let collected = request.body().clone().collect().await;
        collected.map(|c| c.to_bytes()).unwrap_or_default()
    }

    #[test]
    fn secret_headers_and_bodies_are_dropped_when_the_origin_changes() {
        let req = secret_request("POST");
        let none = Credentials::default();
        for code in [307, 308] {
            let other = next(&req, code, "https://evil.example/t", &none, &secrets());
            assert!(!other.request.headers().contains_key("x-api-key"), "{code}");
            assert_eq!(other.request.headers()["x-trace"], "plain");
            assert!(!other.request.headers().contains_key(CONTENT_TYPE));
            assert_eq!(other.request.method(), Method::POST);
            assert!(block_on(body_of(&other.request)).is_empty(), "{code}");
            assert_eq!(
                other.warning.as_deref(),
                Some(
                    "cross-origin redirect to https://evil.example: dropped header x-api-key, \
                     the body, which carry a secret"
                )
            );
        }
    }

    #[test]
    fn secrets_stay_on_a_same_origin_redirect() {
        let req = secret_request("POST");
        let same = next(&req, 307, "/t2", &Credentials::default(), &secrets());
        assert_eq!(same.request.headers()["x-api-key"], "k-s3cr3t-value");
        assert_eq!(body_of_sync(&same.request), body_of_sync(&req));
        assert!(same.warning.is_none());
    }

    fn body_of_sync(request: &Request<Full<Bytes>>) -> Bytes {
        block_on(body_of(request))
    }

    #[test]
    fn requests_without_secrets_cross_origins_unchanged() {
        let req = secret_request("POST");
        let other = next(
            &req,
            308,
            "http://other:8080/t",
            &Credentials::default(),
            &Redactor::new(),
        );
        assert_eq!(other.request.headers()["x-api-key"], "k-s3cr3t-value");
        assert_eq!(body_of_sync(&other.request), body_of_sync(&req));
        assert!(other.warning.is_none());
        let plain = Redactor::new().with(&rustman_core::Secret::new("o", "elsewhere".to_owned()));
        let other = next(
            &req,
            308,
            "http://other:8080/t",
            &Credentials::default(),
            &plain,
        );
        assert!(other.warning.is_none());
    }
}
