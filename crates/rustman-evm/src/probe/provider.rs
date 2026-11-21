//! One probed provider and its transports. Every HTTP request goes through
//! a [`GovernedSender`] under the probe's [`SafetyPolicy`]: hosts limited
//! to the given providers, 10 req/s and 4 in flight by default.
//! WebSocket calls are checked and paced on the same governor by the
//! [`super::meter::Meter`]. Rate discovery gets its own governor at
//! `--max-rate`, and remembers the `Retry-After` a rate limit sent.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use rustman_core::model::EndpointScheme;
use rustman_core::safety::{
    Admission, EnvironmentInfo, Governor, Limits, Permission, PolicyError, PolicyOptions,
    SafetyPolicy, SafetySettings, TOO_MANY_REQUESTS, parse_retry_after,
};
use rustman_core::vars::ResolvedRequest;
use rustman_http::{GovernedSender, SendOptions};
use rustman_jsonrpc::agent::{Governed, check_host};
use rustman_jsonrpc::{HttpFuture, HttpSend};

use crate::rpc::{JsonrpcTransport, Rpc};

/// A provider to probe.
pub struct Provider {
    /// The endpoint, redacted, as reports show it.
    pub endpoint: String,
    /// The environment it was named by.
    pub environment: Option<String>,
    /// The environment's `chain_id`, when it pins one.
    pub pin: Option<u64>,
    /// Probe calls.
    pub rpc: Box<dyn Rpc>,
    /// The transport of the subscription check (`ws://` and `wss://`).
    pub stream: Option<JsonrpcTransport>,
    /// Checks and paces WebSocket calls (HTTP calls are governed by
    /// their sender).
    pub ws_gate: Option<WsGate>,
    /// Rate discovery's transport.
    pub rate: Option<RateTransport>,
}

impl Provider {
    /// A provider answering through `rpc` alone (no subscription check,
    /// no rate discovery, no pacing), for tests and recorded answers.
    pub fn plain(endpoint: &str, rpc: Box<dyn Rpc>) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            environment: None,
            pin: None,
            rpc,
            stream: None,
            ws_gate: None,
            rate: None,
        }
    }

    /// Check and pace a WebSocket call on the provider's governor. The
    /// call holds the returned concurrency slot (if any) while it runs.
    pub(crate) async fn gate_ws(&self) -> Result<Option<Admission>, String> {
        match &self.ws_gate {
            Some(gate) => gate.pass().await.map(Some),
            None => Ok(None),
        }
    }
}

/// The safety policy and governor of a WebSocket provider's calls.
pub struct WsGate {
    sender: Arc<GovernedSender>,
    /// The endpoint as resolved (never shown).
    url: String,
}

impl WsGate {
    /// The policy's origin rules (as for the `http` or `https` origin the
    /// socket upgrades from), then the governor's budgets and pace. The
    /// admission is the call's concurrency slot.
    async fn pass(&self) -> Result<Admission, String> {
        let policy = self.sender.policy();
        check_host(&self.url, policy.allowed_origins()).map_err(|b| b.to_string())?;
        policy
            .check_origin(&http_form(&self.url))
            .map_err(|b| b.to_string())?;
        let governor = self.sender.governor();
        let admission = governor.admit().await.map_err(|e| e.to_string())?;
        governor.pace().await.map_err(|e| e.to_string())?;
        Ok(admission)
    }
}

/// `url` with `ws` and `wss` read as the `http` and `https` they upgrade
/// from.
pub fn http_form(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        url.to_owned()
    }
}

/// The safety policy of one provider's calls: its own origin only (a
/// WebSocket endpoint as the `http` or `https` origin it upgrades from),
/// `[safety]`'s production guard, and POST allowed, since JSON-RPC is
/// POST-only; the read-only method allowlist decides what is sent.
pub fn policy(
    url: &str,
    settings: &SafetySettings,
    production: Option<&str>,
    environment: EnvironmentInfo<'_>,
) -> Result<SafetyPolicy, PolicyError> {
    let target = http_form(url);
    SafetyPolicy::new(
        settings,
        PolicyOptions {
            target: &target,
            permission: Permission::Mutations,
            allow_hosts: &[],
            production,
            environment,
        },
    )
}

