//! How the toolkit talks to a node: one call at a time through [`Rpc`].
//!
//! [`JsonrpcTransport`] sends through `rustman-jsonrpc` (design D1), so
//! method guards, transport settings and redaction apply as for any
//! `jsonrpc` request. Tests replay recorded node answers with
//! `crate::recorded::Recorded` (the `test-fixtures` feature).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::url_secrets::url_secrets;
use rustman_core::vars::{ResolvedCall, ResolvedJsonrpc, ResolvedRequest};
use rustman_http::SendOptions;
use rustman_jsonrpc::guard::check_target;
use rustman_jsonrpc::outcome::{CallOutcome, RpcExchange};
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

/// A call's answer and, when it went over HTTP, the time on the wire
/// (from sending the request to the last response byte, without any wait
/// for the sender's pacing).
pub type Timed = (Result<Value, RpcFailure>, Option<Duration>);

/// A boxed future of one [`Timed`] call.
pub type TimedFuture<'a> = Pin<Box<dyn Future<Output = Timed> + 'a>>;

/// Something that answers JSON-RPC calls.
pub trait Rpc: Send + Sync {
    /// The endpoint, redacted, as shown to users and used as a cache key.
    fn endpoint(&self) -> String;

    /// Call `method` with the positional `params`.
    fn call<'a>(&'a self, method: &'a str, params: Value) -> RpcFuture<'a>;

    /// [`Rpc::call`], with the time on the wire when the transport knows it.
    fn call_timed<'a>(&'a self, method: &'a str, params: Value) -> TimedFuture<'a> {
        Box::pin(async move { (self.call(method, params).await, None) })
    }
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

    /// Hides the endpoint's secrets, and credentials typed into its URL
    /// (userinfo, key-like query values, long opaque path segments).
    pub fn redactor(&self) -> Redactor {
        let mut redactor = Redactor::for_request(&self.endpoint);
        for key in url_secrets(&self.endpoint.url.text) {
            redactor.add_value("url-key", key);
        }
        redactor
    }

    async fn send(&self, method: &str, params: Value) -> Timed {
        let call = ResolvedCall::new(method, Some(params));
        let target = ResolvedJsonrpc::new(self.endpoint.clone(), vec![call], false);
        if let Some(refusal) = check_target(&target) {
            return (Err(RpcFailure::Refused(refusal.message)), None);
        }
        let client = JsonrpcClient::new(self.sender.as_ref());
        let exchange = client.send(&target, &self.options).await;
        let wire = exchange.http.as_ref().map(|http| http.timings.total);
        (self.answer(exchange), wire)
    }

    fn answer(&self, exchange: RpcExchange) -> Result<Value, RpcFailure> {
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
        Box::pin(async move { self.send(method, params).await.0 })
    }

    fn call_timed<'a>(&'a self, method: &'a str, params: Value) -> TimedFuture<'a> {
        Box::pin(self.send(method, params))
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::model::RequestSettings;
    use rustman_core::vars::{Resolved, ResolvedBody, TlsFiles};
    use rustman_http::HttpEngine;

    use super::*;

    fn shown(url: &str) -> String {
        let endpoint = ResolvedRequest {
            method: "POST".to_owned(),
            url: Resolved::literal(url),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets: Vec::new(),
        };
        let sender = Arc::new(HttpEngine::new());
        JsonrpcTransport::new(endpoint, sender, SendOptions::default()).endpoint()
    }

    #[test]
    fn literal_keys_are_hidden_in_the_shown_endpoint() {
        let url = "https://user:hunter2@node.example/v3/9aa3d95b3bc440fa88ea12eaa4456161?apikey=QKEY456&chain=1";
        let endpoint = shown(url);
        for secret in ["hunter2", "9aa3d95b3bc440fa88ea12eaa4456161", "QKEY456"] {
            assert!(!endpoint.contains(secret), "{endpoint}");
        }
        assert!(endpoint.contains("node.example"), "{endpoint}");
        assert!(endpoint.contains("chain=1"), "{endpoint}");
        assert_eq!(shown("http://127.0.0.1:8545/"), "http://127.0.0.1:8545/");
    }
}
