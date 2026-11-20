//! The grant vocabulary (plugins spec, "Grant vocabulary"): what a
//! workspace allows a plugin to do, written in `[plugins.<name>] grants`.
//! A manifest's `capabilities` use the same words, or just the class
//! (`env`, `fs:read`, `net`, `secrets:read`) to request any grant of it.

use std::fmt;

use crate::error::LoadError;

/// One capability a workspace grants a plugin.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Grant {
    Clock,
    Random,
    /// One environment variable.
    Env(String),
    /// A workspace folder, normalized: relative, `/`-separated, no `.` or
    /// `..`; empty for the workspace root.
    FsRead(String),
    Net(NetTarget),
    /// One secret, by name.
    SecretsRead(String),
}

/// The host (and port) of a `net:` grant. Without a port only the default
/// ports (80 and 443) are allowed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NetTarget {
    pub host: String,
    pub port: Option<u16>,
}

impl NetTarget {
    /// Whether a request to `host:port` falls under this grant.
    pub fn allows(&self, host: &str, port: u16) -> bool {
        let host_matches = self.host.eq_ignore_ascii_case(host.trim_end_matches('.'));
        let port_matches = match self.port {
            Some(granted) => granted == port,
            None => matches!(port, 80 | 443),
        };
        host_matches && port_matches
    }
}

impl fmt::Display for NetTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bracketed = self.host.contains(':');
        match (bracketed, self.port) {
            (true, Some(port)) => write!(f, "[{}]:{port}", self.host),
            (true, None) => write!(f, "[{}]", self.host),
            (false, Some(port)) => write!(f, "{}:{port}", self.host),
            (false, None) => f.write_str(&self.host),
        }
    }
}

/// Why a grant (or a manifest capability) is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantError {
    Unknown,
    Wildcard,
    /// An `fs:read` path outside the workspace root.
    Escapes,
    /// An `fs:read` path into `.rustman/`.
    IntoState,
    Invalid(&'static str),
}

impl GrantError {
    /// The load error for `grant` of `plugin`.
    pub fn for_plugin(&self, plugin: &str, grant: &str) -> LoadError {
        let message = match self {
            Self::Unknown => format!("unknown grant '{grant}'"),
            Self::Wildcard => "wildcard grants are not allowed".to_owned(),
            Self::Escapes => format!("grant '{grant}' escapes the workspace"),
            Self::IntoState => format!("grant '{grant}' points into .rustman/"),
            Self::Invalid(reason) => format!("grant '{grant}': {reason}"),
        };
        LoadError::invalid(plugin, message)
    }
}

/// The capability classes a manifest may request as a whole.
pub const CLASSES: [&str; 6] = ["clock", "random", "env", "fs:read", "net", "secrets:read"];

impl Grant {
    pub fn parse(text: &str) -> Result<Self, GrantError> {
        let text = text.trim();
        match text {
            "clock" => return Ok(Self::Clock),
            "random" => return Ok(Self::Random),
            _ => {}
        }
        if let Some(name) = text.strip_prefix("env:") {
            return env_name(name).map(Self::Env);
        }
        if let Some(path) = text.strip_prefix("fs:read:") {
            return normalize_dir(path).map(Self::FsRead);
        }
        if let Some(target) = text.strip_prefix("net:") {
            return net_target(target).map(Self::Net);
        }
        if let Some(name) = text.strip_prefix("secrets:read:") {
            return secret_name(name).map(Self::SecretsRead);
        }
        Err(GrantError::Unknown)
    }

    /// The class a manifest requests to allow this grant.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Clock => "clock",
            Self::Random => "random",
            Self::Env(_) => "env",
            Self::FsRead(_) => "fs:read",
            Self::Net(_) => "net",
            Self::SecretsRead(_) => "secrets:read",
        }
    }

    /// Whether `capabilities` (a manifest's) request this grant: its class,
    /// or this very grant.
    pub fn requested_by(&self, capabilities: &[Capability]) -> bool {
        capabilities.iter().any(|capability| match capability {
            Capability::Class(class) => *class == self.class(),
            Capability::Grant(grant) => grant == self,
        })
    }
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clock => f.write_str("clock"),
            Self::Random => f.write_str("random"),
            Self::Env(name) => write!(f, "env:{name}"),
            Self::FsRead(dir) if dir.is_empty() => f.write_str("fs:read:."),
            Self::FsRead(dir) => write!(f, "fs:read:{dir}"),
            Self::Net(target) => write!(f, "net:{target}"),
            Self::SecretsRead(name) => write!(f, "secrets:read:{name}"),
        }
    }
}

