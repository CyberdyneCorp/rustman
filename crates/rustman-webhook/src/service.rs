//! Handling one request: framing and limit checks, body collection,
//! capture, and the mock or default response.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http::header::{CONNECTION, CONTENT_LENGTH, CONTENT_TYPE};
use http::request::Parts;
use http::{HeaderValue, Request, Response, StatusCode, Version};
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use time::OffsetDateTime;

use crate::capture::{Capture, HeaderEntry};
use crate::config::Limits;
use crate::conn::ConnState;
use crate::event::{Event, Rejection};
use crate::head;
use crate::listener::Shared;
use crate::mocks::Mock;

/// The body of the default acknowledgement (spec: Default response).
pub const DEFAULT_ACK: &str = r#"{"status":"ok","source":"rustman"}"#;

pub(crate) const TOO_MANY_HEADERS: &str = "too many header fields";
pub(crate) const HEADERS_TOO_LARGE: &str = "header fields too large";

/// Bytes a header field adds to a head besides its name and value
/// (`": "` and CRLF).
const FIELD_OVERHEAD: usize = 4;

type Reply = Response<Full<Bytes>>;

/// A request that passed every check.
struct Received {
    parts: Parts,
    headers: Vec<HeaderEntry>,
    body: Bytes,
}

/// Why a request is refused: the status and the event-log reason.
struct Refusal(StatusCode, String);

pub(crate) async fn handle(
    req: Request<Incoming>,
    shared: Arc<Shared>,
    conn: Arc<ConnState>,
) -> Result<Reply, Infallible> {
    let _in_flight = conn.begin();
    let version = req.version();
    let reply = match receive(req, &shared.limits, &conn).await {
        Ok(received) => respond(received, &shared, &conn).await,
        Err(Refusal(status, reason)) => {
            shared.store.emit(Event::Rejected(Rejection {
                remote: conn.remote,
                status: status.as_u16(),
                reason: reason.clone(),
            }));
            conn.linger();
            refusal_response(status, &reason, version)
        }
    };
    Ok(reply)
}

async fn receive(
    req: Request<Incoming>,
    limits: &Limits,
    conn: &ConnState,
) -> Result<Received, Refusal> {
    let (parts, body) = req.into_parts();
    let headers = conn
        .with_tap(|raw| head::headers(parts.version, &parts.headers, raw, limits.max_headers))
        .map_err(|head::UnreadableHead| {
            Refusal(
                StatusCode::BAD_REQUEST,
                "unreadable request head".to_owned(),
            )
        })?;
    check_framing(&headers)?;
    check_header_limits(&headers, limits)?;
    check_declared_length(&parts, limits.max_body)?;
    let body = read_body(body, limits).await?;
    Ok(Received {
        parts,
        headers,
        body,
    })
}

fn has(headers: &[HeaderEntry], name: &str) -> bool {
    headers.iter().any(|h| h.name.eq_ignore_ascii_case(name))
}

/// `Transfer-Encoding` with `Content-Length` is ambiguous framing (RFC 9112
/// §6.1, request smuggling); hyper would silently prefer the former.
fn check_framing(headers: &[HeaderEntry]) -> Result<(), Refusal> {
    if has(headers, "transfer-encoding") && has(headers, "content-length") {
        return Err(Refusal(
            StatusCode::BAD_REQUEST,
            "both Transfer-Encoding and Content-Length".to_owned(),
        ));
    }
    Ok(())
}

fn check_header_limits(headers: &[HeaderEntry], limits: &Limits) -> Result<(), Refusal> {
    let too_large = StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE;
    if headers.len() > limits.max_headers {
        return Err(Refusal(too_large, TOO_MANY_HEADERS.to_owned()));
    }
    let bytes: usize = headers
        .iter()
        .map(|h| h.name.len() + h.value.len() + FIELD_OVERHEAD)
        .sum();
    if bytes > limits.max_header_bytes {
        return Err(Refusal(too_large, HEADERS_TOO_LARGE.to_owned()));
    }
    Ok(())
}

fn body_too_large(max_body: usize) -> Refusal {
    Refusal(
        StatusCode::PAYLOAD_TOO_LARGE,
        format!("body larger than {max_body} bytes"),
    )
}

/// Refuse a declared oversize body before reading any of it.
fn check_declared_length(parts: &Parts, max_body: usize) -> Result<(), Refusal> {
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    match declared {
        Some(length) if length > max_body as u64 => Err(body_too_large(max_body)),
        _ => Ok(()),
    }
}

