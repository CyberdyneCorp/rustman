//! The host `http` import's network (plugins spec, "Plugin network
//! access"; design D3): every request a plugin sends goes through the
//! `http-requests` engine under the session's [`SafetyPolicy`] (via
//! [`GovernedSender`]) and is recorded in history like any other exchange.
//!
//! Plugin calls are synchronous; the network runs its own small runtime,
//! so a call can block on a request from any thread, inside a Tokio
//! runtime or not.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use rustman_core::Environment;
use rustman_core::Redactor;
use rustman_core::history::{Entry, HistoryStore, NewEntry};
use rustman_core::model::RequestSettings;
use rustman_core::plugins::PluginNetGuard;
use rustman_core::run::{RequestRecord, RequestStatus, SentRequest};
use rustman_core::safety::{
    EnvironmentInfo, Governor, Limits, Origin, Permission, PolicyOptions, SafetyPolicy,
    SafetySettings,
};
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, TlsFiles};
use rustman_http::address::{is_localhost_name, is_public_ip};
use rustman_http::runner::exchange_record;
use rustman_http::{
    BodyOptions, BodyOverride, Exchange, GovernedError, GovernedSender, HttpEngine, RequestError,
    SendOptions, header_pairs, millis,
};
use tokio::runtime::Runtime;

use crate::bindings::host::rustman::plugin::http::{Request, Response};
use crate::bindings::host::rustman::plugin::types::{Header, HostError};
use crate::grant::{Grant, NetTarget};

/// The policy target of plugin sessions: it names no reachable host, so
/// only the origins of `net:` grants (and `[safety].allowed_hosts`) are
/// allowed.
const NO_TARGET: &str = "https://plugins.invalid";

/// The largest response body handed to a plugin.
pub const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Sends plugin requests under the session's safety policy.
pub struct GovernedNetwork {
    sender: Arc<GovernedSender>,
    options: SendOptions,
    history: Option<HistoryStore>,
    /// Secrets of the session, redacted from what history records.
    redactor: Redactor,
    /// Agent sessions: admits (and audits) each request on top of the
    /// grants and the policy.
    guard: Option<Arc<dyn PluginNetGuard>>,
    runtime: Option<Runtime>,
}

impl std::fmt::Debug for GovernedNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernedNetwork")
            .field("sender", &self.sender)
            .finish_non_exhaustive()
    }
}

impl Drop for GovernedNetwork {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl GovernedNetwork {
    /// A network sending through `sender`. Redirects are never followed
    /// (the plugin sees them, and a new request is checked again), and
    /// bodies are kept in memory up to [`MAX_RESPONSE_BYTES`].
    pub fn new(
        sender: Arc<GovernedSender>,
        mut options: SendOptions,
        history: Option<HistoryStore>,
        redactor: Redactor,
    ) -> std::io::Result<Self> {
        options.follow_redirects = false;
        options.cookies = None;
        options.body = BodyOptions {
            max_in_memory: MAX_RESPONSE_BYTES + 1,
            max_download: MAX_RESPONSE_BYTES,
            max_decoded: MAX_RESPONSE_BYTES,
            ..BodyOptions::default()
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("rustman-plugin-net")
            .enable_all()
            .build()?;
        Ok(Self {
            sender,
            options,
            history,
            redactor,
            guard: None,
            runtime: Some(runtime),
        })
    }

    /// The network with `guard` admitting each request first: an agent
    /// session's allowlist and audit log ([`PluginNetGuard`]).
    #[must_use]
    pub fn with_guard(mut self, guard: Arc<dyn PluginNetGuard>) -> Self {
        self.guard = Some(guard);
        self
    }

    /// The network of a session whose plugins hold `grants`: `None` when
    /// none holds a `net:` grant. Requests may reach the granted hosts (over
    /// https or http, on the granted port, else 443 or 80) and the hosts of
    /// `[safety].allowed_hosts`, under the workspace `[safety]` settings:
    /// production hosts are refused, and so are destructive methods.
    /// `permission` bounds the methods further: agent sessions pass
    /// [`Permission::ReadOnly`], since a plugin call cannot ask the human
    /// to confirm a mutation.
    pub fn for_grants<'g>(
        safety: &SafetySettings,
        permission: Permission,
        environment: Option<&Environment>,
        grants: impl IntoIterator<Item = &'g Grant>,
        history: Option<HistoryStore>,
        redactor: Redactor,
    ) -> Result<Option<Self>, String> {
        let origins = granted_origins(grants)?;
        if origins.is_empty() {
            return Ok(None);
        }
        let options = PolicyOptions {
            target: NO_TARGET,
            permission,
            allow_hosts: &origins,
            production: None,
            environment: environment.map(EnvironmentInfo::from).unwrap_or_default(),
        };
        let policy = SafetyPolicy::new(safety, options).map_err(|e| e.to_string())?;
        let governor = Governor::new(Limits::from_settings(safety));
        let engine = HttpEngine::with_address_guard(Arc::new(check_address));
        let sender = Arc::new(GovernedSender::with_engine(policy, governor, engine));
        Self::new(sender, SendOptions::default(), history, redactor)
            .map(Some)
            .map_err(|e| format!("plugin network: {e}"))
    }

