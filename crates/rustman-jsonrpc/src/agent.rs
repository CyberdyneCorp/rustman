//! What the agent `rpc_call` tool may send (spec "Agent read-only tool",
//! "Allowlist presets"; design D7).
//!
//! JSON-RPC methods are not classified as reads or writes, so an agent may
//! only call methods matching `[jsonrpc] agent_read_only_methods` (empty by
//! default: nothing). Every call is checked in this order, and nothing is
//! sent unless all pass:
//!
//! 1. the method guards ([`crate::guard`]), which win over everything;
//! 2. the safety policy: the endpoint's origin must be allowlisted;
//! 3. subscriptions are refused (an agent call must end by itself);
//! 4. every method must match a read-only glob.
//!
//! The MCP server registers the tool and hands the runner an
//! [`AgentRunner`]: it checks each call this way, then sends it through
//! [`crate::JsonrpcRunner`] (which asks the guards again) with the
//! session's [`GovernedSender`] as the HTTP transport, so every POST and
//! redirect hop also passes the safety policy and its rate limits. A
//! WebSocket call does not go through that transport: the runner admits
//! and paces it on the same governor before sending it.

use std::sync::{Arc, Mutex, PoisonError};

use rustman_core::model::EndpointScheme;
use rustman_core::run::glob_matches;
use rustman_core::safety::{Admission, Blocked, Origin};
use rustman_core::settings::RpcSettings;
use rustman_core::vars::{ResolvedJsonrpc, ResolvedRequest};
use rustman_http::runner::{JsonrpcCall, JsonrpcExecutor, JsonrpcFuture};
use rustman_http::{GovernedError, GovernedSender, RequestError, SendOptions};

use crate::guard::{self, GuardRefusal};
use crate::http::{HttpFuture, HttpSend};
use crate::run::JsonrpcRunner;

/// The name the tool is registered under.
pub const TOOL_NAME: &str = "rpc_call";

/// The message of a method outside `agent_read_only_methods`.
pub const NOT_READ_ONLY: &str = "method not marked read-only for agents";

/// The message of a subscription asked for by an agent.
pub const NO_SUBSCRIPTIONS: &str = "subscriptions are not available to agents";

/// An `@preset` entry no capability registered: a configuration error
/// (exit code 2) when the agent server starts.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown method preset '@{name}' (registered: {registered})")]
pub struct UnknownPreset {
    pub name: String,
    /// The registered presets, comma-separated, or `none`.
    pub registered: String,
}

impl UnknownPreset {
    /// The exit code of a configuration error.
    pub const EXIT_CODE: u8 = 2;
}

/// Why the agent tool refused a call; nothing was sent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentRefusal {
    /// A method guard refused a method: its own message.
    #[error(transparent)]
    Guard(#[from] GuardRefusal),
    /// The safety policy refused the endpoint.
    #[error(transparent)]
    Policy(#[from] Blocked),
    #[error("{NO_SUBSCRIPTIONS}")]
    Subscription,
    /// The method matches no read-only glob.
    #[error("{NOT_READ_ONLY}")]
    NotReadOnly { method: String },
}

impl AgentRefusal {
    /// Every refusal is a safety refusal: exit code 4.
    pub const EXIT_CODE: u8 = 4;
}

/// The read-only method globs of a workspace, with presets expanded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentRpcPolicy {
    globs: Vec<String>,
}

impl AgentRpcPolicy {
    /// The policy of `[jsonrpc]`; an `@preset` nobody registered fails.
    pub fn from_settings(settings: &RpcSettings) -> Result<Self, UnknownPreset> {
        let mut globs = Vec::new();
        for entry in &settings.agent_read_only_methods {
            match entry.strip_prefix('@') {
                Some(name) => globs.extend(guard::preset(name).ok_or_else(|| unknown(name))?),
                None => globs.push(entry.clone()),
            }
        }
        Ok(Self { globs })
    }

    /// The method globs, presets expanded.
    pub fn globs(&self) -> &[String] {
        &self.globs
    }

    /// Whether `method` matches a read-only glob.
    pub fn is_read_only(&self, method: &str) -> bool {
        self.globs.iter().any(|glob| glob_matches(glob, method))
    }

