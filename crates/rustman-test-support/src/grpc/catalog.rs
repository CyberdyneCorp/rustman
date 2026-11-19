//! A hand-written `shop.v1.Catalog` service: one method per call type, with
//! special request ids that fail with rich error details or answer slowly.
//! Every call's metadata is logged so tests can check what the server got.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body::Body;
use prost::Message as _;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::{KeyAndValueRef, MetadataMap, MetadataValue};
use tonic::server::{Grpc, NamedService};
use tonic::{Code, Request, Response, Status, Streaming};
use tonic_prost::ProstCodec;
use tower_service::Service;

use super::CATALOG_SERVICE;

/// `shop.v1.GetProductRequest` (the fields the server reads).
#[derive(Clone, PartialEq, prost::Message)]
pub struct GetProductRequest {
    #[prost(string, tag = "1")]
    pub id: String,
}

/// `shop.v1.ListProductsRequest`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ListProductsRequest {
    #[prost(int32, tag = "1")]
    pub page_size: i32,
}

/// `shop.v1.Product` (the fields the server sets).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Product {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(message, optional, tag = "9")]
    pub created_at: Option<prost_types::Timestamp>,
    #[prost(int64, tag = "11")]
    pub views: i64,
}

/// `shop.v1.UploadSummary`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct UploadSummary {
    #[prost(int32, tag = "1")]
    pub count: i32,
}

/// `shop.v1.ChatMessage`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ChatMessage {
    #[prost(string, tag = "1")]
    pub text: String,
}

/// `google.rpc.Status`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct RpcStatus {
    #[prost(int32, tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub message: String,
    #[prost(message, repeated, tag = "3")]
    pub details: Vec<prost_types::Any>,
}

/// `google.rpc.BadRequest`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct BadRequest {
    #[prost(message, repeated, tag = "1")]
    pub field_violations: Vec<FieldViolation>,
}

/// `google.rpc.BadRequest.FieldViolation`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FieldViolation {
    #[prost(string, tag = "1")]
    pub field: String,
    #[prost(string, tag = "2")]
    pub description: String,
}

/// `google.rpc.ErrorInfo`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ErrorInfo {
    #[prost(string, tag = "1")]
    pub reason: String,
    #[prost(string, tag = "2")]
    pub domain: String,
}

/// `created_at` of every product: 2026-11-17T10:00:00Z.
pub const CREATED_AT_SECONDS: i64 = 1_794_909_600;

/// How long `GetProduct` takes for the id `slow`.
pub const SLOW_DELAY: Duration = Duration::from_secs(2);

/// The page size of `ListProducts` that never ends.
pub const ENDLESS: i32 = -1;

/// The session cookie `GetProduct` sets (`set-cookie: sid=...`) for the
/// id `session`, with [`SESSION_TOKEN`] as `authorization` metadata.
pub const SESSION_COOKIE: &str = "sid-canary-7f3a9b21c4e8";

/// The bearer token `GetProduct` answers with for the id `session`.
pub const SESSION_TOKEN: &str = "tok-canary-0d5e62a8f1b3";

/// One call the server received: its method and request metadata, binary
/// values decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedCall {
    pub method: String,
    pub metadata: Vec<(String, Vec<u8>)>,
}

impl ReceivedCall {
    /// The value of the metadata entry `name`, as bytes.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.metadata
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    }

    /// The value of `name` as text.
    pub fn text(&self, name: &str) -> Option<String> {
        self.get(name)
            .map(|v| String::from_utf8_lossy(v).into_owned())
    }
}

/// What the catalog saw.
#[derive(Debug, Default)]
pub struct CatalogLog {
    pub calls: Vec<ReceivedCall>,
    /// Messages received per `UploadProducts` call that reached half-close.
    pub uploads: Vec<usize>,
    /// Endless streams the client cancelled.
    pub cancelled_streams: usize,
}

pub(super) type SharedLog = Arc<Mutex<CatalogLog>>;

pub(super) fn lock(log: &SharedLog) -> std::sync::MutexGuard<'_, CatalogLog> {
    log.lock().unwrap_or_else(PoisonError::into_inner)
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The catalog as a tonic service.
#[derive(Clone)]
pub(super) struct CatalogServer {
    pub log: SharedLog,
}

