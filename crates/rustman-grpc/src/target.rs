//! gRPC targets: `host:port`, `grpc://host:port` (plaintext) or
//! `grpcs://host:port` (TLS).

use std::fmt;

use crate::error::GrpcError;

/// The scheme a target is written with, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetScheme {
    /// `grpc://`: plaintext HTTP/2 (h2c).
    Plain,
    /// `grpcs://`: TLS.
    Tls,
}

/// A parsed target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrpcTarget {
    /// The target as written (trimmed), for messages.
    pub text: String,
    pub scheme: Option<TargetScheme>,
    /// The host, IPv6 addresses with their brackets.
    pub host: String,
    pub port: u16,
}

impl GrpcTarget {
    pub fn parse(text: &str) -> Result<Self, GrpcError> {
        let text = text.trim();
        let invalid = || GrpcError::Target(text.to_owned());
        let (scheme, authority) = match text.split_once("://") {
            Some(("grpc", rest)) => (Some(TargetScheme::Plain), rest),
            Some(("grpcs", rest)) => (Some(TargetScheme::Tls), rest),
            Some(_) => return Err(invalid()),
            None => (None, text),
        };
        let authority = authority.trim_end_matches('/');
        let (host, port) = authority.rsplit_once(':').ok_or_else(invalid)?;
        let port = port.parse::<u16>().map_err(|_| invalid())?;
        if !valid_host(host) {
            return Err(invalid());
        }
        Ok(Self {
            text: text.to_owned(),
            scheme,
            host: host.to_owned(),
            port,
        })
    }

    /// Whether the call uses TLS: `tls.enabled` when set, else the scheme,
    /// else TLS only on port 443.
    pub fn uses_tls(&self, enabled: Option<bool>) -> bool {
        enabled.unwrap_or(match self.scheme {
            Some(TargetScheme::Tls) => true,
            Some(TargetScheme::Plain) => false,
            None => self.port == 443,
        })
    }

    /// `host:port`.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// The canonical form `grpc[s]://host:port` for a transport choice; the
    /// key of the descriptor cache.
    pub fn canonical(&self, tls: bool) -> String {
        let scheme = if tls { "grpcs" } else { "grpc" };
        format!("{scheme}://{}", self.authority())
    }
}

/// A DNS name or IPv4 address (letters, digits, `-`, `.`, `_`), or a
/// bracketed IPv6 address. Anything else (`user@host`, `#`, `\\`) could
/// be read as another host by the origin policy than by the connect.
fn valid_host(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

impl fmt::Display for GrpcTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        let t = GrpcTarget::parse("grpc://localhost:50051").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("localhost", 50051));
        assert!(!t.uses_tls(None));
        assert!(t.uses_tls(Some(true)));
        assert_eq!(t.canonical(false), "grpc://localhost:50051");
        let t = GrpcTarget::parse("grpcs://api.example:8443/").unwrap();
        assert!(t.uses_tls(None));
        assert!(!t.uses_tls(Some(false)));
        let t = GrpcTarget::parse("[::1]:443").unwrap();
        assert_eq!(t.authority(), "[::1]:443");
        assert!(t.uses_tls(None));
        assert!(!GrpcTarget::parse("h:50051").unwrap().uses_tls(None));
    }

    #[test]
    fn invalid_targets_are_refused() {
        let hostile = [
            "evil@allowed.example:443",
            "grpcs://evil@allowed.example:443",
            "a#b:1",
            "a\\b:1",
            "a:b:1",
            "[not-ipv6]:1",
            "a b:1",
        ];
        for text in ["http://h:1", "h", "h:x", ":1", "grpc://a/b:1"]
            .into_iter()
            .chain(hostile)
        {
            let err = GrpcTarget::parse(text).unwrap_err();
            assert!(err.to_string().starts_with("grpc: invalid target"), "{err}");
        }
    }
}
