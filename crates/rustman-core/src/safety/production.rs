//! The production guard's detection rules.

use std::fmt;

use serde::Deserialize;

/// Environment names that always mean production (compared ignoring case).
pub const PRODUCTION_ENVIRONMENT_NAMES: [&str; 2] = ["prod", "production"];

/// An entry of `[safety].production_hosts`: an exact host such as
/// `api.example.com`, or `*.example.com` for every subdomain of `example.com`
/// (not `example.com` itself). Entries are normalized the way URLs normalize
/// hosts (lowercase, internationalized names as punycode, no trailing dot), so
/// no spelling of a target host escapes its entry. IP addresses are exact
/// entries; IPv6 ones may be written with or without brackets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPattern(String);

impl HostPattern {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (wildcard, host) = match text.strip_prefix("*.") {
            Some(domain) => (true, domain),
            None => (false, text),
        };
        normal_pattern_host(host, wildcard)
            .map(|host| Self(if wildcard { format!("*.{host}") } else { host }))
            .ok_or_else(|| {
                format!(
                    "invalid production host '{text}': expected a host such as api.example.com or *.example.com"
                )
            })
    }

    /// Whether `host`, as a URL spells it, is covered by this entry.
    pub fn matches(&self, host: &str) -> bool {
        let host = normal_host(host);
        match self.0.strip_prefix("*.") {
            Some(domain) => host
                .strip_suffix(domain)
                .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.')),
            None => host == self.0,
        }
    }
}

/// A host as URLs compare it: lowercase, without one trailing dot (the
/// fully qualified spelling of the same name).
pub(crate) fn normal_host(host: &str) -> String {
    host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

/// The canonical form of a pattern's host, or `None` when it isn't a host.
fn normal_pattern_host(host: &str, wildcard: bool) -> Option<String> {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.starts_with('.') || host.contains(['*', '/', '@', ' ']) {
        return None;
    }
    let bracketed;
    let host = if host.contains(':') && !host.starts_with('[') {
        bracketed = format!("[{host}]");
        &bracketed
    } else {
        host
    };
    match url::Host::parse(host).ok()? {
        url::Host::Domain(domain) => Some(domain),
        ip if !wildcard => Some(ip.to_string()),
        _ => None,
    }
}

impl fmt::Display for HostPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for HostPattern {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// The environment a target is used with, as far as the guard cares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentInfo<'a> {
    pub name: Option<&'a str>,
    /// `production: true` in the environment file.
    pub marked_production: bool,
}

impl<'a> From<&'a crate::Environment> for EnvironmentInfo<'a> {
    fn from(env: &'a crate::Environment) -> Self {
        Self {
            name: Some(&env.name),
            marked_production: env.production,
        }
    }
}

/// Why a target counts as production.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProductionReason {
    /// The environment file sets `production: true`.
    MarkedEnvironment(String),
    /// The environment is named `prod` or `production`.
    EnvironmentName(String),
    /// The host matches an entry of `[safety].production_hosts`.
    ProductionHost(HostPattern),
}

impl fmt::Display for ProductionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MarkedEnvironment(name) => {
                write!(f, "environment '{name}' is marked production: true")
            }
            Self::EnvironmentName(name) => write!(f, "environment '{name}' is a production name"),
            Self::ProductionHost(pattern) => {
                write!(
                    f,
                    "host matches [safety].production_hosts entry '{pattern}'"
                )
            }
        }
    }
}

/// The first rule that makes `host` (used with `env`) production, if any.
pub fn detect(
    host: &str,
    env: EnvironmentInfo<'_>,
    production_hosts: &[HostPattern],
) -> Option<ProductionReason> {
    let name = env.name.unwrap_or_default();
    if env.marked_production {
        return Some(ProductionReason::MarkedEnvironment(name.to_owned()));
    }
    if PRODUCTION_ENVIRONMENT_NAMES
        .iter()
        .any(|p| p.eq_ignore_ascii_case(name))
    {
        return Some(ProductionReason::EnvironmentName(name.to_owned()));
    }
    matching_pattern(host, production_hosts).map(|p| ProductionReason::ProductionHost(p.clone()))
}

pub(crate) fn matching_pattern<'a>(
    host: &str,
    patterns: &'a [HostPattern],
) -> Option<&'a HostPattern> {
    patterns.iter().find(|p| p.matches(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(text: &str) -> HostPattern {
        HostPattern::parse(text).unwrap()
    }

    #[test]
    fn exact_patterns_match_the_host_ignoring_case() {
        let p = pattern("API.example.com");
        assert!(p.matches("api.example.com"));
        assert!(p.matches("Api.Example.Com"));
        assert!(!p.matches("eu.api.example.com"));
        assert!(!p.matches("api.example.com.evil.net"));
    }

    #[test]
    fn wildcards_match_subdomains_only() {
        let p = pattern("*.example.com");
        assert!(p.matches("api.example.com"));
        assert!(p.matches("eu.api.example.com"));
        assert!(!p.matches("example.com"));
        assert!(!p.matches("badexample.com"));
        assert!(!p.matches("api.example.com.evil.net"));
    }

    #[test]
    fn patterns_and_hosts_are_normalized_like_urls() {
        let rows = [
            // (entry, host as url spells it, matches)
            ("api.example.com", "api.example.com.", true),
            ("api.example.com.", "api.example.com", true),
            ("*.example.com", "api.example.com.", true),
            ("bücher.example", "xn--bcher-kva.example", true),
            ("*.bücher.example", "api.xn--bcher-kva.example", true),
            ("[::1]", "[::1]", true),
            ("::1", "[::1]", true),
            ("[0:0::1]", "[::1]", true),
            ("127.0.0.1", "127.0.0.1", true),
            ("api.example.com", "api.example.com..", false),
        ];
        for (entry, host, expected) in rows {
            assert_eq!(pattern(entry).matches(host), expected, "{entry} {host}");
        }
        assert_eq!(
            pattern("Bücher.Example.").to_string(),
            "xn--bcher-kva.example"
        );
    }

    #[test]
    fn malformed_patterns_are_rejected() {
        for bad in [
            "",
            "*.",
            "*",
            "a.*.com",
            "https://api.example.com",
            "api.example.com:443",
            ".example.com",
            "*.127.0.0.1",
            "*.[::1]",
            "[::1]:443",
        ] {
            assert!(HostPattern::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn detection_rules() {
        let hosts = [pattern("api.example.com")];
        let env = |name, marked| EnvironmentInfo {
            name: Some(name),
            marked_production: marked,
        };
        let cases = [
            (
                "other.net",
                env("live", true),
                Some("environment 'live' is marked production: true"),
            ),
            (
                "other.net",
                env("prod", false),
                Some("environment 'prod' is a production name"),
            ),
            (
                "other.net",
                env("Production", false),
                Some("environment 'Production' is a production name"),
            ),
            (
                "api.example.com",
                env("staging", false),
                Some("host matches [safety].production_hosts entry 'api.example.com'"),
            ),
            (
                "api.example.com",
                EnvironmentInfo::default(),
                Some("host matches [safety].production_hosts entry 'api.example.com'"),
            ),
            ("other.net", env("prod-eu", false), None),
            ("other.net", env("staging", false), None),
            ("other.net", EnvironmentInfo::default(), None),
        ];
        for (host, env, expected) in cases {
            let reason = detect(host, env, &hosts).map(|r| r.to_string());
            assert_eq!(reason.as_deref(), expected, "{host} {env:?}");
        }
    }
}