async fn read_body(body: Incoming, limits: &Limits) -> Result<Bytes, Refusal> {
    let collect = Limited::new(body, limits.max_body).collect();
    match tokio::time::timeout(limits.body_timeout, collect).await {
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(err)) if err.downcast_ref::<LengthLimitError>().is_some() => {
            Err(body_too_large(limits.max_body))
        }
        Ok(Err(err)) => Err(Refusal(
            StatusCode::BAD_REQUEST,
            format!("invalid body: {err}"),
        )),
        Err(_) => Err(Refusal(
            StatusCode::REQUEST_TIMEOUT,
            "request body timeout".to_owned(),
        )),
    }
}

/// Capture the request, then answer with the first matching mock or the
/// default acknowledgement. The capture is announced before a mock's delay.
async fn respond(received: Received, shared: &Shared, conn: &ConnState) -> Reply {
    let Received {
        parts,
        headers,
        body,
    } = received;
    let mock = shared.mocks.find(parts.method.as_str(), parts.uri.path());
    let received_at = OffsetDateTime::now_utc().to_offset(shared.offset);
    let signature = shared
        .verifiers
        .check(parts.uri.path(), &headers, &body, received_at);
    let capture = Capture {
        id: 0,
        received_at,
        remote: conn.remote,
        version: parts.version,
        method: parts.method.as_str().to_owned(),
        path: parts.uri.path().to_owned(),
        query: parts.uri.query().map(str::to_owned),
        headers,
        body,
        status: mock.as_ref().map_or(200, |m| m.status.as_u16()),
        mock: mock.as_ref().map(|m| m.pattern.clone()),
        signature,
    };
    let capture = shared.store.push(capture);
    match mock {
        Some(mock) => mocked(&mock, &capture, shared).await,
        None => default_ack(),
    }
}

async fn mocked(mock: &Mock, capture: &Capture, shared: &Shared) -> Reply {
    shared.store.emit(Event::MockMatched {
        id: capture.id,
        method: capture.method.clone(),
        pattern: mock.pattern.clone(),
        status: mock.status.as_u16(),
        delay_ms: u64::try_from(mock.delay.as_millis()).unwrap_or(u64::MAX),
    });
    tokio::time::sleep(mock.delay).await;
    mock.response()
}

fn json_response(status: StatusCode, body: String) -> Reply {
    let length = body.len();
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
    response
}

/// `200 {"status":"ok","source":"rustman"}` with its `Content-Length`.
pub(crate) fn default_ack() -> Reply {
    json_response(StatusCode::OK, DEFAULT_ACK.to_owned())
}

/// A JSON error body; HTTP/1 connections are closed after it.
fn refusal_response(status: StatusCode, reason: &str, version: Version) -> Reply {
    let body = serde_json::json!({ "error": reason, "status": status.as_u16() }).to_string();
    let mut response = json_response(status, body);
    if version < Version::HTTP_2 {
        response
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Vec<HeaderEntry> {
        pairs
            .iter()
            .map(|(n, v)| HeaderEntry::new(*n, Bytes::copy_from_slice(v.as_bytes())))
            .collect()
    }

    #[tokio::test]
    async fn default_ack_is_exact_with_content_length() {
        let response = default_ack();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "application/json");
        assert_eq!(response.headers()["content-length"], "34");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body, r#"{"status":"ok","source":"rustman"}"#);
    }

    #[test]
    fn te_with_cl_is_refused() {
        let both = entries(&[("Transfer-Encoding", "chunked"), ("Content-Length", "10")]);
        assert!(check_framing(&both).is_err());
        assert!(check_framing(&entries(&[("Content-Length", "10")])).is_ok());
    }

    #[test]
    fn header_limits_count_fields_and_bytes() {
        let limits = Limits {
            max_headers: 2,
            max_header_bytes: 16,
            ..Limits::default()
        };
        let reason = |pairs: &[(&str, &str)]| {
            check_header_limits(&entries(pairs), &limits)
                .err()
                .map(|r| r.1)
        };
        assert_eq!(reason(&[("a", "1"), ("b", "2")]), None);
        assert_eq!(
            reason(&[("a", "1"), ("b", "2"), ("c", "3")]).as_deref(),
            Some(TOO_MANY_HEADERS)
        );
        assert_eq!(
            reason(&[("a", "123456789012")]).as_deref(),
            Some(HEADERS_TOO_LARGE)
        );
    }

    #[test]
    fn refusals_are_json_and_close_http1() {
        let response = refusal_response(StatusCode::PAYLOAD_TOO_LARGE, "body", Version::HTTP_11);
        assert_eq!(response.headers()["connection"], "close");
        let h2 = refusal_response(StatusCode::PAYLOAD_TOO_LARGE, "body", Version::HTTP_2);
        assert!(h2.headers().get("connection").is_none());
    }
}
