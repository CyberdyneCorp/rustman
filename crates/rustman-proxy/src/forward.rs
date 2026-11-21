//! Relaying one request upstream and its response back (design D1): the
//! upstream leg opens its connection with `rustman-http`'s connector, so DNS,
//! the connect timeout and TLS verification behave as in `http-requests`.
//! Bodies stream through; the exchange is recorded once the response body
//! is done.

use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::uri::{Authority, PathAndQuery, Scheme};
use http::{Request, Response, StatusCode, Uri, Version};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use rustman_http::RawConnector;
use tokio::sync::Notify;

use crate::body::{BoxError, Budget, ProxyBody, Sink, Tee, full, sink};
use crate::capture::{BodyCapture, Exchange, Header, Recorded};
use crate::recorder::Recorder;

/// Headers that describe one connection, never forwarded (RFC 9110
/// section 7.6.1), plus the proxy's own credentials.
const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "proxy-connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// The origin a request is sent to.
#[derive(Clone, Debug)]
pub(crate) struct Origin {
    pub scheme: Scheme,
    pub authority: Authority,
}

impl Origin {
    /// The origin of an absolute-form request URI. Its authority may carry
    /// userinfo: see [`Origin::has_userinfo`].
    pub fn of(uri: &Uri) -> Option<Self> {
        Some(Self {
            scheme: uri.scheme()?.clone(),
            authority: uri.authority()?.clone(),
        })
    }

    /// Whether the authority carries `user:password@`, which a proxy must
    /// treat as an error (RFC 9110 section 4.2.4) rather than forward or
    /// record.
    pub fn has_userinfo(&self) -> bool {
        self.authority.as_str().contains('@')
    }

    /// The port, the scheme's default when the authority has none.
    pub fn port(&self) -> u16 {
        let default = if self.scheme == Scheme::HTTPS {
            443
        } else {
            80
        };
        self.authority.port_u16().unwrap_or(default)
    }

    fn uri(&self, path: &str) -> Result<Uri, http::Error> {
        Uri::builder()
            .scheme(self.scheme.clone())
            .authority(self.authority.clone())
            .path_and_query(path)
            .build()
    }

    /// The `Host` header value: the authority without the scheme's
    /// default port.
    fn host_header(&self) -> String {
        let default = if self.scheme == Scheme::HTTPS {
            443
        } else {
            80
        };
        match self.authority.port_u16() {
            Some(port) if port == default => self.authority.host().to_owned(),
            _ => self.authority.as_str().to_owned(),
        }
    }
}

/// What forwarding needs from the proxy.
pub(crate) struct Upstream {
    /// Opens upstream connections for clients on this machine.
    pub direct: RawConnector,
    /// Opens upstream connections for clients on the network: targets that
    /// resolve to an address that is not globally routable are refused.
    pub guarded: RawConnector,
    pub recorder: Recorder,
    pub max_body: usize,
    /// Shared by the copies of every body in flight.
    pub budget: Budget,
    /// How long the upstream may take to send a response head once the
    /// request is sent.
    pub response_timeout: Duration,
}

impl Upstream {
    /// The connector for a client on the network (`remote`) or not.
    pub fn connector(&self, remote: bool) -> &RawConnector {
        if remote { &self.guarded } else { &self.direct }
    }
}

/// How a request reached the proxy.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Leg {
    /// Decrypted from an intercepted tunnel.
    pub intercepted: bool,
    /// From a client on the network.
    pub remote: bool,
}

/// An exchange whose response is not finished yet.
struct Pending {
    started: SystemTime,
    clock: Instant,
    method: String,
    url: String,
    version: String,
    request_headers: Vec<Header>,
    request_body: Sink,
    intercepted: bool,
    recorder: Recorder,
}

/// What the response contributed to an exchange.
struct ResponsePart {
    status: Option<u16>,
    headers: Vec<Header>,
    body: BodyCapture,
    error: Option<String>,
}

impl Pending {
    fn finish(self, response: ResponsePart) {
        let request_body = self
            .request_body
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .capture();
        let exchange = Exchange {
            method: self.method,
            url: self.url,
            version: self.version,
            request_headers: self.request_headers,
            request_body,
            status: response.status,
            response_headers: response.headers,
            response_body: response.body,
            duration: self.clock.elapsed(),
            intercepted: self.intercepted,
            error: response.error,
        };
        self.recorder
            .record(self.started, Recorded::Exchange(Box::new(exchange)));
    }
}

