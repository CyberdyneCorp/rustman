//! The bench under the safety policy (load-testing "Target is mandatory and
//! pins the allowlist", "Method gating", "Production guard").
//!
//! - The allowlist is exactly the `--target` origin: `[safety].allowed_hosts`
//!   is ignored and there is no `--allow-host`.
//! - Before any load, every request is resolved with the environment, the
//!   `--var` overrides and the first data row; one whose method or origin the
//!   policy refuses stops the bench (exit 4). A URL that depends on a
//!   capture can't be known statically and is checked when it is sent.
//! - A proxy (the request's `settings.proxy`, `[http] proxy` or the
//!   `HTTP(S)_PROXY` variables) is where the load actually goes, so it must
//!   be the target origin too; `proxy: none` or `NO_PROXY` bypasses it.
//! - At runtime every request goes through a [`GovernedSender`] (as the
//!   runner's [`SendGate`]): a request or redirect hop the policy refuses,
//!   or one routed through a proxy outside it, is not sent and counts as a
//!   `blocked` error.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use rustman_core::Redactor;
use rustman_core::data_rows::RowSource;
use rustman_core::model::{Request, Variables};
use rustman_core::run::{DataRun, Plan};
use rustman_core::safety::{
    Blocked, EnvironmentInfo, Permission, PolicyError, PolicyOptions, SafetyPolicy, SafetySettings,
};
use rustman_core::vars::{Layer, ResolvedRequest, Resolver, VariablesSource, resolve_request};
use rustman_core::workspace::RequestRef;
use rustman_http::runner::{RunContext, SendGate};
use rustman_http::{
    Exchange, GovernedError, GovernedSender, ProxySettings, ProxyUrl, RequestError, SendOptions,
};

use crate::error::BenchError;

/// What the user allowed on the command line.
#[derive(Clone, Copy, Debug)]
pub struct Safety<'a> {
    /// `--target`: the only origin requests may go to.
    pub target: &'a str,
    /// From `--allow-mutations` and `--allow-destructive`.
    pub permission: Permission,
    /// `--production <host>`.
    pub production: Option<&'a str>,
}

/// The bench's policy: `[safety]` production hosts, but no allowed host
/// besides the target.
pub fn policy(
    settings: &SafetySettings,
    safety: Safety<'_>,
    environment: EnvironmentInfo<'_>,
) -> Result<SafetyPolicy, BenchError> {
    if let Some(problem) = target_problem(safety.target) {
        return Err(BenchError::Usage(format!("bench: --target {problem}")));
    }
    let pinned = SafetySettings {
        allowed_hosts: Vec::new(),
        ..settings.clone()
    };
    let options = PolicyOptions {
        target: safety.target,
        permission: safety.permission,
        allow_hosts: &[],
        production: safety.production,
        environment,
    };
    SafetyPolicy::new(&pinned, options).map_err(policy_error)
}

/// Why `target` is not a bare origin, or `None` when it is (or does not
/// parse, which the policy reports). The target is shown in the report
/// and the dashboards, so credentials (userinfo) and anything past the
/// origin (a path, a query such as `?token=`, a fragment) are refused, and
/// the message never echoes them.
fn target_problem(target: &str) -> Option<&'static str> {
    let url = url::Url::parse(target.trim()).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return Some("must not hold credentials (user:password@); put them in the request's auth");
    }
    let bare = matches!(url.path(), "" | "/") && url.query().is_none() && url.fragment().is_none();
    (!bare).then_some("must be an origin (scheme://host[:port]) without a path, query or fragment")
}

fn policy_error(err: PolicyError) -> BenchError {
    if err.is_refusal() {
        BenchError::Refused(err.to_string())
    } else {
        BenchError::Usage(format!("bench: --target: {err}"))
    }
}

/// What a static check resolves requests with.
pub struct Preflight<'a> {
    pub ctx: RunContext<'a>,
    pub overrides: &'a Variables,
    pub data: Option<&'a DataRun>,
}