impl NamedService for CatalogServer {
    const NAME: &'static str = CATALOG_SERVICE;
}

/// A tower service out of a handler function, for `tonic::server::Grpc`.
struct Handler<Req, Res> {
    log: SharedLog,
    run: fn(SharedLog, Req) -> BoxFuture<Result<Res, Status>>,
}

impl<Req, Res> Service<Req> for Handler<Req, Res> {
    type Response = Res;
    type Error = Status;
    type Future = BoxFuture<Result<Res, Status>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Status>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Req) -> Self::Future {
        (self.run)(Arc::clone(&self.log), request)
    }
}

type Unary<Req, Res> = Handler<Request<Req>, Response<Res>>;
type Streamed<Req, Res> = Handler<Request<Streaming<Req>>, Response<Res>>;

impl<B> Service<http::Request<B>> for CatalogServer
where
    B: Body + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Result<Self::Response, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let log = self.log.clone();
        let path = request.uri().path().to_owned();
        Box::pin(async move {
            let response = match path.rsplit('/').next().unwrap_or_default() {
                "GetProduct" => {
                    let run = |log, r| Box::pin(get_product(log, r)) as BoxFuture<_>;
                    let h: Unary<GetProductRequest, Product> = Handler { log, run };
                    Grpc::new(ProstCodec::default()).unary(h, request).await
                }
                "ListProducts" => {
                    let run = |log, r| Box::pin(list_products(log, r)) as BoxFuture<_>;
                    let h: Unary<ListProductsRequest, ProductStream> = Handler { log, run };
                    Grpc::new(ProstCodec::default())
                        .server_streaming(h, request)
                        .await
                }
                "UploadProducts" => {
                    let run = |log, r| Box::pin(upload(log, r)) as BoxFuture<_>;
                    let h: Streamed<Product, UploadSummary> = Handler { log, run };
                    Grpc::new(ProstCodec::default())
                        .client_streaming(h, request)
                        .await
                }
                "Chat" => {
                    let run = |log, r| Box::pin(chat(log, r)) as BoxFuture<_>;
                    let h: Streamed<ChatMessage, ChatStream> = Handler { log, run };
                    Grpc::new(ProstCodec::default()).streaming(h, request).await
                }
                _ => Status::unimplemented(path).into_http(),
            };
            Ok(response)
        })
    }
}

fn record(log: &SharedLog, method: &str, metadata: &MetadataMap) {
    let metadata = metadata
        .iter()
        .map(|entry| match entry {
            KeyAndValueRef::Ascii(k, v) => (k.to_string(), v.as_bytes().to_vec()),
            KeyAndValueRef::Binary(k, v) => (
                k.to_string(),
                v.to_bytes().map(|b| b.to_vec()).unwrap_or_default(),
            ),
        })
        .collect();
    lock(log).calls.push(ReceivedCall {
        method: method.to_owned(),
        metadata,
    });
}

fn product(id: &str) -> Product {
    Product {
        id: id.to_owned(),
        name: format!("Product {id}"),
        created_at: Some(prost_types::Timestamp {
            seconds: CREATED_AT_SECONDS,
            nanos: 0,
        }),
        views: 42,
    }
}

fn any(type_name: &str, value: Vec<u8>) -> prost_types::Any {
    prost_types::Any {
        type_url: format!("type.googleapis.com/{type_name}"),
        value,
    }
}

/// A status carrying `google.rpc.Status` details.
fn rich(code: Code, message: &str, details: Vec<prost_types::Any>) -> Status {
    let status = RpcStatus {
        code: code as i32,
        message: message.to_owned(),
        details,
    };
    Status::with_details(code, message, Bytes::from(status.encode_to_vec()))
}

