//! Building the outgoing `http::Request` from a `ResolvedRequest`.
//!
//! The resolved text already holds secret values; this is the only place they
//! are turned into wire bytes.

use bytes::Bytes;
use http::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, HOST, USER_AGENT};
use http::{HeaderName, HeaderValue, Method, Request, Uri};
use http_body_util::Full;
use rustman_core::vars::{ResolvedBody, ResolvedPair, ResolvedRequest};

use crate::error::RequestError;
use crate::multipart;

/// `User-Agent` sent unless the request sets one.
pub const USER_AGENT_VALUE: &str = concat!("rustman/", env!("CARGO_PKG_VERSION"));

/// The request to send and the warnings building it produced (a stale
/// `Content-Length` or `Host` that was replaced).
pub(crate) async fn build(
    request: &ResolvedRequest,
) -> Result<(Request<Full<Bytes>>, Vec<String>), RequestError> {
    let uri = uri(&request.url.text, &request.query)?;
    let method = Method::from_bytes(request.method.as_bytes())
        .map_err(|_| RequestError::Invalid(format!("invalid HTTP method '{}'", request.method)))?;
    let (body, content_type) = body(&request.body).await?;
    let mut warnings = Vec::new();
    let mut builder = Request::builder().method(method).uri(&uri);
    let headers = builder.headers_mut().expect("method and URI are valid");
    for pair in &request.headers {
        let (name, value) = header(pair)?;
        // Content-Length and Host are always computed by the engine.
        if let Some(warning) = computed_header(&name, &value, &uri, body.len()) {
            warnings.extend(warning);
            continue;
        }
        headers.append(name, value);
    }
    let defaults = [
        (USER_AGENT, Some(USER_AGENT_VALUE.to_owned())),
        (ACCEPT, Some("*/*".to_owned())),
        (CONTENT_TYPE, content_type),
    ];
    for (name, value) in defaults {
        if let (Some(value), false) = (value, headers.contains_key(&name)) {
            let value = HeaderValue::try_from(value).expect("default values are valid");
            headers.insert(name, value);
        }
    }
    let request = builder.body(Full::new(body)).expect("parts were validated");
    Ok((request, warnings))
}

/// For `Content-Length` and `Host`, which the engine computes: `Some` with a
/// warning when the request's value differs from the computed one.
fn computed_header(
    name: &HeaderName,
    value: &HeaderValue,
    uri: &Uri,
    body_len: usize,
) -> Option<Option<String>> {
    let given = String::from_utf8_lossy(value.as_bytes());
    let given = given.trim();
    if name == CONTENT_LENGTH {
        let stale = given != body_len.to_string();
        return Some(stale.then(|| {
            format!("ignored stale Content-Length: {given} (sent the computed {body_len})")
        }));
    }
    if name == HOST {
        let computed = uri.authority().map_or("", |a| a.as_str());
        let stale = !given.eq_ignore_ascii_case(computed);
        return Some(
            stale.then(|| format!("ignored Host: {given} (sent the URL's host {computed})")),
        );
    }
    None
}

/// A copy of `request` (method, URI, version, headers and body), for a
/// redirect hop or a wire-only header.
pub(crate) fn duplicate(request: &Request<Full<Bytes>>) -> Request<Full<Bytes>> {
    let mut copy = Request::builder()
        .method(request.method().clone())
        .uri(request.uri().clone())
        .version(request.version())
        .body(request.body().clone())
        .expect("parts come from a valid request");
    *copy.headers_mut() = request.headers().clone();
    copy
}

/// The URL with `query` appended, without any fragment.
pub(crate) fn uri(url: &str, query: &[ResolvedPair]) -> Result<Uri, RequestError> {
    let base = url.split_once('#').map_or(url, |(base, _)| base);
    let mut full = base.to_owned();
    if !query.is_empty() {
        let mut encoded = form_urlencoded::Serializer::new(String::new());
        for pair in query {
            encoded.append_pair(&pair.name.text, &pair.value.text);
        }
        if !base.contains('?') {
            full.push('?');
        } else if !base.ends_with(['?', '&']) {
            full.push('&');
        }
        full.push_str(&encoded.finish());
    }
    full.parse::<Uri>()
        .map_err(|e| RequestError::Invalid(format!("invalid URL '{full}': {e}")))
}