/// Refuse the bench before any load when a request of `plan` is not an
/// HTTP request, or the policy refuses its method or (when it resolves
/// without captures) its origin.
pub fn preflight(
    plan: &Plan<'_>,
    policy: &SafetyPolicy,
    with: &Preflight<'_>,
) -> Result<(), BenchError> {
    for request in &plan.requests {
        let Request::Http(http) = request.request() else {
            return Err(BenchError::Usage(format!(
                "bench: {} is a {} request; bench runs http requests only",
                file_of(request),
                request.request().kind()
            )));
        };
        let method = http.method.as_ref().map_or("GET", |m| m.as_str());
        if let Err(reason) = policy.check_method(method) {
            return Err(refused(request, reason, method));
        }
        if let Some(resolved) = resolve(request, with) {
            check_origin(request, policy, &resolved)?;
            check_static_proxy(request, policy, &resolved, with.ctx.settings)?;
        }
    }
    Ok(())
}

/// `request` resolved without captures, or `None` when it needs one.
fn resolve(request: &RequestRef<'_>, with: &Preflight<'_>) -> Option<ResolvedRequest> {
    let ctx = &with.ctx;
    let mut resolver = Resolver::for_request(ctx.settings, request, ctx.environment, ctx.secrets);
    resolver.push(VariablesSource::new(Layer::Override, with.overrides));
    if let Some(data) = with.data
        && let Some(row) = data.row(1)
    {
        resolver.push(RowSource::new(row, &data.secret));
    }
    resolve_request(request, &resolver).ok()
}

fn check_origin(
    request: &RequestRef<'_>,
    policy: &SafetyPolicy,
    resolved: &ResolvedRequest,
) -> Result<(), BenchError> {
    let url = &resolved.url.text;
    policy.check_origin(url).map_err(|reason| {
        let origin = Redactor::for_request(resolved)
            .text(&host_of(url))
            .into_owned();
        refused(request, reason, &origin)
    })
}

/// Refuse a request whose proxy, chosen as the engine chooses it, is not
/// the target origin.
fn check_static_proxy(
    request: &RequestRef<'_>,
    policy: &SafetyPolicy,
    resolved: &ResolvedRequest,
    settings: &rustman_core::settings::Settings,
) -> Result<(), BenchError> {
    let proxies = ProxySettings::select(
        resolved.settings.proxy.as_deref(),
        settings.http.proxy.as_deref(),
        |name| std::env::var(name).ok(),
    );
    // An invalid proxy fails the request itself, as it would in a run.
    let Ok(proxies) = proxies else {
        return Ok(());
    };
    match outside_proxy(policy, &proxies, &resolved.url.text) {
        Some(proxy) => Err(BenchError::Refused(format!(
            "{}: blocked: proxy not allowed ({}); the load would go to the proxy, \
             not --target (set the request's proxy to none, or bench the proxy itself)",
            file_of(request),
            proxy.host
        ))),
        None => Ok(()),
    }
}

/// The proxy a request to `url` goes through when it is outside the
/// policy's allowlist.
pub fn outside_proxy<'p>(
    policy: &SafetyPolicy,
    proxies: &'p ProxySettings,
    url: &str,
) -> Option<&'p ProxyUrl> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let proxy = proxies.route(parsed.scheme() == "https", host)?;
    policy
        .check_origin(&proxy.to_string())
        .is_err()
        .then_some(proxy)
}

fn refused(request: &RequestRef<'_>, reason: Blocked, detail: &str) -> BenchError {
    BenchError::Refused(format!("{}: {reason} ({detail})", file_of(request)))
}

fn file_of(request: &RequestRef<'_>) -> String {
    request.entry.path.to_string_lossy().replace('\\', "/")
}

