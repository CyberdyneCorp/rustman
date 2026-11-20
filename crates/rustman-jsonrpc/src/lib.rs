//! The JSON-RPC 2.0 client for Rustman (add-jsonrpc-client): envelopes
//! with id handling, calls, batches and notifications over HTTP (through
//! `rustman-http`) and WebSocket (the stack shared with the GraphQL
//! client), retries for idempotent calls, and generic subscriptions.
//!
//! The request-file model and its interpolation live in `rustman-core`
//! ([`rustman_core::model::JsonrpcRequest`],
//! [`rustman_core::vars::resolve_jsonrpc`]). Nothing here interprets
//! method names or decodes values: results are kept exactly as received.

pub mod adhoc;
pub mod agent;
pub mod catalog;
pub mod client;
pub mod envelope;
mod error;
pub mod guard;
pub mod http;
pub mod outcome;
pub mod record;
pub mod retry;
pub mod run;
pub mod subscription;
pub mod ws;

pub use adhoc::{AD_HOC, ad_hoc_entry};
pub use catalog::{CATALOG_CACHE_DIR, Catalog, CatalogError, ErrorSpec, MethodSpec, ParamSpec};
pub use client::JsonrpcClient;
pub use envelope::{Envelope, IdSequence, Response, RpcError, classify, request_body};
pub use error::JsonrpcError;
pub use guard::{
    GuardRefusal, MethodGuard, check_target, register_guard, register_preset, unregister_guard,
};
pub use http::{HttpFuture, HttpSend, http_request, send_http};
pub use outcome::{Attempt, BATCH_REJECTED, CallOutcome, CallResult, RpcExchange, TransportKind};
pub use retry::{LIMIT_EXCEEDED, RetryPolicy};
pub use run::{Interactive, JsonrpcRunner};
pub use subscription::{
    MAX_KEPT_EVENTS, SubscribeOptions, SubscriptionEnd, SubscriptionLimits, SubscriptionRecord,
};
pub use ws::{WsConnection, WsPool, send_ws};
