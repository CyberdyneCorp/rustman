//! Monitors are read-only by default (monitors "Monitors are read-only by
//! default", design D1). Every request goes through a [`GovernedSender`]
//! (the runner's [`SendGate`]) under a [`SafetyPolicy`] whose allowlist is
//! the target hosts of the monitor's environment:
//!
//! - the origins of the environment's URL-valued variables, and
//! - the origins of the monitor's requests resolved with the environment
//!   (a URL that needs a capture is known only when it is sent).
//!
//! GET, HEAD and OPTIONS are allowed; POST, PUT and PATCH only with
//! `allow_mutations: true`; DELETE and other methods never. A refused
//! request is not sent and errors with `blocked: <reason>`.
//!
//! The production guard does not apply: the monitor file names its
//! environment, which is the confirmation `--production` gives elsewhere,
//! and the method rules above hold on every host.

use futures_util::future::BoxFuture;
use rustman_core::model::VarValue;
use rustman_core::run::Plan;
use rustman_core::safety::{
    EnvironmentInfo, Governor, Limits, MethodClass, Origin, Permission, PolicyError, PolicyOptions,
    SafetyPolicy, SafetySettings,
};
use rustman_core::vars::{ResolvedRequest, Resolver, resolve_request};
use rustman_http::runner::{RunContext, SendGate};
use rustman_http::{Exchange, GovernedError, GovernedSender, RequestError, SendOptions};

/// The origins requests of `plan` may go to, in first-seen order.
pub fn allowed_origins(plan: &Plan<'_>, ctx: &RunContext<'_>) -> Vec<Origin> {
    let mut origins: Vec<Origin> = Vec::new();
    let mut add = |url: &str| {
        if let Ok(origin) = Origin::of(url)
            && !origins.contains(&origin)
        {
            origins.push(origin);
        }
    };
    if let Some(env) = ctx.environment {
        for (_, value) in env.variables.iter() {
            if let VarValue::String(text) = value {
                add(text);
            }
        }
    }
    for request in &plan.requests {
        let resolver = Resolver::for_request(ctx.settings, request, ctx.environment, ctx.secrets);
        if let Ok(resolved) = resolve_request(request, &resolver) {
            add(&resolved.url.text);
        }
    }
    origins
}

/// The monitor's policy over `origins` (at least one): `[safety]` rate and
/// concurrency, no other allowed host and no production guard.
pub fn policy(
    settings: &SafetySettings,
    origins: &[Origin],
    allow_mutations: bool,
) -> Result<SafetyPolicy, PolicyError> {
    let (target, others) = origins
        .split_first()
        .ok_or_else(|| PolicyError::InvalidTarget("no target host".to_owned()))?;
    let target = target.to_string();
    let pinned = SafetySettings {
        allowed_hosts: Vec::new(),
        production_hosts: Vec::new(),
        ..settings.clone()
    };
    let permission = if allow_mutations {
        Permission::Mutations
    } else {
        Permission::ReadOnly
    };
    let options = PolicyOptions {
        target: &target,
        permission,
        allow_hosts: others,
        production: None,
        environment: EnvironmentInfo::default(),
    };
    SafetyPolicy::new(&pinned, options)
}

/// Why a monitor may not send `method`, or `None` when it may.
pub fn method_refusal(method: &str, allow_mutations: bool) -> Option<String> {
    match MethodClass::of(method) {
        MethodClass::ReadOnly => None,
        MethodClass::Mutation if allow_mutations => None,
        MethodClass::Mutation => Some(format!("method {method} requires allow_mutations")),
        MethodClass::Destructive if method == "DELETE" => {
            Some("method DELETE is never allowed for monitors".to_owned())
        }
        MethodClass::Destructive => Some(format!("method {method} is not allowed for monitors")),
    }
}

/// Sends a monitor's requests: the method rules first, then the governed
/// sender (allowlist on every redirect hop, rate and concurrency).
pub struct MonitorGate {
    sender: GovernedSender,
    allow_mutations: bool,
}

impl MonitorGate {
    pub fn new(policy: SafetyPolicy, settings: &SafetySettings, allow_mutations: bool) -> Self {
        let governor = Governor::new(Limits::from_settings(settings));
        Self {
            sender: GovernedSender::new(policy, governor),
            allow_mutations,
        }
    }
}

impl SendGate for MonitorGate {
    fn send<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> BoxFuture<'a, Result<Exchange, RequestError>> {
        Box::pin(async move {
            if let Some(reason) = method_refusal(&request.method, self.allow_mutations) {
                return Err(RequestError::Blocked(reason));
            }
            self.sender.send(request, options).await.map_err(gate_error)
        })
    }
}

/// `blocked: host not allowed (https://other.example.com)`; the URL is
/// already redacted.
fn gate_error(err: GovernedError) -> RequestError {
    match err {
        GovernedError::Blocked { reason, url } => {
            let shown = Origin::of(&url).map_or(url, |o| o.to_string());
            RequestError::Blocked(format!("{} ({shown})", reason.reason()))
        }
        GovernedError::NotRun(budget) => RequestError::Blocked(budget.detail().to_owned()),
        GovernedError::Request(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::safety::Blocked;

    #[test]
    fn reads_pass_mutations_need_opt_in_and_delete_never() {
        for method in ["GET", "HEAD", "OPTIONS"] {
            assert_eq!(method_refusal(method, false), None);
        }
        assert_eq!(
            method_refusal("POST", false).unwrap(),
            "method POST requires allow_mutations"
        );
        assert_eq!(method_refusal("PATCH", true), None);
        assert_eq!(
            method_refusal("DELETE", true).unwrap(),
            "method DELETE is never allowed for monitors"
        );
        assert_eq!(
            method_refusal("PURGE", true).unwrap(),
            "method PURGE is not allowed for monitors"
        );
    }

    #[test]
    fn the_allowlist_is_the_given_origins_only() {
        let settings = SafetySettings {
            allowed_hosts: vec![Origin::parse("https://elsewhere.example.com").unwrap()],
            ..SafetySettings::default()
        };
        let origins = [
            Origin::parse("https://api.example.com").unwrap(),
            Origin::parse("http://127.0.0.1:8080").unwrap(),
        ];
        let policy = policy(&settings, &origins, false).unwrap();
        assert!(policy.check_origin("https://api.example.com/x").is_ok());
        assert!(policy.check_origin("http://127.0.0.1:8080/y").is_ok());
        assert_eq!(
            policy.check_origin("https://elsewhere.example.com/"),
            Err(Blocked::HostNotAllowed)
        );
        assert!(super::policy(&settings, &[], false).is_err());
    }

    /// A host listed in `[safety].production_hosts` is still monitored:
    /// the monitor file's environment is the confirmation.
    #[test]
    fn production_hosts_are_monitored_read_only() {
        let settings = SafetySettings {
            production_hosts: vec![
                rustman_core::safety::HostPattern::parse("api.example.com").unwrap(),
            ],
            ..SafetySettings::default()
        };
        let origins = [Origin::parse("https://api.example.com").unwrap()];
        let policy = policy(&settings, &origins, false).unwrap();
        assert!(
            policy
                .check("GET", "https://api.example.com/health")
                .is_ok()
        );
        assert!(
            policy
                .check("POST", "https://api.example.com/health")
                .is_err()
        );
    }

    #[test]
    fn refusals_name_the_origin() {
        let err = gate_error(GovernedError::Blocked {
            reason: Blocked::HostNotAllowed,
            url: "https://other.example.com/x?q=1".into(),
        });
        assert_eq!(
            err.to_string(),
            "blocked: host not allowed (https://other.example.com:443)"
        );
    }
}