/// What a manifest requests in `capabilities`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capability {
    /// A whole class, such as `env`.
    Class(&'static str),
    /// One grant, such as `env:HOME`.
    Grant(Grant),
}

impl Capability {
    pub fn parse(text: &str) -> Result<Self, GrantError> {
        let text = text.trim();
        match CLASSES.into_iter().find(|class| *class == text) {
            Some(class) => Ok(Self::Class(class)),
            None => Grant::parse(text).map(Self::Grant),
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Class(class) => f.write_str(class),
            Self::Grant(grant) => grant.fmt(f),
        }
    }
}

/// Parse a plugin's `grants`, refusing any that `capabilities` (its
/// manifest's) do not request.
pub fn parse_grants(
    plugin: &str,
    grants: &[String],
    capabilities: &[Capability],
) -> Result<Vec<Grant>, LoadError> {
    let mut parsed = Vec::new();
    for text in grants {
        let grant = Grant::parse(text).map_err(|e| e.for_plugin(plugin, text))?;
        if !grant.requested_by(capabilities) {
            let message = format!("grant '{}' not requested by the plugin", text.trim());
            return Err(LoadError::invalid(plugin, message));
        }
        if !parsed.contains(&grant) {
            parsed.push(grant);
        }
    }
    Ok(parsed)
}

fn has_wildcard(text: &str) -> bool {
    text.contains(['*', '?'])
}

fn env_name(name: &str) -> Result<String, GrantError> {
    if has_wildcard(name) {
        return Err(GrantError::Wildcard);
    }
    let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if valid {
        Ok(name.to_owned())
    } else {
        Err(GrantError::Invalid("not an environment variable name"))
    }
}

fn secret_name(name: &str) -> Result<String, GrantError> {
    if has_wildcard(name) {
        return Err(GrantError::Wildcard);
    }
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if valid {
        Ok(name.to_owned())
    } else {
        Err(GrantError::Invalid("not a secret name"))
    }
}

fn net_target(text: &str) -> Result<NetTarget, GrantError> {
    if has_wildcard(text) {
        return Err(GrantError::Wildcard);
    }
    if text.contains("://") || text.contains(['/', '@', '?', '#']) {
        return Err(GrantError::Invalid("expected a host and an optional port"));
    }
    let (host, port) = split_host_port(text)?;
    let label = |l: &str| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let ipv6 = host.contains(':') && host.chars().all(|c| c.is_ascii_hexdigit() || c == ':');
    if !(ipv6 || host.split('.').all(label)) {
        return Err(GrantError::Invalid("not a host name"));
    }
    Ok(NetTarget {
        host: host.to_ascii_lowercase(),
        port,
    })
}

/// `host`, `host:port`, `[v6]` or `[v6]:port`.
fn split_host_port(text: &str) -> Result<(&str, Option<u16>), GrantError> {
    let port = |p: &str| {
        p.parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(GrantError::Invalid("not a port"))
    };
    if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or(GrantError::Invalid("unclosed '['"))?;
        return match after {
            "" => Ok((host, None)),
            _ => match after.strip_prefix(':') {
                Some(p) => Ok((host, Some(port(p)?))),
                None => Err(GrantError::Invalid("expected a host and an optional port")),
            },
        };
    }
    match text.split_once(':') {
        Some((host, p)) => Ok((host, Some(port(p)?))),
        None => Ok((text, None)),
    }
}

/// `path` as a normalized workspace-relative folder (see [`Grant::FsRead`]).
pub fn normalize_dir(path: &str) -> Result<String, GrantError> {
    if has_wildcard(path) {
        return Err(GrantError::Wildcard);
    }
    let path = path.trim();
    if path.is_empty() {
        return Err(GrantError::Invalid("empty path"));
    }
    let parts = normalize_relative(path)?;
    match parts.first() {
        Some(first) if first.eq_ignore_ascii_case(".rustman") => Err(GrantError::IntoState),
        _ => Ok(parts.join("/")),
    }
}