    pub fn policy(&self) -> &SafetyPolicy {
        self.sender.policy()
    }

    /// Send `request` for `plugin`, whose `net:` grants allow its host.
    pub fn fetch(&self, plugin: &str, request: Request) -> Result<Response, HostError> {
        let resolved = resolved(&request);
        let mut redactor = Redactor::for_request(&resolved);
        redactor.merge(&self.redactor);
        if let Some(guard) = &self.guard {
            let url = &resolved.url.text;
            let shown = redactor.text(&without_userinfo(url)).into_owned();
            guard
                .admit(plugin, &resolved.method, url, &shown)
                .map_err(|reason| HostError::PolicyDenied(redactor.text(&reason).into_owned()))?;
        }
        let mut options = self.options.clone();
        if !request.body.is_empty() {
            options.body_override = Some(BodyOverride {
                bytes: Bytes::from(request.body),
                content_type: None,
                content_encoding: None,
            });
        }
        let started = Instant::now();
        let sent = self.block_on(self.sender.send(&resolved, &options));
        match sent {
            Ok(exchange) => {
                redactor.merge(&exchange.credentials);
                self.record(plugin, &resolved, &exchange, &redactor, started);
                Ok(response(&exchange))
            }
            Err(GovernedError::Blocked { reason, .. }) => {
                Err(HostError::PolicyDenied(reason.reason().to_owned()))
            }
            // The address guard refused what the granted name resolved to.
            Err(GovernedError::Request(RequestError::Blocked(reason))) => {
                Err(HostError::PolicyDenied(redactor.text(&reason).into_owned()))
            }
            Err(GovernedError::NotRun(budget)) => Err(HostError::PolicyDenied(budget.to_string())),
            Err(GovernedError::Request(e)) => Err(HostError::Failed(
                redactor.text(&e.to_string()).into_owned(),
            )),
        }
    }

    /// Run `future` on the network's runtime, from a thread outside any
    /// runtime.
    fn block_on<F: std::future::Future + Send>(&self, future: F) -> F::Output
    where
        F::Output: Send,
    {
        let handle = self
            .runtime
            .as_ref()
            .expect("the runtime lives until drop")
            .handle();
        std::thread::scope(|scope| {
            scope
                .spawn(|| handle.block_on(future))
                .join()
                .expect("the plugin network thread does not panic")
        })
    }

    /// Record the exchange in history, redacted; a failed write is logged.
    fn record(
        &self,
        plugin: &str,
        resolved: &ResolvedRequest,
        exchange: &Exchange,
        redactor: &Redactor,
        started: Instant,
    ) {
        let Some(store) = &self.history else {
            return;
        };
        let id = format!("plugin:{plugin}");
        let mut record = RequestRecord::new(id.clone(), id.clone(), id);
        record.status = RequestStatus::Passed;
        record.exchange = Some(exchange_record(resolved, exchange, redactor));
        record.duration_ms = millis(started.elapsed());
        let entry = Entry::new(NewEntry {
            record: &record,
            sent: SentRequest::new(resolved, redactor),
            request: None,
            environment: None,
            spill: None,
            replay: None,
        });
        if let Err(e) = store.record(&entry) {
            tracing::warn!(plugin, "history: {}", redactor.text(&e.to_string()));
        }
    }
}

/// Whether a `net:` grant of `host` names this machine or a private
/// network itself: `localhost`, or a literal private address.
pub fn is_local_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    is_localhost_name(bare) || bare.parse::<IpAddr>().is_ok_and(|ip| !is_public_ip(ip))
}

/// The connect-time check of plugin requests (plugins spec, "Plugin
/// network access"): a granted host name may not resolve to this machine
/// or a private network, unless the grant names it literally (an IP
/// address, which is never resolved, or `localhost`). Checked on the
/// addresses actually connected to, so DNS cannot be re-pointed after it.
pub fn check_address(host: &str, ip: IpAddr) -> Result<(), String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let literal = bare.parse::<IpAddr>().is_ok() || is_localhost_name(bare);
    if literal || is_public_ip(ip) {
        return Ok(());
    }
    Err(format!(
        "{bare} resolves to the private address {ip} (grant the address itself to reach it)"
    ))
}