/// What a governed provider is built from.
pub struct ProviderSpec {
    pub endpoint: ResolvedRequest,
    pub options: SendOptions,
    pub environment: Option<String>,
    pub pin: Option<u64>,
    /// Hosts limited to the probed providers.
    pub policy: SafetyPolicy,
    /// Rate, concurrency and budgets of the probe calls.
    pub limits: Limits,
    /// `--max-rate`: the governor rate of rate discovery.
    pub max_rate: std::num::NonZeroU32,
}

/// A provider whose calls pass `spec.policy` and its governor.
pub fn governed(spec: ProviderSpec) -> Provider {
    let sender = Arc::new(GovernedSender::new(
        spec.policy.clone(),
        Governor::new(spec.limits),
    ));
    let http: Arc<dyn HttpSend + Send + Sync> = Arc::new(Governed(sender.clone()));
    let spec_url = spec.endpoint.url.text.clone();
    let ws = EndpointScheme::of(&spec_url) == Some(EndpointScheme::WebSocket);
    let stream = ws
        .then(|| JsonrpcTransport::new(spec.endpoint.clone(), http.clone(), spec.options.clone()));
    let rpc = JsonrpcTransport::new(spec.endpoint.clone(), http, spec.options.clone());
    let endpoint = Rpc::endpoint(&rpc);
    let rate_limits = Limits {
        rate: spec.max_rate,
        concurrency: std::num::NonZeroU32::MIN,
        ..spec.limits
    };
    let rate_sender = GovernedSender::new(spec.policy, Governor::new(rate_limits));
    let watch = Arc::new(Watched::new(Arc::new(Governed(Arc::new(rate_sender)))));
    let rate = RateTransport {
        rpc: JsonrpcTransport::new(spec.endpoint, watch.clone(), spec.options),
        watch,
    };
    Provider {
        endpoint,
        environment: spec.environment,
        pin: spec.pin,
        stream,
        rpc: Box::new(rpc),
        ws_gate: ws.then(|| WsGate {
            sender,
            url: spec_url,
        }),
        rate: Some(rate),
    }
}

/// Rate discovery's calls, and the `Retry-After` they saw.
pub struct RateTransport {
    pub rpc: JsonrpcTransport,
    pub watch: Arc<Watched>,
}

/// An HTTP sender that remembers the `Retry-After` of the last `429`.
pub struct Watched {
    inner: Arc<dyn HttpSend + Send + Sync>,
    retry_after: Mutex<Option<Duration>>,
}

impl Watched {
    pub fn new(inner: Arc<dyn HttpSend + Send + Sync>) -> Self {
        Self {
            inner,
            retry_after: Mutex::new(None),
        }
    }

    /// The `Retry-After` of the last `429`, taken.
    pub fn take_retry_after(&self) -> Option<Duration> {
        self.retry_after
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl HttpSend for Watched {
    fn send_http<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> HttpFuture<'a> {
        Box::pin(async move {
            let exchange = self.inner.send_http(request, options).await?;
            if exchange.status == TOO_MANY_REQUESTS {
                let after = exchange
                    .headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| parse_retry_after(v, SystemTime::now()));
                *self
                    .retry_after
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = after;
            }
            Ok(exchange)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    /// A WebSocket gate whose governor lets one call run at a time.
    fn one_at_a_time() -> WsGate {
        let url = "ws://127.0.0.1:1/";
        let policy = policy(url, &SafetySettings::default(), None, Default::default()).unwrap();
        let limits = Limits {
            rate: NonZeroU32::new(1000).unwrap(),
            concurrency: NonZeroU32::MIN,
            ..Limits::default()
        };
        WsGate {
            sender: Arc::new(GovernedSender::new(policy, Governor::new(limits))),
            url: url.to_owned(),
        }
    }

    #[tokio::test]
    async fn a_websocket_call_holds_its_concurrency_slot() {
        let gate = one_at_a_time();
        let slot = gate.pass().await.unwrap();
        let second = tokio::time::timeout(Duration::from_millis(300), gate.pass()).await;
        assert!(
            second.is_err(),
            "a second call ran while the first held the slot"
        );
        drop(slot);
        let third = tokio::time::timeout(Duration::from_secs(10), gate.pass()).await;
        assert!(third.unwrap().is_ok());
    }

    #[test]
    fn websocket_urls_read_as_their_http_origin() {
        assert_eq!(http_form("wss://a/x"), "https://a/x");
        assert_eq!(http_form("ws://a"), "http://a");
        assert_eq!(http_form("http://a"), "http://a");
    }
}