/// The failures `GetProduct` answers for special ids.
fn failure(id: &str) -> Option<Status> {
    let status = match id {
        "missing" => Status::not_found("product missing not found"),
        "invalid" => {
            let detail = BadRequest {
                field_violations: vec![FieldViolation {
                    field: "id".into(),
                    description: "must start with p-".into(),
                }],
            };
            let detail = any("google.rpc.BadRequest", detail.encode_to_vec());
            rich(Code::InvalidArgument, "invalid product id", vec![detail])
        }
        "quota" => {
            let detail = ErrorInfo {
                reason: "QUOTA_EXCEEDED".into(),
                domain: "shop.example".into(),
            };
            let detail = any("google.rpc.ErrorInfo", detail.encode_to_vec());
            rich(Code::ResourceExhausted, "quota exceeded", vec![detail])
        }
        "mystery" => {
            let detail = prost_types::Any {
                type_url: "type.example.com/acme.Mystery".into(),
                value: vec![1, 2, 3],
            };
            rich(Code::Internal, "something odd", vec![detail])
        }
        _ => return None,
    };
    Some(status)
}

async fn get_product(
    log: SharedLog,
    request: Request<GetProductRequest>,
) -> Result<Response<Product>, Status> {
    record(&log, "GetProduct", request.metadata());
    let id = request.into_inner().id;
    if let Some(status) = failure(&id) {
        return Err(status);
    }
    if id == "slow" {
        tokio::time::sleep(SLOW_DELAY).await;
    }
    let mut response = Response::new(product(&id));
    let metadata = response.metadata_mut();
    metadata.insert("x-served-by", MetadataValue::from_static("catalog"));
    if id == "session" {
        let cookie = format!("sid={SESSION_COOKIE}; Path=/; HttpOnly");
        metadata.insert("set-cookie", cookie.parse().expect("ascii"));
        let token = format!("Bearer {SESSION_TOKEN}");
        metadata.insert("authorization", token.parse().expect("ascii"));
    }
    Ok(response)
}

type ProductStream = ReceiverStream<Result<Product, Status>>;

async fn list_products(
    log: SharedLog,
    request: Request<ListProductsRequest>,
) -> Result<Response<ProductStream>, Status> {
    record(&log, "ListProducts", request.metadata());
    let size = match request.into_inner().page_size {
        0 => 3,
        n => n,
    };
    let (tx, rx) = mpsc::channel(4);
    tokio::spawn(async move {
        if size == ENDLESS {
            return endless(tx, log).await;
        }
        for i in 1..=size {
            if tx.send(Ok(product(&format!("p-{i}")))).await.is_err() {
                return;
            }
        }
    });
    Ok(Response::new(ReceiverStream::new(rx)))
}

/// Products until the client goes away, which is logged as a cancel.
async fn endless(tx: mpsc::Sender<Result<Product, Status>>, log: SharedLog) {
    let mut i = 0u64;
    loop {
        i += 1;
        if tx.send(Ok(product(&format!("p-{i}")))).await.is_err() {
            lock(&log).cancelled_streams += 1;
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn upload(
    log: SharedLog,
    request: Request<Streaming<Product>>,
) -> Result<Response<UploadSummary>, Status> {
    record(&log, "UploadProducts", request.metadata());
    let mut inbound = request.into_inner();
    let mut count = 0;
    while inbound.message().await?.is_some() {
        count += 1;
    }
    lock(&log).uploads.push(count);
    let count = i32::try_from(count).unwrap_or(i32::MAX);
    Ok(Response::new(UploadSummary { count }))
}

type ChatStream = ReceiverStream<Result<ChatMessage, Status>>;

/// Says `welcome`, then echoes every message until the client half-closes.
async fn chat(
    log: SharedLog,
    request: Request<Streaming<ChatMessage>>,
) -> Result<Response<ChatStream>, Status> {
    record(&log, "Chat", request.metadata());
    let mut inbound = request.into_inner();
    let (tx, rx) = mpsc::channel(4);
    tokio::spawn(async move {
        let welcome = ChatMessage {
            text: "welcome".into(),
        };
        if tx.send(Ok(welcome)).await.is_err() {
            return;
        }
        while let Ok(Some(message)) = inbound.message().await {
            let echo = ChatMessage {
                text: format!("echo: {}", message.text),
            };
            if tx.send(Ok(echo)).await.is_err() {
                return;
            }
        }
    });
    Ok(Response::new(ReceiverStream::new(rx)))
}