    /// Whether an agent may send `target`, whose endpoint must be one of
    /// `allowed` origins.
    pub fn check(&self, target: &ResolvedJsonrpc, allowed: &[Origin]) -> Result<(), AgentRefusal> {
        if let Some(refusal) = guard::check_target(target) {
            return Err(refusal.into());
        }
        check_host(&target.http.url.text, allowed)?;
        if target.subscription.is_some() {
            return Err(AgentRefusal::Subscription);
        }
        match target.calls.iter().find(|c| !self.is_read_only(&c.method)) {
            Some(call) => Err(AgentRefusal::NotReadOnly {
                method: call.method.clone(),
            }),
            None => Ok(()),
        }
    }
}

/// The safety policy's origin rule for an endpoint (`ws` and `wss` are
/// checked as the `http` and `https` origins they upgrade from).
pub fn check_host(endpoint: &str, allowed: &[Origin]) -> Result<(), Blocked> {
    let origin = Origin::of(&http_form(endpoint)).map_err(|_| Blocked::HostNotAllowed)?;
    if allowed.contains(&origin) {
        Ok(())
    } else {
        Err(Blocked::HostNotAllowed)
    }
}

/// `endpoint` with `ws` and `wss` read as the `http` and `https` they
/// upgrade from.
fn http_form(endpoint: &str) -> String {
    if let Some(rest) = endpoint.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = endpoint.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        endpoint.to_owned()
    }
}

/// A [`GovernedSender`] as the HTTP transport: the policy checks each
/// POST and redirect hop, and the governor paces them.
pub struct Governed(pub Arc<GovernedSender>);

impl HttpSend for Governed {
    fn send_http<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> HttpFuture<'a> {
        Box::pin(async move {
            self.0.send(request, options).await.map_err(|e| match e {
                GovernedError::Blocked { reason, .. } => {
                    RequestError::Blocked(reason.reason().to_owned())
                }
                GovernedError::Request(e) => e,
                other => RequestError::Network(other.to_string()),
            })
        })
    }
}

/// Sends the calls of the agent tool: [`AgentRpcPolicy::check`] against
/// the sender's allowed origins (and the policy's production rule, which
/// WebSocket endpoints otherwise skip), then the [`JsonrpcRunner`] over
/// [`Governed`]. Subscriptions never get this far. The first refusal is
/// kept for the tool's error.
pub struct AgentRunner {
    policy: AgentRpcPolicy,
    sender: Arc<GovernedSender>,
    runner: JsonrpcRunner,
    refused: Mutex<Option<AgentRefusal>>,
}

impl AgentRunner {
    pub fn new(policy: AgentRpcPolicy, sender: Arc<GovernedSender>) -> Self {
        let runner = JsonrpcRunner {
            http: Some(Arc::new(Governed(sender.clone()))),
            ..JsonrpcRunner::default()
        };
        Self {
            policy,
            sender,
            runner,
            refused: Mutex::new(None),
        }
    }

    /// The first call this runner refused; nothing of it was sent.
    pub fn refused(&self) -> Option<AgentRefusal> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Close the WebSocket connections the calls opened.
    pub async fn close(&self) {
        self.runner.close().await;
    }

    fn admit(&self, target: &ResolvedJsonrpc) -> Result<(), AgentRefusal> {
        let policy = self.sender.policy();
        self.policy.check(target, policy.allowed_origins())?;
        policy.check_origin(&http_form(&target.http.url.text))?;
        Ok(())
    }

    /// Admit and pace a WebSocket call on the session's governor, as
    /// [`Governed`] does for HTTP: the slot is held until the call ends.
    async fn govern_ws(&self, target: &ResolvedJsonrpc) -> Result<Option<Admission>, String> {
        if EndpointScheme::of(&target.http.url.text) != Some(EndpointScheme::WebSocket) {
            return Ok(None);
        }
        let governor = self.sender.governor();
        let admission = governor.admit().await.map_err(|e| e.to_string())?;
        governor.pace().await.map_err(|e| e.to_string())?;
        Ok(Some(admission))
    }
}

impl JsonrpcExecutor for AgentRunner {
    fn execute<'a>(&'a self, call: JsonrpcCall<'a>) -> JsonrpcFuture<'a> {
        Box::pin(async move {
            if let Err(refusal) = self.admit(call.resolved) {
                let message = refusal.to_string();
                let mut refused = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
                refused.get_or_insert(refusal);
                return Err(message);
            }
            let _slot = self.govern_ws(call.resolved).await?;
            self.runner.execute(call).await
        })
    }
}

fn unknown(name: &str) -> UnknownPreset {
    let names = guard::preset_names();
    UnknownPreset {
        name: name.to_owned(),
        registered: if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        },
    }
}