/// Send `request` to `origin`, record the exchange and answer the client.
pub(crate) async fn forward(
    request: Request<Incoming>,
    origin: &Origin,
    leg: Leg,
    upstream: &Upstream,
) -> Response<ProxyBody> {
    let (parts, body) = request.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map_or("/", PathAndQuery::as_str)
        .to_owned();
    let pending = Pending {
        started: SystemTime::now(),
        clock: Instant::now(),
        method: parts.method.to_string(),
        url: format!("{}://{}{path}", origin.scheme, origin.authority),
        version: version_name(parts.version).to_owned(),
        request_headers: header_list(&parts.headers),
        request_body: sink(&upstream.budget),
        intercepted: leg.intercepted,
        recorder: upstream.recorder.clone(),
    };
    // hyper drops the request body once it is sent: the response timeout
    // starts then, so a long upload is not cut short.
    let sent = Arc::new(Notify::new());
    let signal = Arc::clone(&sent);
    let body = Tee::new(body, pending.request_body.clone(), upstream.max_body)
        .on_drop(move || signal.notify_one());
    let outgoing = outgoing_request(parts, body, origin, &path);
    let connector = upstream.connector(leg.remote);
    let response = send(
        outgoing,
        origin,
        connector,
        &sent,
        upstream.response_timeout,
    );
    match response.await {
        Ok(response) => relay(response, pending, upstream),
        Err(failure) => {
            let message = format!("upstream error: {}", failure.error);
            pending.finish(ResponsePart {
                status: None,
                headers: Vec::new(),
                body: BodyCapture::default(),
                error: Some(message.clone()),
            });
            plain(failure.status, &format!("rustman record: {message}\n"))
        }
    }
}

/// Why no response came back, and the status the client gets.
struct Failure {
    status: StatusCode,
    error: BoxError,
}

impl Failure {
    fn bad_gateway(error: impl Into<BoxError>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            error: error.into(),
        }
    }
}

/// The request as sent upstream: origin-form, HTTP/1.1, without
/// hop-by-hop headers, `Host` naming the origin.
fn outgoing_request(
    parts: http::request::Parts,
    body: Tee<Incoming>,
    origin: &Origin,
    path: &str,
) -> Result<Request<Tee<Incoming>>, BoxError> {
    let mut request = Request::builder()
        .method(parts.method)
        .uri(path)
        .version(Version::HTTP_11)
        .body(body)?;
    let headers = request.headers_mut();
    *headers = end_to_end(&parts.headers);
    headers.insert(header::HOST, HeaderValue::try_from(origin.host_header())?);
    Ok(request)
}

/// Send `request` and wait for the response head: at most `timeout` once
/// `sent` is notified (the request body was sent), else `504`.
async fn send<B>(
    request: Result<Request<B>, BoxError>,
    origin: &Origin,
    connector: &RawConnector,
    sent: &Notify,
    timeout: Duration,
) -> Result<Response<Incoming>, Failure>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxError>,
{
    let request = request.map_err(Failure::bad_gateway)?;
    let uri = origin.uri("/").map_err(Failure::bad_gateway)?;
    let stream = connector.open(&uri).await.map_err(Failure::bad_gateway)?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(Failure::bad_gateway)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let response = sender.send_request(request);
    tokio::pin!(response);
    tokio::select! {
        result = &mut response => return result.map_err(Failure::bad_gateway),
        () = sent.notified() => {}
    }
    match tokio::time::timeout(timeout, response).await {
        Ok(result) => result.map_err(Failure::bad_gateway),
        Err(_) => Err(Failure {
            status: StatusCode::GATEWAY_TIMEOUT,
            error: format!("no response within {} s", timeout.as_secs_f64()).into(),
        }),
    }
}

