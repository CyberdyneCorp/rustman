//! CORS (spec: CORS): on by default when bound to loopback. A preflight
//! `OPTIONS` is answered `204` before validation, with the request origin
//! echoed, the methods of the matched path and the requested headers; any
//! other response to a request with an `Origin` echoes it too. Credentials
//! are never allowed, so a page cannot read responses with the browser's
//! cookies or auth for the mock's origin.

use bytes::Bytes;
use http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_HEADERS,
    ACCESS_CONTROL_REQUEST_METHOD, ORIGIN, VARY,
};
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use http_body_util::Full;

/// Whether CORS is on: the explicit choice, else on for loopback only.
pub fn enabled(explicit: Option<bool>, loopback: bool) -> bool {
    explicit.unwrap_or(loopback)
}

/// Whether the request is a browser preflight.
pub fn is_preflight(method: &Method, headers: &HeaderMap) -> bool {
    method == Method::OPTIONS
        && headers.contains_key(ORIGIN)
        && headers.contains_key(ACCESS_CONTROL_REQUEST_METHOD)
}

/// The `204` preflight answer; `allow` lists the path's methods (`GET,
/// POST`), else the requested method is allowed.
pub fn preflight(headers: &HeaderMap, allow: Option<String>) -> Response<Full<Bytes>> {
    let mut reply = Response::new(Full::new(Bytes::new()));
    *reply.status_mut() = StatusCode::NO_CONTENT;
    let out = reply.headers_mut();
    let requested = headers.get(ACCESS_CONTROL_REQUEST_METHOD).cloned();
    let methods = allow
        .and_then(|a| HeaderValue::from_str(&a).ok())
        .or(requested);
    if let Some(methods) = methods {
        out.insert(ACCESS_CONTROL_ALLOW_METHODS, methods);
    }
    if let Some(wanted) = headers.get(ACCESS_CONTROL_REQUEST_HEADERS) {
        out.insert(ACCESS_CONTROL_ALLOW_HEADERS, wanted.clone());
    }
    out.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    decorate(headers, out);
    reply
}

/// Echo the request `Origin` on a response, without allowing credentials.
pub fn decorate(request: &HeaderMap, response: &mut HeaderMap) {
    let Some(origin) = request.get(ORIGIN) else {
        return;
    };
    response.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    response.insert(ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static("*"));
    response.append(VARY, HeaderValue::from_static("Origin"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser(headers: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn on_by_default_for_loopback_only() {
        assert!(enabled(None, true));
        assert!(!enabled(None, false));
        assert!(enabled(Some(true), false));
        assert!(!enabled(Some(false), true));
    }

    #[test]
    fn a_preflight_needs_origin_and_a_requested_method() {
        let full = browser(&[
            ("origin", "http://localhost:5173"),
            ("access-control-request-method", "POST"),
        ]);
        assert!(is_preflight(&Method::OPTIONS, &full));
        assert!(!is_preflight(&Method::GET, &full));
        let plain = browser(&[("origin", "http://localhost:5173")]);
        assert!(!is_preflight(&Method::OPTIONS, &plain));
    }

    #[test]
    fn the_preflight_echoes_origin_methods_and_headers() {
        let request = browser(&[
            ("origin", "http://localhost:5173"),
            ("access-control-request-method", "PUT"),
            ("access-control-request-headers", "content-type, x-trace"),
        ]);
        let reply = preflight(&request, None);
        assert_eq!(reply.status(), 204);
        let h = reply.headers();
        assert_eq!(h[ACCESS_CONTROL_ALLOW_ORIGIN], "http://localhost:5173");
        assert_eq!(h[ACCESS_CONTROL_ALLOW_METHODS], "PUT");
        assert_eq!(h[ACCESS_CONTROL_ALLOW_HEADERS], "content-type, x-trace");
        let listed = preflight(&request, Some("GET, POST".into()));
        assert_eq!(listed.headers()[ACCESS_CONTROL_ALLOW_METHODS], "GET, POST");
    }

    #[test]
    fn credentials_are_never_allowed() {
        let request = browser(&[("origin", "https://evil.example")]);
        let mut response = HeaderMap::new();
        decorate(&request, &mut response);
        assert_eq!(
            response[ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://evil.example"
        );
        assert!(!response.contains_key("access-control-allow-credentials"));
    }

    #[test]
    fn no_origin_no_cors_headers() {
        let mut response = HeaderMap::new();
        decorate(&HeaderMap::new(), &mut response);
        assert!(response.is_empty());
    }
}
