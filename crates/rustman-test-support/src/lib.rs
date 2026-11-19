//! The in-process test server (design D14) and helpers shared by the
//! integration tests of every Rustman crate. Nothing here touches the network
//! beyond the loopback interface. Never published.

pub mod bad_api;
pub mod certs;
pub mod digest;
pub mod graphql;
pub mod graphql_http;
pub mod graphql_sse;
pub mod graphql_ws;
pub mod jsonrpc;
pub mod mcp_http;
pub mod mcp_oauth;
pub mod oauth;
pub mod openapi;
pub mod proxy;
pub mod raw;
pub mod routes;
pub mod server;

pub use bad_api::BadApi;
pub use certs::TestCerts;
pub use digest::DigestServer;
pub use graphql::{GraphqlServer, Introspection};
pub use graphql_http::{GraphqlHttpServer, RecordedRequest, Reply};
pub use graphql_sse::{GraphqlSseServer, SseEnd, SseScript};
pub use graphql_ws::{GraphqlWsServer, WsScript, WsSession};
pub use jsonrpc::{JsonrpcHttpServer, JsonrpcWsServer, RpcReply, RpcWsSession, WsFrame};
pub use mcp_http::McpHttpServer;
pub use oauth::OAuthServer;
pub use proxy::TestProxy;
pub use server::{TestServer, Tls};