/// The host of `url` as a refusal names it (`blocked: host not allowed
/// (other.example.com)`, like the proxy refusal): the bare host, with the
/// port only when it is not the scheme's default, so the target's host on
/// another port is told apart. The host is kept as written, not as URL
/// parsing normalizes it (lowercased), so the redactor still finds a secret
/// in it. The URL itself when it has no host.
fn host_of(url: &str) -> String {
    let Some(port) = url::Url::parse(url)
        .ok()
        .filter(|u| u.host_str().is_some())
        .map(|u| u.port())
    else {
        return url.to_owned();
    };
    let host = written_host(url);
    match port {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

/// The host of `url` as written: its authority without the userinfo and
/// the port.
fn written_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.starts_with('[') {
        return authority
            .find(']')
            .map_or(authority, |at| &authority[..=at]);
    }
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
}

/// The [`SendGate`] of one VU: its own [`GovernedSender`] (its own
/// connection pool) sharing the bench's governor and policy. A refused
/// request becomes `blocked: <reason> (<origin>)`.
pub struct BenchGate {
    sender: GovernedSender,
    /// Requests that waited for the `max_rps` limit before going out.
    held: AtomicU64,
    /// How long a request must wait in the gate to count as held.
    held_after: Duration,
}

/// The shortest wait that counts as held: the rest of the gate (policy
/// checks, a free concurrency slot) takes microseconds.
const MIN_HELD: Duration = Duration::from_millis(1);

impl BenchGate {
    pub fn new(sender: GovernedSender) -> Self {
        let held_after = held_after(sender.governor().limits().rate);
        Self {
            sender,
            held: AtomicU64::new(0),
            held_after,
        }
    }

    /// How many of this VU's requests waited for the `max_rps` limit.
    pub fn held(&self) -> u64 {
        self.held.load(Ordering::Relaxed)
    }

    /// How many `429` responses made this VU's sender back off.
    pub fn backoffs(&self) -> u64 {
        self.sender.backoffs()
    }
}

impl SendGate for BenchGate {
    fn send<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> BoxFuture<'a, Result<Exchange, RequestError>> {
        Box::pin(async move {
            let policy = self.sender.policy();
            if let Some(proxy) = outside_proxy(policy, &options.proxy, &request.url.text) {
                return Err(RequestError::Blocked(format!(
                    "proxy not allowed ({})",
                    proxy.host
                )));
            }
            let started = Instant::now();
            let exchange = self.sender.send(request, options).await;
            if let Ok(exchange) = &exchange
                && started.elapsed().saturating_sub(exchange.timings.total) >= self.held_after
            {
                self.held.fetch_add(1, Ordering::Relaxed);
            }
            exchange.map_err(gate_error)
        })
    }
}

/// A request held for the rate limit waited for a meaningful part of its
/// interval: half of it (`1 / (2 * rate)`), and at least [`MIN_HELD`]. A
/// request that just missed its slot by a timer tick is not counted.
fn held_after(rate: std::num::NonZeroU32) -> Duration {
    (Duration::from_secs(1) / rate.get() / 2).max(MIN_HELD)
}

