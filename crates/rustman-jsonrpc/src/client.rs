//! Sending a resolved `jsonrpc` request over the transport its endpoint
//! selects: HTTP(S) through `rustman-http`, or WebSocket.

use std::time::Instant;

use rustman_core::model::EndpointScheme;
use rustman_core::vars::ResolvedJsonrpc;
use rustman_http::SendOptions;

use crate::error::JsonrpcError;
use crate::http::{HttpSend, send_http};
use crate::outcome::{RpcExchange, TransportKind};
use crate::subscription::SubscribeOptions;
use crate::ws::{WsPool, send_ws};

/// What a send needs besides the request: the HTTP sender, the run's
/// WebSocket pool (none closes each connection after its exchange), and
/// how subscriptions run.
pub struct JsonrpcClient<'a> {
    pub http: &'a dyn HttpSend,
    pub pool: Option<&'a WsPool>,
    pub subscribe: SubscribeOptions,
}

impl<'a> JsonrpcClient<'a> {
    /// A client sending HTTP through `http`, with no pool and headless
    /// subscription bounds.
    pub fn new(http: &'a dyn HttpSend) -> Self {
        Self {
            http,
            pool: None,
            subscribe: SubscribeOptions::default(),
        }
    }

    #[must_use]
    pub fn with_pool(mut self, pool: &'a WsPool) -> Self {
        self.pool = Some(pool);
        self
    }

    #[must_use]
    pub fn with_subscribe(mut self, subscribe: SubscribeOptions) -> Self {
        self.subscribe = subscribe;
        self
    }

    /// Send `target` with `options`. The exchange always comes back; its
    /// `error` says why no usable answer did.
    pub async fn send(&self, target: &ResolvedJsonrpc, options: &SendOptions) -> RpcExchange {
        let scheme = EndpointScheme::of(&target.http.url.text);
        match (scheme, &target.subscription) {
            (Some(EndpointScheme::WebSocket), _) => {
                send_ws(options, target, self.pool, &self.subscribe).await
            }
            (_, Some(_)) => unsupported(
                target,
                "subscriptions require a ws:// or wss:// endpoint".to_owned(),
            ),
            (Some(EndpointScheme::Http), None) => {
                send_http(self.http, options, target, &self.subscribe.cancel).await
            }
            (None, None) => {
                let shown = target.redactor().text(&target.http.url.text).into_owned();
                unsupported(
                    target,
                    format!("endpoint must be an http, https, ws or wss URL: '{shown}'"),
                )
            }
        }
    }
}

fn unsupported(target: &ResolvedJsonrpc, message: String) -> RpcExchange {
    let started = Instant::now();
    let mut exchange = RpcExchange::new(TransportKind::Http, target.batch);
    exchange.error = Some(JsonrpcError::Unsupported(message));
    exchange.elapsed = started.elapsed();
    exchange
}
