//! How the toolkit talks to a node: one call at a time through [`Rpc`].
//!
//! [`JsonrpcTransport`] sends through `rustman-jsonrpc` (design D1), so
//! method guards, transport settings and redaction apply as for any
//! `jsonrpc` request. Tests replay recorded node answers with
//! `crate::recorded::Recorded` (the `test-fixtures` feature).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rustman_core::Redactor;
use rustman_core::vars::{ResolvedCall, ResolvedJsonrpc, ResolvedRequest};
use rustman_http::SendOptions;
use rustman_jsonrpc::guard::check_target;
use rustman_jsonrpc::outcome::CallOutcome;
use rustman_jsonrpc::{HttpSend, JsonrpcClient};
use serde_json::Value;

use crate::error::EvmError;

/// The JSON-RPC code of an unknown method.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// Why a call returned no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcFailure {
    /// The node answered with an error object.
    Error {
        code: i64,
        message: String,
        /// `error.data`, as hex revert data for reverted calls.
        data: Option<Value>,
    },
    /// No usable answer: network, TLS, timeout, invalid JSON. Redacted.
    Transport(String),
    /// A method guard refused the call; nothing was sent.
    Refused(String),
}

impl RpcFailure {
    /// The node does not implement the method.
    pub fn is_method_not_found(&self) -> bool {
        matches!(self, Self::Error { code, .. } if *code == METHOD_NOT_FOUND)
    }

    /// As an [`EvmError`] for `method`: refusals exit with 4, the rest 3.
    pub fn into_error(self, method: &str) -> EvmError {
        match self {
            Self::Error { code, message, .. } => {
                EvmError::Rpc(format!("{method}: rpc error {code}: {message}"))
            }
            Self::Transport(message) => EvmError::Rpc(format!("{method}: {message}")),
            Self::Refused(message) => EvmError::Refused(message),
        }
    }
}

/// A boxed future of one call. Not `Send`: the JSON-RPC client's
/// WebSocket path is not, so callers drive it on their own task.
pub type RpcFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, RpcFailure>> + 'a>>;

/// Something that answers JSON-RPC calls.
pub trait Rpc: Send + Sync {
    /// The endpoint, redacted, as shown to users and used as a cache key.
    fn endpoint(&self) -> String;

    /// Call `method` with the positional `params`.
    fn call<'a>(&'a self, method: &'a str, params: Value) -> RpcFuture<'a>;
}

/// Call `method` and turn any failure into an [`EvmError`].
pub async fn call(rpc: &dyn Rpc, method: &str, params: Value) -> Result<Value, EvmError> {
    rpc.call(method, params)
        .await
        .map_err(|failure| failure.into_error(method))
}

/// Calls through `rustman-jsonrpc` to one resolved endpoint.
pub struct JsonrpcTransport {
    endpoint: ResolvedRequest,
    sender: Arc<dyn HttpSend + Send + Sync>,
    options: SendOptions,
}

impl JsonrpcTransport {
    /// Calls to `endpoint` (URL, headers, auth and the secrets they use),
    /// sent over HTTP by `sender` or over WebSocket for `ws(s)://` URLs.
    pub fn new(
        endpoint: ResolvedRequest,
        sender: Arc<dyn HttpSend + Send + Sync>,
        options: SendOptions,
    ) -> Self {
        Self {
            endpoint,
            sender,
            options,
        }
    }

    /// The resolved endpoint calls go to.
    pub fn request(&self) -> &ResolvedRequest {
        &self.endpoint
    }

    pub fn sender(&self) -> &(dyn HttpSend + Send + Sync) {
        self.sender.as_ref()
    }

    pub fn options(&self) -> &SendOptions {
        &self.options
    }

    /// Hides the endpoint's secrets.
    pub fn redactor(&self) -> Redactor {
        Redactor::for_request(&self.endpoint)
    }

    async fn send(&self, method: &str, params: Value) -> Result<Value, RpcFailure> {
        let call = ResolvedCall::new(method, Some(params));
        let target = ResolvedJsonrpc::new(self.endpoint.clone(), vec![call], false);
        if let Some(refusal) = check_target(&target) {
            return Err(RpcFailure::Refused(refusal.message));
        }
        let client = JsonrpcClient::new(self.sender.as_ref());
        let exchange = client.send(&target, &self.options).await;
        let redactor = self.redactor();
        if let Some(error) = &exchange.error {
            let shown = redactor.text(&error.to_string()).into_owned();
            let blocked = matches!(
                error,
                rustman_jsonrpc::JsonrpcError::Transport(rustman_http::RequestError::Blocked(_))
            );
            return Err(match blocked {
                true => RpcFailure::Refused(shown),
                false => RpcFailure::Transport(shown),
            });
        }
        match exchange.calls.into_iter().next().map(|c| c.outcome) {
            Some(CallOutcome::Result(value)) => Ok(value),
            Some(CallOutcome::Error(error)) => Err(RpcFailure::Error {
                code: error.code,
                message: redactor.text(&error.message).into_owned(),
                data: error.data,
            }),
            Some(CallOutcome::Failed(reason)) => {
                Err(RpcFailure::Transport(redactor.text(&reason).into_owned()))
            }
            Some(CallOutcome::Notification) | None => {
                Err(RpcFailure::Transport("no response".to_owned()))
            }
        }
    }
}

impl Rpc for JsonrpcTransport {
    fn endpoint(&self) -> String {
        self.redactor().text(&self.endpoint.url.text).into_owned()
    }

    fn call<'a>(&'a self, method: &'a str, params: Value) -> RpcFuture<'a> {
        Box::pin(self.send(method, params))
    }
}