/// Stream the upstream response to the client; the exchange is recorded
/// when its body is dropped (finished or abandoned).
fn relay(
    response: Response<Incoming>,
    pending: Pending,
    upstream: &Upstream,
) -> Response<ProxyBody> {
    let (parts, body) = response.into_parts();
    let sink: Sink = sink(&upstream.budget);
    let status = parts.status.as_u16();
    let headers = header_list(&parts.headers);
    let copy = sink.clone();
    let body = Tee::new(body, sink, upstream.max_body).on_drop(move || {
        let body = copy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .capture();
        pending.finish(ResponsePart {
            status: Some(status),
            headers,
            body,
            error: None,
        });
    });
    let mut out = Response::new(http_body_util::BodyExt::boxed(body));
    *out.status_mut() = parts.status;
    *out.headers_mut() = end_to_end(&parts.headers);
    out
}

/// A short text response from the proxy itself.
pub(crate) fn plain(status: StatusCode, text: &str) -> Response<ProxyBody> {
    let mut response = Response::new(full(text.to_owned()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// `headers` without hop-by-hop headers, nor those `Connection` names.
fn end_to_end(headers: &HeaderMap) -> HeaderMap {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|name| HeaderName::try_from(name.trim()).ok())
        .collect();
    let mut out = headers.clone();
    for name in HOP_BY_HOP.iter().copied() {
        out.remove(name);
    }
    for name in named {
        out.remove(name);
    }
    out
}

/// The headers as received, values decoded lossily.
fn header_list(headers: &HeaderMap) -> Vec<Header> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            (name.as_str().to_owned(), value)
        })
        .collect()
}

fn version_name(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_2 => "HTTP/2",
        Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/1.1",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_by_hop_headers_are_dropped() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", HeaderValue::from_static("close, x-trace"));
        headers.insert("x-trace", HeaderValue::from_static("1"));
        headers.insert("proxy-authorization", HeaderValue::from_static("Basic x"));
        headers.insert("accept", HeaderValue::from_static("*/*"));
        let kept = end_to_end(&headers);
        let names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
        assert_eq!(names, ["accept"]);
    }

    #[tokio::test]
    async fn network_clients_cannot_reach_loopback_through_the_forward_path() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let options = rustman_http::SendOptions::default();
        let upstream = Upstream {
            direct: RawConnector::new(&options).unwrap(),
            guarded: RawConnector::guarded(&options, crate::guard::network_client_guard()).unwrap(),
            recorder: Recorder {
                store: crate::store::CaptureStore::new(10, 1024),
                filter: crate::filter::CaptureFilter::new(&crate::filter::FilterRules::default())
                    .unwrap(),
            },
            max_body: 1024,
            budget: Budget::new(1024),
            response_timeout: Duration::from_secs(5),
        };
        let origin = Origin::of(&format!("http://{addr}/").parse::<Uri>().unwrap()).unwrap();
        let request = || {
            let body = http_body_util::Full::new(bytes::Bytes::new());
            Ok(Request::get("/").body(body).unwrap())
        };
        let sent = Notify::new();
        let timeout = Duration::from_secs(5);
        let Err(failure) = send(request(), &origin, upstream.connector(true), &sent, timeout).await
        else {
            panic!("a network client reached loopback");
        };
        let err = failure.error;
        assert!(err.to_string().contains("refused"), "{err}");
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(accepted.is_err(), "a network client reached loopback");
        let reached = async {
            let accepted = listener.accept().await;
            drop(accepted);
        };
        let local = send(
            request(),
            &origin,
            upstream.connector(false),
            &sent,
            timeout,
        );
        let ((), _local) = tokio::join!(reached, local);
    }

    #[test]
    fn userinfo_is_detected_in_the_authority() {
        let origin = |s: &str| Origin::of(&s.parse::<Uri>().unwrap()).unwrap();
        assert!(origin("http://alice:pw@a.test/").has_userinfo());
        assert!(!origin("http://a.test:8080/").has_userinfo());
    }

    #[test]
    fn the_host_header_drops_the_default_port() {
        let origin = |s: &str| Origin::of(&s.parse::<Uri>().unwrap()).unwrap();
        assert_eq!(origin("https://a.test:443/").host_header(), "a.test");
        assert_eq!(origin("http://a.test:8080/").host_header(), "a.test:8080");
        assert_eq!(origin("http://a.test/").host_header(), "a.test");
    }
}
