//! An in-process OTLP trace collector (add-trace-propagation section 4):
//! accepts `Export` over gRPC (HTTP/2, `application/grpc`) and
//! `http/protobuf` (`POST /v1/traces`) on one port, and decodes the spans
//! with its own minimal protobuf messages, independent of the exporter's.

use std::convert::Infallible;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use prost::Message;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// One export request the collector received.
#[derive(Clone, Debug)]
pub struct ExportHit {
    pub path: String,
    pub content_type: String,
    pub headers: HeaderMap,
}

/// One span, decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectedSpan {
    pub service: String,
    pub trace_id: String,
    pub span_id: String,
    pub trace_state: String,
    pub name: String,
    /// The OTLP `SpanKind` number: 3 is `CLIENT`.
    pub kind: i32,
    /// The OTLP status code: 2 is `ERROR`.
    pub status_code: i32,
    pub attributes: Vec<(String, String)>,
    pub events: Vec<String>,
}

impl CollectedSpan {
    pub const CLIENT: i32 = 3;

    /// The attribute `key`, as text.
    pub fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct State {
    hits: Vec<ExportHit>,
    spans: Vec<CollectedSpan>,
}

pub struct OtlpCollector {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for OtlpCollector {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OtlpCollector {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let state = shared.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(state.clone(), req));
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        Self { addr, state, task }
    }

    /// `http://127.0.0.1:<port>`, for either protocol.
    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn hits(&self) -> Vec<ExportHit> {
        self.state.lock().unwrap().hits.clone()
    }

    pub fn spans(&self) -> Vec<CollectedSpan> {
        self.state.lock().unwrap().spans.clone()
    }

    /// The spans, once at least `count` arrived or `within` passed.
    pub async fn wait_for_spans(&self, count: usize, within: Duration) -> Vec<CollectedSpan> {
        let deadline = Instant::now() + within;
        loop {
            let spans = self.spans();
            if spans.len() >= count || Instant::now() >= deadline {
                return spans;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// An `http://127.0.0.1:<port>` nothing listens on: a collector that is down.
pub async fn unreachable_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

type Body = BoxBody<Bytes, Infallible>;

async fn handle(
    state: Arc<Mutex<State>>,
    req: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let content_type = parts
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let grpc = content_type.starts_with("application/grpc");
    let message = if grpc {
        body.get(5..).unwrap_or_default()
    } else {
        &body[..]
    };
    let decoded = ExportTraceServiceRequest::decode(message);
    let mut state = state.lock().unwrap();
    state.hits.push(ExportHit {
        path: parts.uri.path().to_owned(),
        content_type,
        headers: parts.headers,
    });
    let Ok(request) = decoded else {
        return Ok(plain(StatusCode::BAD_REQUEST));
    };
    state.spans.extend(collected(request));
    Ok(if grpc {
        grpc_ok()
    } else {
        plain(StatusCode::OK)
    })
}

fn plain(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::new()).boxed());
    *response.status_mut() = status;
    response
}

/// An empty `ExportTraceServiceResponse` and `grpc-status: 0` trailers.
fn grpc_ok() -> Response<Body> {
    let mut trailers = HeaderMap::new();
    trailers.insert("grpc-status", HeaderValue::from_static("0"));
    let frames: Vec<Result<Frame<Bytes>, Infallible>> = vec![
        Ok(Frame::data(Bytes::from_static(&[0, 0, 0, 0, 0]))),
        Ok(Frame::trailers(trailers)),
    ];
    let mut response = Response::new(StreamBody::new(futures_util::stream::iter(frames)).boxed());
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/grpc"),
    );
    response
}

fn collected(request: ExportTraceServiceRequest) -> Vec<CollectedSpan> {
    let mut spans = Vec::new();
    for resource in request.resource_spans {
        let attributes = resource.resource.map(|r| r.attributes).unwrap_or_default();
        let service = text_attributes(attributes)
            .into_iter()
            .find(|(k, _)| k == "service.name")
            .map(|(_, v)| v)
            .unwrap_or_default();
        for span in resource.scope_spans.into_iter().flat_map(|s| s.spans) {
            spans.push(CollectedSpan {
                service: service.clone(),
                trace_id: hex(&span.trace_id),
                span_id: hex(&span.span_id),
                trace_state: span.trace_state,
                name: span.name,
                kind: span.kind,
                status_code: span.status.map_or(0, |s| s.code),
                attributes: text_attributes(span.attributes),
                events: span.events.into_iter().map(|e| e.name).collect(),
            });
        }
    }
    spans
}

fn text_attributes(attributes: Vec<KeyValue>) -> Vec<(String, String)> {
    attributes
        .into_iter()
        .map(|kv| {
            let value = match kv.value.and_then(|v| v.value) {
                Some(Value::String(s)) => s,
                Some(Value::Bool(b)) => b.to_string(),
                Some(Value::Int(i)) => i.to_string(),
                Some(Value::Double(d)) => d.to_string(),
                None => String::new(),
            };
            (kv.key, value)
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

// The subset of the OTLP trace protos the collector reads (field numbers
// from opentelemetry-proto's `trace.proto`, `common.proto` and
// `resource.proto`); other fields are skipped.

#[derive(Clone, PartialEq, Message)]
struct ExportTraceServiceRequest {
    #[prost(message, repeated, tag = "1")]
    resource_spans: Vec<ResourceSpans>,
}

#[derive(Clone, PartialEq, Message)]
struct ResourceSpans {
    #[prost(message, optional, tag = "1")]
    resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    scope_spans: Vec<ScopeSpans>,
}

#[derive(Clone, PartialEq, Message)]
struct Resource {
    #[prost(message, repeated, tag = "1")]
    attributes: Vec<KeyValue>,
}

#[derive(Clone, PartialEq, Message)]
struct ScopeSpans {
    #[prost(message, repeated, tag = "2")]
    spans: Vec<Span>,
}

#[derive(Clone, PartialEq, Message)]
struct Span {
    #[prost(bytes = "vec", tag = "1")]
    trace_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    span_id: Vec<u8>,
    #[prost(string, tag = "3")]
    trace_state: String,
    #[prost(string, tag = "5")]
    name: String,
    #[prost(int32, tag = "6")]
    kind: i32,
    #[prost(message, repeated, tag = "9")]
    attributes: Vec<KeyValue>,
    #[prost(message, repeated, tag = "11")]
    events: Vec<Event>,
    #[prost(message, optional, tag = "15")]
    status: Option<Status>,
}

#[derive(Clone, PartialEq, Message)]
struct Event {
    #[prost(string, tag = "2")]
    name: String,
}

#[derive(Clone, PartialEq, Message)]
struct Status {
    #[prost(int32, tag = "3")]
    code: i32,
}

#[derive(Clone, PartialEq, Message)]
struct KeyValue {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(message, optional, tag = "2")]
    value: Option<AnyValue>,
}

#[derive(Clone, PartialEq, Message)]
struct AnyValue {
    #[prost(oneof = "Value", tags = "1, 2, 3, 4")]
    value: Option<Value>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
enum Value {
    #[prost(string, tag = "1")]
    String(String),
    #[prost(bool, tag = "2")]
    Bool(bool),
    #[prost(int64, tag = "3")]
    Int(i64),
    #[prost(double, tag = "4")]
    Double(f64),
}