fn gate_error(err: GovernedError) -> RequestError {
    match err {
        GovernedError::Blocked { reason, url } => {
            RequestError::Blocked(format!("{} ({})", reason.reason(), host_of(&url)))
        }
        GovernedError::NotRun(budget) => RequestError::Blocked(budget.detail().to_owned()),
        GovernedError::Request(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::safety::Origin;

    #[test]
    fn allowed_hosts_of_the_settings_are_ignored() {
        let settings = SafetySettings {
            allowed_hosts: vec![Origin::parse("https://other.example.com").unwrap()],
            ..SafetySettings::default()
        };
        let safety = Safety {
            target: "https://staging.example.com",
            permission: Permission::ReadOnly,
            production: None,
        };
        let policy = policy(&settings, safety, EnvironmentInfo::default()).unwrap();
        assert_eq!(policy.allowed_origins().len(), 1);
        assert_eq!(
            policy.check_origin("https://other.example.com/x"),
            Err(Blocked::HostNotAllowed)
        );
        assert!(policy.check_origin("https://staging.example.com/x").is_ok());
    }

    #[test]
    fn a_bad_target_is_a_usage_error_and_production_a_refusal() {
        let bad = Safety {
            target: "not a url",
            permission: Permission::ReadOnly,
            production: None,
        };
        let err = policy(&SafetySettings::default(), bad, EnvironmentInfo::default()).unwrap_err();
        assert!(!err.is_refusal(), "{err}");
        let prod = Safety {
            target: "https://api.example.com",
            ..bad
        };
        let env = EnvironmentInfo {
            name: Some("prod"),
            marked_production: false,
        };
        let err = policy(&SafetySettings::default(), prod, env).unwrap_err();
        assert!(err.is_refusal());
        assert!(
            err.to_string().contains("--production api.example.com"),
            "{err}"
        );
    }

    /// The target's host on another port: the port tells them apart.
    #[test]
    fn refused_requests_name_the_host_and_port() {
        let err = gate_error(GovernedError::Blocked {
            reason: Blocked::HostNotAllowed,
            url: "http://127.0.0.1:18931/x".into(),
        });
        assert_eq!(
            err.to_string(),
            "blocked: host not allowed (127.0.0.1:18931)"
        );
        assert_eq!(host_of("not a url"), "not a url");
        assert_eq!(
            host_of("https://Secret-Host.example.com:443/x"),
            "Secret-Host.example.com"
        );
        assert_eq!(host_of("http://u:p@[::1]:8080/x"), "[::1]:8080");
        assert_eq!(host_of("http://[::1]/x"), "[::1]");
    }

    #[test]
    fn refused_requests_name_a_bare_host_on_its_default_port() {
        let err = gate_error(GovernedError::Blocked {
            reason: Blocked::HostNotAllowed,
            url: "https://other.example.com/x?q=1".into(),
        });
        assert_eq!(
            err.to_string(),
            "blocked: host not allowed (other.example.com)"
        );
    }

    #[test]
    fn a_held_request_waited_half_an_interval() {
        let at = |rate| held_after(std::num::NonZeroU32::new(rate).unwrap());
        assert_eq!(at(200), Duration::from_micros(2500));
        assert_eq!(at(1), Duration::from_millis(500));
        assert_eq!(at(5000), MIN_HELD);
    }

    #[test]
    fn only_a_proxy_at_the_target_origin_is_allowed() {
        let safety = Safety {
            target: "http://127.0.0.1:8080",
            permission: Permission::ReadOnly,
            production: None,
        };
        let policy = policy(
            &SafetySettings::default(),
            safety,
            EnvironmentInfo::default(),
        )
        .unwrap();
        let url = "http://127.0.0.1:8080/x";
        let at_target = ProxySettings::explicit("http://127.0.0.1:8080").unwrap();
        assert!(outside_proxy(&policy, &at_target, url).is_none());
        let elsewhere = ProxySettings::explicit("http://u:p@10.0.0.9:3128").unwrap();
        let proxy = outside_proxy(&policy, &elsewhere, url).unwrap();
        assert_eq!(proxy.host, "10.0.0.9");
        let socks = ProxySettings::explicit("socks5://127.0.0.1:8080").unwrap();
        assert!(outside_proxy(&policy, &socks, url).is_some());
        assert!(outside_proxy(&policy, &ProxySettings::default(), url).is_none());
        let bypassed = ProxySettings {
            no_proxy: rustman_http::NoProxy::parse("127.0.0.1"),
            ..elsewhere
        };
        assert!(outside_proxy(&policy, &bypassed, url).is_none());
    }

    #[test]
    fn a_target_with_credentials_is_a_usage_error() {
        for target in [
            "http://bob:s3cret@api.example.com",
            "http://bob@api.example.com",
        ] {
            let safety = Safety {
                target,
                permission: Permission::ReadOnly,
                production: None,
            };
            let err = policy(
                &SafetySettings::default(),
                safety,
                EnvironmentInfo::default(),
            )
            .unwrap_err();
            assert!(!err.is_refusal());
            assert!(!err.to_string().contains("s3cret"), "{err}");
        }
    }

    #[test]
    fn a_target_beyond_its_origin_is_a_usage_error() {
        let check = |target| {
            let safety = Safety {
                target,
                permission: Permission::ReadOnly,
                production: None,
            };
            policy(
                &SafetySettings::default(),
                safety,
                EnvironmentInfo::default(),
            )
        };
        for target in [
            "http://api.example.com/v1",
            "http://api.example.com/?token=s3cret",
            "http://api.example.com?token=s3cret",
            "http://api.example.com/#s3cret",
        ] {
            let err = check(target).unwrap_err();
            assert!(!err.is_refusal(), "{target}");
            assert!(err.to_string().contains("must be an origin"), "{err}");
            assert!(!err.to_string().contains("s3cret"), "{err}");
        }
        for target in ["http://api.example.com", "http://api.example.com:8080/"] {
            assert!(check(target).is_ok(), "{target}");
        }
    }
}