/// The components of a workspace-relative `path`, with `.` dropped and
/// `..` applied; [`GrantError::Escapes`] for absolute paths, drive or
/// stream prefixes, and `..` above the root.
pub fn normalize_relative(path: &str) -> Result<Vec<&str>, GrantError> {
    if path.starts_with(['/', '\\']) || path.contains(':') {
        return Err(GrantError::Escapes);
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or(GrantError::Escapes)?;
            }
            part => parts.push(part),
        }
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(texts: &[&str], capabilities: &[&str]) -> Result<Vec<Grant>, String> {
        let capabilities: Vec<Capability> = capabilities
            .iter()
            .map(|c| Capability::parse(c).unwrap())
            .collect();
        let texts: Vec<String> = texts.iter().map(|t| (*t).to_owned()).collect();
        parse_grants("hmac", &texts, &capabilities).map_err(|e| e.to_string())
    }

    #[test]
    fn the_whole_vocabulary_parses() {
        let all = [
            "clock",
            "random",
            "env:HOME",
            "fs:read:data/in",
            "net:auth.example.com",
            "net:localhost:8443",
            "secrets:read:hmac_key",
        ];
        let parsed = grants(&all, &CLASSES).unwrap();
        let shown: Vec<String> = parsed.iter().map(Grant::to_string).collect();
        assert_eq!(shown, all);
        assert_eq!(
            Grant::parse("net:[::1]:9000").unwrap().to_string(),
            "net:[::1]:9000"
        );
        assert_eq!(
            Grant::parse("fs:read:./a/../b/").unwrap().to_string(),
            "fs:read:b"
        );
        assert_eq!(
            Grant::parse("fs:read:.").unwrap(),
            Grant::FsRead(String::new())
        );
    }

    #[test]
    fn grant_beyond_the_manifest() {
        assert_eq!(
            grants(&["env:HOME"], &["clock"]).unwrap_err(),
            "plugin hmac: grant 'env:HOME' not requested by the plugin"
        );
        assert!(grants(&["env:HOME"], &["env"]).is_ok());
        assert!(grants(&["env:HOME"], &["env:HOME"]).is_ok());
        assert_eq!(
            grants(&["env:PATH"], &["env:HOME"]).unwrap_err(),
            "plugin hmac: grant 'env:PATH' not requested by the plugin"
        );
    }

    #[test]
    fn path_escape() {
        for escape in [
            "fs:read:../",
            "fs:read:a/../../b",
            "fs:read:/etc",
            "fs:read:C:\\x",
        ] {
            assert_eq!(
                grants(&[escape], &["fs:read"]).unwrap_err(),
                format!("plugin hmac: grant '{escape}' escapes the workspace")
            );
        }
        assert_eq!(
            grants(&["fs:read:.rustman/plugins"], &["fs:read"]).unwrap_err(),
            "plugin hmac: grant 'fs:read:.rustman/plugins' points into .rustman/"
        );
    }

    #[test]
    fn wildcard_host_refused() {
        for wildcard in ["net:*", "net:*.example.com", "env:AWS_*", "secrets:read:*"] {
            assert_eq!(
                grants(&[wildcard], &CLASSES).unwrap_err(),
                "plugin hmac: wildcard grants are not allowed"
            );
        }
        assert_eq!(Capability::parse("net:*"), Err(GrantError::Wildcard));
    }

    #[test]
    fn malformed_grants_are_named() {
        assert_eq!(
            grants(&["telemetry"], &CLASSES).unwrap_err(),
            "plugin hmac: unknown grant 'telemetry'"
        );
        assert_eq!(
            grants(&["net:https://a.example/x"], &CLASSES).unwrap_err(),
            "plugin hmac: grant 'net:https://a.example/x': expected a host and an optional port"
        );
        assert!(grants(&["net:a.example:0"], &CLASSES).is_err());
        assert!(grants(&["env:"], &CLASSES).is_err());
        assert!(grants(&["secrets:read:a b"], &CLASSES).is_err());
    }

    #[test]
    fn net_grants_match_host_and_port() {
        let Grant::Net(target) = Grant::parse("net:Auth.Example.com").unwrap() else {
            panic!("not a net grant");
        };
        assert!(target.allows("auth.example.com", 443));
        assert!(target.allows("AUTH.example.com.", 80));
        assert!(!target.allows("auth.example.com", 8443));
        assert!(!target.allows("evil.example.net", 443));
        let Grant::Net(target) = Grant::parse("net:localhost:8443").unwrap() else {
            panic!("not a net grant");
        };
        assert!(target.allows("localhost", 8443));
        assert!(!target.allows("localhost", 443));
    }
}
