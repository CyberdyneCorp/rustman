//! Answering preview requests: the reload event stream, and the files of
//! the served build (HTML with the preview additions).

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode, header};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Channel, Full};

use super::RELOAD_PATH;
use super::inject::{inject, placeholder};
use super::state::{Shared, Snapshot};
use crate::output::is_contained;

type Body = BoxBody<Bytes, Infallible>;

/// Reload events buffered for a slow page.
const EVENT_BUFFER: usize = 4;

pub async fn respond<B>(
    shared: Arc<Shared>,
    request: Request<B>,
) -> Result<Response<Body>, Infallible> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        let mut response = plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n");
        response
            .headers_mut()
            .insert(header::ALLOW, header::HeaderValue::from_static("GET, HEAD"));
        return Ok(response);
    }
    if !is_loopback_host(request.headers().get(header::HOST)) {
        return Ok(plain(StatusCode::MISDIRECTED_REQUEST, "unknown host\n"));
    }
    let path = request.uri().path();
    if path == RELOAD_PATH {
        return Ok(events(&shared));
    }
    Ok(file(&shared.snapshot(), path).await)
}

/// The served file at `path`, or a page that reloads after the next build.
async fn file(snapshot: &Snapshot, path: &str) -> Response<Body> {
    let error = snapshot.error.as_deref();
    let Some(site) = &snapshot.site else {
        let title = if error.is_some() {
            "The docs build failed"
        } else {
            "Building the docs"
        };
        let page = placeholder(title, error, snapshot.generation);
        return page_response(StatusCode::SERVICE_UNAVAILABLE, page);
    };
    let found = match file_name(path) {
        Some(name) => tokio::fs::read(site.path().join(&name))
            .await
            .ok()
            .map(|bytes| (name, bytes)),
        None => None,
    };
    let Some((name, bytes)) = found else {
        let page = placeholder("Not found", error, snapshot.generation);
        return page_response(StatusCode::NOT_FOUND, page);
    };
    if name.ends_with(".html") {
        let html = inject(&String::from_utf8_lossy(&bytes), error, snapshot.generation);
        return page_response(StatusCode::OK, html);
    }
    asset_response(&name, bytes)
}

/// A non-page site file. Opened on its own (a `[docs].logo` SVG, say),
/// it runs no script: the policy blocks scripts and sandboxes it.
fn asset_response(name: &str, bytes: Vec<u8>) -> Response<Body> {
    let mut response = response(
        StatusCode::OK,
        content_type(name),
        Full::from(bytes).boxed(),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(ASSET_POLICY),
    );
    response
}

const ASSET_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

/// The file a URL path names inside the site (`/` is `index.html`), or
/// `None` when it would leave the site.
fn file_name(path: &str) -> Option<String> {
    let mut name = path.trim_start_matches('/').to_owned();
    if name.is_empty() || name.ends_with('/') {
        name.push_str("index.html");
    }
    is_contained(&name).then_some(name)
}

/// Whether a request's `Host` names this machine's loopback interface
/// (`localhost`, `127.0.0.1`, `[::1]`, with any port). A page from another
/// origin that rebinds its own name to `127.0.0.1` sends its own name, so
/// it cannot read the preview.
fn is_loopback_host(host: Option<&header::HeaderValue>) -> bool {
    let Some(host) = host.and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or(rest, |(name, _)| name),
        None => host.split_once(':').map_or(host, |(name, _)| name),
    };
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "::1"
}

/// The SSE stream of build generations: the current one at once, then one
/// event per build.
fn events(shared: &Shared) -> Response<Body> {
    let (mut sender, body) = Channel::<Bytes, Infallible>::new(EVENT_BUFFER);
    let mut generations = shared.generations();
    tokio::spawn(async move {
        loop {
            let generation = *generations.borrow_and_update();
            let event = format!("retry: 1000\ndata: {generation}\n\n");
            if sender.send_data(Bytes::from(event)).await.is_err() {
                return;
            }
            if generations.changed().await.is_err() {
                return;
            }
        }
    });
    response(StatusCode::OK, "text/event-stream", body.boxed())
}

fn content_type(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map_or("", |(_, e)| e);
    match extension.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "md" => "text/markdown; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn page_response(status: StatusCode, html: String) -> Response<Body> {
    response(status, "text/html; charset=utf-8", Full::from(html).boxed())
}

fn plain(status: StatusCode, text: &'static str) -> Response<Body> {
    response(
        status,
        "text/plain; charset=utf-8",
        Full::from(text).boxed(),
    )
}

fn response(status: StatusCode, content_type: &'static str, body: Body) -> Response<Body> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_map_to_site_files_only() {
        assert_eq!(file_name("/").as_deref(), Some("index.html"));
        assert_eq!(
            file_name("/endpoints/users-list.html").as_deref(),
            Some("endpoints/users-list.html")
        );
        assert_eq!(
            file_name("/reference/").as_deref(),
            Some("reference/index.html")
        );
        for bad in ["/../secret", "/a/../../b", "/C:/x", "/a\\b"] {
            assert_eq!(file_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_loopback_hosts_are_answered() {
        let host = |v: &'static str| Some(header::HeaderValue::from_static(v));
        for good in [
            "localhost",
            "LOCALHOST:4000",
            "127.0.0.1:4000",
            "[::1]:4000",
            "[::1]",
        ] {
            assert!(is_loopback_host(host(good).as_ref()), "{good}");
        }
        for bad in [
            "evil.example",
            "evil.example:4000",
            "127.0.0.1.evil.example",
            "localhost.evil",
        ] {
            assert!(!is_loopback_host(host(bad).as_ref()), "{bad}");
        }
        assert!(!is_loopback_host(None));
    }

    #[test]
    fn assets_are_served_with_a_no_script_policy() {
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>";
        let response = asset_response("assets/logo.1a2b3c4d.svg", svg.to_vec());
        let headers = response.headers();
        assert_eq!(headers[header::CONTENT_TYPE], "image/svg+xml");
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], ASSET_POLICY);
        assert!(ASSET_POLICY.contains("default-src 'none'"));
        assert!(ASSET_POLICY.contains("sandbox"));
    }

    #[test]
    fn every_response_forbids_content_sniffing() {
        let responses = [
            asset_response("assets/app.js", Vec::new()),
            page_response(StatusCode::OK, String::new()),
            plain(StatusCode::NOT_FOUND, "x"),
        ];
        for response in responses {
            assert_eq!(
                response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
        }
    }

    #[test]
    fn content_types_follow_extensions() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("assets/app.1a2b3c4d.JS"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type("assets/logo.1a2b3c4d.png"), "image/png");
        assert_eq!(
            content_type(".rustman-docs-manifest"),
            "application/octet-stream"
        );
    }
}
