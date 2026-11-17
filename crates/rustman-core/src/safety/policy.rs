//! `SafetyPolicy`: the single decision on whether an automated request may be
//! sent (design D5).

use std::fmt;

use super::config::SafetySettings;
use super::method::{MethodClass, Permission};
use super::origin::Origin;
use super::production::{self, EnvironmentInfo, HostPattern, ProductionReason};

/// What the user asked for on the command line (or an agent session asked
/// for), on top of `[safety]`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PolicyOptions<'a> {
    /// The target URL; its origin is always allowed.
    pub target: &'a str,
    /// From `--allow-mutations` and `--allow-destructive`.
    pub permission: Permission,
    /// `--allow-host` origins, added to `[safety].allowed_hosts`.
    pub allow_hosts: &'a [Origin],
    /// `--production <host>`.
    pub production: Option<&'a str>,
    /// The active environment.
    pub environment: EnvironmentInfo<'a>,
}

/// Why a single request was not sent. Reports show it as
/// `blocked: <reason>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// POST, PUT or PATCH without `--allow-mutations`.
    MutationsNotAllowed,
    /// DELETE (or an unknown method) without `--allow-destructive`.
    DestructiveNotAllowed,
    /// An origin outside the allowlist, or a URL with no http(s) origin.
    HostNotAllowed,
    /// An allowed origin whose host matches `[safety].production_hosts`, but
    /// that `--production` did not name.
    ProductionHostNotConfirmed,
}

impl Blocked {
    pub fn reason(self) -> &'static str {
        match self {
            Self::MutationsNotAllowed => "mutations not allowed",
            Self::DestructiveNotAllowed => "destructive not allowed",
            Self::HostNotAllowed => "host not allowed",
            Self::ProductionHostNotConfirmed => "production host not confirmed",
        }
    }
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blocked: {}", self.reason())
    }
}

impl std::error::Error for Blocked {}

/// Why a policy could not be built at all. Front-ends exit with code 4 when
/// [`PolicyError::is_refusal`] holds, and 2 otherwise.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("invalid target: {0}")]
    InvalidTarget(String),
    #[error(
        "refusing to send requests to production host '{host}' ({reason}); repeat the host with --production {host} to allow non-destructive requests"
    )]
    ProductionNotConfirmed {
        host: String,
        reason: ProductionReason,
    },
    #[error("--production '{given}' does not match the target host '{host}'")]
    ProductionHostMismatch { given: String, host: String },
    #[error(
        "refusing --allow-destructive against production host '{host}' ({reason}): destructive requests are never sent to production"
    )]
    DestructiveOnProduction {
        host: String,
        reason: ProductionReason,
    },
}

impl PolicyError {
    /// Whether this is a safety refusal (exit code 4) rather than bad input.
    pub fn is_refusal(&self) -> bool {
        !matches!(self, Self::InvalidTarget(_))
    }
}

/// The rules every governed request passes: method gating, the origin
/// allowlist and the production guard. Built once per session; checked before
/// every request and on every redirect hop.
#[derive(Clone, Debug)]
pub struct SafetyPolicy {
    target: Origin,
    allowed: Vec<Origin>,
    permission: Permission,
    production_hosts: Vec<HostPattern>,
    production: Option<ProductionReason>,
    confirmed_host: Option<String>,
}

impl SafetyPolicy {
    /// Build the policy, refusing a production target unless `--production`
    /// repeats its host, and refusing `--allow-destructive` against production
    /// in every case.
    pub fn new(settings: &SafetySettings, options: PolicyOptions<'_>) -> Result<Self, PolicyError> {
        let target = Origin::of(options.target).map_err(PolicyError::InvalidTarget)?;
        let confirmed_host = confirmed_host(&target, options.production)?;
        let production = production::detect(
            target.host(),
            options.environment,
            &settings.production_hosts,
        );
        if let Some(reason) = &production {
            guard_production(
                &target,
                reason,
                confirmed_host.is_some(),
                options.permission,
            )?;
        }
        let mut allowed = vec![target.clone()];
        for origin in settings.allowed_hosts.iter().chain(options.allow_hosts) {
            if !allowed.contains(origin) {
                allowed.push(origin.clone());
            }
        }
        Ok(Self {
            target,
            allowed,
            permission: options.permission,
            production_hosts: settings.production_hosts.clone(),
            production,
            confirmed_host,
        })
    }

    /// Whether `method url` may be sent now. Call it with the final URL (after
    /// variable resolution) and again for every redirect hop.
    pub fn check(&self, method: &str, url: &str) -> Result<(), Blocked> {
        self.check_origin(url)?;
        self.check_method(method)
    }

    /// The method rule alone, for planning before a URL is known.
    pub fn check_method(&self, method: &str) -> Result<(), Blocked> {
        let class = MethodClass::of(method);
        if self.permission.allows(class) {
            return Ok(());
        }
        Err(match class {
            MethodClass::Mutation => Blocked::MutationsNotAllowed,
            _ => Blocked::DestructiveNotAllowed,
        })
    }

    /// The origin rules alone: the allowlist, then the production hosts.
    pub fn check_origin(&self, url: &str) -> Result<(), Blocked> {
        let origin = Origin::of(url).map_err(|_| Blocked::HostNotAllowed)?;
        if !self.allowed.contains(&origin) {
            return Err(Blocked::HostNotAllowed);
        }
        let unconfirmed = self.confirmed_host.as_deref() != Some(origin.host());
        if unconfirmed
            && production::matching_pattern(origin.host(), &self.production_hosts).is_some()
        {
            return Err(Blocked::ProductionHostNotConfirmed);
        }
        Ok(())
    }

    pub fn target(&self) -> &Origin {
        &self.target
    }

    /// The target's origin first, then the extra allowed origins.
    pub fn allowed_origins(&self) -> &[Origin] {
        &self.allowed
    }

    pub fn permission(&self) -> Permission {
        self.permission
    }

    /// Why the target is production, when it is.
    pub fn production(&self) -> Option<&ProductionReason> {
        self.production.as_ref()
    }
}

/// The target host when `--production` names it; an error when it names
/// another host.
fn confirmed_host(target: &Origin, given: Option<&str>) -> Result<Option<String>, PolicyError> {
    let Some(given) = given else {
        return Ok(None);
    };
    if same_host(given, target.host()) {
        Ok(Some(target.host().to_owned()))
    } else {
        Err(PolicyError::ProductionHostMismatch {
            given: given.to_owned(),
            host: target.host().to_owned(),
        })
    }
}

/// Hosts compare as URLs normalize them (case, trailing dot, punycode); an
/// IPv6 address may be given with or without brackets. A wildcard confirms
/// nothing.
fn same_host(given: &str, host: &str) -> bool {
    !given.starts_with("*.") && HostPattern::parse(given).is_ok_and(|p| p.matches(host))
}

fn guard_production(
    target: &Origin,
    reason: &ProductionReason,
    confirmed: bool,
    permission: Permission,
) -> Result<(), PolicyError> {
    let host = target.host().to_owned();
    let reason = reason.clone();
    if !confirmed {
        return Err(PolicyError::ProductionNotConfirmed { host, reason });
    }
    if permission == Permission::Destructive {
        return Err(PolicyError::DestructiveOnProduction { host, reason });
    }
    Ok(())
}