/// The origins the `net:` grants among `grants` reach.
fn granted_origins<'g>(grants: impl IntoIterator<Item = &'g Grant>) -> Result<Vec<Origin>, String> {
    let mut origins = Vec::new();
    for grant in grants {
        let Grant::Net(target) = grant else {
            continue;
        };
        for origin in target_origins(target) {
            let origin = Origin::parse(&origin)?;
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
    }
    Ok(origins)
}

fn target_origins(target: &NetTarget) -> [String; 2] {
    let host = if target.host.contains(':') {
        format!("[{}]", target.host)
    } else {
        target.host.clone()
    };
    match target.port {
        Some(port) => [
            format!("https://{host}:{port}"),
            format!("http://{host}:{port}"),
        ],
        None => [format!("https://{host}"), format!("http://{host}")],
    }
}

fn resolved(request: &Request) -> ResolvedRequest {
    ResolvedRequest {
        method: request.method.to_ascii_uppercase(),
        url: Resolved::literal(request.url.clone()),
        query: Vec::new(),
        headers: request
            .headers
            .iter()
            .map(|h| ResolvedPair {
                name: Resolved::literal(h.name.clone()),
                value: Resolved::literal(h.value.clone()),
            })
            .collect(),
        body: ResolvedBody::None,
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets: Vec::new(),
    }
}

fn response(exchange: &Exchange) -> Response {
    Response {
        status: exchange.status,
        headers: header_pairs(&exchange.headers)
            .into_iter()
            .map(|(name, value)| Header { name, value })
            .collect(),
        body: exchange.body.to_vec(),
    }
}

/// `url` with any userinfo (`user:password@`) replaced by a marker, for
/// audit lines and history: the redactor only removes known secrets.
fn without_userinfo(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) if !parsed.username().is_empty() || parsed.password().is_some() => {
            let _ = parsed.set_password(None);
            let _ = parsed.set_username("«redacted:userinfo»");
            parsed.to_string()
        }
        _ => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a plugin URL's `user:password@` reached the agent
    /// audit line, because the redactor only knows secret values.
    #[test]
    fn userinfo_is_hidden_in_shown_urls() {
        assert_eq!(
            without_userinfo("http://alice:hunter2pw@localhost:8080/x?k=v"),
            "http://%C2%ABredacted%3Auserinfo%C2%BB@localhost:8080/x?k=v"
        );
        assert_eq!(
            without_userinfo("https://api.example.com/x"),
            "https://api.example.com/x"
        );
        assert_eq!(without_userinfo("not a url"), "not a url");
    }

    fn network(permission: Permission) -> GovernedNetwork {
        let grant = Grant::parse("net:localhost:9").unwrap();
        GovernedNetwork::for_grants(
            &SafetySettings::default(),
            permission,
            None,
            [&grant],
            None,
            Redactor::default(),
        )
        .unwrap()
        .unwrap()
    }

    fn post() -> Request {
        Request {
            method: "post".to_owned(),
            url: "http://localhost:9/orders".to_owned(),
            headers: Vec::new(),
            body: b"{}".to_vec(),
        }
    }

    /// A granted name that resolves to a private address is refused; a
    /// grant naming the address or `localhost` itself still reaches it.
    #[test]
    fn granted_names_may_not_resolve_to_private_addresses() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        let refused = check_address("rebind.example.com", ip("169.254.169.254"));
        assert_eq!(
            refused.unwrap_err(),
            "rebind.example.com resolves to the private address 169.254.169.254 \
             (grant the address itself to reach it)"
        );
        assert!(check_address("api.example.com", ip("10.1.2.3")).is_err());
        assert!(check_address("api.example.com", ip("::1")).is_err());
        assert!(check_address("api.example.com", ip("93.184.216.34")).is_ok());
        assert!(check_address("localhost", ip("127.0.0.1")).is_ok());
        assert!(check_address("LOCALHOST.", ip("::1")).is_ok());
        assert!(check_address("app.localhost", ip("127.0.0.1")).is_ok());
        assert!(check_address("10.1.2.3", ip("10.1.2.3")).is_ok());
        assert!(check_address("[::1]", ip("::1")).is_ok());
    }

    /// Agent sessions build the network read-only: a plugin cannot send a
    /// mutation to its granted host without the human's confirmation.
    #[test]
    fn read_only_networks_refuse_mutations() {
        let refused = network(Permission::ReadOnly).fetch("p", post());
        assert!(
            matches!(&refused, Err(HostError::PolicyDenied(_))),
            "{refused:?}"
        );
        let allowed = network(Permission::Mutations).fetch("p", post());
        assert!(
            !matches!(&allowed, Err(HostError::PolicyDenied(_))),
            "{allowed:?}"
        );
    }
}