fn header(pair: &ResolvedPair) -> Result<(HeaderName, HeaderValue), RequestError> {
    let name = HeaderName::from_bytes(pair.name.text.as_bytes())
        .map_err(|_| RequestError::Invalid(format!("invalid header name '{}'", pair.name.text)))?;
    let value = HeaderValue::from_str(&pair.value.text)
        .map_err(|_| RequestError::Invalid(format!("invalid value for header '{name}'")))?;
    Ok((name, value))
}

/// The body bytes and the `Content-Type` its type implies.
async fn body(body: &ResolvedBody) -> Result<(Bytes, Option<String>), RequestError> {
    let typed = |bytes: Vec<u8>, content_type: &str| {
        Ok((Bytes::from(bytes), Some(content_type.to_owned())))
    };
    match body {
        ResolvedBody::None => Ok((Bytes::new(), None)),
        ResolvedBody::Json(value) => typed(
            serde_json::to_vec(value).expect("JSON values serialize"),
            "application/json",
        ),
        ResolvedBody::JsonText(text) => typed(text.text.clone().into_bytes(), "application/json"),
        ResolvedBody::Text(text) => {
            typed(text.text.clone().into_bytes(), "text/plain; charset=utf-8")
        }
        ResolvedBody::Form(fields) => typed(
            form(fields).into_bytes(),
            "application/x-www-form-urlencoded",
        ),
        ResolvedBody::File(file) => {
            let bytes = tokio::fs::read(&file.path).await.map_err(|e| {
                RequestError::Io(format!("cannot read body file {}: {e}", file.raw))
            })?;
            typed(bytes, "application/octet-stream")
        }
        ResolvedBody::Multipart(parts) => {
            let encoded = multipart::encode(parts).await?;
            Ok((encoded.body.clone(), Some(encoded.content_type())))
        }
    }
}

/// `application/x-www-form-urlencoded` text of `fields`.
pub(crate) fn form(fields: &[ResolvedPair]) -> String {
    let mut encoded = form_urlencoded::Serializer::new(String::new());
    for field in fields {
        encoded.append_pair(&field.name.text, &field.value.text);
    }
    encoded.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::vars::Resolved;

    fn pair(name: &str, value: &str) -> ResolvedPair {
        ResolvedPair {
            name: Resolved::literal(name),
            value: Resolved::literal(value),
        }
    }

    #[test]
    fn query_is_appended_and_encoded() {
        let q = [pair("q", "a b"), pair("x", "1&2")];
        let uri = uri("http://h/p#frag", &q).unwrap();
        assert_eq!(uri.to_string(), "http://h/p?q=a+b&x=1%262");
        let uri = uri_of("http://h/p?a=1", &q[..1]);
        assert_eq!(uri, "http://h/p?a=1&q=a+b");
        assert_eq!(uri_of("http://h/p", &[]), "http://h/p");
    }

    fn uri_of(url: &str, q: &[ResolvedPair]) -> String {
        uri(url, q).unwrap().to_string()
    }

    #[test]
    fn invalid_urls_are_reported() {
        let err = uri("http://h/a b", &[]).unwrap_err();
        assert!(
            err.to_string().starts_with("invalid URL 'http://h/a b'"),
            "{err}"
        );
    }

    #[test]
    fn invalid_headers_are_reported() {
        let err = header(&pair("bad name", "v")).unwrap_err();
        assert_eq!(err.to_string(), "invalid header name 'bad name'");
        let err = header(&pair("x", "line\nbreak")).unwrap_err();
        assert_eq!(err.to_string(), "invalid value for header 'x'");
    }
}
